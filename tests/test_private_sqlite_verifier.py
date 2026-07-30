from __future__ import annotations

import ast
from contextlib import nullcontext
from copy import deepcopy
import hashlib
import inspect
import json
import os
from pathlib import Path
import sqlite3
import stat
import tempfile
import time
import unittest
from unittest import mock

from bulkload_lib import private_runtime
from bulkload_lib import private_sqlite_plan as legacy_sqlite_plan
from bulkload_lib import private_sqlite_verifier as verifier
from bulkload_lib.model import BulkloadError, canonical_bytes, sha256_bytes
from bulkload_lib.private_sqlite_plan import MAX_SQLITE_VALUE_BYTES
from bulkload_lib.private_sqlite_protocol import (
    MAX_FAILURES,
    MAX_PROTOCOL_BYTES,
    MAX_TREE_ENTRIES,
    VERIFIER_ORACLE_REPORT_SCHEMA,
    validate_composed_bundle_manifest,
    validate_composition_receipt,
    validate_verifier_oracle_report,
)
from tests.private_sqlite_v7_fixtures import (
    BASENAME,
    COMPLETED_AT,
    OBSERVATION_ID,
    ORACLE_FALSE_CLAIMS,
    PRE_RECEIPT_COMPLETED_NODE_IDS,
    build_handbuilt_bundle,
    canonical_file,
    completed_before_receipt,
    self_digest,
)


def _tree_snapshot(root: Path) -> dict[str, tuple]:
    result: dict[str, tuple] = {}
    for path in sorted((root, *root.rglob("*"))):
        info = path.lstat()
        relative = "." if path == root else path.relative_to(root).as_posix()
        digest = None
        if stat.S_ISREG(info.st_mode):
            hasher = hashlib.sha256()
            with path.open("rb") as source:
                while block := source.read(1024 * 1024):
                    hasher.update(block)
            digest = hasher.hexdigest()
        result[relative] = (
            info.st_dev,
            info.st_ino,
            stat.S_IMODE(info.st_mode),
            info.st_nlink,
            info.st_size,
            info.st_mtime_ns,
            digest,
        )
    return result


def _rewrite_canonical_json(path: Path, value: dict[str, object]) -> None:
    path.write_bytes(canonical_file(value))
    path.chmod(0o600)


def _schema_limits(**counts: int) -> dict[str, int]:
    result = {key: 0 for key in verifier._SCHEMA_COUNT_LABELS}
    result.update(counts)
    result["actions"] = sum(result.values())
    result["schema_sql_bytes"] = verifier.MAX_SQLITE_SCHEMA_SQL_BYTES
    result["migration_bytes"] = verifier.MAX_SQLITE_MIGRATION_BYTES
    return result


class _ScriptedSchemaConnection:
    def __init__(self, responses: dict[str, list[tuple[object, ...]]]) -> None:
        self.responses = responses
        self.current_limits = {
            verifier.sqlite3.SQLITE_LIMIT_LENGTH: 1_000_000_000,
            verifier.sqlite3.SQLITE_LIMIT_SQL_LENGTH: 1_000_000_000,
            verifier.sqlite3.SQLITE_LIMIT_COLUMN: 2000,
        }
        self.limit_events: list[tuple[int, int]] = []

    def getlimit(self, category: int) -> int:
        return self.current_limits[category]

    def setlimit(self, category: int, value: int) -> int:
        previous = self.current_limits[category]
        self.current_limits[category] = value
        self.limit_events.append((category, value))
        return previous

    def set_progress_handler(self, *_: object) -> None:
        return None

    def execute(
        self,
        statement: str,
        *_: object,
    ) -> object:
        keys = (
            "internal",
            "schema",
            "table_list",
            "columns",
            "indexes",
            "index_terms",
            "foreign_keys",
            "migrations",
        )
        if "name LIKE 'sqlite_%'" in statement:
            key = "internal"
        elif "SELECT type,name,tbl_name,sql" in statement:
            key = "schema"
        elif "FROM pragma_table_list" in statement:
            key = "table_list"
        elif "FROM pragma_table_xinfo" in statement:
            key = "columns"
        elif "FROM pragma_index_list" in statement:
            key = "indexes"
        elif "FROM pragma_index_xinfo" in statement:
            key = "index_terms"
        elif "FROM pragma_foreign_key_list" in statement:
            key = "foreign_keys"
        elif "FROM _sqlx_migrations" in statement:
            key = "migrations"
        else:
            raise AssertionError(f"unexpected schema statement: {statement}")
        if key not in keys:
            raise AssertionError(f"unknown schema response key: {key}")
        return iter(self.responses.get(key, []))


class PrivateSqliteV7VerifierTest(unittest.TestCase):
    def test_verifier_uses_protocol_limits_without_local_drift(self) -> None:
        self.assertEqual(verifier.MAX_PROTOCOL_BYTES, MAX_PROTOCOL_BYTES)
        self.assertEqual(verifier.MAX_TREE_ENTRIES, MAX_TREE_ENTRIES)
        self.assertEqual(verifier.MAX_FAILURES, MAX_FAILURES)
        distinct_failures = [
            verifier._failure(
                f"failure-{index}",
                scope="limit-regression",
            )
            for index in range(MAX_FAILURES + 1)
        ]
        with self.assertRaisesRegex(BulkloadError, "failure budget exceeded"):
            verifier._dedupe_failures(distinct_failures)

    def test_json_reader_uses_the_protocol_byte_limit(self) -> None:
        payload = canonical_file({"payload": "x" * (8 * 1024 * 1024)})
        self.assertGreater(len(payload), 8 * 1024 * 1024)
        self.assertLessEqual(len(payload), MAX_PROTOCOL_BYTES)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            path = root / "protocol.json"
            path.write_bytes(payload)
            path.chmod(0o600)
            parent_descriptor = os.open(
                root,
                os.O_RDONLY
                | getattr(os, "O_DIRECTORY", 0)
                | getattr(os, "O_CLOEXEC", 0),
            )
            self.addCleanup(os.close, parent_descriptor)
            value, observed_payload, _, document_descriptor = (
                verifier._read_private_json_at(
                    parent_descriptor,
                    path.name,
                    label="protocol document",
                )
            )
            self.addCleanup(os.close, document_descriptor)
        self.assertEqual(len(value["payload"]), 8 * 1024 * 1024)
        self.assertTrue(value["payload"].startswith("x"))
        self.assertTrue(value["payload"].endswith("x"))
        self.assertEqual(observed_payload, payload)

    def test_directory_enumeration_is_bounded_and_sorted(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            for name in ("third", "first", "second"):
                (root / name).write_bytes(b"")
            descriptor = os.open(
                root,
                os.O_RDONLY
                | getattr(os, "O_DIRECTORY", 0)
                | getattr(os, "O_CLOEXEC", 0),
            )
            self.addCleanup(os.close, descriptor)
            with mock.patch.object(verifier, "MAX_TREE_ENTRIES", 2):
                with self.assertRaisesRegex(BulkloadError, "entry budget exceeded"):
                    verifier._bounded_directory_entries(
                        descriptor,
                        label="test directory",
                        deadline=verifier.time.monotonic() + 60,
                    )
            self.assertEqual(
                verifier._bounded_directory_entries(
                    descriptor,
                    label="test directory",
                    deadline=verifier.time.monotonic() + 60,
                ),
                ["first", "second", "third"],
            )

    def test_hash_and_sqlite_health_work_are_deadline_aware(self) -> None:
        with tempfile.TemporaryFile() as sqlite_file:
            sqlite_file.write(b"x" * (1024 * 1024 + 1))
            sqlite_file.flush()
            with mock.patch.object(
                verifier.time,
                "monotonic",
                side_effect=(0.0, 0.0, 2.0),
            ):
                with self.assertRaisesRegex(BulkloadError, "hash exceeded"):
                    verifier._hash_descriptor(
                        sqlite_file.fileno(),
                        2 * 1024 * 1024,
                        deadline=1.0,
                    )

        class DeadlineConnection:
            def __init__(self) -> None:
                self.handler = None
                self.progress_calls: list[tuple[object, int]] = []

            def set_progress_handler(self, handler: object, steps: int) -> None:
                self.handler = handler
                self.progress_calls.append((handler, steps))

            def execute(self, _: str) -> object:
                if self.handler is not None and self.handler():
                    raise verifier.sqlite3.OperationalError("interrupted")
                raise AssertionError("deadline callback did not interrupt SQLite")

        connection = DeadlineConnection()
        with mock.patch.object(
            verifier.time,
            "monotonic",
            side_effect=(0.0, 2.0, 2.0),
        ):
            with self.assertRaisesRegex(BulkloadError, "health checks exceeded"):
                verifier._sqlite_health_checks(connection, deadline=1.0)
        self.assertEqual(connection.progress_calls[-1], (None, 0))

    def test_public_oracle_snapshots_inputs_before_observation(self) -> None:
        action_plan = {"marker": "original"}
        opening_plan = {"marker": "opening"}
        compose_request = {"marker": "request"}
        capacity_observation = {"marker": "capacity"}
        runtime_authority = {
            "source_digests": {
                verifier.PRIVATE_SQLITE_VERIFIER_SOURCE_PATH: "0" * 64,
            },
        }
        lease = mock.Mock()

        def mutate_caller(
            _: Path,
            observed_action: dict[str, object],
            observed_opening: dict[str, object],
            observed_request: dict[str, object],
            observed_capacity: dict[str, object],
            **__: object,
        ) -> dict[str, object]:
            self.assertIsNot(observed_action, action_plan)
            self.assertIsNot(observed_opening, opening_plan)
            self.assertIsNot(observed_request, compose_request)
            self.assertIsNot(observed_capacity, capacity_observation)
            self.assertEqual(observed_action, {"marker": "original"})
            action_plan["marker"] = "mutated"
            return {"snapshot": "report"}

        with (
            mock.patch.object(verifier, "_verify_input_bindings"),
            mock.patch.object(
                private_runtime,
                "validate_private_runtime_authority",
            ),
            mock.patch.object(
                private_runtime,
                "open_pinned_private_runtime_authority",
                return_value=nullcontext(lease),
            ),
            mock.patch.object(
                verifier,
                "_observe_codex_private_sqlite_bundle",
                side_effect=mutate_caller,
            ),
        ):
            with self.assertRaisesRegex(
                BulkloadError,
                "action plan changed during observation",
            ):
                verifier.observe_codex_private_sqlite_bundle(
                    Path("/not-opened"),
                    action_plan,
                    opening_plan,
                    compose_request,
                    capacity_observation,
                    verifier_runtime_authority=runtime_authority,
                )
        lease.revalidate.assert_called_once_with()

    def test_input_snapshot_has_one_cumulative_byte_budget(self) -> None:
        values = (
            ("first", {"payload": "a" * 32}),
            ("second", {"payload": "b" * 32}),
        )
        with mock.patch.object(verifier, "MAX_PROTOCOL_BYTES", 64):
            with self.assertRaisesRegex(
                BulkloadError,
                "cumulative byte budget",
            ):
                verifier._snapshot_input_documents(
                    values,
                    deadline=time.monotonic() + 60,
                )

    def test_input_snapshot_deadline_starts_before_canonicalization(self) -> None:
        with mock.patch.object(verifier.time, "monotonic", return_value=2.0):
            with self.assertRaisesRegex(
                BulkloadError,
                "input snapshot exceeded its deadline",
            ):
                verifier._snapshot_input_documents(
                    (("action plan", {"payload": "bounded"}),),
                    deadline=1.0,
                )

    def test_schema_scan_stops_at_the_streamed_table_budget(self) -> None:
        class TableFlood:
            def __init__(self) -> None:
                self.limits = {
                    verifier.sqlite3.SQLITE_LIMIT_LENGTH: 1_000_000_000,
                    verifier.sqlite3.SQLITE_LIMIT_SQL_LENGTH: 1_000_000_000,
                    verifier.sqlite3.SQLITE_LIMIT_COLUMN: 2000,
                }

            def getlimit(self, _: int) -> int:
                return self.limits[_]

            def setlimit(self, category: int, value: int) -> int:
                previous = self.limits[category]
                self.limits[category] = value
                return previous

            def set_progress_handler(self, *_: object) -> None:
                return None

            def execute(self, statement: str, *_: object) -> object:
                if "FROM pragma_table_list" in statement:
                    return (
                        ("main", f"view_{index}", "view", 0, 0, 0)
                        for index in range(verifier.MAX_TABLES + 1)
                    )
                if "FROM sqlite_schema" in statement:
                    return iter(())
                raise AssertionError(f"unexpected statement: {statement}")

        with self.assertRaisesRegex(BulkloadError, "table-object budget exceeded"):
            verifier._scan_schema_independently(
                TableFlood(),  # type: ignore[arg-type]
                deadline=time.monotonic() + 60,
                limits=_schema_limits(table_objects=verifier.MAX_TABLES),
            )

    def test_schema_limits_are_derived_from_the_exact_action_and_opening(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = build_handbuilt_bundle(
                Path(directory).resolve(),
                actual_rows=[(1, b"one")],
            )
        limits = verifier._schema_scan_limits(
            fixture.action_plan["sqlite_families"][0],
            fixture.opening_plan["sqlite_families"][0],
        )
        self.assertEqual(
            limits,
            {
                "internal_tables": 0,
                "schema_records": 1,
                "table_objects": 1,
                "tables": 1,
                "columns": 2,
                "indexes": 0,
                "index_terms": 0,
                "foreign_keys": 0,
                "migrations": 0,
                "actions": 5,
                "schema_sql_bytes": verifier.MAX_SQLITE_SCHEMA_SQL_BYTES,
                "migration_bytes": verifier.MAX_SQLITE_MIGRATION_BYTES,
            },
        )

    def test_independent_schema_and_semantic_encodings_match_legacy_producer(
        self,
    ) -> None:
        connection = sqlite3.connect(":memory:")
        self.addCleanup(connection.close)
        connection.execute(
            "CREATE TABLE parity("
            "id_a INTEGER NOT NULL,"
            "id_b TEXT NOT NULL,"
            "nullable TEXT,"
            "integral INTEGER,"
            "real_value REAL,"
            "text_value TEXT,"
            "blob_value BLOB,"
            "generated TEXT GENERATED ALWAYS AS (text_value || ':g') STORED,"
            "PRIMARY KEY(id_a,id_b)"
            ") STRICT"
        )
        connection.executemany(
            "INSERT INTO parity("
            "id_a,id_b,nullable,integral,real_value,text_value,blob_value"
            ") VALUES (?,?,?,?,?,?,?)",
            (
                (-(1 << 63), "", None, (1 << 63) - 1, -0.0, "mu-\u03bc", b""),
                (2, "alpha", "present", -(1 << 63), -1.25, "", b"\x00\xff"),
                (10, "\u03b2", "", 0, 1.5, "snowman-\u2603", b"x" * (1024 * 1024)),
            ),
        )
        connection.commit()
        deadline = time.monotonic() + 60
        producer_schema, producer_blockers, producer_migrations = (
            legacy_sqlite_plan._structured_schema(
                connection,
                deadline=deadline,
            )
        )
        action_family = {
            "migration": {
                "source_migrations_sha256": sha256_bytes(canonical_bytes([])),
            },
            "tables": [
                {
                    "name": "parity",
                    "identity_columns": ["id_a", "id_b"],
                }
            ],
        }
        opening_family = {"source_schema": producer_schema}
        observed_schema, observed_blockers, observed_migrations = (
            verifier._scan_schema_independently(
                connection,
                deadline=deadline,
                limits=verifier._schema_scan_limits(
                    action_family,
                    opening_family,
                ),
            )
        )
        self.assertEqual(observed_schema, producer_schema)
        self.assertEqual(observed_blockers, producer_blockers)
        self.assertEqual(observed_migrations, producer_migrations)

        table_contract = next(
            table for table in producer_schema["tables"] if table["name"] == "parity"
        )
        table_rule = action_family["tables"][0]
        producer_state: dict[str, object] = {}
        for _ in legacy_sqlite_plan._semantic_row_stream(
            connection,
            basename="state_5.sqlite",
            role="destination",
            table_contract=table_contract,
            table_rule=table_rule,
            path_rules=[],
            path_map={},
            accepted_session_paths={},
            deadline=deadline,
            state=producer_state,
        ):
            pass
        observed_state = verifier._scan_table(
            connection,
            basename="state_5.sqlite",
            table_contract=table_contract,
            action_table=table_rule,
            deadline=deadline,
            budget={"rows": 0, "bytes": 0},
        )
        self.assertEqual(observed_state["name"], "parity")
        self.assertEqual(
            {
                key: observed_state[key]
                for key in (
                    "row_count",
                    "classified_bytes",
                    "semantic_rows_sha256",
                )
            },
            producer_state,
        )

    def test_semantic_scan_never_delegates_ordering_and_bounds_retained_rows(
        self,
    ) -> None:
        inner = sqlite3.connect(":memory:")
        self.addCleanup(inner.close)
        inner.execute(
            "CREATE TABLE bounded(id INTEGER PRIMARY KEY, payload BLOB NOT NULL)"
        )
        inner.executemany(
            "INSERT INTO bounded(id,payload) VALUES (?,?)",
            ((3, b"three"), (1, b"one"), (2, b"two")),
        )
        inner.commit()

        class RecordingConnection:
            def __init__(self) -> None:
                self.statements: list[str] = []

            def setlimit(self, category: int, value: int) -> int:
                return inner.setlimit(category, value)

            def set_progress_handler(self, handler: object, steps: int) -> None:
                inner.set_progress_handler(handler, steps)  # type: ignore[arg-type]

            def execute(self, statement: str) -> object:
                self.statements.append(statement)
                return inner.execute(statement)

        connection = RecordingConnection()
        with mock.patch.object(verifier, "MAX_VERIFIER_RETAINED_ROWS", 2):
            with self.assertRaisesRegex(
                BulkloadError,
                "retained identity ordering budget exceeded",
            ):
                verifier._scan_table(
                    connection,  # type: ignore[arg-type]
                    basename="state_5.sqlite",
                    table_contract={
                        "columns": [{"name": "id"}, {"name": "payload"}],
                    },
                    action_table={
                        "name": "bounded",
                        "identity_columns": ["id"],
                    },
                    deadline=time.monotonic() + 60,
                    budget={"rows": 0, "bytes": 0},
                )
        self.assertTrue(connection.statements)
        self.assertTrue(
            all(
                "ORDER BY" not in statement.upper()
                for statement in connection.statements
            )
        )

    def test_semantic_ordering_matches_frozen_producer_for_utf16_text(
        self,
    ) -> None:
        cases = (
            (
                "UTF-16le",
                (("a", b"ascii"), ("\u0100", b"utf16le-orders-first")),
            ),
            (
                "UTF-16be",
                (("\ue000", b"bmp"), ("\U00010000", b"surrogate-orders-first")),
            ),
        )
        for encoding, rows in cases:
            with self.subTest(encoding=encoding):
                with tempfile.TemporaryDirectory() as directory:
                    database_path = Path(directory) / "utf16.sqlite"
                    connection = sqlite3.connect(database_path)
                    try:
                        connection.execute(f"PRAGMA encoding='{encoding}'")
                        connection.execute(
                            "CREATE TABLE parity("
                            "id TEXT PRIMARY KEY,"
                            "payload BLOB NOT NULL"
                            ") STRICT"
                        )
                        connection.executemany(
                            "INSERT INTO parity(id,payload) VALUES (?,?)",
                            rows,
                        )
                        connection.commit()

                        deadline = time.monotonic() + 60
                        schema, blockers, _ = legacy_sqlite_plan._structured_schema(
                            connection,
                            deadline=deadline,
                        )
                        self.assertEqual(blockers, [])
                        table_contract = next(
                            table
                            for table in schema["tables"]
                            if table["name"] == "parity"
                        )
                        table_rule = {
                            "name": "parity",
                            "identity_columns": ["id"],
                        }
                        producer_state: dict[str, object] = {}
                        for _ in legacy_sqlite_plan._semantic_row_stream(
                            connection,
                            basename="state_5.sqlite",
                            role="destination",
                            table_contract=table_contract,
                            table_rule=table_rule,
                            path_rules=[],
                            path_map={},
                            accepted_session_paths={},
                            deadline=deadline,
                            state=producer_state,
                        ):
                            pass
                        observed_state = verifier._scan_table(
                            connection,
                            basename="state_5.sqlite",
                            table_contract=table_contract,
                            action_table=table_rule,
                            deadline=deadline,
                            budget={"rows": 0, "bytes": 0},
                        )
                        self.assertEqual(
                            observed_state,
                            {"name": "parity", **producer_state},
                        )
                    finally:
                        connection.close()

    def test_schema_sqlite_limits_precede_queries_and_are_restored(self) -> None:
        inner = verifier.sqlite3.connect(":memory:")
        self.addCleanup(inner.close)

        class RecordingConnection:
            def __init__(self) -> None:
                self.events: list[tuple[str, int, int]] = []
                self.first_query_limits: dict[int, int] | None = None

            def getlimit(self, category: int) -> int:
                return inner.getlimit(category)

            def setlimit(self, category: int, value: int) -> int:
                self.events.append(("set", category, value))
                return inner.setlimit(category, value)

            def set_progress_handler(self, handler: object, steps: int) -> None:
                inner.set_progress_handler(handler, steps)  # type: ignore[arg-type]

            def execute(self, statement: str, *args: object) -> object:
                if self.first_query_limits is None:
                    self.first_query_limits = {
                        category: inner.getlimit(category)
                        for category in (
                            verifier.sqlite3.SQLITE_LIMIT_LENGTH,
                            verifier.sqlite3.SQLITE_LIMIT_SQL_LENGTH,
                            verifier.sqlite3.SQLITE_LIMIT_COLUMN,
                        )
                    }
                return inner.execute(statement, *args)

        connection = RecordingConnection()
        previous = {
            category: inner.getlimit(category)
            for category in (
                verifier.sqlite3.SQLITE_LIMIT_LENGTH,
                verifier.sqlite3.SQLITE_LIMIT_SQL_LENGTH,
                verifier.sqlite3.SQLITE_LIMIT_COLUMN,
            )
        }
        verifier._scan_schema_independently(
            connection,  # type: ignore[arg-type]
            deadline=time.monotonic() + 60,
            limits=_schema_limits(),
        )
        self.assertEqual(
            connection.first_query_limits,
            {
                verifier.sqlite3.SQLITE_LIMIT_LENGTH: (
                    verifier._MAX_SQLITE_SCHEMA_FETCH_BYTES
                ),
                verifier.sqlite3.SQLITE_LIMIT_SQL_LENGTH: (
                    verifier._MAX_SQLITE_SCHEMA_QUERY_BYTES
                ),
                verifier.sqlite3.SQLITE_LIMIT_COLUMN: (
                    verifier.MAX_SQLITE_SCHEMA_COLUMNS
                ),
            },
        )
        self.assertEqual(
            {category: inner.getlimit(category) for category in previous},
            previous,
        )

    def test_schema_scan_rejects_each_overbound_opening_category(self) -> None:
        schema_table = (
            "table",
            "items",
            "items",
            "CREATE TABLE items(id INTEGER)",
        )
        cases = (
            (
                "schema object",
                {
                    "schema": [
                        schema_table,
                        ("view", "v", "v", "CREATE VIEW v AS SELECT 1"),
                    ]
                },
                _schema_limits(schema_records=1),
                "schema-object budget exceeded",
            ),
            (
                "column",
                {
                    "schema": [schema_table],
                    "table_list": [("main", "items", "table", 2, 0, 0)],
                    "columns": [
                        (0, "id", "INTEGER", 0, None, 0, 0),
                        (1, "value", "BLOB", 0, None, 0, 0),
                    ],
                },
                _schema_limits(
                    schema_records=1,
                    table_objects=1,
                    tables=1,
                    columns=1,
                ),
                "column budget exceeded",
            ),
            (
                "index term",
                {
                    "schema": [
                        schema_table,
                        (
                            "index",
                            "items_idx",
                            "items",
                            "CREATE INDEX items_idx ON items(id)",
                        ),
                    ],
                    "table_list": [("main", "items", "table", 0, 0, 0)],
                    "indexes": [(0, "items_idx", 0, "c", 0)],
                    "index_terms": [
                        (0, 0, "id", 0, "BINARY", 1),
                        (1, -1, None, 0, "BINARY", 0),
                    ],
                },
                _schema_limits(
                    schema_records=2,
                    table_objects=1,
                    tables=1,
                    indexes=1,
                    index_terms=1,
                ),
                "index-term budget exceeded",
            ),
            (
                "foreign key",
                {
                    "schema": [schema_table],
                    "table_list": [("main", "items", "table", 0, 0, 0)],
                    "foreign_keys": [
                        (
                            0,
                            0,
                            "parent",
                            "parent_id",
                            "id",
                            "NO ACTION",
                            "CASCADE",
                            "NONE",
                        ),
                        (
                            1,
                            0,
                            "other",
                            "other_id",
                            "id",
                            "NO ACTION",
                            "CASCADE",
                            "NONE",
                        ),
                    ],
                },
                _schema_limits(
                    schema_records=1,
                    table_objects=1,
                    tables=1,
                    foreign_keys=1,
                ),
                "foreign-key budget exceeded",
            ),
            (
                "migration",
                {
                    "schema": [
                        (
                            "table",
                            "_sqlx_migrations",
                            "_sqlx_migrations",
                            "CREATE TABLE _sqlx_migrations(version INTEGER)",
                        )
                    ],
                    "table_list": [("main", "_sqlx_migrations", "table", 0, 0, 0)],
                    "migrations": [
                        (1, "integer", "one", "text", "AA", "blob", 1, "integer"),
                        (2, "integer", "two", "text", "BB", "blob", 1, "integer"),
                    ],
                },
                _schema_limits(
                    schema_records=1,
                    table_objects=1,
                    tables=1,
                    migrations=1,
                ),
                "migration budget exceeded",
            ),
        )
        for label, responses, limits, error in cases:
            with self.subTest(label=label):
                connection = _ScriptedSchemaConnection(responses)
                with self.assertRaisesRegex(BulkloadError, error):
                    verifier._scan_schema_independently(
                        connection,  # type: ignore[arg-type]
                        deadline=time.monotonic() + 60,
                        limits=limits,
                    )

    def test_schema_scan_rejects_overbound_raw_sql_before_retention(self) -> None:
        oversized_sql = "x" * (verifier.MAX_SQLITE_SCHEMA_SQL_BYTES + 1)
        connection = _ScriptedSchemaConnection(
            {
                "schema": [
                    ("table", "items", "items", oversized_sql),
                ],
            }
        )
        with self.assertRaisesRegex(
            BulkloadError,
            "schema-sql-bytes budget exceeded",
        ):
            verifier._scan_schema_independently(
                connection,  # type: ignore[arg-type]
                deadline=time.monotonic() + 60,
                limits=_schema_limits(schema_records=1),
            )

    def test_schema_scan_rejects_overbound_migration_bytes(self) -> None:
        oversized_description = "x" * (verifier.MAX_SQLITE_MIGRATION_BYTES + 1)
        connection = _ScriptedSchemaConnection(
            {
                "schema": [
                    (
                        "table",
                        "_sqlx_migrations",
                        "_sqlx_migrations",
                        "CREATE TABLE _sqlx_migrations(version INTEGER)",
                    )
                ],
                "table_list": [("main", "_sqlx_migrations", "table", 0, 0, 0)],
                "migrations": [
                    (
                        1,
                        "integer",
                        oversized_description,
                        "text",
                        "AA",
                        "blob",
                        1,
                        "integer",
                    )
                ],
            }
        )
        with self.assertRaisesRegex(
            BulkloadError,
            "migration-bytes budget exceeded",
        ):
            verifier._scan_schema_independently(
                connection,  # type: ignore[arg-type]
                deadline=time.monotonic() + 60,
                limits=_schema_limits(
                    schema_records=1,
                    table_objects=1,
                    tables=1,
                    migrations=1,
                ),
            )

    def test_public_oracle_identity_and_time_are_not_caller_supplied(self) -> None:
        parameters = inspect.signature(
            verifier.observe_codex_private_sqlite_bundle
        ).parameters
        self.assertEqual(
            list(parameters),
            [
                "bundle_path",
                "action_plan",
                "opening_plan",
                "compose_request",
                "capacity_observation",
                "verifier_runtime_authority",
            ],
        )
        self.assertNotIn("observation_id", parameters)
        self.assertNotIn("observed_at", parameters)

    def test_module_has_only_read_only_oracle_surface(self) -> None:
        path = Path(inspect.getsourcefile(verifier)).resolve()
        tree = ast.parse(path.read_text(encoding="utf-8"))
        imported_names: set[str] = set()
        imported_modules: set[str] = set()
        forbidden_attributes = {
            "atomic_write",
            "backup",
            "blobopen",
            "call",
            "check_call",
            "check_output",
            "chown",
            "chmod",
            "copy",
            "copy2",
            "copyfile",
            "copyfileobj",
            "copymode",
            "copystat",
            "fsync",
            "lchmod",
            "link",
            "mkfifo",
            "mkdir",
            "mknod",
            "move",
            "popen",
            "Popen",
            "rename",
            "rmdir",
            "run",
            "symlink",
            "system",
            "touch",
            "truncate",
            "unlink",
            "write",
            "write_bytes",
            "write_text",
            "writelines",
        }
        called_attributes: set[str] = set()
        loaded_names: set[str] = set()
        public_functions: set[str] = set()
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                imported_modules.update(alias.name for alias in node.names)
            elif isinstance(node, ast.ImportFrom):
                imported_modules.add(node.module or "")
                imported_names.update(alias.name for alias in node.names)
            elif isinstance(node, ast.Attribute) and isinstance(
                node.ctx,
                ast.Load,
            ):
                called_attributes.add(node.attr)
            elif isinstance(node, ast.Name) and isinstance(node.ctx, ast.Load):
                loaded_names.add(node.id)
            elif isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                if not node.name.startswith("_"):
                    public_functions.add(node.name)

        self.assertNotIn(
            "bulkload_lib.private_sqlite_composer",
            imported_modules,
        )
        self.assertNotIn("private_sqlite_composer", imported_modules)
        for forbidden_module in {
            "ftplib",
            "http",
            "shutil",
            "socket",
            "subprocess",
            "urllib.request",
        }:
            self.assertFalse(
                any(
                    module == forbidden_module
                    or module.startswith(f"{forbidden_module}.")
                    for module in imported_modules
                )
            )
        self.assertNotIn(
            "INDEPENDENT_VERIFICATION_RECEIPT_SCHEMA",
            imported_names,
        )
        self.assertNotIn(
            "validate_independent_verification_receipt",
            imported_names,
        )
        self.assertFalse(forbidden_attributes & called_attributes)
        self.assertNotIn("open", loaded_names)
        source = path.read_text(encoding="utf-8")
        self.assertNotIn("os.replace(", source)
        self.assertNotIn("Path.replace(", source)
        self.assertFalse(
            {"O_APPEND", "O_CREAT", "O_RDWR", "O_TRUNC", "O_WRONLY"} & loaded_names
        )
        self.assertNotIn("argparse", imported_modules)
        self.assertEqual(
            public_functions,
            {"observe_codex_private_sqlite_bundle"},
        )

    def test_raw_path_traversal_is_rejected_before_normalization(self) -> None:
        with self.assertRaisesRegex(BulkloadError, "must be absolute"):
            verifier._open_absolute_parent(Path("/private/tmp/safe/../sealed-bundle"))
        with self.assertRaisesRegex(BulkloadError, "must be absolute"):
            verifier._open_absolute_parent(Path("relative/sealed-bundle"))
        with self.assertRaisesRegex(BulkloadError, "must be absolute"):
            verifier._open_absolute_parent(Path("~/git/not-a-bundle"))

    def test_strict_json_reader_rejects_duplicate_nonfinite_and_noncanonical(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            root.chmod(0o700)
            descriptor = os.open(
                root,
                os.O_RDONLY
                | getattr(os, "O_DIRECTORY", 0)
                | getattr(os, "O_CLOEXEC", 0),
            )
            self.addCleanup(os.close, descriptor)
            payloads = {
                "duplicate.json": b'{"a":1,"a":2}\n',
                "nonfinite.json": b'{"a":NaN}\n',
                "whitespace.json": b'{ "a":1 }\n',
                "newline.json": b'{"a":1}\n\n',
            }
            for name, payload in payloads.items():
                path = root / name
                path.write_bytes(payload)
                path.chmod(0o600)
                with self.subTest(name=name):
                    with self.assertRaises(BulkloadError):
                        verifier._read_private_json_at(
                            descriptor,
                            name,
                            label=name,
                        )

    def test_public_oracle_requires_exact_verifier_source_inventory_binding(
        self,
    ) -> None:
        self.assertEqual(
            verifier.PRIVATE_SQLITE_VERIFIER_SOURCE_PATH,
            "scripts/bulkload_lib/private_sqlite_verifier.py",
        )
        self.assertNotIn(
            verifier.PRIVATE_SQLITE_VERIFIER_SOURCE_PATH,
            private_runtime.LEGACY_PRIVATE_RUNTIME_AUTHORITY_V6["source_digests"],
        )
        self.assertIn(
            verifier.PRIVATE_SQLITE_VERIFIER_SOURCE_PATH,
            private_runtime.PRIVATE_RUNTIME_SOURCE_KEYS,
        )
        self.assertNotIn(
            "scripts/bulkload_lib/private_sqlite_composer.py",
            private_runtime.PRIVATE_RUNTIME_SOURCE_KEYS,
        )
        verifier_path = Path(inspect.getsourcefile(verifier)).resolve()
        skill_root = verifier_path.parents[2]
        policy = json.loads(
            (
                skill_root / "references" / private_runtime.PRIVATE_STATE_POLICY_NAME
            ).read_text(encoding="utf-8")
        )
        self.assertEqual(
            policy["source_digests"][verifier.PRIVATE_SQLITE_VERIFIER_SOURCE_PATH],
            hashlib.sha256(verifier_path.read_bytes()).hexdigest(),
        )
        with (
            mock.patch.object(verifier, "_verify_input_bindings"),
            mock.patch.object(
                private_runtime,
                "open_pinned_private_runtime_authority",
                return_value=nullcontext(mock.Mock()),
            ),
        ):
            with self.assertRaisesRegex(
                BulkloadError,
                "does not bind the verifier oracle source",
            ):
                verifier.observe_codex_private_sqlite_bundle(
                    Path("/not-opened"),
                    {},
                    {},
                    {},
                    {},
                    verifier_runtime_authority=deepcopy(
                        private_runtime.LEGACY_PRIVATE_RUNTIME_AUTHORITY_V6
                    ),
                )

    def test_public_oracle_revalidates_pinned_runtime_on_zero_report(self) -> None:
        verifier_path = Path(inspect.getsourcefile(verifier)).resolve()
        skill_root = verifier_path.parents[2]
        policy_payload = (
            skill_root / "references" / private_runtime.PRIVATE_STATE_POLICY_NAME
        ).read_bytes()
        policy = json.loads(policy_payload)
        authority = {
            "schema": private_runtime.PRIVATE_RUNTIME_AUTHORITY_SCHEMA,
            "policy_schema": policy["schema"],
            "policy_sha256": hashlib.sha256(policy_payload).hexdigest(),
            "runtime_source_sha256": policy["runtime_source_sha256"],
            "source_digests": deepcopy(policy["source_digests"]),
        }
        lease = mock.Mock()
        with tempfile.TemporaryDirectory() as directory:
            fixture = build_handbuilt_bundle(
                Path(directory).resolve(),
                actual_rows=[(1, b"one")],
            )
            with (
                mock.patch.object(verifier, "_verify_input_bindings"),
                mock.patch.object(
                    private_runtime,
                    "open_pinned_private_runtime_authority",
                    return_value=nullcontext(lease),
                ),
            ):
                report = verifier.observe_codex_private_sqlite_bundle(
                    fixture.bundle_path,
                    fixture.action_plan,
                    fixture.opening_plan,
                    fixture.compose_request,
                    fixture.capacity_observation,
                    verifier_runtime_authority=authority,
                )
        self.assertEqual(report["failures"], [])
        self.assertTrue(all(value is False for value in report["claims"].values()))
        lease.revalidate.assert_called_once_with()

    def test_input_binding_recomputes_v6_request_against_action(self) -> None:
        action_plan = {
            "schema": verifier.PRIVATE_SQLITE_ACTION_PLAN_SCHEMA,
            "runtime_authority": deepcopy(
                private_runtime.LEGACY_PRIVATE_RUNTIME_AUTHORITY_V5_REPAIRED
            ),
        }
        compose_request = {
            "schema": verifier.PRIVATE_SQLITE_COMPOSE_REQUEST_SCHEMA,
            "request_runtime_authority": deepcopy(
                private_runtime.LEGACY_PRIVATE_RUNTIME_AUTHORITY_V6
            ),
            "action_plan_producer_runtime_authority": deepcopy(
                private_runtime.LEGACY_PRIVATE_RUNTIME_AUTHORITY_V5_REPAIRED
            ),
            "output_intent": {"workspace": {"marker": "exact-workspace"}},
        }
        capacity_observation = {
            "schema": verifier.PRIVATE_SQLITE_CAPACITY_OBSERVATION_SCHEMA,
            "observation_runtime_authority": deepcopy(
                private_runtime.LEGACY_PRIVATE_RUNTIME_AUTHORITY_V6
            ),
        }
        with (
            mock.patch.object(
                verifier,
                "validate_codex_private_sqlite_compose_plan",
            ),
            mock.patch.object(
                verifier,
                "validate_codex_private_sqlite_action_plan",
            ),
            mock.patch.object(
                verifier,
                "validate_codex_private_sqlite_compose_request",
            ),
            mock.patch.object(
                verifier,
                "validate_codex_private_sqlite_capacity_observation",
            ),
            mock.patch.object(
                verifier,
                "validate_codex_private_sqlite_compose_request_against_action",
                side_effect=BulkloadError("exact v6 derivation sentinel"),
            ) as exact_validator,
        ):
            with self.assertRaisesRegex(
                BulkloadError,
                "exact v6 derivation sentinel",
            ):
                verifier._verify_input_bindings(
                    action_plan,
                    {},
                    compose_request,
                    capacity_observation,
                )
        exact_validator.assert_called_once_with(
            compose_request,
            action_plan,
            compose_request["output_intent"]["workspace"],
            private_runtime.LEGACY_PRIVATE_RUNTIME_AUTHORITY_V6,
        )

    def test_typed_blob_bound_accepts_over_8mib_and_exact_64mib(self) -> None:
        over_eight = b"x" * (8 * 1024 * 1024 + 17)
        encoded = verifier._typed_value("blob", over_eight)
        self.assertEqual(encoded[:1], b"B")
        self.assertEqual(
            int.from_bytes(encoded[1:9], "big"),
            len(over_eight),
        )
        del encoded, over_eight

        exact = bytes(MAX_SQLITE_VALUE_BYTES)
        encoded = verifier._typed_value("blob", exact)
        self.assertEqual(
            int.from_bytes(encoded[1:9], "big"),
            MAX_SQLITE_VALUE_BYTES,
        )
        del encoded, exact
        with self.assertRaisesRegex(BulkloadError, "exceeds its byte budget"):
            verifier._typed_value(
                "blob",
                bytes(MAX_SQLITE_VALUE_BYTES + 1),
            )

    def test_table_scan_allows_record_overhead_at_exact_value_bound(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            with mock.patch.object(verifier, "MAX_SQLITE_VALUE_BYTES", 1024):
                fixture = build_handbuilt_bundle(
                    root,
                    actual_rows=[(1, bytes(1024))],
                )
                report = verifier._observe_codex_private_sqlite_bundle(
                    fixture.bundle_path,
                    fixture.action_plan,
                    fixture.opening_plan,
                    fixture.compose_request,
                    fixture.capacity_observation,
                    verifier_runtime_binding=fixture.verifier_runtime_binding,
                    observation_id=OBSERVATION_ID,
                    observed_at=COMPLETED_AT,
                )
            self.assertEqual(report["failures"], [])
            self.assertTrue(report["observed_checks"]["semantic_comparisons_observed"])

    def test_semantic_budget_counts_beyond_5gib_without_materialization(
        self,
    ) -> None:
        five_gib = 5 * 1024**3
        budget = {"rows": 17, "bytes": five_gib}
        verifier._charge_semantic_budget(budget, 23)
        self.assertEqual(budget, {"rows": 18, "bytes": five_gib + 23})

        budget = {
            "rows": verifier.MAX_SQLITE_PLAN_ROWS - 1,
            "bytes": verifier.MAX_SQLITE_PLAN_ROW_BYTES - 1,
        }
        verifier._charge_semantic_budget(budget, 1)
        self.assertEqual(budget["rows"], verifier.MAX_SQLITE_PLAN_ROWS)
        self.assertEqual(budget["bytes"], verifier.MAX_SQLITE_PLAN_ROW_BYTES)
        with self.assertRaisesRegex(BulkloadError, "semantic budget exceeded"):
            verifier._charge_semantic_budget(budget, 0)

    def test_handbuilt_owner_private_bundle_yields_oracle_only_report(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            rows = [
                (1, b"shared"),
                (2, b"x" * (8 * 1024 * 1024 + 17)),
                (3, b"destination"),
            ]
            fixture = build_handbuilt_bundle(root, actual_rows=rows)
            validate_composed_bundle_manifest(fixture.manifest)
            validate_composition_receipt(fixture.receipt)
            before = _tree_snapshot(root)
            report = verifier._observe_codex_private_sqlite_bundle(
                fixture.bundle_path,
                fixture.action_plan,
                fixture.opening_plan,
                fixture.compose_request,
                fixture.capacity_observation,
                verifier_runtime_binding=fixture.verifier_runtime_binding,
                observation_id=OBSERVATION_ID,
                observed_at=COMPLETED_AT,
            )
            after = _tree_snapshot(root)
            self.assertEqual(after, before)
            self.assertEqual(report["schema"], VERIFIER_ORACLE_REPORT_SCHEMA)
            self.assertEqual(report["failures"], [])
            self.assertTrue(all(report["observed_checks"].values()))
            self.assertEqual(set(report["claims"]), ORACLE_FALSE_CLAIMS)
            self.assertTrue(all(value is False for value in report["claims"].values()))
            validate_verifier_oracle_report(report)

    def test_receipt_binds_exact_real_graph_prefix_before_serialization(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = build_handbuilt_bundle(
                Path(directory).resolve(),
                actual_rows=[(1, b"one")],
            )
            graph = fixture.action_plan["operation_graph"]
            node_ids = [node["id"] for node in graph["nodes"]]
            self.assertEqual(
                node_ids,
                [
                    "create-versioned-staging",
                    f"baseline:{BASENAME}",
                    f"source-only:{BASENAME}:items",
                    f"verify:{BASENAME}",
                    "write-manifest",
                    "write-receipt",
                    "fsync-bundle",
                    "seal-bundle",
                ],
            )
            self.assertEqual(
                fixture.receipt["operation_graph"]["completed_node_ids"],
                PRE_RECEIPT_COMPLETED_NODE_IDS,
            )
            self.assertEqual(
                fixture.receipt["operation_graph"]["completed_node_ids"],
                completed_before_receipt(graph),
            )

            report = verifier._observe_codex_private_sqlite_bundle(
                fixture.bundle_path,
                fixture.action_plan,
                fixture.opening_plan,
                fixture.compose_request,
                fixture.capacity_observation,
                verifier_runtime_binding=fixture.verifier_runtime_binding,
                observation_id=OBSERVATION_ID,
                observed_at=COMPLETED_AT,
            )
        self.assertNotIn(
            "operation-graph-binding-differs",
            {failure["code"] for failure in report["failures"]},
        )

    def test_capacity_admission_families_bind_exact_action_inventory(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = build_handbuilt_bundle(
                Path(directory).resolve(),
                actual_rows=[(1, b"one")],
            )
            receipt = deepcopy(fixture.receipt)
            for observation in receipt["capacity_admission"]["observations"]:
                if observation["family"] is not None:
                    observation["family"] = "other.sqlite"
            self_digest(receipt, "receipt_sha256")
            _rewrite_canonical_json(
                fixture.bundle_path / "composition-receipt.json",
                receipt,
            )
            report = verifier._observe_codex_private_sqlite_bundle(
                fixture.bundle_path,
                fixture.action_plan,
                fixture.opening_plan,
                fixture.compose_request,
                fixture.capacity_observation,
                verifier_runtime_binding=fixture.verifier_runtime_binding,
                observation_id=OBSERVATION_ID,
                observed_at=COMPLETED_AT,
            )
        self.assertIn(
            "capacity-admission-differs",
            {failure["code"] for failure in report["failures"]},
        )
        self.assertFalse(
            report["observed_checks"]["composition_receipt_structure_observed"]
        )

    def test_manifest_receipt_and_oracle_chronology_is_cross_bound(self) -> None:
        for label, manifest_created_at in (
            ("before-final-capacity", "2026-07-29T18:05:59Z"),
            ("after-receipt", "2026-07-29T18:08:00Z"),
        ):
            with (
                self.subTest(label=label),
                tempfile.TemporaryDirectory() as directory,
            ):
                fixture = build_handbuilt_bundle(
                    Path(directory).resolve(),
                    actual_rows=[(1, b"one")],
                )
                manifest = deepcopy(fixture.manifest)
                manifest["created_at"] = manifest_created_at
                self_digest(manifest, "manifest_sha256")
                manifest_payload = canonical_file(manifest)
                receipt = deepcopy(fixture.receipt)
                receipt["manifest"] = {
                    "schema": manifest["schema"],
                    "manifest_sha256": manifest["manifest_sha256"],
                    "canonical_file_sha256": hashlib.sha256(
                        manifest_payload
                    ).hexdigest(),
                    "canonical_file_bytes": len(manifest_payload),
                }
                self_digest(receipt, "receipt_sha256")
                _rewrite_canonical_json(
                    fixture.bundle_path / "manifest.json",
                    manifest,
                )
                _rewrite_canonical_json(
                    fixture.bundle_path / "composition-receipt.json",
                    receipt,
                )
                report = verifier._observe_codex_private_sqlite_bundle(
                    fixture.bundle_path,
                    fixture.action_plan,
                    fixture.opening_plan,
                    fixture.compose_request,
                    fixture.capacity_observation,
                    verifier_runtime_binding=fixture.verifier_runtime_binding,
                    observation_id=OBSERVATION_ID,
                    observed_at=COMPLETED_AT,
                )
            self.assertIn(
                "composition-chronology-differs",
                {failure["code"] for failure in report["failures"]},
            )
            self.assertFalse(
                report["observed_checks"]["composition_receipt_structure_observed"]
            )

    def test_claimed_sqlite_engine_must_equal_observed_engine(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = build_handbuilt_bundle(
                Path(directory).resolve(),
                actual_rows=[(1, b"one")],
            )
            forged_observation = deepcopy(fixture.manifest["sqlite_engine_authority"])
            forged_observation["sqlite_version"] = "9.9.9"
            with mock.patch.object(
                verifier,
                "_sqlite_engine_authority",
                return_value=forged_observation,
            ):
                report = verifier._observe_codex_private_sqlite_bundle(
                    fixture.bundle_path,
                    fixture.action_plan,
                    fixture.opening_plan,
                    fixture.compose_request,
                    fixture.capacity_observation,
                    verifier_runtime_binding=fixture.verifier_runtime_binding,
                    observation_id=OBSERVATION_ID,
                    observed_at=COMPLETED_AT,
                )
            self.assertIn(
                "claimed-sqlite-engine-differs",
                {failure["code"] for failure in report["failures"]},
            )
            self.assertFalse(
                report["observed_checks"]["sqlite_engine_consistency_observed"]
            )

    def test_late_sqlite_tree_mutation_blocks_report_return(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = build_handbuilt_bundle(
                Path(directory).resolve(),
                actual_rows=[(1, b"one")],
            )
            late_sidecar = fixture.bundle_path / "sqlite" / "late-sidecar"

            def inject_late_sidecar(_: dict[str, object]) -> None:
                late_sidecar.write_bytes(b"late")
                late_sidecar.chmod(0o600)

            with mock.patch.object(
                verifier,
                "validate_verifier_oracle_report",
                side_effect=inject_late_sidecar,
            ):
                with self.assertRaisesRegex(
                    BulkloadError,
                    "offline SQLite candidate directory changed",
                ):
                    verifier._observe_codex_private_sqlite_bundle(
                        fixture.bundle_path,
                        fixture.action_plan,
                        fixture.opening_plan,
                        fixture.compose_request,
                        fixture.capacity_observation,
                        verifier_runtime_binding=fixture.verifier_runtime_binding,
                        observation_id=OBSERVATION_ID,
                        observed_at=COMPLETED_AT,
                    )

    def test_self_redigested_wrong_semantic_output_is_observed_as_failure(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            expected_rows = [
                (1, b"shared"),
                (2, b"source"),
                (3, b"destination"),
            ]
            actual_rows = [
                (1, b"shared"),
                (2, b"wrong-but-self-redigested"),
                (3, b"destination"),
            ]
            fixture = build_handbuilt_bundle(
                root,
                actual_rows=actual_rows,
                expected_rows=expected_rows,
            )
            # Both writer-origin bodies are structurally valid and bind the
            # actual wrong bytes. They are still only claims for the oracle to
            # compare with the supplied action.
            validate_composed_bundle_manifest(fixture.manifest)
            validate_composition_receipt(fixture.receipt)
            report = verifier._observe_codex_private_sqlite_bundle(
                fixture.bundle_path,
                fixture.action_plan,
                fixture.opening_plan,
                fixture.compose_request,
                fixture.capacity_observation,
                verifier_runtime_binding=fixture.verifier_runtime_binding,
                observation_id=OBSERVATION_ID,
                observed_at=COMPLETED_AT,
            )
            self.assertIn(
                "semantic-union-differs",
                {failure["code"] for failure in report["failures"]},
            )
            self.assertTrue(report["families"][0]["manifest_matches_observed"])
            self.assertFalse(
                report["families"][0]["semantic_output_matches_action_observed"]
            )
            self.assertFalse(report["observed_checks"]["semantic_comparisons_observed"])
            self.assertTrue(all(value is False for value in report["claims"].values()))
            validate_verifier_oracle_report(report)


if __name__ == "__main__":
    unittest.main()
