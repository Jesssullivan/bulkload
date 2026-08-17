from __future__ import annotations

import sqlite3
import time
import unittest

from bulkload_lib.private_sqlite_plan import (
    _classify_table,
    _semantic_row_stream,
    _structured_schema,
)


class CodexPrivateSqliteExpectedOutputTest(unittest.TestCase):
    def _database(self, rows: list[tuple[int, str]]) -> sqlite3.Connection:
        connection = sqlite3.connect(":memory:")
        connection.execute(
            "CREATE TABLE items(id INTEGER PRIMARY KEY, value TEXT) STRICT"
        )
        connection.executemany("INSERT INTO items VALUES (?, ?)", rows)
        return connection

    def _contract(self, connection: sqlite3.Connection) -> dict:
        contract, blockers, _ = _structured_schema(
            connection,
            deadline=time.monotonic() + 30,
        )
        self.assertEqual(blockers, [])
        return contract

    def _classify(
        self,
        source: sqlite3.Connection,
        destination: sqlite3.Connection,
        *,
        include_expected_output: bool,
    ) -> tuple[dict, list[dict]]:
        source_contract = self._contract(source)
        destination_contract = self._contract(destination)
        return _classify_table(
            source,
            destination,
            basename="logs_1.sqlite",
            source_contract=source_contract,
            destination_contract=destination_contract,
            table_rule={
                "name": "items",
                "merge_class": "keyed-union",
                "identity_columns": ["id"],
            },
            registry_family={"path_authorities": []},
            path_map={
                "source_session_root": "/source/sessions",
                "destination_session_root": "/destination/sessions",
                "rules": [],
            },
            accepted_session_paths={"source": {}, "destination": {}},
            deadline=time.monotonic() + 30,
            include_expected_output=include_expected_output,
        )

    def test_expected_output_digest_matches_independent_complete_stream(self) -> None:
        source = self._database([(1, "shared"), (2, "source")])
        destination = self._database([(1, "shared"), (3, "destination")])
        expected = self._database([(1, "shared"), (2, "source"), (3, "destination")])
        self.addCleanup(source.close)
        self.addCleanup(destination.close)
        self.addCleanup(expected.close)

        relation, blockers = self._classify(
            source,
            destination,
            include_expected_output=True,
        )
        self.assertEqual(blockers, [])
        self.assertEqual(
            {
                "shared_equal": relation["shared_equal"],
                "source_only": relation["source_only"],
                "destination_only": relation["destination_only"],
                "conflicts": relation["conflicts"],
            },
            {
                "shared_equal": 1,
                "source_only": 1,
                "destination_only": 1,
                "conflicts": 0,
            },
        )

        contract = self._contract(expected)
        table = next(item for item in contract["tables"] if item["name"] == "items")
        state: dict = {}
        stream = _semantic_row_stream(
            expected,
            basename="logs_1.sqlite",
            role="destination",
            table_contract=table,
            table_rule={
                "name": "items",
                "merge_class": "keyed-union",
                "identity_columns": ["id"],
            },
            path_rules=[],
            path_map={
                "source_session_root": "/source/sessions",
                "destination_session_root": "/destination/sessions",
            },
            accepted_session_paths={},
            deadline=time.monotonic() + 30,
            state=state,
        )
        list(stream)
        self.assertEqual(
            {
                key: value
                for key, value in relation["expected_output"].items()
                if key != "partitions"
            },
            state,
        )
        partitions = relation["expected_output"]["partitions"]
        self.assertEqual(
            {name: partition["row_count"] for name, partition in partitions.items()},
            {
                "shared_equal": 1,
                "source_only": 1,
                "destination_only": 1,
            },
        )
        self.assertEqual(
            relation["expected_output"]["classified_bytes"],
            sum(partition["classified_bytes"] for partition in partitions.values()),
        )

    def test_default_v4_shape_does_not_gain_expected_output(self) -> None:
        source = self._database([(1, "shared")])
        destination = self._database([(1, "shared")])
        self.addCleanup(source.close)
        self.addCleanup(destination.close)

        relation, blockers = self._classify(
            source,
            destination,
            include_expected_output=False,
        )
        self.assertEqual(blockers, [])
        self.assertNotIn("expected_output", relation)

    def test_conflict_has_no_expected_output_authority(self) -> None:
        source = self._database([(1, "source")])
        destination = self._database([(1, "destination")])
        self.addCleanup(source.close)
        self.addCleanup(destination.close)

        relation, blockers = self._classify(
            source,
            destination,
            include_expected_output=True,
        )
        self.assertEqual(relation["conflicts"], 1)
        self.assertIsNone(relation["expected_output"])
        self.assertEqual(
            blockers,
            [
                {
                    "code": "sqlite-shared-row-divergence",
                    "table": "items",
                    "count": 1,
                }
            ],
        )


if __name__ == "__main__":
    unittest.main()
