from __future__ import annotations

from copy import deepcopy
import unittest

from bulkload_lib.model import (
    BulkloadError,
    canonical_bytes,
    object_digest,
    sha256_bytes,
)
from bulkload_lib.private_sqlite_protocol import (
    INDEPENDENT_VERIFICATION_RECEIPT_SCHEMA,
    V5_POLICY_SHA256,
    V5_RUNTIME_SHA256,
    V5_SOURCE_COMMIT,
    V6_POLICY_SHA256,
    V6_RUNTIME_SHA256,
    V6_SOURCE_COMMIT,
    VERIFIER_ORACLE_REPORT_SCHEMA,
    validate_composed_bundle_manifest,
    validate_composition_receipt,
    validate_independent_verification_receipt,
    validate_verifier_oracle_report,
)
from tests.private_sqlite_v7_fixtures import (
    MANIFEST_FALSE_CLAIMS,
    ORACLE_FALSE_CLAIMS,
    PRE_RECEIPT_COMPLETED_NODE_IDS,
    RECEIPT_FALSE_CLAIMS,
    WRITER_TRANSITIONS,
    handbuilt_manifest,
    handbuilt_oracle_report,
    handbuilt_receipt,
)


class PrivateSqliteV7ProtocolTest(unittest.TestCase):
    def setUp(self) -> None:
        self.manifest = handbuilt_manifest()
        self.receipt = handbuilt_receipt(self.manifest)
        self.report = handbuilt_oracle_report(self.manifest, self.receipt)

    @staticmethod
    def _redigest(value: dict, field: str) -> None:
        value[field] = object_digest(value, field)

    def test_handbuilt_protocol_documents_are_strictly_valid(self) -> None:
        validate_composed_bundle_manifest(self.manifest)
        validate_composition_receipt(self.receipt)
        validate_verifier_oracle_report(self.report)

    def test_receipt_stops_before_metadata_durability(self) -> None:
        transitions = self.receipt["operation_graph"]["state_transitions"]
        self.assertEqual(transitions, WRITER_TRANSITIONS)
        self.assertEqual(transitions[-1], "PRESEAL_REVALIDATED")
        self.assertNotIn("BUNDLE_METADATA_DURABLE", transitions)

        overclaimed = deepcopy(self.receipt)
        overclaimed_transitions = overclaimed["operation_graph"]["state_transitions"]
        overclaimed_transitions.append("BUNDLE_METADATA_DURABLE")
        overclaimed["operation_graph"]["state_transitions_sha256"] = sha256_bytes(
            canonical_bytes(overclaimed_transitions)
        )
        self._redigest(overclaimed, "receipt_sha256")
        with self.assertRaisesRegex(
            BulkloadError,
            "writer state transitions differ",
        ):
            validate_composition_receipt(overclaimed)

    def test_receipt_completed_nodes_are_the_real_pre_serialization_prefix(
        self,
    ) -> None:
        completed = self.receipt["operation_graph"]["completed_node_ids"]
        self.assertEqual(completed, PRE_RECEIPT_COMPLETED_NODE_IDS)
        self.assertIn("write-manifest", completed)
        for incomplete in ("write-receipt", "fsync-bundle", "seal-bundle"):
            self.assertNotIn(incomplete, completed)
            with self.subTest(incomplete=incomplete):
                overclaimed = deepcopy(self.receipt)
                overclaimed["operation_graph"]["completed_node_ids"] = sorted(
                    [*completed, incomplete]
                )
                self._redigest(overclaimed, "receipt_sha256")
                with self.assertRaisesRegex(
                    BulkloadError,
                    "completed operation nodes.*pre-seal",
                ):
                    validate_composition_receipt(overclaimed)

        missing_manifest = deepcopy(self.receipt)
        missing_manifest["operation_graph"]["completed_node_ids"].remove(
            "write-manifest"
        )
        self._redigest(missing_manifest, "receipt_sha256")
        with self.assertRaisesRegex(
            BulkloadError,
            "completed operation nodes.*pre-seal",
        ):
            validate_composition_receipt(missing_manifest)

    def test_preseal_receipt_keeps_seal_preconditions_false(self) -> None:
        self.assertIs(
            self.receipt["claims"]["seal_preconditions_complete"],
            False,
        )

    def test_every_capacity_phase_repeats_full_conservative_requirement(
        self,
    ) -> None:
        observations = self.receipt["capacity_admission"]["observations"]
        expected = (
            observations[0]["remaining_required_bytes"],
            observations[0]["remaining_required_inodes"],
        )
        self.assertTrue(
            all(
                (
                    observation["remaining_required_bytes"],
                    observation["remaining_required_inodes"],
                )
                == expected
                for observation in observations
            )
        )

        for replacement in (0, expected[0] - 1):
            with self.subTest(replacement=replacement):
                changed = deepcopy(self.receipt)
                later = changed["capacity_admission"]["observations"][1]
                later["available_bytes"] = replacement
                later["remaining_required_bytes"] = replacement
                later["sufficient"] = True
                self._redigest(changed, "receipt_sha256")
                with self.assertRaisesRegex(
                    BulkloadError,
                    "conservative requirement differs",
                ):
                    validate_composition_receipt(changed)

    def test_each_protocol_document_rejects_missing_and_extra_keys(self) -> None:
        cases = (
            (
                self.manifest,
                "manifest_sha256",
                validate_composed_bundle_manifest,
            ),
            (
                self.receipt,
                "receipt_sha256",
                validate_composition_receipt,
            ),
            (
                self.report,
                "oracle_report_sha256",
                validate_verifier_oracle_report,
            ),
        )
        for original, digest_field, validator in cases:
            with self.subTest(schema=original["schema"], mutation="missing"):
                missing = deepcopy(original)
                missing.pop("implementation")
                self._redigest(missing, digest_field)
                with self.assertRaisesRegex(BulkloadError, "keys differ"):
                    validator(missing)
            with self.subTest(schema=original["schema"], mutation="extra"):
                extra = deepcopy(original)
                extra["unreviewed"] = False
                self._redigest(extra, digest_field)
                with self.assertRaisesRegex(BulkloadError, "keys differ"):
                    validator(extra)

    def test_nested_contracts_reject_missing_extra_and_wrong_digest_shape(
        self,
    ) -> None:
        mutations: list[tuple[str, dict]] = []

        missing_source = deepcopy(self.manifest)
        del missing_source["producer_lineage"]["writer_runtime_authority"][
            "source_sha256"
        ]
        mutations.append(("writer source key", missing_source))

        extra_family = deepcopy(self.manifest)
        extra_family["families"][0]["schema"]["unreviewed"] = "0" * 64
        mutations.append(("family schema key", extra_family))

        uppercase_digest = deepcopy(self.manifest)
        uppercase_digest["accepted_inputs"]["action_plan"]["canonical_file_sha256"] = (
            "A" * 64
        )
        mutations.append(("uppercase digest", uppercase_digest))

        too_small = deepcopy(self.manifest)
        too_small["accepted_inputs"]["compose_request"]["canonical_file_bytes"] = 1
        mutations.append(("canonical byte count", too_small))

        for label, changed in mutations:
            with self.subTest(label=label):
                self._redigest(changed, "manifest_sha256")
                with self.assertRaises(BulkloadError):
                    validate_composed_bundle_manifest(changed)

    def test_exact_v5_and_v6_producer_lineage_is_immutable(self) -> None:
        action = self.manifest["producer_lineage"]["action_plan_producer"]
        request = self.manifest["producer_lineage"]["compose_request_producer"]
        self.assertEqual(
            action,
            {
                "source_commit": V5_SOURCE_COMMIT,
                "policy_sha256": V5_POLICY_SHA256,
                "runtime_source_sha256": V5_RUNTIME_SHA256,
            },
        )
        self.assertEqual(
            request,
            {
                "source_commit": V6_SOURCE_COMMIT,
                "policy_sha256": V6_POLICY_SHA256,
                "runtime_source_sha256": V6_RUNTIME_SHA256,
            },
        )
        for producer, key in (
            ("action_plan_producer", "source_commit"),
            ("action_plan_producer", "policy_sha256"),
            ("action_plan_producer", "runtime_source_sha256"),
            ("compose_request_producer", "source_commit"),
            ("compose_request_producer", "policy_sha256"),
            ("compose_request_producer", "runtime_source_sha256"),
        ):
            with self.subTest(producer=producer, key=key):
                changed = deepcopy(self.manifest)
                current = changed["producer_lineage"][producer][key]
                changed["producer_lineage"][producer][key] = (
                    "0" if current[0] != "0" else "1"
                ) + current[1:]
                self._redigest(changed, "manifest_sha256")
                with self.assertRaisesRegex(
                    BulkloadError,
                    "immutable producer",
                ):
                    validate_composed_bundle_manifest(changed)

    def test_runtime_source_bindings_require_exact_writer_and_verifier_leaf(
        self,
    ) -> None:
        writer = deepcopy(self.manifest)
        binding = writer["producer_lineage"]["writer_runtime_authority"]
        verifier_path = "scripts/bulkload_lib/private_sqlite_verifier.py"
        binding["source_path"] = verifier_path
        binding["runtime_authority"]["source_digests"] = {
            verifier_path: binding["source_sha256"]
        }
        self._redigest(writer, "manifest_sha256")
        with self.assertRaisesRegex(BulkloadError, "source binding differs"):
            validate_composed_bundle_manifest(writer)

        verifier = deepcopy(self.report)
        binding = verifier["verifier_runtime_authority"]
        writer_path = "scripts/bulkload_lib/private_sqlite_composer.py"
        binding["source_path"] = writer_path
        binding["runtime_authority"]["source_digests"] = {
            writer_path: binding["source_sha256"]
        }
        self._redigest(verifier, "oracle_report_sha256")
        with self.assertRaisesRegex(BulkloadError, "source binding differs"):
            validate_verifier_oracle_report(verifier)

        stale_digest = deepcopy(self.report)
        stale_digest["verifier_runtime_authority"]["source_sha256"] = "0" * 64
        self._redigest(stale_digest, "oracle_report_sha256")
        with self.assertRaisesRegex(BulkloadError, "source binding differs"):
            validate_verifier_oracle_report(stale_digest)

    def test_sqlite_engine_authority_requires_exact_threadsafe_value(
        self,
    ) -> None:
        for forged in (-1, 3, True, "1"):
            with self.subTest(threadsafe=forged):
                changed = deepcopy(self.manifest)
                authority = changed["sqlite_engine_authority"]
                authority["threadsafe"] = forged
                if forged == 3:
                    options = [
                        option
                        for option in authority["compile_options"]
                        if not option.startswith("THREADSAFE=")
                    ]
                    authority["compile_options"] = sorted([*options, "THREADSAFE=3"])
                    authority["compile_options_sha256"] = sha256_bytes(
                        canonical_bytes(authority["compile_options"])
                    )
                self._redigest(changed, "manifest_sha256")
                with self.assertRaisesRegex(
                    BulkloadError,
                    "threadsafe mode is invalid",
                ):
                    validate_composed_bundle_manifest(changed)

    def test_sqlite_engine_authority_accepts_exact_sqlite_threadsafe_values(
        self,
    ) -> None:
        for threadsafe in (0, 1, 2):
            with self.subTest(threadsafe=threadsafe):
                changed = deepcopy(self.manifest)
                authority = changed["sqlite_engine_authority"]
                options = [
                    option
                    for option in authority["compile_options"]
                    if not option.startswith("THREADSAFE=")
                ]
                authority["threadsafe"] = threadsafe
                authority["compile_options"] = sorted(
                    [*options, f"THREADSAFE={threadsafe}"]
                )
                authority["compile_options_sha256"] = sha256_bytes(
                    canonical_bytes(authority["compile_options"])
                )
                self._redigest(changed, "manifest_sha256")
                validate_composed_bundle_manifest(changed)

    def test_sqlite_engine_authority_cross_binds_one_compile_option(
        self,
    ) -> None:
        cases = (
            ("mismatched", 2, ["THREADSAFE=1"]),
            ("duplicate", 1, ["THREADSAFE=1", "THREADSAFE=2"]),
            ("missing", 1, []),
        )
        for label, threadsafe, threadsafe_options in cases:
            with self.subTest(label=label):
                changed = deepcopy(self.manifest)
                authority = changed["sqlite_engine_authority"]
                options = [
                    option
                    for option in authority["compile_options"]
                    if not option.startswith("THREADSAFE=")
                ]
                authority["threadsafe"] = threadsafe
                authority["compile_options"] = sorted([*options, *threadsafe_options])
                authority["compile_options_sha256"] = sha256_bytes(
                    canonical_bytes(authority["compile_options"])
                )
                self._redigest(changed, "manifest_sha256")
                with self.assertRaisesRegex(
                    BulkloadError,
                    "threadsafe compile authority differs",
                ):
                    validate_composed_bundle_manifest(changed)

    def test_self_redigested_claim_overreach_is_rejected(self) -> None:
        cases = (
            (
                self.manifest,
                "manifest_sha256",
                MANIFEST_FALSE_CLAIMS,
                validate_composed_bundle_manifest,
            ),
            (
                self.receipt,
                "receipt_sha256",
                RECEIPT_FALSE_CLAIMS,
                validate_composition_receipt,
            ),
            (
                self.report,
                "oracle_report_sha256",
                ORACLE_FALSE_CLAIMS,
                validate_verifier_oracle_report,
            ),
        )
        for original, digest_field, false_claims, validator in cases:
            for claim in sorted(false_claims):
                with self.subTest(schema=original["schema"], claim=claim):
                    changed = deepcopy(original)
                    changed["claims"][claim] = True
                    self._redigest(changed, digest_field)
                    with self.assertRaisesRegex(BulkloadError, "claims.*differ"):
                        validator(changed)

    def test_oracle_report_cannot_be_retyped_as_a_final_receipt(self) -> None:
        self.assertEqual(self.report["schema"], VERIFIER_ORACLE_REPORT_SCHEMA)
        self.assertNotEqual(
            self.report["schema"],
            INDEPENDENT_VERIFICATION_RECEIPT_SCHEMA,
        )
        self.assertTrue(all(value is False for value in self.report["claims"].values()))
        with self.assertRaisesRegex(BulkloadError, "keys differ"):
            validate_independent_verification_receipt(self.report)

        retyped = deepcopy(self.report)
        retyped["schema"] = INDEPENDENT_VERIFICATION_RECEIPT_SCHEMA
        self._redigest(retyped, "oracle_report_sha256")
        with self.assertRaisesRegex(BulkloadError, "keys differ"):
            validate_independent_verification_receipt(retyped)

    def test_self_redigested_artifact_and_tree_mutations_still_fail(self) -> None:
        artifact = deepcopy(self.report)
        artifact["manifest_observation"]["canonical_file_bytes"] += 1
        self._redigest(artifact, "oracle_report_sha256")
        with self.assertRaisesRegex(BulkloadError, "observations are incomplete"):
            validate_verifier_oracle_report(artifact)

        tree = deepcopy(self.report)
        tree["bundle_observation"]["tree_inventory"][0]["size"] += 1
        tree["bundle_observation"]["tree_inventory_sha256"] = sha256_bytes(
            canonical_bytes(tree["bundle_observation"]["tree_inventory"])
        )
        self._redigest(tree, "oracle_report_sha256")
        with self.assertRaisesRegex(BulkloadError, "observations are incomplete"):
            validate_verifier_oracle_report(tree)


if __name__ == "__main__":
    unittest.main()
