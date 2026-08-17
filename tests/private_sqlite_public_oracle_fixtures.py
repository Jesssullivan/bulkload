"""Complete test-only artifact chain for the public SQLite v7 oracle.

The frozen v4/v5 envelopes reuse their shipped fragment classifiers. The v6
documents and v7 bundle are assembled only as test evidence; no shipped
producer or verifier implementation is used to create the candidate bundle.
"""

from __future__ import annotations

from copy import deepcopy
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
import hashlib
import json
import os
from pathlib import Path
import shutil
import sqlite3
import time
from typing import Any
from urllib.parse import quote
import uuid

from bulkload_lib import private_quiescence
from bulkload_lib import private_runtime
from bulkload_lib import private_sqlite_plan
from bulkload_lib import private_sqlite_request
from bulkload_lib import private_state
from bulkload_lib.model import canonical_bytes, object_digest, sha256_bytes, utc_now
from bulkload_lib.private_sqlite_plan import (
    SQLITE_ADAPTER_REGISTRY_SCHEMA,
    SQLITE_PATH_MAP_SCHEMA,
    validate_sqlite_adapter_registry,
    validate_sqlite_path_map,
)
from bulkload_lib.private_sqlite_request import (
    DEFAULT_CAPACITY_OBSERVATION_TTL_SECONDS,
    PRIVATE_SQLITE_CAPACITY_OBSERVATION_IMPLEMENTATION,
    PRIVATE_SQLITE_CAPACITY_OBSERVATION_SCHEMA,
    PRIVATE_SQLITE_COMPOSE_REQUEST_IMPLEMENTATION,
    PRIVATE_SQLITE_COMPOSE_REQUEST_SCHEMA,
)
from bulkload_lib.sessions import (
    capture_codex_sessions,
    compile_codex_session_union_plan,
)
from tests.private_sqlite_legacy_fixtures import (
    build_v4_compose_plan_fixture,
    build_v5_action_plan_fixture,
    build_v5_close_request_fixture,
    build_v5_session_reclose_fixture,
)
from tests.private_sqlite_v7_fixtures import (
    HOST_AUTHORITY_ID,
    ORACLE_FALSE_CLAIMS,
    ORACLE_OBSERVED_CHECKS,
    accepted_artifacts,
    canonical_file,
    completed_before_receipt,
    handbuilt_manifest,
    handbuilt_receipt,
    self_digest,
    sqlite_engine_authority,
    workspace,
    _directory_lineage,
    _file_sha256,
    _filesystem_identity,
    _filesystem_mount,
    _typed_value,
)


SOURCE_AUTHORITY = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
DESTINATION_AUTHORITY = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
SESSION_ID = "11111111-1111-4111-8111-111111111111"
BASENAME = "state_5.sqlite"


@dataclass(frozen=True)
class PublicOracleFixture:
    root: Path
    bundle_path: Path
    action_plan: dict[str, Any]
    opening_plan: dict[str, Any]
    compose_request: dict[str, Any]
    capacity_observation: dict[str, Any]
    manifest: dict[str, Any]
    receipt: dict[str, Any]


def _timestamp(value: datetime) -> str:
    return (
        value.astimezone(UTC)
        .replace(microsecond=0)
        .isoformat()
        .replace(
            "+00:00",
            "Z",
        )
    )


def _write_rollout(root: Path) -> Path:
    directory = root / "2026" / "07" / "29"
    directory.mkdir(parents=True, mode=0o700)
    current = root
    current.chmod(0o700)
    for component in ("2026", "07", "29"):
        current /= component
        current.chmod(0o700)
    path = directory / f"rollout-2026-07-29T00-00-00-{SESSION_ID}.jsonl"
    path.write_text(
        json.dumps(
            {
                "timestamp": "2026-07-29T00:00:00Z",
                "type": "session_meta",
                "payload": {"id": SESSION_ID},
            },
            separators=(",", ":"),
            sort_keys=True,
        )
        + "\n",
        encoding="utf-8",
    )
    path.chmod(0o600)
    return path


def _create_state_database(path: Path, rollout: Path) -> None:
    connection = sqlite3.connect(path)
    try:
        connection.execute("PRAGMA journal_mode=DELETE")
        connection.execute(
            """
            CREATE TABLE _sqlx_migrations(
                version INTEGER PRIMARY KEY,
                description TEXT NOT NULL,
                installed_on TEXT NOT NULL,
                success INTEGER NOT NULL,
                checksum BLOB NOT NULL,
                execution_time INTEGER NOT NULL
            )
            """
        )
        connection.execute(
            """
            INSERT INTO _sqlx_migrations
            VALUES(40, 'fixture-base', 'now', 1, x'0042', 1)
            """
        )
        connection.execute(
            """
            CREATE TABLE threads(
                id TEXT PRIMARY KEY,
                rollout_path TEXT NOT NULL,
                payload BLOB NOT NULL
            ) STRICT
            """
        )
        connection.execute(
            "INSERT INTO threads VALUES(?, ?, ?)",
            (SESSION_ID, os.fspath(rollout.resolve()), b"oracle-fixture"),
        )
        connection.commit()
    finally:
        connection.close()
    path.chmod(0o600)


def _capture_private_bundle(
    evidence_root: Path,
    codex_home: Path,
    *,
    role: str,
    authority: str,
    label: str,
    purpose: str,
    accepted_plan_sha256: str | None = None,
) -> tuple[dict[str, Any], Path]:
    output = evidence_root / f"{label}-bundle"
    attestation_path = evidence_root / f".{label}-{uuid.uuid4()}.json"
    attestation = private_quiescence.create_codex_private_quiescence_attestation(
        codex_home,
        attestation_path,
        purpose=purpose,
        capture_role=role,
        host_authority_id=authority,
        codex_version="0.145.0",
        selected_state_classes=["sqlite"],
        sqlite_home=codex_home,
        create_only_output=output,
        acknowledge_writers_quiesced=True,
        accepted_plan_sha256=accepted_plan_sha256,
    )
    capture = private_state.capture_codex_private_state(
        codex_home,
        output,
        role=role,
        host_authority_id=authority,
        codex_version="0.145.0",
        sqlite_home=codex_home,
        include_auth=False,
        include_sqlite=True,
        acknowledge_private_capture=True,
        quiescence=private_state.private_quiescence_capture_record(attestation),
    )
    return capture, output


def _registry_for(
    source_bundle: Path,
    destination_bundle: Path,
) -> dict[str, Any]:
    deadline = time.monotonic() + 60
    with private_sqlite_plan._open_pinned_snapshot(
        source_bundle,
        BASENAME,
    ) as source:
        source_schema, source_blockers, source_migrations = (
            private_sqlite_plan._structured_schema(source, deadline=deadline)
        )
    with private_sqlite_plan._open_pinned_snapshot(
        destination_bundle,
        BASENAME,
    ) as destination:
        destination_schema, destination_blockers, destination_migrations = (
            private_sqlite_plan._structured_schema(
                destination,
                deadline=deadline,
            )
        )
    if source_blockers or destination_blockers:
        raise AssertionError((source_blockers, destination_blockers))
    migration_relation, _ = private_sqlite_plan._migration_relation(
        source_migrations,
        destination_migrations,
    )
    registry: dict[str, Any] = {
        "schema": SQLITE_ADAPTER_REGISTRY_SCHEMA,
        "registry_id": "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
        "created_at": utc_now(),
        "families": [
            {
                "basename": BASENAME,
                "family_role": "state",
                "generation": 5,
                "source_codex_version": "0.145.0",
                "destination_codex_version": "0.145.0",
                "source_schema_contract_sha256": source_schema[
                    "schema_contract_sha256"
                ],
                "destination_schema_contract_sha256": destination_schema[
                    "schema_contract_sha256"
                ],
                "source_raw_schema_sha256": source_schema["raw_schema_sha256"],
                "destination_raw_schema_sha256": destination_schema[
                    "raw_schema_sha256"
                ],
                "source_migrations_sha256": sha256_bytes(
                    canonical_bytes(source_migrations)
                ),
                "destination_migrations_sha256": sha256_bytes(
                    canonical_bytes(destination_migrations)
                ),
                "source_application_id": source_schema["application_id"],
                "destination_application_id": destination_schema["application_id"],
                "source_user_version": source_schema["user_version"],
                "destination_user_version": destination_schema["user_version"],
                "migration_relation": migration_relation,
                "adapter": None,
                "tables": [
                    {
                        "name": "_sqlx_migrations",
                        "merge_class": "exact",
                        "identity_columns": ["version"],
                    },
                    {
                        "name": "threads",
                        "merge_class": "keyed-union",
                        "identity_columns": ["id"],
                    },
                ],
                "path_authorities": [
                    {
                        "table": "threads",
                        "session_id_column": "id",
                        "column": "rollout_path",
                        "kind": "session-rollout",
                    }
                ],
                "edges": [],
                "allowed_collations": sorted(
                    set(source_schema["collations"])
                    | set(destination_schema["collations"])
                    | {"BINARY", "NOCASE", "RTRIM"}
                ),
            }
        ],
    }
    registry["registry_sha256"] = object_digest(registry, "registry_sha256")
    validate_sqlite_adapter_registry(registry)
    return registry


def _path_map_for(
    source_sessions: dict[str, Any],
    destination_sessions: dict[str, Any],
    session_plan: dict[str, Any],
) -> dict[str, Any]:
    path_map: dict[str, Any] = {
        "schema": SQLITE_PATH_MAP_SCHEMA,
        "mapping_id": "dddddddd-dddd-4ddd-8ddd-dddddddddddd",
        "created_at": utc_now(),
        "source_host_authority_id": SOURCE_AUTHORITY,
        "destination_host_authority_id": DESTINATION_AUTHORITY,
        "codex_version": {
            "source": "0.145.0",
            "destination": "0.145.0",
        },
        "session_union_plan_sha256": session_plan["plan_sha256"],
        "source_session_catalog_sha256": source_sessions["catalog_sha256"],
        "destination_session_catalog_sha256": destination_sessions["catalog_sha256"],
        "source_session_root": source_sessions["resolved_root"],
        "destination_session_root": destination_sessions["resolved_root"],
        "rules": [
            {
                "family_basename": BASENAME,
                "table": "threads",
                "session_id_column": "id",
                "column": "rollout_path",
                "kind": "session-rollout",
            }
        ],
    }
    path_map["path_map_sha256"] = object_digest(path_map, "path_map_sha256")
    validate_sqlite_path_map(path_map)
    return path_map


def _build_legacy_chain(root: Path) -> tuple[dict[str, Any], dict[str, Any], Path]:
    evidence_root = root / "legacy-evidence"
    live_root = root / "live"
    evidence_root.mkdir(mode=0o700)
    live_root.mkdir(mode=0o700)
    source_home = live_root / "source"
    destination_home = live_root / "destination"
    source_home.mkdir(mode=0o700)
    destination_home.mkdir(mode=0o700)
    source_sessions = source_home / "sessions"
    destination_sessions = destination_home / "sessions"
    source_rollout = _write_rollout(source_sessions)
    destination_rollout = _write_rollout(destination_sessions)
    _create_state_database(source_home / BASENAME, source_rollout)
    _create_state_database(destination_home / BASENAME, destination_rollout)

    opening_paths: dict[str, Path] = {}
    for role, home, authority in (
        ("source", source_home, SOURCE_AUTHORITY),
        ("destination", destination_home, DESTINATION_AUTHORITY),
    ):
        for pass_name in ("a", "b"):
            _, path = _capture_private_bundle(
                evidence_root,
                home,
                role=role,
                authority=authority,
                label=f"opening-{role}-{pass_name}",
                purpose="capture",
            )
            opening_paths[f"{role}_{pass_name}"] = path

    runtime = deepcopy(private_runtime.ACCEPTED_H6_PRIVATE_RUNTIME_AUTHORITY_V5)
    compatibility = private_state.compile_codex_private_state_plan(
        opening_paths["source_a"],
        opening_paths["destination_a"],
        runtime_authority=runtime,
    )
    opening_sessions = {
        f"{role}_{pass_name}": capture_codex_sessions(
            sessions_root,
            role=role,
            acknowledge_writers_quiesced=True,
            host_authority_id=authority,
        )
        for role, sessions_root, authority in (
            ("source", source_sessions, SOURCE_AUTHORITY),
            ("destination", destination_sessions, DESTINATION_AUTHORITY),
        )
        for pass_name in ("a", "b")
    }
    session_plan = compile_codex_session_union_plan(
        opening_sessions["source_a"],
        opening_sessions["source_b"],
        opening_sessions["destination_a"],
        opening_sessions["destination_b"],
    )
    registry = _registry_for(
        opening_paths["source_a"],
        opening_paths["destination_a"],
    )
    path_map = _path_map_for(
        opening_sessions["source_a"],
        opening_sessions["destination_a"],
        session_plan,
    )
    opening_plan = build_v4_compose_plan_fixture(
        compatibility,
        opening_paths["source_a"],
        opening_paths["source_b"],
        opening_paths["destination_a"],
        opening_paths["destination_b"],
        adapter_registry=registry,
        path_map=path_map,
        session_union_plan=session_plan,
        session_source_a=opening_sessions["source_a"],
        session_source_b=opening_sessions["source_b"],
        session_destination_a=opening_sessions["destination_a"],
        session_destination_b=opening_sessions["destination_b"],
        runtime_authority=runtime,
        created_at=utc_now(),
    )

    stopped_at = _timestamp(datetime.now(UTC) - timedelta(seconds=1))
    close_request = build_v5_close_request_fixture(
        opening_plan,
        session_plan,
        opening_sessions["source_a"],
        opening_sessions["source_b"],
        opening_sessions["destination_a"],
        opening_sessions["destination_b"],
        runtime,
        writer_stop_epoch_id=str(uuid.uuid4()),
        writer_stop_epoch_at=stopped_at,
        created_at=utc_now(),
    )
    closing_paths: dict[str, Path] = {}
    for role, home, authority in (
        ("source", source_home, SOURCE_AUTHORITY),
        ("destination", destination_home, DESTINATION_AUTHORITY),
    ):
        for pass_name in ("a", "b"):
            _, path = _capture_private_bundle(
                evidence_root,
                home,
                role=role,
                authority=authority,
                label=f"closing-{role}-{pass_name}",
                purpose="close",
                accepted_plan_sha256=close_request["close_request_sha256"],
            )
            closing_paths[f"{role}_{pass_name}"] = path

    closing_sessions = {
        f"{role}_{pass_name}": build_v5_session_reclose_fixture(
            sessions_root,
            role=role,
            close_request=close_request,
            acknowledge_writers_quiesced=True,
        )
        for role, sessions_root in (
            ("source", source_sessions),
            ("destination", destination_sessions),
        )
        for pass_name in ("a", "b")
    }
    action_plan = build_v5_action_plan_fixture(
        opening_plan,
        close_request,
        closing_paths["source_a"],
        closing_paths["source_b"],
        closing_paths["destination_a"],
        closing_paths["destination_b"],
        closing_sessions["source_a"],
        closing_sessions["source_b"],
        closing_sessions["destination_a"],
        closing_sessions["destination_b"],
        runtime_authority=runtime,
        created_at=utc_now(),
    )
    return action_plan, opening_plan, closing_paths["destination_a"]


def _synthetic_protected_namespaces() -> list[dict[str, Any]]:
    protected: list[dict[str, Any]] = []
    bindings: dict[
        str, tuple[dict[str, Any], dict[str, Any], list[dict[str, Any]], str]
    ] = {}
    next_inode = 100

    def record(
        namespace_class: str,
        role: str,
        path: str,
        *,
        exists: bool,
        require_private: bool,
    ) -> None:
        nonlocal next_inode
        identity = mount = lineage = lineage_sha256 = None
        if exists:
            binding = bindings.get(path)
            if binding is None:
                identity = {
                    "device": 10,
                    "inode": next_inode,
                    "uid": os.getuid(),
                    "mode": 0o700 if require_private else 0o755,
                }
                mount = {
                    "device": 10,
                    "filesystem_id": 20,
                    "linux_mount_id": None,
                }
                lineage = [{"identity": deepcopy(identity), "mount": deepcopy(mount)}]
                lineage_sha256 = sha256_bytes(canonical_bytes(lineage))
                bindings[path] = (
                    identity,
                    mount,
                    lineage,
                    lineage_sha256,
                )
                next_inode += 1
            else:
                identity, mount, lineage, lineage_sha256 = deepcopy(binding)
        protected.append(
            {
                "class": namespace_class,
                "role": role,
                "path": path,
                "exists": exists,
                "require_private": require_private,
                "identity": identity,
                "mount": mount,
                "lineage": lineage,
                "lineage_sha256": lineage_sha256,
            }
        )

    evidence_parent = "/private/bulkload-v7-evidence"
    for namespace_class in (
        "opening-private-bundle",
        "closing-private-bundle",
    ):
        for role in (
            "source_a",
            "source_b",
            "destination_a",
            "destination_b",
        ):
            record(
                namespace_class,
                role,
                f"{evidence_parent}/{namespace_class}/{role}",
                exists=True,
                require_private=True,
            )
    record(
        "protocol-evidence-parent",
        "private-json",
        evidence_parent,
        exists=True,
        require_private=True,
    )
    record(
        "request-output-parent",
        "private-json",
        evidence_parent,
        exists=True,
        require_private=True,
    )
    record(
        "request-runtime-root",
        "consumer",
        "/private/bulkload-v7-runtime",
        exists=True,
        require_private=False,
    )
    for role in (
        "source:codex_home",
        "source:sqlite_home",
        "destination:codex_home",
        "destination:sqlite_home",
    ):
        record(
            "live-private-root",
            role,
            f"/private/bulkload-v7-live/{role.replace(':', '-')}",
            exists=False,
            require_private=True,
        )
    for role in (
        "opening_session_source_a",
        "opening_session_source_b",
        "opening_session_destination_a",
        "opening_session_destination_b",
        "session_source_a",
        "session_source_b",
        "session_destination_a",
        "session_destination_b",
    ):
        record(
            "live-session-root",
            role,
            f"/private/bulkload-v7-live/{role}",
            exists=False,
            require_private=True,
        )
    protected.sort(key=canonical_bytes)
    return protected


def _workspace_record(
    workspace_parent: Path,
    action_plan: dict[str, Any],
) -> dict[str, Any]:
    descriptor = os.open(
        workspace_parent,
        os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_CLOEXEC", 0),
    )
    try:
        parent_info = os.fstat(descriptor)
        parent_mount = _filesystem_mount(descriptor)
        parent_lineage = _directory_lineage(descriptor)
    finally:
        os.close(descriptor)
    leaf = private_sqlite_request._derived_compose_output_leaf(
        action_plan["action_plan_sha256"]
    )
    protected = _synthetic_protected_namespaces()
    return {
        "resolved_parent": os.fspath(workspace_parent),
        "parent_identity": _filesystem_identity(parent_info),
        "mount": parent_mount,
        "lineage": parent_lineage,
        "lineage_sha256": sha256_bytes(canonical_bytes(parent_lineage)),
        "final_leaf": leaf,
        "target_observed_absent": True,
        "staging_prefix": f".{leaf}-staging-",
        "staging_namespace_observed_empty": True,
        "same_parent_staging_required": True,
        "protected_namespaces": protected,
        "protected_namespaces_sha256": sha256_bytes(canonical_bytes(protected)),
    }


def _frozen_v6_request(
    action_plan: dict[str, Any],
    workspace_record: dict[str, Any],
    *,
    created_at: str,
) -> dict[str, Any]:
    request: dict[str, Any] = {
        "schema": PRIVATE_SQLITE_COMPOSE_REQUEST_SCHEMA,
        "created_at": created_at,
        "request_id": str(uuid.uuid4()),
        "action_plan": private_sqlite_request._action_binding(action_plan),
        "action_plan_producer_runtime_authority": deepcopy(
            action_plan["runtime_authority"]
        ),
        "request_runtime_authority": deepcopy(
            private_runtime.ACCEPTED_H7_PRIVATE_RUNTIME_AUTHORITY_V6
        ),
        "required_composer_runtime_authority": None,
        "output_intent": {
            "workspace": deepcopy(workspace_record),
            "create_only": True,
            "replace": False,
            "delete": False,
            "future_directory_mode": 0o700,
            "future_file_mode": 0o600,
            "same_mount_required": True,
            "randomized_staging_sibling_required": True,
        },
        "capacity_requirement": private_sqlite_request._capacity_requirement(
            action_plan
        ),
        "capacity_observation": {
            "required": True,
            "maximum_age_seconds": DEFAULT_CAPACITY_OBSERVATION_TTL_SECONDS,
            "space_reserved": False,
            "future_write_guaranteed": False,
            "quota_proof": False,
        },
        "claims": {
            "request_complete": True,
            "request_runtime_bound": True,
            "composer_runtime_bound": False,
            "executed": False,
            "ready_for_internal_offline_compose": False,
            "ready_for_offline_compose": False,
            "composer_implemented": False,
            "compose_authorized": False,
            "publication_authorized": False,
            "install_authorized": False,
            "combined_authorized": False,
            "apply_authorized": False,
            "provider_runtime_acceptance": False,
            "provider_writer_proof": False,
        },
        "implementation": PRIVATE_SQLITE_COMPOSE_REQUEST_IMPLEMENTATION,
    }
    request["request_body_sha256"] = sha256_bytes(canonical_bytes(request))
    request["request_sha256"] = object_digest(request, "request_sha256")
    return request


def _frozen_v6_capacity(
    request: dict[str, Any],
    *,
    created_at: datetime,
) -> dict[str, Any]:
    requirement = request["capacity_requirement"]
    fragment_size = 4096
    available_blocks = (
        requirement["required_bytes"] + fragment_size - 1
    ) // fragment_size
    observation: dict[str, Any] = {
        "schema": PRIVATE_SQLITE_CAPACITY_OBSERVATION_SCHEMA,
        "created_at": _timestamp(created_at),
        "expires_at": _timestamp(
            created_at + timedelta(seconds=DEFAULT_CAPACITY_OBSERVATION_TTL_SECONDS)
        ),
        "observation_id": str(uuid.uuid4()),
        "host_authority_id": HOST_AUTHORITY_ID,
        "request": {
            "schema": request["schema"],
            "request_sha256": request["request_sha256"],
            "body_sha256": sha256_bytes(canonical_bytes(request)),
        },
        "observation_runtime_authority": deepcopy(request["request_runtime_authority"]),
        "workspace": deepcopy(request["output_intent"]["workspace"]),
        "requirement": deepcopy(requirement),
        "filesystem": {
            "fragment_size": fragment_size,
            "available_blocks": available_blocks,
            "available_bytes": available_blocks * fragment_size,
            "available_inodes": requirement["required_inodes"],
        },
        "claims": {
            "capacity_sufficient_observed": True,
            "space_reserved": False,
            "future_write_guaranteed": False,
            "quota_proof": False,
            "executed": False,
            "ready_for_internal_offline_compose": False,
            "compose_authorized": False,
        },
        "implementation": PRIVATE_SQLITE_CAPACITY_OBSERVATION_IMPLEMENTATION,
    }
    observation["observation_body_sha256"] = sha256_bytes(canonical_bytes(observation))
    observation["observation_sha256"] = object_digest(
        observation,
        "observation_sha256",
    )
    return observation


def _fixture_identity_sort_payload(value: Any, typed_value: bytes) -> bytes:
    if value is None:
        return b""
    if type(value) is int:
        return str(value).encode("ascii")
    if isinstance(value, (str, bytes)):
        return typed_value[9:]
    raise AssertionError("fixture identity has an unsupported SQLite type")


def _observed_table_facts(
    connection: sqlite3.Connection,
    *,
    basename: str,
    table_contract: dict[str, Any],
    action_table: dict[str, Any],
) -> dict[str, Any]:
    """Build candidate facts without trusting a shipped planner or verifier."""
    table_name = action_table["name"]
    columns = [column["name"] for column in table_contract["columns"]]
    identities = action_table["identity_columns"]

    def quoted(value: str) -> str:
        return '"' + value.replace('"', '""') + '"'

    retained_rows: list[
        tuple[tuple[tuple[str, bytes], ...], tuple[bytes, ...], bytes]
    ] = []
    row_prefix = canonical_bytes(
        {"basename": basename, "table": table_name, "columns": columns}
    )
    query = (
        f"SELECT {', '.join(quoted(column) for column in columns)} "
        f"FROM {quoted(table_name)}"
    )
    for row in connection.execute(query):
        values = dict(zip(columns, row, strict=True))
        typed_values = {column: _typed_value(values[column]) for column in columns}
        identity_parts = tuple(typed_values[column] for column in identities)
        sort_key = tuple(
            (
                {
                    type(None): "null",
                    int: "integer",
                    str: "text",
                    bytes: "blob",
                }[type(values[column])],
                _fixture_identity_sort_payload(
                    values[column],
                    typed_values[column],
                ),
            )
            for column in identities
        )
        row_hasher = hashlib.sha256()
        row_hasher.update(row_prefix)
        for column in columns:
            row_hasher.update(typed_values[column])
        retained_rows.append((sort_key, identity_parts, row_hasher.digest()))

    retained_rows.sort(key=lambda item: item[0])
    digest = hashlib.sha256()
    digest.update(canonical_bytes({"table": table_name, "columns": columns}))
    previous_identity: tuple[bytes, ...] | None = None
    for _, identity_parts, row_digest in retained_rows:
        if identity_parts == previous_identity:
            raise AssertionError("fixture identity is duplicated")
        digest.update(sum(map(len, identity_parts)).to_bytes(8, "big"))
        for identity_part in identity_parts:
            digest.update(identity_part)
        digest.update(row_digest)
        previous_identity = identity_parts
    return {
        "row_count": len(retained_rows),
        "semantic_rows_sha256": digest.hexdigest(),
    }


def _manifest_family(
    database_path: Path,
    action_family: dict[str, Any],
    opening_family: dict[str, Any],
) -> tuple[dict[str, Any], dict[str, Any]]:
    uri = f"file:{quote(os.fspath(database_path), safe='/')}?mode=ro&immutable=1"
    connection = sqlite3.connect(uri, uri=True)
    try:
        engine = sqlite_engine_authority(connection)
        journal_mode_row = connection.execute("PRAGMA journal_mode").fetchone()
        source_schema = opening_family["source_schema"]
        contracts = {table["name"]: table for table in source_schema["tables"]}
        observed_tables = {
            action_table["name"]: _observed_table_facts(
                connection,
                basename=action_family["basename"],
                table_contract=contracts[action_table["name"]],
                action_table=action_table,
            )
            for action_table in action_family["tables"]
        }
    finally:
        connection.close()
    if journal_mode_row is None or len(journal_mode_row) != 1:
        raise AssertionError("fixture SQLite journal mode is unavailable")
    tables = []
    for action_table in action_family["tables"]:
        contract = contracts[action_table["name"]]
        observed = observed_tables[action_table["name"]]
        tables.append(
            {
                "name": action_table["name"],
                "identity_columns": deepcopy(action_table["identity_columns"]),
                "row_count": observed["row_count"],
                "semantic_rows_sha256": observed["semantic_rows_sha256"],
                "schema_sha256": sha256_bytes(canonical_bytes(contract)),
                "foreign_keys_sha256": sha256_bytes(
                    canonical_bytes(contract["foreign_keys"])
                ),
            }
        )
    relative_path = f"sqlite/{action_family['basename']}"
    family = {
        "basename": action_family["basename"],
        "relative_path": relative_path,
        "mode": 0o600,
        "size": database_path.stat().st_size,
        "sha256": _file_sha256(database_path),
        "journal_mode": str(journal_mode_row[0]).lower(),
        "application_id": source_schema["application_id"],
        "user_version": source_schema["user_version"],
        "schema": {
            "raw_schema_sha256": source_schema["raw_schema_sha256"],
            "schema_contract_sha256": source_schema["schema_contract_sha256"],
            "structured_schema_sha256": sha256_bytes(canonical_bytes(source_schema)),
        },
        "migrations": {
            "migrations_sha256": action_family["migration"]["source_migrations_sha256"],
            "exact": True,
        },
        "edges": {
            "registry_sha256": action_family["edge_contract"]["registry_sha256"],
            "observed_sha256": action_family["edge_contract"]["registry_sha256"],
            "closed": True,
        },
        "tables": tables,
        "absent_sidecars": sorted(
            f"{relative_path}{suffix}" for suffix in ("-journal", "-shm", "-wal")
        ),
    }
    return family, engine


def build_public_oracle_fixture(root: Path) -> PublicOracleFixture:
    """Build a full frozen chain and independent v7 candidate under ``root``."""
    root = root.resolve()
    root.chmod(0o700)
    action_plan, opening_plan, destination_close = _build_legacy_chain(root)
    workspace_parent = root / "workspace"
    workspace_parent.mkdir(mode=0o700)
    request_workspace = _workspace_record(workspace_parent, action_plan)

    timeline = datetime.now(UTC).replace(microsecond=0)
    compose_request = _frozen_v6_request(
        action_plan,
        request_workspace,
        created_at=_timestamp(timeline),
    )
    capacity_observation = _frozen_v6_capacity(
        compose_request,
        created_at=timeline,
    )

    bundle_path = workspace_parent / request_workspace["final_leaf"]
    sqlite_directory = bundle_path / "sqlite"
    sqlite_directory.mkdir(parents=True, mode=0o700)
    bundle_path.chmod(0o700)
    sqlite_directory.chmod(0o700)
    database_path = sqlite_directory / BASENAME
    shutil.copyfile(destination_close / "sqlite" / BASENAME, database_path)
    database_path.chmod(0o600)

    manifest_workspace = workspace(
        action_plan_sha256=action_plan["action_plan_sha256"],
        resolved_parent=request_workspace["resolved_parent"],
        parent_identity=request_workspace["parent_identity"],
        parent_mount=request_workspace["mount"],
        parent_lineage=request_workspace["lineage"],
        staging_identity=_filesystem_identity(bundle_path.stat()),
    )
    action_family = action_plan["sqlite_families"][0]
    opening_family = opening_plan["sqlite_families"][0]
    family, engine = _manifest_family(
        database_path,
        action_family,
        opening_family,
    )
    accepted = accepted_artifacts(
        action_plan,
        compose_request,
        capacity_observation,
    )
    manifest = handbuilt_manifest(
        accepted=accepted,
        family=family,
        workspace_record=manifest_workspace,
        engine=engine,
    )
    manifest["created_at"] = _timestamp(timeline)
    self_digest(manifest, "manifest_sha256")

    graph_sha256 = sha256_bytes(canonical_bytes(action_plan["operation_graph"]))
    requirement = compose_request["capacity_requirement"]
    receipt = handbuilt_receipt(
        manifest,
        completed_node_ids=completed_before_receipt(action_plan["operation_graph"]),
        operation_graph_sha256=graph_sha256,
        required_bytes=requirement["required_bytes"],
        required_inodes=requirement["required_inodes"],
    )
    receipt["completed_at"] = _timestamp(timeline)
    receipt["capacity_admission"]["expires_at"] = capacity_observation["expires_at"]
    for observation in receipt["capacity_admission"]["observations"]:
        observation["observed_at"] = _timestamp(timeline)
    self_digest(receipt, "receipt_sha256")

    for path, value in (
        (bundle_path / "manifest.json", manifest),
        (bundle_path / "composition-receipt.json", receipt),
    ):
        path.write_bytes(canonical_file(value))
        path.chmod(0o600)

    return PublicOracleFixture(
        root=root,
        bundle_path=bundle_path,
        action_plan=action_plan,
        opening_plan=opening_plan,
        compose_request=compose_request,
        capacity_observation=capacity_observation,
        manifest=manifest,
        receipt=receipt,
    )


def tamper_candidate_payload_and_redigest(
    fixture: PublicOracleFixture,
    payload: bytes,
) -> None:
    """Change candidate bytes and rebuild only the candidate-origin claims."""
    database_path = fixture.bundle_path / "sqlite" / BASENAME
    connection = sqlite3.connect(database_path)
    try:
        connection.execute(
            "UPDATE threads SET payload = ? WHERE id = ?",
            (payload, SESSION_ID),
        )
        connection.commit()
    finally:
        connection.close()
    database_path.chmod(0o600)

    action_family = fixture.action_plan["sqlite_families"][0]
    opening_family = fixture.opening_plan["sqlite_families"][0]
    family, engine = _manifest_family(
        database_path,
        action_family,
        opening_family,
    )
    manifest = handbuilt_manifest(
        accepted=deepcopy(fixture.manifest["accepted_inputs"]),
        family=family,
        workspace_record=deepcopy(fixture.manifest["workspace"]),
        engine=engine,
    )
    manifest["created_at"] = fixture.manifest["created_at"]
    self_digest(manifest, "manifest_sha256")

    prior_receipt = fixture.receipt
    requirement = fixture.compose_request["capacity_requirement"]
    receipt = handbuilt_receipt(
        manifest,
        completed_node_ids=completed_before_receipt(
            fixture.action_plan["operation_graph"]
        ),
        operation_graph_sha256=sha256_bytes(
            canonical_bytes(fixture.action_plan["operation_graph"])
        ),
        required_bytes=requirement["required_bytes"],
        required_inodes=requirement["required_inodes"],
    )
    receipt["completed_at"] = prior_receipt["completed_at"]
    receipt["capacity_admission"]["expires_at"] = prior_receipt["capacity_admission"][
        "expires_at"
    ]
    for observed, prior in zip(
        receipt["capacity_admission"]["observations"],
        prior_receipt["capacity_admission"]["observations"],
        strict=True,
    ):
        observed["observed_at"] = prior["observed_at"]
    self_digest(receipt, "receipt_sha256")

    fixture.manifest.clear()
    fixture.manifest.update(manifest)
    fixture.receipt.clear()
    fixture.receipt.update(receipt)
    for path, value in (
        (fixture.bundle_path / "manifest.json", fixture.manifest),
        (fixture.bundle_path / "composition-receipt.json", fixture.receipt),
    ):
        path.write_bytes(canonical_file(value))
        path.chmod(0o600)


__all__ = [
    "ORACLE_FALSE_CLAIMS",
    "ORACLE_OBSERVED_CHECKS",
    "PublicOracleFixture",
    "build_public_oracle_fixture",
    "tamper_candidate_payload_and_redigest",
]
