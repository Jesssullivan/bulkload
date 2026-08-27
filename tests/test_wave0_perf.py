"""Wave-0 performance-slice regressions.

Every slice in this wave is required to be output-identical: no digest value
and no artifact field name may change. These tests pin the *behaviour* that
each redundant read was carrying, so the redundancy can be removed without
removing a fence.
"""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

from bulkload_lib.model import BulkloadError, canonical_bytes, sha256_bytes
from bulkload_lib import scanner
from bulkload_lib.scanner import (
    _jsonl_records,
    capture_agent_state,
    validate_agent_capture,
    validate_live_snapshot_generation,
)

from test_bulkload import CutoverFixture


def reference_jsonl_records(path: Path, *, replacements=()) -> dict:
    """The pre-slice implementation, kept verbatim as the value oracle."""
    hashes: list[str] = []
    transformed_hashes: list[str] = []
    hasher = hashlib.sha256()
    transformed_hasher = hashlib.sha256()
    with path.open("rb") as stream:
        for line in stream:
            json.loads(line)
            transformed = line
            for source, destination in replacements:
                transformed = transformed.replace(source, destination)
            json.loads(transformed)
            hashes.append(sha256_bytes(line))
            transformed_hashes.append(sha256_bytes(transformed))
            hasher.update(line)
            transformed_hasher.update(transformed)
    return {
        "records": hashes,
        "records_sha256": sha256_bytes(canonical_bytes(hashes)),
        "sha256": hasher.hexdigest(),
        "translated_records": transformed_hashes,
        "translated_sha256": transformed_hasher.hexdigest(),
    }


def live_capture(
    fixture: CutoverFixture,
    name: str,
    *,
    role: str = "source",
    base: dict | None = None,
) -> dict:
    home = fixture.source_home if role == "source" else fixture.destination_home
    git_root = fixture.source_git if role == "source" else fixture.destination_git
    seats = fixture.source_seats if role == "source" else fixture.destination_seats
    return capture_agent_state(
        role=role,
        home=home,
        git_root=git_root,
        codex_root=None,
        claude_root=None,
        pi_root=None,
        seats=seats,
        path_map=fixture.path_map,
        writers_quiesced=False,
        snapshot_root=fixture.root / "evidence" / f"{name}.snapshot",
        snapshot_base_seal=Path(base["catalog"]["snapshot"]["seal_path"])
        if base is not None
        else None,
        managed_exclusions=fixture.managed_exclusions,
        rsync_path=fixture.rsync_path,
        max_files=50_000,
        max_bytes=4 * 1024**3,
        max_sqlite_rows=100_000,
        snapshot_reserve_bytes=0,
    )


class SingleEpochFenceTests(unittest.TestCase):
    """W0-1: the second back-to-back epoch() was redundant, not a fence."""

    def test_single_epoch_still_fails_on_a_real_post_seal_divergence(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=True)
            capture = live_capture(fixture, "source-a")
            validate_agent_capture(capture, expected_role="source")
            snapshot = capture["catalog"]["snapshot"]

            # The sealed snapshot still matches the untouched live tree.
            validate_live_snapshot_generation(snapshot)

            # Mutating a live payload byte after the seal must still abort,
            # with one epoch exactly as it did with two.
            tracked = fixture.source_repo / "untracked.txt"
            tracked.write_bytes(b"mutated after the immutable seal\n")
            with self.assertRaisesRegex(
                BulkloadError, "live source changed after immutable snapshot B"
            ):
                validate_live_snapshot_generation(snapshot)

    def test_same_size_mutation_is_still_caught_by_the_single_epoch(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=True)
            history = fixture.source_home / ".codex" / "history.jsonl"
            original = history.read_bytes()
            capture = live_capture(fixture, "source-a")
            snapshot = capture["catalog"]["snapshot"]
            replacement = bytearray(original)
            replacement[-2] = original[-2] ^ 0x01
            history.write_bytes(bytes(replacement))
            self.assertEqual(len(bytes(replacement)), len(original))
            with self.assertRaisesRegex(
                BulkloadError, "live source changed after immutable snapshot B"
            ):
                validate_live_snapshot_generation(snapshot)

    def test_validate_runs_exactly_one_generation_pass_per_root(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=True)
            capture = live_capture(fixture, "source-a")
            snapshot = capture["catalog"]["snapshot"]
            real = scanner._tree_generation
            calls: list[str] = []

            def counted(root, **kwargs):
                calls.append(str(root))
                return real(root, **kwargs)

            with mock.patch.object(scanner, "_tree_generation", side_effect=counted):
                validate_live_snapshot_generation(snapshot)
            self.assertEqual(len(calls), len(snapshot["roots"]))


class IdenticalLineShortCircuitTests(unittest.TestCase):
    """W0-2: a no-op transform must reuse the original parse and hash."""

    @staticmethod
    def write_corpus(path: Path) -> None:
        path.write_bytes(
            b"".join(
                json.dumps(
                    {"cwd": f"/Users/one/git/repo/{index}", "n": index},
                    sort_keys=True,
                ).encode()
                + b"\n"
                for index in range(64)
            )
        )

    def test_no_replacements_produce_the_reference_record(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "history.jsonl"
            self.write_corpus(path)
            self.assertEqual(_jsonl_records(path), reference_jsonl_records(path))

    def test_non_matching_replacement_produces_the_reference_record(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "history.jsonl"
            self.write_corpus(path)
            replacements = ((b"/Users/absent", b"/Users/other"),)
            self.assertEqual(
                _jsonl_records(path, replacements=replacements),
                reference_jsonl_records(path, replacements=replacements),
            )

    def test_full_and_partial_rewrites_produce_the_reference_record(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "history.jsonl"
            self.write_corpus(path)
            for replacements in (
                ((b"/Users/one", b"/Users/two"),),
                # Only one line matches, so the lazy fork happens mid-file and
                # has to carry every earlier identical line with it.
                ((b"repo/7", b"repo/x"),),
                ((b"/Users/one", b"/Users/two"), (b"repo/1", b"repo/y")),
            ):
                with self.subTest(replacements=replacements):
                    self.assertEqual(
                        _jsonl_records(path, replacements=replacements),
                        reference_jsonl_records(path, replacements=replacements),
                    )

    def test_partial_rewrite_first_line_only(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "history.jsonl"
            path.write_bytes(b'{"a":"one"}\n{"a":"two"}\n{"a":"three"}\n')
            replacements = ((b"one", b"ONE"),)
            self.assertEqual(
                _jsonl_records(path, replacements=replacements),
                reference_jsonl_records(path, replacements=replacements),
            )

    def test_identical_lines_are_parsed_once(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "history.jsonl"
            self.write_corpus(path)
            real = scanner.json.loads
            calls: list[int] = []

            def counted(payload, *args, **kwargs):
                calls.append(len(payload))
                return real(payload, *args, **kwargs)

            with mock.patch.object(scanner.json, "loads", side_effect=counted):
                _jsonl_records(path)
            self.assertEqual(len(calls), 64)

    def test_rewrite_to_invalid_jsonl_still_raises(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "history.jsonl"
            path.write_bytes(b'{"a":"one"}\n')
            with self.assertRaisesRegex(
                BulkloadError, "path rewriting produced invalid JSONL"
            ):
                _jsonl_records(path, replacements=((b'"a"', b"a"),))

    def test_incomplete_final_record_still_raises(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "history.jsonl"
            path.write_bytes(b'{"a":"one"}\n{"a":"tw')
            with self.assertRaises(scanner._MalformedAppendState):
                _jsonl_records(path)


if __name__ == "__main__":
    unittest.main()
