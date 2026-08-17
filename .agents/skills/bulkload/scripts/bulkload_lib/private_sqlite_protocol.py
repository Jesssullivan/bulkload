"""Strict v7 protocol for offline SQLite composition evidence.

This module defines data contracts only.  It does not open SQLite databases,
compose state, publish receipts, install bundles, or make a public command
available.  Writer and verifier implementations may share these protocol
validators, but may not share semantic row or composition implementations.
"""

from __future__ import annotations

from datetime import UTC, datetime
from pathlib import PurePosixPath
import re
from typing import Any
import uuid

from .model import BulkloadError, canonical_bytes, object_digest, sha256_bytes


COMPOSED_BUNDLE_MANIFEST_SCHEMA = (
    "dev.tinyland.bulkload.codex-private-sqlite-composed-bundle-manifest.v7"
)
COMPOSITION_RECEIPT_SCHEMA = (
    "dev.tinyland.bulkload.codex-private-sqlite-composition-receipt.v7"
)
INDEPENDENT_VERIFICATION_RECEIPT_SCHEMA = (
    "dev.tinyland.bulkload.codex-private-sqlite-independent-verification-receipt.v7"
)
VERIFIER_ORACLE_REPORT_SCHEMA = (
    "dev.tinyland.bulkload.codex-private-sqlite-verifier-oracle-report.v7"
)

COMPOSED_BUNDLE_MANIFEST_IMPLEMENTATION = "sqlite-composed-bundle-manifest-protocol-v7"
COMPOSITION_RECEIPT_IMPLEMENTATION = "sqlite-composition-preseal-receipt-protocol-v7"
INDEPENDENT_VERIFICATION_RECEIPT_IMPLEMENTATION = (
    "sqlite-independent-verification-receipt-protocol-v7"
)
VERIFIER_ORACLE_REPORT_IMPLEMENTATION = (
    "sqlite-bundle-shaped-read-only-diagnostic-oracle-v7"
)

ACTION_PLAN_SCHEMA = "dev.tinyland.bulkload.codex-private-sqlite-compose-action-plan.v5"
COMPOSE_REQUEST_SCHEMA = "dev.tinyland.bulkload.codex-private-sqlite-compose-request.v6"
CAPACITY_OBSERVATION_SCHEMA = (
    "dev.tinyland.bulkload.codex-private-sqlite-capacity-observation.v6"
)
RUNTIME_AUTHORITY_SCHEMA = "dev.tinyland.bulkload.codex-private-runtime-authority.v1"

V5_SOURCE_COMMIT = "4d949a846690b265b0bf775ec81ff4b9e5a52ddc"
V5_POLICY_SHA256 = "78318633ef06ca12d6dc7e72199c07dc6f5cfdf0e9cf13bc5bd3f3e7c2ddbcf0"
V5_RUNTIME_SHA256 = "cc9f96adb8189e0a41133244231edf51dd837fa75459728355b93eeb981c7392"
V6_SOURCE_COMMIT = "7bd06a05f7a4710e42fac6be08b477b801493c95"
V6_POLICY_SHA256 = "6d8c1e01c7ea244adb5e3d09279c8ac640a8a5a3c9a4966b30eeff5cc56e289e"
V6_RUNTIME_SHA256 = "5adfc213cf657d24d3bd8de3ab6b5f9e3d1bf14627045c8526d0b32241f460f5"

MAX_PROTOCOL_BYTES = 16 * 1024 * 1024
MAX_CHECKED_INTEGER = (1 << 63) - 1
MAX_SOURCE_FILES = 256
MAX_LINEAGE_RECORDS = 128
MAX_FAMILIES = 64
MAX_TABLES = 4096
MAX_TREE_ENTRIES = 8192
MAX_FAILURES = 1024
MAX_COMPILE_OPTIONS = 1024
MAX_TEXT_BYTES = 4096

_SHA256 = re.compile(r"[0-9a-f]{64}")
_GIT_COMMIT = re.compile(r"[0-9a-f]{40}")
_UTC_SECONDS = re.compile(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z")
_LEAF = re.compile(r"[A-Za-z0-9.][A-Za-z0-9._-]{0,254}")
_IDENTIFIER = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
SQLITE_FAMILY_BASENAME = re.compile(
    r"(?:state|logs|goals|memories)_[1-9][0-9]*\.sqlite"
)
_NODE_ID = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.:-]{0,511}")
_FAILURE_CODE = re.compile(r"[a-z0-9][a-z0-9-]{0,95}")
_FIELD = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.:\[\]-]{0,511}")
_SQLITE_VERSION = re.compile(r"\d+\.\d+\.\d+")

PRESEAL_RECEIPT_TRANSITIONS = [
    "PRE_MUTATION_VALIDATED",
    "STAGING_CLAIMED",
    "STAGING_DURABLE",
    "BASELINES_COMPLETE",
    "UNIONS_COMPLETE",
    "WRITER_SELF_CHECK_COMPLETE",
    "PRESEAL_REVALIDATED",
]
PRESEAL_RECEIPT_EXCLUDED_NODE_IDS = frozenset(
    {
        "write-receipt",
        "fsync-bundle",
        "seal-bundle",
    }
)

MANIFEST_POSITIVE_CLAIMS = frozenset(
    {
        "destination_baseline_streamed",
        "source_only_rows_inserted",
        "payload_complete",
        "writer_self_check_passed",
    }
)
MANIFEST_FALSE_CLAIMS = frozenset(
    {
        "existing_destination_rows_mutated",
        "identity_remapped",
        "deduplicated",
        "rows_deleted",
        "final_leaf_commit_observed",
        "independently_verified",
        "space_reserved",
        "published",
        "installed",
        "composer_implemented",
        "ready_for_internal_offline_compose",
        "ready_for_offline_compose",
        "sqlite_compose",
        "sqlite_publish",
        "sqlite_union_ready",
        "combined",
        "ready_for_apply",
        "provider_runtime_acceptance_verified",
        "provider_writer_proof",
    }
)
RECEIPT_POSITIVE_CLAIMS = frozenset(
    {
        "input_chain_recomputed",
        "capacity_admitted",
        "ticket_consumed",
        "composition_complete",
        "writer_self_verified",
    }
)
RECEIPT_FALSE_CLAIMS = frozenset(
    {
        "seal_preconditions_complete",
        "final_leaf_commit_observed",
        "bundle_sealed",
        "independently_verified",
        "space_reserved",
        "quota_proof",
        "composer_implemented",
        "ready_for_internal_offline_compose",
        "ready_for_offline_compose",
        "sqlite_compose",
        "sqlite_publish",
        "sqlite_union_ready",
        "published",
        "installed",
        "combined",
        "ready_for_apply",
        "provider_runtime_acceptance_verified",
        "provider_writer_proof",
    }
)
_VERIFICATION_SUCCESS_CLAIMS = {
    "input_chain_recomputed",
    "bundle_tree_verified",
    "manifest_verified",
    "composition_receipt_verified",
    "family_semantics_verified",
    "bundle_sealed",
    "offline_verified",
}
_VERIFICATION_FALSE_CLAIMS = {
    "composer_implemented",
    "ready_for_internal_offline_compose",
    "ready_for_offline_compose",
    "sqlite_compose",
    "sqlite_publish",
    "sqlite_union_ready",
    "published",
    "installed",
    "activated",
    "session_execution_verified",
    "combined",
    "ready_for_apply",
    "provider_runtime_acceptance_verified",
    "provider_writer_proof",
}
_ORACLE_OBSERVED_CHECKS = {
    "artifact_bindings_observed",
    "descriptor_custody_observed",
    "tree_inventory_observed",
    "manifest_structure_observed",
    "composition_receipt_structure_observed",
    "sqlite_engine_consistency_observed",
    "integrity_checks_observed",
    "foreign_key_checks_observed",
    "schema_comparisons_observed",
    "migration_comparisons_observed",
    "edge_comparisons_observed",
    "semantic_comparisons_observed",
}
ORACLE_FALSE_CLAIMS = frozenset(
    {
        "final_leaf_commit_observed",
        "bundle_sealed",
        "full_against_inputs_recomputed",
        "independent_verification_complete",
        "offline_bundle_verified",
        "composer_implemented",
        "ready_for_internal_offline_compose",
        "ready_for_offline_compose",
        "sqlite_compose",
        "sqlite_publish",
        "published",
        "publication_authorized",
        "installed",
        "install_authorized",
        "session_union_executed",
        "sqlite_union_ready",
        "provider_runtime_acceptance",
        "provider_runtime_acceptance_verified",
        "provider_writer_proof",
        "combined",
        "combined_authorized",
        "ready_for_apply",
        "apply_authorized",
        "engine_diversity_verified",
    }
)
_FAMILY_VERIFICATION_FIELDS = {
    "manifest_matches",
    "action_plan_matches",
    "immutable_inputs_match",
    "destination_baseline_preserved",
    "source_only_rows_present",
    "shared_equal_rows_once",
    "integrity_check_passed",
    "foreign_key_check_passed",
}
_ORACLE_FAMILY_OBSERVATION_FIELDS = {
    "manifest_matches_observed",
    "action_plan_matches_observed",
    "integrity_check_observed",
    "foreign_key_check_observed",
    "schema_matches_action_observed",
    "migration_matches_action_observed",
    "edge_matches_action_observed",
    "semantic_output_matches_action_observed",
}
_FAILURE_SCOPES = {
    "protocol",
    "input-chain",
    "sqlite-engine",
    "workspace",
    "bundle",
    "manifest",
    "composition-receipt",
    "family",
}


def _require_exact_keys(
    value: Any,
    expected: set[str],
    label: str,
) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != expected:
        raise BulkloadError(f"{label} keys differ from the exact contract")
    return value


def _require_sha256(value: Any, label: str) -> str:
    if not isinstance(value, str) or _SHA256.fullmatch(value) is None:
        raise BulkloadError(f"{label} must be a lowercase SHA-256 digest")
    return value


def _require_uuid(value: Any, label: str) -> str:
    if not isinstance(value, str):
        raise BulkloadError(f"{label} must be a canonical UUID")
    try:
        parsed = uuid.UUID(value)
    except ValueError as error:
        raise BulkloadError(f"{label} must be a canonical UUID") from error
    if str(parsed) != value:
        raise BulkloadError(f"{label} must be a canonical UUID")
    return value


def _parse_timestamp(value: Any, label: str) -> datetime:
    if not isinstance(value, str) or _UTC_SECONDS.fullmatch(value) is None:
        raise BulkloadError(f"{label} must be a canonical UTC-seconds timestamp")
    try:
        parsed = datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=UTC)
    except ValueError as error:
        raise BulkloadError(
            f"{label} must be a canonical UTC-seconds timestamp"
        ) from error
    return parsed


def _require_integer(
    value: Any,
    label: str,
    *,
    maximum: int = MAX_CHECKED_INTEGER,
) -> int:
    if type(value) is not int or value < 0 or value > maximum:
        raise BulkloadError(f"{label} is outside the checked integer range")
    return value


def _require_text(value: Any, label: str, *, maximum: int = MAX_TEXT_BYTES) -> str:
    if not isinstance(value, str) or not value:
        raise BulkloadError(f"{label} must be a nonempty string")
    try:
        payload = value.encode("utf-8")
    except UnicodeEncodeError as error:
        raise BulkloadError(f"{label} must be valid UTF-8") from error
    if len(payload) > maximum or any(
        ord(character) < 0x20 or ord(character) == 0x7F for character in value
    ):
        raise BulkloadError(f"{label} is not a bounded printable string")
    return value


def _require_leaf(value: Any, label: str) -> str:
    if (
        not isinstance(value, str)
        or value in {"", ".", ".."}
        or "/" in value
        or _LEAF.fullmatch(value) is None
    ):
        raise BulkloadError(f"{label} must be one canonical leaf")
    return value


def _require_identifier(value: Any, label: str) -> str:
    if not isinstance(value, str) or _IDENTIFIER.fullmatch(value) is None:
        raise BulkloadError(f"{label} must be a supported SQLite identifier")
    return value


def _require_relative_path(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value or "\x00" in value:
        raise BulkloadError(f"{label} must be a canonical relative path")
    candidate = PurePosixPath(value)
    if (
        candidate.is_absolute()
        or any(part in {"", ".", ".."} for part in candidate.parts)
        or candidate.as_posix() != value
    ):
        raise BulkloadError(f"{label} must be a canonical relative path")
    if len(value.encode("utf-8")) > 1024:
        raise BulkloadError(f"{label} exceeds its byte bound")
    return value


def _require_absolute_path(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value.startswith("/") or "\x00" in value:
        raise BulkloadError(f"{label} must be a canonical absolute POSIX path")
    candidate = PurePosixPath(value)
    if (
        any(part in {"", ".", ".."} for part in candidate.parts[1:])
        or candidate.as_posix() != value
        or len(value.encode("utf-8")) > 4096
    ):
        raise BulkloadError(f"{label} must be a canonical absolute POSIX path")
    return value


def _require_canonical_list(
    value: Any,
    label: str,
    *,
    maximum: int,
) -> list[Any]:
    if not isinstance(value, list) or len(value) > maximum:
        raise BulkloadError(f"{label} is not a bounded list")
    bodies = [canonical_bytes(item) for item in value]
    if bodies != sorted(set(bodies)):
        raise BulkloadError(f"{label} is not canonically ordered and unique")
    return value


def _validate_identity(
    value: Any,
    label: str,
) -> dict[str, Any]:
    identity = _require_exact_keys(
        value,
        {"device", "inode", "uid", "mode"},
        label,
    )
    for key in ("device", "inode", "uid", "mode"):
        _require_integer(identity[key], f"{label} {key}")
    if identity["mode"] > 0o7777:
        raise BulkloadError(f"{label} mode is invalid")
    return identity


def _validate_mode_identity(
    value: Any,
    label: str,
    *,
    required_mode: int,
) -> dict[str, Any]:
    identity = _validate_identity(value, label)
    if identity["mode"] != required_mode:
        raise BulkloadError(f"{label} mode differs")
    return identity


def _validate_mount(value: Any, label: str) -> dict[str, Any]:
    mount = _require_exact_keys(
        value,
        {"device", "filesystem_id", "linux_mount_id"},
        label,
    )
    _require_integer(mount["device"], f"{label} device")
    for key in ("filesystem_id", "linux_mount_id"):
        if mount[key] is not None:
            _require_integer(mount[key], f"{label} {key}")
    if mount["filesystem_id"] is None and mount["linux_mount_id"] is None:
        raise BulkloadError(f"{label} is ambiguous")
    return mount


def _validate_lineage(
    value: Any,
    label: str,
    *,
    leaf_identity: dict[str, Any],
    leaf_mount: dict[str, Any],
) -> list[dict[str, Any]]:
    if not isinstance(value, list) or not value or len(value) > MAX_LINEAGE_RECORDS:
        raise BulkloadError(f"{label} is not a bounded nonempty lineage")
    seen: set[tuple[int, int]] = set()
    for index, item in enumerate(value):
        record = _require_exact_keys(
            item,
            {"identity", "mount"},
            f"{label} record",
        )
        identity = _validate_identity(
            record["identity"],
            f"{label} identity",
        )
        mount = _validate_mount(record["mount"], f"{label} mount")
        if mount["device"] != identity["device"]:
            raise BulkloadError(f"{label} mount device differs")
        key = (identity["device"], identity["inode"])
        if key in seen:
            raise BulkloadError(f"{label} contains a cycle")
        seen.add(key)
        if index == 0 and (identity != leaf_identity or mount != leaf_mount):
            raise BulkloadError(f"{label} leaf binding differs")
    return value


def _validate_runtime_authority(value: Any, label: str) -> dict[str, Any]:
    authority = _require_exact_keys(
        value,
        {
            "schema",
            "policy_schema",
            "policy_sha256",
            "runtime_source_sha256",
            "source_digests",
        },
        label,
    )
    if authority["schema"] != RUNTIME_AUTHORITY_SCHEMA:
        raise BulkloadError(f"{label} schema differs")
    policy_schema = _require_text(
        authority["policy_schema"],
        f"{label} policy schema",
        maximum=512,
    )
    if not policy_schema.startswith("dev.tinyland.bulkload."):
        raise BulkloadError(f"{label} policy schema is outside the authority domain")
    _require_sha256(authority["policy_sha256"], f"{label} policy digest")
    _require_sha256(
        authority["runtime_source_sha256"],
        f"{label} runtime-source digest",
    )
    sources = authority["source_digests"]
    if not isinstance(sources, dict) or not sources or len(sources) > MAX_SOURCE_FILES:
        raise BulkloadError(f"{label} source inventory is invalid")
    for path, digest in sources.items():
        _require_relative_path(path, f"{label} source path")
        if not path.startswith("scripts/"):
            raise BulkloadError(f"{label} source path is outside scripts")
        _require_sha256(digest, f"{label} source digest")
    return authority


def _validate_runtime_source_binding(
    value: Any,
    label: str,
    *,
    required_source_leaf: str,
) -> dict[str, Any]:
    binding = _require_exact_keys(
        value,
        {"runtime_authority", "source_path", "source_sha256"},
        label,
    )
    authority = _validate_runtime_authority(
        binding["runtime_authority"],
        f"{label} runtime",
    )
    path = _require_relative_path(binding["source_path"], f"{label} source path")
    if (
        not path.startswith("scripts/bulkload_lib/")
        or not path.endswith(".py")
        or PurePosixPath(path).name != required_source_leaf
        or authority["source_digests"].get(path) != binding["source_sha256"]
    ):
        raise BulkloadError(f"{label} source binding differs from its runtime")
    _require_sha256(binding["source_sha256"], f"{label} source digest")
    return binding


def _validate_exact_producer(
    value: Any,
    label: str,
    *,
    source_commit: str,
    policy_sha256: str,
    runtime_sha256: str,
) -> dict[str, Any]:
    producer = _require_exact_keys(
        value,
        {"source_commit", "policy_sha256", "runtime_source_sha256"},
        label,
    )
    if (
        not isinstance(producer["source_commit"], str)
        or _GIT_COMMIT.fullmatch(producer["source_commit"]) is None
    ):
        raise BulkloadError(f"{label} source commit is invalid")
    _require_sha256(producer["policy_sha256"], f"{label} policy digest")
    _require_sha256(
        producer["runtime_source_sha256"],
        f"{label} runtime digest",
    )
    if producer != {
        "source_commit": source_commit,
        "policy_sha256": policy_sha256,
        "runtime_source_sha256": runtime_sha256,
    }:
        raise BulkloadError(f"{label} differs from the immutable producer")
    return producer


def _validate_producer_lineage(value: Any) -> dict[str, Any]:
    lineage = _require_exact_keys(
        value,
        {
            "action_plan_producer",
            "compose_request_producer",
            "writer_runtime_authority",
        },
        "composition producer lineage",
    )
    _validate_exact_producer(
        lineage["action_plan_producer"],
        "action-plan producer",
        source_commit=V5_SOURCE_COMMIT,
        policy_sha256=V5_POLICY_SHA256,
        runtime_sha256=V5_RUNTIME_SHA256,
    )
    _validate_exact_producer(
        lineage["compose_request_producer"],
        "compose-request producer",
        source_commit=V6_SOURCE_COMMIT,
        policy_sha256=V6_POLICY_SHA256,
        runtime_sha256=V6_RUNTIME_SHA256,
    )
    _validate_runtime_source_binding(
        lineage["writer_runtime_authority"],
        "writer runtime authority",
        required_source_leaf="private_sqlite_composer.py",
    )
    return lineage


def _validate_sqlite_engine_authority(value: Any) -> dict[str, Any]:
    authority = _require_exact_keys(
        value,
        {
            "sqlite_version",
            "sqlite_source_id",
            "compile_options",
            "compile_options_sha256",
            "threadsafe",
        },
        "SQLite engine authority",
    )
    if (
        not isinstance(authority["sqlite_version"], str)
        or _SQLITE_VERSION.fullmatch(authority["sqlite_version"]) is None
    ):
        raise BulkloadError("SQLite engine version is invalid")
    _require_text(
        authority["sqlite_source_id"],
        "SQLite source ID",
        maximum=1024,
    )
    options = authority["compile_options"]
    if (
        not isinstance(options, list)
        or len(options) > MAX_COMPILE_OPTIONS
        or any(
            not isinstance(option, str)
            or not option
            or len(option.encode("utf-8")) > 512
            or any(ord(character) < 0x20 for character in option)
            for option in options
        )
        or options != sorted(set(options))
    ):
        raise BulkloadError("SQLite compile options are not canonical")
    _require_sha256(
        authority["compile_options_sha256"],
        "SQLite compile-options digest",
    )
    if sha256_bytes(canonical_bytes(options)) != authority["compile_options_sha256"]:
        raise BulkloadError("SQLite compile-options digest mismatch")
    threadsafe = authority["threadsafe"]
    if type(threadsafe) is not int or threadsafe not in {0, 1, 2}:
        raise BulkloadError("SQLite threadsafe mode is invalid")
    threadsafe_options = [
        option for option in options if option.startswith("THREADSAFE=")
    ]
    if threadsafe_options != [f"THREADSAFE={threadsafe}"]:
        raise BulkloadError("SQLite threadsafe compile authority differs")
    return authority


def _validate_input_binding(
    value: Any,
    label: str,
    *,
    schema: str,
    digest_field: str,
) -> dict[str, Any]:
    binding = _require_exact_keys(
        value,
        {
            "schema",
            digest_field,
            "canonical_file_sha256",
            "canonical_file_bytes",
        },
        label,
    )
    if binding["schema"] != schema:
        raise BulkloadError(f"{label} schema differs")
    _require_sha256(binding[digest_field], f"{label} accepted self-digest")
    _require_sha256(
        binding["canonical_file_sha256"],
        f"{label} canonical-file digest",
    )
    byte_count = _require_integer(
        binding["canonical_file_bytes"],
        f"{label} canonical-file byte count",
    )
    if byte_count < 2 or byte_count > MAX_PROTOCOL_BYTES:
        raise BulkloadError(f"{label} canonical file exceeds its byte budget")
    return binding


def _validate_accepted_inputs(value: Any) -> dict[str, Any]:
    accepted = _require_exact_keys(
        value,
        {"action_plan", "compose_request", "capacity_observation"},
        "composition accepted inputs",
    )
    _validate_input_binding(
        accepted["action_plan"],
        "accepted action plan",
        schema=ACTION_PLAN_SCHEMA,
        digest_field="action_plan_sha256",
    )
    _validate_input_binding(
        accepted["compose_request"],
        "accepted compose request",
        schema=COMPOSE_REQUEST_SCHEMA,
        digest_field="request_sha256",
    )
    _validate_input_binding(
        accepted["capacity_observation"],
        "accepted capacity observation",
        schema=CAPACITY_OBSERVATION_SCHEMA,
        digest_field="observation_sha256",
    )
    return accepted


def _validate_workspace(
    value: Any,
    *,
    action_plan_sha256: str,
) -> dict[str, Any]:
    workspace = _require_exact_keys(
        value,
        {
            "resolved_parent",
            "parent_identity",
            "mount",
            "lineage",
            "lineage_sha256",
            "staging_leaf",
            "staging_identity",
            "final_leaf",
        },
        "composition workspace",
    )
    _require_absolute_path(
        workspace["resolved_parent"],
        "composition workspace parent",
    )
    parent = _validate_mode_identity(
        workspace["parent_identity"],
        "composition workspace parent identity",
        required_mode=0o700,
    )
    mount = _validate_mount(workspace["mount"], "composition workspace mount")
    if mount["device"] != parent["device"]:
        raise BulkloadError("composition workspace mount device differs")
    lineage = _validate_lineage(
        workspace["lineage"],
        "composition workspace lineage",
        leaf_identity=parent,
        leaf_mount=mount,
    )
    _require_sha256(
        workspace["lineage_sha256"],
        "composition workspace lineage digest",
    )
    if sha256_bytes(canonical_bytes(lineage)) != workspace["lineage_sha256"]:
        raise BulkloadError("composition workspace lineage digest mismatch")
    final_leaf = _require_leaf(workspace["final_leaf"], "composition final leaf")
    if final_leaf != f"bulkload-sqlite-compose-{action_plan_sha256}":
        raise BulkloadError("composition final leaf differs from its action plan")
    staging_leaf = _require_leaf(
        workspace["staging_leaf"],
        "composition staging leaf",
    )
    staging_prefix = f".{final_leaf}-staging-"
    if not staging_leaf.startswith(staging_prefix):
        raise BulkloadError("composition staging leaf prefix differs")
    _require_uuid(
        staging_leaf.removeprefix(staging_prefix),
        "composition staging attempt ID",
    )
    staging = _validate_mode_identity(
        workspace["staging_identity"],
        "composition staging identity",
        required_mode=0o700,
    )
    if staging["device"] != parent["device"]:
        raise BulkloadError("composition staging device differs")
    return workspace


def _validate_schema_facts(value: Any, label: str) -> dict[str, Any]:
    facts = _require_exact_keys(
        value,
        {"raw_schema_sha256", "schema_contract_sha256", "structured_schema_sha256"},
        label,
    )
    for key in facts:
        _require_sha256(facts[key], f"{label} {key}")
    return facts


def _validate_migration_facts(value: Any, label: str) -> dict[str, Any]:
    facts = _require_exact_keys(
        value,
        {"migrations_sha256", "exact"},
        label,
    )
    _require_sha256(facts["migrations_sha256"], f"{label} digest")
    if facts["exact"] is not True:
        raise BulkloadError(f"{label} must remain exact")
    return facts


def _validate_edge_facts(value: Any, label: str) -> dict[str, Any]:
    facts = _require_exact_keys(
        value,
        {"registry_sha256", "observed_sha256", "closed"},
        label,
    )
    _require_sha256(facts["registry_sha256"], f"{label} registry digest")
    _require_sha256(facts["observed_sha256"], f"{label} observed digest")
    if facts["closed"] is not True:
        raise BulkloadError(f"{label} must be closed")
    return facts


def _validate_table_facts(value: Any, label: str) -> dict[str, Any]:
    table = _require_exact_keys(
        value,
        {
            "name",
            "identity_columns",
            "row_count",
            "semantic_rows_sha256",
            "schema_sha256",
            "foreign_keys_sha256",
        },
        label,
    )
    _require_identifier(table["name"], f"{label} name")
    identities = table["identity_columns"]
    if (
        not isinstance(identities, list)
        or not identities
        or len(identities) > 64
        or len(set(identities)) != len(identities)
    ):
        raise BulkloadError(f"{label} identity columns are invalid")
    for index, name in enumerate(identities):
        _require_identifier(name, f"{label} identity column {index}")
    _require_integer(table["row_count"], f"{label} row count")
    for key in (
        "semantic_rows_sha256",
        "schema_sha256",
        "foreign_keys_sha256",
    ):
        _require_sha256(table[key], f"{label} {key}")
    return table


def _validate_family(value: Any, label: str) -> dict[str, Any]:
    family = _require_exact_keys(
        value,
        {
            "basename",
            "relative_path",
            "mode",
            "size",
            "sha256",
            "journal_mode",
            "application_id",
            "user_version",
            "schema",
            "migrations",
            "edges",
            "tables",
            "absent_sidecars",
        },
        label,
    )
    basename = _require_leaf(family["basename"], f"{label} basename")
    if SQLITE_FAMILY_BASENAME.fullmatch(basename) is None:
        raise BulkloadError(f"{label} basename is outside the Codex family namespace")
    if family["relative_path"] != f"sqlite/{basename}":
        raise BulkloadError(f"{label} relative path differs")
    if family["mode"] != 0o600:
        raise BulkloadError(f"{label} mode differs")
    _require_integer(family["size"], f"{label} size")
    _require_sha256(family["sha256"], f"{label} physical digest")
    if family["journal_mode"] != "delete":
        raise BulkloadError(f"{label} journal mode differs")
    _require_integer(
        family["application_id"],
        f"{label} application ID",
        maximum=(1 << 31) - 1,
    )
    _require_integer(
        family["user_version"],
        f"{label} user version",
        maximum=(1 << 31) - 1,
    )
    _validate_schema_facts(family["schema"], f"{label} schema")
    _validate_migration_facts(family["migrations"], f"{label} migrations")
    _validate_edge_facts(family["edges"], f"{label} edges")
    tables = family["tables"]
    if not isinstance(tables, list) or not tables or len(tables) > MAX_TABLES:
        raise BulkloadError(f"{label} tables are not a bounded nonempty list")
    for index, table in enumerate(tables):
        _validate_table_facts(table, f"{label} table {index}")
    names = [table["name"] for table in tables]
    if names != sorted(set(names)):
        raise BulkloadError(f"{label} tables are not canonical")
    expected_sidecars = sorted(
        f"{family['relative_path']}{suffix}" for suffix in ("-journal", "-shm", "-wal")
    )
    if family["absent_sidecars"] != expected_sidecars:
        raise BulkloadError(f"{label} absent-sidecar inventory differs")
    return family


def _validate_families(value: Any) -> list[dict[str, Any]]:
    if not isinstance(value, list) or not value or len(value) > MAX_FAMILIES:
        raise BulkloadError("composed families are not a bounded nonempty list")
    table_count = 0
    for index, family in enumerate(value):
        _validate_family(family, f"composed family {index}")
        table_count += len(family["tables"])
    if table_count > MAX_TABLES:
        raise BulkloadError("composed family table inventory exceeds its bound")
    basenames = [family["basename"] for family in value]
    if basenames != sorted(set(basenames)):
        raise BulkloadError("composed families are not canonical")
    return value


def _payload_projection(family: dict[str, Any]) -> dict[str, Any]:
    return {
        "relative_path": family["relative_path"],
        "type": "regular-file",
        "mode": family["mode"],
        "size": family["size"],
        "sha256": family["sha256"],
    }


def _validate_inventory_entry(value: Any, label: str) -> dict[str, Any]:
    entry = _require_exact_keys(
        value,
        {"relative_path", "type", "mode", "size", "sha256"},
        label,
    )
    _require_relative_path(entry["relative_path"], f"{label} path")
    if entry["type"] != "regular-file" or entry["mode"] != 0o600:
        raise BulkloadError(f"{label} type or mode differs")
    _require_integer(entry["size"], f"{label} size")
    _require_sha256(entry["sha256"], f"{label} digest")
    return entry


def _validate_payload_inventory(
    value: Any,
    families: list[dict[str, Any]],
) -> dict[str, Any]:
    inventory = _require_exact_keys(
        value,
        {"entries", "inventory_sha256"},
        "composed payload inventory",
    )
    entries = inventory["entries"]
    if not isinstance(entries, list) or len(entries) > MAX_TREE_ENTRIES:
        raise BulkloadError("composed payload entries exceed their bound")
    for index, entry in enumerate(entries):
        _validate_inventory_entry(entry, f"composed payload entry {index}")
    expected = [_payload_projection(family) for family in families]
    if entries != expected:
        raise BulkloadError("composed payload inventory differs from families")
    _require_sha256(
        inventory["inventory_sha256"],
        "composed payload inventory digest",
    )
    if sha256_bytes(canonical_bytes(entries)) != inventory["inventory_sha256"]:
        raise BulkloadError("composed payload inventory digest mismatch")
    return inventory


def _validate_boolean_claims(
    value: Any,
    label: str,
    *,
    positive: set[str],
    negative: set[str],
    success: bool = True,
) -> dict[str, Any]:
    claims = _require_exact_keys(value, positive | negative, label)
    if any(claims[key] is not (True if success else False) for key in positive) or any(
        claims[key] is not False for key in negative
    ):
        raise BulkloadError(f"{label} differ")
    return claims


def _validate_self_digest(
    value: dict[str, Any],
    digest_field: str,
    label: str,
) -> None:
    _require_sha256(value[digest_field], f"{label} self-digest")
    if object_digest(value, digest_field) != value[digest_field]:
        raise BulkloadError(f"{label} self-digest mismatch")
    if len(canonical_bytes(value)) > MAX_PROTOCOL_BYTES:
        raise BulkloadError(f"{label} exceeds its byte budget")


def validate_composed_bundle_manifest(value: dict[str, Any]) -> None:
    """Validate one strict, self-digested v7 composed-bundle manifest."""
    manifest = _require_exact_keys(
        value,
        {
            "schema",
            "created_at",
            "composition_id",
            "producer_lineage",
            "sqlite_engine_authority",
            "accepted_inputs",
            "workspace",
            "families",
            "payload_inventory",
            "claims",
            "implementation",
            "manifest_sha256",
        },
        "composed-bundle manifest",
    )
    if manifest["schema"] != COMPOSED_BUNDLE_MANIFEST_SCHEMA:
        raise BulkloadError("composed-bundle manifest schema differs")
    _parse_timestamp(manifest["created_at"], "manifest created_at")
    _require_uuid(manifest["composition_id"], "manifest composition ID")
    _validate_producer_lineage(manifest["producer_lineage"])
    _validate_sqlite_engine_authority(manifest["sqlite_engine_authority"])
    accepted = _validate_accepted_inputs(manifest["accepted_inputs"])
    _validate_workspace(
        manifest["workspace"],
        action_plan_sha256=accepted["action_plan"]["action_plan_sha256"],
    )
    families = _validate_families(manifest["families"])
    _validate_payload_inventory(manifest["payload_inventory"], families)
    _validate_boolean_claims(
        manifest["claims"],
        "composed-bundle manifest claims",
        positive=MANIFEST_POSITIVE_CLAIMS,
        negative=MANIFEST_FALSE_CLAIMS,
    )
    if manifest["implementation"] != COMPOSED_BUNDLE_MANIFEST_IMPLEMENTATION:
        raise BulkloadError("composed-bundle manifest implementation differs")
    _validate_self_digest(
        manifest,
        "manifest_sha256",
        "composed-bundle manifest",
    )


def _validate_capacity_admission(value: Any) -> dict[str, Any]:
    admission = _require_exact_keys(
        value,
        {
            "capacity_observation_sha256",
            "host_authority_id",
            "expires_at",
            "observations",
            "space_reserved",
            "future_write_guaranteed",
            "quota_proof",
        },
        "composition capacity admission",
    )
    _require_sha256(
        admission["capacity_observation_sha256"],
        "capacity-admission observation digest",
    )
    _require_uuid(admission["host_authority_id"], "capacity-admission host ID")
    expires = _parse_timestamp(
        admission["expires_at"],
        "capacity-admission expiry",
    )
    observations = admission["observations"]
    if not isinstance(observations, list) or len(observations) < 6:
        raise BulkloadError("capacity-admission observations are incomplete")
    expected_start = ["pre-staging", "post-staging-pre-ticket"]
    expected_end = ["pre-metadata", "pre-seal"]
    if (
        [item.get("phase") for item in observations[:2]] != expected_start
        or [item.get("phase") for item in observations[-2:]] != expected_end
        or (len(observations) - 4) % 2
    ):
        raise BulkloadError("capacity-admission phase sequence differs")
    family_names: list[str] = []
    previous_at: datetime | None = None
    # The verifier cross-binds the first pair to the v6 request. Requiring the
    # same positive pair here makes that complete requirement authoritative for
    # every later phase; claimed progress can never reduce admission pressure.
    conservative_requirement: tuple[int, int] | None = None
    for index, item in enumerate(observations):
        record = _require_exact_keys(
            item,
            {
                "phase",
                "family",
                "observed_at",
                "available_bytes",
                "available_inodes",
                "remaining_required_bytes",
                "remaining_required_inodes",
                "sufficient",
            },
            f"capacity-admission observation {index}",
        )
        if record["phase"] not in {
            "pre-staging",
            "post-staging-pre-ticket",
            "pre-family-baseline",
            "pre-family-transaction",
            "pre-metadata",
            "pre-seal",
        }:
            raise BulkloadError("capacity-admission phase is invalid")
        observed_at = _parse_timestamp(
            record["observed_at"],
            f"capacity-admission observation {index} timestamp",
        )
        if observed_at >= expires or (
            previous_at is not None and observed_at < previous_at
        ):
            raise BulkloadError("capacity-admission timestamps differ")
        previous_at = observed_at
        for key in (
            "available_bytes",
            "available_inodes",
            "remaining_required_bytes",
            "remaining_required_inodes",
        ):
            _require_integer(record[key], f"capacity-admission {key}")
        observed_requirement = (
            record["remaining_required_bytes"],
            record["remaining_required_inodes"],
        )
        if (
            observed_requirement[0] < 1
            or observed_requirement[1] < 1
            or (
                conservative_requirement is not None
                and observed_requirement != conservative_requirement
            )
        ):
            raise BulkloadError("capacity-admission conservative requirement differs")
        conservative_requirement = observed_requirement
        sufficient = (
            record["available_bytes"] >= record["remaining_required_bytes"]
            and record["available_inodes"] >= record["remaining_required_inodes"]
        )
        if record["sufficient"] is not sufficient or not sufficient:
            raise BulkloadError("capacity-admission sufficiency differs")
        if record["phase"] in {
            "pre-family-baseline",
            "pre-family-transaction",
        }:
            _require_leaf(record["family"], "capacity-admission family")
        elif record["family"] is not None:
            raise BulkloadError("capacity-admission non-family phase names a family")
    middle = observations[2:-2]
    for index in range(0, len(middle), 2):
        baseline, transaction = middle[index : index + 2]
        if (
            baseline["phase"] != "pre-family-baseline"
            or transaction["phase"] != "pre-family-transaction"
            or baseline["family"] != transaction["family"]
        ):
            raise BulkloadError("capacity-admission family phases differ")
        family_names.append(baseline["family"])
    if family_names != sorted(set(family_names)):
        raise BulkloadError("capacity-admission families are not canonical")
    if (
        admission["space_reserved"] is not False
        or admission["future_write_guaranteed"] is not False
        or admission["quota_proof"] is not False
    ):
        raise BulkloadError("capacity admission overclaims filesystem authority")
    return admission


def _validate_workspace_claim(
    value: Any,
    *,
    action_plan_sha256: str,
) -> dict[str, Any]:
    claim = _require_exact_keys(
        value,
        {
            "workspace",
            "cooperating_lock_acquired",
            "exclusive_staging_claimed",
            "final_leaf_observed_absent",
            "same_parent_staging",
            "ticket_nonce_sha256",
            "ticket_consumed",
            "failure_policy",
        },
        "composition workspace claim",
    )
    _validate_workspace(
        claim["workspace"],
        action_plan_sha256=action_plan_sha256,
    )
    if (
        claim["cooperating_lock_acquired"] is not True
        or claim["exclusive_staging_claimed"] is not True
        or claim["final_leaf_observed_absent"] is not True
        or claim["same_parent_staging"] is not True
        or claim["ticket_consumed"] is not True
        or claim["failure_policy"] != "preserve-fail-held-no-cleanup-v1"
    ):
        raise BulkloadError("composition workspace claim differs")
    _require_sha256(
        claim["ticket_nonce_sha256"],
        "composition ticket-nonce digest",
    )
    return claim


def _validate_operation_graph(value: Any) -> dict[str, Any]:
    graph = _require_exact_keys(
        value,
        {
            "action_plan_operation_graph_sha256",
            "completed_node_ids",
            "state_transitions",
            "state_transitions_sha256",
            "self_checks",
        },
        "composition operation graph",
    )
    _require_sha256(
        graph["action_plan_operation_graph_sha256"],
        "action-plan operation-graph digest",
    )
    nodes = graph["completed_node_ids"]
    if not isinstance(nodes, list) or not nodes or len(nodes) > MAX_TREE_ENTRIES:
        raise BulkloadError("completed operation nodes are invalid")
    if (
        any(
            not isinstance(node, str) or _NODE_ID.fullmatch(node) is None
            for node in nodes
        )
        or nodes != sorted(set(nodes))
        or "write-manifest" not in nodes
        or bool(PRESEAL_RECEIPT_EXCLUDED_NODE_IDS & set(nodes))
    ):
        raise BulkloadError("completed operation nodes are not canonical pre-seal")
    if graph["state_transitions"] != PRESEAL_RECEIPT_TRANSITIONS:
        raise BulkloadError("writer state transitions differ")
    _require_sha256(
        graph["state_transitions_sha256"],
        "writer state-transition digest",
    )
    if (
        sha256_bytes(canonical_bytes(graph["state_transitions"]))
        != graph["state_transitions_sha256"]
    ):
        raise BulkloadError("writer state-transition digest mismatch")
    _validate_self_checks(graph["self_checks"])
    return graph


def _validate_manifest_binding(
    value: Any,
    label: str,
) -> dict[str, Any]:
    binding = _require_exact_keys(
        value,
        {
            "schema",
            "manifest_sha256",
            "canonical_file_sha256",
            "canonical_file_bytes",
        },
        label,
    )
    if binding["schema"] != COMPOSED_BUNDLE_MANIFEST_SCHEMA:
        raise BulkloadError(f"{label} schema differs")
    _require_sha256(binding["manifest_sha256"], f"{label} self-digest")
    _require_sha256(
        binding["canonical_file_sha256"],
        f"{label} canonical-file digest",
    )
    byte_count = _require_integer(
        binding["canonical_file_bytes"],
        f"{label} byte count",
    )
    if byte_count < 2 or byte_count > MAX_PROTOCOL_BYTES:
        raise BulkloadError(f"{label} exceeds its byte budget")
    return binding


def _validate_self_checks(value: Any) -> dict[str, Any]:
    fields = {
        "input_chain_recomputed",
        "schema_verified",
        "migrations_verified",
        "edges_verified",
        "counts_verified",
        "semantic_digests_verified",
        "integrity_check_passed",
        "foreign_key_check_passed",
        "tree_exact",
        "sidecars_absent",
    }
    checks = _require_exact_keys(value, fields, "writer self-checks")
    if any(checks[key] is not True for key in fields):
        raise BulkloadError("writer self-checks differ")
    return checks


def validate_composition_receipt(value: dict[str, Any]) -> None:
    """Validate one strict pre-seal v7 composition receipt."""
    receipt = _require_exact_keys(
        value,
        {
            "schema",
            "completed_at",
            "composition_id",
            "producer_lineage",
            "sqlite_engine_authority",
            "accepted_inputs",
            "capacity_admission",
            "workspace_claim",
            "operation_graph",
            "manifest",
            "claims",
            "implementation",
            "receipt_sha256",
        },
        "composition receipt",
    )
    if receipt["schema"] != COMPOSITION_RECEIPT_SCHEMA:
        raise BulkloadError("composition receipt schema differs")
    completed_at = _parse_timestamp(
        receipt["completed_at"],
        "composition receipt completed_at",
    )
    _require_uuid(receipt["composition_id"], "composition receipt ID")
    _validate_producer_lineage(receipt["producer_lineage"])
    _validate_sqlite_engine_authority(receipt["sqlite_engine_authority"])
    accepted = _validate_accepted_inputs(receipt["accepted_inputs"])
    admission = _validate_capacity_admission(receipt["capacity_admission"])
    if (
        admission["capacity_observation_sha256"]
        != accepted["capacity_observation"]["observation_sha256"]
        or completed_at
        >= _parse_timestamp(
            admission["expires_at"],
            "capacity-admission expiry",
        )
        or completed_at
        < _parse_timestamp(
            admission["observations"][-1]["observed_at"],
            "final capacity observation timestamp",
        )
    ):
        raise BulkloadError("composition receipt capacity binding differs")
    _validate_workspace_claim(
        receipt["workspace_claim"],
        action_plan_sha256=accepted["action_plan"]["action_plan_sha256"],
    )
    _validate_operation_graph(receipt["operation_graph"])
    _validate_manifest_binding(receipt["manifest"], "receipt manifest binding")
    _validate_boolean_claims(
        receipt["claims"],
        "composition receipt claims",
        positive=RECEIPT_POSITIVE_CLAIMS,
        negative=RECEIPT_FALSE_CLAIMS,
    )
    if receipt["implementation"] != COMPOSITION_RECEIPT_IMPLEMENTATION:
        raise BulkloadError("composition receipt implementation differs")
    _validate_self_digest(
        receipt,
        "receipt_sha256",
        "composition receipt",
    )


def _validate_tree_entry(value: Any, label: str) -> dict[str, Any]:
    entry = _require_exact_keys(
        value,
        {"relative_path", "type", "mode", "size", "sha256"},
        label,
    )
    path = _require_relative_path(entry["relative_path"], f"{label} path")
    if entry["type"] == "directory":
        if (
            path != "sqlite"
            or entry["mode"] != 0o700
            or entry["size"] is not None
            or entry["sha256"] is not None
        ):
            raise BulkloadError(f"{label} directory binding differs")
    elif entry["type"] == "regular-file":
        if entry["mode"] != 0o600:
            raise BulkloadError(f"{label} file mode differs")
        _require_integer(entry["size"], f"{label} size")
        _require_sha256(entry["sha256"], f"{label} digest")
    else:
        raise BulkloadError(f"{label} type is unsupported")
    return entry


def _validate_bundle(value: Any, *, action_plan_sha256: str) -> dict[str, Any]:
    bundle = _require_exact_keys(
        value,
        {
            "resolved_final_path",
            "final_leaf",
            "identity",
            "mount",
            "lineage",
            "lineage_sha256",
            "tree_inventory",
            "tree_inventory_sha256",
        },
        "verified bundle",
    )
    path = _require_absolute_path(
        bundle["resolved_final_path"],
        "verified bundle path",
    )
    final_leaf = _require_leaf(bundle["final_leaf"], "verified bundle leaf")
    if (
        PurePosixPath(path).name != final_leaf
        or final_leaf != f"bulkload-sqlite-compose-{action_plan_sha256}"
    ):
        raise BulkloadError("verified bundle final leaf differs")
    identity = _validate_mode_identity(
        bundle["identity"],
        "verified bundle identity",
        required_mode=0o700,
    )
    mount = _validate_mount(bundle["mount"], "verified bundle mount")
    if mount["device"] != identity["device"]:
        raise BulkloadError("verified bundle mount device differs")
    lineage = _validate_lineage(
        bundle["lineage"],
        "verified bundle lineage",
        leaf_identity=identity,
        leaf_mount=mount,
    )
    _require_sha256(
        bundle["lineage_sha256"],
        "verified bundle lineage digest",
    )
    if sha256_bytes(canonical_bytes(lineage)) != bundle["lineage_sha256"]:
        raise BulkloadError("verified bundle lineage digest mismatch")
    entries = bundle["tree_inventory"]
    if not isinstance(entries, list) or len(entries) > MAX_TREE_ENTRIES:
        raise BulkloadError("verified bundle tree exceeds its bound")
    for index, entry in enumerate(entries):
        _validate_tree_entry(entry, f"verified bundle entry {index}")
    paths = [entry["relative_path"] for entry in entries]
    if paths != sorted(set(paths)):
        raise BulkloadError("verified bundle tree is not canonical")
    _require_sha256(
        bundle["tree_inventory_sha256"],
        "verified bundle tree digest",
    )
    if sha256_bytes(canonical_bytes(entries)) != bundle["tree_inventory_sha256"]:
        raise BulkloadError("verified bundle tree digest mismatch")
    return bundle


def _validate_observed_artifact(
    value: Any,
    label: str,
    *,
    schema: str,
    self_digest_field: str,
) -> dict[str, Any]:
    observation = _require_exact_keys(
        value,
        {
            "schema",
            self_digest_field,
            "canonical_file_sha256",
            "canonical_file_bytes",
            "strictly_valid",
        },
        label,
    )
    if observation["strictly_valid"] is True:
        if observation["schema"] != schema:
            raise BulkloadError(f"{label} schema differs")
        _require_sha256(
            observation[self_digest_field],
            f"{label} self-digest",
        )
    elif observation["strictly_valid"] is False:
        if (
            observation["schema"] is not None
            and not isinstance(observation["schema"], str)
        ) or (
            observation[self_digest_field] is not None
            and (
                not isinstance(observation[self_digest_field], str)
                or _SHA256.fullmatch(observation[self_digest_field]) is None
            )
        ):
            raise BulkloadError(f"{label} invalid-body binding is malformed")
    else:
        raise BulkloadError(f"{label} validity must be boolean")
    if observation["canonical_file_sha256"] is None:
        if observation["canonical_file_bytes"] is not None:
            raise BulkloadError(f"{label} absent-file binding differs")
    else:
        _require_sha256(
            observation["canonical_file_sha256"],
            f"{label} canonical-file digest",
        )
        byte_count = _require_integer(
            observation["canonical_file_bytes"],
            f"{label} canonical-file byte count",
        )
        if byte_count > MAX_PROTOCOL_BYTES:
            raise BulkloadError(f"{label} file exceeds its byte budget")
    if observation["strictly_valid"] is True and (
        observation["canonical_file_sha256"] is None
        or observation["canonical_file_bytes"] is None
    ):
        raise BulkloadError(f"{label} valid body lacks a complete-file binding")
    return observation


def _validate_family_verification(value: Any, label: str) -> dict[str, Any]:
    verification = _require_exact_keys(
        value,
        {"observed", *_FAMILY_VERIFICATION_FIELDS},
        label,
    )
    _validate_family(verification["observed"], f"{label} observed family")
    for field in _FAMILY_VERIFICATION_FIELDS:
        if type(verification[field]) is not bool:
            raise BulkloadError(f"{label} {field} must be boolean")
    return verification


def _validate_oracle_family_observation(
    value: Any,
    label: str,
) -> dict[str, Any]:
    observation = _require_exact_keys(
        value,
        {"observed", *_ORACLE_FAMILY_OBSERVATION_FIELDS},
        label,
    )
    _validate_family(observation["observed"], f"{label} observed family")
    for field in _ORACLE_FAMILY_OBSERVATION_FIELDS:
        if type(observation[field]) is not bool:
            raise BulkloadError(f"{label} {field} must be boolean")
    return observation


def _validate_failure(value: Any, label: str) -> dict[str, Any]:
    failure = _require_exact_keys(
        value,
        {"code", "scope", "family", "field"},
        label,
    )
    if (
        not isinstance(failure["code"], str)
        or _FAILURE_CODE.fullmatch(failure["code"]) is None
        or failure["scope"] not in _FAILURE_SCOPES
    ):
        raise BulkloadError(f"{label} code or scope is invalid")
    if failure["family"] is not None:
        _require_leaf(failure["family"], f"{label} family")
    if failure["field"] is not None and (
        not isinstance(failure["field"], str)
        or _FIELD.fullmatch(failure["field"]) is None
    ):
        raise BulkloadError(f"{label} field locator is invalid")
    return failure


def _expected_success_tree(
    families: list[dict[str, Any]],
    manifest: dict[str, Any],
    receipt: dict[str, Any],
) -> list[dict[str, Any]]:
    entries = [
        {
            "relative_path": "composition-receipt.json",
            "type": "regular-file",
            "mode": 0o600,
            "size": receipt["canonical_file_bytes"],
            "sha256": receipt["canonical_file_sha256"],
        },
        {
            "relative_path": "manifest.json",
            "type": "regular-file",
            "mode": 0o600,
            "size": manifest["canonical_file_bytes"],
            "sha256": manifest["canonical_file_sha256"],
        },
        {
            "relative_path": "sqlite",
            "type": "directory",
            "mode": 0o700,
            "size": None,
            "sha256": None,
        },
    ]
    entries.extend(_payload_projection(family["observed"]) for family in families)
    return sorted(entries, key=lambda item: item["relative_path"])


def validate_verifier_oracle_report(value: dict[str, Any]) -> None:
    """Validate a non-authoritative, in-memory v7 verifier observation.

    Observed checks describe what the read-only oracle compared during this
    call. They never become a bundle-seal or independent-verification receipt
    claim, even when every comparison succeeds.
    """
    report = _require_exact_keys(
        value,
        {
            "schema",
            "observed_at",
            "observation_id",
            "verifier_runtime_authority",
            "accepted_artifacts",
            "bundle_observation",
            "manifest_observation",
            "composition_receipt_observation",
            "families",
            "failures",
            "observed_checks",
            "claims",
            "implementation",
            "oracle_report_sha256",
        },
        "verifier oracle report",
    )
    if report["schema"] != VERIFIER_ORACLE_REPORT_SCHEMA:
        raise BulkloadError("verifier oracle-report schema differs")
    _parse_timestamp(report["observed_at"], "oracle report observed_at")
    _require_uuid(report["observation_id"], "oracle observation ID")
    _validate_runtime_source_binding(
        report["verifier_runtime_authority"],
        "oracle verifier runtime authority",
        required_source_leaf="private_sqlite_verifier.py",
    )
    accepted = _validate_accepted_inputs(report["accepted_artifacts"])
    bundle = _validate_bundle(
        report["bundle_observation"],
        action_plan_sha256=accepted["action_plan"]["action_plan_sha256"],
    )
    manifest = _validate_observed_artifact(
        report["manifest_observation"],
        "oracle manifest observation",
        schema=COMPOSED_BUNDLE_MANIFEST_SCHEMA,
        self_digest_field="manifest_sha256",
    )
    composition_receipt = _validate_observed_artifact(
        report["composition_receipt_observation"],
        "oracle composition-receipt observation",
        schema=COMPOSITION_RECEIPT_SCHEMA,
        self_digest_field="receipt_sha256",
    )
    families = report["families"]
    if not isinstance(families, list) or len(families) > MAX_FAMILIES:
        raise BulkloadError("oracle family observations exceed their bound")
    for index, family in enumerate(families):
        _validate_oracle_family_observation(
            family,
            f"oracle family observation {index}",
        )
    basenames = [family["observed"]["basename"] for family in families]
    if basenames != sorted(set(basenames)):
        raise BulkloadError("oracle family observations are not canonical")
    failures = _require_canonical_list(
        report["failures"],
        "verifier oracle failures",
        maximum=MAX_FAILURES,
    )
    for index, failure in enumerate(failures):
        _validate_failure(failure, f"oracle failure {index}")
    checks = _require_exact_keys(
        report["observed_checks"],
        _ORACLE_OBSERVED_CHECKS,
        "verifier oracle observed checks",
    )
    if any(type(checks[key]) is not bool for key in _ORACLE_OBSERVED_CHECKS):
        raise BulkloadError("verifier oracle observed checks must be boolean")
    if not failures and (
        not families
        or manifest["strictly_valid"] is not True
        or composition_receipt["strictly_valid"] is not True
        or any(checks[key] is not True for key in _ORACLE_OBSERVED_CHECKS)
        or any(
            family[field] is not True
            for family in families
            for field in _ORACLE_FAMILY_OBSERVATION_FIELDS
        )
        or bundle["tree_inventory"]
        != _expected_success_tree(families, manifest, composition_receipt)
    ):
        raise BulkloadError("zero-failure oracle observations are incomplete")
    _validate_boolean_claims(
        report["claims"],
        "verifier oracle claims",
        positive=set(),
        negative=ORACLE_FALSE_CLAIMS,
    )
    if report["implementation"] != VERIFIER_ORACLE_REPORT_IMPLEMENTATION:
        raise BulkloadError("verifier oracle implementation differs")
    _validate_self_digest(
        report,
        "oracle_report_sha256",
        "verifier oracle report",
    )


def validate_independent_verification_receipt(
    value: dict[str, Any],
) -> None:
    """Validate a strict in-memory or separately published v7 verifier report."""
    receipt = _require_exact_keys(
        value,
        {
            "schema",
            "verified_at",
            "verification_id",
            "verifier_runtime_authority",
            "sqlite_engine_authority",
            "accepted_inputs",
            "bundle",
            "manifest",
            "composition_receipt",
            "families",
            "failures",
            "claims",
            "implementation",
            "verification_sha256",
        },
        "independent verification receipt",
    )
    if receipt["schema"] != INDEPENDENT_VERIFICATION_RECEIPT_SCHEMA:
        raise BulkloadError("independent verification receipt schema differs")
    _parse_timestamp(receipt["verified_at"], "verification receipt verified_at")
    _require_uuid(receipt["verification_id"], "verification receipt ID")
    _validate_runtime_source_binding(
        receipt["verifier_runtime_authority"],
        "verifier runtime authority",
        required_source_leaf="private_sqlite_verifier.py",
    )
    _validate_sqlite_engine_authority(receipt["sqlite_engine_authority"])
    accepted = _validate_accepted_inputs(receipt["accepted_inputs"])
    bundle = _validate_bundle(
        receipt["bundle"],
        action_plan_sha256=accepted["action_plan"]["action_plan_sha256"],
    )
    manifest = _validate_observed_artifact(
        receipt["manifest"],
        "observed manifest",
        schema=COMPOSED_BUNDLE_MANIFEST_SCHEMA,
        self_digest_field="manifest_sha256",
    )
    composition_receipt = _validate_observed_artifact(
        receipt["composition_receipt"],
        "observed composition receipt",
        schema=COMPOSITION_RECEIPT_SCHEMA,
        self_digest_field="receipt_sha256",
    )
    families = receipt["families"]
    if not isinstance(families, list) or len(families) > MAX_FAMILIES:
        raise BulkloadError("verified families exceed their bound")
    for index, family in enumerate(families):
        _validate_family_verification(family, f"verified family {index}")
    basenames = [family["observed"]["basename"] for family in families]
    if basenames != sorted(set(basenames)):
        raise BulkloadError("verified families are not canonical")
    failures = _require_canonical_list(
        receipt["failures"],
        "independent verification failures",
        maximum=MAX_FAILURES,
    )
    for index, failure in enumerate(failures):
        _validate_failure(failure, f"verification failure {index}")
    success = not failures
    if success:
        if (
            not families
            or manifest["strictly_valid"] is not True
            or composition_receipt["strictly_valid"] is not True
            or any(
                family[field] is not True
                for family in families
                for field in _FAMILY_VERIFICATION_FIELDS
            )
            or bundle["tree_inventory"]
            != _expected_success_tree(families, manifest, composition_receipt)
        ):
            raise BulkloadError("successful verification evidence is incomplete")
    _validate_boolean_claims(
        receipt["claims"],
        "independent verification claims",
        positive=_VERIFICATION_SUCCESS_CLAIMS,
        negative=_VERIFICATION_FALSE_CLAIMS,
        success=success,
    )
    if receipt["implementation"] != INDEPENDENT_VERIFICATION_RECEIPT_IMPLEMENTATION:
        raise BulkloadError("independent verification implementation differs")
    _validate_self_digest(
        receipt,
        "verification_sha256",
        "independent verification receipt",
    )
