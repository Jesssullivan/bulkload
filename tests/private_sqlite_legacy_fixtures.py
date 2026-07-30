"""Test-only assemblers for frozen v4/v5 SQLite artifact envelopes.

These helpers intentionally reuse the shipped fragment classifiers. They do not
copy filesystem traversal, SQLite schema inspection, or row-classification
logic, and they are not included in the installed Bulkload skill.
"""

from __future__ import annotations

from copy import deepcopy
import os
from pathlib import Path
from typing import Any

from bulkload_lib.model import object_digest
from bulkload_lib import private_sqlite_action_plan
from bulkload_lib import private_sqlite_close
from bulkload_lib import private_sqlite_plan
from bulkload_lib.sessions import capture_codex_sessions


def build_v4_compose_plan_fixture(
    compatibility_plan: dict[str, Any],
    source_a_directory: Path,
    source_b_directory: Path,
    destination_a_directory: Path,
    destination_b_directory: Path,
    *,
    adapter_registry: dict[str, Any],
    path_map: dict[str, Any],
    session_union_plan: dict[str, Any],
    session_source_a: dict[str, Any],
    session_source_b: dict[str, Any],
    session_destination_a: dict[str, Any],
    session_destination_b: dict[str, Any],
    runtime_authority: dict[str, Any],
    created_at: str,
) -> dict[str, Any]:
    """Assemble a persisted v4 candidate from production-classified fragments."""
    source_a, source_a_root = private_sqlite_plan.read_codex_private_bundle(
        source_a_directory,
        "source",
    )
    source_b, _ = private_sqlite_plan.read_codex_private_bundle(
        source_b_directory,
        "source",
    )
    destination_a, destination_a_root = private_sqlite_plan.read_codex_private_bundle(
        destination_a_directory,
        "destination",
    )
    destination_b, _ = private_sqlite_plan.read_codex_private_bundle(
        destination_b_directory,
        "destination",
    )
    source_pair = private_sqlite_plan._private_pair_binding(
        source_a,
        source_b,
        role="source",
    )
    destination_pair = private_sqlite_plan._private_pair_binding(
        destination_a,
        destination_b,
        role="destination",
    )

    source_families = {item["basename"] for item in source_a["sqlite_families"]}
    destination_families = {
        item["basename"] for item in destination_a["sqlite_families"]
    }
    registry_by_basename = {
        item["basename"]: item for item in adapter_registry["families"]
    }
    classification_blockers: list[dict[str, Any]] = []
    family_relations: list[dict[str, Any]] = []
    if source_families != destination_families:
        classification_blockers.append(
            {
                "code": "sqlite-family-set-mismatch",
                "missing_from_source": sorted(destination_families - source_families),
                "missing_from_destination": sorted(
                    source_families - destination_families
                ),
            }
        )
    else:
        plan_budget = {"rows": 0, "bytes": 0}
        for basename in sorted(source_families):
            relation, blockers = private_sqlite_plan._classify_family(
                basename,
                source_a,
                destination_a,
                source_a_root,
                destination_a_root,
                registry_by_basename.get(basename),
                path_map,
                session_union_plan,
                deadline=float("inf"),
                plan_budget=plan_budget,
            )
            family_relations.append(relation)
            classification_blockers.extend(
                {"basename": basename, **blocker} for blocker in blockers
            )

    unobserved_registry = sorted(
        set(registry_by_basename) - source_families - destination_families
    )
    if unobserved_registry:
        classification_blockers.append(
            {
                "code": "sqlite-registry-family-unobserved",
                "basenames": unobserved_registry,
            }
        )
    blockers = private_sqlite_plan._dedupe_blockers(
        [
            *classification_blockers,
            {"code": "post-plan-private-close-required"},
            {"code": "sqlite-composer-not-implemented"},
            {"code": "session-union-execution-and-verification-not-implemented"},
        ]
    )
    plan: dict[str, Any] = {
        "schema": private_sqlite_plan.PRIVATE_SQLITE_PLAN_SCHEMA,
        "created_at": created_at,
        "runtime_authority": runtime_authority,
        "accepted_inputs": {
            "compatibility_plan_sha256": compatibility_plan["plan_sha256"],
            "adapter_registry_sha256": adapter_registry["registry_sha256"],
            "path_map_sha256": path_map["path_map_sha256"],
            "session_union_plan_sha256": session_union_plan["plan_sha256"],
        },
        "private_opening": {
            "source": source_pair,
            "destination": destination_pair,
        },
        "session_union": {
            "plan_sha256": session_union_plan["plan_sha256"],
            "source": session_union_plan["source"],
            "destination": session_union_plan["destination"],
            "prefix_evidence": session_union_plan["prefix_evidence"],
            "ready_for_attended_copy": True,
            "executed": False,
            "verified": False,
        },
        "adapter_registry": adapter_registry,
        "path_map": path_map,
        "sqlite_families": family_relations,
        "blockers": blockers,
        "opening_stable": True,
        "post_plan_close_required": True,
        "post_plan_close_proven": False,
        "readiness": {
            "classification_complete": not classification_blockers,
            "composer_implemented": False,
            "sqlite_union_ready": False,
            "sqlite_compose": False,
            "sqlite_publish": False,
            "combined": False,
            "ready_for_apply": False,
        },
        "implementation": "sqlite-compose-opening-plan-only-v4",
    }
    plan["plan_sha256"] = object_digest(plan, "plan_sha256")
    return plan


def build_v5_close_request_fixture(
    opening_plan: dict[str, Any],
    session_union_plan: dict[str, Any],
    session_source_a: dict[str, Any],
    session_source_b: dict[str, Any],
    session_destination_a: dict[str, Any],
    session_destination_b: dict[str, Any],
    runtime_authority: dict[str, Any],
    *,
    writer_stop_epoch_id: str,
    writer_stop_epoch_at: str,
    created_at: str,
) -> dict[str, Any]:
    """Assemble a persisted v5 close candidate from binding fragments."""
    session_opening = {
        "source": private_sqlite_close._session_opening_binding(
            session_source_a,
            session_source_b,
            role="source",
            session_plan_custody=session_union_plan["source"],
        ),
        "destination": private_sqlite_close._session_opening_binding(
            session_destination_a,
            session_destination_b,
            role="destination",
            session_plan_custody=session_union_plan["destination"],
        ),
    }
    request: dict[str, Any] = {
        "schema": private_sqlite_close.PRIVATE_SQLITE_CLOSE_REQUEST_SCHEMA,
        "created_at": created_at,
        "runtime_authority": deepcopy(runtime_authority),
        "opening_inputs_revalidated": True,
        "opening_plan": private_sqlite_close._opening_plan_binding(opening_plan),
        "private_opening": {
            role: private_sqlite_close._private_opening_binding(
                opening_plan["private_opening"][role]
            )
            for role in ("source", "destination")
        },
        "session_union": private_sqlite_close._session_union_binding(
            session_union_plan
        ),
        "session_opening": session_opening,
        "writer_stop_epoch": private_sqlite_close._writer_stop_epoch(
            writer_stop_epoch_id,
            writer_stop_epoch_at,
            operator_acknowledged_writers_stopped=True,
            created_at=created_at,
        ),
        "required_closing": {
            "planes": ["private", "session"],
            "roles": ["source", "destination"],
            "passes_per_role": 2,
        },
    }
    request["close_request_sha256"] = object_digest(
        request,
        "close_request_sha256",
    )
    return request


def build_v5_session_reclose_fixture(
    root: Path,
    *,
    role: str,
    close_request: dict[str, Any],
    acknowledge_writers_quiesced: bool,
) -> dict[str, Any]:
    """Assemble a persisted v5 reclose candidate around one fresh snapshot."""
    opening = close_request["session_opening"][role]["stable_projection"]
    root = root.expanduser()
    root_text = os.fspath(root)
    if (
        not root.is_absolute()
        or os.path.normpath(root_text) != root_text
        or root_text != opening["root"]
    ):
        raise AssertionError("session reclose fixture root differs from opening")
    budgets = opening["budgets"]
    snapshot = capture_codex_sessions(
        root,
        role=role,
        acknowledge_writers_quiesced=acknowledge_writers_quiesced,
        host_authority_id=opening["host_authority_id"],
        max_files=budgets["max_files"],
        max_entries=budgets["max_entries"],
        max_directories=budgets["max_directories"],
        max_bytes=budgets["max_total_bytes"],
        max_file_bytes=budgets["max_file_bytes"],
        max_record_bytes=budgets["max_record_bytes"],
        max_records_per_file=budgets["max_records_per_file"],
        max_path_bytes=budgets["max_path_bytes"],
        max_path_components=budgets["max_path_components"],
        max_catalog_bytes=budgets["max_catalog_bytes"],
        max_errors=budgets["max_errors"],
        max_output_bytes=budgets["max_output_bytes"],
    )
    capture: dict[str, Any] = {
        "schema": private_sqlite_close.PRIVATE_SQLITE_SESSION_RECLOSE_SCHEMA,
        "created_at": snapshot["captured_at"],
        "close_request_sha256": close_request["close_request_sha256"],
        "writer_stop_epoch": deepcopy(close_request["writer_stop_epoch"]),
        "role": role,
        "opening_stable_projection_sha256": close_request["session_opening"][role][
            "stable_projection_sha256"
        ],
        "snapshot": snapshot,
    }
    capture["reclose_capture_sha256"] = object_digest(
        capture,
        "reclose_capture_sha256",
    )
    return capture


def build_v5_action_plan_fixture(
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
    runtime_authority: dict[str, Any],
    created_at: str,
) -> dict[str, Any]:
    """Assemble a persisted v5 action candidate from classified fragments."""
    private_bindings, captures, roots = (
        private_sqlite_action_plan._private_close_context(
            close_request,
            source_close_a_directory,
            source_close_b_directory,
            destination_close_a_directory,
            destination_close_b_directory,
        )
    )
    session_bindings = (
        private_sqlite_action_plan.validate_codex_private_sqlite_session_reclose_set(
            close_request,
            session_source_close_a,
            session_source_close_b,
            session_destination_close_a,
            session_destination_close_b,
        )
    )
    private_sqlite_action_plan._validate_close_identity_set(
        close_request,
        private_bindings,
        session_bindings,
    )
    accepted_session_paths = private_sqlite_action_plan._session_paths(
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
        private_sqlite_action_plan._action_family(
            opening_family,
            captures["source"],
            captures["destination"],
            roots,
            registry_by_basename.get(opening_family["basename"]),
            opening_plan["path_map"],
            accepted_session_paths,
            deadline=float("inf"),
            plan_budget=plan_budget,
        )
        for opening_family in opening_plan["sqlite_families"]
    ]
    family_blockers = [
        {"basename": family["basename"], **blocker}
        for family in families
        for blocker in family["blockers"]
    ]
    opening_global_blockers = [
        deepcopy(blocker)
        for blocker in opening_plan["blockers"]
        if blocker["code"] not in private_sqlite_action_plan._V4_TERMINAL_BLOCKERS
        and "basename" not in blocker
    ]
    blockers = private_sqlite_action_plan._dedupe_blockers(
        [*family_blockers, *opening_global_blockers]
    )
    descriptive_complete = bool(
        not blockers
        and all(family["descriptive_action_complete"] for family in families)
    )
    action_plan: dict[str, Any] = {
        "schema": private_sqlite_action_plan.PRIVATE_SQLITE_ACTION_PLAN_SCHEMA,
        "created_at": created_at,
        "runtime_authority": deepcopy(runtime_authority),
        "accepted_inputs": {
            "opening_plan": private_sqlite_action_plan._opening_binding(opening_plan),
            "close_request": private_sqlite_action_plan._close_binding(close_request),
            "private_close": private_bindings,
            "session_close": session_bindings,
            "opening_global_blockers": opening_global_blockers,
        },
        "sqlite_families": families,
        "blockers": blockers,
        "operation_graph": private_sqlite_action_plan._operation_graph(
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
        "implementation": (
            private_sqlite_action_plan.PRIVATE_SQLITE_ACTION_PLAN_IMPLEMENTATION
        ),
    }
    action_plan["action_plan_sha256"] = object_digest(
        action_plan,
        "action_plan_sha256",
    )
    return action_plan
