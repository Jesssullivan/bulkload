from __future__ import annotations

from copy import deepcopy
import hashlib
import os
from pathlib import Path
import shutil
import sqlite3
import tempfile
import time
import unittest
from unittest import mock
import uuid

from bulkload_lib.model import (
    BulkloadError,
    canonical_bytes,
    object_digest,
    sha256_bytes,
)
from bulkload_lib import private_sqlite_action_plan
from bulkload_lib import private_runtime
from bulkload_lib import private_sqlite_request
from bulkload_lib.private_sqlite_action_plan import (
    PRIVATE_SQLITE_ACTION_PLAN_SCHEMA,
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
    DEFAULT_CAPACITY_OBSERVATION_TTL_SECONDS,
    PRIVATE_SQLITE_CAPACITY_OBSERVATION_IMPLEMENTATION,
    PRIVATE_SQLITE_CAPACITY_OBSERVATION_SCHEMA,
    PRIVATE_SQLITE_COMPOSE_REQUEST_IMPLEMENTATION,
    PRIVATE_SQLITE_COMPOSE_REQUEST_SCHEMA,
    _derived_compose_output_leaf,
    validate_codex_private_sqlite_capacity_observation,
    validate_codex_private_sqlite_capacity_observation_against_request,
    validate_codex_private_sqlite_compose_request,
    validate_codex_private_sqlite_compose_request_against_action,
)
from tests.private_sqlite_legacy_fixtures import build_v5_action_plan_fixture


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
                    "capture_id": uuid.uuid4().hex,
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
            return build_v5_action_plan_fixture(
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
                runtime_authority=runtime,
                created_at="2026-07-29T00:00:00Z",
            )

    def test_close_identity_set_normalizes_supported_uuid_encodings(self) -> None:
        private_bindings = self._private_bindings()
        session_bindings = self._session_bindings()
        session_bindings["source"][0]["capture_id"] = uuid.UUID(
            private_bindings["source"][0]["capture_id"]
        ).hex

        with self.assertRaisesRegex(
            BulkloadError,
            "cross-plane closing identities are not distinct",
        ):
            private_sqlite_action_plan._validate_close_identity_set(
                self.close_request,
                private_bindings,
                session_bindings,
            )

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
        plan = self._compile()
        plan["runtime_authority"] = deepcopy(
            private_runtime.LEGACY_PRIVATE_RUNTIME_AUTHORITY_V4
        )
        plan["action_plan_sha256"] = object_digest(
            plan,
            "action_plan_sha256",
        )
        with self.assertRaisesRegex(BulkloadError, "requires policy v5"):
            validate_codex_private_sqlite_action_plan(plan)

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
            self.assertIsNone(
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
        return deepcopy(private_runtime.ACCEPTED_H7_PRIVATE_RUNTIME_AUTHORITY_V6)

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
        leaf = _derived_compose_output_leaf(action_plan["action_plan_sha256"])
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

    def _frozen_v6_request(self, action_plan: dict | None = None) -> dict:
        action = action_plan or self._compile()
        workspace = self._workspace_record(action)
        request = {
            "schema": PRIVATE_SQLITE_COMPOSE_REQUEST_SCHEMA,
            "created_at": "2026-07-29T18:00:00Z",
            "request_id": "77777777-7777-4777-8777-777777777777",
            "action_plan": private_sqlite_request._action_binding(action),
            "action_plan_producer_runtime_authority": deepcopy(
                action["runtime_authority"]
            ),
            "request_runtime_authority": self._request_runtime_authority(),
            "required_composer_runtime_authority": None,
            "output_intent": {
                "workspace": workspace,
                "create_only": True,
                "replace": False,
                "delete": False,
                "future_directory_mode": 0o700,
                "future_file_mode": 0o600,
                "same_mount_required": True,
                "randomized_staging_sibling_required": True,
            },
            "capacity_requirement": private_sqlite_request._capacity_requirement(
                action
            ),
            "capacity_observation": {
                "required": True,
                "maximum_age_seconds": DEFAULT_CAPACITY_OBSERVATION_TTL_SECONDS,
                "space_reserved": False,
                "future_write_guaranteed": False,
                "quota_proof": False,
            },
            "claims": {
                "request_complete": True,
                "request_runtime_bound": True,
                "composer_runtime_bound": False,
                "executed": False,
                "ready_for_internal_offline_compose": False,
                "ready_for_offline_compose": False,
                "composer_implemented": False,
                "compose_authorized": False,
                "publication_authorized": False,
                "install_authorized": False,
                "combined_authorized": False,
                "apply_authorized": False,
                "provider_runtime_acceptance": False,
                "provider_writer_proof": False,
            },
            "implementation": PRIVATE_SQLITE_COMPOSE_REQUEST_IMPLEMENTATION,
        }
        request["request_body_sha256"] = sha256_bytes(canonical_bytes(request))
        request["request_sha256"] = object_digest(request, "request_sha256")
        return request

    def _frozen_v6_capacity(self, request: dict) -> dict:
        requirement = request["capacity_requirement"]
        fragment_size = 4096
        available_blocks = (
            requirement["required_bytes"] + fragment_size - 1
        ) // fragment_size
        filesystem = {
            "fragment_size": fragment_size,
            "available_blocks": available_blocks,
            "available_bytes": available_blocks * fragment_size,
            "available_inodes": requirement["required_inodes"],
        }
        observation = {
            "schema": PRIVATE_SQLITE_CAPACITY_OBSERVATION_SCHEMA,
            "created_at": "2026-07-29T18:01:00Z",
            "expires_at": "2026-07-29T18:06:00Z",
            "observation_id": "99999999-9999-4999-8999-999999999999",
            "host_authority_id": "88888888-8888-4888-8888-888888888888",
            "request": {
                "schema": request["schema"],
                "request_sha256": request["request_sha256"],
                "body_sha256": sha256_bytes(canonical_bytes(request)),
            },
            "observation_runtime_authority": deepcopy(
                request["request_runtime_authority"]
            ),
            "workspace": deepcopy(request["output_intent"]["workspace"]),
            "requirement": deepcopy(requirement),
            "filesystem": filesystem,
            "claims": {
                "capacity_sufficient_observed": True,
                "space_reserved": False,
                "future_write_guaranteed": False,
                "quota_proof": False,
                "executed": False,
                "ready_for_internal_offline_compose": False,
                "compose_authorized": False,
            },
            "implementation": PRIVATE_SQLITE_CAPACITY_OBSERVATION_IMPLEMENTATION,
        }
        observation["observation_body_sha256"] = sha256_bytes(
            canonical_bytes(observation)
        )
        observation["observation_sha256"] = object_digest(
            observation,
            "observation_sha256",
        )
        return observation

    def test_v7_validates_frozen_v6_artifacts_without_producer_api(self) -> None:
        action = self._compile()
        request = self._frozen_v6_request(action)
        observation = self._frozen_v6_capacity(request)

        validate_codex_private_sqlite_compose_request(request)
        validate_codex_private_sqlite_compose_request_against_action(
            request,
            action,
            request["output_intent"]["workspace"],
            request["request_runtime_authority"],
        )
        validate_codex_private_sqlite_capacity_observation(observation)
        validate_codex_private_sqlite_capacity_observation_against_request(
            observation,
            request,
            request["output_intent"]["workspace"],
            request["request_runtime_authority"],
        )

        for retired in (
            "compile_codex_private_sqlite_compose_request",
            "compile_codex_private_sqlite_capacity_observation",
            "_recompute_frozen_v6_compose_request",
            "_recompute_frozen_v6_capacity_observation",
            "PinnedComposeWorkspace",
        ):
            self.assertFalse(hasattr(private_sqlite_request, retired), retired)

    def test_v7_request_rejects_self_redigested_action_drift(self) -> None:
        action = self._compile()
        request = self._frozen_v6_request(action)
        request["action_plan"]["action_plan_sha256"] = "0" * 64
        body = {
            key: value
            for key, value in request.items()
            if key not in {"request_body_sha256", "request_sha256"}
        }
        request["request_body_sha256"] = sha256_bytes(canonical_bytes(body))
        request["request_sha256"] = object_digest(request, "request_sha256")

        validate_codex_private_sqlite_compose_request(request)
        with self.assertRaisesRegex(BulkloadError, "exact inputs"):
            validate_codex_private_sqlite_compose_request_against_action(
                request,
                action,
                request["output_intent"]["workspace"],
                request["request_runtime_authority"],
            )

    def test_v7_capacity_rejects_self_redigested_request_drift(self) -> None:
        request = self._frozen_v6_request()
        observation = self._frozen_v6_capacity(request)
        observation["request"]["request_sha256"] = "0" * 64
        body = {
            key: value
            for key, value in observation.items()
            if key not in {"observation_body_sha256", "observation_sha256"}
        }
        observation["observation_body_sha256"] = sha256_bytes(canonical_bytes(body))
        observation["observation_sha256"] = object_digest(
            observation,
            "observation_sha256",
        )

        validate_codex_private_sqlite_capacity_observation(observation)
        with self.assertRaisesRegex(BulkloadError, "differs from its request"):
            validate_codex_private_sqlite_capacity_observation_against_request(
                observation,
                request,
                request["output_intent"]["workspace"],
                request["request_runtime_authority"],
            )

    def test_v7_capacity_ttl_remains_fail_closed(self) -> None:
        request = self._frozen_v6_request()
        observation = self._frozen_v6_capacity(request)
        observation["expires_at"] = "2026-07-29T18:11:00Z"
        body = {
            key: value
            for key, value in observation.items()
            if key not in {"observation_body_sha256", "observation_sha256"}
        }
        observation["observation_body_sha256"] = sha256_bytes(canonical_bytes(body))
        observation["observation_sha256"] = object_digest(
            observation,
            "observation_sha256",
        )
        with self.assertRaisesRegex(BulkloadError, "TTL differs"):
            validate_codex_private_sqlite_capacity_observation(observation)


if __name__ == "__main__":
    unittest.main()
