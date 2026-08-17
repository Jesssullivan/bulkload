from __future__ import annotations

from contextlib import redirect_stderr
import io
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.dont_write_bytecode = True

import bulkload_lib.cli as bulkload_cli  # noqa: E402
from bulkload_lib.cli import main as cli_main  # noqa: E402
from bulkload_lib.executor import (  # noqa: E402
    apply_plan,
    export_copy_paths,
    verify_plan,
)
from bulkload_lib.model import (  # noqa: E402
    BulkloadError,
    atomic_write,
    atomic_write_json,
    canonical_bytes,
    durable_makedirs,
    normalize_relative,
    object_digest,
    read_json,
    sanitize_remote_url,
    sha256_bytes,
)
from bulkload_lib.planner import (  # noqa: E402
    compile_plan,
    validate_plan,
    validate_snapshot,
)
from bulkload_lib.scanner import (  # noqa: E402
    _path_collisions,
    capture_git_runtime,
    capture_snapshot,
)
import bulkload_lib.sessions as session_catalogs  # noqa: E402
from bulkload_lib.sessions import (  # noqa: E402
    capture_codex_session_close_capture,
    capture_codex_session_prefix_proof,
    capture_codex_sessions,
    compile_codex_session_close_request,
    compile_codex_session_prefix_request,
    compile_codex_session_union_plan,
    validate_codex_session_close_capture,
    validate_codex_session_close_request,
    validate_codex_session_prefix_proof,
    validate_codex_session_prefix_request,
    validate_codex_session_snapshot,
)
from tests.unprivileged_test_main import run_unittest_main  # noqa: E402

TEST_HOST_AUTHORITY_ID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
OTHER_HOST_AUTHORITY_ID = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"


def git(repo: Path, *arguments: str) -> str:
    process = subprocess.run(
        [
            "git",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "-C",
            str(repo),
            *arguments,
        ],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=True,
    )
    return process.stdout.strip()


def make_pair(root: Path) -> tuple[Path, Path]:
    root.mkdir(parents=True, exist_ok=True)
    source = root / "source"
    destination = root / "destination"
    subprocess.run(
        [
            "git",
            "-c",
            "core.hooksPath=/dev/null",
            "init",
            "--template=",
            "-q",
            "-b",
            "main",
            str(source),
        ],
        check=True,
    )
    (source / "tracked.txt").write_text("base\n", encoding="utf-8")
    git(source, "add", "tracked.txt")
    git(
        source,
        "-c",
        "user.name=Bulkload Test",
        "-c",
        "user.email=bulkload@example.invalid",
        "commit",
        "-q",
        "-m",
        "base",
    )
    subprocess.run(
        [
            "git",
            "-c",
            "core.hooksPath=/dev/null",
            "clone",
            "-q",
            str(source),
            str(destination),
        ],
        check=True,
    )
    return source, destination


def write_codex_rollout(
    root: Path,
    session_id: str,
    *,
    day: str = "2026/07/24",
    message: str = "payload",
    mode: int = 0o600,
) -> Path:
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    root.chmod(0o700)
    directory = root / day
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    current = root
    for component in day.split("/"):
        current /= component
        current.chmod(0o700)
    path = directory / f"rollout-2026-07-24T00-00-00-{session_id}.jsonl"
    records = [
        {
            "timestamp": "2026-07-24T00:00:00Z",
            "type": "session_meta",
            "payload": {"id": session_id},
        },
        {
            "timestamp": "2026-07-24T00:00:01Z",
            "type": "event_msg",
            "payload": {"message": message},
        },
    ]
    path.write_text(
        "".join(
            json.dumps(record, separators=(",", ":"), sort_keys=True) + "\n"
            for record in records
        ),
        encoding="utf-8",
    )
    path.chmod(mode)
    return path


def capture_codex(
    root: Path,
    *,
    role: str = "source",
    host_authority_id: str = TEST_HOST_AUTHORITY_ID,
    **budgets: int,
) -> dict:
    return capture_codex_sessions(
        root,
        role=role,
        acknowledge_writers_quiesced=True,
        host_authority_id=host_authority_id,
        **budgets,
    )


def refresh_codex_snapshot(snapshot: dict) -> None:
    catalog = {
        "directories": snapshot["directories"],
        "sessions": snapshot["sessions"],
        "non_private_file_count": snapshot["non_private_file_count"],
        "non_private_directory_count": snapshot["non_private_directory_count"],
    }
    snapshot["catalog_sha256"] = sha256_bytes(canonical_bytes(catalog))
    snapshot["snapshot_sha256"] = object_digest(snapshot, "snapshot_sha256")


def append_codex_event(path: Path, message: str) -> None:
    record = {
        "timestamp": "2026-07-24T00:00:02Z",
        "type": "event_msg",
        "payload": {"message": message},
    }
    with path.open("a", encoding="utf-8") as stream:
        stream.write(json.dumps(record, separators=(",", ":"), sort_keys=True) + "\n")


def refresh_codex_prefix_proof(proof: dict) -> None:
    proof["proofs_sha256"] = sha256_bytes(canonical_bytes(proof["proofs"]))
    proof["proof_sha256"] = object_digest(proof, "proof_sha256")


def refresh_codex_close_capture(close_capture: dict) -> None:
    refresh_codex_snapshot(close_capture["snapshot"])
    close_capture["close_capture_sha256"] = object_digest(
        close_capture,
        "close_capture_sha256",
    )


def capture_codex_closes(
    source: Path,
    destination: Path,
    close_request: dict,
) -> dict[str, dict]:
    return {
        "source_close_a": capture_codex_session_close_capture(
            source,
            role="source",
            close_request=close_request,
            acknowledge_writers_quiesced=True,
        ),
        "source_close_b": capture_codex_session_close_capture(
            source,
            role="source",
            close_request=close_request,
            acknowledge_writers_quiesced=True,
        ),
        "destination_close_a": capture_codex_session_close_capture(
            destination,
            role="destination",
            close_request=close_request,
            acknowledge_writers_quiesced=True,
        ),
        "destination_close_b": capture_codex_session_close_capture(
            destination,
            role="destination",
            close_request=close_request,
            acknowledge_writers_quiesced=True,
        ),
    }


class BulkloadProtocolTest(unittest.TestCase):
    def test_mutation_suite_runs_as_an_unprivileged_user(self) -> None:
        if hasattr(os, "geteuid"):
            self.assertNotEqual(os.geteuid(), 0)

    def test_production_apply_refuses_root_before_mutation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, destination = make_pair(root)
            (source / "notes.txt").write_text("new\n", encoding="utf-8")
            plan = compile_plan(
                capture_snapshot(source, "repo"),
                capture_snapshot(source, "repo"),
                capture_snapshot(destination, "repo"),
            )

            with (
                mock.patch("bulkload_lib.executor.os.geteuid", return_value=0),
                self.assertRaisesRegex(BulkloadError, "apply refuses to run as root"),
            ):
                apply_plan(
                    plan,
                    source_root=source,
                    destination_root=destination,
                    accepted_digest=plan["plan_sha256"],
                    state_root=root / "state",
                    receipt_path=root / "receipt.json",
                )

            self.assertFalse((destination / "notes.txt").exists())
            self.assertFalse((root / "state").exists())
            self.assertFalse((root / "receipt.json").exists())

    def test_codex_session_union_is_stable_absent_only_and_preserving(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source-sessions"
            destination = root / "destination-sessions"
            common = "11111111-1111-4111-8111-111111111111"
            source_only = "22222222-2222-4222-8222-222222222222"
            destination_only = "33333333-3333-4333-8333-333333333333"
            write_codex_rollout(source, common, message="same", mode=0o644)
            write_codex_rollout(source, source_only, message="source", mode=0o644)
            write_codex_rollout(destination, common, message="same")
            write_codex_rollout(destination, destination_only, message="destination")

            source_a = capture_codex(source)
            source_b = capture_codex(source)
            destination_a = capture_codex(destination, role="destination")
            destination_b = capture_codex(destination, role="destination")
            validate_codex_session_snapshot(source_a)
            self.assertTrue(source_a["complete"], source_a["errors"])
            self.assertNotEqual(source_a["capture_id"], source_b["capture_id"])
            self.assertEqual(source_a["catalog_sha256"], source_b["catalog_sha256"])
            self.assertEqual(source_a["non_private_file_count"], 2)

            plan = compile_codex_session_union_plan(
                source_a,
                source_b,
                destination_a,
                destination_b,
            )
            self.assertTrue(plan["intent"]["ready_for_attended_copy"])
            self.assertEqual(
                [item["session_id"] for item in plan["intent"]["copy_if_absent"]],
                [source_only],
            )
            self.assertEqual(
                [item["session_id"] for item in plan["intent"]["exact_common"]],
                [common],
            )
            self.assertEqual(
                [item["session_id"] for item in plan["intent"]["preserve_destination"]],
                [destination_only],
            )
            self.assertEqual(
                plan["intent"]["custody_findings"]["source_non_private_files"],
                2,
            )
            self.assertEqual(plan["intent"]["blockers"], [])

    def test_codex_session_union_blocks_same_uuid_different_bytes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source-sessions"
            destination = root / "destination-sessions"
            session_id = "44444444-4444-4444-8444-444444444444"
            write_codex_rollout(source, session_id, message="source tail")
            write_codex_rollout(destination, session_id, message="destination tail")

            plan = compile_codex_session_union_plan(
                capture_codex(source),
                capture_codex(source),
                capture_codex(destination, role="destination"),
                capture_codex(destination, role="destination"),
            )
            self.assertFalse(plan["intent"]["ready_for_attended_copy"])
            self.assertEqual(
                [item["code"] for item in plan["intent"]["blockers"]],
                ["same-uuid-prefix-proof-required"],
            )
            self.assertEqual(plan["intent"]["copy_if_absent"], [])

    def test_codex_session_union_promotes_only_repeated_source_prefix(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source-sessions"
            destination = root / "destination-sessions"
            session_id = "45454545-4545-4545-8545-454545454545"
            source_path = write_codex_rollout(source, session_id, message="base")
            write_codex_rollout(destination, session_id, message="base")
            append_codex_event(source_path, "source continuation")

            source_a = capture_codex(source)
            source_b = capture_codex(source)
            destination_a = capture_codex(destination, role="destination")
            destination_b = capture_codex(destination, role="destination")
            request = compile_codex_session_prefix_request(
                source_a,
                source_b,
                destination_a,
                destination_b,
            )
            validate_codex_session_prefix_request(request)
            self.assertEqual(
                [
                    (item["session_id"], item["longer_role"])
                    for item in request["requests"]
                ],
                [(session_id, "source")],
            )
            proof_a = capture_codex_session_prefix_proof(
                source,
                role="source",
                prefix_request=request,
                source_a=source_a,
                source_b=source_b,
                destination_a=destination_a,
                destination_b=destination_b,
                acknowledge_writers_quiesced=True,
            )
            proof_b = capture_codex_session_prefix_proof(
                source,
                role="source",
                prefix_request=request,
                source_a=source_a,
                source_b=source_b,
                destination_a=destination_a,
                destination_b=destination_b,
                acknowledge_writers_quiesced=True,
            )
            validate_codex_session_prefix_proof(proof_a)
            self.assertNotEqual(proof_a["capture_id"], proof_b["capture_id"])
            close_request = compile_codex_session_close_request(
                request,
                source_a,
                source_b,
                destination_a,
                destination_b,
                source_prefix_a=proof_a,
                source_prefix_b=proof_b,
            )
            validate_codex_session_close_request(close_request)
            closes = capture_codex_closes(source, destination, close_request)

            plan = compile_codex_session_union_plan(
                source_a,
                source_b,
                destination_a,
                destination_b,
                prefix_request=request,
                source_prefix_a=proof_a,
                source_prefix_b=proof_b,
                close_request=close_request,
                **closes,
            )

            self.assertTrue(plan["intent"]["ready_for_attended_copy"])
            self.assertEqual(plan["intent"]["blockers"], [])
            self.assertNotIn("close_request_sha256", source_a)
            self.assertEqual(
                source_a["schema"],
                "dev.tinyland.bulkload.codex-sessions.v2",
            )
            self.assertEqual(
                plan["prefix_evidence"]["close_request_sha256"],
                close_request["close_request_sha256"],
            )
            self.assertEqual(
                [
                    item["session_id"]
                    for item in plan["intent"]["promote_source_superset"]
                ],
                [session_id],
            )
            self.assertEqual(
                plan["prefix_evidence"]["source_prefix_proofs"],
                [
                    {
                        "capture_id": proof_a["capture_id"],
                        "proof_sha256": proof_a["proof_sha256"],
                    },
                    {
                        "capture_id": proof_b["capture_id"],
                        "proof_sha256": proof_b["proof_sha256"],
                    },
                ],
            )
            self.assertEqual(
                plan["prefix_evidence"]["source_close_snapshots"],
                [
                    {
                        "capture_id": closes["source_close_a"]["snapshot"][
                            "capture_id"
                        ],
                        "snapshot_sha256": closes["source_close_a"]["snapshot"][
                            "snapshot_sha256"
                        ],
                        "close_capture_sha256": closes["source_close_a"][
                            "close_capture_sha256"
                        ],
                    },
                    {
                        "capture_id": closes["source_close_b"]["snapshot"][
                            "capture_id"
                        ],
                        "snapshot_sha256": closes["source_close_b"]["snapshot"][
                            "snapshot_sha256"
                        ],
                        "close_capture_sha256": closes["source_close_b"][
                            "close_capture_sha256"
                        ],
                    },
                ],
            )

    def test_codex_session_union_preserves_destination_prefix_superset(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source-sessions"
            destination = root / "destination-sessions"
            session_id = "46464646-4646-4646-8646-464646464646"
            write_codex_rollout(source, session_id, message="base")
            destination_path = write_codex_rollout(
                destination,
                session_id,
                message="base",
            )
            append_codex_event(destination_path, "destination continuation")

            source_a = capture_codex(source)
            source_b = capture_codex(source)
            destination_a = capture_codex(destination, role="destination")
            destination_b = capture_codex(destination, role="destination")
            request = compile_codex_session_prefix_request(
                source_a,
                source_b,
                destination_a,
                destination_b,
            )
            proof_a = capture_codex_session_prefix_proof(
                destination,
                role="destination",
                prefix_request=request,
                source_a=source_a,
                source_b=source_b,
                destination_a=destination_a,
                destination_b=destination_b,
                acknowledge_writers_quiesced=True,
            )
            proof_b = capture_codex_session_prefix_proof(
                destination,
                role="destination",
                prefix_request=request,
                source_a=source_a,
                source_b=source_b,
                destination_a=destination_a,
                destination_b=destination_b,
                acknowledge_writers_quiesced=True,
            )
            close_request = compile_codex_session_close_request(
                request,
                source_a,
                source_b,
                destination_a,
                destination_b,
                destination_prefix_a=proof_a,
                destination_prefix_b=proof_b,
            )
            closes = capture_codex_closes(source, destination, close_request)
            plan = compile_codex_session_union_plan(
                source_a,
                source_b,
                destination_a,
                destination_b,
                prefix_request=request,
                destination_prefix_a=proof_a,
                destination_prefix_b=proof_b,
                close_request=close_request,
                **closes,
            )

            self.assertTrue(plan["intent"]["ready_for_attended_copy"])
            self.assertEqual(plan["intent"]["promote_source_superset"], [])
            self.assertEqual(
                [
                    item["session_id"]
                    for item in plan["intent"]["preserve_destination_superset"]
                ],
                [session_id],
            )
            self.assertEqual(
                plan["prefix_evidence"]["destination_prefix_proofs"],
                [
                    {
                        "capture_id": proof_a["capture_id"],
                        "proof_sha256": proof_a["proof_sha256"],
                    },
                    {
                        "capture_id": proof_b["capture_id"],
                        "proof_sha256": proof_b["proof_sha256"],
                    },
                ],
            )

    def test_codex_session_union_blocks_missing_or_divergent_prefix_proof(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source-sessions"
            destination = root / "destination-sessions"
            session_id = "47474747-4747-4747-8747-474747474747"
            source_path = write_codex_rollout(source, session_id, message="base")
            write_codex_rollout(destination, session_id, message="baSe")
            append_codex_event(source_path, "source continuation")

            source_a = capture_codex(source)
            source_b = capture_codex(source)
            destination_a = capture_codex(destination, role="destination")
            destination_b = capture_codex(destination, role="destination")
            request = compile_codex_session_prefix_request(
                source_a,
                source_b,
                destination_a,
                destination_b,
            )
            proof_a = capture_codex_session_prefix_proof(
                source,
                role="source",
                prefix_request=request,
                source_a=source_a,
                source_b=source_b,
                destination_a=destination_a,
                destination_b=destination_b,
                acknowledge_writers_quiesced=True,
            )
            proof_b = capture_codex_session_prefix_proof(
                source,
                role="source",
                prefix_request=request,
                source_a=source_a,
                source_b=source_b,
                destination_a=destination_a,
                destination_b=destination_b,
                acknowledge_writers_quiesced=True,
            )
            close_request = compile_codex_session_close_request(
                request,
                source_a,
                source_b,
                destination_a,
                destination_b,
                source_prefix_a=proof_a,
                source_prefix_b=proof_b,
            )
            closes = capture_codex_closes(source, destination, close_request)
            with self.assertRaisesRegex(
                BulkloadError,
                "source prefix proof requires pass A and pass B",
            ):
                compile_codex_session_union_plan(
                    source_a,
                    source_b,
                    destination_a,
                    destination_b,
                    prefix_request=request,
                    close_request=close_request,
                    **closes,
                )
            divergent = compile_codex_session_union_plan(
                source_a,
                source_b,
                destination_a,
                destination_b,
                prefix_request=request,
                source_prefix_a=proof_a,
                source_prefix_b=proof_b,
                close_request=close_request,
                **closes,
            )
            self.assertEqual(
                [item["code"] for item in divergent["intent"]["blockers"]],
                ["same-uuid-divergent-bytes"],
            )
            self.assertEqual(
                divergent["intent"]["promote_source_superset"],
                [],
            )

    def test_codex_session_union_rejects_prefix_pass_drift(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source-sessions"
            destination = root / "destination-sessions"
            session_id = "48484848-4848-4848-8848-484848484848"
            source_path = write_codex_rollout(source, session_id, message="base")
            write_codex_rollout(destination, session_id, message="base")
            append_codex_event(source_path, "source continuation")
            source_a = capture_codex(source)
            source_b = capture_codex(source)
            destination_a = capture_codex(destination, role="destination")
            destination_b = capture_codex(destination, role="destination")
            request = compile_codex_session_prefix_request(
                source_a,
                source_b,
                destination_a,
                destination_b,
            )
            proof_a = capture_codex_session_prefix_proof(
                source,
                role="source",
                prefix_request=request,
                source_a=source_a,
                source_b=source_b,
                destination_a=destination_a,
                destination_b=destination_b,
                acknowledge_writers_quiesced=True,
            )
            proof_b = capture_codex_session_prefix_proof(
                source,
                role="source",
                prefix_request=request,
                source_a=source_a,
                source_b=source_b,
                destination_a=destination_a,
                destination_b=destination_b,
                acknowledge_writers_quiesced=True,
            )
            close_request = compile_codex_session_close_request(
                request,
                source_a,
                source_b,
                destination_a,
                destination_b,
                source_prefix_a=proof_a,
                source_prefix_b=proof_b,
            )
            closes = capture_codex_closes(source, destination, close_request)
            proof_b["proofs"][0]["prefix_ends_at_record"] = False
            refresh_codex_prefix_proof(proof_b)

            with self.assertRaisesRegex(BulkloadError, "pass A and pass B differ"):
                compile_codex_session_union_plan(
                    source_a,
                    source_b,
                    destination_a,
                    destination_b,
                    prefix_request=request,
                    source_prefix_a=proof_a,
                    source_prefix_b=proof_b,
                    close_request=close_request,
                    **closes,
                )

    def test_codex_prefix_plan_rejects_stale_source_or_destination_close(
        self,
    ) -> None:
        for stale_role in ("source", "destination"):
            with (
                self.subTest(stale_role=stale_role),
                tempfile.TemporaryDirectory() as directory,
            ):
                root = Path(directory)
                source = root / "source-sessions"
                destination = root / "destination-sessions"
                session_id = "49494949-4949-4949-8949-494949494949"
                source_path = write_codex_rollout(source, session_id, message="base")
                destination_path = write_codex_rollout(
                    destination,
                    session_id,
                    message="base",
                )
                append_codex_event(source_path, "source continuation")
                source_a = capture_codex(source)
                source_b = capture_codex(source)
                destination_a = capture_codex(destination, role="destination")
                destination_b = capture_codex(destination, role="destination")
                request = compile_codex_session_prefix_request(
                    source_a,
                    source_b,
                    destination_a,
                    destination_b,
                )
                proof_a = capture_codex_session_prefix_proof(
                    source,
                    role="source",
                    prefix_request=request,
                    source_a=source_a,
                    source_b=source_b,
                    destination_a=destination_a,
                    destination_b=destination_b,
                    acknowledge_writers_quiesced=True,
                )
                proof_b = capture_codex_session_prefix_proof(
                    source,
                    role="source",
                    prefix_request=request,
                    source_a=source_a,
                    source_b=source_b,
                    destination_a=destination_a,
                    destination_b=destination_b,
                    acknowledge_writers_quiesced=True,
                )
                close_request = compile_codex_session_close_request(
                    request,
                    source_a,
                    source_b,
                    destination_a,
                    destination_b,
                    source_prefix_a=proof_a,
                    source_prefix_b=proof_b,
                )
                append_codex_event(
                    source_path if stale_role == "source" else destination_path,
                    f"{stale_role} changed after proof",
                )

                with self.assertRaisesRegex(
                    BulkloadError,
                    rf"{stale_role} close capture differs from requested custody",
                ):
                    capture_codex_session_close_capture(
                        source if stale_role == "source" else destination,
                        role=stale_role,
                        close_request=close_request,
                        acknowledge_writers_quiesced=True,
                    )

    def test_codex_prefix_plan_rejects_preproof_stale_close_snapshots(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source-sessions"
            destination = root / "destination-sessions"
            session_id = "49a949a9-49a9-49a9-89a9-49a949a949a9"
            source_path = write_codex_rollout(source, session_id, message="base")
            destination_path = write_codex_rollout(
                destination,
                session_id,
                message="base",
            )
            append_codex_event(source_path, "source continuation")
            source_a = capture_codex(source)
            source_b = capture_codex(source)
            destination_a = capture_codex(destination, role="destination")
            destination_b = capture_codex(destination, role="destination")
            request = compile_codex_session_prefix_request(
                source_a,
                source_b,
                destination_a,
                destination_b,
            )

            stale_closes = {
                "source_close_a": capture_codex(source),
                "source_close_b": capture_codex(source),
                "destination_close_a": capture_codex(
                    destination,
                    role="destination",
                ),
                "destination_close_b": capture_codex(
                    destination,
                    role="destination",
                ),
            }
            append_codex_event(destination_path, "destination changed")
            proof_a = capture_codex_session_prefix_proof(
                source,
                role="source",
                prefix_request=request,
                source_a=source_a,
                source_b=source_b,
                destination_a=destination_a,
                destination_b=destination_b,
                acknowledge_writers_quiesced=True,
            )
            proof_b = capture_codex_session_prefix_proof(
                source,
                role="source",
                prefix_request=request,
                source_a=source_a,
                source_b=source_b,
                destination_a=destination_a,
                destination_b=destination_b,
                acknowledge_writers_quiesced=True,
            )
            close_request = compile_codex_session_close_request(
                request,
                source_a,
                source_b,
                destination_a,
                destination_b,
                source_prefix_a=proof_a,
                source_prefix_b=proof_b,
            )

            with self.assertRaisesRegex(
                BulkloadError,
                "Codex session close capture has unexpected fields",
            ):
                compile_codex_session_union_plan(
                    source_a,
                    source_b,
                    destination_a,
                    destination_b,
                    prefix_request=request,
                    source_prefix_a=proof_a,
                    source_prefix_b=proof_b,
                    close_request=close_request,
                    **stale_closes,
                )

    def test_codex_prefix_plan_requires_fresh_distinct_close_pairs(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source-sessions"
            destination = root / "destination-sessions"
            session_id = "4a4a4a4a-4a4a-4a4a-8a4a-4a4a4a4a4a4a"
            source_path = write_codex_rollout(source, session_id, message="base")
            write_codex_rollout(destination, session_id, message="base")
            append_codex_event(source_path, "source continuation")
            source_a = capture_codex(source)
            source_b = capture_codex(source)
            destination_a = capture_codex(destination, role="destination")
            destination_b = capture_codex(destination, role="destination")
            request = compile_codex_session_prefix_request(
                source_a,
                source_b,
                destination_a,
                destination_b,
            )
            proof_a = capture_codex_session_prefix_proof(
                source,
                role="source",
                prefix_request=request,
                source_a=source_a,
                source_b=source_b,
                destination_a=destination_a,
                destination_b=destination_b,
                acknowledge_writers_quiesced=True,
            )
            proof_b = capture_codex_session_prefix_proof(
                source,
                role="source",
                prefix_request=request,
                source_a=source_a,
                source_b=source_b,
                destination_a=destination_a,
                destination_b=destination_b,
                acknowledge_writers_quiesced=True,
            )
            with self.assertRaisesRegex(BulkloadError, "close pass A and pass B"):
                compile_codex_session_union_plan(
                    source_a,
                    source_b,
                    destination_a,
                    destination_b,
                    prefix_request=request,
                    source_prefix_a=proof_a,
                    source_prefix_b=proof_b,
                )

            with self.assertRaisesRegex(
                BulkloadError,
                "source prefix proof requires pass A and pass B",
            ):
                compile_codex_session_close_request(
                    request,
                    source_a,
                    source_b,
                    destination_a,
                    destination_b,
                    source_prefix_a=proof_a,
                )

            close_request = compile_codex_session_close_request(
                request,
                source_a,
                source_b,
                destination_a,
                destination_b,
                source_prefix_a=proof_a,
                source_prefix_b=proof_b,
            )
            closes = capture_codex_closes(source, destination, close_request)
            reused_source = json.loads(json.dumps(closes["source_close_a"]))
            reused_source["snapshot"]["capture_id"] = source_a["capture_id"]
            refresh_codex_close_capture(reused_source)
            reused = {
                **closes,
                "source_close_a": reused_source,
            }
            with self.assertRaisesRegex(BulkloadError, "globally distinct"):
                compile_codex_session_union_plan(
                    source_a,
                    source_b,
                    destination_a,
                    destination_b,
                    prefix_request=request,
                    source_prefix_a=proof_a,
                    source_prefix_b=proof_b,
                    close_request=close_request,
                    **reused,
                )

    def test_codex_prefix_plan_rejects_replayed_close_request(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source-sessions"
            destination = root / "destination-sessions"
            session_id = "4b4b4b4b-4b4b-4b4b-8b4b-4b4b4b4b4b4b"
            source_path = write_codex_rollout(source, session_id, message="base")
            write_codex_rollout(destination, session_id, message="base")
            append_codex_event(source_path, "source continuation")
            source_a = capture_codex(source)
            source_b = capture_codex(source)
            destination_a = capture_codex(destination, role="destination")
            destination_b = capture_codex(destination, role="destination")
            request = compile_codex_session_prefix_request(
                source_a,
                source_b,
                destination_a,
                destination_b,
            )
            proof_a = capture_codex_session_prefix_proof(
                source,
                role="source",
                prefix_request=request,
                source_a=source_a,
                source_b=source_b,
                destination_a=destination_a,
                destination_b=destination_b,
                acknowledge_writers_quiesced=True,
            )
            proof_b = capture_codex_session_prefix_proof(
                source,
                role="source",
                prefix_request=request,
                source_a=source_a,
                source_b=source_b,
                destination_a=destination_a,
                destination_b=destination_b,
                acknowledge_writers_quiesced=True,
            )
            close_request = compile_codex_session_close_request(
                request,
                source_a,
                source_b,
                destination_a,
                destination_b,
                source_prefix_a=proof_a,
                source_prefix_b=proof_b,
            )
            closes = capture_codex_closes(source, destination, close_request)
            replayed_request = json.loads(json.dumps(close_request))
            replayed_request["created_at"] = "2026-07-28T23:59:59Z"
            replayed_request["close_request_sha256"] = object_digest(
                replayed_request,
                "close_request_sha256",
            )
            validate_codex_session_close_request(replayed_request)

            with self.assertRaisesRegex(
                BulkloadError,
                "source close capture binding is invalid",
            ):
                compile_codex_session_union_plan(
                    source_a,
                    source_b,
                    destination_a,
                    destination_b,
                    prefix_request=request,
                    source_prefix_a=proof_a,
                    source_prefix_b=proof_b,
                    close_request=replayed_request,
                    **closes,
                )

    def test_codex_session_capture_fails_closed_on_unknown_or_moving_state(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "sessions"
            session_id = "55555555-5555-4555-8555-555555555555"
            write_codex_rollout(root, session_id)
            (root / "unexpected.db").write_bytes(b"not portable")
            (root / "unexpected.db").chmod(0o600)
            incomplete = capture_codex(root)
            self.assertFalse(incomplete["complete"])
            self.assertTrue(
                any(
                    "unexpected regular file" in error for error in incomplete["errors"]
                )
            )

            (root / "unexpected.db").unlink()
            bounded = capture_codex(root, max_record_bytes=8)
            self.assertFalse(bounded["complete"])
            self.assertTrue(
                any(
                    "record byte budget exceeded" in error
                    for error in bounded["errors"]
                )
            )
            write_codex_rollout(root, session_id, mode=0o620)
            writable = capture_codex(root)
            self.assertFalse(writable["complete"])
            self.assertTrue(
                any("non-writable-by-others" in error for error in writable["errors"])
            )
            write_codex_rollout(root, session_id)
            first = capture_codex(root)
            write_codex_rollout(root, session_id, message="changed")
            second = capture_codex(root)
            destination = Path(directory) / "destination"
            write_codex_rollout(
                destination,
                "66666666-6666-4666-8666-666666666666",
            )
            with self.assertRaisesRegex(BulkloadError, "pass A and pass B differ"):
                compile_codex_session_union_plan(
                    first,
                    second,
                    capture_codex(destination, role="destination"),
                    capture_codex(destination, role="destination"),
                )

    def test_codex_capture_rejects_privileged_modes_and_self_validates(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            privileged_file = os.stat_result(
                (
                    stat.S_IFREG | 0o4600,
                    1,
                    1,
                    1,
                    os.getuid(),
                    os.getgid(),
                    0,
                    0,
                    0,
                    0,
                )
            )
            with self.assertRaisesRegex(BulkloadError, "privileged permission bits"):
                session_catalogs._stable_user_regular(
                    privileged_file,
                    root / "rollout.jsonl",
                    role="source",
                )

            privileged_directory = os.stat_result(
                (
                    stat.S_IFDIR | 0o1700,
                    2,
                    1,
                    1,
                    os.getuid(),
                    os.getgid(),
                    0,
                    0,
                    0,
                    0,
                )
            )
            with self.assertRaisesRegex(BulkloadError, "privileged permission bits"):
                session_catalogs._validate_stable_directory(
                    root / "2026",
                    privileged_directory,
                    role="source",
                )

            public_source_root = root / "public-source"
            write_codex_rollout(
                public_source_root,
                "74747474-7474-4474-8474-747474747474",
            )
            public_source_root.chmod(0o755)
            public_source_capture = capture_codex(public_source_root)
            self.assertTrue(public_source_capture["complete"])
            self.assertEqual(public_source_capture["non_private_directory_count"], 0)
            validate_codex_session_snapshot(public_source_capture)
            for field in (
                "non_private_file_count",
                "non_private_directory_count",
            ):
                forged_count = dict(public_source_capture)
                forged_count[field] = False
                refresh_codex_snapshot(forged_count)
                with self.assertRaisesRegex(BulkloadError, "count mismatch"):
                    validate_codex_session_snapshot(forged_count)

            empty_source_root = root / "empty-source"
            empty_source_root.mkdir(mode=0o700)
            empty_source_capture = capture_codex(empty_source_root)
            self.assertTrue(empty_source_capture["complete"])
            self.assertEqual(empty_source_capture["total_bytes"], 0)
            empty_source_capture["total_bytes"] = False
            refresh_codex_snapshot(empty_source_capture)
            with self.assertRaisesRegex(BulkloadError, "total_bytes mismatch"):
                validate_codex_session_snapshot(empty_source_capture)

            failed_traversal_root = root / "failed-traversal"
            write_codex_rollout(
                failed_traversal_root,
                "78787878-7878-4878-8878-787878787878",
            )
            (failed_traversal_root / "2026").chmod(0o755)
            real_scandir = session_catalogs.os.scandir
            scandir_calls = 0

            def fail_second_scandir(descriptor: int):
                nonlocal scandir_calls
                scandir_calls += 1
                if scandir_calls == 2:
                    raise OSError("fixture child enumeration failure")
                return real_scandir(descriptor)

            with mock.patch(
                "bulkload_lib.sessions.os.scandir",
                side_effect=fail_second_scandir,
            ):
                failed_traversal = capture_codex(failed_traversal_root)
            self.assertFalse(failed_traversal["complete"])
            self.assertEqual(failed_traversal["directories"], [])
            self.assertEqual(failed_traversal["non_private_directory_count"], 0)
            validate_codex_session_snapshot(failed_traversal)

            valid_root = root / "valid"
            write_codex_rollout(
                valid_root,
                "76767676-7676-4676-8676-767676767676",
            )
            invalid_capture = capture_codex(valid_root)
            invalid_capture["directories"][0]["mode"] = "1700"
            output = root / "invalid-capture.json"
            stderr = io.StringIO()
            with (
                redirect_stderr(stderr),
                mock.patch(
                    "bulkload_lib.cli.capture_codex_sessions",
                    return_value=invalid_capture,
                ),
                mock.patch("sys.stdout", new=io.StringIO()),
            ):
                result = cli_main(
                    [
                        "codex-capture",
                        "--root",
                        str(valid_root),
                        "--output",
                        str(output),
                        "--role",
                        "source",
                        "--acknowledge-writers-quiesced",
                        "--host-authority-id",
                        TEST_HOST_AUTHORITY_ID,
                    ]
                )
            self.assertEqual(result, 2)
            self.assertFalse(output.exists())
            self.assertIn("directory record is invalid", stderr.getvalue())

    def test_codex_evidence_stdout_publication_is_forbidden(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sessions = root / "sessions"
            write_codex_rollout(
                sessions,
                "75757575-7575-4575-8575-757575757575",
            )
            stdout = io.StringIO()
            stderr = io.StringIO()
            with redirect_stderr(stderr), mock.patch("sys.stdout", new=stdout):
                result = cli_main(
                    [
                        "codex-capture",
                        "--root",
                        str(sessions),
                        "--output",
                        "-",
                        "--role",
                        "source",
                        "--acknowledge-writers-quiesced",
                        "--host-authority-id",
                        TEST_HOST_AUTHORITY_ID,
                    ]
                )
            self.assertEqual(result, 2)
            self.assertEqual(stdout.getvalue(), "")
            self.assertIn("stdout publication is forbidden", stderr.getvalue())

    def test_darwin_codex_publication_requires_atomic_rename_symbol(self) -> None:
        with (
            mock.patch.object(bulkload_cli.sys, "platform", "darwin"),
            mock.patch.object(bulkload_cli.ctypes, "CDLL", return_value=object()),
            self.assertRaisesRegex(
                BulkloadError,
                "atomic no-replace evidence publication is unavailable",
            ),
        ):
            bulkload_cli._rename_codex_output_noreplace(3, "source", 4, "target")

    def test_codex_capture_cli_forwards_record_budget(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sessions = root / "sessions"
            output = root / "capture.json"
            write_codex_rollout(
                sessions,
                "77777777-7777-4777-8777-777777777777",
            )

            with mock.patch("sys.stdout", new=io.StringIO()):
                result = cli_main(
                    [
                        "codex-capture",
                        "--root",
                        str(sessions),
                        "--output",
                        str(output),
                        "--role",
                        "source",
                        "--acknowledge-writers-quiesced",
                        "--host-authority-id",
                        TEST_HOST_AUTHORITY_ID,
                        "--max-record-bytes",
                        "8",
                    ]
                )

            self.assertEqual(result, 3)
            snapshot = json.loads(output.read_text(encoding="utf-8"))
            self.assertFalse(snapshot["complete"])
            self.assertTrue(
                any(
                    "record byte budget exceeded" in error
                    for error in snapshot["errors"]
                )
            )

    def test_codex_evidence_output_parent_rejects_any_group_or_world_bits(
        self,
    ) -> None:
        for mode in (0o750, 0o705, 0o744):
            with (
                self.subTest(mode=f"{mode:04o}"),
                tempfile.TemporaryDirectory() as directory,
            ):
                root = Path(directory)
                sessions = root / "sessions"
                evidence = root / "evidence"
                evidence.mkdir(mode=0o700)
                evidence.chmod(mode)
                write_codex_rollout(
                    sessions,
                    "76767676-7676-4676-8676-767676767676",
                )
                output = evidence / "capture.json"
                stderr = io.StringIO()

                with (
                    redirect_stderr(stderr),
                    mock.patch("sys.stdout", new=io.StringIO()),
                ):
                    result = cli_main(
                        [
                            "codex-capture",
                            "--root",
                            str(sessions),
                            "--output",
                            str(output),
                            "--role",
                            "source",
                            "--acknowledge-writers-quiesced",
                            "--host-authority-id",
                            TEST_HOST_AUTHORITY_ID,
                        ]
                    )

                self.assertEqual(result, 2)
                self.assertFalse(output.exists())
                self.assertIn("not owner-private", stderr.getvalue())

    def test_codex_capture_never_creates_evidence_inside_failed_root(self) -> None:
        for case in ("missing", "symlink", "invalid-custody"):
            with self.subTest(case=case), tempfile.TemporaryDirectory() as directory:
                parent = Path(directory)
                sessions = parent / "sessions"
                if case == "symlink":
                    actual = parent / "actual"
                    actual.mkdir(mode=0o700)
                    sessions.symlink_to(actual, target_is_directory=True)
                elif case == "invalid-custody":
                    sessions.mkdir(mode=0o700)
                    sessions.chmod(0o733)
                output = sessions / "capture.json"

                with (
                    redirect_stderr(io.StringIO()),
                    mock.patch("sys.stdout", new=io.StringIO()),
                ):
                    result = cli_main(
                        [
                            "codex-capture",
                            "--root",
                            str(sessions),
                            "--output",
                            str(output),
                            "--role",
                            "source",
                            "--acknowledge-writers-quiesced",
                            "--host-authority-id",
                            TEST_HOST_AUTHORITY_ID,
                        ]
                    )

                self.assertEqual(result, 2)
                self.assertFalse(output.exists())
                if case == "missing":
                    self.assertFalse(sessions.exists())
                elif case == "symlink":
                    self.assertFalse((actual / "capture.json").exists())
                    self.assertTrue(sessions.is_symlink())

    def test_codex_plan_cli_requires_both_destination_captures(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source-sessions"
            destination = root / "destination-sessions"
            evidence = root / "evidence"
            evidence.mkdir(mode=0o700)
            write_codex_rollout(
                source,
                "77887788-7788-4788-8788-778877887788",
            )
            write_codex_rollout(
                destination,
                "77997799-7799-4799-8799-779977997799",
            )
            artifacts = {
                "source-a": capture_codex(source),
                "source-b": capture_codex(source),
                "destination-a": capture_codex(
                    destination,
                    role="destination",
                ),
                "destination-b": capture_codex(
                    destination,
                    role="destination",
                ),
            }
            for name, snapshot in artifacts.items():
                atomic_write_json(evidence / f"{name}.json", snapshot)
            output = evidence / "plan.json"

            with mock.patch("sys.stdout", new=io.StringIO()):
                result = cli_main(
                    [
                        "codex-plan",
                        "--source-a",
                        str(evidence / "source-a.json"),
                        "--source-b",
                        str(evidence / "source-b.json"),
                        "--destination-a",
                        str(evidence / "destination-a.json"),
                        "--destination-b",
                        str(evidence / "destination-b.json"),
                        "--output",
                        str(output),
                    ]
                )

            self.assertEqual(result, 0)
            plan = read_json(output)
            self.assertEqual(
                plan["destination"]["capture_ids"],
                [
                    artifacts["destination-a"]["capture_id"],
                    artifacts["destination-b"]["capture_id"],
                ],
            )

    def test_codex_prefix_cli_compiles_repeated_proof_into_plan_v3(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source-sessions"
            destination = root / "destination-sessions"
            evidence = root / "evidence"
            evidence.mkdir(mode=0o700)
            session_id = "77a177a1-77a1-47a1-87a1-77a177a177a1"
            source_path = write_codex_rollout(source, session_id, message="base")
            write_codex_rollout(destination, session_id, message="base")
            append_codex_event(source_path, "source continuation")
            artifacts = {
                "source-a": capture_codex(source),
                "source-b": capture_codex(source),
                "destination-a": capture_codex(
                    destination,
                    role="destination",
                ),
                "destination-b": capture_codex(
                    destination,
                    role="destination",
                ),
            }
            for name, snapshot in artifacts.items():
                atomic_write_json(evidence / f"{name}.json", snapshot)
            common = [
                "--source-a",
                str(evidence / "source-a.json"),
                "--source-b",
                str(evidence / "source-b.json"),
                "--destination-a",
                str(evidence / "destination-a.json"),
                "--destination-b",
                str(evidence / "destination-b.json"),
            ]
            request_path = evidence / "request.json"
            with mock.patch("sys.stdout", new=io.StringIO()):
                request_result = cli_main(
                    [
                        "codex-prefix-request",
                        *common,
                        "--output",
                        str(request_path),
                    ]
                )
            self.assertEqual(request_result, 0)

            proof_paths = [evidence / "proof-a.json", evidence / "proof-b.json"]
            for proof_path in proof_paths:
                with mock.patch("sys.stdout", new=io.StringIO()):
                    proof_result = cli_main(
                        [
                            "codex-prefix-proof",
                            "--prefix-request",
                            str(request_path),
                            *common,
                            "--root",
                            str(source),
                            "--role",
                            "source",
                            "--acknowledge-writers-quiesced",
                            "--output",
                            str(proof_path),
                        ]
                    )
                self.assertEqual(proof_result, 0)

            close_request_path = evidence / "close-request.json"
            with mock.patch("sys.stdout", new=io.StringIO()):
                close_request_result = cli_main(
                    [
                        "codex-close-request",
                        "--prefix-request",
                        str(request_path),
                        *common,
                        "--source-prefix-a",
                        str(proof_paths[0]),
                        "--source-prefix-b",
                        str(proof_paths[1]),
                        "--output",
                        str(close_request_path),
                    ]
                )
            self.assertEqual(close_request_result, 0)
            close_request = read_json(close_request_path)
            validate_codex_session_close_request(close_request)

            missing_close_plan = evidence / "missing-close-plan.json"
            with (
                redirect_stderr(io.StringIO()),
                mock.patch("sys.stdout", new=io.StringIO()),
            ):
                missing_close_result = cli_main(
                    [
                        "codex-plan",
                        *common,
                        "--prefix-request",
                        str(request_path),
                        "--close-request",
                        str(close_request_path),
                        "--source-prefix-a",
                        str(proof_paths[0]),
                        "--source-prefix-b",
                        str(proof_paths[1]),
                        "--output",
                        str(missing_close_plan),
                    ]
                )
            self.assertEqual(missing_close_result, 2)
            self.assertFalse(missing_close_plan.exists())

            close_paths: dict[str, Path] = {}
            closes: dict[str, dict] = {}
            for option, close_root, role in (
                ("source-close-a", source, "source"),
                ("source-close-b", source, "source"),
                ("destination-close-a", destination, "destination"),
                ("destination-close-b", destination, "destination"),
            ):
                path = evidence / f"{option}.json"
                with mock.patch("sys.stdout", new=io.StringIO()):
                    close_result = cli_main(
                        [
                            "codex-close-capture",
                            "--close-request",
                            str(close_request_path),
                            "--root",
                            str(close_root),
                            "--role",
                            role,
                            "--acknowledge-writers-quiesced",
                            "--output",
                            str(path),
                        ]
                    )
                self.assertEqual(close_result, 0)
                close_paths[option] = path
                closes[option.replace("-", "_")] = read_json(path)
                validate_codex_session_close_capture(closes[option.replace("-", "_")])
            close_arguments = [
                argument
                for option, path in close_paths.items()
                for argument in (f"--{option}", str(path))
            ]
            plan_path = evidence / "plan-v3.json"
            with mock.patch("sys.stdout", new=io.StringIO()):
                plan_result = cli_main(
                    [
                        "codex-plan",
                        *common,
                        "--prefix-request",
                        str(request_path),
                        "--close-request",
                        str(close_request_path),
                        "--source-prefix-a",
                        str(proof_paths[0]),
                        "--source-prefix-b",
                        str(proof_paths[1]),
                        *close_arguments,
                        "--output",
                        str(plan_path),
                    ]
                )
            self.assertEqual(plan_result, 0)
            plan = read_json(plan_path)
            self.assertEqual(
                plan["schema"],
                "dev.tinyland.bulkload.codex-session-union-plan.v3",
            )
            self.assertEqual(
                [
                    item["session_id"]
                    for item in plan["intent"]["promote_source_superset"]
                ],
                [session_id],
            )
            self.assertEqual(
                plan["prefix_evidence"]["destination_close_snapshots"],
                [
                    {
                        "capture_id": closes["destination_close_a"]["snapshot"][
                            "capture_id"
                        ],
                        "snapshot_sha256": closes["destination_close_a"]["snapshot"][
                            "snapshot_sha256"
                        ],
                        "close_capture_sha256": closes["destination_close_a"][
                            "close_capture_sha256"
                        ],
                    },
                    {
                        "capture_id": closes["destination_close_b"]["snapshot"][
                            "capture_id"
                        ],
                        "snapshot_sha256": closes["destination_close_b"]["snapshot"][
                            "snapshot_sha256"
                        ],
                        "close_capture_sha256": closes["destination_close_b"][
                            "close_capture_sha256"
                        ],
                    },
                ],
            )
            for path in [
                request_path,
                *proof_paths,
                close_request_path,
                *close_paths.values(),
                plan_path,
            ]:
                self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)

    def test_codex_plan_cli_protects_local_roots_after_hostname_change(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source-sessions"
            destination = root / "destination-sessions"
            evidence = root / "evidence"
            evidence.mkdir(mode=0o700)
            write_codex_rollout(
                source,
                "77aa77aa-77aa-47aa-87aa-77aa77aa77aa",
            )
            write_codex_rollout(
                destination,
                "77bb77bb-77bb-47bb-87bb-77bb77bb77bb",
            )
            artifacts = {
                "source-a": capture_codex(source),
                "source-b": capture_codex(source),
                "destination-a": capture_codex(
                    destination,
                    role="destination",
                ),
                "destination-b": capture_codex(
                    destination,
                    role="destination",
                ),
            }
            for snapshot in artifacts.values():
                snapshot["host"] = "prior-hostname.example.invalid"
                refresh_codex_snapshot(snapshot)
            for name, snapshot in artifacts.items():
                atomic_write_json(evidence / f"{name}.json", snapshot)
            output = source / "plan.json"

            with (
                redirect_stderr(io.StringIO()),
                mock.patch("sys.stdout", new=io.StringIO()),
            ):
                result = cli_main(
                    [
                        "codex-plan",
                        "--source-a",
                        str(evidence / "source-a.json"),
                        "--source-b",
                        str(evidence / "source-b.json"),
                        "--destination-a",
                        str(evidence / "destination-a.json"),
                        "--destination-b",
                        str(evidence / "destination-b.json"),
                        "--output",
                        str(output),
                    ]
                )

            self.assertEqual(result, 2)
            self.assertFalse(output.exists())

    def test_codex_plan_cli_rechecks_swapped_output_parent(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source-sessions"
            destination = root / "destination-sessions"
            evidence = root / "evidence"
            evidence.mkdir(mode=0o700)
            inside = source / "captured-empty-directory"
            inside.mkdir(parents=True, mode=0o700)
            source.chmod(0o700)
            outside = root / "outside"
            outside.mkdir(mode=0o700)
            output_parent = root / "output-parent"
            output_parent.symlink_to(outside, target_is_directory=True)
            write_codex_rollout(
                source,
                "77cc77cc-77cc-47cc-87cc-77cc77cc77cc",
            )
            write_codex_rollout(
                destination,
                "77dd77dd-77dd-47dd-87dd-77dd77dd77dd",
            )
            artifacts = {
                "source-a": capture_codex(source),
                "source-b": capture_codex(source),
                "destination-a": capture_codex(
                    destination,
                    role="destination",
                ),
                "destination-b": capture_codex(
                    destination,
                    role="destination",
                ),
            }
            for name, snapshot in artifacts.items():
                atomic_write_json(evidence / f"{name}.json", snapshot)
            output = output_parent / "plan.json"
            real_compile = compile_codex_session_union_plan
            swapped = False

            def swap_parent(*args: object, **kwargs: object) -> dict:
                nonlocal swapped
                plan = real_compile(*args, **kwargs)
                output_parent.unlink()
                output_parent.symlink_to(inside, target_is_directory=True)
                swapped = True
                return plan

            with (
                mock.patch(
                    "bulkload_lib.cli.compile_codex_session_union_plan",
                    side_effect=swap_parent,
                ),
                redirect_stderr(io.StringIO()),
                mock.patch("sys.stdout", new=io.StringIO()),
            ):
                result = cli_main(
                    [
                        "codex-plan",
                        "--source-a",
                        str(evidence / "source-a.json"),
                        "--source-b",
                        str(evidence / "source-b.json"),
                        "--destination-a",
                        str(evidence / "destination-a.json"),
                        "--destination-b",
                        str(evidence / "destination-b.json"),
                        "--output",
                        str(output),
                    ]
                )

            self.assertTrue(swapped)
            self.assertEqual(result, 2)
            self.assertFalse((inside / "plan.json").exists())

    def test_codex_plan_cli_revalidates_pinned_inputs_before_output(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source-sessions"
            destination = root / "destination-sessions"
            evidence = root / "evidence"
            evidence.mkdir(mode=0o700)
            write_codex_rollout(
                source,
                "77ee77ee-77ee-47ee-87ee-77ee77ee77ee",
            )
            write_codex_rollout(
                destination,
                "77ff77ff-77ff-47ff-87ff-77ff77ff77ff",
            )
            artifacts = {
                "source-a": capture_codex(source),
                "source-b": capture_codex(source),
                "destination-a": capture_codex(
                    destination,
                    role="destination",
                ),
                "destination-b": capture_codex(
                    destination,
                    role="destination",
                ),
            }
            for name, snapshot in artifacts.items():
                atomic_write_json(evidence / f"{name}.json", snapshot)
            source_a_path = evidence / "source-a.json"
            output = evidence / "plan.json"
            real_compile = compile_codex_session_union_plan
            replaced = False

            def replace_input(*args: object, **kwargs: object) -> dict:
                nonlocal replaced
                plan = real_compile(*args, **kwargs)
                atomic_write_json(source_a_path, artifacts["source-a"])
                replaced = True
                return plan

            with (
                mock.patch(
                    "bulkload_lib.cli.compile_codex_session_union_plan",
                    side_effect=replace_input,
                ),
                redirect_stderr(io.StringIO()),
                mock.patch("sys.stdout", new=io.StringIO()),
            ):
                result = cli_main(
                    [
                        "codex-plan",
                        "--source-a",
                        str(source_a_path),
                        "--source-b",
                        str(evidence / "source-b.json"),
                        "--destination-a",
                        str(evidence / "destination-a.json"),
                        "--destination-b",
                        str(evidence / "destination-b.json"),
                        "--output",
                        str(output),
                    ]
                )

            self.assertTrue(replaced)
            self.assertEqual(result, 2)
            self.assertFalse(output.exists())

    def test_codex_plan_cli_revalidates_pinned_inputs_at_publish(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source-sessions"
            destination = root / "destination-sessions"
            evidence = root / "evidence"
            evidence.mkdir(mode=0o700)
            write_codex_rollout(
                source,
                "78007800-7800-4800-8800-780078007800",
            )
            write_codex_rollout(
                destination,
                "78017801-7801-4801-8801-780178017801",
            )
            artifacts = {
                "source-a": capture_codex(source),
                "source-b": capture_codex(source),
                "destination-a": capture_codex(
                    destination,
                    role="destination",
                ),
                "destination-b": capture_codex(
                    destination,
                    role="destination",
                ),
            }
            for name, snapshot in artifacts.items():
                atomic_write_json(evidence / f"{name}.json", snapshot)
            source_a_path = evidence / "source-a.json"
            output = evidence / "plan.json"
            real_fsync = os.fsync
            mutated = False

            def mutate_input_during_temp_fsync(descriptor: int) -> None:
                nonlocal mutated
                if not mutated and stat.S_ISREG(os.fstat(descriptor).st_mode):
                    source_a_path.write_text("{}\n", encoding="utf-8")
                    mutated = True
                real_fsync(descriptor)

            with (
                mock.patch(
                    "bulkload_lib.cli.os.fsync",
                    side_effect=mutate_input_during_temp_fsync,
                ),
                redirect_stderr(io.StringIO()),
                mock.patch("sys.stdout", new=io.StringIO()),
            ):
                result = cli_main(
                    [
                        "codex-plan",
                        "--source-a",
                        str(source_a_path),
                        "--source-b",
                        str(evidence / "source-b.json"),
                        "--destination-a",
                        str(evidence / "destination-a.json"),
                        "--destination-b",
                        str(evidence / "destination-b.json"),
                        "--output",
                        str(output),
                    ]
                )

            self.assertTrue(mutated)
            self.assertEqual(result, 2)
            self.assertFalse(output.exists())

    def test_codex_plan_cli_never_overwrites_target_inserted_at_publish(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source-sessions"
            destination = root / "destination-sessions"
            evidence = root / "evidence"
            evidence.mkdir(mode=0o700)
            write_codex_rollout(
                source,
                "78027802-7802-4802-8802-780278027802",
            )
            write_codex_rollout(
                destination,
                "78037803-7803-4803-8803-780378037803",
            )
            artifacts = {
                "source-a": capture_codex(source),
                "source-b": capture_codex(source),
                "destination-a": capture_codex(
                    destination,
                    role="destination",
                ),
                "destination-b": capture_codex(
                    destination,
                    role="destination",
                ),
            }
            for name, snapshot in artifacts.items():
                atomic_write_json(evidence / f"{name}.json", snapshot)
            source_a_path = evidence / "source-a.json"
            source_a_bytes = source_a_path.read_bytes()
            output = evidence / "plan.json"
            real_publish = bulkload_cli._rename_codex_output_noreplace
            inserted = False

            def insert_input_at_target(*args: object) -> None:
                nonlocal inserted
                source_a_path.rename(output)
                inserted = True
                real_publish(*args)

            with (
                mock.patch(
                    "bulkload_lib.cli._rename_codex_output_noreplace",
                    side_effect=insert_input_at_target,
                ),
                redirect_stderr(io.StringIO()),
                mock.patch("sys.stdout", new=io.StringIO()),
            ):
                result = cli_main(
                    [
                        "codex-plan",
                        "--source-a",
                        str(source_a_path),
                        "--source-b",
                        str(evidence / "source-b.json"),
                        "--destination-a",
                        str(evidence / "destination-a.json"),
                        "--destination-b",
                        str(evidence / "destination-b.json"),
                        "--output",
                        str(output),
                    ]
                )

            self.assertTrue(inserted)
            self.assertEqual(result, 2)
            self.assertFalse(source_a_path.exists())
            self.assertEqual(output.read_bytes(), source_a_bytes)

    def test_codex_plan_cli_rejects_temp_bytes_changed_at_publish(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source-sessions"
            destination = root / "destination-sessions"
            evidence = root / "evidence"
            evidence.mkdir(mode=0o700)
            write_codex_rollout(
                source,
                "78047804-7804-4804-8804-780478047804",
            )
            write_codex_rollout(
                destination,
                "78057805-7805-4805-8805-780578057805",
            )
            artifacts = {
                "source-a": capture_codex(source),
                "source-b": capture_codex(source),
                "destination-a": capture_codex(
                    destination,
                    role="destination",
                ),
                "destination-b": capture_codex(
                    destination,
                    role="destination",
                ),
            }
            for name, snapshot in artifacts.items():
                atomic_write_json(evidence / f"{name}.json", snapshot)
            output = evidence / "plan.json"
            real_publish = bulkload_cli._rename_codex_output_noreplace
            changed = False

            def change_temp_then_link(
                source_directory: int,
                source_name: str,
                destination_directory: int,
                destination_name: str,
            ) -> None:
                nonlocal changed
                descriptor = os.open(
                    source_name,
                    os.O_WRONLY,
                    dir_fd=source_directory,
                )
                try:
                    size = os.fstat(descriptor).st_size
                    os.ftruncate(descriptor, 0)
                    os.write(descriptor, b"x" * size)
                finally:
                    os.close(descriptor)
                changed = True
                real_publish(
                    source_directory,
                    source_name,
                    destination_directory,
                    destination_name,
                )

            with (
                mock.patch(
                    "bulkload_lib.cli._rename_codex_output_noreplace",
                    side_effect=change_temp_then_link,
                ),
                redirect_stderr(io.StringIO()),
                mock.patch("sys.stdout", new=io.StringIO()),
            ):
                result = cli_main(
                    [
                        "codex-plan",
                        "--source-a",
                        str(evidence / "source-a.json"),
                        "--source-b",
                        str(evidence / "source-b.json"),
                        "--destination-a",
                        str(evidence / "destination-a.json"),
                        "--destination-b",
                        str(evidence / "destination-b.json"),
                        "--output",
                        str(output),
                    ]
                )

            self.assertTrue(changed)
            self.assertEqual(result, 2)
            self.assertTrue(output.exists())
            self.assertEqual(set(output.read_bytes()), {ord("x")})

    def test_codex_plan_cli_rejects_parent_moved_during_publish(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source-sessions"
            destination = root / "destination-sessions"
            evidence = root / "evidence"
            evidence.mkdir(mode=0o700)
            write_codex_rollout(
                source,
                "78067806-7806-4806-8806-780678067806",
            )
            write_codex_rollout(
                destination,
                "78077807-7807-4807-8807-780778077807",
            )
            artifacts = {
                "source-a": capture_codex(source),
                "source-b": capture_codex(source),
                "destination-a": capture_codex(
                    destination,
                    role="destination",
                ),
                "destination-b": capture_codex(
                    destination,
                    role="destination",
                ),
            }
            for name, snapshot in artifacts.items():
                atomic_write_json(evidence / f"{name}.json", snapshot)
            moved = root / "evidence-moved"
            output = evidence / "plan.json"
            real_publish = bulkload_cli._rename_codex_output_noreplace
            moved_parent = False

            def move_parent_then_publish(*args: object) -> None:
                nonlocal moved_parent
                evidence.rename(moved)
                evidence.mkdir(mode=0o700)
                moved_parent = True
                real_publish(*args)

            with (
                mock.patch(
                    "bulkload_lib.cli._rename_codex_output_noreplace",
                    side_effect=move_parent_then_publish,
                ),
                redirect_stderr(io.StringIO()),
                mock.patch("sys.stdout", new=io.StringIO()),
            ):
                result = cli_main(
                    [
                        "codex-plan",
                        "--source-a",
                        str(evidence / "source-a.json"),
                        "--source-b",
                        str(evidence / "source-b.json"),
                        "--destination-a",
                        str(evidence / "destination-a.json"),
                        "--destination-b",
                        str(evidence / "destination-b.json"),
                        "--output",
                        str(output),
                    ]
                )

            self.assertTrue(moved_parent)
            self.assertEqual(result, 2)
            self.assertFalse(output.exists())
            self.assertTrue((moved / "plan.json").exists())

    def test_codex_plan_cli_rejects_target_replaced_during_publish(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source-sessions"
            destination = root / "destination-sessions"
            evidence = root / "evidence"
            evidence.mkdir(mode=0o700)
            write_codex_rollout(
                source,
                "78087808-7808-4808-8808-780878087808",
            )
            write_codex_rollout(
                destination,
                "78097809-7809-4809-8809-780978097809",
            )
            artifacts = {
                "source-a": capture_codex(source),
                "source-b": capture_codex(source),
                "destination-a": capture_codex(
                    destination,
                    role="destination",
                ),
                "destination-b": capture_codex(
                    destination,
                    role="destination",
                ),
            }
            for name, snapshot in artifacts.items():
                atomic_write_json(evidence / f"{name}.json", snapshot)
            output = evidence / "plan.json"
            stolen = evidence / "stolen.json"
            bogus = b'{"bogus":true}\n'
            real_publish = bulkload_cli._rename_codex_output_noreplace
            replaced = False

            def replace_target_after_publish(
                source_directory: int,
                source_name: str,
                destination_directory: int,
                destination_name: str,
            ) -> None:
                nonlocal replaced
                real_publish(
                    source_directory,
                    source_name,
                    destination_directory,
                    destination_name,
                )
                os.rename(
                    destination_name,
                    stolen.name,
                    src_dir_fd=destination_directory,
                    dst_dir_fd=destination_directory,
                )
                descriptor = os.open(
                    destination_name,
                    os.O_WRONLY | os.O_CREAT | os.O_EXCL,
                    0o600,
                    dir_fd=destination_directory,
                )
                try:
                    os.write(descriptor, bogus)
                    os.fsync(descriptor)
                finally:
                    os.close(descriptor)
                replaced = True

            with (
                mock.patch(
                    "bulkload_lib.cli._rename_codex_output_noreplace",
                    side_effect=replace_target_after_publish,
                ),
                redirect_stderr(io.StringIO()),
                mock.patch("sys.stdout", new=io.StringIO()),
            ):
                result = cli_main(
                    [
                        "codex-plan",
                        "--source-a",
                        str(evidence / "source-a.json"),
                        "--source-b",
                        str(evidence / "source-b.json"),
                        "--destination-a",
                        str(evidence / "destination-a.json"),
                        "--destination-b",
                        str(evidence / "destination-b.json"),
                        "--output",
                        str(output),
                    ]
                )

            self.assertTrue(replaced)
            self.assertEqual(result, 2)
            self.assertEqual(output.read_bytes(), bogus)
            self.assertTrue(stolen.exists())

    def test_repo_capture_cli_does_not_require_session_record_budget(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            repository = root / "repository"
            repository.mkdir()
            output = root / "capture.json"
            snapshot = {
                "catalog_sha256": "a" * 64,
                "catalog": [],
                "complete": True,
            }

            with (
                mock.patch(
                    "bulkload_lib.cli.capture_snapshot",
                    return_value=snapshot,
                ) as capture,
                mock.patch("sys.stdout", new=io.StringIO()),
            ):
                result = cli_main(
                    [
                        "capture",
                        "--root",
                        str(repository),
                        "--output",
                        str(output),
                    ]
                )

            self.assertEqual(result, 0)
            self.assertNotIn("max_record_bytes", capture.call_args.kwargs)
            self.assertEqual(
                json.loads(output.read_text(encoding="utf-8")),
                snapshot,
            )

    def test_codex_capture_pins_directories_against_symlink_swap(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sessions = root / "sessions"
            child = sessions / "2026"
            child.mkdir(parents=True, mode=0o700)
            sessions.chmod(0o700)
            child.chmod(0o700)
            outside = root / "outside"
            write_codex_rollout(
                outside,
                "88888888-8888-4888-8888-888888888888",
                day="",
            )
            real_open_directory = session_catalogs._open_directory
            raced = False

            def swap_before_open(
                name: str | Path,
                *,
                parent_descriptor: int | None,
                path: Path,
            ) -> int:
                nonlocal raced
                if parent_descriptor is not None and name == "2026" and not raced:
                    raced = True
                    child.rmdir()
                    child.symlink_to(outside, target_is_directory=True)
                return real_open_directory(
                    name,
                    parent_descriptor=parent_descriptor,
                    path=path,
                )

            with mock.patch.object(
                session_catalogs,
                "_open_directory",
                side_effect=swap_before_open,
            ):
                snapshot = capture_codex(sessions)

            self.assertTrue(raced)
            self.assertFalse(snapshot["complete"])
            self.assertEqual(snapshot["sessions"], [])
            self.assertTrue(
                any("without following links" in error for error in snapshot["errors"])
            )

    def test_codex_capture_checks_actual_size_against_total_budget(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            sessions = Path(directory) / "sessions"
            rollout = write_codex_rollout(
                sessions,
                "99999999-9999-4999-8999-999999999999",
            )
            max_bytes = rollout.stat().st_size
            real_capture_rollout = session_catalogs._capture_rollout

            def report_larger_capture(*args: object, **kwargs: object) -> dict:
                record = real_capture_rollout(*args, **kwargs)
                return {**record, "size": max_bytes + 1}

            with mock.patch.object(
                session_catalogs,
                "_capture_rollout",
                side_effect=report_larger_capture,
            ):
                snapshot = capture_codex(
                    sessions,
                    max_bytes=max_bytes,
                )

            self.assertFalse(snapshot["complete"])
            self.assertEqual(snapshot["sessions"], [])
            self.assertEqual(snapshot["total_bytes"], 0)
            self.assertTrue(
                any("byte budget exceeded" in error for error in snapshot["errors"])
            )

    def test_codex_capture_charges_malformed_candidates_to_file_and_byte_budgets(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sessions = root / "sessions"
            first = write_codex_rollout(
                sessions,
                "9a9a9a9a-9a9a-4a9a-8a9a-9a9a9a9a9a9a",
                day="2026/07/24",
            )
            second = write_codex_rollout(
                sessions,
                "9b9b9b9b-9b9b-4b9b-8b9b-9b9b9b9b9b9b",
                day="2026/07/25",
            )
            for rollout in (first, second):
                payload = rollout.read_bytes()
                self.assertTrue(payload.endswith(b"}\n"))
                rollout.write_bytes(payload[:-2] + b"!\n")

            byte_bounded = capture_codex(
                sessions,
                max_bytes=first.stat().st_size,
            )
            self.assertFalse(byte_bounded["complete"])
            self.assertTrue(
                any(
                    "total byte budget exceeded" in error
                    for error in byte_bounded["errors"]
                )
            )
            self.assertEqual(
                sum("strict UTF-8 JSONL" in error for error in byte_bounded["errors"]),
                1,
            )

            file_bounded = capture_codex(
                sessions,
                max_files=1,
                max_bytes=first.stat().st_size + second.stat().st_size,
            )
            self.assertFalse(file_bounded["complete"])
            self.assertTrue(
                any("file budget exceeded" in error for error in file_bounded["errors"])
            )
            self.assertEqual(
                sum("strict UTF-8 JSONL" in error for error in file_bounded["errors"]),
                1,
            )

    def test_codex_snapshot_rejects_path_uuid_mismatch(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            sessions = Path(directory) / "sessions"
            write_codex_rollout(
                sessions,
                "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            )
            snapshot = capture_codex(sessions)
            snapshot["sessions"][0]["relative_path"] = (
                "2026/07/24/"
                "rollout-2026-07-24T00-00-00-bbbbbbbb-bbbb-4bbb-8bbb-"
                "bbbbbbbbbbbb.jsonl"
            )
            catalog = {
                "sessions": snapshot["sessions"],
                "non_private_file_count": snapshot["non_private_file_count"],
                "non_private_directory_count": snapshot["non_private_directory_count"],
            }
            snapshot["catalog_sha256"] = sha256_bytes(canonical_bytes(catalog))
            snapshot["snapshot_sha256"] = object_digest(
                snapshot,
                "snapshot_sha256",
            )

            with self.assertRaisesRegex(
                BulkloadError,
                "record identity is invalid",
            ):
                validate_codex_session_snapshot(snapshot)

    def test_codex_capture_requires_explicit_quiescence(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(
                BulkloadError,
                "writer-quiescence acknowledgement",
            ):
                capture_codex_sessions(
                    Path(directory) / "sessions",
                    role="source",
                    acknowledge_writers_quiesced=False,
                    host_authority_id=TEST_HOST_AUTHORITY_ID,
                )

    def test_codex_destination_requires_private_files_and_directories(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            file_root = root / "file-mode"
            write_codex_rollout(
                file_root,
                "abababab-abab-4bab-8bab-abababababab",
                mode=0o644,
            )
            file_capture = capture_codex(file_root, role="destination")
            self.assertFalse(file_capture["complete"])
            self.assertTrue(
                any("exactly 0600" in error for error in file_capture["errors"])
            )

            directory_root = root / "directory-mode"
            write_codex_rollout(
                directory_root,
                "acacacac-acac-4cac-8cac-acacacacacac",
            )
            (directory_root / "2026").chmod(0o755)
            directory_capture = capture_codex(
                directory_root,
                role="destination",
            )
            self.assertFalse(directory_capture["complete"])
            self.assertTrue(
                any("exactly 0700" in error for error in directory_capture["errors"])
            )

    def test_codex_plan_requires_stable_destination_pair_and_roles(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            destination = root / "destination"
            write_codex_rollout(
                source,
                "adadadad-adad-4dad-8dad-adadadadadad",
            )
            destination_id = "aeaeaeae-aeae-4eae-8eae-aeaeaeaeaeae"
            write_codex_rollout(destination, destination_id, message="first")
            source_a = capture_codex(source)
            source_b = capture_codex(source)
            destination_a = capture_codex(destination, role="destination")
            write_codex_rollout(destination, destination_id, message="second")
            destination_b = capture_codex(destination, role="destination")

            with self.assertRaisesRegex(
                BulkloadError,
                "destination pass A and pass B differ",
            ):
                compile_codex_session_union_plan(
                    source_a,
                    source_b,
                    destination_a,
                    destination_b,
                )

            wrong_role_a = capture_codex(destination)
            wrong_role_b = capture_codex(destination)
            with self.assertRaisesRegex(BulkloadError, "wrong role"):
                compile_codex_session_union_plan(
                    source_a,
                    source_b,
                    wrong_role_a,
                    wrong_role_b,
                )

            stable_destination_a = capture_codex(
                destination,
                role="destination",
            )
            empty = destination / "empty-directory"
            empty.mkdir(mode=0o700)
            stable_destination_b = capture_codex(
                destination,
                role="destination",
            )
            with self.assertRaisesRegex(
                BulkloadError,
                "destination pass A and pass B differ",
            ):
                compile_codex_session_union_plan(
                    source_a,
                    source_b,
                    stable_destination_a,
                    stable_destination_b,
                )

    def test_codex_stability_barrier_compares_catalog_bodies(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            sessions = Path(directory) / "sessions"
            write_codex_rollout(
                sessions,
                "aeeeeeee-aeee-4eee-8eee-aeeeeeeeeeee",
            )
            source_a = capture_codex(sessions)
            source_b = capture_codex(sessions)
            source_b["sessions"][0]["sha256"] = "f" * 64
            forced_digest = "e" * 64
            for snapshot in (source_a, source_b):
                snapshot["catalog_sha256"] = forced_digest
                snapshot["snapshot_sha256"] = object_digest(
                    snapshot,
                    "snapshot_sha256",
                )

            with (
                mock.patch.object(
                    session_catalogs,
                    "sha256_bytes",
                    return_value=forced_digest,
                ),
                self.assertRaisesRegex(
                    BulkloadError,
                    "source pass A and pass B differ",
                ),
            ):
                session_catalogs._validate_stable_capture_pair(
                    source_a,
                    source_b,
                    role="source",
                )

    def test_codex_plan_requires_four_distinct_capture_ids(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            destination = root / "destination"
            write_codex_rollout(
                source,
                "afafafaf-afaf-4faf-8faf-afafafafafaf",
            )
            write_codex_rollout(
                destination,
                "b0b0b0b0-b0b0-40b0-80b0-b0b0b0b0b0b0",
            )
            source_a = capture_codex(source)
            source_b = capture_codex(source)
            destination_a = capture_codex(destination, role="destination")
            destination_b = capture_codex(destination, role="destination")
            destination_a["capture_id"] = source_a["capture_id"]
            refresh_codex_snapshot(destination_a)

            with self.assertRaisesRegex(BulkloadError, "four distinct"):
                compile_codex_session_union_plan(
                    source_a,
                    source_b,
                    destination_a,
                    destination_b,
                )

    def test_codex_plan_rejects_same_root_through_ancestor_alias(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            real_parent = root / "real"
            sessions = real_parent / "sessions"
            alias_parent = root / "alias"
            write_codex_rollout(
                sessions,
                "b0c0b0c0-b0c0-40c0-80c0-b0c0b0c0b0c0",
            )
            alias_parent.symlink_to(real_parent, target_is_directory=True)
            aliased_sessions = alias_parent / "sessions"

            source_a = capture_codex(sessions)
            source_b = capture_codex(sessions)
            destination_a = capture_codex(
                aliased_sessions,
                role="destination",
            )
            destination_b = capture_codex(
                aliased_sessions,
                role="destination",
            )

            self.assertNotEqual(source_a["root"], destination_a["root"])
            self.assertEqual(
                source_a["root_identity"],
                destination_a["root_identity"],
            )
            with self.assertRaisesRegex(
                BulkloadError,
                "source and destination must differ",
            ):
                compile_codex_session_union_plan(
                    source_a,
                    source_b,
                    destination_a,
                    destination_b,
                )

    def test_codex_plan_rejects_nested_root_custody(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "sessions"
            destination = source / "nested-destination"
            write_codex_rollout(
                source,
                "b0d0b0d0-b0d0-40d0-80d0-b0d0b0d0b0d0",
                day="2026/07/24",
            )
            write_codex_rollout(
                destination,
                "b0e0b0e0-b0e0-40e0-80e0-b0e0b0e0b0e0",
                day="2026/07/25",
            )

            with self.assertRaisesRegex(BulkloadError, "roots overlap"):
                compile_codex_session_union_plan(
                    capture_codex(source),
                    capture_codex(source),
                    capture_codex(destination, role="destination"),
                    capture_codex(destination, role="destination"),
                )

    def test_codex_plan_rejects_same_custody_after_hostname_change(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            sessions = Path(directory) / "sessions"
            write_codex_rollout(
                sessions,
                "b0f0b0f0-b0f0-40f0-80f0-b0f0b0f0b0f0",
            )
            source_a = capture_codex(sessions)
            source_b = capture_codex(sessions)
            destination_a = capture_codex(sessions, role="destination")
            destination_b = capture_codex(sessions, role="destination")
            for snapshot in (destination_a, destination_b):
                snapshot["host"] = "renamed-host.example.invalid"
                refresh_codex_snapshot(snapshot)

            with self.assertRaisesRegex(BulkloadError, "roots overlap"):
                compile_codex_session_union_plan(
                    source_a,
                    source_b,
                    destination_a,
                    destination_b,
                )

    def test_codex_plan_scopes_equal_root_identity_by_host_authority(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            sessions = Path(directory) / "sessions"
            write_codex_rollout(
                sessions,
                "b1a1b1a1-b1a1-41a1-81a1-b1a1b1a1b1a1",
            )
            plan = compile_codex_session_union_plan(
                capture_codex(sessions),
                capture_codex(sessions),
                capture_codex(
                    sessions,
                    role="destination",
                    host_authority_id=OTHER_HOST_AUTHORITY_ID,
                ),
                capture_codex(
                    sessions,
                    role="destination",
                    host_authority_id=OTHER_HOST_AUTHORITY_ID,
                ),
            )

            self.assertTrue(plan["intent"]["ready_for_attended_copy"])
            self.assertEqual(plan["intent"]["copy_if_absent"], [])
            self.assertEqual(
                plan["source"]["host_authority_id"],
                TEST_HOST_AUTHORITY_ID,
            )
            self.assertEqual(
                plan["destination"]["host_authority_id"],
                OTHER_HOST_AUTHORITY_ID,
            )

    def test_codex_any_blocker_suppresses_all_copy_candidates(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            destination = root / "destination"
            divergent = "b1b1b1b1-b1b1-41b1-81b1-b1b1b1b1b1b1"
            absent = "b2b2b2b2-b2b2-42b2-82b2-b2b2b2b2b2b2"
            write_codex_rollout(source, divergent, message="source")
            write_codex_rollout(source, absent, message="absent")
            write_codex_rollout(
                destination,
                divergent,
                message="destination",
            )

            plan = compile_codex_session_union_plan(
                capture_codex(source),
                capture_codex(source),
                capture_codex(destination, role="destination"),
                capture_codex(destination, role="destination"),
            )

            self.assertFalse(plan["intent"]["ready_for_attended_copy"])
            self.assertEqual(plan["intent"]["copy_if_absent"], [])
            self.assertEqual(
                [item["code"] for item in plan["intent"]["blockers"]],
                ["same-uuid-prefix-proof-required"],
            )

    def test_codex_cross_input_path_collision_is_a_global_blocker(self) -> None:
        source = [
            {
                "session_id": "b3b3b3b3-b3b3-43b3-83b3-b3b3b3b3b3b3",
                "relative_path": "Day/Rollout.jsonl",
                "sha256": "1" * 64,
                "size": 1,
                "mode": "0600",
                "records": 1,
            },
            {
                "session_id": "b4b4b4b4-b4b4-44b4-84b4-b4b4b4b4b4b4",
                "relative_path": "other/rollout.jsonl",
                "sha256": "2" * 64,
                "size": 1,
                "mode": "0600",
                "records": 1,
            },
        ]
        destination = [
            {
                "session_id": "b5b5b5b5-b5b5-45b5-85b5-b5b5b5b5b5b5",
                "relative_path": "day/rollout.jsonl",
                "sha256": "3" * 64,
                "size": 1,
                "mode": "0600",
                "records": 1,
            }
        ]

        classified = session_catalogs._classify_codex_session_union(
            source,
            destination,
            [],
            [],
        )

        self.assertEqual(classified["copy_if_absent"], [])
        self.assertEqual(
            [item["code"] for item in classified["blockers"]],
            ["relative-path-namespace-collision"],
        )

    def test_codex_plan_blocks_file_directory_namespace_collisions(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            destination = root / "destination"
            source_rollout = write_codex_rollout(
                source,
                "b5c5b5c5-b5c5-45c5-85c5-b5c5b5c5b5c5",
            )
            occupied = destination / source_rollout.relative_to(source)
            occupied.mkdir(parents=True, mode=0o700)
            destination.chmod(0o700)
            current = destination
            for component in occupied.relative_to(destination).parts:
                current /= component
                current.chmod(0o700)

            plan = compile_codex_session_union_plan(
                capture_codex(source),
                capture_codex(source),
                capture_codex(destination, role="destination"),
                capture_codex(destination, role="destination"),
            )

            self.assertFalse(plan["intent"]["ready_for_attended_copy"])
            self.assertEqual(plan["intent"]["copy_if_absent"], [])
            self.assertEqual(
                [item["code"] for item in plan["intent"]["blockers"]],
                ["relative-path-type-collision"],
            )

            prefix_source = root / "prefix-source"
            prefix_destination = root / "prefix-destination"
            ancestor_id = "b5d5b5d5-b5d5-45d5-85d5-b5d5b5d5b5d5"
            ancestor = write_codex_rollout(
                prefix_destination,
                ancestor_id,
                day="",
            )
            write_codex_rollout(
                prefix_source,
                "b5e5b5e5-b5e5-45e5-85e5-b5e5b5e5b5e5",
                day=ancestor.name,
            )

            prefix_plan = compile_codex_session_union_plan(
                capture_codex(prefix_source),
                capture_codex(prefix_source),
                capture_codex(prefix_destination, role="destination"),
                capture_codex(prefix_destination, role="destination"),
            )
            self.assertFalse(prefix_plan["intent"]["ready_for_attended_copy"])
            self.assertEqual(prefix_plan["intent"]["copy_if_absent"], [])
            self.assertEqual(
                [item["code"] for item in prefix_plan["intent"]["blockers"]],
                ["relative-path-type-collision"],
            )

    def test_codex_capture_rejects_strict_json_and_identity_violations(
        self,
    ) -> None:
        cases = {
            "duplicate": (
                b'{"type":"session_meta","type":"session_meta","payload":{"id":"%s"}}\n'
            ),
            "nonfinite": (
                b'{"type":"session_meta","payload":{"id":"%s"},"value":NaN}\n'
            ),
            "overflow": (
                b'{"type":"session_meta","payload":{"id":"%s"},"value":1e999}\n'
            ),
            "not-first": b'{"type":"event_msg","payload":{"id":"%s"}}\n',
            "noncanonical": (b'{"type":"session_meta","payload":{"id":"%s"}}\n'),
        }
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for index, (name, template) in enumerate(cases.items(), start=1):
                session_id = f"b6b6b6b{index}-b6b6-46b6-86b6-b6b6b6b6b6b6"
                case_root = root / name
                rollout = write_codex_rollout(case_root, session_id)
                embedded_id = (
                    session_id.upper() if name == "noncanonical" else session_id
                )
                rollout.write_bytes(template % embedded_id.encode("ascii"))
                snapshot = capture_codex(case_root)
                self.assertFalse(snapshot["complete"], name)

            utf16_root = root / "utf16"
            utf16_id = "b6c6b6c6-b6c6-46c6-86c6-b6c6b6c6b6c6"
            utf16_rollout = write_codex_rollout(utf16_root, utf16_id)
            utf16_record = (
                json.dumps(
                    {
                        "type": "session_meta",
                        "payload": {"id": utf16_id},
                    },
                    separators=(",", ":"),
                )
                + "\n"
            )
            utf16_rollout.write_bytes(b"\xfe\xff" + utf16_record.encode("utf-16-be"))
            utf16_snapshot = capture_codex(utf16_root)
            self.assertFalse(utf16_snapshot["complete"])
            self.assertTrue(
                any("strict UTF-8 JSONL" in error for error in utf16_snapshot["errors"])
            )

            duplicate_meta_root = root / "duplicate-meta"
            duplicate_meta_id = "b7b7b7b7-b7b7-47b7-87b7-b7b7b7b7b7b7"
            duplicate_meta = write_codex_rollout(
                duplicate_meta_root,
                duplicate_meta_id,
            )
            meta = (
                json.dumps(
                    {
                        "type": "session_meta",
                        "payload": {"id": duplicate_meta_id},
                    },
                    separators=(",", ":"),
                )
                + "\n"
            )
            duplicate_meta.write_text(meta + meta, encoding="utf-8")
            snapshot = capture_codex(duplicate_meta_root)
            self.assertFalse(snapshot["complete"])
            self.assertTrue(
                any("multiple session_meta" in error for error in snapshot["errors"])
            )

    def test_codex_capture_rejects_external_hardlink_alias(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sessions = root / "sessions"
            rollout = write_codex_rollout(
                sessions,
                "b8b8b8b8-b8b8-48b8-88b8-b8b8b8b8b8b8",
            )
            os.link(rollout, root / "external-alias.jsonl")

            snapshot = capture_codex(sessions)

            self.assertFalse(snapshot["complete"])
            self.assertEqual(snapshot["sessions"], [])
            self.assertTrue(
                any(
                    "hardlinks are unsupported" in error for error in snapshot["errors"]
                )
            )

    def test_codex_capture_emits_valid_bounded_duplicate_uuid_failure(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            sessions = Path(directory) / "sessions"
            session_id = "b8c8b8c8-b8c8-48c8-88c8-b8c8b8c8b8c8"
            write_codex_rollout(sessions, session_id, day="2026/07/24")
            write_codex_rollout(sessions, session_id, day="2026/07/25")

            snapshot = capture_codex(sessions)

            self.assertFalse(snapshot["complete"])
            self.assertEqual(snapshot["sessions"], [])
            self.assertTrue(
                any(
                    "duplicate Codex session UUID" in error
                    for error in snapshot["errors"]
                )
            )
            validate_codex_session_snapshot(snapshot)

    def test_codex_capture_revalidates_root_path_after_scan(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sessions = root / "sessions"
            displaced = root / "displaced"
            write_codex_rollout(
                sessions,
                "b9b9b9b9-b9b9-49b9-89b9-b9b9b9b9b9b9",
            )
            real_capture = session_catalogs._capture_rollout
            swapped = False

            def swap_root(*args: object, **kwargs: object) -> dict:
                nonlocal swapped
                record = real_capture(*args, **kwargs)
                if not swapped:
                    sessions.rename(displaced)
                    sessions.mkdir(mode=0o700)
                    swapped = True
                return record

            with mock.patch.object(
                session_catalogs,
                "_capture_rollout",
                side_effect=swap_root,
            ):
                snapshot = capture_codex(sessions)

            self.assertTrue(swapped)
            self.assertFalse(snapshot["complete"])
            self.assertTrue(
                any(
                    "root path changed" in error
                    or "root authority changed" in error
                    or "root changed during capture" in error
                    for error in snapshot["errors"]
                )
            )

    def test_codex_capture_revalidates_closed_subtrees(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            sessions = Path(directory) / "sessions"
            write_codex_rollout(
                sessions,
                "bab0bab0-bab0-4ab0-8ab0-bab0bab0bab0",
            )
            year = sessions / "2026"
            real_open_relative = session_catalogs._open_relative_directory
            mutated = False

            def mutate_closed_subtree(
                root_descriptor: int,
                relative_path: str,
            ) -> int:
                nonlocal mutated
                if relative_path == "2026" and not mutated:
                    year.chmod(0o750)
                    mutated = True
                return real_open_relative(root_descriptor, relative_path)

            with mock.patch.object(
                session_catalogs,
                "_open_relative_directory",
                side_effect=mutate_closed_subtree,
            ):
                snapshot = capture_codex(sessions)

            self.assertTrue(mutated)
            self.assertFalse(snapshot["complete"])
            self.assertTrue(
                any("changed after its scan" in error for error in snapshot["errors"])
            )

    def test_codex_capture_enforces_traversal_record_and_error_budgets(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sessions = root / "sessions"
            write_codex_rollout(
                sessions,
                "bbb1bbb1-bbb1-4bb1-8bb1-bbb1bbb1bbb1",
            )
            entries = capture_codex(sessions, max_entries=1)
            self.assertFalse(entries["complete"])
            self.assertTrue(
                any("entry budget exceeded" in error for error in entries["errors"])
            )

            records = capture_codex(sessions, max_records_per_file=1)
            self.assertFalse(records["complete"])
            self.assertTrue(
                any(
                    "record-count budget exceeded" in error
                    for error in records["errors"]
                )
            )

            error_root = root / "errors"
            error_root.mkdir(mode=0o700)
            for name in ("a.db", "b.db"):
                (error_root / name).write_bytes(b"x")
                (error_root / name).chmod(0o600)
            bounded_errors = capture_codex(error_root, max_errors=1)
            self.assertFalse(bounded_errors["complete"])
            self.assertEqual(len(bounded_errors["errors"]), 1)

    def test_codex_capture_enforces_catalog_and_output_budgets(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            sessions = Path(directory) / "sessions"
            write_codex_rollout(
                sessions,
                "bbb2bbb2-bbb2-4bb2-8bb2-bbb2bbb2bbb2",
            )
            catalog = capture_codex(sessions, max_catalog_bytes=1)
            self.assertFalse(catalog["complete"])
            self.assertEqual(catalog["sessions"], [])
            self.assertTrue(
                any(
                    "catalog byte budget exceeded" in error
                    for error in catalog["errors"]
                )
            )

            with self.assertRaisesRegex(BulkloadError, "bounded failure envelope"):
                capture_codex(
                    sessions,
                    max_catalog_bytes=1,
                    max_output_bytes=1,
                )

    def test_stable_plan_apply_and_verify(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, destination = make_pair(root)
            (source / "tracked.txt").write_text("changed\n", encoding="utf-8")
            (source / "notes.txt").write_text("new\n", encoding="utf-8")

            source_a = capture_snapshot(source, "repo")
            source_b = capture_snapshot(source, "repo")
            destination_before = capture_snapshot(destination, "repo")
            self.assertEqual(source_a["catalog_sha256"], source_b["catalog_sha256"])

            plan = compile_plan(source_a, source_b, destination_before)
            self.assertTrue(plan["intent"]["ready"])
            self.assertEqual(
                export_copy_paths(plan, plan["plan_sha256"]),
                ["notes.txt", "tracked.txt"],
            )
            receipt = apply_plan(
                plan,
                source_root=source,
                destination_root=destination,
                accepted_digest=plan["plan_sha256"],
                state_root=root / "state",
                receipt_path=root / "receipt.json",
            )
            self.assertEqual(len(receipt["operations"]), 2)
            self.assertEqual((destination / "tracked.txt").read_text(), "changed\n")
            self.assertEqual((destination / "notes.txt").read_text(), "new\n")
            self.assertEqual(
                (
                    root
                    / "state"
                    / "backups"
                    / plan["plan_sha256"]
                    / "__root__"
                    / "tracked.txt"
                ).read_text(),
                "base\n",
            )

            destination_after = capture_snapshot(destination, "repo")
            verification = verify_plan(plan, destination_after, plan["plan_sha256"])
            self.assertTrue(verification["verified"], verification["failures"])
            wrong_mode = verify_plan(
                plan, capture_snapshot(destination, "fleet"), plan["plan_sha256"]
            )
            self.assertFalse(wrong_mode["verified"])
            self.assertIn(
                "destination-mode-mismatch",
                {item["code"] for item in wrong_mode["failures"]},
            )

            replay = apply_plan(
                plan,
                source_root=source,
                destination_root=destination,
                accepted_digest=plan["plan_sha256"],
                state_root=root / "state",
                receipt_path=root / "receipt-replay.json",
            )
            self.assertTrue(
                all(
                    item["result"] == "verified-existing"
                    for item in replay["operations"]
                )
            )
            backup = (
                root
                / "state"
                / "backups"
                / plan["plan_sha256"]
                / "__root__"
                / "tracked.txt"
            )
            backup.write_text("corrupt\n", encoding="utf-8")
            with self.assertRaisesRegex(BulkloadError, "exact backup"):
                apply_plan(
                    plan,
                    source_root=source,
                    destination_root=destination,
                    accepted_digest=plan["plan_sha256"],
                    state_root=root / "state",
                    receipt_path=root / "receipt-corrupt.json",
                )

    def test_moving_source_fails_barrier(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, destination = make_pair(root)
            (source / "notes.txt").write_text("one\n", encoding="utf-8")
            source_a = capture_snapshot(source, "repo")
            (source / "notes.txt").write_text("two\n", encoding="utf-8")
            source_b = capture_snapshot(source, "repo")
            with self.assertRaisesRegex(BulkloadError, "pass A and pass B differ"):
                compile_plan(source_a, source_b, capture_snapshot(destination, "repo"))

    def test_same_capture_cannot_fill_both_source_passes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source, destination = make_pair(Path(directory))
            snapshot = capture_snapshot(source, "repo")
            with self.assertRaisesRegex(BulkloadError, "distinct captures"):
                compile_plan(snapshot, snapshot, capture_snapshot(destination, "repo"))

    def test_verification_requires_a_fresh_destination_capture(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source, destination = make_pair(Path(directory))
            source_a = capture_snapshot(source, "repo")
            source_b = capture_snapshot(source, "repo")
            destination_before = capture_snapshot(destination, "repo")
            plan = compile_plan(source_a, source_b, destination_before)
            self.assertTrue(plan["intent"]["ready"], plan["intent"]["blockers"])
            self.assertEqual(plan["intent"]["operations"], [])

            reused = verify_plan(plan, destination_before, plan["plan_sha256"])
            self.assertFalse(reused["verified"])
            self.assertIn(
                "destination-snapshot-not-fresh",
                {item["code"] for item in reused["failures"]},
            )
            fresh = verify_plan(
                plan,
                capture_snapshot(destination, "repo"),
                plan["plan_sha256"],
            )
            self.assertTrue(fresh["verified"], fresh["failures"])

            alternates = destination / ".git" / "objects" / "info" / "alternates"
            alternates.parent.mkdir(parents=True, exist_ok=True)
            alternates.write_text(f"{source / '.git' / 'objects'}\n", encoding="utf-8")
            with_alternates = capture_snapshot(destination, "repo")
            self.assertTrue(with_alternates["complete"], with_alternates["errors"])
            rejected = verify_plan(plan, with_alternates, plan["plan_sha256"])
            self.assertFalse(rejected["verified"])
            self.assertIn(
                ("repository-unsupported-git-authority", "has_alternates"),
                {
                    (item["code"], item.get("authority"))
                    for item in rejected["failures"]
                },
            )

    def test_sensitive_untracked_path_blocks(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, destination = make_pair(root)
            (source / ".env").write_text("TOKEN=not-a-real-token\n", encoding="utf-8")
            source_a = capture_snapshot(source, "repo")
            source_b = capture_snapshot(source, "repo")
            redacted = next(
                item
                for item in source_b["catalog"][0]["files"]
                if item["path"] == ".env"
            )
            self.assertEqual(redacted["kind"], "redacted")
            self.assertNotIn("sha256", redacted)
            plan = compile_plan(
                source_a, source_b, capture_snapshot(destination, "repo")
            )
            self.assertFalse(plan["intent"]["ready"])
            self.assertIn(
                "source-path-ineligible",
                {item["code"] for item in plan["intent"]["blockers"]},
            )

    def test_clean_tracked_sensitive_path_is_privately_attested(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, destination = make_pair(root)
            sensitive_content = "tracked-sensitive-placeholder\n"
            base_observed_bytes = capture_snapshot(source, "repo")["catalog"][0][
                "observed_bytes"
            ]
            (source / ".envrc").write_text(sensitive_content, encoding="utf-8")
            git(source, "add", ".envrc")
            git(
                source,
                "-c",
                "user.name=Bulkload Test",
                "-c",
                "user.email=bulkload@example.invalid",
                "commit",
                "-q",
                "-m",
                "tracked sensitive fixture",
            )
            git(destination, "fetch", "-q", "origin")
            git(destination, "reset", "-q", "--hard", "origin/main")

            source_a = capture_snapshot(source, "repo")
            source_b = capture_snapshot(source, "repo")
            destination_before = capture_snapshot(destination, "repo")
            record = next(
                item
                for item in source_b["catalog"][0]["files"]
                if item["path"] == ".envrc"
            )

            self.assertIsNone(record["status"])
            self.assertEqual(record["kind"], "redacted")
            self.assertFalse(record["eligible"])
            for field in ("git_blob_oid", "index_entries", "mode", "sha256", "size"):
                self.assertNotIn(field, record)
            self.assertEqual(
                source_b["catalog"][0]["observed_bytes"], base_observed_bytes
            )
            serialized = canonical_bytes(source_b).decode()
            self.assertNotIn(sensitive_content, serialized)
            self.assertNotIn("_bulkload_private_", serialized)
            self.assertFalse(
                any(field.startswith("_bulkload_private_") for field in record)
            )

            plan = compile_plan(source_a, source_b, destination_before)
            self.assertTrue(plan["intent"]["ready"], plan["intent"]["blockers"])
            self.assertEqual(plan["intent"]["operations"], [])
            verification = verify_plan(
                plan,
                capture_snapshot(destination, "repo"),
                plan["plan_sha256"],
            )
            self.assertTrue(verification["verified"], verification["failures"])

            forbidden_fields = {
                "_bulkload_private_git_blob_oid": "0" * 40,
                "content": sensitive_content,
                "git_blob_oid": "0" * 40,
                "index_entries": [],
                "mode": "0644",
                "sha256": "0" * 64,
                "size": len(sensitive_content),
            }
            for field, value in forbidden_fields.items():
                with self.subTest(forged_field=field):
                    forged = json.loads(json.dumps(source_b))
                    forged_record = next(
                        item
                        for item in forged["catalog"][0]["files"]
                        if item["path"] == ".envrc"
                    )
                    forged_record[field] = value
                    forged["catalog_sha256"] = sha256_bytes(
                        canonical_bytes(forged["catalog"])
                    )
                    forged["snapshot_sha256"] = object_digest(forged, "snapshot_sha256")
                    with self.assertRaisesRegex(
                        BulkloadError, "unexpected fields|keys differ"
                    ):
                        validate_snapshot(forged)

    def test_tracked_sensitive_size_does_not_leak_through_budget_errors(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source, _destination = make_pair(Path(directory))
            sensitive_content = "tracked-sensitive-placeholder\n"
            (source / ".envrc").write_text(sensitive_content, encoding="utf-8")
            git(source, "add", ".envrc")
            git(
                source,
                "-c",
                "user.name=Bulkload Test",
                "-c",
                "user.email=bulkload@example.invalid",
                "commit",
                "-q",
                "-m",
                "tracked sensitive fixture",
            )

            snapshot = capture_snapshot(
                source,
                "repo",
                max_bytes=len(sensitive_content.encode()) + 2,
            )
            self.assertFalse(snapshot["complete"])
            joined_errors = "\n".join(snapshot["errors"])
            self.assertIn("file exceeds remaining byte budget", joined_errors)
            self.assertNotIn(" > ", joined_errors)
            self.assertNotIn(str(len(sensitive_content.encode())), joined_errors)

    def test_tracked_sensitive_drift_is_redacted_and_blocked(self) -> None:
        variants = {
            "content": "M",
            "delete": "D",
            "mode": "T",
            "symlink": "T",
        }
        for variant, expected_status in variants.items():
            with (
                self.subTest(variant=variant),
                tempfile.TemporaryDirectory() as directory,
            ):
                root = Path(directory)
                source, destination = make_pair(root)
                sensitive_path = source / ".envrc"
                sensitive_path.write_text(
                    "tracked-sensitive-placeholder\n", encoding="utf-8"
                )
                git(source, "add", ".envrc")
                git(
                    source,
                    "-c",
                    "user.name=Bulkload Test",
                    "-c",
                    "user.email=bulkload@example.invalid",
                    "commit",
                    "-q",
                    "-m",
                    "tracked sensitive fixture",
                )
                git(destination, "fetch", "-q", "origin")
                git(destination, "reset", "-q", "--hard", "origin/main")

                if variant == "content":
                    sensitive_path.write_text(
                        "changed-sensitive-placeholder\n", encoding="utf-8"
                    )
                elif variant == "delete":
                    sensitive_path.unlink()
                elif variant == "mode":
                    sensitive_path.chmod(0o600)
                else:
                    sensitive_path.unlink()
                    sensitive_path.symlink_to("sensitive-target")

                source_a = capture_snapshot(source, "repo")
                source_b = capture_snapshot(source, "repo")
                record = next(
                    item
                    for item in source_b["catalog"][0]["files"]
                    if item["path"] == ".envrc"
                )

                self.assertEqual(record["status"]["worktree"], expected_status)
                self.assertEqual(record["kind"], "redacted")
                self.assertFalse(record["eligible"])
                for field in (
                    "git_blob_oid",
                    "index_entries",
                    "mode",
                    "sha256",
                    "size",
                ):
                    self.assertNotIn(field, record)
                serialized = canonical_bytes(source_b).decode()
                self.assertNotIn("changed-sensitive-placeholder", serialized)
                self.assertNotIn("sensitive-target", serialized)
                self.assertNotIn("_bulkload_private_", serialized)
                self.assertFalse(
                    any(field.startswith("_bulkload_private_") for field in record)
                )

                plan = compile_plan(
                    source_a, source_b, capture_snapshot(destination, "repo")
                )
                self.assertFalse(plan["intent"]["ready"])
                self.assertIn(
                    "source-path-ineligible",
                    {item["code"] for item in plan["intent"]["blockers"]},
                )

    def test_sensitive_variants_and_symlink_mutations_block(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, destination = make_pair(root)
            (source / ".envrc").write_text("placeholder\n", encoding="utf-8")
            (source / ".git-credentials").write_text("placeholder\n", encoding="utf-8")
            (source / ".ssh").mkdir()
            (source / ".ssh" / "custom-key").write_text(
                "placeholder\n", encoding="utf-8"
            )
            os.symlink("intermediate/file", source / "indirect-link")

            source_a = capture_snapshot(source, "repo")
            source_b = capture_snapshot(source, "repo")
            records = {item["path"]: item for item in source_b["catalog"][0]["files"]}
            for path in (
                ".envrc",
                ".git-credentials",
                ".ssh/custom-key",
                "indirect-link",
            ):
                self.assertFalse(records[path]["eligible"])
            self.assertEqual(records["indirect-link"]["kind"], "symlink")
            self.assertIn("unsupported", records["indirect-link"]["blocked_reason"])
            self.assertEqual(
                records["indirect-link"]["sha256"],
                sha256_bytes(b"intermediate/file"),
            )
            self.assertNotIn("link_target", records["indirect-link"])
            self.assertNotIn("intermediate/file", canonical_bytes(source_b).decode())

            plan = compile_plan(
                source_a, source_b, capture_snapshot(destination, "repo")
            )
            self.assertFalse(plan["intent"]["ready"])
            self.assertEqual(
                {item["path"] for item in plan["intent"]["blockers"]},
                {
                    ".envrc",
                    ".git-credentials",
                    ".ssh/custom-key",
                    "indirect-link",
                },
            )

    def test_tracked_credentials_appledouble_and_privileged_modes_block(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, destination = make_pair(root)
            (source / ".npmrc").write_text("placeholder\n", encoding="utf-8")
            git(source, "add", ".npmrc")
            git(
                source,
                "-c",
                "user.name=Bulkload Test",
                "-c",
                "user.email=bulkload@example.invalid",
                "commit",
                "-q",
                "-m",
                "credential-shaped fixture",
            )
            git(destination, "fetch", "-q", "origin")
            git(destination, "reset", "-q", "--hard", "origin/main")
            (source / ".npmrc").write_text("changed-placeholder\n", encoding="utf-8")
            (source / "._default.rules").write_text("metadata\n", encoding="utf-8")
            privileged = source / "helper"
            privileged.write_text("fixture\n", encoding="utf-8")
            privileged.chmod(0o4755)

            source_a = capture_snapshot(source, "repo")
            source_b = capture_snapshot(source, "repo")
            records = {item["path"]: item for item in source_b["catalog"][0]["files"]}
            for path in (".npmrc", "._default.rules"):
                self.assertFalse(records[path]["eligible"])
                self.assertNotIn("sha256", records[path])
            if privileged.stat().st_mode & 0o7000:
                self.assertFalse(records["helper"]["eligible"])
                self.assertNotIn("sha256", records["helper"])
            destination_snapshot = capture_snapshot(destination, "repo")
            self.assertTrue(destination_snapshot["complete"])
            plan = compile_plan(source_a, source_b, destination_snapshot)
            self.assertFalse(plan["intent"]["ready"])
            self.assertIn(
                "source-path-ineligible",
                {item["code"] for item in plan["intent"]["blockers"]},
            )

    def test_wrong_digest_and_traversal_fail_closed(self) -> None:
        with self.assertRaises(BulkloadError):
            normalize_relative("../escape")
        with self.assertRaisesRegex(BulkloadError, "control and format"):
            normalize_relative("review-\u202egnit.txt")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, destination = make_pair(root)
            (source / "notes.txt").write_text("new\n", encoding="utf-8")
            source_a = capture_snapshot(source, "repo")
            source_b = capture_snapshot(source, "repo")
            plan = compile_plan(
                source_a, source_b, capture_snapshot(destination, "repo")
            )
            with self.assertRaisesRegex(BulkloadError, "operator-supplied"):
                apply_plan(
                    plan,
                    source_root=source,
                    destination_root=destination,
                    accepted_digest="0" * 64,
                    state_root=root / "state",
                    receipt_path=root / "receipt.json",
                )
            self.assertFalse((destination / "notes.txt").exists())

    def test_apply_rejects_overlap_unplanned_dirt_and_state_receipt(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, destination = make_pair(root)
            (source / "notes.txt").write_text("new\n", encoding="utf-8")
            source_a = capture_snapshot(source, "repo")
            source_b = capture_snapshot(source, "repo")
            plan = compile_plan(
                source_a, source_b, capture_snapshot(destination, "repo")
            )
            with self.assertRaisesRegex(BulkloadError, "disjoint"):
                apply_plan(
                    plan,
                    source_root=source,
                    destination_root=source,
                    accepted_digest=plan["plan_sha256"],
                    state_root=root / "state-a",
                    receipt_path=root / "receipt-a.json",
                )
            with self.assertRaisesRegex(BulkloadError, "receipt"):
                apply_plan(
                    plan,
                    source_root=source,
                    destination_root=destination,
                    accepted_digest=plan["plan_sha256"],
                    state_root=root / "state-b",
                    receipt_path=root / "state-b" / "receipt.json",
                )
            (destination / "surprise.txt").write_text("unplanned\n", encoding="utf-8")
            with self.assertRaisesRegex(BulkloadError, "unplanned dirt"):
                apply_plan(
                    plan,
                    source_root=source,
                    destination_root=destination,
                    accepted_digest=plan["plan_sha256"],
                    state_root=root / "state-c",
                    receipt_path=root / "receipt-c.json",
                )

            other = root / "other-destination"
            subprocess.run(
                [
                    "git",
                    "-c",
                    "core.hooksPath=/dev/null",
                    "clone",
                    "-q",
                    str(source),
                    str(other),
                ],
                check=True,
            )
            with self.assertRaisesRegex(BulkloadError, "destination root"):
                apply_plan(
                    plan,
                    source_root=source,
                    destination_root=other,
                    accepted_digest=plan["plan_sha256"],
                    state_root=root / "state-d",
                    receipt_path=root / "receipt-d.json",
                )
            wrong_target = verify_plan(plan, source_b, plan["plan_sha256"])
            self.assertFalse(wrong_target["verified"])
            self.assertIn(
                "destination-root-mismatch",
                {failure["code"] for failure in wrong_target["failures"]},
            )

    def test_destination_staged_and_deleted_states_block_planning(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, destination = make_pair(root)
            (source / "tracked.txt").write_text("source\n", encoding="utf-8")
            (destination / "tracked.txt").write_text("destination\n", encoding="utf-8")
            git(destination, "add", "tracked.txt")
            plan = compile_plan(
                capture_snapshot(source, "repo"),
                capture_snapshot(source, "repo"),
                capture_snapshot(destination, "repo"),
            )
            validate_plan(plan)
            self.assertIn(
                "destination-staged-index-state",
                {item["code"] for item in plan["intent"]["blockers"]},
            )

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, destination = make_pair(root)
            (source / "tracked.txt").write_text("source\n", encoding="utf-8")
            (destination / "tracked.txt").unlink()
            plan = compile_plan(
                capture_snapshot(source, "repo"),
                capture_snapshot(source, "repo"),
                capture_snapshot(destination, "repo"),
            )
            validate_plan(plan)
            self.assertIn(
                "destination-path-ineligible",
                {item["code"] for item in plan["intent"]["blockers"]},
            )

    def test_plan_validation_blocks_git_admin_paths_and_wrong_acceptance(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, destination = make_pair(root)
            (source / "notes.txt").write_text("new\n", encoding="utf-8")
            plan = compile_plan(
                capture_snapshot(source, "repo"),
                capture_snapshot(source, "repo"),
                capture_snapshot(destination, "repo"),
            )
            with self.assertRaisesRegex(BulkloadError, "operator-supplied"):
                export_copy_paths(plan, "0" * 64)
            with self.assertRaisesRegex(BulkloadError, "operator-supplied"):
                verify_plan(plan, capture_snapshot(destination, "repo"), "0" * 64)

            malicious = json.loads(json.dumps(plan))
            malicious["intent"]["expected_files"][0]["path"] = (
                ".git/hooks/post-checkout"
            )
            malicious["intent"]["operations"][0]["path"] = ".git/hooks/post-checkout"
            malicious["plan_sha256"] = sha256_bytes(
                canonical_bytes(malicious["intent"])
            )
            malicious["envelope_sha256"] = object_digest(malicious, "envelope_sha256")
            with self.assertRaisesRegex(BulkloadError, "administration path"):
                export_copy_paths(malicious, malicious["plan_sha256"])

            privileged = json.loads(json.dumps(plan))
            privileged["intent"]["expected_files"][0]["identity"]["mode"] = "4755"
            privileged["intent"]["operations"][0]["after"]["mode"] = "4755"
            privileged["plan_sha256"] = sha256_bytes(
                canonical_bytes(privileged["intent"])
            )
            privileged["envelope_sha256"] = object_digest(privileged, "envelope_sha256")
            with self.assertRaisesRegex(BulkloadError, "privileged mode"):
                validate_plan(privileged)

            sensitive = json.loads(json.dumps(plan))
            sensitive_repository = sensitive["intent"]["expected_repositories"][0]
            sensitive_status = [{"index": "?", "path": ".envrc", "worktree": "?"}]
            sensitive_repository["dirty_paths"] = [".envrc"]
            sensitive_repository["status"] = sensitive_status
            sensitive_repository["status_sha256"] = sha256_bytes(
                canonical_bytes(sensitive_status)
            )
            sensitive["intent"]["expected_files"][0]["path"] = ".envrc"
            sensitive["intent"]["operations"][0]["path"] = ".envrc"
            sensitive["plan_sha256"] = sha256_bytes(
                canonical_bytes(sensitive["intent"])
            )
            sensitive["envelope_sha256"] = object_digest(sensitive, "envelope_sha256")
            with self.assertRaisesRegex(BulkloadError, "ineligible content path"):
                validate_plan(sensitive)

            symlink = json.loads(json.dumps(plan))
            symlink_identity = {
                "kind": "symlink",
                "link_target": "intermediate/file",
                "mode": "0777",
                "size": len("intermediate/file"),
            }
            symlink["intent"]["expected_files"][0]["identity"] = symlink_identity
            symlink["intent"]["operations"][0]["after"] = symlink_identity
            symlink["plan_sha256"] = sha256_bytes(canonical_bytes(symlink["intent"]))
            symlink["envelope_sha256"] = object_digest(symlink, "envelope_sha256")
            with self.assertRaisesRegex(BulkloadError, "symlink mutations"):
                validate_plan(symlink)

            omitted = json.loads(json.dumps(plan))
            omitted["intent"]["expected_files"] = []
            omitted["intent"]["operations"] = []
            omitted["plan_sha256"] = sha256_bytes(canonical_bytes(omitted["intent"]))
            omitted["envelope_sha256"] = object_digest(omitted, "envelope_sha256")
            with self.assertRaisesRegex(BulkloadError, "every dirty path"):
                validate_plan(omitted)

            invalid_destination = capture_snapshot(destination, "repo")
            invalid_destination["host"] = 7
            invalid_destination["snapshot_sha256"] = object_digest(
                invalid_destination, "snapshot_sha256"
            )
            with self.assertRaisesRegex(BulkloadError, "snapshot host"):
                compile_plan(
                    capture_snapshot(source, "repo"),
                    capture_snapshot(source, "repo"),
                    invalid_destination,
                )

    def test_snapshot_status_digest_and_file_bindings_are_recomputed(self) -> None:
        def redigest(snapshot: dict[str, object]) -> None:
            snapshot["catalog_sha256"] = sha256_bytes(
                canonical_bytes(snapshot["catalog"])
            )
            snapshot["snapshot_sha256"] = object_digest(snapshot, "snapshot_sha256")

        with tempfile.TemporaryDirectory() as directory:
            source, destination = make_pair(Path(directory))
            (source / "tracked.txt").write_text("changed\n", encoding="utf-8")
            source_a = capture_snapshot(source, "repo")
            source_b = capture_snapshot(source, "repo")
            for snapshot in (source_a, source_b):
                snapshot["catalog"][0]["status"] = []
                redigest(snapshot)
            with self.assertRaisesRegex(BulkloadError, "status_sha256 mismatch"):
                compile_plan(source_a, source_b, capture_snapshot(destination, "repo"))

            source_a = capture_snapshot(source, "repo")
            source_b = capture_snapshot(source, "repo")
            for snapshot in (source_a, source_b):
                file_record = next(
                    item
                    for item in snapshot["catalog"][0]["files"]
                    if item["path"] == "tracked.txt"
                )
                file_record["status"] = None
                redigest(snapshot)
            with self.assertRaisesRegex(BulkloadError, "file/status binding differs"):
                compile_plan(source_a, source_b, capture_snapshot(destination, "repo"))

            source_a = capture_snapshot(source, "repo")
            source_b = capture_snapshot(source, "repo")
            for snapshot in (source_a, source_b):
                duplicate = json.loads(json.dumps(snapshot["catalog"][0]["files"][0]))
                duplicate["git_class"] = "untracked"
                snapshot["catalog"][0]["files"].append(duplicate)
                redigest(snapshot)
            with self.assertRaisesRegex(BulkloadError, "duplicate snapshot file"):
                compile_plan(source_a, source_b, capture_snapshot(destination, "repo"))

    def test_capture_is_hook_suppressed_bounded_and_no_follow(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, destination = make_pair(root)
            marker = root / "fsmonitor-ran"
            hook = root / "fsmonitor-hook"
            hook.write_text(f"#!/bin/sh\ntouch '{marker}'\nexit 0\n", encoding="utf-8")
            hook.chmod(0o700)
            git(source, "config", "core.fsmonitor", str(hook))
            snapshot = capture_snapshot(source, "repo", max_bytes=1)
            self.assertFalse(snapshot["complete"])
            self.assertFalse(marker.exists())
            self.assertTrue(any("byte budget" in error for error in snapshot["errors"]))

            nested = source / "nested"
            nested.mkdir()
            (nested / "file.txt").write_text("inside\n", encoding="utf-8")
            git(source, "add", "nested/file.txt")
            git(
                source,
                "-c",
                "user.name=Bulkload Test",
                "-c",
                "user.email=bulkload@example.invalid",
                "commit",
                "-q",
                "-m",
                "nested",
            )
            git(destination, "fetch", "-q", "origin")
            shutil.rmtree(nested)
            outside = root / "outside"
            outside.mkdir()
            (outside / "file.txt").write_text("outside\n", encoding="utf-8")
            os.symlink(outside, nested)
            escaped = capture_snapshot(source, "repo")
            self.assertFalse(escaped["complete"])
            self.assertTrue(
                any("nested/file.txt" in error for error in escaped["errors"])
            )

    def test_assume_unchanged_cannot_hide_tracked_bytes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source, destination = make_pair(Path(directory))
            git(source, "update-index", "--assume-unchanged", "tracked.txt")
            (source / "tracked.txt").write_text("hidden-change\n", encoding="utf-8")
            self.assertEqual(git(source, "status", "--short"), "")
            snapshot = capture_snapshot(source, "repo")
            self.assertTrue(snapshot["complete"], snapshot["errors"])
            self.assertEqual(
                snapshot["catalog"][0]["status"],
                [{"index": " ", "path": "tracked.txt", "worktree": "M"}],
            )

    def test_capture_never_executes_git_content_filters(self) -> None:
        for driver, command in (
            ("probe-clean", "clean"),
            ("probe-process", "process"),
            ("lfs", "clean"),
        ):
            with self.subTest(driver=driver, command=command):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    source, _ = make_pair(root)
                    marker = root / "filter-executed"
                    script = root / "filter.sh"
                    script.write_text('#!/bin/sh\n: > "$1"\ncat\n', encoding="utf-8")
                    script.chmod(0o700)
                    (source / ".gitattributes").write_text(
                        f"tracked.txt filter={driver}\n", encoding="utf-8"
                    )
                    git(
                        source,
                        "config",
                        f"filter.{driver}.{command}",
                        f"{script} {marker}",
                    )
                    (source / "tracked.txt").write_text(
                        "same-size!\n", encoding="utf-8"
                    )
                    snapshot = capture_snapshot(source, "repo")
                    self.assertTrue(snapshot["complete"], snapshot["errors"])
                    self.assertFalse(marker.exists())
                    if driver == "lfs":
                        self.assertTrue(snapshot["catalog"][0]["has_lfs_attributes"])
                    else:
                        self.assertTrue(snapshot["catalog"][0]["has_content_filters"])

    def test_derived_status_blocks_staged_delete_and_rename(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source, destination = make_pair(Path(directory))
            git(source, "rm", "-q", "tracked.txt")
            source_a = capture_snapshot(source, "repo")
            source_b = capture_snapshot(source, "repo")
            self.assertEqual(
                source_b["catalog"][0]["status"],
                [{"index": "D", "path": "tracked.txt", "worktree": " "}],
            )
            plan = compile_plan(
                source_a, source_b, capture_snapshot(destination, "repo")
            )
            self.assertIn(
                "staged-index-state-unsupported",
                {item["code"] for item in plan["intent"]["blockers"]},
            )

        with tempfile.TemporaryDirectory() as directory:
            source, destination = make_pair(Path(directory))
            git(source, "mv", "tracked.txt", "renamed.txt")
            snapshot = capture_snapshot(source, "repo")
            self.assertEqual(
                [
                    (item["index"], item["path"])
                    for item in snapshot["catalog"][0]["status"]
                ],
                [("A", "renamed.txt"), ("D", "tracked.txt")],
            )

    def test_all_local_refs_are_cataloged_and_reconciled_one_way(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source, destination = make_pair(Path(directory))
            git(source, "update-ref", "refs/stash", "HEAD")
            source_a = capture_snapshot(source, "repo")
            source_b = capture_snapshot(source, "repo")
            source_repository = source_b["catalog"][0]
            self.assertIn(
                "refs/stash", {item["name"] for item in source_repository["refs"]}
            )
            self.assertEqual(
                source_repository["refs_sha256"],
                sha256_bytes(canonical_bytes(source_repository["refs"])),
            )
            self.assertEqual(
                source_repository["local_refs_sha256"],
                sha256_bytes(
                    canonical_bytes(
                        [
                            item
                            for item in source_repository["refs"]
                            if not item["name"].startswith("refs/remotes/")
                        ]
                    )
                ),
            )

            blocked = compile_plan(
                source_a, source_b, capture_snapshot(destination, "repo")
            )
            self.assertIn(
                "git-source-ref-missing-or-different",
                {item["code"] for item in blocked["intent"]["blockers"]},
            )

            git(destination, "update-ref", "refs/stash", "HEAD")
            git(destination, "update-ref", "refs/notes/destination-only", "HEAD")
            ready = compile_plan(
                capture_snapshot(source, "repo"),
                capture_snapshot(source, "repo"),
                capture_snapshot(destination, "repo"),
            )
            self.assertTrue(ready["intent"]["ready"], ready["intent"]["blockers"])
            self.assertIn(
                "destination-only-local-ref",
                {item["code"] for item in ready["intent"]["findings"]},
            )

            git(source, "symbolic-ref", "refs/heads/alias", "refs/heads/main")
            git(destination, "update-ref", "refs/heads/alias", "HEAD")
            source_with_alias = capture_snapshot(source, "repo")
            alias_record = next(
                item
                for item in source_with_alias["catalog"][0]["refs"]
                if item["name"] == "refs/heads/alias"
            )
            self.assertEqual(alias_record["symref"], "refs/heads/main")
            symbolic_mismatch = compile_plan(
                source_with_alias,
                capture_snapshot(source, "repo"),
                capture_snapshot(destination, "repo"),
            )
            self.assertIn(
                "git-source-ref-missing-or-different",
                {item["code"] for item in symbolic_mismatch["intent"]["blockers"]},
            )
            git(
                destination,
                "symbolic-ref",
                "refs/heads/alias",
                "refs/heads/main",
            )
            reconciled = compile_plan(
                capture_snapshot(source, "repo"),
                capture_snapshot(source, "repo"),
                capture_snapshot(destination, "repo"),
            )
            self.assertTrue(
                reconciled["intent"]["ready"], reconciled["intent"]["blockers"]
            )

            destination_head = git(destination, "rev-parse", "HEAD")
            git(
                destination,
                "update-ref",
                f"refs/replace/{destination_head}",
                destination_head,
            )
            replace_extra = compile_plan(
                capture_snapshot(source, "repo"),
                capture_snapshot(source, "repo"),
                capture_snapshot(destination, "repo"),
            )
            self.assertIn(
                "git-destination-replace-ref-extra",
                {item["code"] for item in replace_extra["intent"]["blockers"]},
            )

    def test_reflog_and_pseudo_ref_roots_require_explicit_destination_anchors(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source, destination = make_pair(Path(directory))
            base = git(source, "rev-parse", "HEAD")
            (source / "tracked.txt").write_text("reflog-only\n", encoding="utf-8")
            git(source, "add", "tracked.txt")
            git(
                source,
                "-c",
                "user.name=Bulkload Test",
                "-c",
                "user.email=bulkload@example.invalid",
                "commit",
                "-q",
                "-m",
                "reflog-only",
            )
            reflog_only = git(source, "rev-parse", "HEAD")
            git(source, "reset", "-q", "--hard", base)

            tree = git(source, "rev-parse", "HEAD^{tree}")
            pseudo_only = git(
                source,
                "-c",
                "user.name=Bulkload Test",
                "-c",
                "user.email=bulkload@example.invalid",
                "commit-tree",
                tree,
                "-m",
                "pseudo-only",
            )
            git(source, "update-ref", "ORIG_HEAD", pseudo_only)

            with mock.patch("bulkload_lib.scanner.MAX_RECOVERY_CANDIDATES", 1):
                over_budget = capture_snapshot(source, "repo")
            self.assertFalse(over_budget["complete"])
            self.assertTrue(
                any(
                    "recovery root budget exceeded" in error
                    for error in over_budget["errors"]
                ),
                over_budget["errors"],
            )

            source_a = capture_snapshot(source, "repo")
            source_b = capture_snapshot(source, "repo")
            recovery_roots = source_b["catalog"][0]["recovery_roots"]
            self.assertEqual(recovery_roots, sorted([pseudo_only, reflog_only]))
            blocked = compile_plan(
                source_a, source_b, capture_snapshot(destination, "repo")
            )
            recovery_blocker = next(
                item
                for item in blocked["intent"]["blockers"]
                if item["code"] == "git-recovery-roots-missing"
            )
            self.assertEqual(
                recovery_blocker["objects"], sorted([pseudo_only, reflog_only])
            )
            stripped = json.loads(json.dumps(blocked))
            stripped["intent"]["blockers"] = []
            stripped["intent"]["ready"] = True
            stripped["plan_sha256"] = sha256_bytes(canonical_bytes(stripped["intent"]))
            stripped["envelope_sha256"] = object_digest(stripped, "envelope_sha256")
            with self.assertRaisesRegex(
                BulkloadError, "destination lacks source recovery roots"
            ):
                validate_plan(stripped)

            for name, object_id in (
                ("reflog-export", reflog_only),
                ("pseudo-export", pseudo_only),
            ):
                git(source, "update-ref", f"refs/heads/{name}", object_id)
                git(
                    destination,
                    "fetch",
                    "-q",
                    str(source),
                    f"refs/heads/{name}:refs/heads/{name}",
                )
                git(source, "update-ref", "-d", f"refs/heads/{name}")

            retained_tree = git(destination, "rev-parse", "HEAD^{tree}")
            retainer = git(
                destination,
                "-c",
                "user.name=Bulkload Test",
                "-c",
                "user.email=bulkload@example.invalid",
                "commit-tree",
                retained_tree,
                "-p",
                reflog_only,
                "-p",
                pseudo_only,
                "-m",
                "retain source recovery closure",
            )
            git(destination, "update-ref", "refs/heads/recovery-retainer", retainer)
            git(destination, "update-ref", "-d", "refs/heads/reflog-export")
            git(destination, "update-ref", "-d", "refs/heads/pseudo-export")

            destination_snapshot = capture_snapshot(destination, "repo")
            ancestor_only = compile_plan(
                capture_snapshot(source, "repo"),
                capture_snapshot(source, "repo"),
                destination_snapshot,
            )
            self.assertIn(
                "git-recovery-roots-missing",
                {item["code"] for item in ancestor_only["intent"]["blockers"]},
            )

            git(
                destination,
                "update-ref",
                "refs/heads/bulkload-retain-reflog",
                reflog_only,
            )
            git(
                destination,
                "update-ref",
                "refs/heads/bulkload-retain-pseudo",
                pseudo_only,
            )
            ready = compile_plan(
                capture_snapshot(source, "repo"),
                capture_snapshot(source, "repo"),
                capture_snapshot(destination, "repo"),
            )
            self.assertTrue(ready["intent"]["ready"], ready["intent"]["blockers"])

    def test_tracked_permission_mode_is_migrated_and_verified_exactly(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, destination = make_pair(root)
            os.chmod(source / "tracked.txt", 0o600)
            source_a = capture_snapshot(source, "repo")
            source_b = capture_snapshot(source, "repo")
            source_repo = source_b["catalog"][0]
            self.assertEqual(
                source_repo["status"],
                [{"index": " ", "path": "tracked.txt", "worktree": "T"}],
            )
            plan = compile_plan(
                source_a, source_b, capture_snapshot(destination, "repo")
            )
            self.assertTrue(plan["intent"]["ready"], plan["intent"]["blockers"])
            self.assertEqual(plan["intent"]["operations"][0]["after"]["mode"], "0600")
            apply_plan(
                plan,
                source_root=source,
                destination_root=destination,
                accepted_digest=plan["plan_sha256"],
                state_root=root / "state",
                receipt_path=root / "receipt.json",
            )
            self.assertEqual(
                stat.S_IMODE((destination / "tracked.txt").stat().st_mode), 0o600
            )
            verified = verify_plan(
                plan, capture_snapshot(destination, "repo"), plan["plan_sha256"]
            )
            self.assertTrue(verified["verified"], verified["failures"])

            os.chmod(destination / "tracked.txt", 0o644)
            drifted = verify_plan(
                plan, capture_snapshot(destination, "repo"), plan["plan_sha256"]
            )
            self.assertFalse(drifted["verified"])
            self.assertIn(
                "file-identity-mismatch",
                {item["code"] for item in drifted["failures"]},
            )

    def test_replace_objects_are_neutralized_and_grafts_block(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source, destination = make_pair(Path(directory))
            original = git(source, "rev-parse", "HEAD")
            (source / "tracked.txt").write_text("replacement\n", encoding="utf-8")
            git(source, "add", "tracked.txt")
            git(
                source,
                "-c",
                "user.name=Bulkload Test",
                "-c",
                "user.email=bulkload@example.invalid",
                "commit",
                "-q",
                "-m",
                "replacement fixture",
            )
            replacement = git(source, "rev-parse", "HEAD")
            git(source, "reset", "-q", "--hard", original)
            git(source, "replace", original, replacement)

            source_a = capture_snapshot(source, "repo")
            source_b = capture_snapshot(source, "repo")
            repository = source_b["catalog"][0]
            self.assertEqual(repository["head"], original)
            self.assertEqual(repository["status"], [])
            self.assertIn(
                f"refs/replace/{original}",
                {item["name"] for item in repository["refs"]},
            )
            plan = compile_plan(
                source_a, source_b, capture_snapshot(destination, "repo")
            )
            self.assertIn(
                "git-source-ref-missing-or-different",
                {item["code"] for item in plan["intent"]["blockers"]},
            )

        with tempfile.TemporaryDirectory() as directory:
            source, destination = make_pair(Path(directory))
            grafts = source / ".git" / "info" / "grafts"
            grafts.parent.mkdir(parents=True, exist_ok=True)
            grafts.write_text(f"{git(source, 'rev-parse', 'HEAD')}\n", encoding="ascii")
            source_a = capture_snapshot(source, "repo")
            source_b = capture_snapshot(source, "repo")
            self.assertTrue(source_b["catalog"][0]["has_grafts"])
            plan = compile_plan(
                source_a, source_b, capture_snapshot(destination, "repo")
            )
            self.assertIn(
                "git-grafts-unsupported",
                {item["code"] for item in plan["intent"]["blockers"]},
            )

    def test_non_head_local_ref_reachable_objects_must_exist(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, destination = make_pair(root)
            git(source, "switch", "-q", "-c", "side")
            git(
                source,
                "-c",
                "user.name=Bulkload Test",
                "-c",
                "user.email=bulkload@example.invalid",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "side",
            )
            side = git(source, "rev-parse", "HEAD")
            git(source, "switch", "-q", "main")
            commit_payload = subprocess.run(
                ["git", "-C", str(source), "cat-file", "commit", side],
                stdout=subprocess.PIPE,
                check=True,
            ).stdout
            written = (
                subprocess.run(
                    [
                        "git",
                        "-C",
                        str(destination),
                        "hash-object",
                        "-t",
                        "commit",
                        "-w",
                        "--stdin",
                    ],
                    input=commit_payload,
                    stdout=subprocess.PIPE,
                    check=True,
                )
                .stdout.decode("ascii")
                .strip()
            )
            self.assertEqual(written, side)
            git(destination, "update-ref", "refs/heads/side", side)

            source_a = capture_snapshot(source, "repo")
            source_b = capture_snapshot(source, "repo")
            ready = compile_plan(
                source_a,
                source_b,
                capture_snapshot(destination, "repo"),
            )
            self.assertTrue(ready["intent"]["ready"], ready["intent"]["blockers"])

            object_path = destination / ".git" / "objects" / side[:2] / side[2:]
            self.assertTrue(object_path.is_file())
            object_path.rename(object_path.with_name(f"{object_path.name}.missing"))
            broken = capture_snapshot(destination, "repo")
            self.assertFalse(broken["complete"])
            self.assertTrue(
                any("rev-list" in error for error in broken["errors"]),
                broken["errors"],
            )
            with self.assertRaises(BulkloadError):
                compile_plan(source_a, source_b, broken)

    def test_object_store_integrity_is_verified(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source, _ = make_pair(Path(directory))
            base_blob = git(source, "rev-parse", "HEAD:tracked.txt")
            replacement_blob = (
                subprocess.run(
                    ["git", "-C", str(source), "hash-object", "-w", "--stdin"],
                    input=b"different object bytes\n",
                    stdout=subprocess.PIPE,
                    check=True,
                )
                .stdout.decode("ascii")
                .strip()
            )
            object_root = source / ".git" / "objects"
            base_path = object_root / base_blob[:2] / base_blob[2:]
            replacement_path = object_root / replacement_blob[:2] / replacement_blob[2:]
            corrupt = base_path.with_name(f"{base_path.name}.corrupt")
            corrupt.write_bytes(replacement_path.read_bytes())
            corrupt.replace(base_path)

            snapshot = capture_snapshot(source, "repo")
            self.assertFalse(snapshot["complete"])
            self.assertTrue(
                any("fsck" in error for error in snapshot["errors"]),
                snapshot["errors"],
            )

    def test_shallow_repository_is_an_incomplete_capture(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, _ = make_pair(root)
            for index in range(2):
                (source / "tracked.txt").write_text(
                    f"revision {index}\n", encoding="utf-8"
                )
                git(source, "add", "tracked.txt")
                git(
                    source,
                    "-c",
                    "user.name=Bulkload Test",
                    "-c",
                    "user.email=bulkload@example.invalid",
                    "commit",
                    "-q",
                    "-m",
                    f"revision {index}",
                )
            shallow = root / "shallow"
            subprocess.run(
                [
                    "git",
                    "-c",
                    "core.hooksPath=/dev/null",
                    "clone",
                    "-q",
                    "--depth",
                    "1",
                    source.as_uri(),
                    str(shallow),
                ],
                check=True,
            )
            self.assertEqual(
                git(source, "rev-parse", "HEAD"), git(shallow, "rev-parse", "HEAD")
            )

            source_a = capture_snapshot(source, "repo")
            source_b = capture_snapshot(source, "repo")
            shallow_snapshot = capture_snapshot(shallow, "repo")
            self.assertFalse(shallow_snapshot["complete"])
            self.assertTrue(
                any(
                    "shallow Git history" in error
                    for error in shallow_snapshot["errors"]
                ),
                shallow_snapshot["errors"],
            )
            with self.assertRaisesRegex(BulkloadError, "incomplete"):
                compile_plan(source_a, source_b, shallow_snapshot)

    def test_effective_promisor_configuration_blocks_before_object_access(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, destination = make_pair(root)
            git(destination, "config", "--local", "remote.origin.promisor", "true")
            object_root = destination / ".git" / "objects"
            before = sorted(
                path.relative_to(object_root).as_posix()
                for path in object_root.rglob("*")
                if path.is_file()
            )
            snapshot = capture_snapshot(destination, "repo")
            after = sorted(
                path.relative_to(object_root).as_posix()
                for path in object_root.rglob("*")
                if path.is_file()
            )
            self.assertFalse(snapshot["complete"])
            self.assertEqual(before, after)
            self.assertTrue(
                any("partial/promisor" in error for error in snapshot["errors"]),
                snapshot["errors"],
            )
            self.assertTrue(capture_snapshot(source, "repo")["complete"])

            git(destination, "config", "--local", "--unset", "remote.origin.promisor")
            isolated_home = root / "isolated-home"
            isolated_home.mkdir()
            global_config = isolated_home / ".gitconfig"
            for value in ("promisor = true", "promisor"):
                with self.subTest(global_promisor=value):
                    global_config.write_text(
                        f'[remote "origin"]\n\t{value}\n', encoding="utf-8"
                    )
                    with mock.patch.dict(
                        os.environ,
                        {
                            "HOME": str(isolated_home),
                            "XDG_CONFIG_HOME": str(isolated_home / ".config"),
                        },
                        clear=False,
                    ):
                        global_snapshot = capture_snapshot(destination, "repo")
                    self.assertFalse(global_snapshot["complete"])
                    self.assertTrue(
                        any(
                            "partial/promisor" in error
                            for error in global_snapshot["errors"]
                        ),
                        global_snapshot["errors"],
                    )

    def test_ref_change_after_planning_breaks_apply_and_verify(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, destination = make_pair(root)
            (source / "notes.txt").write_text("new\n", encoding="utf-8")
            plan = compile_plan(
                capture_snapshot(source, "repo"),
                capture_snapshot(source, "repo"),
                capture_snapshot(destination, "repo"),
            )
            self.assertTrue(plan["intent"]["ready"], plan["intent"]["blockers"])
            git(source, "update-ref", "refs/notes/late-source", "HEAD")
            with self.assertRaisesRegex(BulkloadError, "source Git local_refs changed"):
                apply_plan(
                    plan,
                    source_root=source,
                    destination_root=destination,
                    accepted_digest=plan["plan_sha256"],
                    state_root=root / "state",
                    receipt_path=root / "receipt.json",
                )
            self.assertFalse((destination / "notes.txt").exists())

            git(destination, "update-ref", "refs/notes/late-destination", "HEAD")
            verification = verify_plan(
                plan, capture_snapshot(destination, "repo"), plan["plan_sha256"]
            )
            self.assertFalse(verification["verified"])
            self.assertIn(
                "repository-local_refs_sha256-mismatch",
                {item["code"] for item in verification["failures"]},
            )

    def test_remote_tracking_ref_drift_does_not_block_apply_or_verify(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, destination = make_pair(root)
            (source / "notes.txt").write_text("new\n", encoding="utf-8")
            plan = compile_plan(
                capture_snapshot(source, "repo"),
                capture_snapshot(source, "repo"),
                capture_snapshot(destination, "repo"),
            )
            self.assertTrue(plan["intent"]["ready"], plan["intent"]["blockers"])

            git(source, "update-ref", "refs/remotes/transient/source", "HEAD")
            git(
                destination,
                "update-ref",
                "refs/remotes/transient/destination",
                "HEAD",
            )
            receipt = apply_plan(
                plan,
                source_root=source,
                destination_root=destination,
                accepted_digest=plan["plan_sha256"],
                state_root=root / "state",
                receipt_path=root / "receipt.json",
            )
            self.assertEqual(receipt["operations"][0]["result"], "copied")

            git(destination, "update-ref", "refs/remotes/transient/later", "HEAD")
            verification = verify_plan(
                plan, capture_snapshot(destination, "repo"), plan["plan_sha256"]
            )
            self.assertTrue(verification["verified"], verification["failures"])

    def test_apply_rejects_self_digested_plan_for_staged_source(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, destination = make_pair(root)
            (source / "tracked.txt").write_text("changed\n", encoding="utf-8")
            plan = compile_plan(
                capture_snapshot(source, "repo"),
                capture_snapshot(source, "repo"),
                capture_snapshot(destination, "repo"),
            )
            self.assertTrue(plan["intent"]["ready"], plan["intent"]["blockers"])

            git(source, "add", "tracked.txt")
            staged = capture_snapshot(source, "repo")["catalog"][0]
            crafted = json.loads(json.dumps(plan))
            source_precondition = crafted["intent"]["expected_repositories"][0]
            source_precondition["status"] = staged["status"]
            source_precondition["status_sha256"] = staged["status_sha256"]
            crafted["plan_sha256"] = sha256_bytes(canonical_bytes(crafted["intent"]))
            crafted["envelope_sha256"] = object_digest(crafted, "envelope_sha256")
            validate_plan(crafted)
            with self.assertRaisesRegex(
                BulkloadError, "source index or conflict state is unsafe"
            ):
                apply_plan(
                    crafted,
                    source_root=source,
                    destination_root=destination,
                    accepted_digest=crafted["plan_sha256"],
                    state_root=root / "state",
                    receipt_path=root / "receipt.json",
                )
            self.assertEqual((destination / "tracked.txt").read_text(), "base\n")

    def test_stripped_cross_git_blockers_fail_validation_or_preflight(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, destination = make_pair(root)
            (source / "tracked.txt").write_text("ahead\n", encoding="utf-8")
            git(source, "add", "tracked.txt")
            git(
                source,
                "-c",
                "user.name=Bulkload Test",
                "-c",
                "user.email=bulkload@example.invalid",
                "commit",
                "-q",
                "-m",
                "ahead",
            )
            (source / "notes.txt").write_text("new\n", encoding="utf-8")
            plan = compile_plan(
                capture_snapshot(source, "repo"),
                capture_snapshot(source, "repo"),
                capture_snapshot(destination, "repo"),
            )
            self.assertIn(
                "git-head-mismatch",
                {item["code"] for item in plan["intent"]["blockers"]},
            )

            stripped = json.loads(json.dumps(plan))
            stripped["intent"]["blockers"] = []
            stripped["intent"]["ready"] = True
            stripped["plan_sha256"] = sha256_bytes(canonical_bytes(stripped["intent"]))
            stripped["envelope_sha256"] = object_digest(stripped, "envelope_sha256")
            with self.assertRaisesRegex(BulkloadError, "head differ"):
                validate_plan(stripped)

            forged = json.loads(json.dumps(stripped))
            source_expected = forged["intent"]["expected_repositories"][0]
            destination_expected = forged["intent"]["destination_repositories_before"][
                0
            ]
            for field in (
                "branch",
                "head",
                "local_refs",
                "local_refs_sha256",
            ):
                destination_expected[field] = source_expected[field]
            forged["plan_sha256"] = sha256_bytes(canonical_bytes(forged["intent"]))
            forged["envelope_sha256"] = object_digest(forged, "envelope_sha256")
            validate_plan(forged)
            with self.assertRaisesRegex(BulkloadError, "destination Git head changed"):
                apply_plan(
                    forged,
                    source_root=source,
                    destination_root=destination,
                    accepted_digest=forged["plan_sha256"],
                    state_root=root / "state",
                    receipt_path=root / "receipt.json",
                )
            self.assertFalse((destination / "notes.txt").exists())

        with tempfile.TemporaryDirectory() as directory:
            source, destination = make_pair(Path(directory))
            head = git(destination, "rev-parse", "HEAD")
            git(destination, "update-ref", f"refs/replace/{head}", head)
            plan = compile_plan(
                capture_snapshot(source, "repo"),
                capture_snapshot(source, "repo"),
                capture_snapshot(destination, "repo"),
            )
            stripped = json.loads(json.dumps(plan))
            stripped["intent"]["blockers"] = []
            stripped["intent"]["ready"] = True
            stripped["plan_sha256"] = sha256_bytes(canonical_bytes(stripped["intent"]))
            stripped["envelope_sha256"] = object_digest(stripped, "envelope_sha256")
            with self.assertRaisesRegex(BulkloadError, "extra replacement refs"):
                validate_plan(stripped)

        with tempfile.TemporaryDirectory() as directory:
            source, destination = make_pair(Path(directory))
            (destination / "tracked.txt").write_text(
                "precious destination work\n", encoding="utf-8"
            )
            plan = compile_plan(
                capture_snapshot(source, "repo"),
                capture_snapshot(source, "repo"),
                capture_snapshot(destination, "repo"),
            )
            self.assertIn(
                "destination-only-dirt",
                {item["code"] for item in plan["intent"]["blockers"]},
            )
            stripped = json.loads(json.dumps(plan))
            stripped["intent"]["blockers"] = []
            stripped["intent"]["ready"] = True
            stripped["plan_sha256"] = sha256_bytes(canonical_bytes(stripped["intent"]))
            stripped["envelope_sha256"] = object_digest(stripped, "envelope_sha256")
            with self.assertRaisesRegex(BulkloadError, "dirt absent from source"):
                validate_plan(stripped)

            forged = json.loads(json.dumps(stripped))
            source_expected = forged["intent"]["expected_repositories"][0]
            source_expected["dirty_paths"] = ["tracked.txt"]
            forged["plan_sha256"] = sha256_bytes(canonical_bytes(forged["intent"]))
            forged["envelope_sha256"] = object_digest(forged, "envelope_sha256")
            with self.assertRaisesRegex(BulkloadError, "differ from status paths"):
                validate_plan(forged)

    def test_cli_evidence_outputs_cannot_dirty_captured_roots(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            source, destination = make_pair(root)
            source_a = capture_snapshot(source, "repo")
            source_b = capture_snapshot(source, "repo")
            destination_before = capture_snapshot(destination, "repo")
            plan = compile_plan(source_a, source_b, destination_before)
            evidence = root / "evidence"
            evidence.mkdir()
            source_a_path = evidence / "source-a.json"
            source_b_path = evidence / "source-b.json"
            destination_path = evidence / "destination.json"
            plan_path = evidence / "plan.json"
            for path, value in (
                (source_a_path, source_a),
                (source_b_path, source_b),
                (destination_path, destination_before),
                (plan_path, plan),
            ):
                atomic_write_json(path, value)

            with redirect_stderr(io.StringIO()):
                capture_exit = cli_main(
                    [
                        "capture",
                        "--mode",
                        "repo",
                        "--root",
                        str(source),
                        "--output",
                        str(source / "snapshot.json"),
                    ]
                )
                plan_exit = cli_main(
                    [
                        "plan",
                        "--source-a",
                        str(source_a_path),
                        "--source-b",
                        str(source_b_path),
                        "--destination",
                        str(destination_path),
                        "--output",
                        str(source / "plan.json"),
                    ]
                )
                verify_exit = cli_main(
                    [
                        "verify",
                        "--plan",
                        str(plan_path),
                        "--accept-plan",
                        plan["plan_sha256"],
                        "--destination",
                        str(destination_path),
                        "--output",
                        str(destination / "verification.json"),
                    ]
                )
            self.assertEqual((capture_exit, plan_exit, verify_exit), (2, 2, 2))
            self.assertFalse((source / "snapshot.json").exists())
            self.assertFalse((source / "plan.json").exists())
            self.assertFalse((destination / "verification.json").exists())

    def test_cli_evidence_outputs_cannot_overwrite_inputs(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            source, destination = make_pair(root)
            (source / "notes.txt").write_text("new\n", encoding="utf-8")
            source_a = capture_snapshot(source, "repo")
            source_b = capture_snapshot(source, "repo")
            destination_before = capture_snapshot(destination, "repo")
            plan = compile_plan(source_a, source_b, destination_before)
            evidence = root / "evidence"
            evidence.mkdir()
            source_a_path = evidence / "source-a.json"
            source_b_path = evidence / "source-b.json"
            destination_path = evidence / "destination.json"
            plan_path = evidence / "plan.json"
            for path, value in (
                (source_a_path, source_a),
                (source_b_path, source_b),
                (destination_path, destination_before),
                (plan_path, plan),
            ):
                atomic_write_json(path, value)
            before = {
                path: path.read_bytes()
                for path in (
                    source_a_path,
                    source_b_path,
                    destination_path,
                    plan_path,
                )
            }

            with redirect_stderr(io.StringIO()):
                results = (
                    cli_main(
                        [
                            "plan",
                            "--source-a",
                            str(source_a_path),
                            "--source-b",
                            str(source_b_path),
                            "--destination",
                            str(destination_path),
                            "--output",
                            str(source_a_path),
                        ]
                    ),
                    cli_main(
                        [
                            "verify",
                            "--plan",
                            str(plan_path),
                            "--accept-plan",
                            plan["plan_sha256"],
                            "--destination",
                            str(destination_path),
                            "--output",
                            str(plan_path),
                        ]
                    ),
                    cli_main(
                        [
                            "verify",
                            "--plan",
                            str(plan_path),
                            "--accept-plan",
                            plan["plan_sha256"],
                            "--destination",
                            str(destination_path),
                            "--output",
                            str(destination_path),
                        ]
                    ),
                    cli_main(
                        [
                            "apply",
                            "--plan",
                            str(plan_path),
                            "--accept-plan",
                            plan["plan_sha256"],
                            "--source-root",
                            str(source),
                            "--destination-root",
                            str(destination),
                            "--state-root",
                            str(root / "state"),
                            "--receipt",
                            str(plan_path),
                        ]
                    ),
                )
            self.assertEqual(results, (2, 2, 2, 2))
            self.assertEqual(before, {path: path.read_bytes() for path in before})
            self.assertFalse((destination / "notes.txt").exists())

    def test_ambient_git_index_override_is_ignored(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, _ = make_pair(root)
            alternate_index = root / "alternate.index"
            subprocess.run(
                ["git", "-C", str(source), "read-tree", "--empty"],
                env={**os.environ, "GIT_INDEX_FILE": str(alternate_index)},
                check=True,
            )
            with mock.patch.dict(
                os.environ, {"GIT_INDEX_FILE": str(alternate_index)}, clear=False
            ):
                snapshot = capture_snapshot(source, "repo")
            self.assertTrue(snapshot["complete"], snapshot["errors"])
            self.assertIn(
                "tracked.txt",
                {item["path"] for item in snapshot["catalog"][0]["files"]},
            )

    def test_clean_stopped_rebase_is_typed_and_blocks_v1(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source, destination = make_pair(Path(directory))
            git(source, "switch", "-q", "-c", "topic")
            git(
                source,
                "-c",
                "user.name=Bulkload Test",
                "-c",
                "user.email=bulkload@example.invalid",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "topic",
            )
            stopped = subprocess.run(
                [
                    "git",
                    "-c",
                    "core.hooksPath=/dev/null",
                    "-C",
                    str(source),
                    "rebase",
                    "--exec",
                    "false",
                    "main",
                ],
                env={**os.environ, "GIT_EDITOR": "true"},
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
            )
            self.assertNotEqual(stopped.returncode, 0)
            self.assertEqual(git(source, "status", "--porcelain"), "")
            self.assertTrue((source / ".git/rebase-merge").is_dir())

            snapshot = capture_snapshot(source, "repo")
            self.assertFalse(snapshot["complete"])
            self.assertEqual(snapshot["catalog"][0]["git_operation_state"], ["rebase"])
            self.assertTrue(
                any(
                    "active Git operation state" in error
                    for error in snapshot["errors"]
                ),
                snapshot["errors"],
            )
            with self.assertRaisesRegex(BulkloadError, "active Git operation state"):
                validate_snapshot(snapshot)
            with self.assertRaisesRegex(BulkloadError, "runtime capture is incomplete"):
                capture_git_runtime(source)
            with self.assertRaises(BulkloadError):
                compile_plan(
                    snapshot,
                    capture_snapshot(source, "repo"),
                    capture_snapshot(destination, "repo"),
                )

    def test_effective_lfs_attributes_block_without_comment_false_positive(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, destination = make_pair(root)
            assets = source / "assets"
            assets.mkdir()
            (assets / ".gitattributes").write_text(
                "*.bin filter=lfs\n", encoding="utf-8"
            )
            (assets / "payload.bin").write_bytes(b"fixture\n")
            source_a = capture_snapshot(source, "repo")
            source_b = capture_snapshot(source, "repo")
            self.assertTrue(source_b["catalog"][0]["has_lfs_attributes"])
            plan = compile_plan(
                source_a, source_b, capture_snapshot(destination, "repo")
            )
            self.assertIn(
                "git-lfs-requires-separate-plan",
                {item["code"] for item in plan["intent"]["blockers"]},
            )

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, _ = make_pair(root)
            (source / ".gitattributes").write_text(
                "# *.bin filter=lfs\n", encoding="utf-8"
            )
            (source / "payload.bin").write_bytes(b"fixture\n")
            snapshot = capture_snapshot(source, "repo")
            self.assertFalse(snapshot["catalog"][0]["has_lfs_attributes"])

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            source, _ = make_pair(root)
            (source / ".git" / "info").mkdir(parents=True, exist_ok=True)
            (source / ".git" / "info" / "attributes").write_text(
                "*.bin filter=lfs\n", encoding="utf-8"
            )
            (source / "payload.bin").write_bytes(b"fixture\n")
            snapshot = capture_snapshot(source, "repo")
            repository = snapshot["catalog"][0]
            self.assertFalse(repository["has_lfs_attributes"])
            self.assertTrue(repository["has_unportable_attributes"])

    def test_remote_redaction_and_duplicate_json_rejection(self) -> None:
        self.assertEqual(
            sanitize_remote_url(
                "https://user:token@example.com/repo.git?token=secret#fragment"
            ),
            "https://example.com/repo.git",
        )
        self.assertEqual(
            sanitize_remote_url(
                "oauth2:token@git.example.com:owner/repo.git?secret=yes"
            ),
            "git.example.com:owner/repo.git",
        )
        local_remote = "/private/example/repo.git"
        sanitized_local = sanitize_remote_url(local_remote)
        self.assertTrue(sanitized_local.startswith("local-path:sha256:"))
        self.assertNotIn(local_remote, sanitized_local)
        colon_local = "/private/example:repo.git"
        sanitized_colon_local = sanitize_remote_url(colon_local)
        self.assertTrue(sanitized_colon_local.startswith("local-path:sha256:"))
        self.assertNotIn(colon_local, sanitized_colon_local)
        file_remote = "file:///private/example/repo.git?token=secret"
        sanitized_file_remote = sanitize_remote_url(file_remote)
        self.assertTrue(sanitized_file_remote.startswith("local-path:sha256:"))
        self.assertNotIn("private", sanitized_file_remote)
        self.assertNotIn("secret", sanitized_file_remote)
        for locator in (
            "ext::ssh -o SyntheticMarker=DO_NOT_SERIALIZE host %S repo",
            "foo::https://user:secret@example.invalid/repo.git",
            "user:secret@host/path",
            "custom://user:secret@example.invalid/repo.git",
        ):
            with self.subTest(locator=locator):
                with self.assertRaisesRegex(BulkloadError, "Git remote"):
                    sanitize_remote_url(locator)

        with tempfile.TemporaryDirectory() as directory:
            source, destination = make_pair(Path(directory))
            git(
                source,
                "remote",
                "add",
                "helper",
                "ext::ssh -o SyntheticMarker=DO_NOT_SERIALIZE host %S repo",
            )
            snapshot = capture_snapshot(source, "repo")
            serialized = canonical_bytes(snapshot).decode("utf-8")
            self.assertFalse(snapshot["complete"])
            self.assertNotIn("DO_NOT_SERIALIZE", serialized)

            valid = capture_snapshot(destination, "repo")
            forged = json.loads(json.dumps(valid))
            forged["catalog"][0]["remotes"][0]["urls"] = [
                "ext::ssh -o SyntheticMarker=DO_NOT_SERIALIZE host %S repo"
            ]
            forged["catalog_sha256"] = sha256_bytes(canonical_bytes(forged["catalog"]))
            forged["snapshot_sha256"] = object_digest(forged, "snapshot_sha256")
            with self.assertRaisesRegex(BulkloadError, "Git remote"):
                validate_snapshot(forged)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "duplicate.json"
            path.write_text('{"value":1,"value":2}\n', encoding="utf-8")
            with self.assertRaisesRegex(BulkloadError, "duplicate JSON key"):
                read_json(path)

    def test_fleet_discovery(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            make_pair(root / "one")
            make_pair(root / "two")
            snapshot = capture_snapshot(root, "fleet")
            self.assertTrue(snapshot["complete"], snapshot["errors"])
            self.assertEqual(
                [item["logical_path"] for item in snapshot["catalog"]],
                ["one/destination", "one/source", "two/destination", "two/source"],
            )
            forged = json.loads(json.dumps(snapshot))
            forged["catalog"][1]["logical_path"] = "one//destination"
            forged["catalog_sha256"] = sha256_bytes(canonical_bytes(forged["catalog"]))
            forged["snapshot_sha256"] = object_digest(forged, "snapshot_sha256")
            with self.assertRaisesRegex(BulkloadError, "non-canonical"):
                validate_snapshot(forged)

    def test_fleet_bare_and_symlink_git_authority_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, _ = make_pair(root / "pair")
            subprocess.run(
                [
                    "git",
                    "clone",
                    "-q",
                    "--bare",
                    str(source),
                    str(root / "archive.git"),
                ],
                check=True,
            )
            alias = root / "alias"
            alias.mkdir()
            os.symlink(source / ".git", alias / ".git", target_is_directory=True)
            special = root / "special"
            special.mkdir()
            os.mkfifo(special / ".git")

            snapshot = capture_snapshot(root, "fleet")
            self.assertFalse(snapshot["complete"])
            self.assertTrue(
                any(
                    "bare Git repository is unsupported" in error
                    for error in snapshot["errors"]
                ),
                snapshot["errors"],
            )
            self.assertTrue(
                any(
                    "symlink .git authority is unsupported" in error
                    for error in snapshot["errors"]
                ),
                snapshot["errors"],
            )
            self.assertTrue(
                any(
                    "special .git authority is unsupported" in error
                    for error in snapshot["errors"]
                ),
                snapshot["errors"],
            )
            logical_paths = {item["logical_path"] for item in snapshot["catalog"]}
            self.assertNotIn("archive.git", logical_paths)
            self.assertNotIn("alias", logical_paths)
            self.assertNotIn("special", logical_paths)

            with self.assertRaisesRegex(BulkloadError, "symlink .git authority"):
                capture_snapshot(alias, "repo")

    def test_non_utf8_git_metadata_is_an_incomplete_exit_three_capture(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, _ = make_pair(root)
            with (source / ".git" / "config").open("ab") as config:
                config.write(
                    b'\n[remote "invalid-\xff"]\n\turl = https://example.invalid/repo.git\n'
                )

            snapshot = capture_snapshot(source, "repo")
            self.assertFalse(snapshot["complete"])
            self.assertTrue(
                any("non-UTF-8 Git metadata" in error for error in snapshot["errors"]),
                snapshot["errors"],
            )
            self.assertNotIn(
                "UnicodeDecodeError", canonical_bytes(snapshot).decode("utf-8")
            )

            output = root / "capture.json"
            stderr = io.StringIO()
            with redirect_stderr(stderr), mock.patch("sys.stdout", new=io.StringIO()):
                result = cli_main(
                    [
                        "capture",
                        "--root",
                        str(source),
                        "--mode",
                        "repo",
                        "--output",
                        str(output),
                    ]
                )
            self.assertEqual(result, 3)
            self.assertEqual(stderr.getvalue(), "")
            written = read_json(output)
            self.assertFalse(written["complete"])
            self.assertTrue(
                any("non-UTF-8 Git metadata" in error for error in written["errors"])
            )

    def test_relative_path_aliases_are_rejected(self) -> None:
        for value in ("a//b", "a/./b", "a/b/", "./a"):
            with self.subTest(value=value):
                with self.assertRaisesRegex(BulkloadError, "non-canonical"):
                    normalize_relative(value)

    def test_fleet_root_repository_uses_dot_identity(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, destination = make_pair(root)
            (source / "tracked.txt").write_text("changed\n", encoding="utf-8")
            source_a = capture_snapshot(source, "fleet")
            source_b = capture_snapshot(source, "fleet")
            destination_before = capture_snapshot(destination, "fleet")
            self.assertEqual(source_b["catalog"][0]["logical_path"], ".")
            plan = compile_plan(source_a, source_b, destination_before)
            self.assertTrue(plan["intent"]["ready"], plan["intent"]["blockers"])
            self.assertEqual(
                export_copy_paths(plan, plan["plan_sha256"]), ["tracked.txt"]
            )

    def test_staging_allowlist_includes_dirty_files_with_no_operations(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, destination = make_pair(root)
            for repository in (source, destination):
                (repository / "tracked.txt").write_text(
                    "same-dirty\n", encoding="utf-8"
                )
                (repository / "notes.txt").write_text("same-new\n", encoding="utf-8")
            plan = compile_plan(
                capture_snapshot(source, "repo"),
                capture_snapshot(source, "repo"),
                capture_snapshot(destination, "repo"),
            )
            self.assertTrue(plan["intent"]["ready"], plan["intent"]["blockers"])
            self.assertEqual(plan["intent"]["operations"], [])
            self.assertEqual(
                export_copy_paths(plan, plan["plan_sha256"]),
                ["notes.txt", "tracked.txt"],
            )

            staging = root / "staging"
            subprocess.run(
                ["git", "clone", "-q", str(source), str(staging)], check=True
            )
            for relative in export_copy_paths(plan, plan["plan_sha256"]):
                shutil.copy2(source / relative, staging / relative)
            receipt = apply_plan(
                plan,
                source_root=staging,
                destination_root=destination,
                accepted_digest=plan["plan_sha256"],
                state_root=root / "state",
                receipt_path=root / "receipt.json",
            )
            self.assertEqual(receipt["operations"], [])

    def test_durable_directory_creation_and_journal_failure_boundary(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            with (
                mock.patch("bulkload_lib.model.os.mkdir", wraps=os.mkdir) as mkdir_spy,
                mock.patch("bulkload_lib.model.os.fsync", wraps=os.fsync) as fsync_spy,
            ):
                durable_makedirs(root / "one" / "two")
            self.assertEqual(mkdir_spy.call_count, 2)
            self.assertEqual(fsync_spy.call_count, 4)

            real = root / "real"
            real.mkdir()
            os.symlink(real, root / "linked")
            with self.assertRaisesRegex(BulkloadError, "real directory"):
                durable_makedirs(root / "linked" / "child")

            evidence_target = root / "evidence-target"
            evidence_target.mkdir()
            os.symlink(evidence_target, root / "evidence-link")
            atomic_write(root / "evidence-link" / "result.json", b"{}\n")
            self.assertEqual((evidence_target / "result.json").read_bytes(), b"{}\n")

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, destination = make_pair(root)
            (source / "notes.txt").write_text("new\n", encoding="utf-8")
            plan = compile_plan(
                capture_snapshot(source, "repo"),
                capture_snapshot(source, "repo"),
                capture_snapshot(destination, "repo"),
            )
            with mock.patch(
                "bulkload_lib.executor._fsync_directory",
                side_effect=BulkloadError("injected journal fsync failure"),
            ):
                with self.assertRaisesRegex(BulkloadError, "journal fsync"):
                    apply_plan(
                        plan,
                        source_root=source,
                        destination_root=destination,
                        accepted_digest=plan["plan_sha256"],
                        state_root=root / "state",
                        receipt_path=root / "receipt.json",
                    )
            self.assertFalse((destination / "notes.txt").exists())

    def test_fleet_does_not_prune_repo_named_target_and_detects_collisions(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, _ = make_pair(root / "target")
            (source / "ss.txt").write_text("one\n", encoding="utf-8")
            (source / "ß.txt").write_text("two\n", encoding="utf-8")
            snapshot = capture_snapshot(root, "fleet")
            self.assertIn(
                "target/source", [item["logical_path"] for item in snapshot["catalog"]]
            )
            self.assertEqual(
                _path_collisions(["ss.txt", "ß.txt"]), [["ss.txt", "ß.txt"]]
            )

    def test_fleet_walk_errors_make_snapshot_incomplete(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            make_pair(root / "repo")
            real_walk = os.walk

            def walk_with_error(path: Path, *, followlinks: bool, onerror: object):
                assert callable(onerror)
                onerror(PermissionError(13, "denied", str(root / "unreadable")))
                yield from real_walk(path, followlinks=followlinks, onerror=onerror)

            with mock.patch(
                "bulkload_lib.scanner.os.walk", side_effect=walk_with_error
            ):
                snapshot = capture_snapshot(root, "fleet")
            self.assertFalse(snapshot["complete"])
            self.assertTrue(any("walk error" in error for error in snapshot["errors"]))

    def test_fleet_plan_with_no_matching_repositories_is_valid_but_blocked(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source_root = root / "source-fleet"
            destination_root = root / "destination-fleet"
            make_pair(source_root / "one")
            make_pair(destination_root / "two")
            plan = compile_plan(
                capture_snapshot(source_root, "fleet"),
                capture_snapshot(source_root, "fleet"),
                capture_snapshot(destination_root, "fleet"),
            )
            validate_plan(plan)
            self.assertFalse(plan["intent"]["ready"])
            self.assertIn(
                "destination-repository-missing",
                {blocker["code"] for blocker in plan["intent"]["blockers"]},
            )


if __name__ == "__main__":
    run_unittest_main()
