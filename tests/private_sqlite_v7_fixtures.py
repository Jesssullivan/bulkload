from __future__ import annotations

from copy import deepcopy
from dataclasses import dataclass
import hashlib
import os
from pathlib import Path
import sqlite3
import stat
import struct
from typing import Any

from bulkload_lib.model import canonical_bytes, object_digest, sha256_bytes
from bulkload_lib.private_sqlite_protocol import (
    ACTION_PLAN_SCHEMA,
    CAPACITY_OBSERVATION_SCHEMA,
    COMPOSED_BUNDLE_MANIFEST_IMPLEMENTATION,
    COMPOSED_BUNDLE_MANIFEST_SCHEMA,
    COMPOSE_REQUEST_SCHEMA,
    COMPOSITION_RECEIPT_IMPLEMENTATION,
    COMPOSITION_RECEIPT_SCHEMA,
    MAX_CHECKED_INTEGER,
    MANIFEST_FALSE_CLAIMS,
    MANIFEST_POSITIVE_CLAIMS,
    ORACLE_FALSE_CLAIMS,
    PRESEAL_RECEIPT_EXCLUDED_NODE_IDS,
    PRESEAL_RECEIPT_TRANSITIONS,
    RECEIPT_FALSE_CLAIMS,
    RECEIPT_POSITIVE_CLAIMS,
    RUNTIME_AUTHORITY_SCHEMA,
    V5_POLICY_SHA256,
    V5_RUNTIME_SHA256,
    V5_SOURCE_COMMIT,
    V6_POLICY_SHA256,
    V6_RUNTIME_SHA256,
    V6_SOURCE_COMMIT,
    VERIFIER_ORACLE_REPORT_IMPLEMENTATION,
    VERIFIER_ORACLE_REPORT_SCHEMA,
)


COMPOSITION_ID = "11111111-1111-4111-8111-111111111111"
OBSERVATION_ID = "22222222-2222-4222-8222-222222222222"
HOST_AUTHORITY_ID = "33333333-3333-4333-8333-333333333333"
STAGING_ID = "44444444-4444-4444-8444-444444444444"
CREATED_AT = "2026-07-29T18:00:00Z"
MANIFEST_CREATED_AT = "2026-07-29T18:06:30Z"
COMPLETED_AT = "2026-07-29T18:07:00Z"
EXPIRES_AT = "2026-07-29T19:00:00Z"
BASENAME = "state_5.sqlite"

ORACLE_OBSERVED_CHECKS = {
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
ORACLE_FAMILY_FIELDS = {
    "manifest_matches_observed",
    "action_plan_matches_observed",
    "integrity_check_observed",
    "foreign_key_check_observed",
    "schema_matches_action_observed",
    "migration_matches_action_observed",
    "edge_matches_action_observed",
    "semantic_output_matches_action_observed",
}
WRITER_TRANSITIONS = list(PRESEAL_RECEIPT_TRANSITIONS)
PRE_RECEIPT_COMPLETED_NODE_IDS = sorted(
    {
        f"baseline:{BASENAME}",
        "create-versioned-staging",
        f"source-only:{BASENAME}:items",
        f"verify:{BASENAME}",
        "write-manifest",
    }
)


def self_digest(value: dict[str, Any], field: str) -> dict[str, Any]:
    value[field] = object_digest(value, field)
    return value


def canonical_file(value: dict[str, Any]) -> bytes:
    return canonical_bytes(value) + b"\n"


def representative_action_graph(
    *,
    basename: str = BASENAME,
    table: str = "items",
    baseline_row_count: int = 1,
    source_only_row_count: int = 1,
    baseline_sha256: str = "5" * 64,
    source_only_sha256: str = "6" * 64,
    family_sha256: str = "7" * 64,
) -> dict[str, Any]:
    baseline_id = f"baseline:{basename}"
    source_only_id = f"source-only:{basename}:{table}"
    verifier_id = f"verify:{basename}"

    def node(
        identifier: str,
        kind: str,
        depends_on: list[str],
        *,
        family: str | None = None,
        table_name: str | None = None,
        row_count: int | None = None,
        expected_sha256: str | None = None,
    ) -> dict[str, Any]:
        return {
            "id": identifier,
            "kind": kind,
            "depends_on": depends_on,
            "family": family,
            "table": table_name,
            "row_count": row_count,
            "expected_sha256": expected_sha256,
        }

    return {
        "descriptive_only": True,
        "create_only": True,
        "existing_destination_mutation": False,
        "blocked": False,
        "nodes": [
            node(
                "create-versioned-staging",
                "create-versioned-owner-private-staging",
                [],
            ),
            node(
                baseline_id,
                "stream-destination-family-baseline",
                ["create-versioned-staging"],
                family=basename,
                row_count=baseline_row_count,
                expected_sha256=baseline_sha256,
            ),
            node(
                source_only_id,
                "insert-source-only-canonical-identity-order",
                [baseline_id],
                family=basename,
                table_name=table,
                row_count=source_only_row_count,
                expected_sha256=source_only_sha256,
            ),
            node(
                verifier_id,
                "verify-family-schema-header-edges-and-output-digests",
                [source_only_id],
                family=basename,
                expected_sha256=family_sha256,
            ),
            node(
                "write-manifest",
                "write-canonical-bundle-manifest",
                [verifier_id],
            ),
            node(
                "write-receipt",
                "write-canonical-composition-receipt",
                ["write-manifest"],
            ),
            node(
                "fsync-bundle",
                "fsync-complete-files-and-directories",
                ["write-receipt"],
            ),
            node(
                "seal-bundle",
                "seal-complete-bundle-with-no-replace-rename",
                ["fsync-bundle"],
            ),
        ],
    }


def completed_before_receipt(operation_graph: dict[str, Any]) -> list[str]:
    return sorted(
        node["id"]
        for node in operation_graph["nodes"]
        if node["id"] not in PRESEAL_RECEIPT_EXCLUDED_NODE_IDS
    )


def runtime_source_binding(source_leaf: str) -> dict[str, Any]:
    source_path = f"scripts/bulkload_lib/{source_leaf}"
    source_sha256 = sha256_bytes(source_leaf.encode("utf-8"))
    authority = {
        "schema": RUNTIME_AUTHORITY_SCHEMA,
        "policy_schema": "dev.tinyland.bulkload.codex-private-state-policy.v7",
        "policy_sha256": sha256_bytes(b"hand-built-v7-policy"),
        "runtime_source_sha256": sha256_bytes(b"hand-built-v7-runtime"),
        "source_digests": {source_path: source_sha256},
    }
    return {
        "runtime_authority": authority,
        "source_path": source_path,
        "source_sha256": source_sha256,
    }


def producer_lineage() -> dict[str, Any]:
    return {
        "action_plan_producer": {
            "source_commit": V5_SOURCE_COMMIT,
            "policy_sha256": V5_POLICY_SHA256,
            "runtime_source_sha256": V5_RUNTIME_SHA256,
        },
        "compose_request_producer": {
            "source_commit": V6_SOURCE_COMMIT,
            "policy_sha256": V6_POLICY_SHA256,
            "runtime_source_sha256": V6_RUNTIME_SHA256,
        },
        "writer_runtime_authority": runtime_source_binding(
            "private_sqlite_composer.py"
        ),
    }


def accepted_artifacts(
    action_plan: dict[str, Any] | None = None,
    compose_request: dict[str, Any] | None = None,
    capacity_observation: dict[str, Any] | None = None,
) -> dict[str, Any]:
    action_plan = action_plan or {
        "schema": ACTION_PLAN_SCHEMA,
        "action_plan_sha256": "a" * 64,
    }
    compose_request = compose_request or {
        "schema": COMPOSE_REQUEST_SCHEMA,
        "request_sha256": "b" * 64,
    }
    capacity_observation = capacity_observation or {
        "schema": CAPACITY_OBSERVATION_SCHEMA,
        "observation_sha256": "c" * 64,
    }

    def binding(value: dict[str, Any], digest_field: str) -> dict[str, Any]:
        payload = canonical_file(value)
        return {
            "schema": value["schema"],
            digest_field: value[digest_field],
            "canonical_file_sha256": sha256_bytes(payload),
            "canonical_file_bytes": len(payload),
        }

    return {
        "action_plan": binding(action_plan, "action_plan_sha256"),
        "compose_request": binding(compose_request, "request_sha256"),
        "capacity_observation": binding(
            capacity_observation,
            "observation_sha256",
        ),
    }


def identity(
    *,
    inode: int,
    mode: int,
    device: int = 1,
    uid: int | None = None,
) -> dict[str, int]:
    return {
        "device": device,
        "inode": inode,
        "uid": os.getuid() if uid is None else uid,
        "mode": mode,
    }


def mount(*, device: int = 1) -> dict[str, int | None]:
    return {
        "device": device,
        "filesystem_id": 7,
        "linux_mount_id": None,
    }


def lineage(
    leaf_identity: dict[str, Any],
    leaf_mount: dict[str, Any],
) -> list[dict[str, Any]]:
    return [
        {
            "identity": deepcopy(leaf_identity),
            "mount": deepcopy(leaf_mount),
        }
    ]


def sqlite_engine_authority(connection: sqlite3.Connection) -> dict[str, Any]:
    options = sorted(
        str(row[0]) for row in connection.execute("PRAGMA compile_options")
    )
    source_id = connection.execute("SELECT sqlite_source_id()").fetchone()
    threadsafe = next(
        int(option.split("=", 1)[1])
        for option in options
        if option.startswith("THREADSAFE=")
    )
    return {
        "sqlite_version": sqlite3.sqlite_version,
        "sqlite_source_id": source_id[0],
        "compile_options": options,
        "compile_options_sha256": sha256_bytes(canonical_bytes(options)),
        "threadsafe": threadsafe,
    }


def handbuilt_family(
    *,
    basename: str = BASENAME,
    size: int = 4096,
    physical_sha256: str = "d" * 64,
    schema_sha256: str = "e" * 64,
    semantic_sha256: str = "f" * 64,
    migration_sha256: str = "0" * 64,
    edge_sha256: str = "1" * 64,
) -> dict[str, Any]:
    relative_path = f"sqlite/{basename}"
    return {
        "basename": basename,
        "relative_path": relative_path,
        "mode": 0o600,
        "size": size,
        "sha256": physical_sha256,
        "journal_mode": "delete",
        "application_id": 0,
        "user_version": 0,
        "schema": {
            "raw_schema_sha256": schema_sha256,
            "schema_contract_sha256": schema_sha256,
            "structured_schema_sha256": schema_sha256,
        },
        "migrations": {
            "migrations_sha256": migration_sha256,
            "exact": True,
        },
        "edges": {
            "registry_sha256": edge_sha256,
            "observed_sha256": edge_sha256,
            "closed": True,
        },
        "tables": [
            {
                "name": "items",
                "identity_columns": ["id"],
                "row_count": 3,
                "semantic_rows_sha256": semantic_sha256,
                "schema_sha256": schema_sha256,
                "foreign_keys_sha256": edge_sha256,
            }
        ],
        "absent_sidecars": sorted(
            f"{relative_path}{suffix}" for suffix in ("-journal", "-shm", "-wal")
        ),
    }


def workspace(
    *,
    action_plan_sha256: str,
    resolved_parent: str = "/private/compose-workspace",
    parent_identity: dict[str, Any] | None = None,
    parent_mount: dict[str, Any] | None = None,
    parent_lineage: list[dict[str, Any]] | None = None,
    staging_identity: dict[str, Any] | None = None,
) -> dict[str, Any]:
    parent_identity = parent_identity or identity(inode=10, mode=0o700)
    parent_mount = parent_mount or mount()
    parent_lineage = parent_lineage or lineage(parent_identity, parent_mount)
    final_leaf = f"bulkload-sqlite-compose-{action_plan_sha256}"
    return {
        "resolved_parent": resolved_parent,
        "parent_identity": deepcopy(parent_identity),
        "mount": deepcopy(parent_mount),
        "lineage": deepcopy(parent_lineage),
        "lineage_sha256": sha256_bytes(canonical_bytes(parent_lineage)),
        "staging_leaf": f".{final_leaf}-staging-{STAGING_ID}",
        "staging_identity": staging_identity or identity(inode=11, mode=0o700),
        "final_leaf": final_leaf,
    }


def handbuilt_manifest(
    *,
    accepted: dict[str, Any] | None = None,
    family: dict[str, Any] | None = None,
    workspace_record: dict[str, Any] | None = None,
    engine: dict[str, Any] | None = None,
) -> dict[str, Any]:
    accepted = accepted or accepted_artifacts()
    family = family or handbuilt_family()
    workspace_record = workspace_record or workspace(
        action_plan_sha256=accepted["action_plan"]["action_plan_sha256"]
    )
    engine = engine or {
        "sqlite_version": "3.50.0",
        "sqlite_source_id": "hand-built-source-id",
        "compile_options": ["THREADSAFE=1"],
        "compile_options_sha256": sha256_bytes(canonical_bytes(["THREADSAFE=1"])),
        "threadsafe": 1,
    }
    entries = [
        {
            "relative_path": family["relative_path"],
            "type": "regular-file",
            "mode": family["mode"],
            "size": family["size"],
            "sha256": family["sha256"],
        }
    ]
    manifest = {
        "schema": COMPOSED_BUNDLE_MANIFEST_SCHEMA,
        "created_at": MANIFEST_CREATED_AT,
        "composition_id": COMPOSITION_ID,
        "producer_lineage": producer_lineage(),
        "sqlite_engine_authority": deepcopy(engine),
        "accepted_inputs": deepcopy(accepted),
        "workspace": deepcopy(workspace_record),
        "families": [deepcopy(family)],
        "payload_inventory": {
            "entries": entries,
            "inventory_sha256": sha256_bytes(canonical_bytes(entries)),
        },
        "claims": {
            **{name: True for name in MANIFEST_POSITIVE_CLAIMS},
            **{name: False for name in MANIFEST_FALSE_CLAIMS},
        },
        "implementation": COMPOSED_BUNDLE_MANIFEST_IMPLEMENTATION,
    }
    return self_digest(manifest, "manifest_sha256")


def capacity_admission(
    *,
    observation_sha256: str,
    required_bytes: int = 32 * 1024 * 1024,
    required_inodes: int = 16,
    basename: str = BASENAME,
) -> dict[str, Any]:
    phases = [
        ("pre-staging", None, "2026-07-29T18:01:00Z"),
        ("post-staging-pre-ticket", None, "2026-07-29T18:02:00Z"),
        ("pre-family-baseline", basename, "2026-07-29T18:03:00Z"),
        ("pre-family-transaction", basename, "2026-07-29T18:04:00Z"),
        ("pre-metadata", None, "2026-07-29T18:05:00Z"),
        ("pre-seal", None, "2026-07-29T18:06:00Z"),
    ]
    observations = [
        {
            "phase": phase,
            "family": family,
            "observed_at": observed_at,
            "available_bytes": required_bytes * 2,
            "available_inodes": required_inodes * 2,
            "remaining_required_bytes": required_bytes,
            "remaining_required_inodes": required_inodes,
            "sufficient": True,
        }
        for phase, family, observed_at in phases
    ]
    return {
        "capacity_observation_sha256": observation_sha256,
        "host_authority_id": HOST_AUTHORITY_ID,
        "expires_at": EXPIRES_AT,
        "observations": observations,
        "space_reserved": False,
        "future_write_guaranteed": False,
        "quota_proof": False,
    }


def handbuilt_receipt(
    manifest: dict[str, Any],
    *,
    completed_node_ids: list[str] | None = None,
    operation_graph_sha256: str = "2" * 64,
    required_bytes: int = 32 * 1024 * 1024,
    required_inodes: int = 16,
) -> dict[str, Any]:
    manifest_payload = canonical_file(manifest)
    accepted = manifest["accepted_inputs"]
    receipt = {
        "schema": COMPOSITION_RECEIPT_SCHEMA,
        "completed_at": COMPLETED_AT,
        "composition_id": manifest["composition_id"],
        "producer_lineage": deepcopy(manifest["producer_lineage"]),
        "sqlite_engine_authority": deepcopy(manifest["sqlite_engine_authority"]),
        "accepted_inputs": deepcopy(accepted),
        "capacity_admission": capacity_admission(
            observation_sha256=accepted["capacity_observation"]["observation_sha256"],
            required_bytes=required_bytes,
            required_inodes=required_inodes,
            basename=manifest["families"][0]["basename"],
        ),
        "workspace_claim": {
            "workspace": deepcopy(manifest["workspace"]),
            "cooperating_lock_acquired": True,
            "exclusive_staging_claimed": True,
            "final_leaf_observed_absent": True,
            "same_parent_staging": True,
            "ticket_nonce_sha256": sha256_bytes(b"hand-built-ticket"),
            "ticket_consumed": True,
            "failure_policy": "preserve-fail-held-no-cleanup-v1",
        },
        "operation_graph": {
            "action_plan_operation_graph_sha256": operation_graph_sha256,
            "completed_node_ids": (
                deepcopy(PRE_RECEIPT_COMPLETED_NODE_IDS)
                if completed_node_ids is None
                else completed_node_ids
            ),
            "state_transitions": deepcopy(WRITER_TRANSITIONS),
            "state_transitions_sha256": sha256_bytes(
                canonical_bytes(WRITER_TRANSITIONS)
            ),
            "self_checks": {
                "input_chain_recomputed": True,
                "schema_verified": True,
                "migrations_verified": True,
                "edges_verified": True,
                "counts_verified": True,
                "semantic_digests_verified": True,
                "integrity_check_passed": True,
                "foreign_key_check_passed": True,
                "tree_exact": True,
                "sidecars_absent": True,
            },
        },
        "manifest": {
            "schema": COMPOSED_BUNDLE_MANIFEST_SCHEMA,
            "manifest_sha256": manifest["manifest_sha256"],
            "canonical_file_sha256": sha256_bytes(manifest_payload),
            "canonical_file_bytes": len(manifest_payload),
        },
        "claims": {
            **{name: True for name in RECEIPT_POSITIVE_CLAIMS},
            **{name: False for name in RECEIPT_FALSE_CLAIMS},
        },
        "implementation": COMPOSITION_RECEIPT_IMPLEMENTATION,
    }
    return self_digest(receipt, "receipt_sha256")


def handbuilt_oracle_report(
    manifest: dict[str, Any] | None = None,
    receipt: dict[str, Any] | None = None,
) -> dict[str, Any]:
    manifest = manifest or handbuilt_manifest()
    receipt = receipt or handbuilt_receipt(manifest)
    family = deepcopy(manifest["families"][0])
    manifest_payload = canonical_file(manifest)
    receipt_payload = canonical_file(receipt)
    bundle_identity = identity(inode=12, mode=0o700)
    bundle_mount = mount()
    bundle_lineage = lineage(bundle_identity, bundle_mount)
    entries = sorted(
        [
            {
                "relative_path": "composition-receipt.json",
                "type": "regular-file",
                "mode": 0o600,
                "size": len(receipt_payload),
                "sha256": sha256_bytes(receipt_payload),
            },
            {
                "relative_path": "manifest.json",
                "type": "regular-file",
                "mode": 0o600,
                "size": len(manifest_payload),
                "sha256": sha256_bytes(manifest_payload),
            },
            {
                "relative_path": "sqlite",
                "type": "directory",
                "mode": 0o700,
                "size": None,
                "sha256": None,
            },
            {
                "relative_path": family["relative_path"],
                "type": "regular-file",
                "mode": family["mode"],
                "size": family["size"],
                "sha256": family["sha256"],
            },
        ],
        key=lambda item: item["relative_path"],
    )
    report = {
        "schema": VERIFIER_ORACLE_REPORT_SCHEMA,
        "observed_at": COMPLETED_AT,
        "observation_id": OBSERVATION_ID,
        "verifier_runtime_authority": runtime_source_binding(
            "private_sqlite_verifier.py"
        ),
        "accepted_artifacts": deepcopy(manifest["accepted_inputs"]),
        "bundle_observation": {
            "resolved_final_path": (
                f"{manifest['workspace']['resolved_parent']}/"
                f"{manifest['workspace']['final_leaf']}"
            ),
            "final_leaf": manifest["workspace"]["final_leaf"],
            "identity": bundle_identity,
            "mount": bundle_mount,
            "lineage": bundle_lineage,
            "lineage_sha256": sha256_bytes(canonical_bytes(bundle_lineage)),
            "tree_inventory": entries,
            "tree_inventory_sha256": sha256_bytes(canonical_bytes(entries)),
        },
        "manifest_observation": {
            "schema": COMPOSED_BUNDLE_MANIFEST_SCHEMA,
            "manifest_sha256": manifest["manifest_sha256"],
            "canonical_file_sha256": sha256_bytes(manifest_payload),
            "canonical_file_bytes": len(manifest_payload),
            "strictly_valid": True,
        },
        "composition_receipt_observation": {
            "schema": COMPOSITION_RECEIPT_SCHEMA,
            "receipt_sha256": receipt["receipt_sha256"],
            "canonical_file_sha256": sha256_bytes(receipt_payload),
            "canonical_file_bytes": len(receipt_payload),
            "strictly_valid": True,
        },
        "families": [
            {
                "observed": family,
                **{name: True for name in ORACLE_FAMILY_FIELDS},
            }
        ],
        "failures": [],
        "observed_checks": {name: True for name in ORACLE_OBSERVED_CHECKS},
        "claims": {name: False for name in ORACLE_FALSE_CLAIMS},
        "implementation": VERIFIER_ORACLE_REPORT_IMPLEMENTATION,
    }
    return self_digest(report, "oracle_report_sha256")


def _typed_value(value: Any) -> bytes:
    if value is None:
        tag, payload = b"N", b""
    elif type(value) is int:
        tag, payload = b"I", struct.pack(">q", value)
    elif type(value) is float:
        tag, payload = b"R", struct.pack(">d", value)
    elif isinstance(value, str):
        tag, payload = b"T", value.encode("utf-8")
    elif isinstance(value, bytes):
        tag, payload = b"B", value
    else:
        raise TypeError(f"unsupported fixture value: {type(value)!r}")
    return tag + len(payload).to_bytes(8, "big") + payload


def semantic_table_facts(
    rows: list[tuple[int, bytes]],
    *,
    basename: str = BASENAME,
) -> dict[str, Any]:
    columns = ["id", "value"]
    digest = hashlib.sha256()
    digest.update(canonical_bytes({"table": "items", "columns": columns}))
    classified_bytes = 0
    for row_id, value in sorted(rows):
        identity_payload = _typed_value(row_id)
        row_payload = (
            canonical_bytes(
                {
                    "basename": basename,
                    "table": "items",
                    "columns": columns,
                }
            )
            + _typed_value(row_id)
            + _typed_value(value)
        )
        digest.update(len(identity_payload).to_bytes(8, "big"))
        digest.update(identity_payload)
        digest.update(hashlib.sha256(row_payload).digest())
        classified_bytes += len(identity_payload) + len(row_payload)
    return {
        "row_count": len(rows),
        "semantic_rows_sha256": digest.hexdigest(),
        "classified_bytes": classified_bytes,
    }


def _file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while block := source.read(1024 * 1024):
            digest.update(block)
    return digest.hexdigest()


def _filesystem_identity(info: os.stat_result) -> dict[str, int]:
    return {
        "device": int(info.st_dev),
        "inode": int(info.st_ino),
        "uid": int(info.st_uid),
        "mode": stat.S_IMODE(info.st_mode),
    }


def _filesystem_mount(descriptor: int) -> dict[str, int | None]:
    info = os.fstat(descriptor)
    filesystem = os.fstatvfs(descriptor)
    filesystem_id = getattr(filesystem, "f_fsid", None)
    if filesystem_id is not None:
        filesystem_id = int(filesystem_id)
        if filesystem_id < 0 or filesystem_id > MAX_CHECKED_INTEGER:
            filesystem_id = None
    linux_mount_id: int | None = None
    fdinfo = Path(f"/proc/self/fdinfo/{descriptor}")
    if fdinfo.parent.exists():
        for line in fdinfo.read_text(encoding="utf-8").splitlines():
            if line.startswith("mnt_id:"):
                linux_mount_id = int(line.partition(":")[2].strip())
                break
        if linux_mount_id is None:
            raise AssertionError("fixture cannot observe the Linux mount ID")
    if filesystem_id is None and linux_mount_id is None:
        raise AssertionError("fixture cannot observe the filesystem identity")
    return {
        "device": int(info.st_dev),
        "filesystem_id": filesystem_id,
        "linux_mount_id": linux_mount_id,
    }


def _directory_lineage(descriptor: int) -> list[dict[str, Any]]:
    flags = (
        os.O_RDONLY
        | getattr(os, "O_DIRECTORY", 0)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_CLOEXEC", 0)
    )
    current = os.dup(descriptor)
    records: list[dict[str, Any]] = []
    seen: set[tuple[int, int, int | None, int | None]] = set()
    try:
        for _ in range(128):
            info = os.fstat(current)
            observed_mount = _filesystem_mount(current)
            identity = (
                int(info.st_dev),
                int(info.st_ino),
                observed_mount["filesystem_id"],
                observed_mount["linux_mount_id"],
            )
            if identity in seen:
                raise AssertionError("fixture directory lineage contains a cycle")
            seen.add(identity)
            records.append(
                {
                    "identity": _filesystem_identity(info),
                    "mount": observed_mount,
                }
            )
            parent = os.open("..", flags, dir_fd=current)
            parent_info = os.fstat(parent)
            parent_mount = _filesystem_mount(parent)
            if (
                int(parent_info.st_dev),
                int(parent_info.st_ino),
                parent_mount,
            ) == (
                int(info.st_dev),
                int(info.st_ino),
                observed_mount,
            ):
                os.close(parent)
                return records
            os.close(current)
            current = parent
    finally:
        os.close(current)
    raise AssertionError("fixture directory lineage exceeds its bound")


def _simple_items_schema(create_sql: str) -> dict[str, Any]:
    create_sql_sha256 = sha256_bytes(create_sql.encode("utf-8"))
    schema_records = [
        {
            "type": "table",
            "name": "items",
            "table": "items",
            "sql_sha256": create_sql_sha256,
        }
    ]
    schema: dict[str, Any] = {
        "application_id": 0,
        "user_version": 0,
        "raw_schema_sha256": sha256_bytes(canonical_bytes(schema_records)),
        "schema_records": schema_records,
        "omitted_table_objects": [],
        "classification_blockers": [],
        "tables": [
            {
                "name": "items",
                "type": "table",
                "ncol": 2,
                "without_rowid": False,
                "strict": True,
                "create_sql_sha256": create_sql_sha256,
                "columns": [
                    {
                        "cid": 0,
                        "name": "id",
                        "declared_type": "INTEGER",
                        "affinity": "INTEGER",
                        "not_null": False,
                        "default_sql_sha256": None,
                        "primary_key_position": 1,
                        "hidden": 0,
                        "generated_kind": "none",
                    },
                    {
                        "cid": 1,
                        "name": "value",
                        "declared_type": "BLOB",
                        "affinity": "BLOB",
                        "not_null": False,
                        "default_sql_sha256": None,
                        "primary_key_position": 0,
                        "hidden": 0,
                        "generated_kind": "none",
                    },
                ],
                "indexes": [],
                "foreign_keys": [],
            }
        ],
        "triggers": [],
        "views": [],
        "collations": [],
    }
    schema["schema_contract_sha256"] = sha256_bytes(canonical_bytes(schema))
    return schema


@dataclass(frozen=True)
class HandBuiltBundle:
    bundle_path: Path
    action_plan: dict[str, Any]
    opening_plan: dict[str, Any]
    compose_request: dict[str, Any]
    capacity_observation: dict[str, Any]
    verifier_runtime_binding: dict[str, Any]
    manifest: dict[str, Any]
    receipt: dict[str, Any]


def build_handbuilt_bundle(
    root: Path,
    *,
    actual_rows: list[tuple[int, bytes]],
    expected_rows: list[tuple[int, bytes]] | None = None,
) -> HandBuiltBundle:
    """Create fixture bytes directly; no composer or protocol compiler exists."""
    expected_rows = expected_rows or actual_rows
    workspace_parent = root.resolve() / "workspace"
    workspace_parent.mkdir(mode=0o700)
    database_seed = root.resolve() / "seed.sqlite"
    connection = sqlite3.connect(database_seed)
    create_sql = "CREATE TABLE items(id INTEGER PRIMARY KEY, value BLOB) STRICT"
    try:
        connection.execute(create_sql)
        connection.executemany("INSERT INTO items VALUES (?, ?)", actual_rows)
        connection.commit()
        stored_sql = connection.execute(
            "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'items'"
        ).fetchone()
        if stored_sql != (create_sql,):
            raise AssertionError("fixture SQLite rewrote the literal schema")
        engine = sqlite_engine_authority(connection)
        journal_mode = connection.execute("PRAGMA journal_mode").fetchone()[0]
    finally:
        connection.close()
    database_seed.chmod(0o600)

    schema = _simple_items_schema(create_sql)
    migrations: list[list[Any]] = []
    expected = semantic_table_facts(expected_rows)
    actual = semantic_table_facts(actual_rows)
    table_contract = next(
        table for table in schema["tables"] if table["name"] == "items"
    )
    action_plan: dict[str, Any] = {
        "schema": ACTION_PLAN_SCHEMA,
        "operation_graph": representative_action_graph(
            baseline_row_count=actual["row_count"],
            source_only_row_count=expected["row_count"],
            baseline_sha256=actual["semantic_rows_sha256"],
            source_only_sha256=expected["semantic_rows_sha256"],
            family_sha256=schema["schema_contract_sha256"],
        ),
        "sqlite_families": [
            {
                "basename": BASENAME,
                "schema": {
                    "source_application_id": schema["application_id"],
                    "source_user_version": schema["user_version"],
                },
                "migration": {
                    "source_migrations_sha256": sha256_bytes(
                        canonical_bytes(migrations)
                    ),
                    "exact": True,
                },
                "edge_contract": {
                    "registry_sha256": sha256_bytes(canonical_bytes([])),
                    "registry_matches_observed": True,
                },
                "tables": [
                    {
                        "name": "items",
                        "identity_columns": ["id"],
                        "expected_output": expected,
                    }
                ],
            }
        ],
    }
    self_digest(action_plan, "action_plan_sha256")
    final_leaf = f"bulkload-sqlite-compose-{action_plan['action_plan_sha256']}"
    bundle_path = workspace_parent / final_leaf
    sqlite_path = bundle_path / "sqlite"
    sqlite_path.mkdir(parents=True, mode=0o700)
    bundle_path.chmod(0o700)
    sqlite_path.chmod(0o700)
    database_path = sqlite_path / BASENAME
    database_seed.replace(database_path)
    database_path.chmod(0o600)

    parent_descriptor = os.open(
        workspace_parent,
        os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_CLOEXEC", 0),
    )
    try:
        parent_info = os.fstat(parent_descriptor)
        parent_mount = _filesystem_mount(parent_descriptor)
        parent_lineage = _directory_lineage(parent_descriptor)
    finally:
        os.close(parent_descriptor)
    parent_identity = _filesystem_identity(parent_info)
    bundle_identity = _filesystem_identity(bundle_path.stat())
    workspace_record = workspace(
        action_plan_sha256=action_plan["action_plan_sha256"],
        resolved_parent=os.fspath(workspace_parent),
        parent_identity=parent_identity,
        parent_mount=parent_mount,
        parent_lineage=parent_lineage,
        staging_identity=bundle_identity,
    )
    opening_plan = {
        "sqlite_families": [
            {
                "basename": BASENAME,
                "source_schema": schema,
            }
        ]
    }
    compose_request: dict[str, Any] = {
        "schema": COMPOSE_REQUEST_SCHEMA,
        "output_intent": {
            "workspace": {
                "resolved_parent": workspace_record["resolved_parent"],
                "parent_identity": deepcopy(parent_identity),
                "mount": deepcopy(parent_mount),
                "lineage": deepcopy(parent_lineage),
                "lineage_sha256": workspace_record["lineage_sha256"],
                "final_leaf": final_leaf,
                "staging_prefix": f".{final_leaf}-staging-",
            }
        },
        "capacity_requirement": {
            "required_bytes": max(database_path.stat().st_size * 4, 32 * 1024**2),
            "required_inodes": 16,
        },
    }
    self_digest(compose_request, "request_sha256")
    capacity_observation: dict[str, Any] = {
        "schema": CAPACITY_OBSERVATION_SCHEMA,
        "expires_at": EXPIRES_AT,
        "host_authority_id": HOST_AUTHORITY_ID,
    }
    self_digest(capacity_observation, "observation_sha256")
    accepted = accepted_artifacts(
        action_plan,
        compose_request,
        capacity_observation,
    )
    family = handbuilt_family(
        size=database_path.stat().st_size,
        physical_sha256=_file_sha256(database_path),
        schema_sha256=schema["raw_schema_sha256"],
        semantic_sha256=actual["semantic_rows_sha256"],
        migration_sha256=sha256_bytes(canonical_bytes(migrations)),
        edge_sha256=sha256_bytes(canonical_bytes([])),
    )
    family["schema"] = {
        "raw_schema_sha256": schema["raw_schema_sha256"],
        "schema_contract_sha256": schema["schema_contract_sha256"],
        "structured_schema_sha256": sha256_bytes(canonical_bytes(schema)),
    }
    family["tables"] = [
        {
            "name": "items",
            "identity_columns": ["id"],
            "row_count": actual["row_count"],
            "semantic_rows_sha256": actual["semantic_rows_sha256"],
            "schema_sha256": sha256_bytes(canonical_bytes(table_contract)),
            "foreign_keys_sha256": sha256_bytes(
                canonical_bytes(table_contract["foreign_keys"])
            ),
        }
    ]
    family["journal_mode"] = str(journal_mode).lower()
    manifest = handbuilt_manifest(
        accepted=accepted,
        family=family,
        workspace_record=workspace_record,
        engine=engine,
    )
    graph_sha256 = sha256_bytes(canonical_bytes(action_plan["operation_graph"]))
    receipt = handbuilt_receipt(
        manifest,
        completed_node_ids=completed_before_receipt(action_plan["operation_graph"]),
        operation_graph_sha256=graph_sha256,
        required_bytes=compose_request["capacity_requirement"]["required_bytes"],
        required_inodes=compose_request["capacity_requirement"]["required_inodes"],
    )
    for path, value in (
        (bundle_path / "manifest.json", manifest),
        (bundle_path / "composition-receipt.json", receipt),
    ):
        path.write_bytes(canonical_file(value))
        path.chmod(0o600)
    return HandBuiltBundle(
        bundle_path=bundle_path,
        action_plan=action_plan,
        opening_plan=opening_plan,
        compose_request=compose_request,
        capacity_observation=capacity_observation,
        verifier_runtime_binding=runtime_source_binding("private_sqlite_verifier.py"),
        manifest=manifest,
        receipt=receipt,
    )
