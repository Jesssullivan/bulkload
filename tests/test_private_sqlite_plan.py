from __future__ import annotations

import argparse
from collections.abc import Callable
from copy import deepcopy
import json
from pathlib import Path
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
from bulkload_lib import private_quiescence
from bulkload_lib import private_runtime
from bulkload_lib import private_sqlite_plan
from bulkload_lib import private_state
import bulkload_lib.cli as bulkload_cli
from bulkload_lib.private_sqlite_plan import (
    SQLITE_ADAPTER_REGISTRY_SCHEMA,
    SQLITE_PATH_MAP_SCHEMA,
    validate_codex_private_sqlite_compose_plan,
    validate_codex_private_sqlite_compose_plan_against_inputs,
    validate_sqlite_adapter_registry,
    validate_sqlite_path_map,
)
from bulkload_lib.private_sqlite_close import (
    validate_codex_private_sqlite_close_request,
)
from bulkload_lib.sessions import (
    capture_codex_sessions,
    compile_codex_session_union_plan,
)
from tests.private_sqlite_legacy_fixtures import (
    build_v4_compose_plan_fixture,
    build_v5_close_request_fixture,
)


SOURCE_AUTHORITY = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
DESTINATION_AUTHORITY = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
SESSION_ID = "11111111-1111-4111-8111-111111111111"
LARGE_PAYLOAD_BYTES = 8 * 1024 * 1024 + 17


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


def create_state_database(
    path: Path,
    rollout: Path,
    *,
    payload_bytes: int = 1024,
) -> None:
    connection = sqlite3.connect(path)
    try:
        connection.execute("PRAGMA journal_mode=DELETE")
        connection.execute(
            """
            CREATE TABLE _sqlx_migrations(
                version INTEGER PRIMARY KEY,
                description TEXT NOT NULL,
                installed_on TEXT NOT NULL,
                success INTEGER NOT NULL,
                checksum BLOB NOT NULL,
                execution_time INTEGER NOT NULL
            )
            """
        )
        connection.execute(
            """
            INSERT INTO _sqlx_migrations
            VALUES(40, 'fixture-base', 'now', 1, x'0042', 1)
            """
        )
        connection.execute(
            """
            CREATE TABLE threads(
                id TEXT PRIMARY KEY,
                rollout_path TEXT NOT NULL,
                payload BLOB NOT NULL
            ) STRICT
            """
        )
        connection.execute(
            "INSERT INTO threads VALUES(?, ?, ?)",
            (SESSION_ID, str(rollout), b"\x00" * payload_bytes),
        )
        connection.commit()
    finally:
        connection.close()
    path.chmod(0o600)


def capture_quiescence(
    home: Path,
    output: Path,
    *,
    role: str,
    authority: str,
) -> dict:
    attestation_path = output.parent / f".{output.name}-{uuid.uuid4()}.json"
    attestation = private_quiescence.create_codex_private_quiescence_attestation(
        home,
        attestation_path,
        purpose="capture",
        capture_role=role,
        host_authority_id=authority,
        codex_version="0.145.0",
        selected_state_classes=["sqlite"],
        sqlite_home=home,
        create_only_output=output,
        acknowledge_writers_quiesced=True,
    )
    return private_state.private_quiescence_capture_record(attestation)


def capture_private(
    root: Path,
    home: Path,
    *,
    role: str,
    authority: str,
    pass_name: str,
) -> tuple[dict, Path]:
    output = root / f"{role}-{pass_name}-bundle"
    value = private_state.capture_codex_private_state(
        home,
        output,
        role=role,
        host_authority_id=authority,
        codex_version="0.145.0",
        sqlite_home=home,
        include_auth=False,
        include_sqlite=True,
        acknowledge_private_capture=True,
        quiescence=capture_quiescence(
            home,
            output,
            role=role,
            authority=authority,
        ),
    )
    return value, output


def registry_for(
    source_bundle: Path,
    destination_bundle: Path,
) -> dict:
    deadline = time.monotonic() + 60
    with private_sqlite_plan._open_pinned_snapshot(
        source_bundle,
        "state_5.sqlite",
    ) as source:
        source_schema, source_blockers, source_migrations = (
            private_sqlite_plan._structured_schema(source, deadline=deadline)
        )
    with private_sqlite_plan._open_pinned_snapshot(
        destination_bundle,
        "state_5.sqlite",
    ) as destination:
        destination_schema, destination_blockers, destination_migrations = (
            private_sqlite_plan._structured_schema(
                destination,
                deadline=deadline,
            )
        )
    if source_blockers or destination_blockers:
        raise AssertionError((source_blockers, destination_blockers))
    relation, _ = private_sqlite_plan._migration_relation(
        source_migrations,
        destination_migrations,
    )
    value = {
        "schema": SQLITE_ADAPTER_REGISTRY_SCHEMA,
        "registry_id": "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
        "created_at": "2026-07-29T00:00:00Z",
        "families": [
            {
                "basename": "state_5.sqlite",
                "family_role": "state",
                "generation": 5,
                "source_codex_version": "0.145.0",
                "destination_codex_version": "0.145.0",
                "source_schema_contract_sha256": source_schema[
                    "schema_contract_sha256"
                ],
                "destination_schema_contract_sha256": destination_schema[
                    "schema_contract_sha256"
                ],
                "source_raw_schema_sha256": source_schema["raw_schema_sha256"],
                "destination_raw_schema_sha256": destination_schema[
                    "raw_schema_sha256"
                ],
                "source_migrations_sha256": sha256_bytes(
                    canonical_bytes(source_migrations)
                ),
                "destination_migrations_sha256": sha256_bytes(
                    canonical_bytes(destination_migrations)
                ),
                "source_application_id": source_schema["application_id"],
                "destination_application_id": destination_schema["application_id"],
                "source_user_version": source_schema["user_version"],
                "destination_user_version": destination_schema["user_version"],
                "migration_relation": relation,
                "adapter": None,
                "tables": [
                    {
                        "name": "_sqlx_migrations",
                        "merge_class": "exact",
                        "identity_columns": ["version"],
                    },
                    {
                        "name": "threads",
                        "merge_class": "keyed-union",
                        "identity_columns": ["id"],
                    },
                ],
                "path_authorities": [
                    {
                        "table": "threads",
                        "session_id_column": "id",
                        "column": "rollout_path",
                        "kind": "session-rollout",
                    }
                ],
                "edges": [],
                "allowed_collations": sorted(
                    set(source_schema["collations"])
                    | set(destination_schema["collations"])
                    | {"BINARY", "NOCASE", "RTRIM"}
                ),
            }
        ],
    }
    value["registry_sha256"] = object_digest(value, "registry_sha256")
    validate_sqlite_adapter_registry(value)
    return value


def path_map_for(
    source_sessions: dict,
    destination_sessions: dict,
    session_plan: dict,
) -> dict:
    value = {
        "schema": SQLITE_PATH_MAP_SCHEMA,
        "mapping_id": "dddddddd-dddd-4ddd-8ddd-dddddddddddd",
        "created_at": "2026-07-29T00:00:00Z",
        "source_host_authority_id": SOURCE_AUTHORITY,
        "destination_host_authority_id": DESTINATION_AUTHORITY,
        "codex_version": {
            "source": "0.145.0",
            "destination": "0.145.0",
        },
        "session_union_plan_sha256": session_plan["plan_sha256"],
        "source_session_catalog_sha256": source_sessions["catalog_sha256"],
        "destination_session_catalog_sha256": destination_sessions["catalog_sha256"],
        "source_session_root": source_sessions["resolved_root"],
        "destination_session_root": destination_sessions["resolved_root"],
        "rules": [
            {
                "family_basename": "state_5.sqlite",
                "table": "threads",
                "session_id_column": "id",
                "column": "rollout_path",
                "kind": "session-rollout",
            }
        ],
    }
    value["path_map_sha256"] = object_digest(value, "path_map_sha256")
    validate_sqlite_path_map(value)
    return value


class CodexPrivateSqlitePlanTest(unittest.TestCase):
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

    def compile_fixture(
        self,
        root: Path,
        *,
        payload_bytes: int = 1024,
        database_mutator: Callable[[Path], None] | None = None,
        home_mutator: Callable[[Path], None] | None = None,
        runtime_authority: dict[str, object] | None = None,
    ) -> tuple[dict, dict[str, object]]:
        source_home = root / "source-home"
        destination_home = root / "destination-home"
        source_home.mkdir(mode=0o700)
        destination_home.mkdir(mode=0o700)
        source_sessions_root = source_home / "sessions"
        destination_sessions_root = destination_home / "sessions"
        source_rollout = write_rollout(source_sessions_root)
        destination_rollout = write_rollout(destination_sessions_root)
        create_state_database(
            source_home / "state_5.sqlite",
            source_rollout.resolve(),
            payload_bytes=payload_bytes,
        )
        create_state_database(
            destination_home / "state_5.sqlite",
            destination_rollout.resolve(),
            payload_bytes=payload_bytes,
        )
        if database_mutator is not None:
            database_mutator(source_home / "state_5.sqlite")
            database_mutator(destination_home / "state_5.sqlite")
        if home_mutator is not None:
            home_mutator(source_home)
            home_mutator(destination_home)

        source_private_a, source_a_path = capture_private(
            root,
            source_home,
            role="source",
            authority=SOURCE_AUTHORITY,
            pass_name="a",
        )
        source_private_b, source_b_path = capture_private(
            root,
            source_home,
            role="source",
            authority=SOURCE_AUTHORITY,
            pass_name="b",
        )
        destination_private_a, destination_a_path = capture_private(
            root,
            destination_home,
            role="destination",
            authority=DESTINATION_AUTHORITY,
            pass_name="a",
        )
        destination_private_b, destination_b_path = capture_private(
            root,
            destination_home,
            role="destination",
            authority=DESTINATION_AUTHORITY,
            pass_name="b",
        )
        runtime = (
            private_runtime.current_private_runtime_authority()
            if runtime_authority is None
            else deepcopy(runtime_authority)
        )
        compatibility = private_state.compile_codex_private_state_plan(
            source_a_path,
            destination_a_path,
            runtime_authority=runtime,
        )

        session_source_a = capture_codex_sessions(
            source_sessions_root,
            role="source",
            acknowledge_writers_quiesced=True,
            host_authority_id=SOURCE_AUTHORITY,
        )
        session_source_b = capture_codex_sessions(
            source_sessions_root,
            role="source",
            acknowledge_writers_quiesced=True,
            host_authority_id=SOURCE_AUTHORITY,
        )
        session_destination_a = capture_codex_sessions(
            destination_sessions_root,
            role="destination",
            acknowledge_writers_quiesced=True,
            host_authority_id=DESTINATION_AUTHORITY,
        )
        session_destination_b = capture_codex_sessions(
            destination_sessions_root,
            role="destination",
            acknowledge_writers_quiesced=True,
            host_authority_id=DESTINATION_AUTHORITY,
        )
        session_plan = compile_codex_session_union_plan(
            session_source_a,
            session_source_b,
            session_destination_a,
            session_destination_b,
        )
        registry = registry_for(source_a_path, destination_a_path)
        path_map = path_map_for(
            session_source_a,
            session_destination_a,
            session_plan,
        )
        plan = build_v4_compose_plan_fixture(
            compatibility,
            source_a_path,
            source_b_path,
            destination_a_path,
            destination_b_path,
            adapter_registry=registry,
            path_map=path_map,
            session_union_plan=session_plan,
            session_source_a=session_source_a,
            session_source_b=session_source_b,
            session_destination_a=session_destination_a,
            session_destination_b=session_destination_b,
            runtime_authority=runtime,
            created_at="2026-07-29T00:00:00Z",
        )
        return plan, {
            "compatibility": compatibility,
            "registry": registry,
            "path_map": path_map,
            "source_private_a": source_private_a,
            "source_private_b": source_private_b,
            "destination_private_a": destination_private_a,
            "destination_private_b": destination_private_b,
            "source_a_path": source_a_path,
            "source_b_path": source_b_path,
            "destination_a_path": destination_a_path,
            "destination_b_path": destination_b_path,
            "session_plan": session_plan,
            "session_source_a": session_source_a,
            "session_source_b": session_source_b,
            "session_destination_a": session_destination_a,
            "session_destination_b": session_destination_b,
            "runtime": runtime,
        }

    def validate_against_inputs(
        self,
        plan: dict,
        evidence: dict[str, object],
    ) -> None:
        self.assertIsNone(
            validate_codex_private_sqlite_compose_plan_against_inputs(
                plan,
                evidence["compatibility"],
                evidence["source_a_path"],
                evidence["source_b_path"],
                evidence["destination_a_path"],
                evidence["destination_b_path"],
                adapter_registry=evidence["registry"],
                path_map=evidence["path_map"],
                session_union_plan=evidence["session_plan"],
                session_source_a=evidence["session_source_a"],
                session_source_b=evidence["session_source_b"],
                session_destination_a=evidence["session_destination_a"],
                session_destination_b=evidence["session_destination_b"],
            )
        )

    def classify_records(
        self,
        create_sql: str,
        source_rows: list[tuple[object, object]],
        destination_rows: list[tuple[object, object]],
        *,
        expected_schema_blockers: set[str] | None = None,
    ) -> tuple[dict, list[dict]]:
        source = sqlite3.connect(":memory:")
        destination = sqlite3.connect(":memory:")
        try:
            for connection, rows in (
                (source, source_rows),
                (destination, destination_rows),
            ):
                connection.execute(create_sql)
                connection.executemany(
                    "INSERT INTO records VALUES(?, ?)",
                    rows,
                )
                connection.commit()
            deadline = time.monotonic() + 60
            source_schema, source_blockers, _ = private_sqlite_plan._structured_schema(
                source,
                deadline=deadline,
            )
            destination_schema, destination_blockers, _ = (
                private_sqlite_plan._structured_schema(
                    destination,
                    deadline=deadline,
                )
            )
            expected_codes = expected_schema_blockers or set()
            self.assertEqual(
                {blocker["code"] for blocker in source_blockers},
                expected_codes,
            )
            self.assertEqual(
                {blocker["code"] for blocker in destination_blockers},
                expected_codes,
            )
            return private_sqlite_plan._classify_table(
                source,
                destination,
                basename="state_5.sqlite",
                source_contract=source_schema,
                destination_contract=destination_schema,
                table_rule={
                    "name": "records",
                    "merge_class": "keyed-union",
                    "identity_columns": ["id"],
                },
                registry_family={"path_authorities": []},
                path_map={"rules": []},
                accepted_session_paths={"source": {}, "destination": {}},
                deadline=deadline,
            )
        finally:
            source.close()
            destination.close()

    def test_four_pass_plan_normalizes_paths_and_remains_fail_held(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            plan, _ = self.compile_fixture(
                Path(directory),
                payload_bytes=LARGE_PAYLOAD_BYTES,
            )
            validate_codex_private_sqlite_compose_plan(plan)
            self.assertTrue(plan["opening_stable"])
            self.assertTrue(plan["readiness"]["classification_complete"])
            self.assertFalse(plan["readiness"]["composer_implemented"])
            self.assertFalse(plan["readiness"]["sqlite_union_ready"])
            self.assertFalse(plan["readiness"]["sqlite_publish"])
            self.assertFalse(plan["readiness"]["ready_for_apply"])
            self.assertEqual(
                {blocker["code"] for blocker in plan["blockers"]},
                {
                    "post-plan-private-close-required",
                    "sqlite-composer-not-implemented",
                    "session-union-execution-and-verification-not-implemented",
                },
            )
            threads = next(
                item
                for item in plan["sqlite_families"][0]["table_relations"]
                if item["name"] == "threads"
            )
            self.assertEqual(threads["shared_equal"], 1)
            self.assertEqual(threads["source_only"], 0)
            self.assertEqual(threads["destination_only"], 0)
            self.assertEqual(threads["conflicts"], 0)
            self.assertGreater(
                threads["source"]["classified_bytes"],
                LARGE_PAYLOAD_BYTES,
            )
            encoded = canonical_bytes(plan)
            self.assertNotIn(b"rollout-2026-07-29", encoded)
            self.assertNotIn(b"\\x00\\x00\\x00", encoded)

    def test_v5_close_recomputes_the_complete_real_v4_opening(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            legacy_runtime = deepcopy(
                private_runtime.LEGACY_PRIVATE_RUNTIME_AUTHORITY_V5_REPAIRED
            )
            plan, evidence = self.compile_fixture(
                Path(directory),
                runtime_authority=legacy_runtime,
            )
            request = build_v5_close_request_fixture(
                plan,
                evidence["session_plan"],
                evidence["session_source_a"],
                evidence["session_source_b"],
                evidence["session_destination_a"],
                evidence["session_destination_b"],
                evidence["runtime"],
                writer_stop_epoch_id=str(uuid.uuid4()),
                writer_stop_epoch_at="2026-07-29T00:00:00Z",
                created_at="2026-07-29T00:00:00Z",
            )
            validate_codex_private_sqlite_close_request(request)
            self.assertTrue(request["opening_inputs_revalidated"])
            self.assertEqual(
                request["opening_plan"]["plan_sha256"],
                plan["plan_sha256"],
            )
            self.assertEqual(request["runtime_authority"], evidence["runtime"])

    def test_tampered_registry_and_path_map_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            plan, evidence = self.compile_fixture(Path(directory))
            registry = json.loads(json.dumps(evidence["registry"]))
            registry["families"][0]["tables"][0]["merge_class"] = "keyed-union"
            with self.assertRaisesRegex(BulkloadError, "digest mismatch"):
                validate_sqlite_adapter_registry(registry)

            missing_registry_path = json.loads(json.dumps(evidence["registry"]))
            missing_registry_path["families"][0]["path_authorities"] = []
            missing_registry_path["registry_sha256"] = object_digest(
                missing_registry_path,
                "registry_sha256",
            )
            with self.assertRaisesRegex(
                BulkloadError,
                "v4 exact authority",
            ):
                validate_sqlite_adapter_registry(missing_registry_path)

            path_map = json.loads(json.dumps(evidence["path_map"]))
            path_map["source_session_root"] += "-near-miss"
            path_map["path_map_sha256"] = object_digest(
                path_map,
                "path_map_sha256",
            )
            validate_sqlite_path_map(path_map)
            with self.assertRaisesRegex(
                BulkloadError,
                "does not bind the session closure",
            ):
                validate_codex_private_sqlite_compose_plan_against_inputs(
                    plan,
                    evidence["compatibility"],
                    evidence["source_a_path"],
                    evidence["source_b_path"],
                    evidence["destination_a_path"],
                    evidence["destination_b_path"],
                    adapter_registry=evidence["registry"],
                    path_map=path_map,
                    session_union_plan=evidence["session_plan"],
                    session_source_a=evidence["session_source_a"],
                    session_source_b=evidence["session_source_b"],
                    session_destination_a=evidence["session_destination_a"],
                    session_destination_b=evidence["session_destination_b"],
                )

            missing_mapped_path = json.loads(json.dumps(evidence["path_map"]))
            missing_mapped_path["rules"] = []
            missing_mapped_path["path_map_sha256"] = object_digest(
                missing_mapped_path,
                "path_map_sha256",
            )
            with self.assertRaisesRegex(
                BulkloadError,
                "v4 exact authority",
            ):
                validate_sqlite_path_map(missing_mapped_path)

            unregistered = json.loads(json.dumps(evidence["registry"]))
            unregistered["families"] = []
            unregistered["registry_sha256"] = object_digest(
                unregistered,
                "registry_sha256",
            )
            unregistered_plan = build_v4_compose_plan_fixture(
                evidence["compatibility"],
                evidence["source_a_path"],
                evidence["source_b_path"],
                evidence["destination_a_path"],
                evidence["destination_b_path"],
                adapter_registry=unregistered,
                path_map=evidence["path_map"],
                session_union_plan=evidence["session_plan"],
                session_source_a=evidence["session_source_a"],
                session_source_b=evidence["session_source_b"],
                session_destination_a=evidence["session_destination_a"],
                session_destination_b=evidence["session_destination_b"],
                runtime_authority=evidence["runtime"],
                created_at="2026-07-29T00:00:00Z",
            )
            self.assertIn(
                "sqlite-adapter-not-registered",
                {
                    blocker["code"]
                    for blocker in unregistered_plan["sqlite_families"][0]["blockers"]
                },
            )

    def test_stable_pair_and_typed_digest_collisions_are_rejected(self) -> None:
        self.assertNotEqual(
            private_sqlite_plan._typed_value("integer", 1),
            private_sqlite_plan._typed_value("text", "1"),
        )
        self.assertNotEqual(
            private_sqlite_plan._typed_value("null", None),
            private_sqlite_plan._typed_value("text", ""),
        )
        self.assertNotEqual(
            private_sqlite_plan._typed_value("blob", b"x"),
            private_sqlite_plan._typed_value("text", "x"),
        )
        self.assertNotEqual(
            private_sqlite_plan._typed_value("real", -0.0),
            private_sqlite_plan._typed_value("real", 0.0),
        )
        with tempfile.TemporaryDirectory() as directory:
            _, evidence = self.compile_fixture(Path(directory))
            drifted = json.loads(json.dumps(evidence["source_private_b"]))
            drifted["sqlite_live_namespace_sha256"] = "0" * 64
            with self.assertRaisesRegex(
                BulkloadError,
                "sqlite-opening-stability-mismatch",
            ):
                private_sqlite_plan._private_pair_binding(
                    evidence["source_private_a"],
                    drifted,
                    role="source",
                )

    def test_sql_sort_order_drives_cross_input_merge(self) -> None:
        relation, blockers = self.classify_records(
            """
            CREATE TABLE records(
                id TEXT PRIMARY KEY,
                payload TEXT NOT NULL
            ) STRICT
            """,
            [("aa", "source-only"), ("b", "source")],
            [("b", "destination")],
        )
        self.assertEqual(relation["source_only"], 1)
        self.assertEqual(relation["destination_only"], 0)
        self.assertEqual(relation["conflicts"], 1)
        self.assertIn(
            "sqlite-shared-row-divergence",
            {blocker["code"] for blocker in blockers},
        )

    def test_unsafe_and_secondary_unique_claims_fail_held(self) -> None:
        for create_sql in (
            """
            CREATE TABLE records(
                id TEXT NOT NULL COLLATE NOCASE UNIQUE,
                payload TEXT NOT NULL
            ) STRICT
            """,
            """
            CREATE TABLE records(
                id TEXT UNIQUE,
                payload TEXT NOT NULL
            ) STRICT
            """,
        ):
            with self.subTest(create_sql=create_sql):
                relation, blockers = self.classify_records(
                    create_sql,
                    [("A", "source")],
                    [("a", "destination")],
                    expected_schema_blockers=(
                        {"sqlite-column-collation-semantics-not-classified"}
                        if "COLLATE" in create_sql
                        else set()
                    ),
                )
                self.assertIsNone(relation["source"])
                self.assertFalse(relation["semantic_classification_complete"])
                self.assertIn(
                    "sqlite-identity-unique-semantics-unsupported",
                    {blocker["code"] for blocker in blockers},
                )

        relation, blockers = self.classify_records(
            """
            CREATE TABLE records(
                id TEXT PRIMARY KEY,
                payload TEXT NOT NULL UNIQUE
            ) STRICT
            """,
            [("a", "same")],
            [("a", "same")],
        )
        self.assertEqual(relation["shared_equal"], 1)
        self.assertFalse(relation["semantic_classification_complete"])
        self.assertIn(
            "sqlite-secondary-unique-closure-not-implemented",
            {blocker["code"] for blocker in blockers},
        )

    def test_observed_edges_triggers_and_views_are_explicit_blockers(self) -> None:
        def add_relational_schema(path: Path) -> None:
            connection = sqlite3.connect(path)
            try:
                connection.execute("PRAGMA foreign_keys=ON")
                connection.execute("CREATE TABLE parent(id INTEGER PRIMARY KEY)")
                connection.execute(
                    """
                    CREATE TABLE child(
                        id INTEGER PRIMARY KEY,
                        parent_id INTEGER NOT NULL,
                        FOREIGN KEY(parent_id) REFERENCES parent(id)
                            ON UPDATE CASCADE ON DELETE RESTRICT
                    )
                    """
                )
                connection.execute(
                    """
                    CREATE TRIGGER child_touch
                    AFTER INSERT ON child
                    BEGIN
                        UPDATE parent SET id = id WHERE id = NEW.parent_id;
                    END
                    """
                )
                connection.execute("CREATE VIEW child_view AS SELECT id FROM child")
                connection.commit()
            finally:
                connection.close()

        with tempfile.TemporaryDirectory() as directory:
            plan, _ = self.compile_fixture(
                Path(directory),
                database_mutator=add_relational_schema,
            )
            family = plan["sqlite_families"][0]
            codes = {blocker["code"] for blocker in family["blockers"]}
            self.assertIn("sqlite-edge-registry-mismatch", codes)
            self.assertIn(
                "sqlite-post-compose-edge-closure-required",
                codes,
            )
            self.assertIn(
                "sqlite-trigger-semantics-not-classified",
                codes,
            )
            self.assertIn("sqlite-view-semantics-not-classified", codes)
            self.assertFalse(family["classification_complete"])
            self.assertFalse(family["edge_closure"]["registry_matches_observed"])
            self.assertNotEqual(
                family["edge_closure"]["source_observed_sha256"],
                sha256_bytes(canonical_bytes([])),
            )
            validate_codex_private_sqlite_compose_plan(plan)

    def test_global_evidence_ids_are_disjoint(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            plan, _ = self.compile_fixture(Path(directory))
            tampered = json.loads(json.dumps(plan))
            tampered["private_opening"]["destination"]["quiescence_attestation_ids"][
                0
            ] = tampered["private_opening"]["source"]["quiescence_attestation_ids"][0]
            tampered["plan_sha256"] = object_digest(
                tampered,
                "plan_sha256",
            )
            with self.assertRaisesRegex(
                BulkloadError,
                "evidence IDs are not globally distinct",
            ):
                validate_codex_private_sqlite_compose_plan(tampered)

            repeated_capture_digest = json.loads(json.dumps(plan))
            repeated_capture_digest["private_opening"]["source"]["capture_sha256s"][
                1
            ] = repeated_capture_digest["private_opening"]["source"]["capture_sha256s"][
                0
            ]
            repeated_capture_digest["plan_sha256"] = object_digest(
                repeated_capture_digest,
                "plan_sha256",
            )
            with self.assertRaisesRegex(
                BulkloadError,
                "capture digests are reused",
            ):
                validate_codex_private_sqlite_compose_plan(repeated_capture_digest)

            malformed_session = json.loads(json.dumps(plan))
            malformed_session["session_union"]["source"] = {}
            malformed_session["plan_sha256"] = object_digest(
                malformed_session,
                "plan_sha256",
            )
            with self.assertRaisesRegex(
                BulkloadError,
                "source binding is invalid",
            ):
                validate_codex_private_sqlite_compose_plan(malformed_session)

            cross_plane_reuse = json.loads(json.dumps(plan))
            session_capture_id = cross_plane_reuse["session_union"]["source"][
                "capture_ids"
            ][0]
            cross_plane_reuse["private_opening"]["source"][
                "quiescence_attestation_ids"
            ][0] = str(uuid.UUID(hex=session_capture_id))
            cross_plane_reuse["plan_sha256"] = object_digest(
                cross_plane_reuse,
                "plan_sha256",
            )
            with self.assertRaisesRegex(
                BulkloadError,
                "private and session evidence reuse an evidence ID",
            ):
                validate_codex_private_sqlite_compose_plan(cross_plane_reuse)

    def test_equal_lexical_roots_are_scoped_by_host_authority(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            _, evidence = self.compile_fixture(Path(directory))
            path_map = json.loads(json.dumps(evidence["path_map"]))
            shared_root = path_map["source_session_root"]
            path_map["destination_session_root"] = shared_root
            path_map["path_map_sha256"] = object_digest(
                path_map,
                "path_map_sha256",
            )
            validate_sqlite_path_map(path_map)
            relative_path = private_sqlite_plan._session_paths(
                evidence["session_plan"]
            )["source"][SESSION_ID]
            translated = private_sqlite_plan._translate_rollout_path(
                str(Path(shared_root) / relative_path),
                role="source",
                source_root=shared_root,
                destination_root=shared_root,
                expected_relative_path=relative_path,
            )
            self.assertEqual(
                translated,
                str(Path(shared_root) / relative_path),
            )

    def test_rollout_path_must_match_exact_session_path(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            with self.assertRaisesRegex(
                BulkloadError,
                "does not match its accepted session UUID",
            ):
                private_sqlite_plan._translate_rollout_path(
                    str(root / "wrong.jsonl"),
                    role="source",
                    source_root=str(root),
                    destination_root=str(root / "destination"),
                    expected_relative_path="expected.jsonl",
                )

    def test_migration_types_and_registered_40_to_42_prefix(self) -> None:
        connection = sqlite3.connect(":memory:")
        try:
            connection.execute(
                """
                CREATE TABLE _sqlx_migrations(
                    version INTEGER PRIMARY KEY,
                    description TEXT NOT NULL,
                    success INTEGER NOT NULL,
                    checksum BLOB NOT NULL
                )
                """
            )
            connection.execute(
                "INSERT INTO _sqlx_migrations VALUES(40, 'bad', 2, x'00')"
            )
            _, blockers, migrations = private_sqlite_plan._structured_schema(
                connection,
                deadline=time.monotonic() + 60,
            )
            self.assertEqual(migrations, [])
            self.assertIn(
                "sqlite-migration-record-type-invalid",
                {blocker["code"] for blocker in blockers},
            )
        finally:
            connection.close()

        destination = [[40, "base", "00", True]]
        source = [
            *destination,
            [41, "middle", "01", True],
            [42, "current", "02", True],
        ]
        relation, details = private_sqlite_plan._migration_relation(
            source,
            destination,
        )
        self.assertEqual(relation, "registered-prefix-upgrade")
        self.assertEqual(details["common_prefix_count"], 1)
        self.assertEqual(details["source_count"], 3)
        self.assertEqual(details["destination_count"], 1)

        with tempfile.TemporaryDirectory() as directory:
            _, evidence = self.compile_fixture(Path(directory))
            registry = json.loads(json.dumps(evidence["registry"]))
            family = registry["families"][0]
            family["migration_relation"] = "registered-prefix-upgrade"
            family["source_migrations_sha256"] = sha256_bytes(canonical_bytes(source))
            family["destination_migrations_sha256"] = sha256_bytes(
                canonical_bytes(destination)
            )
            family["adapter"] = {
                "adapter_id": "state-40-to-42",
                "adapter_source_sha256": "1" * 64,
            }
            registry["registry_sha256"] = object_digest(
                registry,
                "registry_sha256",
            )
            validate_sqlite_adapter_registry(registry)
            family["adapter"] = None
            registry["registry_sha256"] = object_digest(
                registry,
                "registry_sha256",
            )
            with self.assertRaises(BulkloadError):
                validate_sqlite_adapter_registry(registry)

    def test_autoincrement_internal_state_is_fail_held(self) -> None:
        connection = sqlite3.connect(":memory:")
        try:
            connection.execute(
                """
                CREATE TABLE records(
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    payload TEXT NOT NULL
                ) STRICT
                """
            )
            connection.execute("INSERT INTO records(payload) VALUES('first')")
            schema, blockers, _ = private_sqlite_plan._structured_schema(
                connection,
                deadline=time.monotonic() + 60,
            )
            blocker = {
                "code": "sqlite-internal-table-state-not-classified",
                "name": "sqlite_sequence",
            }
            self.assertIn(blocker, blockers)
            self.assertIn(blocker, schema["classification_blockers"])
            private_sqlite_plan._validate_schema_contract(
                schema,
                label="autoincrement schema",
            )
        finally:
            connection.close()

    def test_virtual_schema_catalog_remains_digest_bound_while_fail_held(
        self,
    ) -> None:
        connection = sqlite3.connect(":memory:")
        try:
            connection.execute("CREATE VIRTUAL TABLE search USING fts5(content)")
            schema, blockers, _ = private_sqlite_plan._structured_schema(
                connection,
                deadline=time.monotonic() + 60,
            )
            self.assertIn(
                "sqlite-virtual-or-shadow-table-unsupported",
                {blocker["code"] for blocker in blockers},
            )
            self.assertTrue(schema["schema_records"])
            self.assertEqual(
                schema["raw_schema_sha256"],
                sha256_bytes(canonical_bytes(schema["schema_records"])),
            )
            private_sqlite_plan._validate_schema_contract(
                schema,
                label="virtual schema regression",
            )
        finally:
            connection.close()

    def test_unknown_family_is_reported_fail_held_instead_of_aborting(self) -> None:
        def add_unknown_family(home: Path) -> None:
            connection = sqlite3.connect(home / "custom.sqlite")
            try:
                connection.execute(
                    "CREATE TABLE records(id INTEGER PRIMARY KEY, value TEXT) STRICT"
                )
                connection.execute("INSERT INTO records VALUES(1, 'value')")
                connection.commit()
            finally:
                connection.close()
            (home / "custom.sqlite").chmod(0o600)

        with tempfile.TemporaryDirectory() as directory:
            plan, _ = self.compile_fixture(
                Path(directory),
                home_mutator=add_unknown_family,
            )
            custom = next(
                family
                for family in plan["sqlite_families"]
                if family["basename"] == "custom.sqlite"
            )
            self.assertEqual(custom["family_role"], "unsupported")
            self.assertIsNone(custom["generation"])
            self.assertFalse(custom["classification_complete"])
            self.assertEqual(
                {blocker["code"] for blocker in custom["blockers"]},
                {
                    "sqlite-adapter-not-registered",
                    "sqlite-family-unsupported",
                },
            )
            self.assertFalse(plan["readiness"]["classification_complete"])
            validate_codex_private_sqlite_compose_plan(plan)

    def test_column_collation_and_deadline_guards_fail_closed(self) -> None:
        connection = sqlite3.connect(":memory:")
        try:
            connection.create_collation(
                "CUSTOM",
                lambda left, right: (left > right) - (left < right),
            )
            connection.execute(
                """
                CREATE TABLE records(
                    id TEXT PRIMARY KEY,
                    payload TEXT COLLATE CUSTOM
                ) STRICT
                """
            )
            _, blockers, _ = private_sqlite_plan._structured_schema(
                connection,
                deadline=time.monotonic() + 60,
            )
            self.assertIn(
                {
                    "code": "sqlite-column-collation-semantics-not-classified",
                    "table": "records",
                },
                blockers,
            )
            self.assertFalse(
                private_sqlite_plan._sql_has_keyword(
                    "CREATE TABLE t(v TEXT DEFAULT 'COLLATE')",
                    "COLLATE",
                )
            )

            with mock.patch.object(
                private_sqlite_plan.time,
                "monotonic",
                side_effect=[0.0, 2.0, 2.0],
            ):
                with self.assertRaisesRegex(BulkloadError, "exceeded its deadline"):
                    with private_sqlite_plan._sqlite_deadline_guard(
                        connection,
                        deadline=1.0,
                        label="SQLite test query",
                    ):
                        connection.execute(
                            """
                            WITH RECURSIVE numbers(value) AS (
                                SELECT 1
                                UNION ALL
                                SELECT value + 1 FROM numbers WHERE value < 100000
                            )
                            SELECT sum(value) FROM numbers
                            """
                        ).fetchone()
        finally:
            connection.close()

    def test_structural_readiness_migration_and_artifact_bindings(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            plan, _ = self.compile_fixture(Path(directory))

            table_false = json.loads(json.dumps(plan))
            table_false["sqlite_families"][0]["table_relations"][0][
                "semantic_classification_complete"
            ] = False
            table_false["plan_sha256"] = object_digest(
                table_false,
                "plan_sha256",
            )
            with self.assertRaisesRegex(
                BulkloadError,
                "table classification readiness differs",
            ):
                validate_codex_private_sqlite_compose_plan(table_false)

            missing_relations = json.loads(json.dumps(plan))
            missing_relations["sqlite_families"][0]["table_relations"] = []
            missing_relations["plan_sha256"] = object_digest(
                missing_relations,
                "plan_sha256",
            )
            with self.assertRaisesRegex(
                BulkloadError,
                "do not cover the visible schema",
            ):
                validate_codex_private_sqlite_compose_plan(missing_relations)

            exact_divergence = json.loads(json.dumps(plan))
            migration_table = exact_divergence["sqlite_families"][0]["table_relations"][
                0
            ]
            migration_table["shared_equal"] = 0
            migration_table["source_only"] = 1
            migration_table["destination_only"] = 1
            exact_divergence["plan_sha256"] = object_digest(
                exact_divergence,
                "plan_sha256",
            )
            with self.assertRaisesRegex(BulkloadError, "readiness is invalid"):
                validate_codex_private_sqlite_compose_plan(exact_divergence)

            exact_digest_drift = json.loads(json.dumps(plan))
            exact_relation = exact_digest_drift["sqlite_families"][0][
                "table_relations"
            ][0]
            exact_relation["source"]["semantic_rows_sha256"] = "4" * 64
            exact_digest_drift["plan_sha256"] = object_digest(
                exact_digest_drift,
                "plan_sha256",
            )
            with self.assertRaisesRegex(BulkloadError, "readiness is invalid"):
                validate_codex_private_sqlite_compose_plan(exact_digest_drift)

            keyed_digest_drift = json.loads(json.dumps(plan))
            keyed_relation = next(
                relation
                for relation in keyed_digest_drift["sqlite_families"][0][
                    "table_relations"
                ]
                if relation["name"] == "threads"
            )
            keyed_relation["source"]["semantic_rows_sha256"] = "6" * 64
            keyed_digest_drift["plan_sha256"] = object_digest(
                keyed_digest_drift,
                "plan_sha256",
            )
            with self.assertRaisesRegex(BulkloadError, "readiness is invalid"):
                validate_codex_private_sqlite_compose_plan(keyed_digest_drift)

            exact_adapter = json.loads(json.dumps(plan))
            exact_adapter["sqlite_families"][0]["migration_relation"]["adapter"] = {
                "adapter_id": "not-valid-for-exact",
                "adapter_source_sha256": "1" * 64,
            }
            exact_adapter["plan_sha256"] = object_digest(
                exact_adapter,
                "plan_sha256",
            )
            with self.assertRaisesRegex(
                BulkloadError,
                "exact migration claim differs",
            ):
                validate_codex_private_sqlite_compose_plan(exact_adapter)

            bad_migration_count = json.loads(json.dumps(plan))
            bad_migration_count["sqlite_families"][0]["migration_relation"][
                "common_prefix_count"
            ] = 0
            bad_migration_count["plan_sha256"] = object_digest(
                bad_migration_count,
                "plan_sha256",
            )
            with self.assertRaisesRegex(
                BulkloadError,
                "migration empty-set digest differs",
            ):
                validate_codex_private_sqlite_compose_plan(bad_migration_count)

            falsely_blocked = json.loads(json.dumps(plan))
            falsely_blocked["sqlite_families"][0]["migration_relation"]["relation"] = (
                "blocked"
            )
            falsely_blocked["plan_sha256"] = object_digest(
                falsely_blocked,
                "plan_sha256",
            )
            with self.assertRaisesRegex(
                BulkloadError,
                "blocked migration claim differs",
            ):
                validate_codex_private_sqlite_compose_plan(falsely_blocked)

            false_prefix = json.loads(json.dumps(plan))
            false_prefix_relation = false_prefix["sqlite_families"][0][
                "migration_relation"
            ]
            false_prefix_relation.update(
                {
                    "relation": "registered-prefix-upgrade",
                    "source_count": 2,
                    "destination_count": 1,
                    "common_prefix_count": 1,
                    "source_tail_sha256": "2" * 64,
                    "adapter": {
                        "adapter_id": "forged-prefix",
                        "adapter_source_sha256": "3" * 64,
                    },
                }
            )
            false_prefix["plan_sha256"] = object_digest(
                false_prefix,
                "plan_sha256",
            )
            with self.assertRaisesRegex(
                BulkloadError,
                "prefix-upgrade migration claim differs",
            ):
                validate_codex_private_sqlite_compose_plan(false_prefix)

            bad_header = json.loads(json.dumps(plan))
            bad_header["sqlite_families"][0]["source_artifact"]["application_id"] += 1
            bad_header["plan_sha256"] = object_digest(
                bad_header,
                "plan_sha256",
            )
            with self.assertRaisesRegex(
                BulkloadError,
                "artifact, schema, and migration bindings differ",
            ):
                validate_codex_private_sqlite_compose_plan(bad_header)

            bad_capture = json.loads(json.dumps(plan))
            bad_capture["sqlite_families"][0]["source_artifact"]["capture_sha256"] = (
                "f" * 64
            )
            bad_capture["plan_sha256"] = object_digest(
                bad_capture,
                "plan_sha256",
            )
            with self.assertRaisesRegex(
                BulkloadError,
                "artifact differs from its opening capture",
            ):
                validate_codex_private_sqlite_compose_plan(bad_capture)

            over_budget = json.loads(json.dumps(plan))
            relation = next(
                item
                for item in over_budget["sqlite_families"][0]["table_relations"]
                if item["name"] == "threads"
            )
            relation["shared_equal"] = private_sqlite_plan.MAX_SQLITE_PLAN_ROWS + 1
            for role in ("source", "destination"):
                relation[role]["row_count"] = relation["shared_equal"]
                relation[role]["classified_bytes"] = (
                    private_sqlite_plan.MAX_SQLITE_PLAN_ROW_BYTES + 1
                )
            over_budget["plan_sha256"] = object_digest(
                over_budget,
                "plan_sha256",
            )
            with self.assertRaisesRegex(
                BulkloadError,
                "persisted semantic classification budget exceeded",
            ):
                validate_codex_private_sqlite_compose_plan(over_budget)

            cross_wired = json.loads(json.dumps(plan))
            cross_wired["private_opening"]["source"]["host_authority_id"] = (
                "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee"
            )
            cross_wired["plan_sha256"] = object_digest(
                cross_wired,
                "plan_sha256",
            )
            with self.assertRaisesRegex(
                BulkloadError,
                "differs from its stable projection",
            ):
                validate_codex_private_sqlite_compose_plan(cross_wired)

            raw_blocker = json.loads(json.dumps(plan))
            family = raw_blocker["sqlite_families"][0]
            family["blockers"] = [
                {
                    "code": "sqlite-trigger-semantics-not-classified",
                    "raw": "must-not-enter-plan",
                }
            ]
            family["classification_complete"] = False
            raw_blocker["blockers"] = private_sqlite_plan._dedupe_blockers(
                [
                    *raw_blocker["blockers"],
                    {
                        "basename": family["basename"],
                        **family["blockers"][0],
                    },
                ]
            )
            raw_blocker["readiness"]["classification_complete"] = False
            raw_blocker["plan_sha256"] = object_digest(
                raw_blocker,
                "plan_sha256",
            )
            with self.assertRaisesRegex(BulkloadError, "keys differ"):
                validate_codex_private_sqlite_compose_plan(raw_blocker)

    def test_structural_validator_rejects_self_redigested_counterexamples(
        self,
    ) -> None:
        def redigest_projection(value: dict, role: str) -> None:
            binding = value["private_opening"][role]
            binding["stable_projection_sha256"] = sha256_bytes(
                canonical_bytes(binding["stable_projection"])
            )

        def redigest_schema(schema: dict) -> None:
            schema["schema_contract_sha256"] = sha256_bytes(
                canonical_bytes(
                    {
                        key: item
                        for key, item in schema.items()
                        if key != "schema_contract_sha256"
                    }
                )
            )

        def redigest_plan(value: dict) -> None:
            value["plan_sha256"] = object_digest(value, "plan_sha256")

        def table_relation(value: dict, name: str) -> dict:
            return next(
                relation
                for relation in value["sqlite_families"][0]["table_relations"]
                if relation["name"] == name
            )

        with tempfile.TemporaryDirectory() as directory:
            plan, _ = self.compile_fixture(Path(directory))

            injected_projection = json.loads(json.dumps(plan))
            injected_projection["private_opening"]["source"]["stable_projection"][
                "raw"
            ] = "must-not-enter-plan"
            redigest_projection(injected_projection, "source")
            redigest_plan(injected_projection)
            with self.assertRaisesRegex(BulkloadError, "keys differ"):
                validate_codex_private_sqlite_compose_plan(injected_projection)

            injected_family = json.loads(json.dumps(plan))
            injected_family["private_opening"]["source"]["stable_projection"][
                "sqlite_families"
            ][0]["raw"] = "must-not-enter-plan"
            redigest_projection(injected_family, "source")
            redigest_plan(injected_family)
            with self.assertRaisesRegex(BulkloadError, "keys differ"):
                validate_codex_private_sqlite_compose_plan(injected_family)

            undersized_budgets = json.loads(json.dumps(plan))
            configurable_budgets = {
                "max_sqlite_families",
                "max_total_sqlite_bytes",
                "backup_timeout_seconds",
                "max_thread_entries",
                "max_thread_index_bytes",
                "max_metadata_entries",
                "max_metadata_bytes",
            }
            for role in ("source", "destination"):
                budgets = undersized_budgets["private_opening"][role][
                    "stable_projection"
                ]["budgets"]
                for key in configurable_budgets:
                    budgets[key] = 1
                redigest_projection(undersized_budgets, role)
            redigest_plan(undersized_budgets)
            with self.assertRaisesRegex(BulkloadError, "exceeds its capture budget"):
                validate_codex_private_sqlite_compose_plan(undersized_budgets)

            missing_latest = json.loads(json.dumps(plan))
            for role in ("source", "destination"):
                missing_latest["private_opening"][role]["stable_projection"][
                    "sqlite_families"
                ][0]["latest_migration"] = None
                redigest_projection(missing_latest, role)
            redigest_plan(missing_latest)
            with self.assertRaisesRegex(
                BulkloadError,
                "latest migration is invalid",
            ):
                validate_codex_private_sqlite_compose_plan(missing_latest)

            forged_latest = json.loads(json.dumps(plan))
            for role in ("source", "destination"):
                forged_latest["private_opening"][role]["stable_projection"][
                    "sqlite_families"
                ][0]["latest_migration"] += 1
                redigest_projection(forged_latest, role)
            redigest_plan(forged_latest)
            with self.assertRaisesRegex(
                BulkloadError,
                "projected migration metadata differs",
            ):
                validate_codex_private_sqlite_compose_plan(forged_latest)

            forged_raw_schema = json.loads(json.dumps(plan))
            for role in ("source_schema", "destination_schema"):
                schema = forged_raw_schema["sqlite_families"][0][role]
                schema["raw_schema_sha256"] = "7" * 64
                redigest_schema(schema)
            redigest_plan(forged_raw_schema)
            with self.assertRaisesRegex(
                BulkloadError,
                "raw-schema digest differs from schema records",
            ):
                validate_codex_private_sqlite_compose_plan(forged_raw_schema)

            unblocked_raw_omission = json.loads(json.dumps(plan))
            for role in ("source_schema", "destination_schema"):
                schema = unblocked_raw_omission["sqlite_families"][0][role]
                schema["schema_records"].append(
                    {
                        "type": "table",
                        "name": "unrepresented",
                        "table": "unrepresented",
                        "sql_sha256": "8" * 64,
                    }
                )
                schema["schema_records"].sort(
                    key=lambda record: (record["type"], record["name"])
                )
                schema["raw_schema_sha256"] = sha256_bytes(
                    canonical_bytes(schema["schema_records"])
                )
                redigest_schema(schema)
            redigest_plan(unblocked_raw_omission)
            with self.assertRaisesRegex(
                BulkloadError,
                "raw-schema omissions differ",
            ):
                validate_codex_private_sqlite_compose_plan(unblocked_raw_omission)

            wrong_omission_type = json.loads(json.dumps(plan["sqlite_families"][0]))
            for role in ("source_schema", "destination_schema"):
                schema = wrong_omission_type[role]
                schema["schema_records"].append(
                    {
                        "type": "table",
                        "name": "unrepresented",
                        "table": "unrepresented",
                        "sql_sha256": "8" * 64,
                    }
                )
                schema["schema_records"].sort(
                    key=lambda record: (record["type"], record["name"])
                )
                schema["omitted_table_objects"] = [
                    {"name": "unrepresented", "type": "virtual"}
                ]
                schema["raw_schema_sha256"] = sha256_bytes(
                    canonical_bytes(schema["schema_records"])
                )
                redigest_schema(schema)
            wrong_omission_type["blockers"] = private_sqlite_plan._dedupe_blockers(
                [
                    {
                        "code": "sqlite-virtual-or-shadow-table-unsupported",
                        "role": "source",
                        "name": "unrepresented",
                        "type": "shadow",
                    },
                    {
                        "code": "sqlite-virtual-or-shadow-table-unsupported",
                        "role": "destination",
                        "name": "unrepresented",
                        "type": "virtual",
                    },
                ]
            )
            wrong_omission_type["classification_complete"] = False
            with self.assertRaisesRegex(
                BulkloadError,
                "omissions differ from blockers",
            ):
                private_sqlite_plan._validate_family_relation(wrong_omission_type)

            missing_role_blocker = json.loads(json.dumps(plan["sqlite_families"][0]))
            for role in ("source_schema", "destination_schema"):
                schema = missing_role_blocker[role]
                schema["classification_blockers"] = [
                    {"code": "sqlite-migration-not-successful"}
                ]
                redigest_schema(schema)
            with self.assertRaisesRegex(
                BulkloadError,
                "role classification blockers differ",
            ):
                private_sqlite_plan._validate_family_relation(missing_role_blocker)

            role_injected_schema = json.loads(
                json.dumps(plan["sqlite_families"][0]["source_schema"])
            )
            role_injected_schema["classification_blockers"] = [
                {
                    "code": "sqlite-migration-not-successful",
                    "role": "source",
                }
            ]
            redigest_schema(role_injected_schema)
            with self.assertRaisesRegex(
                BulkloadError,
                "role-local fields",
            ):
                private_sqlite_plan._validate_schema_contract(
                    role_injected_schema,
                    label="role-injected schema",
                )

            forged_structured_schema = json.loads(json.dumps(plan))
            for role in ("source_schema", "destination_schema"):
                schema = forged_structured_schema["sqlite_families"][0][role]
                threads = next(
                    table for table in schema["tables"] if table["name"] == "threads"
                )
                threads["create_sql_sha256"] = "9" * 64
                raw_threads = next(
                    record
                    for record in schema["schema_records"]
                    if record["type"] == "table" and record["name"] == "threads"
                )
                raw_threads["sql_sha256"] = "9" * 64
                schema["raw_schema_sha256"] = sha256_bytes(
                    canonical_bytes(schema["schema_records"])
                )
                redigest_schema(schema)
            redigest_plan(forged_structured_schema)
            with self.assertRaisesRegex(
                BulkloadError,
                "adapter exact-key blocker differs",
            ):
                validate_codex_private_sqlite_compose_plan(forged_structured_schema)

            forged_migration_counts = json.loads(json.dumps(plan))
            migration = forged_migration_counts["sqlite_families"][0][
                "migration_relation"
            ]
            migration["source_count"] += 1
            migration["destination_count"] += 1
            migration["common_prefix_count"] += 1
            redigest_plan(forged_migration_counts)
            with self.assertRaisesRegex(
                BulkloadError,
                "migration table count differs",
            ):
                validate_codex_private_sqlite_compose_plan(forged_migration_counts)

            forged_empty_migration = json.loads(json.dumps(plan))
            migration = forged_empty_migration["sqlite_families"][0][
                "migration_relation"
            ]
            migration["source_count"] = 0
            migration["destination_count"] = 0
            migration["source_latest_migration"] = None
            migration["destination_latest_migration"] = None
            migration["common_prefix_count"] = 0
            redigest_plan(forged_empty_migration)
            with self.assertRaisesRegex(
                BulkloadError,
                "empty-set digest differs",
            ):
                validate_codex_private_sqlite_compose_plan(forged_empty_migration)

            undercharged = json.loads(json.dumps(plan))
            relation = table_relation(undercharged, "threads")
            relation["source"]["classified_bytes"] = 0
            relation["destination"]["classified_bytes"] = 0
            redigest_plan(undercharged)
            with self.assertRaisesRegex(
                BulkloadError,
                "classified-byte lower bound differs",
            ):
                validate_codex_private_sqlite_compose_plan(undercharged)

            equal_digest_for_different_rows = json.loads(json.dumps(plan))
            relation = table_relation(
                equal_digest_for_different_rows,
                "threads",
            )
            relation["shared_equal"] = 0
            relation["source_only"] = 1
            relation["destination_only"] = 1
            redigest_plan(equal_digest_for_different_rows)
            with self.assertRaisesRegex(
                BulkloadError,
                "divergent-rowset digests are equal",
            ):
                validate_codex_private_sqlite_compose_plan(
                    equal_digest_for_different_rows
                )

            missing_divergence_blocker = json.loads(json.dumps(plan))
            relation = table_relation(missing_divergence_blocker, "threads")
            relation["shared_equal"] = 0
            relation["conflicts"] = 1
            relation["source"]["semantic_rows_sha256"] = "6" * 64
            relation["semantic_classification_complete"] = False
            redigest_plan(missing_divergence_blocker)
            with self.assertRaisesRegex(
                BulkloadError,
                "table divergence blockers differ",
            ):
                validate_codex_private_sqlite_compose_plan(missing_divergence_blocker)

            missing_exact_divergence_blocker = json.loads(json.dumps(plan))
            relation = table_relation(
                missing_exact_divergence_blocker,
                "_sqlx_migrations",
            )
            relation["shared_equal"] = 0
            relation["source_only"] = 1
            relation["destination_only"] = 1
            relation["source"]["semantic_rows_sha256"] = "5" * 64
            relation["semantic_classification_complete"] = False
            redigest_plan(missing_exact_divergence_blocker)
            with self.assertRaisesRegex(
                BulkloadError,
                "table divergence blockers differ",
            ):
                validate_codex_private_sqlite_compose_plan(
                    missing_exact_divergence_blocker
                )

            false_family_completeness = json.loads(json.dumps(plan))
            family = false_family_completeness["sqlite_families"][0]
            relation = table_relation(false_family_completeness, "threads")
            relation["shared_equal"] = 0
            relation["conflicts"] = 1
            relation["source"]["semantic_rows_sha256"] = "6" * 64
            relation["semantic_classification_complete"] = False
            family["blockers"] = [
                {
                    "code": "sqlite-shared-row-divergence",
                    "table": "threads",
                    "count": 1,
                }
            ]
            redigest_plan(false_family_completeness)
            with self.assertRaisesRegex(
                BulkloadError,
                "family readiness is invalid",
            ):
                validate_codex_private_sqlite_compose_plan(false_family_completeness)

            projected_migration_count = json.loads(json.dumps(plan))
            for role in ("source", "destination"):
                projected_migration_count["private_opening"][role]["stable_projection"][
                    "sqlite_families"
                ][0]["migration_count"] += 1
                redigest_projection(projected_migration_count, role)
            redigest_plan(projected_migration_count)
            with self.assertRaisesRegex(
                BulkloadError,
                "projected migration-table count differs",
            ):
                validate_codex_private_sqlite_compose_plan(projected_migration_count)

            projected_thread_count = json.loads(json.dumps(plan))
            for role in ("source", "destination"):
                projected_family = projected_thread_count["private_opening"][role][
                    "stable_projection"
                ]["sqlite_families"][0]
                projected_family["thread_count"] += 1
                next(
                    table
                    for table in projected_family["tables"]
                    if table["name"] == "threads"
                )["row_count"] += 1
                redigest_projection(projected_thread_count, role)
            redigest_plan(projected_thread_count)
            with self.assertRaisesRegex(
                BulkloadError,
                "projected table count differs",
            ):
                validate_codex_private_sqlite_compose_plan(projected_thread_count)

    def test_persisted_schema_edges_and_unique_claims_are_cross_bound(self) -> None:
        def redigest_schema(schema: dict) -> None:
            schema["schema_contract_sha256"] = sha256_bytes(
                canonical_bytes(
                    {
                        key: value
                        for key, value in schema.items()
                        if key != "schema_contract_sha256"
                    }
                )
            )

        with tempfile.TemporaryDirectory() as directory:
            plan, _ = self.compile_fixture(Path(directory))

            forged_edge = json.loads(json.dumps(plan))
            foreign_key = {
                "id": 0,
                "sequence": 0,
                "referenced_table": "threads",
                "from_column": "id",
                "to_column": "id",
                "on_update": "NO ACTION",
                "on_delete": "NO ACTION",
                "match": "NONE",
                "supported": True,
            }
            for role in ("source_schema", "destination_schema"):
                schema = forged_edge["sqlite_families"][0][role]
                threads = next(
                    table for table in schema["tables"] if table["name"] == "threads"
                )
                threads["foreign_keys"] = [foreign_key]
                redigest_schema(schema)
            forged_edge["plan_sha256"] = object_digest(
                forged_edge,
                "plan_sha256",
            )
            with self.assertRaisesRegex(
                BulkloadError,
                "observed-edge digest differs from schema",
            ):
                validate_codex_private_sqlite_compose_plan(forged_edge)

            forged_schema_skew = json.loads(json.dumps(plan))
            destination_schema = forged_schema_skew["sqlite_families"][0][
                "destination_schema"
            ]
            destination_schema["raw_schema_sha256"] = "7" * 64
            redigest_schema(destination_schema)
            forged_schema_skew["plan_sha256"] = object_digest(
                forged_schema_skew,
                "plan_sha256",
            )
            with self.assertRaisesRegex(
                BulkloadError,
                "raw-schema digest differs from schema records",
            ):
                validate_codex_private_sqlite_compose_plan(forged_schema_skew)

            forged_unique = json.loads(json.dumps(plan))
            secondary_index = {
                "name": "zz_threads_payload_unique",
                "unique": True,
                "origin": "c",
                "partial": False,
                "terms": [
                    {
                        "sequence": 0,
                        "cid": 2,
                        "name": "payload",
                        "descending": False,
                        "collation": "BINARY",
                        "key": True,
                    },
                    {
                        "sequence": 1,
                        "cid": -1,
                        "name": None,
                        "descending": False,
                        "collation": "BINARY",
                        "key": False,
                    },
                ],
                "create_sql_sha256": "5" * 64,
            }
            for role in ("source_schema", "destination_schema"):
                schema = forged_unique["sqlite_families"][0][role]
                threads = next(
                    table for table in schema["tables"] if table["name"] == "threads"
                )
                threads["indexes"].append(secondary_index)
                threads["indexes"].sort(key=lambda index: index["name"])
                schema["schema_records"].append(
                    {
                        "type": "index",
                        "name": secondary_index["name"],
                        "table": "threads",
                        "sql_sha256": secondary_index["create_sql_sha256"],
                    }
                )
                schema["schema_records"].sort(
                    key=lambda record: (record["type"], record["name"])
                )
                schema["raw_schema_sha256"] = sha256_bytes(
                    canonical_bytes(schema["schema_records"])
                )
                redigest_schema(schema)
            forged_unique["plan_sha256"] = object_digest(
                forged_unique,
                "plan_sha256",
            )
            with self.assertRaisesRegex(
                BulkloadError,
                "table classification blockers differ",
            ):
                validate_codex_private_sqlite_compose_plan(forged_unique)

    def test_shared_deadline_covers_python_classification_and_final_issuance(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            plan, evidence = self.compile_fixture(Path(directory))
            actual_relation = private_sqlite_plan._migration_relation
            clock = [1000.0]

            def expire_after_relation(
                source: list[list[object]],
                destination: list[list[object]],
            ) -> tuple[str, dict[str, object]]:
                result = actual_relation(source, destination)
                clock[0] = 1601.0
                return result

            with mock.patch.object(
                private_sqlite_plan.time,
                "monotonic",
                side_effect=lambda: clock[0],
            ):
                with mock.patch.object(
                    private_sqlite_plan,
                    "_migration_relation",
                    side_effect=expire_after_relation,
                ):
                    with self.assertRaisesRegex(
                        BulkloadError,
                        "migration classification exceeded its deadline",
                    ):
                        validate_codex_private_sqlite_compose_plan_against_inputs(
                            plan,
                            evidence["compatibility"],
                            evidence["source_a_path"],
                            evidence["source_b_path"],
                            evidence["destination_a_path"],
                            evidence["destination_b_path"],
                            adapter_registry=evidence["registry"],
                            path_map=evidence["path_map"],
                            session_union_plan=evidence["session_plan"],
                            session_source_a=evidence["session_source_a"],
                            session_source_b=evidence["session_source_b"],
                            session_destination_a=evidence["session_destination_a"],
                            session_destination_b=evidence["session_destination_b"],
                        )

    def test_structural_and_against_input_validation_reject_tamper(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            plan, evidence = self.compile_fixture(Path(directory))
            self.validate_against_inputs(plan, evidence)

            malformed = json.loads(json.dumps(plan))
            schema = malformed["sqlite_families"][0]["source_schema"]
            schema["tables"][0]["foreign_keys"] = [{"id": "forged"}]
            schema["schema_contract_sha256"] = sha256_bytes(
                canonical_bytes(
                    {
                        key: value
                        for key, value in schema.items()
                        if key != "schema_contract_sha256"
                    }
                )
            )
            malformed["plan_sha256"] = object_digest(
                malformed,
                "plan_sha256",
            )
            with self.assertRaisesRegex(
                BulkloadError,
                "foreign key",
            ):
                validate_codex_private_sqlite_compose_plan(malformed)

            source_only_drift = json.loads(json.dumps(plan))
            source_schema = source_only_drift["sqlite_families"][0]["source_schema"]
            source_schema["tables"][0]["columns"][0]["declared_type"] += " "
            source_schema["schema_contract_sha256"] = sha256_bytes(
                canonical_bytes(
                    {
                        key: value
                        for key, value in source_schema.items()
                        if key != "schema_contract_sha256"
                    }
                )
            )
            source_only_drift["plan_sha256"] = object_digest(
                source_only_drift,
                "plan_sha256",
            )
            with self.assertRaisesRegex(
                BulkloadError,
                "schema-adapter blocker differs from schema",
            ):
                validate_codex_private_sqlite_compose_plan(source_only_drift)

            drifted = json.loads(json.dumps(plan))
            source_opening = drifted["private_opening"]["source"]
            source_opening["host"] += "-drift"
            source_opening["stable_projection"]["host"] = source_opening["host"]
            source_opening["stable_projection_sha256"] = sha256_bytes(
                canonical_bytes(source_opening["stable_projection"])
            )
            drifted["plan_sha256"] = object_digest(drifted, "plan_sha256")
            validate_codex_private_sqlite_compose_plan(drifted)
            with self.assertRaisesRegex(
                BulkloadError,
                "differs from its opening inputs",
            ):
                self.validate_against_inputs(drifted, evidence)

    def test_sqlite_producer_and_oracle_commands_are_retired_in_v7(self) -> None:
        parser = bulkload_cli.build_parser()
        command_action = next(
            action
            for action in parser._actions
            if isinstance(action, argparse._SubParsersAction)
        )
        for command in (
            "codex-private-sqlite-compose-plan",
            "codex-private-sqlite-close-request",
            "codex-private-sqlite-session-reclose",
            "codex-private-sqlite-private-reclose",
            "codex-private-sqlite-compose-action-plan",
            "codex-private-sqlite-compose-request",
            "codex-private-sqlite-capacity-observe",
            "codex-private-sqlite-compose",
            "codex-private-sqlite-verify",
            "codex-private-sqlite-oracle",
            "codex-private-sqlite-verifier-oracle",
            "codex-private-sqlite-publish",
            "codex-private-sqlite-install",
        ):
            self.assertNotIn(command, command_action.choices)

    def test_value_and_publication_budgets_are_aligned(self) -> None:
        self.assertEqual(
            private_sqlite_plan.MAX_SQLITE_PLAN_BYTES + 1,
            private_state.MAX_PRIVATE_PLAN_BYTES,
        )
        connection = sqlite3.connect(":memory:")
        try:
            connection.execute(
                "CREATE TABLE records(id TEXT PRIMARY KEY, payload BLOB) STRICT"
            )
            connection.execute("INSERT INTO records VALUES('id', zeroblob(2048))")
            with mock.patch.object(
                private_sqlite_plan,
                "MAX_SQLITE_VALUE_BYTES",
                1024,
            ):
                with self.assertRaisesRegex(
                    BulkloadError,
                    "value exceeds its byte budget",
                ):
                    private_sqlite_plan._preflight_semantic_rows(
                        connection,
                        table="records",
                        columns=["id", "payload"],
                        deadline=time.monotonic() + 60,
                    )
        finally:
            connection.close()

        with tempfile.TemporaryDirectory() as directory:
            with mock.patch.object(
                private_sqlite_plan,
                "MAX_SQLITE_PLAN_ROWS",
                3,
            ):
                with self.assertRaisesRegex(
                    BulkloadError,
                    "semantic classification budget exceeded",
                ):
                    self.compile_fixture(Path(directory))

        relation, _ = self.classify_records(
            """
            CREATE TABLE records(
                id TEXT PRIMARY KEY,
                payload TEXT NOT NULL
            ) STRICT
            """,
            [("id", "value")],
            [("id", "value")],
        )
        combined_bytes = (
            relation["source"]["classified_bytes"]
            + relation["destination"]["classified_bytes"]
        )
        with mock.patch.object(
            private_sqlite_plan,
            "MAX_SQLITE_PLAN_ROW_BYTES",
            combined_bytes - 1,
        ):
            with self.assertRaisesRegex(
                BulkloadError,
                "semantic classification budget exceeded",
            ):
                self.classify_records(
                    """
                    CREATE TABLE records(
                        id TEXT PRIMARY KEY,
                        payload TEXT NOT NULL
                    ) STRICT
                    """,
                    [("id", "value")],
                    [("id", "value")],
                )


if __name__ == "__main__":
    unittest.main()
