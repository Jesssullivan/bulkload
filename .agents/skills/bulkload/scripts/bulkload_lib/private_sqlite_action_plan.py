"""Closed, source-only action planning for private Codex SQLite composition.

This module emits only a descriptive input to a later offline composer.  It
does not create an output database, publish a bundle, install provider state,
or expose an apply surface.
"""

from __future__ import annotations

from copy import deepcopy
import os
from pathlib import Path
import re
import time
from typing import Any
import uuid

from . import private_runtime
from .model import (
    BulkloadError,
    canonical_bytes,
    object_digest,
    sha256_bytes,
    utc_now,
)
from .private_sqlite_close import (
    PRIVATE_SQLITE_CLOSE_REQUEST_SCHEMA,
    validate_codex_private_sqlite_close_request,
    validate_codex_private_sqlite_private_reclose_set,
    validate_codex_private_sqlite_session_reclose_set,
)
from .private_sqlite_plan import (
    MAX_SQLITE_PLAN_BYTES,
    MAX_SQLITE_PLAN_ROW_BYTES,
    MAX_SQLITE_PLAN_ROWS,
    MAX_SQLITE_PLAN_SECONDS,
    PRIVATE_SQLITE_PLAN_SCHEMA,
    _classify_table,
    _family_identity,
    _open_pinned_snapshot,
    _stable_private_projection,
    _validate_private_sqlite_family_blocker,
    _validate_private_sqlite_plan_blocker,
    _validate_table_relation,
    validate_codex_private_sqlite_compose_plan,
)
from .private_state import read_codex_private_bundle


PRIVATE_SQLITE_ACTION_PLAN_SCHEMA = (
    "dev.tinyland.bulkload.codex-private-sqlite-compose-action-plan.v5"
)
PRIVATE_SQLITE_ACTION_PLAN_IMPLEMENTATION = (
    "sqlite-compose-closed-offline-action-plan-exact-v5"
)

_SHA256 = re.compile(r"[0-9a-f]{64}")
_IDENTIFIER = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
_ROLES = ("source", "destination")
_V4_TERMINAL_BLOCKERS = {
    "post-plan-private-close-required",
    "sqlite-composer-not-implemented",
    "session-union-execution-and-verification-not-implemented",
}
_PLANNED_EDGE_BLOCKER = "sqlite-post-compose-edge-closure-required"
_ACTION_BLOCKER_FIELDS = {
    "pinned-migration-adapter-not-implemented": set(),
    "exact-schema-contract-required": set(),
    "close-table-classification-differs": {"table"},
}
_FALSE_READINESS = (
    "composer_implemented",
    "sqlite_union_ready",
    "sqlite_compose",
    "sqlite_publish",
    "combined",
    "ready_for_apply",
    "provider_runtime_acceptance_verified",
)


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


def _uuid_hex(value: str, label: str) -> str:
    return uuid.UUID(_require_uuid(value, label)).hex


def _require_identifier(value: Any, label: str) -> str:
    if not isinstance(value, str) or _IDENTIFIER.fullmatch(value) is None:
        raise BulkloadError(f"{label} must be a supported SQLite identifier")
    return value


def _dedupe_blockers(
    blockers: list[dict[str, Any]],
) -> list[dict[str, Any]]:
    by_body = {canonical_bytes(blocker): blocker for blocker in blockers}
    return [by_body[body] for body in sorted(by_body)]


def _opening_binding(opening_plan: dict[str, Any]) -> dict[str, Any]:
    accepted_inputs = deepcopy(opening_plan["accepted_inputs"])
    return {
        "schema": opening_plan["schema"],
        "plan_sha256": opening_plan["plan_sha256"],
        "body_sha256": sha256_bytes(canonical_bytes(opening_plan)),
        "accepted_inputs_sha256": sha256_bytes(canonical_bytes(accepted_inputs)),
    }


def _close_binding(close_request: dict[str, Any]) -> dict[str, Any]:
    return {
        "schema": close_request["schema"],
        "close_request_sha256": close_request["close_request_sha256"],
        "body_sha256": sha256_bytes(canonical_bytes(close_request)),
        "runtime_authority": deepcopy(close_request["runtime_authority"]),
        "writer_stop_epoch": deepcopy(close_request["writer_stop_epoch"]),
    }


def _validate_close_binds_opening(
    opening_plan: dict[str, Any],
    close_request: dict[str, Any],
) -> None:
    expected_opening = {
        "schema": opening_plan["schema"],
        "plan_sha256": opening_plan["plan_sha256"],
        "body_sha256": sha256_bytes(canonical_bytes(opening_plan)),
        "accepted_inputs": opening_plan["accepted_inputs"],
        "accepted_inputs_sha256": sha256_bytes(
            canonical_bytes(opening_plan["accepted_inputs"])
        ),
    }
    expected_private = {
        role: {
            "binding": opening_plan["private_opening"][role],
            "binding_sha256": sha256_bytes(
                canonical_bytes(opening_plan["private_opening"][role])
            ),
        }
        for role in _ROLES
    }
    session = opening_plan["session_union"]
    if (
        close_request["opening_plan"] != expected_opening
        or close_request["private_opening"] != expected_private
        or close_request["session_union"]["plan_sha256"] != session["plan_sha256"]
        or close_request["session_union"]["source"] != session["source"]
        or close_request["session_union"]["destination"] != session["destination"]
        or close_request["session_union"]["prefix_evidence"]
        != session["prefix_evidence"]
    ):
        raise BulkloadError(
            "private SQLite close request does not bind the exact opening plan"
        )


def _private_close_context(
    close_request: dict[str, Any],
    source_a_directory: Path,
    source_b_directory: Path,
    destination_a_directory: Path,
    destination_b_directory: Path,
) -> tuple[
    dict[str, list[dict[str, str]]],
    dict[str, dict[str, Any]],
    dict[str, Path],
]:
    bindings = validate_codex_private_sqlite_private_reclose_set(
        close_request,
        source_a_directory,
        source_b_directory,
        destination_a_directory,
        destination_b_directory,
    )
    directories = {
        "source": (source_a_directory, source_b_directory),
        "destination": (destination_a_directory, destination_b_directory),
    }
    captures: dict[str, dict[str, Any]] = {}
    roots: dict[str, Path] = {}
    for role, role_directories in directories.items():
        first, first_root = read_codex_private_bundle(role_directories[0], role)
        second, _ = read_codex_private_bundle(role_directories[1], role)
        expected_projection = close_request["private_opening"][role]["binding"][
            "stable_projection"
        ]
        if (
            _stable_private_projection(first) != expected_projection
            or _stable_private_projection(second) != expected_projection
        ):
            raise BulkloadError(
                f"private SQLite private {role} close projection differs"
            )
        captures[role] = first
        roots[role] = first_root
    return bindings, captures, roots


def _session_paths(
    session_source_close_a: dict[str, Any],
    session_destination_close_a: dict[str, Any],
) -> dict[str, dict[str, str]]:
    paths: dict[str, dict[str, str]] = {"source": {}, "destination": {}}
    for role, wrapper in (
        ("source", session_source_close_a),
        ("destination", session_destination_close_a),
    ):
        for item in wrapper["snapshot"]["sessions"]:
            session_id = item.get("session_id")
            relative_path = item.get("relative_path")
            _require_uuid(session_id, f"private SQLite {role} session ID")
            if (
                not isinstance(relative_path, str)
                or not relative_path
                or Path(relative_path).is_absolute()
                or os.path.normpath(relative_path) != relative_path
                or ".." in Path(relative_path).parts
            ):
                raise BulkloadError(f"private SQLite {role} session path is invalid")
            previous = paths[role].get(session_id)
            if previous is not None and previous != relative_path:
                raise BulkloadError(f"private SQLite {role} session ID has two paths")
            paths[role][session_id] = relative_path
    return paths


def _validate_close_identity_set(
    close_request: dict[str, Any],
    private_bindings: dict[str, list[dict[str, str]]],
    session_bindings: dict[str, list[dict[str, str]]],
) -> None:
    identities = [
        item[key]
        for role in _ROLES
        for item in private_bindings[role]
        for key in ("capture_id", "quiescence_attestation_id")
    ]
    identities.extend(
        item["capture_id"] for role in _ROLES for item in session_bindings[role]
    )
    identities.append(close_request["writer_stop_epoch"]["epoch_id"])
    normalized = [
        _uuid_hex(item, "private SQLite closing evidence identity")
        for item in identities
    ]
    if len(normalized) != 13 or len(set(normalized)) != 13:
        raise BulkloadError(
            "private SQLite cross-plane closing identities are not distinct"
        )


def _family_capture_binding(
    capture: dict[str, Any],
    basename: str,
) -> dict[str, Any]:
    family = next(
        (item for item in capture["sqlite_families"] if item["basename"] == basename),
        None,
    )
    if family is None:
        raise BulkloadError("private SQLite closing family is absent")
    return {
        "capture_sha256": capture["capture_sha256"],
        "snapshot_sha256": family["sha256"],
        "snapshot_size": family["snapshot_size"],
        "source_sha256": family["source_sha256"],
        "source_size": family["source_size"],
        "schema_sha256": family["schema_sha256"],
        "migrations_sha256": family["migrations_sha256"],
        "application_id": family["application_id"],
        "user_version": family["user_version"],
    }


def _path_rewrite_required(
    basename: str,
    table: str,
    registry_family: dict[str, Any],
) -> bool:
    return any(
        rule["table"] == table for rule in registry_family["path_authorities"]
    ) and any(
        rule["family_basename"] == basename and rule["table"] == table
        for rule in registry_family.get("_path_map_rules", [])
    )


def _table_action(
    relation: dict[str, Any],
    *,
    rewrite_source_rollout_paths: bool,
) -> dict[str, Any]:
    base = {
        key: deepcopy(value)
        for key, value in relation.items()
        if key != "expected_output"
    }
    expected_output = deepcopy(relation.get("expected_output"))
    return {
        **base,
        "expected_output": expected_output,
        "operation": {
            "baseline": "preserve-destination-exact",
            "shared_equal": "retain-once-from-destination",
            "destination_only": "retain-from-destination",
            "source_only": "insert-in-canonical-identity-order",
            "rewrite_source_rollout_paths": rewrite_source_rollout_paths,
            "identity_remap": False,
            "deduplicate": False,
            "delete": False,
        },
    }


def _action_family(
    opening_family: dict[str, Any],
    source_capture: dict[str, Any],
    destination_capture: dict[str, Any],
    roots: dict[str, Path],
    registry_family: dict[str, Any] | None,
    path_map: dict[str, Any],
    accepted_session_paths: dict[str, dict[str, str]],
    *,
    deadline: float,
    plan_budget: dict[str, int],
) -> dict[str, Any]:
    basename = opening_family["basename"]
    migration = opening_family["migration_relation"]
    source_schema = opening_family["source_schema"]
    destination_schema = opening_family["destination_schema"]
    schema_exact = bool(
        source_schema["schema_contract_sha256"]
        == destination_schema["schema_contract_sha256"]
        and source_schema["raw_schema_sha256"]
        == destination_schema["raw_schema_sha256"]
        and source_schema["application_id"] == destination_schema["application_id"]
        and source_schema["user_version"] == destination_schema["user_version"]
    )
    blockers = [
        deepcopy(blocker)
        for blocker in opening_family["blockers"]
        if blocker["code"] != _PLANNED_EDGE_BLOCKER
    ]
    if migration["relation"] != "exact":
        blockers.append({"code": "pinned-migration-adapter-not-implemented"})
    if not schema_exact:
        blockers.append({"code": "exact-schema-contract-required"})

    tables: list[dict[str, Any]] = []
    if (
        registry_family is not None
        and schema_exact
        and migration["relation"] == "exact"
    ):
        opening_by_name = {
            relation["name"]: relation for relation in opening_family["table_relations"]
        }
        with _open_pinned_snapshot(
            roots["source"],
            basename,
        ) as source_connection:
            with _open_pinned_snapshot(
                roots["destination"],
                basename,
            ) as destination_connection:
                for table_rule in registry_family["tables"]:
                    relation, table_blockers = _classify_table(
                        source_connection,
                        destination_connection,
                        basename=basename,
                        source_contract=source_schema,
                        destination_contract=destination_schema,
                        table_rule=table_rule,
                        registry_family=registry_family,
                        path_map=path_map,
                        accepted_session_paths=accepted_session_paths,
                        deadline=deadline,
                        plan_budget=plan_budget,
                        include_expected_output=True,
                    )
                    opening_relation = opening_by_name.get(table_rule["name"])
                    observed_v4_shape = {
                        key: value
                        for key, value in relation.items()
                        if key != "expected_output"
                    }
                    if opening_relation != observed_v4_shape:
                        blockers.append(
                            {
                                "code": "close-table-classification-differs",
                                "table": table_rule["name"],
                            }
                        )
                    blockers.extend(table_blockers)
                    tables.append(
                        _table_action(
                            relation,
                            rewrite_source_rollout_paths=_path_rewrite_required(
                                basename,
                                table_rule["name"],
                                registry_family,
                            ),
                        )
                    )
    blockers = _dedupe_blockers(blockers)
    descriptive_complete = bool(
        not blockers
        and registry_family is not None
        and schema_exact
        and migration["relation"] == "exact"
        and opening_family["edge_closure"]["registry_matches_observed"]
        and len(tables) == len(registry_family["tables"])
        and all(
            table["semantic_classification_complete"]
            and table["conflicts"] == 0
            and table["expected_output"] is not None
            for table in tables
        )
    )
    return {
        "basename": basename,
        "family_role": opening_family["family_role"],
        "generation": opening_family["generation"],
        "opening_family_sha256": sha256_bytes(canonical_bytes(opening_family)),
        "source_artifact": _family_capture_binding(source_capture, basename),
        "destination_artifact": _family_capture_binding(
            destination_capture,
            basename,
        ),
        "schema": {
            "source_contract_sha256": source_schema["schema_contract_sha256"],
            "destination_contract_sha256": destination_schema["schema_contract_sha256"],
            "source_raw_sha256": source_schema["raw_schema_sha256"],
            "destination_raw_sha256": destination_schema["raw_schema_sha256"],
            "source_application_id": source_schema["application_id"],
            "destination_application_id": destination_schema["application_id"],
            "source_user_version": source_schema["user_version"],
            "destination_user_version": destination_schema["user_version"],
            "exact": schema_exact,
        },
        "migration": {
            "relation": migration["relation"],
            "source_migrations_sha256": migration["source_migrations_sha256"],
            "destination_migrations_sha256": migration["destination_migrations_sha256"],
            "adapter": deepcopy(migration["adapter"]),
            "exact": migration["relation"] == "exact",
        },
        "tables": tables,
        "edge_contract": {
            "registry_sha256": opening_family["edge_closure"]["registry_sha256"],
            "source_observed_sha256": opening_family["edge_closure"][
                "source_observed_sha256"
            ],
            "destination_observed_sha256": opening_family["edge_closure"][
                "destination_observed_sha256"
            ],
            "registry_matches_observed": opening_family["edge_closure"][
                "registry_matches_observed"
            ],
            "verify_after_compose": True,
        },
        "blockers": blockers,
        "descriptive_action_complete": descriptive_complete,
        "ready_for_offline_compose": False,
    }


def _node(
    identifier: str,
    kind: str,
    depends_on: list[str],
    *,
    family: str | None = None,
    table: str | None = None,
    row_count: int | None = None,
    expected_sha256: str | None = None,
) -> dict[str, Any]:
    return {
        "id": identifier,
        "kind": kind,
        "depends_on": depends_on,
        "family": family,
        "table": table,
        "row_count": row_count,
        "expected_sha256": expected_sha256,
    }


def _operation_graph(
    families: list[dict[str, Any]],
    *,
    descriptive_complete: bool,
) -> dict[str, Any]:
    if not descriptive_complete:
        return {
            "descriptive_only": True,
            "create_only": True,
            "existing_destination_mutation": False,
            "blocked": True,
            "nodes": [],
        }
    nodes = [
        _node(
            "create-versioned-staging",
            "create-versioned-owner-private-staging",
            [],
        )
    ]
    family_verifiers: list[str] = []
    for family in families:
        basename = family["basename"]
        baseline_id = f"baseline:{basename}"
        baseline_digest = sha256_bytes(
            canonical_bytes(
                [
                    {
                        "table": table["name"],
                        "state": table["destination"],
                    }
                    for table in family["tables"]
                ]
            )
        )
        nodes.append(
            _node(
                baseline_id,
                "stream-destination-family-baseline",
                ["create-versioned-staging"],
                family=basename,
                row_count=sum(
                    table["destination"]["row_count"] for table in family["tables"]
                ),
                expected_sha256=baseline_digest,
            )
        )
        table_nodes: list[str] = []
        for table in family["tables"]:
            table_id = f"source-only:{basename}:{table['name']}"
            nodes.append(
                _node(
                    table_id,
                    "insert-source-only-canonical-identity-order",
                    [baseline_id],
                    family=basename,
                    table=table["name"],
                    row_count=table["source_only"],
                    expected_sha256=table["expected_output"]["partitions"][
                        "source_only"
                    ]["semantic_rows_sha256"],
                )
            )
            table_nodes.append(table_id)
        verifier = f"verify:{basename}"
        family_digest = sha256_bytes(
            canonical_bytes(
                {
                    "tables": [
                        {
                            "name": table["name"],
                            "expected_output": table["expected_output"],
                        }
                        for table in family["tables"]
                    ],
                    "edge_contract": family["edge_contract"],
                }
            )
        )
        nodes.append(
            _node(
                verifier,
                "verify-family-schema-header-edges-and-output-digests",
                table_nodes or [baseline_id],
                family=basename,
                expected_sha256=family_digest,
            )
        )
        family_verifiers.append(verifier)
    nodes.extend(
        (
            _node(
                "write-manifest",
                "write-canonical-bundle-manifest",
                family_verifiers or ["create-versioned-staging"],
            ),
            _node(
                "write-receipt",
                "write-canonical-composition-receipt",
                ["write-manifest"],
            ),
            _node(
                "fsync-bundle",
                "fsync-complete-files-and-directories",
                ["write-receipt"],
            ),
            _node(
                "seal-bundle",
                "seal-complete-bundle-with-no-replace-rename",
                ["fsync-bundle"],
            ),
        )
    )
    return {
        "descriptive_only": True,
        "create_only": True,
        "existing_destination_mutation": False,
        "blocked": False,
        "nodes": nodes,
    }


def compile_codex_private_sqlite_action_plan(
    opening_plan: dict[str, Any],
    close_request: dict[str, Any],
    source_close_a_directory: Path,
    source_close_b_directory: Path,
    destination_close_a_directory: Path,
    destination_close_b_directory: Path,
    session_source_close_a: dict[str, Any],
    session_source_close_b: dict[str, Any],
    session_destination_close_a: dict[str, Any],
    session_destination_close_b: dict[str, Any],
    *,
    accept_opening_plan: str,
    accept_close_request: str,
    runtime_authority: dict[str, Any],
) -> dict[str, Any]:
    """Compile a closed, descriptive plan for a later offline composer."""
    deadline = time.monotonic() + MAX_SQLITE_PLAN_SECONDS
    private_runtime.validate_private_runtime_authority(runtime_authority)
    if (
        runtime_authority["policy_schema"]
        != private_runtime.LEGACY_PRIVATE_STATE_POLICY_SCHEMA_V5
    ):
        raise BulkloadError("private SQLite action plan requires policy v5")
    if close_request["runtime_authority"] != runtime_authority:
        raise BulkloadError(
            "private SQLite close and action-plan runtime authorities differ"
        )
    validate_codex_private_sqlite_compose_plan(opening_plan)
    if accept_opening_plan != opening_plan["plan_sha256"]:
        raise BulkloadError("accepted private SQLite opening-plan digest differs")
    validate_codex_private_sqlite_close_request(close_request)
    if accept_close_request != close_request["close_request_sha256"]:
        raise BulkloadError("accepted private SQLite close-request digest differs")
    _validate_close_binds_opening(opening_plan, close_request)

    private_bindings, captures, roots = _private_close_context(
        close_request,
        source_close_a_directory,
        source_close_b_directory,
        destination_close_a_directory,
        destination_close_b_directory,
    )
    session_bindings = validate_codex_private_sqlite_session_reclose_set(
        close_request,
        session_source_close_a,
        session_source_close_b,
        session_destination_close_a,
        session_destination_close_b,
    )
    _validate_close_identity_set(
        close_request,
        private_bindings,
        session_bindings,
    )
    accepted_session_paths = _session_paths(
        session_source_close_a,
        session_destination_close_a,
    )

    registry_by_basename = {
        family["basename"]: {
            **family,
            "_path_map_rules": opening_plan["path_map"]["rules"],
        }
        for family in opening_plan["adapter_registry"]["families"]
    }
    plan_budget = {"rows": 0, "bytes": 0}
    families = [
        _action_family(
            opening_family,
            captures["source"],
            captures["destination"],
            roots,
            registry_by_basename.get(opening_family["basename"]),
            opening_plan["path_map"],
            accepted_session_paths,
            deadline=deadline,
            plan_budget=plan_budget,
        )
        for opening_family in opening_plan["sqlite_families"]
    ]
    if (
        plan_budget["rows"] > MAX_SQLITE_PLAN_ROWS
        or plan_budget["bytes"] > MAX_SQLITE_PLAN_ROW_BYTES
    ):
        raise BulkloadError("private SQLite action-plan row budget exceeded")

    family_blockers = [
        {"basename": family["basename"], **blocker}
        for family in families
        for blocker in family["blockers"]
    ]
    opening_global_blockers = [
        deepcopy(blocker)
        for blocker in opening_plan["blockers"]
        if blocker["code"] not in _V4_TERMINAL_BLOCKERS and "basename" not in blocker
    ]
    blockers = _dedupe_blockers([*family_blockers, *opening_global_blockers])
    descriptive_complete = bool(
        not blockers
        and all(family["descriptive_action_complete"] for family in families)
    )
    action_plan: dict[str, Any] = {
        "schema": PRIVATE_SQLITE_ACTION_PLAN_SCHEMA,
        "created_at": utc_now(),
        "runtime_authority": deepcopy(runtime_authority),
        "accepted_inputs": {
            "opening_plan": _opening_binding(opening_plan),
            "close_request": _close_binding(close_request),
            "private_close": private_bindings,
            "session_close": session_bindings,
            "opening_global_blockers": opening_global_blockers,
        },
        "sqlite_families": families,
        "blockers": blockers,
        "operation_graph": _operation_graph(
            families,
            descriptive_complete=descriptive_complete,
        ),
        "post_plan_close_proven": True,
        "readiness": {
            "classification_complete": not blockers,
            "descriptive_action_complete": descriptive_complete,
            "ready_for_offline_compose": False,
            "composer_implemented": False,
            "sqlite_union_ready": False,
            "sqlite_compose": False,
            "sqlite_publish": False,
            "combined": False,
            "ready_for_apply": False,
            "provider_runtime_acceptance_verified": False,
        },
        "implementation": PRIVATE_SQLITE_ACTION_PLAN_IMPLEMENTATION,
    }
    action_plan["action_plan_sha256"] = object_digest(
        action_plan,
        "action_plan_sha256",
    )
    validate_codex_private_sqlite_action_plan(action_plan)
    if time.monotonic() > deadline:
        raise BulkloadError("private SQLite action planning exceeded its deadline")
    return action_plan


def _validate_artifact(value: Any, label: str) -> None:
    artifact = _require_exact_keys(
        value,
        {
            "capture_sha256",
            "snapshot_sha256",
            "snapshot_size",
            "source_sha256",
            "source_size",
            "schema_sha256",
            "migrations_sha256",
            "application_id",
            "user_version",
        },
        label,
    )
    for key in (
        "capture_sha256",
        "snapshot_sha256",
        "source_sha256",
        "schema_sha256",
        "migrations_sha256",
    ):
        _require_sha256(artifact[key], f"{label} {key}")
    for key in ("snapshot_size", "source_size", "application_id", "user_version"):
        if type(artifact[key]) is not int or artifact[key] < 0:
            raise BulkloadError(f"{label} {key} is invalid")


def _validate_action_blocker(value: Any, *, wrapped: bool) -> dict[str, Any]:
    if not isinstance(value, dict) or not isinstance(value.get("code"), str):
        raise BulkloadError("private SQLite action-plan blocker is invalid")
    body = {key: item for key, item in value.items() if key != "basename"}
    code = body["code"]
    if code in _ACTION_BLOCKER_FIELDS:
        blocker = _require_exact_keys(
            body,
            {"code", *_ACTION_BLOCKER_FIELDS[code]},
            "private SQLite action-plan blocker",
        )
        if "table" in blocker:
            _require_identifier(blocker["table"], "action-plan blocker table")
    else:
        _validate_private_sqlite_family_blocker(body)
    if wrapped:
        if set(value) != {"basename", *body}:
            raise BulkloadError("private SQLite wrapped blocker differs")
        if not isinstance(value["basename"], str) or not value["basename"]:
            raise BulkloadError("private SQLite blocker family is invalid")
    return value


def _validate_action_table(value: Any) -> dict[str, Any]:
    table = _require_exact_keys(
        value,
        {
            "name",
            "merge_class",
            "identity_columns",
            "source",
            "destination",
            "shared_equal",
            "source_only",
            "destination_only",
            "conflicts",
            "semantic_classification_complete",
            "expected_output",
            "operation",
        },
        "private SQLite action table",
    )
    v4_shape = {
        key: item
        for key, item in table.items()
        if key not in {"expected_output", "operation"}
    }
    _validate_table_relation(v4_shape, label="private SQLite action table")
    expected = table["expected_output"]
    if expected is not None:
        expected = _require_exact_keys(
            expected,
            {
                "row_count",
                "semantic_rows_sha256",
                "classified_bytes",
                "partitions",
            },
            "private SQLite expected output",
        )
        if (
            type(expected["row_count"]) is not int
            or expected["row_count"] < 0
            or type(expected["classified_bytes"]) is not int
            or expected["classified_bytes"] < 0
        ):
            raise BulkloadError("private SQLite expected output counts are invalid")
        _require_sha256(
            expected["semantic_rows_sha256"],
            "private SQLite expected-output digest",
        )
        if expected["row_count"] != (
            table["shared_equal"] + table["source_only"] + table["destination_only"]
        ):
            raise BulkloadError("private SQLite expected-output count differs")
        partitions = _require_exact_keys(
            expected["partitions"],
            {"shared_equal", "source_only", "destination_only"},
            "private SQLite expected-output partitions",
        )
        for name, count in (
            ("shared_equal", table["shared_equal"]),
            ("source_only", table["source_only"]),
            ("destination_only", table["destination_only"]),
        ):
            partition = _require_exact_keys(
                partitions[name],
                {"row_count", "semantic_rows_sha256", "classified_bytes"},
                f"private SQLite expected-output {name} partition",
            )
            if (
                type(partition["row_count"]) is not int
                or partition["row_count"] != count
                or type(partition["classified_bytes"]) is not int
                or partition["classified_bytes"] < 0
            ):
                raise BulkloadError(
                    f"private SQLite expected-output {name} partition differs"
                )
            _require_sha256(
                partition["semantic_rows_sha256"],
                f"private SQLite expected-output {name} partition digest",
            )
        if expected["classified_bytes"] != sum(
            partition["classified_bytes"] for partition in partitions.values()
        ):
            raise BulkloadError("private SQLite expected-output partition bytes differ")
    operation = _require_exact_keys(
        table["operation"],
        {
            "baseline",
            "shared_equal",
            "destination_only",
            "source_only",
            "rewrite_source_rollout_paths",
            "identity_remap",
            "deduplicate",
            "delete",
        },
        "private SQLite table operation",
    )
    if (
        operation["baseline"] != "preserve-destination-exact"
        or operation["shared_equal"] != "retain-once-from-destination"
        or operation["destination_only"] != "retain-from-destination"
        or operation["source_only"] != "insert-in-canonical-identity-order"
        or type(operation["rewrite_source_rollout_paths"]) is not bool
        or operation["identity_remap"] is not False
        or operation["deduplicate"] is not False
        or operation["delete"] is not False
    ):
        raise BulkloadError("private SQLite table operation is unsafe")
    if table["semantic_classification_complete"] and (
        table["conflicts"] != 0 or expected is None
    ):
        raise BulkloadError("private SQLite expected output is incomplete")
    return table


def _validate_action_family(value: Any) -> dict[str, Any]:
    family = _require_exact_keys(
        value,
        {
            "basename",
            "family_role",
            "generation",
            "opening_family_sha256",
            "source_artifact",
            "destination_artifact",
            "schema",
            "migration",
            "tables",
            "edge_contract",
            "blockers",
            "descriptive_action_complete",
            "ready_for_offline_compose",
        },
        "private SQLite action family",
    )
    expected_role, expected_generation = _family_identity(
        family["basename"],
        allow_unknown=True,
    )
    if (
        family["family_role"] != expected_role
        or family["generation"] != expected_generation
    ):
        raise BulkloadError("private SQLite action family identity differs")
    _require_sha256(
        family["opening_family_sha256"],
        "private SQLite opening-family digest",
    )
    _validate_artifact(family["source_artifact"], "private SQLite source artifact")
    _validate_artifact(
        family["destination_artifact"],
        "private SQLite destination artifact",
    )
    schema = _require_exact_keys(
        family["schema"],
        {
            "source_contract_sha256",
            "destination_contract_sha256",
            "source_raw_sha256",
            "destination_raw_sha256",
            "source_application_id",
            "destination_application_id",
            "source_user_version",
            "destination_user_version",
            "exact",
        },
        "private SQLite action schema",
    )
    for key in (
        "source_contract_sha256",
        "destination_contract_sha256",
        "source_raw_sha256",
        "destination_raw_sha256",
    ):
        _require_sha256(schema[key], f"private SQLite action schema {key}")
    for key in (
        "source_application_id",
        "destination_application_id",
        "source_user_version",
        "destination_user_version",
    ):
        if type(schema[key]) is not int or schema[key] < 0:
            raise BulkloadError(f"private SQLite action schema {key} is invalid")
    expected_schema_exact = bool(
        schema["source_contract_sha256"] == schema["destination_contract_sha256"]
        and schema["source_raw_sha256"] == schema["destination_raw_sha256"]
        and schema["source_application_id"] == schema["destination_application_id"]
        and schema["source_user_version"] == schema["destination_user_version"]
    )
    if schema["exact"] is not expected_schema_exact:
        raise BulkloadError("private SQLite exact-schema claim differs")
    migration = _require_exact_keys(
        family["migration"],
        {
            "relation",
            "source_migrations_sha256",
            "destination_migrations_sha256",
            "adapter",
            "exact",
        },
        "private SQLite action migration",
    )
    for key in ("source_migrations_sha256", "destination_migrations_sha256"):
        _require_sha256(migration[key], f"private SQLite action migration {key}")
    if migration["relation"] not in {
        "exact",
        "registered-prefix-upgrade",
        "blocked",
    }:
        raise BulkloadError("private SQLite action migration relation is invalid")
    if migration["adapter"] is not None:
        adapter = _require_exact_keys(
            migration["adapter"],
            {"adapter_id", "adapter_source_sha256"},
            "private SQLite action migration adapter",
        )
        if not isinstance(adapter["adapter_id"], str) or not adapter["adapter_id"]:
            raise BulkloadError("private SQLite action adapter ID is invalid")
        _require_sha256(
            adapter["adapter_source_sha256"],
            "private SQLite action adapter digest",
        )
    expected_migration_exact = bool(
        migration["relation"] == "exact"
        and migration["source_migrations_sha256"]
        == migration["destination_migrations_sha256"]
        and migration["adapter"] is None
    )
    if migration["exact"] is not expected_migration_exact:
        raise BulkloadError("private SQLite exact-migration claim differs")
    tables = family["tables"]
    if not isinstance(tables, list):
        raise BulkloadError("private SQLite action tables must be a list")
    for table in tables:
        _validate_action_table(table)
    table_names = [table["name"] for table in tables]
    if table_names != sorted(set(table_names)):
        raise BulkloadError("private SQLite action tables are not canonical")
    edge = _require_exact_keys(
        family["edge_contract"],
        {
            "registry_sha256",
            "source_observed_sha256",
            "destination_observed_sha256",
            "registry_matches_observed",
            "verify_after_compose",
        },
        "private SQLite action edge contract",
    )
    for key in (
        "registry_sha256",
        "source_observed_sha256",
        "destination_observed_sha256",
    ):
        _require_sha256(edge[key], f"private SQLite action edge {key}")
    if (
        type(edge["registry_matches_observed"]) is not bool
        or edge["verify_after_compose"] is not True
    ):
        raise BulkloadError("private SQLite action edge contract is invalid")
    blockers = family["blockers"]
    if not isinstance(blockers, list) or [
        canonical_bytes(blocker) for blocker in blockers
    ] != sorted({canonical_bytes(blocker) for blocker in blockers}):
        raise BulkloadError("private SQLite action family blockers are invalid")
    for blocker in blockers:
        _validate_action_blocker(blocker, wrapped=False)
    codes = {blocker["code"] for blocker in blockers}
    if (migration["relation"] != "exact") != (
        "pinned-migration-adapter-not-implemented" in codes
    ):
        raise BulkloadError("private SQLite migration-adapter blocker differs")
    if (not schema["exact"]) != ("exact-schema-contract-required" in codes):
        raise BulkloadError("private SQLite exact-schema blocker differs")
    expected_complete = bool(
        not blockers
        and schema["exact"]
        and migration["exact"]
        and edge["registry_matches_observed"]
        and all(
            table["semantic_classification_complete"]
            and table["conflicts"] == 0
            and table["expected_output"] is not None
            for table in tables
        )
    )
    if (
        family["descriptive_action_complete"] is not expected_complete
        or family["ready_for_offline_compose"] is not False
    ):
        raise BulkloadError("private SQLite family action readiness differs")
    return family


def _validate_close_bindings(
    value: Any,
    *,
    private: bool,
) -> set[str]:
    name = "private" if private else "session"
    closing = _require_exact_keys(
        value,
        {"source", "destination"},
        f"private SQLite {name} close bindings",
    )
    identities: set[str] = set()
    for role in _ROLES:
        items = closing[role]
        if not isinstance(items, list) or len(items) != 2:
            raise BulkloadError(
                f"private SQLite {name} {role} close passes are invalid"
            )
        for item in items:
            if private:
                binding = _require_exact_keys(
                    item,
                    {
                        "capture_id",
                        "capture_sha256",
                        "quiescence_attestation_id",
                        "quiescence_attestation_sha256",
                    },
                    "private SQLite private close binding",
                )
                identity_keys = ("capture_id", "quiescence_attestation_id")
                digest_keys = (
                    "capture_sha256",
                    "quiescence_attestation_sha256",
                )
            else:
                binding = _require_exact_keys(
                    item,
                    {
                        "capture_id",
                        "snapshot_sha256",
                        "reclose_capture_sha256",
                    },
                    "private SQLite session close binding",
                )
                identity_keys = ("capture_id",)
                digest_keys = ("snapshot_sha256", "reclose_capture_sha256")
            for key in identity_keys:
                normalized = _uuid_hex(
                    binding[key],
                    f"private SQLite {name} close identity",
                )
                if normalized in identities:
                    raise BulkloadError(
                        f"private SQLite {name} close identity is reused"
                    )
                identities.add(normalized)
            for key in digest_keys:
                _require_sha256(
                    binding[key],
                    f"private SQLite {name} close digest",
                )
    return identities


def _validate_operation_graph(
    value: Any,
    families: list[dict[str, Any]],
    *,
    descriptive_complete: bool,
) -> None:
    graph = _require_exact_keys(
        value,
        {
            "descriptive_only",
            "create_only",
            "existing_destination_mutation",
            "blocked",
            "nodes",
        },
        "private SQLite operation graph",
    )
    if (
        graph["descriptive_only"] is not True
        or graph["create_only"] is not True
        or graph["existing_destination_mutation"] is not False
        or graph["blocked"] is not (not descriptive_complete)
        or not isinstance(graph["nodes"], list)
    ):
        raise BulkloadError("private SQLite operation graph authority differs")
    for node in graph["nodes"]:
        record = _require_exact_keys(
            node,
            {
                "id",
                "kind",
                "depends_on",
                "family",
                "table",
                "row_count",
                "expected_sha256",
            },
            "private SQLite operation node",
        )
        if (
            not isinstance(record["id"], str)
            or not record["id"]
            or not isinstance(record["kind"], str)
            or not record["kind"]
            or not isinstance(record["depends_on"], list)
            or not all(
                isinstance(dependency, str) and dependency
                for dependency in record["depends_on"]
            )
        ):
            raise BulkloadError("private SQLite operation node is invalid")
        if record["row_count"] is not None and (
            type(record["row_count"]) is not int or record["row_count"] < 0
        ):
            raise BulkloadError("private SQLite operation row count is invalid")
        if record["expected_sha256"] is not None:
            _require_sha256(
                record["expected_sha256"],
                "private SQLite operation expected digest",
            )
    if graph != _operation_graph(
        families,
        descriptive_complete=descriptive_complete,
    ):
        raise BulkloadError("private SQLite operation graph differs from families")


def validate_codex_private_sqlite_action_plan(
    action_plan: dict[str, Any],
) -> None:
    """Deeply validate one self-digested, non-applicable v5 action plan."""
    plan = _require_exact_keys(
        action_plan,
        {
            "schema",
            "created_at",
            "runtime_authority",
            "accepted_inputs",
            "sqlite_families",
            "blockers",
            "operation_graph",
            "post_plan_close_proven",
            "readiness",
            "implementation",
            "action_plan_sha256",
        },
        "private SQLite action plan",
    )
    if plan["schema"] != PRIVATE_SQLITE_ACTION_PLAN_SCHEMA:
        raise BulkloadError("unsupported private SQLite action-plan schema")
    if not isinstance(plan["created_at"], str) or not plan["created_at"]:
        raise BulkloadError("private SQLite action-plan timestamp is invalid")
    private_runtime.validate_private_runtime_authority(plan["runtime_authority"])
    if (
        plan["runtime_authority"]["policy_schema"]
        != private_runtime.LEGACY_PRIVATE_STATE_POLICY_SCHEMA_V5
    ):
        raise BulkloadError("private SQLite action plan requires policy v5")
    accepted = _require_exact_keys(
        plan["accepted_inputs"],
        {
            "opening_plan",
            "close_request",
            "private_close",
            "session_close",
            "opening_global_blockers",
        },
        "private SQLite action-plan accepted inputs",
    )
    opening = _require_exact_keys(
        accepted["opening_plan"],
        {
            "schema",
            "plan_sha256",
            "body_sha256",
            "accepted_inputs_sha256",
        },
        "private SQLite action-plan opening binding",
    )
    if opening["schema"] != PRIVATE_SQLITE_PLAN_SCHEMA:
        raise BulkloadError("private SQLite action plan does not bind v4")
    for key in ("plan_sha256", "body_sha256", "accepted_inputs_sha256"):
        _require_sha256(opening[key], f"private SQLite opening {key}")
    close = _require_exact_keys(
        accepted["close_request"],
        {
            "schema",
            "close_request_sha256",
            "body_sha256",
            "runtime_authority",
            "writer_stop_epoch",
        },
        "private SQLite action-plan close binding",
    )
    if close["schema"] != PRIVATE_SQLITE_CLOSE_REQUEST_SCHEMA:
        raise BulkloadError("private SQLite action-plan close schema differs")
    for key in ("close_request_sha256", "body_sha256"):
        _require_sha256(close[key], f"private SQLite close {key}")
    private_runtime.validate_private_runtime_authority(close["runtime_authority"])
    if close["runtime_authority"] != plan["runtime_authority"]:
        raise BulkloadError(
            "private SQLite close and action-plan runtime authorities differ"
        )
    epoch = _require_exact_keys(
        close["writer_stop_epoch"],
        {
            "epoch_id",
            "stopped_at",
            "operator_acknowledged_writers_stopped",
            "provider_writer_proof",
        },
        "private SQLite action-plan writer-stop epoch",
    )
    epoch_identity = _uuid_hex(
        epoch["epoch_id"],
        "private SQLite action-plan writer-stop epoch ID",
    )
    if (
        not isinstance(epoch["stopped_at"], str)
        or not epoch["stopped_at"]
        or epoch["operator_acknowledged_writers_stopped"] is not True
        or epoch["provider_writer_proof"] is not False
    ):
        raise BulkloadError("private SQLite action-plan writer-stop epoch is invalid")
    private_ids = _validate_close_bindings(
        accepted["private_close"],
        private=True,
    )
    session_ids = _validate_close_bindings(
        accepted["session_close"],
        private=False,
    )
    if (
        private_ids & session_ids
        or epoch_identity in private_ids
        or epoch_identity in session_ids
    ):
        raise BulkloadError(
            "private SQLite cross-plane closing identities are not distinct"
        )
    opening_global_blockers = accepted["opening_global_blockers"]
    if not isinstance(opening_global_blockers, list) or [
        canonical_bytes(blocker) for blocker in opening_global_blockers
    ] != sorted({canonical_bytes(blocker) for blocker in opening_global_blockers}):
        raise BulkloadError("private SQLite opening blockers are invalid")
    for blocker in opening_global_blockers:
        _validate_private_sqlite_plan_blocker(blocker)

    families = plan["sqlite_families"]
    if not isinstance(families, list):
        raise BulkloadError("private SQLite action families must be a list")
    if not families and not any(
        blocker["code"] == "sqlite-family-set-mismatch"
        for blocker in opening_global_blockers
    ):
        raise BulkloadError(
            "private SQLite empty action families require a family-set blocker"
        )
    for family in families:
        _validate_action_family(family)
    basenames = [family["basename"] for family in families]
    if basenames != sorted(set(basenames)):
        raise BulkloadError("private SQLite action families are not canonical")

    blockers = plan["blockers"]
    if not isinstance(blockers, list) or [
        canonical_bytes(blocker) for blocker in blockers
    ] != sorted({canonical_bytes(blocker) for blocker in blockers}):
        raise BulkloadError("private SQLite action-plan blockers are invalid")
    expected_blockers = _dedupe_blockers(
        [
            {"basename": family["basename"], **blocker}
            for family in families
            for blocker in family["blockers"]
        ]
        + opening_global_blockers
    )
    if blockers != expected_blockers:
        raise BulkloadError("private SQLite action-plan blockers differ")
    family_basenames = set(basenames)
    for blocker in blockers:
        if "basename" in blocker:
            if blocker["basename"] not in family_basenames:
                raise BulkloadError("private SQLite blocker family is absent")
            _validate_action_blocker(blocker, wrapped=True)
        else:
            _validate_private_sqlite_plan_blocker(blocker)
    readiness = _require_exact_keys(
        plan["readiness"],
        {
            "classification_complete",
            "descriptive_action_complete",
            "ready_for_offline_compose",
            *_FALSE_READINESS,
        },
        "private SQLite action-plan readiness",
    )
    descriptive_complete = bool(
        not blockers
        and all(family["descriptive_action_complete"] for family in families)
    )
    if (
        readiness["classification_complete"] is not (not blockers)
        or readiness["descriptive_action_complete"] is not descriptive_complete
        or readiness["ready_for_offline_compose"] is not False
        or any(readiness[key] is not False for key in _FALSE_READINESS)
        or plan["post_plan_close_proven"] is not True
        or plan["implementation"] != PRIVATE_SQLITE_ACTION_PLAN_IMPLEMENTATION
    ):
        raise BulkloadError("private SQLite action-plan readiness differs")
    _validate_operation_graph(
        plan["operation_graph"],
        families,
        descriptive_complete=descriptive_complete,
    )
    if len(canonical_bytes(plan)) > MAX_SQLITE_PLAN_BYTES:
        raise BulkloadError("private SQLite action plan exceeds its byte budget")
    _require_sha256(
        plan["action_plan_sha256"],
        "private SQLite action-plan digest",
    )
    if object_digest(plan, "action_plan_sha256") != plan["action_plan_sha256"]:
        raise BulkloadError("private SQLite action-plan digest mismatch")


def validate_codex_private_sqlite_action_plan_against_close(
    action_plan: dict[str, Any],
    opening_plan: dict[str, Any],
    close_request: dict[str, Any],
    source_close_a_directory: Path,
    source_close_b_directory: Path,
    destination_close_a_directory: Path,
    destination_close_b_directory: Path,
    session_source_close_a: dict[str, Any],
    session_source_close_b: dict[str, Any],
    session_destination_close_a: dict[str, Any],
    session_destination_close_b: dict[str, Any],
) -> None:
    """Recompute every v5 claim from the exact closing evidence."""
    validate_codex_private_sqlite_action_plan(action_plan)
    recomputed = compile_codex_private_sqlite_action_plan(
        opening_plan,
        close_request,
        source_close_a_directory,
        source_close_b_directory,
        destination_close_a_directory,
        destination_close_b_directory,
        session_source_close_a,
        session_source_close_b,
        session_destination_close_a,
        session_destination_close_b,
        accept_opening_plan=action_plan["accepted_inputs"]["opening_plan"][
            "plan_sha256"
        ],
        accept_close_request=action_plan["accepted_inputs"]["close_request"][
            "close_request_sha256"
        ],
        runtime_authority=action_plan["runtime_authority"],
    )
    ignored = {"created_at", "action_plan_sha256"}
    observed = {key: value for key, value in action_plan.items() if key not in ignored}
    expected = {key: value for key, value in recomputed.items() if key not in ignored}
    if observed != expected:
        raise BulkloadError(
            "private SQLite action plan differs from its closing evidence"
        )
