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
from tests.unprivileged_test_main import run_unittest_main  # noqa: E402


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


class BulkloadProtocolTest(unittest.TestCase):
    def test_mutation_suite_runs_as_an_unprivileged_user(self) -> None:
        if hasattr(os, "geteuid"):
            self.assertNotEqual(os.geteuid(), 0)

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
