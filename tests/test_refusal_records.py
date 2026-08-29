"""Structured refusal records (T1).

Every test here pins one shape: what a refusal names, where the naming goes,
and what the operator is told to do about it. The engine's historical
behaviour is preserved exactly — the same conditions still refuse, with the
same sentence as the prefix of the message — so these tests are additive
guards over an additive channel.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
import stat
import tempfile
import unittest
from unittest import mock

from bulkload_lib import cli
from bulkload_lib import executor
from bulkload_lib import scanner
from bulkload_lib.model import (
    AGENT_STAGE_SCHEMA,
    REFUSAL_SAMPLE_LIMIT,
    REFUSAL_SCHEMA,
    REFUSAL_STRING_LIMIT,
    BulkloadError,
    canonical_bytes,
    first_mismatch,
    refusal_check,
    refusal_eq,
    refusal_json,
    refuse,
    seal,
    sha256_bytes,
)
from bulkload_lib.scanner import validate_live_snapshot_generation

from test_wave0_perf import CutoverFixture, live_capture


def record_of(error: BulkloadError) -> dict:
    record = getattr(error, "refusal", None)
    assert isinstance(record, dict), f"no refusal record on {error!r}"
    return record


class RefusalRecordShapeTests(unittest.TestCase):
    """The record is a fixed, bounded, canonically serialisable object."""

    def test_every_key_is_always_present_and_serialisable(self) -> None:
        error = refuse(
            "static sentence",
            code="EXAMPLE",
            phase="unit",
            remedy="do the thing",
            detail="field=x expected=1 observed=2",
            root=Path("/tmp/root"),
            label="git",
            sample=[{"path": "/tmp/root/a"}],
            count=1,
        )
        record = record_of(error)
        self.assertEqual(
            sorted(record),
            [
                "code",
                "count",
                "expected",
                "field",
                "label",
                "message",
                "observed",
                "phase",
                "remedy",
                "root",
                "sample",
                "schema",
            ],
        )
        self.assertEqual(record["schema"], REFUSAL_SCHEMA)
        self.assertEqual(record["root"], "/tmp/root")
        # canonical_bytes is the repo's only serialiser and it refuses
        # anything non-canonical, so this assertion is the real contract.
        self.assertIn(b"EXAMPLE", canonical_bytes(record))

    def test_the_original_sentence_stays_the_message_prefix(self) -> None:
        error = refuse(
            "live source changed after immutable snapshot B",
            code="EXAMPLE",
            phase="unit",
            remedy="r",
            detail="changed=1",
        )
        self.assertTrue(
            str(error).startswith("live source changed after immutable snapshot B")
        )
        self.assertEqual(record_of(error)["message"], str(error))

    def test_the_human_line_is_exactly_one_line(self) -> None:
        error = refuse(
            "sentence",
            code="EXAMPLE",
            phase="unit",
            remedy="r",
            detail="first\nsecond\r\nthird",
        )
        self.assertNotIn("\n", str(error))
        self.assertNotIn("\r", str(error))

    def test_samples_and_strings_are_bounded(self) -> None:
        error = refuse(
            "sentence",
            code="EXAMPLE",
            phase="unit",
            remedy="r",
            sample=[{"path": f"/tmp/{index}"} for index in range(200)],
            observed="x" * 5000,
        )
        record = record_of(error)
        self.assertEqual(len(record["sample"]), REFUSAL_SAMPLE_LIMIT)
        self.assertLessEqual(len(record["observed"]), REFUSAL_STRING_LIMIT + 3)

    def test_unrepresentable_values_do_not_break_the_record(self) -> None:
        class Opaque:
            def __repr__(self) -> str:
                return "<opaque>"

        payload = refusal_json({"k": Opaque(), "s": {"b", "a"}, "p": Path("/x")})
        self.assertEqual(payload["k"], "<opaque>")
        self.assertEqual(payload["s"], ["a", "b"])
        self.assertEqual(payload["p"], "/x")
        canonical_bytes(payload)

    def test_a_plain_bulkload_error_still_has_no_record(self) -> None:
        self.assertIsNone(getattr(BulkloadError("plain"), "refusal", "missing"))


class FirstMismatchTests(unittest.TestCase):
    """The diagnosis reproduces the original short-circuit order, lazily."""

    def test_the_first_failing_check_wins(self) -> None:
        result = first_mismatch(
            "FAMILY",
            (
                refusal_eq("A", "one.a", 1, 1),
                refusal_eq("B", "one.b", 2, 3),
                refusal_eq("C", "one.c", 4, 5),
            ),
        )
        self.assertEqual(result["code"], "FAMILY_B")
        self.assertEqual(result["field"], "one.b")
        self.assertEqual((result["expected"], result["observed"]), (2, 3))

    def test_later_checks_are_never_evaluated(self) -> None:
        touched: list[str] = []

        def later() -> bool:
            touched.append("later")
            return True

        first_mismatch(
            "FAMILY",
            (
                refusal_check("A", "a", lambda: False),
                refusal_check("B", "b", later),
            ),
        )
        self.assertEqual(touched, [])

    def test_a_raising_check_counts_as_that_check_failing(self) -> None:
        def boom() -> bool:
            raise KeyError("source")

        result = first_mismatch("FAMILY", (refusal_check("A", "a", boom),))
        self.assertEqual(result["code"], "FAMILY_A")

    def test_nothing_reproducing_returns_the_family_unqualified(self) -> None:
        result = first_mismatch("FAMILY", (refusal_eq("A", "a", 1, 1),))
        self.assertEqual(result["code"], "FAMILY")
        self.assertIn("no single condition", result["detail"])


class FailureOutputTests(unittest.TestCase):
    """`--failure-output` is the machine channel; stderr stays one line."""

    def test_every_verb_accepts_the_flag(self) -> None:
        parser = cli.build_parser()
        required = {
            "agent-capture": [
                "--role",
                "source",
                "--home",
                "h",
                "--git-root",
                "g",
                "--rsync-path",
                "/usr/bin/rsync",
                "--path-map",
                "/a=/b",
            ],
            "agent-plan": [
                "--source-a",
                "a",
                "--source-b",
                "b",
                "--destination-a",
                "c",
                "--destination-b",
                "d",
            ],
            "agent-stage": [
                "--phase",
                "final",
                "--accept-plan-sha256",
                "x",
                "--stage-root",
                "s",
            ],
            "agent-apply": [
                "--plan",
                "p",
                "--stage-receipt",
                "s",
                "--accept-plan-sha256",
                "x",
                "--journal",
                "j",
                "--rollback-root",
                "r",
            ],
            "agent-verify": [
                "--plan",
                "p",
                "--stage-receipt",
                "s",
                "--apply-receipt",
                "a",
            ],
            "agent-rollback": [
                "--apply-receipt",
                "a",
                "--accept-receipt-sha256",
                "x",
            ],
            "agent-recover": [
                "--plan",
                "p",
                "--stage-receipt",
                "s",
                "--journal",
                "j",
                "--strategy",
                "forward",
            ],
        }
        for verb, minimum in required.items():
            with self.subTest(verb=verb):
                arguments = parser.parse_args(
                    [verb, *minimum, "--output", "out.json", "--failure-output", "f"]
                )
                self.assertEqual(arguments.failure_output, "f")

    def test_a_converted_refusal_is_written_and_summarised_once(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            failure = root / "failure.json"
            error = refuse(
                "snapshot payload index count or digest differs",
                code="CUSTODY_REQUIRED_NOT_SEEN",
                phase="validate-snapshot-custody",
                remedy="compare the named paths",
                detail="missing=1",
                sample=[{"relation": "required-not-seen", "path": "/x/y"}],
            )
            with mock.patch.object(cli, "_protect_output", side_effect=error):
                arguments = cli.build_parser().parse_args(
                    [
                        "agent-rollback",
                        "--apply-receipt",
                        "a",
                        "--accept-receipt-sha256",
                        "x",
                        "--output",
                        os.fspath(root / "out.json"),
                        "--failure-output",
                        os.fspath(failure),
                    ]
                )
                with mock.patch.object(cli, "build_parser") as parser:
                    parser.return_value.parse_args.return_value = arguments
                    code = cli.main([])
            self.assertEqual(code, 1)
            record = json.loads(failure.read_text(encoding="utf-8"))
            self.assertEqual(record["code"], "CUSTODY_REQUIRED_NOT_SEEN")
            self.assertEqual(record["command"], "agent-rollback")
            self.assertEqual(record["sample"][0]["path"], "/x/y")
            self.assertEqual(
                stat.S_IMODE(failure.stat().st_mode),
                0o600,
            )

    def test_an_unconverted_refusal_still_writes_a_record(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            failure = root / "failure.json"
            arguments = cli.build_parser().parse_args(
                [
                    "agent-rollback",
                    "--apply-receipt",
                    os.fspath(root / "absent.json"),
                    "--accept-receipt-sha256",
                    "x",
                    "--output",
                    os.fspath(root / "out.json"),
                    "--failure-output",
                    os.fspath(failure),
                ]
            )
            with mock.patch.object(cli, "build_parser") as parser:
                parser.return_value.parse_args.return_value = arguments
                code = cli.main([])
            self.assertEqual(code, 1)
            record = json.loads(failure.read_text(encoding="utf-8"))
            self.assertEqual(record["code"], "UNCLASSIFIED")
            self.assertEqual(record["schema"], REFUSAL_SCHEMA)
            self.assertTrue(record["message"])

    def test_the_failure_path_cannot_collide_with_the_evidence_path(self) -> None:
        parser = cli.build_parser()
        for failure in ("-", "out.json"):
            arguments = parser.parse_args(
                [
                    "agent-rollback",
                    "--apply-receipt",
                    "a",
                    "--accept-receipt-sha256",
                    "x",
                    "--output",
                    "out.json",
                    "--failure-output",
                    failure,
                ]
            )
            with self.subTest(failure=failure):
                with self.assertRaises(BulkloadError):
                    cli._protect_failure_output(arguments)

    def test_an_unwritable_failure_path_does_not_mask_the_refusal(self) -> None:
        arguments = cli.build_parser().parse_args(
            [
                "agent-rollback",
                "--apply-receipt",
                "a",
                "--accept-receipt-sha256",
                "x",
                "--output",
                "out.json",
                "--failure-output",
                "/proc/definitely/not/writable/failure.json",
            ]
        )
        cli._emit_failure(
            arguments,
            cli._failure_record(arguments, BulkloadError("original refusal")),
        )


class LiveGenerationRefusalTests(unittest.TestCase):
    """The epoch fence names the root, the catalog, or the git authority."""

    def test_a_changed_root_is_named_with_its_live_path(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=True)
            capture = live_capture(fixture, "source-a")
            snapshot = capture["catalog"]["snapshot"]
            (fixture.source_repo / "untracked.txt").write_bytes(b"moved\n")
            with self.assertRaises(BulkloadError) as caught:
                validate_live_snapshot_generation(snapshot)
            record = record_of(caught.exception)
            self.assertEqual(record["code"], "LIVE_GENERATION_ROOT")
            self.assertEqual(record["label"], "git")
            self.assertEqual(
                record["root"],
                next(
                    item["live"] for item in snapshot["roots"] if item["label"] == "git"
                ),
            )
            self.assertEqual(record["field"], "generation_sha256")
            self.assertEqual(record["sample"][0]["relation"], "root-generation-changed")
            self.assertIn("git", record["message"])
            self.assertTrue(record["remedy"])

    def test_a_changed_sqlite_catalog_is_named_by_relative_path(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=True)
            capture = live_capture(fixture, "source-a")
            snapshot = capture["catalog"]["snapshot"]
            codex = next(
                root
                for root in snapshot["roots"]
                if root["label"] == "provider-codex" and root["sqlite"]
            )
            relative = codex["sqlite"][0]["relative_path"]
            scanner.sqlite3.connect(Path(codex["live"]) / relative).executescript(
                "CREATE TABLE IF NOT EXISTS refusal_probe(value TEXT);"
                "INSERT INTO refusal_probe VALUES ('t1');"
            )
            with self.assertRaises(BulkloadError) as caught:
                validate_live_snapshot_generation(snapshot)
            record = record_of(caught.exception)
            findings = [
                item
                for item in record["sample"]
                if item["relation"] == "sqlite-logical-changed"
            ]
            self.assertTrue(findings)
            self.assertEqual(findings[0]["relative_path"], relative)
            self.assertEqual(findings[0]["label"], "provider-codex")

    @staticmethod
    def git_rows_change_after(passes_before: int, mutate) -> mock._patch:
        """Return rows verbatim for N calls, then a mutated copy."""
        real = scanner._git_live_authority_rows
        state = {"calls": 0}

        def hooked(root):
            rows = real(root)
            state["calls"] += 1
            if state["calls"] > passes_before:
                return mutate(json.loads(json.dumps(rows)))
            return rows

        return mock.patch.object(
            scanner, "_git_live_authority_rows", side_effect=hooked
        )

    @staticmethod
    def move_head(rows: list[dict]) -> list[dict]:
        rows[0]["worktrees"][0]["head"] = "0" * 40
        return rows

    def test_a_git_authority_move_names_the_worktree_and_field(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            capture = live_capture(fixture, "source-a")
            snapshot = capture["catalog"]["snapshot"]
            # The first pass reproduces the seal, so its rows become the
            # baseline the second pass is diffed against. That is the only
            # way the worktree can be named: the seal itself carries the git
            # authority as one aggregate digest.
            with self.git_rows_change_after(1, self.move_head):
                with self.assertRaises(BulkloadError) as caught:
                    validate_live_snapshot_generation(snapshot)
            record = record_of(caught.exception)
            self.assertEqual(record["code"], "LIVE_GENERATION_GIT_AUTHORITY")
            changed = [
                item
                for item in record["sample"]
                if item["relation"] == "worktree-changed"
            ]
            self.assertTrue(changed)
            self.assertEqual(changed[0]["field"], "head")
            self.assertEqual(changed[0]["observed"], "0" * 40)
            self.assertTrue(changed[0]["worktree"])

    def test_git_detail_degrades_honestly_without_a_baseline(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            capture = live_capture(fixture, "source-a")
            snapshot = capture["catalog"]["snapshot"]
            # No pass ever reproduces the seal, so nothing can be diffed and
            # the record says so rather than inventing a changed worktree.
            with self.git_rows_change_after(0, self.move_head):
                with self.assertRaises(BulkloadError) as caught:
                    validate_live_snapshot_generation(snapshot)
            record = record_of(caught.exception)
            self.assertEqual(record["code"], "LIVE_GENERATION_GIT_AUTHORITY")
            ranked = [
                item for item in record["sample"] if item["relation"] == "observed-only"
            ]
            self.assertTrue(ranked)
            self.assertEqual(ranked[0]["basis"], "git-dir-mtime-ranked")
            self.assertNotIn(
                "worktree-changed", [item["relation"] for item in record["sample"]]
            )

    def test_a_changed_declaration_is_named_by_field(self) -> None:
        snapshot = {
            "declarations": [
                {
                    "backing": "/live/git",
                    "exists": True,
                    "kind": "git",
                    "link": None,
                    "logical": "/live/git",
                    "name": "git",
                }
            ],
            "git_generation_sha256": "a" * 64,
            "roots": [],
        }
        observation = {
            "declarations": [
                {
                    "backing": "/elsewhere/git",
                    "exists": True,
                    "kind": "git",
                    "link": None,
                    "logical": "/live/git",
                    "name": "git",
                }
            ],
            "git_generation_sha256": "a" * 64,
            "roots": [],
        }
        diagnosis = scanner._diagnose_live_generation(
            snapshot,
            expected_rows=[],
            observation=observation,
            git_rows=[],
            baseline_git_rows=None,
        )
        self.assertEqual(diagnosis["code"], "LIVE_GENERATION_DECLARATION")
        self.assertEqual(diagnosis["sample"][0]["field"], "backing")
        self.assertEqual(diagnosis["sample"][0]["observed"], "/elsewhere/git")

    def test_two_categories_report_as_mixed(self) -> None:
        snapshot = {
            "declarations": [],
            "git_generation_sha256": "a" * 64,
            "roots": [
                {
                    "label": "git",
                    "live": "/live/git",
                    "generation_sha256": "b" * 64,
                    "sqlite": [],
                }
            ],
        }
        observation = {
            "declarations": [],
            "git_generation_sha256": "c" * 64,
            "roots": [
                {"label": "git", "generation_sha256": "d" * 64, "sqlite": []},
            ],
        }
        diagnosis = scanner._diagnose_live_generation(
            snapshot,
            expected_rows=[
                {"label": "git", "generation_sha256": "b" * 64, "sqlite": []}
            ],
            observation=observation,
            git_rows=[],
            baseline_git_rows=None,
        )
        self.assertEqual(diagnosis["code"], "LIVE_GENERATION_MIXED")
        self.assertEqual(diagnosis["count"], 2)


class CaptureGitRefusalTests(unittest.TestCase):
    """Capture-time git fences name the authority that moved under the copy."""

    def test_authority_change_during_capture_names_the_worktree(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            real = scanner._git_live_authority_rows
            state = {"calls": 0}

            def hooked(root):
                rows = real(root)
                state["calls"] += 1
                if state["calls"] > 1:
                    rows = json.loads(json.dumps(rows))
                    rows[0]["worktrees"][0]["index_sha256"] = "f" * 64
                return rows

            with mock.patch.object(
                scanner, "_git_live_authority_rows", side_effect=hooked
            ):
                with self.assertRaises(BulkloadError) as caught:
                    live_capture(fixture, "source-a")
            record = record_of(caught.exception)
            self.assertEqual(record["code"], "LIVE_SNAPSHOT_GIT_AUTHORITY")
            self.assertEqual(record["field"], "index_sha256")
            self.assertEqual(record["observed"], "f" * 64)
            self.assertTrue(record["sample"][0]["worktree"])
            self.assertTrue(
                str(caught.exception).startswith(
                    "Git authority changed during live snapshot"
                )
            )


class CustodyRefusalTests(unittest.TestCase):
    """The five custody totals are five codes, and the diff is in the record."""

    @staticmethod
    def base_snapshot(capture: dict) -> dict:
        return capture["catalog"]["snapshot"]

    def test_required_not_seen_names_the_missing_paths(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            snapshot = self.base_snapshot(live_capture(fixture, "source-a"))
            absent = Path(snapshot["roots"][0]["snapshot"]) / "never-captured.txt"
            with self.assertRaises(BulkloadError) as caught:
                scanner.validate_snapshot_custody(snapshot, required_paths={absent})
            record = record_of(caught.exception)
            self.assertEqual(record["code"], "CUSTODY_REQUIRED_NOT_SEEN")
            self.assertEqual(record["field"], "required_paths")
            self.assertEqual(
                record["sample"][0],
                {"relation": "required-not-seen", "path": os.fspath(absent)},
            )
            self.assertEqual(record["count"], 1)
            self.assertIn(os.fspath(absent), record["message"])

    def test_the_sample_is_capped_at_twenty_offending_paths(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            snapshot = self.base_snapshot(live_capture(fixture, "source-a"))
            root = Path(snapshot["roots"][0]["snapshot"])
            required = {root / f"absent-{index:03d}.txt" for index in range(50)}
            with self.assertRaises(BulkloadError) as caught:
                scanner.validate_snapshot_custody(snapshot, required_paths=required)
            record = record_of(caught.exception)
            self.assertEqual(len(record["sample"]), REFUSAL_SAMPLE_LIMIT)
            self.assertEqual(record["count"], 50)

    def test_a_planted_payload_reports_the_namespace_code(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            snapshot = self.base_snapshot(live_capture(fixture, "source-a"))
            root = next(
                item for item in snapshot["roots"] if item["label"] == "provider-codex"
            )
            (Path(root["snapshot"]) / "planted.json").write_text("{}", encoding="utf-8")
            with self.assertRaises(BulkloadError) as caught:
                scanner.validate_snapshot_custody(snapshot, collect_records=True)
            record = record_of(caught.exception)
            self.assertEqual(record["code"], "CUSTODY_NAMESPACE_COUNT")
            self.assertEqual(record["expected"], snapshot["index_entries"])

    def test_a_tampered_payload_names_the_path_and_the_fields(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            snapshot = self.base_snapshot(live_capture(fixture, "source-a"))
            git_root = next(
                item for item in snapshot["roots"] if item["label"] == "git"
            )
            target = Path(git_root["snapshot"]) / "repo" / "tracked.txt"
            target.write_bytes(b"tampered payload byte\n")
            with self.assertRaises(BulkloadError) as caught:
                scanner.validate_snapshot_custody(snapshot, collect_records=True)
            record = record_of(caught.exception)
            self.assertEqual(record["code"], "CUSTODY_PAYLOAD_RECORD")
            self.assertEqual(record["label"], "git")
            self.assertIn("sha256", record["field"])
            self.assertEqual(record["sample"][0]["path"], os.fspath(target))

    def test_a_tampered_seal_names_which_binding_failed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            snapshot = self.base_snapshot(live_capture(fixture, "source-a"))
            index = Path(snapshot["index_path"])
            index.write_bytes(index.read_bytes() + b"{}\n")
            with self.assertRaises(BulkloadError) as caught:
                scanner.validate_snapshot_custody(snapshot, collect_records=True)
            record = record_of(caught.exception)
            self.assertEqual(record["code"], "CUSTODY_SEAL_INDEX_DIGEST")
            self.assertEqual(record["expected"], snapshot["index_sha256"])


class StageObjectRefusalTests(unittest.TestCase):
    """A corrupt stage object is named, not merely counted."""

    def test_a_short_object_names_the_blob_and_the_field(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            stage_root = Path(temporary) / "stage"
            payload = b"staged bytes\n"
            digest = sha256_bytes(payload)
            path = executor._object_path(stage_root, digest)
            path.parent.mkdir(parents=True)
            path.write_bytes(payload)
            manifest = {
                "entries": [
                    {"kind": "file", "blob_sha256": digest, "size": len(payload) + 1}
                ]
            }
            with self.assertRaises(BulkloadError) as caught:
                executor._verify_stage_objects(manifest, stage_root)
            record = record_of(caught.exception)
            self.assertEqual(record["code"], "STAGE_OBJECT_SIZE")
            self.assertEqual(record["label"], digest)
            self.assertEqual(record["sample"][0]["object"], os.fspath(path))
            self.assertEqual(record["observed"], len(payload))

    def test_a_corrupt_object_reports_the_digest_code(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            stage_root = Path(temporary) / "stage"
            payload = b"staged bytes\n"
            digest = sha256_bytes(payload)
            path = executor._object_path(stage_root, digest)
            path.parent.mkdir(parents=True)
            path.write_bytes(b"corrupt byte\n")
            manifest = {
                "entries": [
                    {"kind": "file", "blob_sha256": digest, "size": len(payload)}
                ]
            }
            with self.assertRaises(BulkloadError) as caught:
                executor._verify_stage_objects(manifest, stage_root)
            self.assertEqual(record_of(caught.exception)["code"], "STAGE_OBJECT_DIGEST")


class TransportReceiptRefusalTests(unittest.TestCase):
    """Receipt-detachment refusals name the field that detached."""

    def test_a_malformed_transport_authority_names_the_field(self) -> None:
        manifest = seal(
            {
                "created_at": "2026-08-29T00:00:00Z",
                "entries": [],
                "holds": [],
                "manifest_sha256": None,
                "phase": "final",
                "plan_sha256": "a" * 64,
                "stage_id": "stage-1",
                "stage_root": "/srv/stage",
            },
            "manifest_sha256",
        )
        receipt = seal(
            {
                "capacity": {},
                "created_at": "2026-08-29T00:00:00Z",
                "manifest": manifest,
                "manifest_sha256": manifest["manifest_sha256"],
                "materialization": {},
                "phase": "final",
                "plan_sha256": "a" * 64,
                "ready_for_apply": True,
                "receipt_id": "receipt-1",
                "receipt_sha256": None,
                "schema": AGENT_STAGE_SCHEMA,
                "stage_root": "/srv/stage",
                "transport": {
                    "allowlist_sha256": "c" * 64,
                    "allowlist_size": 1,
                    "destination_host": "destination",
                    "destination_rsync": {},
                    "mode": "local",
                    "quarantine_root": "/srv/stage/.transport-quarantine",
                    "source_host": "",
                    "source_roots": ["/srv/roots"],
                    "source_rsync": {},
                    "source_snapshot": None,
                    "transport_receipt_sha256": None,
                },
            },
            "receipt_sha256",
        )
        with self.assertRaises(BulkloadError) as caught:
            executor.validate_stage_receipt(receipt)
        error = caught.exception
        self.assertIn("transport authority is invalid", str(error))
        record = record_of(error)
        self.assertEqual(record["code"], "STAGE_TRANSPORT_SOURCE_HOST")
        self.assertEqual(record["field"], "transport.source_host")
        self.assertEqual(record["root"], "/srv/stage")


if __name__ == "__main__":
    unittest.main()
