from __future__ import annotations

from copy import deepcopy
import hashlib
from pathlib import Path
import stat
import tempfile
import unittest

from bulkload_lib import private_runtime
from bulkload_lib import private_sqlite_verifier as verifier
from bulkload_lib.model import (
    BulkloadError,
    canonical_bytes,
    object_digest,
    sha256_bytes,
)
from bulkload_lib.private_sqlite_protocol import validate_verifier_oracle_report
from tests.private_sqlite_public_oracle_fixtures import (
    ORACLE_FALSE_CLAIMS,
    ORACLE_OBSERVED_CHECKS,
    build_public_oracle_fixture,
    tamper_candidate_payload_and_redigest,
)


def _tree_snapshot(root: Path) -> dict[str, tuple[object, ...]]:
    result: dict[str, tuple[object, ...]] = {}
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


def _document_snapshot(*values: dict[str, object]) -> tuple[bytes, ...]:
    return tuple(canonical_bytes(value) for value in values)


class CodexPrivateSqlitePublicOracleAcceptanceTest(unittest.TestCase):
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

    def test_complete_frozen_chain_is_observed_without_mutation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = build_public_oracle_fixture(Path(directory))
            runtime_authority = private_runtime.current_private_runtime_authority()
            documents = (
                fixture.action_plan,
                fixture.opening_plan,
                fixture.compose_request,
                fixture.capacity_observation,
                runtime_authority,
            )
            before_documents = _document_snapshot(*documents)
            before_tree = _tree_snapshot(fixture.root)

            report = verifier.observe_codex_private_sqlite_bundle(
                fixture.bundle_path,
                fixture.action_plan,
                fixture.opening_plan,
                fixture.compose_request,
                fixture.capacity_observation,
                verifier_runtime_authority=runtime_authority,
            )

            self.assertEqual(_document_snapshot(*documents), before_documents)
            self.assertEqual(_tree_snapshot(fixture.root), before_tree)
            self.assertEqual(report["failures"], [])
            self.assertEqual(set(report["observed_checks"]), ORACLE_OBSERVED_CHECKS)
            self.assertTrue(all(report["observed_checks"].values()))
            self.assertEqual(set(report["claims"]), ORACLE_FALSE_CLAIMS)
            self.assertTrue(all(value is False for value in report["claims"].values()))
            self.assertTrue(
                all(
                    value is True
                    for family in report["families"]
                    for key, value in family.items()
                    if key != "observed"
                )
            )
            validate_verifier_oracle_report(report)

    def test_exact_request_chain_tamper_refuses_without_mutation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = build_public_oracle_fixture(Path(directory))
            runtime_authority = private_runtime.current_private_runtime_authority()
            tampered = deepcopy(fixture.compose_request)
            tampered["action_plan"]["body_sha256"] = "0" * 64
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
            before_tampered = canonical_bytes(tampered)
            before_original = canonical_bytes(fixture.compose_request)
            before_tree = _tree_snapshot(fixture.root)

            with self.assertRaisesRegex(BulkloadError, "exact inputs"):
                verifier.observe_codex_private_sqlite_bundle(
                    fixture.bundle_path,
                    fixture.action_plan,
                    fixture.opening_plan,
                    tampered,
                    fixture.capacity_observation,
                    verifier_runtime_authority=runtime_authority,
                )

            self.assertEqual(canonical_bytes(tampered), before_tampered)
            self.assertEqual(
                canonical_bytes(fixture.compose_request),
                before_original,
            )
            self.assertEqual(
                runtime_authority,
                private_runtime.current_private_runtime_authority(),
            )
            self.assertEqual(_tree_snapshot(fixture.root), before_tree)

    def test_self_redigested_candidate_tamper_is_observed_without_mutation(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = build_public_oracle_fixture(Path(directory))
            tamper_candidate_payload_and_redigest(
                fixture,
                b"wrong-but-self-redigested",
            )
            runtime_authority = private_runtime.current_private_runtime_authority()
            documents = (
                fixture.action_plan,
                fixture.opening_plan,
                fixture.compose_request,
                fixture.capacity_observation,
                runtime_authority,
            )
            before_documents = _document_snapshot(*documents)
            before_tree = _tree_snapshot(fixture.root)

            report = verifier.observe_codex_private_sqlite_bundle(
                fixture.bundle_path,
                fixture.action_plan,
                fixture.opening_plan,
                fixture.compose_request,
                fixture.capacity_observation,
                verifier_runtime_authority=runtime_authority,
            )

            self.assertEqual(_document_snapshot(*documents), before_documents)
            self.assertEqual(_tree_snapshot(fixture.root), before_tree)
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
