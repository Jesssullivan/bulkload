"""Wave-0 performance-slice regressions.

Every slice in this wave is required to be output-identical: no digest value
and no artifact field name may change. These tests pin the *behaviour* that
each redundant read was carrying, so the redundancy can be removed without
removing a fence.
"""

from __future__ import annotations

import argparse
import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import socket
import tempfile
import time
import unittest
from unittest import mock

from bulkload_lib import cli
from bulkload_lib.cli import _agent_stage, build_parser
from bulkload_lib import executor
from bulkload_lib.executor import push_agent_transport, stage_agent_plan
from bulkload_lib.model import BulkloadError, canonical_bytes, sha256_bytes
from bulkload_lib import scanner
from bulkload_lib.scanner import (
    _jsonl_records,
    capture_agent_state,
    validate_agent_capture,
    validate_live_snapshot_generation,
)

from test_bulkload import CutoverFixture, compile_agent_plan


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
    """W0-1: the fence keeps both passes; only a dominated site drops one.

    The walk inside the fence is sequential, so one pass is not atomic. These
    tests pin the straggler window the second pass carries, and pin that the
    only site allowed to give it up is recovered by a later full fence.
    """

    def test_fence_fails_on_a_real_post_seal_divergence(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=True)
            capture = live_capture(fixture, "source-a")
            validate_agent_capture(capture, expected_role="source")
            snapshot = capture["catalog"]["snapshot"]

            # The sealed snapshot still matches the untouched live tree.
            validate_live_snapshot_generation(snapshot)

            # Mutating a live payload byte after the seal must still abort.
            tracked = fixture.source_repo / "untracked.txt"
            tracked.write_bytes(b"mutated after the immutable seal\n")
            with self.assertRaisesRegex(
                BulkloadError, "live source changed after immutable snapshot B"
            ):
                validate_live_snapshot_generation(snapshot)

    def test_same_size_mutation_is_still_caught(self) -> None:
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

    @staticmethod
    def straggler_behind_the_cursor(victim: Path):
        """Land a write immediately after the first root is digested.

        That is the exact window a single pass cannot see: the walk has
        already moved past the victim's root, so the pass in flight reports
        the pre-write generation and only the *next* pass diverges.
        """
        real = scanner._tree_generation
        state = {"digested": 0}

        def hooked(root, **keywords):
            result = real(root, **keywords)
            state["digested"] += 1
            if state["digested"] == 1:
                victim.write_bytes(b"straggler written behind the walk cursor\n")
            return result

        return mock.patch.object(scanner, "_tree_generation", side_effect=hooked)

    def test_default_fence_catches_a_straggler_written_behind_the_cursor(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=True)
            capture = live_capture(fixture, "source-a")
            snapshot = capture["catalog"]["snapshot"]
            victim = Path(snapshot["roots"][0]["live"]) / "straggler.txt"
            with self.straggler_behind_the_cursor(victim):
                with self.assertRaisesRegex(
                    BulkloadError, "live source changed after immutable snapshot B"
                ):
                    validate_live_snapshot_generation(snapshot)

    def test_a_dominated_single_pass_is_recovered_by_the_next_full_fence(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=True)
            capture = live_capture(fixture, "source-a")
            snapshot = capture["catalog"]["snapshot"]
            victim = Path(snapshot["roots"][0]["live"]) / "straggler.txt"
            with self.straggler_behind_the_cursor(victim):
                # The dominated pre-push fence genuinely misses this write.
                # That is the cost of `passes=1`, stated rather than hidden.
                validate_live_snapshot_generation(snapshot, passes=1)
                # The full-strength fence that follows it still aborts, which
                # is the whole reason the pre-push site may give up a pass.
                with self.assertRaisesRegex(
                    BulkloadError, "live source changed after immutable snapshot B"
                ):
                    validate_live_snapshot_generation(snapshot)

    def test_pass_count_is_two_by_default_and_one_when_dominated(self) -> None:
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
            self.assertEqual(len(calls), 2 * len(snapshot["roots"]))
            calls.clear()
            with mock.patch.object(scanner, "_tree_generation", side_effect=counted):
                validate_live_snapshot_generation(snapshot, passes=1)
            self.assertEqual(len(calls), len(snapshot["roots"]))

    def test_the_fence_cannot_be_reduced_to_no_passes(self) -> None:
        # The guard runs before the snapshot is read, so a would-be caller
        # cannot disable the fence outright by asking for zero passes.
        for rejected in (0, -1, True, 1.5, "2", None):
            with self.assertRaisesRegex(
                BulkloadError, "live generation fence requires at least one pass"
            ):
                validate_live_snapshot_generation({}, passes=rejected)


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


class DeferredAppendDigestTests(unittest.TestCase):
    """W0-3: the discarded whole-file hash on append-jsonl files."""

    @staticmethod
    def provider_records(fixture: CutoverFixture, role: str) -> dict:
        capture = fixture.capture(role)
        providers = {
            provider["name"]: provider for provider in capture["catalog"]["providers"]
        }
        return {
            item["relative_path"]: item
            for item in providers["codex"]["items"]
            if item["kind"] == "regular"
        }

    def test_append_jsonl_record_keeps_its_exact_digest_and_size(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            payload = fixture.source_home / ".codex" / "history.jsonl"
            records = self.provider_records(fixture, "source")
            record = records["history.jsonl"]
            self.assertEqual(record["classification"], "append-jsonl")
            self.assertEqual(
                record["sha256"],
                hashlib.sha256(payload.read_bytes()).hexdigest(),
            )
            self.assertEqual(record["size"], payload.stat().st_size)
            self.assertEqual(record["sha256"], record["translated_sha256"])

    def test_append_jsonl_payload_is_read_once(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            payload = fixture.source_home / ".codex" / "history.jsonl"
            real = scanner.sha256_file
            hashed: list[str] = []

            def counted(path):
                hashed.append(str(path))
                return real(path)

            with mock.patch.object(scanner, "sha256_file", side_effect=counted):
                self.provider_records(fixture, "source")
            self.assertNotIn(str(payload), hashed)

    def test_malformed_append_state_still_carries_a_full_digest(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            payload = fixture.source_home / ".codex" / "history.jsonl"
            # No trailing newline: _jsonl_records rejects it and the record is
            # rebuilt as portable-private, which must still hash the bytes.
            payload.write_bytes(b'{"session_id":"one","text":"source"}')
            record = self.provider_records(fixture, "source")["history.jsonl"]
            self.assertEqual(record["classification"], "portable-private")
            self.assertEqual(
                record["sha256"],
                hashlib.sha256(payload.read_bytes()).hexdigest(),
            )
            self.assertNotIn("records", record)

    def test_a_deferred_digest_can_never_reach_a_record(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            with mock.patch.object(
                scanner, "_jsonl_records", return_value={"records": []}
            ):
                capture = fixture.capture("source")
            # The guard fails the provider closed rather than emitting a
            # record whose digest was deferred and never restored.
            self.assertFalse(capture["complete"])
            self.assertEqual(
                [
                    blocker["code"]
                    for blocker in capture["catalog"]["blockers"]
                    if "lost its digest" in blocker.get("detail", "")
                ],
                ["provider-capture-failed"],
            )
            self.assertNotIn(
                "codex",
                {provider["name"] for provider in capture["catalog"]["providers"]},
            )

    def test_non_append_records_still_hash_in_file_record(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            auth = fixture.source_home / ".codex" / "auth.json"
            record = self.provider_records(fixture, "source")["auth.json"]
            self.assertEqual(
                record["sha256"], hashlib.sha256(auth.read_bytes()).hexdigest()
            )


FAKE_SSH = """#!/usr/bin/env python3
import os
import shlex
import sys

arguments = sys.argv[1:]
while arguments and arguments[0].startswith("-o"):
    arguments.pop(0)
if arguments and arguments[0] == "--":
    arguments.pop(0)
if not arguments:
    raise SystemExit(90)
arguments.pop(0)
if len(arguments) == 1:
    arguments = shlex.split(arguments[0])
os.execv(arguments[0], arguments)
"""


class TransportChecksumTests(unittest.TestCase):
    """W0-4: rsync --checksum is opt-in, not the transport default."""

    def push_argv(self, *, transport_checksum: bool) -> list[str]:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            plan = fixture.plan()
            fake_ssh = fixture.root / "fake-ssh"
            fake_ssh.write_text(FAKE_SSH, encoding="utf-8")
            fake_ssh.chmod(0o700)
            prepare = stage_agent_plan(
                plan,
                accepted_plan_sha256=plan["plan_sha256"],
                phase="preseed",
                stage_root=fixture.stage,
                allow_accounted_copy=True,
                reserve_bytes=0,
                transport_mode="prepare",
            )
            recorded: list[list[str]] = []
            real = executor.subprocess.run

            def recording(argv, **keywords):
                recorded.append([str(item) for item in argv])
                return real(argv, **keywords)

            with mock.patch.object(executor.subprocess, "run", side_effect=recording):
                push_agent_transport(
                    prepare,
                    fixture.stage / ".transport-allowlist-preseed.nul",
                    accepted_plan_sha256=plan["plan_sha256"],
                    phase="preseed",
                    stage_root=fixture.stage,
                    destination_ssh_host=socket.gethostname(),
                    transport_checksum=transport_checksum,
                    _ssh_binary=str(fake_ssh),
                )
            payload = [argv for argv in recorded if "--files-from=-" in argv]
            self.assertEqual(len(payload), 1)
            return payload[0]

    def test_payload_push_omits_checksum_by_default(self) -> None:
        argv = self.push_argv(transport_checksum=False)
        self.assertNotIn("--checksum", argv)
        self.assertIn("--delay-updates", argv)
        self.assertIn("-a", argv)

    def test_transport_checksum_flag_restores_the_whole_file_pass(self) -> None:
        argv = self.push_argv(transport_checksum=True)
        self.assertIn("--checksum", argv)
        self.assertLess(argv.index("--checksum"), argv.index("--delay-updates"))

    def test_parser_default_is_off_and_the_flag_is_push_only(self) -> None:
        base = [
            "agent-stage",
            "--phase",
            "preseed",
            "--accept-plan-sha256",
            "0" * 64,
            "--stage-root",
            "/tmp/stage",
            "--output",
            "-",
        ]
        parser = build_parser()
        self.assertFalse(parser.parse_args(base).transport_checksum)
        self.assertTrue(
            parser.parse_args([*base, "--transport-checksum"]).transport_checksum
        )
        arguments = parser.parse_args(
            [*base, "--transport-checksum", "--plan", "/tmp/plan.json"]
        )
        with self.assertRaisesRegex(
            BulkloadError, "transport checksum applies only to the push transport"
        ):
            _agent_stage(arguments)


class SnapshotIndexWriterTests(unittest.TestCase):
    """W0-5 rider: the index writer is buffered but still durable."""

    def test_index_writer_is_buffered_and_still_fsyncs(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=True)
            real_fdopen = scanner.os.fdopen
            buffering: list[object] = []
            synced: list[int] = []

            def recording_fdopen(descriptor, mode="r", *args, **keywords):
                if mode == "wb":
                    buffering.append(args[0] if args else keywords.get("buffering"))
                return real_fdopen(descriptor, mode, *args, **keywords)

            real_fsync = scanner.os.fsync

            def recording_fsync(descriptor):
                synced.append(descriptor)
                return real_fsync(descriptor)

            with (
                mock.patch.object(scanner.os, "fdopen", side_effect=recording_fdopen),
                mock.patch.object(scanner.os, "fsync", side_effect=recording_fsync),
            ):
                capture = live_capture(fixture, "source-a")

            self.assertNotIn(0, buffering)
            self.assertIn(scanner.SNAPSHOT_INDEX_BUFFER_BYTES, buffering)
            self.assertTrue(synced)
            # The seal still describes the bytes that landed on disk.
            snapshot = capture["catalog"]["snapshot"]
            index = Path(snapshot["index_path"])
            self.assertEqual(
                hashlib.sha256(index.read_bytes()).hexdigest(),
                snapshot["index_sha256"],
            )
            self.assertEqual(
                len(index.read_bytes().splitlines()), snapshot["index_entries"]
            )
            validate_agent_capture(capture, expected_role="source")

    def test_a_partial_index_write_is_never_published(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            real = scanner._snapshot_index_record
            seen: list[str] = []

            def failing(path, **keywords):
                seen.append(str(path))
                if len(seen) > 3:
                    raise BulkloadError("synthetic index-write failure")
                return real(path, **keywords)

            with mock.patch.object(
                scanner, "_snapshot_index_record", side_effect=failing
            ):
                with self.assertRaises(BulkloadError):
                    live_capture(fixture, "source-a")
            self.assertFalse((fixture.root / "evidence" / "source-a.snapshot").exists())


class PhaseTimingAndJobsTests(unittest.TestCase):
    """S5' (PH-S0): stderr-only phase timing and --jobs plumbing."""

    def test_default_job_count_is_the_shipped_constant(self) -> None:
        self.assertEqual(scanner.MAX_CAPTURE_WORKSPACE_WORKERS, 3)
        self.assertEqual(scanner.workspace_worker_count(None, 10), 3)
        self.assertEqual(scanner.workspace_worker_count(None, 2), 2)
        self.assertEqual(scanner.workspace_worker_count(None, 0), 0)

    def test_explicit_job_count_is_bounded_by_pending_work(self) -> None:
        self.assertEqual(scanner.workspace_worker_count(1, 10), 1)
        self.assertEqual(scanner.workspace_worker_count(8, 10), 8)
        self.assertEqual(scanner.workspace_worker_count(8, 2), 2)

    def test_out_of_range_job_counts_are_refused(self) -> None:
        for jobs in (0, -1, scanner.MAX_CAPTURE_JOBS + 1):
            with self.subTest(jobs=jobs):
                with self.assertRaisesRegex(
                    BulkloadError, "capture job count is out of range"
                ):
                    scanner.workspace_worker_count(jobs, 10)

    def test_capture_refuses_an_out_of_range_job_count(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            with self.assertRaisesRegex(
                BulkloadError, "capture job count is out of range"
            ):
                capture_agent_state(
                    role="source",
                    home=fixture.source_home,
                    git_root=fixture.source_git,
                    codex_root=None,
                    claude_root=None,
                    pi_root=None,
                    seats=fixture.source_seats,
                    path_map=fixture.path_map,
                    writers_quiesced=True,
                    rsync_path=fixture.rsync_path,
                    jobs=0,
                )

    def test_parser_jobs_default_is_unset(self) -> None:
        base = [
            "agent-capture",
            "--role",
            "source",
            "--home",
            "/home",
            "--git-root",
            "/home/git",
            "--rsync-path",
            "/usr/bin/rsync",
            "--path-map",
            "/a=/b",
            "--output",
            "/tmp/out.json",
        ]
        parser = build_parser()
        self.assertIsNone(parser.parse_args(base).jobs)
        self.assertEqual(parser.parse_args([*base, "--jobs", "8"]).jobs, 8)

    def test_timing_is_silent_only_when_the_operator_asks_for_quiet(self) -> None:
        """T2: telemetry is default-ON; silence is now the opt-in.

        The old contract was the inverse and produced `preseed-push.log` at 0
        bytes for a 2h32m push. `BULKLOAD_PHASE_TIMING` survives only as the
        off switch for callers that cannot reach the CLI flags.
        """
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            loud = io.StringIO()
            with mock.patch.dict(os.environ, {}, clear=False):
                os.environ.pop("BULKLOAD_PHASE_TIMING", None)
                with contextlib.redirect_stderr(loud):
                    capture = live_capture(fixture, "source-a")
            self.assertIn("bulkload-phase ", loud.getvalue())
            validate_agent_capture(capture, expected_role="source")
            quiet = io.StringIO()
            with mock.patch.dict(os.environ, {"BULKLOAD_PHASE_TIMING": "0"}):
                with contextlib.redirect_stderr(quiet):
                    second = live_capture(fixture, "source-b")
            self.assertEqual(quiet.getvalue(), "")
            validate_agent_capture(second, expected_role="source")

    def test_timing_emits_one_stderr_line_per_phase(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            stream = io.StringIO()
            with mock.patch.dict(os.environ, {"BULKLOAD_PHASE_TIMING": "1"}):
                with contextlib.redirect_stderr(stream):
                    capture = live_capture(fixture, "source-a")
            lines = [
                line
                for line in stream.getvalue().splitlines()
                if line.startswith("bulkload-phase ")
            ]
            self.assertTrue(lines)
            phases = {
                dict(field.split("=", 1) for field in line.split(" ")[1:])["phase"]
                for line in lines
            }
            self.assertLessEqual(
                {"census", "charge", "copy", "digest", "catalog", "seal"}, phases
            )
            for line in lines:
                fields = dict(field.split("=", 1) for field in line.split(" ")[1:])
                self.assertEqual(
                    sorted(fields), ["bytes", "files", "phase", "root", "seconds"]
                )
                float(fields["seconds"])
            # Timing is diagnostic only: it never reaches an artifact.
            self.assertNotIn(b"bulkload-phase", canonical_bytes(capture))
            validate_agent_capture(capture, expected_role="source")

    def test_timing_does_not_change_the_sealed_generations(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            silent = io.StringIO()
            with mock.patch.dict(os.environ, {"BULKLOAD_PHASE_TIMING": "0"}):
                with contextlib.redirect_stderr(silent):
                    quiet = live_capture(fixture, "quiet")["catalog"]["snapshot"]
            stream = io.StringIO()
            with mock.patch.dict(os.environ, {"BULKLOAD_PHASE_TIMING": "1"}):
                with contextlib.redirect_stderr(stream):
                    loud = live_capture(fixture, "loud")["catalog"]["snapshot"]
            self.assertEqual(
                [root["generation_sha256"] for root in quiet["roots"]],
                [root["generation_sha256"] for root in loud["roots"]],
            )
            self.assertEqual(
                quiet["git_generation_sha256"], loud["git_generation_sha256"]
            )
            self.assertEqual(quiet["index_sha256"], loud["index_sha256"])


class EvidenceMemoryGateTests(unittest.TestCase):
    """S6': the release step must not hard-abort where memory is unmeasurable."""

    LARGE = cli.LARGE_PLAN_THRESHOLD_BYTES + 1

    def test_small_evidence_never_probes_memory(self) -> None:
        with mock.patch.object(cli, "_available_memory") as probe:
            cli._require_large_evidence_memory(cli.LARGE_PLAN_THRESHOLD_BYTES)
        probe.assert_not_called()

    def test_unmeasurable_memory_does_not_abort(self) -> None:
        with (
            mock.patch.object(cli, "_proc_meminfo_available", return_value=None),
            mock.patch.object(cli, "_sysconf_available", return_value=None),
        ):
            self.assertIsNone(cli._available_memory())
            cli._require_large_evidence_memory(self.LARGE)

    def test_absent_proc_meminfo_reports_unmeasurable(self) -> None:
        if Path("/proc/meminfo").exists():
            self.skipTest("/proc/meminfo exists on this host")
        self.assertIsNone(cli._proc_meminfo_available())

    def test_a_real_shortfall_still_aborts(self) -> None:
        with mock.patch.object(cli, "_available_memory", return_value=1):
            with self.assertRaisesRegex(
                BulkloadError, "below the bounded Bulkload evidence gate"
            ):
                cli._require_large_evidence_memory(self.LARGE)

    def test_the_multiplier_is_still_four(self) -> None:
        size = self.LARGE
        with mock.patch.object(
            cli, "_available_memory", return_value=size * 4 + 2 * 1024**3
        ):
            cli._require_large_evidence_memory(size)
        with mock.patch.object(
            cli, "_available_memory", return_value=size * 4 + 2 * 1024**3 - 1
        ):
            with self.assertRaises(BulkloadError):
                cli._require_large_evidence_memory(size)

    def test_the_planning_gate_shares_the_portable_probe(self) -> None:
        with mock.patch.object(cli, "_available_memory", return_value=1):
            with self.assertRaisesRegex(
                BulkloadError, "below the bounded AgentPlanV4 planning gate"
            ):
                cli._require_large_evidence_memory(
                    self.LARGE,
                    message=(
                        "destination memory is below the bounded AgentPlanV4 "
                        "planning gate"
                    ),
                )


def flip_byte(path: Path, offset: int = 0) -> None:
    """Change one byte in place without changing the file's size."""
    payload = bytearray(path.read_bytes())
    payload[offset] = payload[offset] ^ 0x01
    mode = path.stat().st_mode
    path.chmod(0o600)
    path.write_bytes(bytes(payload))
    path.chmod(mode & 0o777)


class BaseCustodyTests(unittest.TestCase):
    """S8' (II-S3): --base-custody=sealed, with the X3 git carve-out."""

    @staticmethod
    def base_snapshot(capture: dict) -> dict:
        return capture["catalog"]["snapshot"]

    @staticmethod
    def payload(snapshot: dict, label: str, relative: str) -> Path:
        root = next(item for item in snapshot["roots"] if item["label"] == label)
        return Path(root["snapshot"]) / relative

    def test_sealed_returns_the_identical_record_map(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=True)
            snapshot = self.base_snapshot(live_capture(fixture, "source-a"))
            self.assertEqual(
                scanner.validate_snapshot_custody(snapshot, collect_records=True),
                scanner.validate_snapshot_custody(
                    snapshot, collect_records=True, payload_custody="sealed"
                ),
            )

    def test_sealed_refuses_a_tampered_seal(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            snapshot = self.base_snapshot(live_capture(fixture, "source-a"))
            flip_byte(Path(snapshot["seal_path"]), 8)
            with self.assertRaisesRegex(BulkloadError, "custody seal or index"):
                scanner.validate_snapshot_custody(
                    snapshot, collect_records=True, payload_custody="sealed"
                )

    def test_sealed_refuses_a_tampered_index(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            snapshot = self.base_snapshot(live_capture(fixture, "source-a"))
            index = Path(snapshot["index_path"])
            flip_byte(index, len(index.read_bytes()) // 2)
            with self.assertRaisesRegex(BulkloadError, "custody seal or index"):
                scanner.validate_snapshot_custody(
                    snapshot, collect_records=True, payload_custody="sealed"
                )

    def test_sealed_always_re_verifies_the_git_root(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            snapshot = self.base_snapshot(live_capture(fixture, "source-a"))
            flip_byte(self.payload(snapshot, "git", "repo/tracked.txt"))
            for mode in ("full", "sealed"):
                with self.subTest(mode=mode):
                    with self.assertRaisesRegex(
                        BulkloadError, "snapshot payload differs from sealed index"
                    ):
                        scanner.validate_snapshot_custody(
                            snapshot, collect_records=True, payload_custody=mode
                        )

    def test_sealed_always_re_verifies_sqlite_payloads(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=True)
            snapshot = self.base_snapshot(live_capture(fixture, "source-a"))
            codex = next(
                root for root in snapshot["roots"] if root["label"] == "provider-codex"
            )
            self.assertTrue(codex["sqlite"])
            relative = codex["sqlite"][0]["relative_path"]
            flip_byte(self.payload(snapshot, "provider-codex", relative), 32)
            for mode in ("full", "sealed"):
                with self.subTest(mode=mode):
                    with self.assertRaisesRegex(
                        BulkloadError, "snapshot payload differs from sealed index"
                    ):
                        scanner.validate_snapshot_custody(
                            snapshot, collect_records=True, payload_custody=mode
                        )

    def test_sealed_elides_only_ordinary_provider_payloads(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            snapshot = self.base_snapshot(live_capture(fixture, "source-a"))
            target = self.payload(snapshot, "provider-codex", "history.jsonl")
            flip_byte(target, 2)
            with self.assertRaisesRegex(
                BulkloadError, "snapshot payload differs from sealed index"
            ):
                scanner.validate_snapshot_custody(snapshot, collect_records=True)
            # sealed trades exactly this re-read away, and nothing else: the
            # namespace, count, mode and index digests are all still enforced.
            scanner.validate_snapshot_custody(
                snapshot, collect_records=True, payload_custody="sealed"
            )

    def test_sealed_still_enforces_the_anti_planting_perimeter(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            snapshot = self.base_snapshot(live_capture(fixture, "source-a"))
            planted = Path(snapshot["seal_path"]).parent / "undeclared"
            planted.write_text("planted", encoding="utf-8")
            with self.assertRaisesRegex(BulkloadError, "top-level namespace"):
                scanner.validate_snapshot_custody(
                    snapshot, collect_records=True, payload_custody="sealed"
                )
            planted.unlink()
            extra = self.payload(snapshot, "provider-codex", "planted.json")
            extra.write_text("{}", encoding="utf-8")
            with self.assertRaisesRegex(BulkloadError, "index count or digest"):
                scanner.validate_snapshot_custody(
                    snapshot, collect_records=True, payload_custody="sealed"
                )

    def test_sealed_reads_fewer_payloads_than_full(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            snapshot = self.base_snapshot(live_capture(fixture, "source-a"))

            def count(mode: str) -> list[str]:
                seen: list[str] = []
                real = scanner._snapshot_index_record

                def counted(path, **keywords):
                    seen.append(str(path))
                    return real(path, **keywords)

                with mock.patch.object(
                    scanner, "_snapshot_index_record", side_effect=counted
                ):
                    scanner.validate_snapshot_custody(
                        snapshot, collect_records=True, payload_custody=mode
                    )
                return seen

            full = count("full")
            sealed = count("sealed")
            self.assertLess(len(sealed), len(full))
            git_root = next(
                item["snapshot"] for item in snapshot["roots"] if item["label"] == "git"
            )
            self.assertEqual(
                [path for path in full if path.startswith(git_root)],
                [path for path in sealed if path.startswith(git_root)],
            )

    def test_sealed_cannot_be_combined_with_required_paths(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            snapshot = self.base_snapshot(live_capture(fixture, "source-a"))
            with self.assertRaisesRegex(BulkloadError, "cannot subset required paths"):
                scanner.validate_snapshot_custody(
                    snapshot,
                    required_paths={Path(snapshot["index_path"])},
                    payload_custody="sealed",
                )

    def test_unsupported_custody_mode_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            snapshot = self.base_snapshot(live_capture(fixture, "source-a"))
            with self.assertRaisesRegex(
                BulkloadError, "payload custody mode is unsupported"
            ):
                scanner.validate_snapshot_custody(snapshot, payload_custody="stat")

    def test_chained_capture_under_sealed_matches_full(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            first = live_capture(fixture, "source-a")
            chained_full = live_capture(fixture, "source-b", base=first)
            validate_agent_capture(chained_full, expected_role="source")
            chained_sealed = capture_agent_state(
                role="source",
                home=fixture.source_home,
                git_root=fixture.source_git,
                codex_root=None,
                claude_root=None,
                pi_root=None,
                seats=fixture.source_seats,
                path_map=fixture.path_map,
                writers_quiesced=False,
                snapshot_root=fixture.root / "evidence" / "source-c.snapshot",
                snapshot_base_seal=Path(self.base_snapshot(first)["seal_path"]),
                managed_exclusions=fixture.managed_exclusions,
                rsync_path=fixture.rsync_path,
                max_files=50_000,
                max_bytes=4 * 1024**3,
                max_sqlite_rows=100_000,
                snapshot_reserve_bytes=0,
                base_custody="sealed",
            )
            validate_agent_capture(chained_sealed, expected_role="source")
            full = self.base_snapshot(chained_full)
            sealed = self.base_snapshot(chained_sealed)
            self.assertEqual(full["index_sha256"], sealed["index_sha256"])
            self.assertEqual(
                [root["generation_sha256"] for root in full["roots"]],
                [root["generation_sha256"] for root in sealed["roots"]],
            )
            self.assertIn("base-reflink", sealed["methods"])

    def test_base_custody_requires_a_base_seal(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            with self.assertRaisesRegex(
                BulkloadError, "base custody mode requires a base seal"
            ):
                capture_agent_state(
                    role="source",
                    home=fixture.source_home,
                    git_root=fixture.source_git,
                    codex_root=None,
                    claude_root=None,
                    pi_root=None,
                    seats=fixture.source_seats,
                    path_map=fixture.path_map,
                    writers_quiesced=False,
                    snapshot_root=fixture.root / "evidence" / "orphan.snapshot",
                    managed_exclusions=fixture.managed_exclusions,
                    rsync_path=fixture.rsync_path,
                    snapshot_reserve_bytes=0,
                    base_custody="sealed",
                )

    def test_parser_base_custody_defaults_to_full(self) -> None:
        base = [
            "agent-capture",
            "--role",
            "source",
            "--home",
            "/home",
            "--git-root",
            "/home/git",
            "--rsync-path",
            "/usr/bin/rsync",
            "--path-map",
            "/a=/b",
            "--output",
            "/tmp/out.json",
        ]
        parser = build_parser()
        self.assertEqual(parser.parse_args(base).base_custody, "full")
        self.assertEqual(
            parser.parse_args([*base, "--base-custody", "sealed"]).base_custody,
            "sealed",
        )


class DominatedFenceWiringTests(unittest.TestCase):
    """W0-1: exactly one call site is allowed to drop a pass."""

    def test_push_fences_once_before_the_push_and_at_full_strength_after(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            # The fence only exists on a live (non-quiesced) capture, so this
            # needs the chained live plan, not CutoverFixture.plan().
            source_a = live_capture(fixture, "source-a")
            source_b = live_capture(fixture, "source-b", base=source_a)
            destination_a = live_capture(fixture, "dest-a", role="destination")
            destination_b = live_capture(
                fixture, "dest-b", role="destination", base=destination_a
            )
            plan = compile_agent_plan(source_a, source_b, destination_a, destination_b)
            fake_ssh = fixture.root / "fake-ssh"
            fake_ssh.write_text(FAKE_SSH, encoding="utf-8")
            fake_ssh.chmod(0o700)
            stage = fixture.root / "transport-stage"
            common = {
                "accepted_plan_sha256": plan["plan_sha256"],
                "stage_root": stage,
            }
            preseed_prepare = stage_agent_plan(
                plan,
                phase="preseed",
                allow_accounted_copy=True,
                reserve_bytes=0,
                transport_mode="prepare",
                **common,
            )
            preseed_transport = push_agent_transport(
                preseed_prepare,
                stage / ".transport-allowlist-preseed.nul",
                phase="preseed",
                destination_ssh_host=socket.gethostname(),
                _ssh_binary=str(fake_ssh),
                **common,
            )
            stage_agent_plan(
                plan,
                phase="preseed",
                allow_accounted_copy=True,
                reserve_bytes=0,
                transport_mode="materialize",
                prepare_receipt=preseed_prepare,
                transport_receipt=preseed_transport,
                **common,
            )
            prepare = stage_agent_plan(
                plan,
                phase="final",
                allow_accounted_copy=True,
                reserve_bytes=0,
                transport_mode="prepare",
                **common,
            )

            real = executor.validate_live_snapshot_generation
            recorded: list[int] = []

            def recording(snapshot, *, passes=2):
                recorded.append(passes)
                return real(snapshot, passes=passes)

            with mock.patch.object(
                executor, "validate_live_snapshot_generation", side_effect=recording
            ):
                push_agent_transport(
                    prepare,
                    stage / ".transport-allowlist-final.nul",
                    phase="final",
                    destination_ssh_host=socket.gethostname(),
                    _ssh_binary=str(fake_ssh),
                    **common,
                )
            # Pre-push is dominated and may run one pass; post-push is the
            # last fence before the receipt exists and keeps both.
            self.assertEqual(recorded, [1, 2])


class DefaultOnTelemetryTests(unittest.TestCase):
    """T2: the engine narrates itself, and the counters say what they mean.

    Measured motivation, from the 2026-08-27 ceremony ledger: a 2h32m /
    84 GiB payload push left `logs/preseed-push.log` at 0 bytes, and the one
    telemetry channel that did exist was env-gated off and reported a byte
    total in its `files=` field (`files=67779718491`).
    """

    def setUp(self) -> None:
        # `interval=None` means "keep", so the default has to be restored by
        # name or a fast test interval leaks into the rest of the process.
        self.addCleanup(
            scanner.configure_progress,
            interval=scanner.DEFAULT_HEARTBEAT_SECONDS,
        )

    @staticmethod
    def _fields(line: str) -> dict[str, str]:
        return dict(field.split("=", 1) for field in line.split(" ")[1:])

    def _lines(self, text: str, prefix: str) -> list[dict[str, str]]:
        return [
            self._fields(line)
            for line in text.splitlines()
            if line.startswith(prefix + " ")
        ]

    def test_tree_census_returns_an_entry_count_beside_the_byte_total(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "tree"
            (root / "nested").mkdir(parents=True)
            (root / "a.bin").write_bytes(b"a" * 1000)
            (root / "nested" / "b.bin").write_bytes(b"b" * 2000)
            digest, charged, entries = scanner._tree_census(
                root, provider=None, exclusions=()
            )
            self.assertEqual(len(digest), 64)
            # Bytes are bytes: two payloads only.
            self.assertEqual(charged, 3000)
            # Entries are entries: root, a.bin, nested, nested/b.bin.
            self.assertEqual(entries, 4)

    def test_census_phase_reports_entries_in_files_and_bytes_in_bytes(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            (fixture.source_git / "ballast.bin").write_bytes(b"\0" * 262_144)
            stream = io.StringIO()
            with contextlib.redirect_stderr(stream):
                capture = live_capture(fixture, "source-a")
            validate_agent_capture(capture, expected_role="source")
            census = [
                fields
                for fields in self._lines(stream.getvalue(), "bulkload-phase")
                if fields["phase"] == "census" and fields["root"] == "git"
            ]
            self.assertTrue(census)
            for fields in census:
                self.assertGreaterEqual(int(fields["bytes"]), 262_144)
                # The regression this pins: 67.8 billion "files" in a
                # 1.8M-file corpus was the byte total in the wrong field.
                self.assertLess(int(fields["files"]), int(fields["bytes"]))

    def test_a_long_phase_heartbeats_before_it_exits(self) -> None:
        stream = io.StringIO()
        scanner.configure_progress(stderr=True, interval=0.001)
        with contextlib.redirect_stderr(stream):
            with scanner.phase_timing("fake", "root", total=400, unit="widgets") as it:
                for _ in range(400):
                    it.advance(observed=16)
                    time.sleep(0.0001)
        beats = self._lines(stream.getvalue(), "bulkload-progress")
        self.assertTrue(beats)
        last = beats[-1]
        self.assertEqual(last["phase"], "fake")
        self.assertEqual(last["root"], "root")
        self.assertEqual(last["unit"], "widgets")
        self.assertEqual(last["total"], "400")
        self.assertGreater(int(last["done"]), 0)
        self.assertGreater(int(last["bytes"]), 0)
        float(last["seconds"])
        float(last["rate"])
        exits = self._lines(stream.getvalue(), "bulkload-phase")
        self.assertEqual([item["phase"] for item in exits], ["fake"])
        self.assertEqual(exits[0]["files"], "400")

    def test_a_blocking_phase_heartbeats_from_its_watchdog(self) -> None:
        stream = io.StringIO()
        scanner.configure_progress(stderr=True, interval=0.01)
        with contextlib.redirect_stderr(stream):
            with scanner.phase_timing("push", "final", unit="bytes", watchdog=True):
                # Stands in for the opaque rsync call, which cannot advance a
                # counter of its own from this side.
                time.sleep(0.2)
        beats = self._lines(stream.getvalue(), "bulkload-progress")
        self.assertTrue(beats)
        self.assertEqual({item["phase"] for item in beats}, {"push"})
        self.assertTrue(all(float(item["seconds"]) > 0 for item in beats))

    def test_eta_is_carried_from_the_counters_that_already_exist(self) -> None:
        rate, eta = scanner._rate_fields(50, 10.0, 100)
        self.assertEqual(rate, "5.0")
        self.assertEqual(eta, "10.0")
        self.assertEqual(scanner._rate_fields(0, 10.0, 100), ("-", "-"))
        self.assertEqual(scanner._rate_fields(50, 10.0, None), ("5.0", "-"))

    def test_quiet_suppresses_stderr_but_a_progress_log_still_records(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            log_path = Path(temporary) / "progress.log"
            stream = io.StringIO()
            with log_path.open("a", encoding="utf-8") as log:
                scanner.configure_progress(stderr=False, interval=0.001, log_stream=log)
                with contextlib.redirect_stderr(stream):
                    with scanner.phase_timing("fake", "root") as sample:
                        sample.advance()
            self.assertEqual(stream.getvalue(), "")
            self.assertIn("bulkload-phase phase=fake", log_path.read_text())

    def test_every_verb_announces_itself_to_stderr_and_the_progress_log(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            evidence = fixture.root / "evidence"
            evidence.mkdir(parents=True, exist_ok=True)
            output = evidence / "cli-capture.json"
            log_path = fixture.root / "progress.log"
            stream = io.StringIO()
            with contextlib.redirect_stderr(stream):
                code = cli.main(
                    [
                        "agent-capture",
                        "--role",
                        "source",
                        "--home",
                        str(fixture.source_home),
                        "--git-root",
                        str(fixture.source_git),
                        "--rsync-path",
                        str(fixture.rsync_path),
                        "--path-map",
                        f"{fixture.source_home}={fixture.destination_home}",
                        "--path-map",
                        f"{fixture.source_git}={fixture.destination_git}",
                        "--output",
                        str(output),
                        "--progress-log",
                        str(log_path),
                    ]
                )
            self.assertEqual(code, 0, stream.getvalue())
            for text in (stream.getvalue(), log_path.read_text(encoding="utf-8")):
                runs = self._lines(text, "bulkload-run")
                self.assertEqual(
                    [item["event"] for item in runs], ["start", "end"], text
                )
                self.assertEqual(runs[0]["verb"], "agent-capture")
                self.assertEqual(runs[1]["status"], "ok")
                self.assertEqual(runs[1]["exit"], "0")
                self.assertTrue(self._lines(text, "bulkload-phase"))

    def test_a_refusing_verb_still_closes_its_own_log(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            log_path = root / "progress.log"
            stream = io.StringIO()
            with contextlib.redirect_stderr(stream):
                code = cli.main(
                    [
                        "agent-plan",
                        "--source-a",
                        str(root / "missing-a.json"),
                        "--source-b",
                        str(root / "missing-b.json"),
                        "--destination-a",
                        str(root / "missing-c.json"),
                        "--destination-b",
                        str(root / "missing-d.json"),
                        "--output",
                        str(root / "plan.json"),
                        "--progress-log",
                        str(log_path),
                    ]
                )
            self.assertEqual(code, 1)
            runs = self._lines(log_path.read_text(encoding="utf-8"), "bulkload-run")
            self.assertEqual([item["event"] for item in runs], ["start", "end"])
            self.assertEqual(runs[1]["status"], "fail")
            self.assertEqual(runs[1]["exit"], "1")

    def test_a_progress_log_may_not_land_inside_a_live_root(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            evidence = fixture.root / "evidence"
            evidence.mkdir(parents=True, exist_ok=True)
            stream = io.StringIO()
            with contextlib.redirect_stderr(stream):
                code = cli.main(
                    [
                        "agent-capture",
                        "--role",
                        "source",
                        "--home",
                        str(fixture.source_home),
                        "--git-root",
                        str(fixture.source_git),
                        "--rsync-path",
                        str(fixture.rsync_path),
                        "--path-map",
                        f"{fixture.source_home}={fixture.destination_home}",
                        "--output",
                        str(evidence / "cli-capture.json"),
                        "--progress-log",
                        str(fixture.source_git / "progress.log"),
                    ]
                )
            self.assertEqual(code, 1)
            self.assertIn("progress log overlaps live root", stream.getvalue())
            self.assertFalse((fixture.source_git / "progress.log").exists())

    def test_the_heartbeat_interval_is_bounded(self) -> None:
        for bad in (0.0, -1.0, scanner.MAX_HEARTBEAT_SECONDS + 1):
            with self.subTest(interval=bad):
                with self.assertRaisesRegex(BulkloadError, "out of range"):
                    scanner.configure_progress(interval=bad)

    def test_every_verb_carries_the_progress_flags(self) -> None:
        parser = build_parser()
        verbs: dict[str, argparse.ArgumentParser] = {}
        for action in parser._actions:
            if isinstance(action, argparse._SubParsersAction):
                verbs.update(action.choices)
        self.assertEqual(len(verbs), 7, sorted(verbs))
        for name, subparser in verbs.items():
            options = {
                option
                for action in subparser._actions
                for option in action.option_strings
            }
            with self.subTest(verb=name):
                self.assertLessEqual(
                    {"--quiet", "--progress-log", "--heartbeat-seconds"}, options
                )


if __name__ == "__main__":
    unittest.main()
