"""Immutable cross-plane close evidence for private Codex SQLite planning.

This module is intentionally limited to a close request and fresh Codex
session re-close evidence.  It has no composer, publisher, installer, apply,
activation, or provider-runtime entrypoint.
"""

from __future__ import annotations

from copy import deepcopy
from datetime import UTC, datetime
import os
from pathlib import Path
import re
from typing import Any
import uuid

from .model import (
    BulkloadError,
    canonical_bytes,
    object_digest,
    require_digest,
    sha256_bytes,
    utc_now,
)
from . import private_runtime
from .private_sqlite_plan import (
    PRIVATE_SQLITE_PLAN_SCHEMA,
    _stable_private_projection,
    validate_codex_private_sqlite_compose_plan,
    validate_codex_private_sqlite_compose_plan_against_inputs,
)
from .private_state import (
    read_codex_private_bundle,
    validate_codex_private_capture,
)
from .sessions import (
    CODEX_SESSION_PLAN_SCHEMA,
    MAX_CODEX_SESSION_PLAN_BYTES,
    capture_codex_sessions,
    validate_codex_session_snapshot,
    validate_codex_session_union_evidence_binding,
    validate_codex_session_union_plan,
)


PRIVATE_SQLITE_CLOSE_REQUEST_SCHEMA = (
    "dev.tinyland.bulkload.codex-private-sqlite-close-request.v1"
)
PRIVATE_SQLITE_SESSION_RECLOSE_SCHEMA = (
    "dev.tinyland.bulkload.codex-private-sqlite-session-reclose.v1"
)
MAX_PRIVATE_SQLITE_CLOSE_BYTES = MAX_CODEX_SESSION_PLAN_BYTES
MAX_WRITER_STOP_EPOCH_AGE_SECONDS = 300

_SHA256 = re.compile(r"[0-9a-f]{64}")
_UTC_SECONDS = re.compile(r"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z")
_SESSION_CUSTODY_FIELDS = (
    "host",
    "host_authority_id",
    "root",
    "resolved_root",
    "root_identity",
    "root_lineage",
    "catalog_sha256",
)
_SESSION_STABLE_FIELDS = (
    "role",
    "host",
    "host_authority_id",
    "root",
    "resolved_root",
    "root_identity",
    "root_lineage",
    "budgets",
    "catalog_sha256",
    "catalog",
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
    try:
        return uuid.UUID(value).hex
    except (AttributeError, ValueError) as error:
        raise BulkloadError(f"{label} is not a UUID evidence identity") from error


def _parse_utc_seconds(value: Any, label: str) -> datetime:
    if not isinstance(value, str) or _UTC_SECONDS.fullmatch(value) is None:
        raise BulkloadError(f"{label} must be a canonical UTC-seconds timestamp")
    try:
        parsed = datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=UTC)
    except ValueError as error:
        raise BulkloadError(
            f"{label} must be a canonical UTC-seconds timestamp"
        ) from error
    if parsed.strftime("%Y-%m-%dT%H:%M:%SZ") != value:
        raise BulkloadError(f"{label} must be a canonical UTC-seconds timestamp")
    return parsed


def _catalog_body(snapshot: dict[str, Any]) -> dict[str, Any]:
    return {
        "directories": snapshot["directories"],
        "sessions": snapshot["sessions"],
        "non_private_file_count": snapshot["non_private_file_count"],
        "non_private_directory_count": snapshot["non_private_directory_count"],
    }


def _session_stable_projection(snapshot: dict[str, Any]) -> dict[str, Any]:
    return {
        "role": snapshot["role"],
        "host": snapshot["host"],
        "host_authority_id": snapshot["host_authority_id"],
        "root": snapshot["root"],
        "resolved_root": snapshot["resolved_root"],
        "root_identity": snapshot["root_identity"],
        "root_lineage": snapshot["root_lineage"],
        "budgets": snapshot["budgets"],
        "catalog_sha256": snapshot["catalog_sha256"],
        "catalog": _catalog_body(snapshot),
    }


def _session_custody_from_opening(
    projection: dict[str, Any],
    capture_ids: list[str],
) -> dict[str, Any]:
    return {field: deepcopy(projection[field]) for field in _SESSION_CUSTODY_FIELDS} | {
        "capture_ids": list(capture_ids)
    }


def _session_opening_binding(
    first: dict[str, Any],
    second: dict[str, Any],
    *,
    role: str,
    session_plan_custody: dict[str, Any],
) -> dict[str, Any]:
    for snapshot in (first, second):
        validate_codex_session_snapshot(snapshot)
        if (
            snapshot["complete"] is not True
            or snapshot["writers_quiesced"] is not True
            or snapshot["role"] != role
        ):
            raise BulkloadError(
                f"private SQLite session {role} opening capture is not complete "
                "quiescent role evidence"
            )
    if first["capture_id"] == second["capture_id"]:
        raise BulkloadError(
            f"private SQLite session {role} opening passes are not distinct"
        )
    first_projection = _session_stable_projection(first)
    second_projection = _session_stable_projection(second)
    if canonical_bytes(first_projection) != canonical_bytes(second_projection):
        raise BulkloadError(f"private SQLite session {role} opening catalogs differ")
    capture_ids = [first["capture_id"], second["capture_id"]]
    if (
        _session_custody_from_opening(first_projection, capture_ids)
        != session_plan_custody
    ):
        raise BulkloadError(
            f"private SQLite session {role} opening differs from union custody"
        )
    snapshot_sha256s = [
        first["snapshot_sha256"],
        second["snapshot_sha256"],
    ]
    if len(set(snapshot_sha256s)) != 2:
        raise BulkloadError(
            f"private SQLite session {role} opening snapshots are reused"
        )
    return {
        "stable_projection": deepcopy(first_projection),
        "stable_projection_sha256": sha256_bytes(canonical_bytes(first_projection)),
        "opening_capture_ids": capture_ids,
        "opening_snapshot_sha256s": snapshot_sha256s,
    }


def _private_opening_binding(value: dict[str, Any]) -> dict[str, Any]:
    return {
        "binding": deepcopy(value),
        "binding_sha256": sha256_bytes(canonical_bytes(value)),
    }


def _opening_plan_binding(plan: dict[str, Any]) -> dict[str, Any]:
    accepted_inputs = deepcopy(plan["accepted_inputs"])
    return {
        "schema": plan["schema"],
        "plan_sha256": plan["plan_sha256"],
        "body_sha256": sha256_bytes(canonical_bytes(plan)),
        "accepted_inputs": accepted_inputs,
        "accepted_inputs_sha256": sha256_bytes(canonical_bytes(accepted_inputs)),
    }


def _session_union_binding(plan: dict[str, Any]) -> dict[str, Any]:
    capture_ids = validate_codex_session_union_evidence_binding(
        plan["source"],
        plan["destination"],
        plan["prefix_evidence"],
    )
    return {
        "schema": plan["schema"],
        "plan_sha256": plan["plan_sha256"],
        "body_sha256": sha256_bytes(canonical_bytes(plan)),
        "source": deepcopy(plan["source"]),
        "destination": deepcopy(plan["destination"]),
        "prefix_evidence": deepcopy(plan["prefix_evidence"]),
        "evidence_capture_ids": sorted(capture_ids),
    }


def _writer_stop_epoch(
    epoch_id: str,
    stopped_at: str,
    *,
    operator_acknowledged_writers_stopped: bool,
    created_at: str,
) -> dict[str, Any]:
    _require_uuid(epoch_id, "writer-stop epoch ID")
    stopped = _parse_utc_seconds(stopped_at, "writer-stop epoch timestamp")
    created = _parse_utc_seconds(created_at, "close request timestamp")
    age = (created - stopped).total_seconds()
    if age < 0 or age > MAX_WRITER_STOP_EPOCH_AGE_SECONDS:
        raise BulkloadError("writer-stop epoch is not fresh for this close request")
    if operator_acknowledged_writers_stopped is not True:
        raise BulkloadError(
            "writer-stop epoch requires an operator writer-stop acknowledgement"
        )
    return {
        "epoch_id": epoch_id,
        "stopped_at": stopped_at,
        "operator_acknowledged_writers_stopped": True,
        "provider_writer_proof": False,
    }


def compile_codex_private_sqlite_close_request(
    opening_plan: dict[str, Any],
    session_union_plan: dict[str, Any],
    session_source_a: dict[str, Any],
    session_source_b: dict[str, Any],
    session_destination_a: dict[str, Any],
    session_destination_b: dict[str, Any],
    runtime_authority: dict[str, Any],
    *,
    accept_opening_plan: str,
    accept_session_union_plan: str,
    writer_stop_epoch_id: str,
    writer_stop_epoch_at: str,
    acknowledge_provider_writers_stopped: bool,
    opening_compatibility_plan: dict[str, Any],
    opening_source_a_directory: Path,
    opening_source_b_directory: Path,
    opening_destination_a_directory: Path,
    opening_destination_b_directory: Path,
    opening_adapter_registry: dict[str, Any],
    opening_path_map: dict[str, Any],
    opening_session_prefix_request: dict[str, Any] | None = None,
    opening_session_source_prefix_a: dict[str, Any] | None = None,
    opening_session_source_prefix_b: dict[str, Any] | None = None,
    opening_session_destination_prefix_a: dict[str, Any] | None = None,
    opening_session_destination_prefix_b: dict[str, Any] | None = None,
    opening_session_close_request: dict[str, Any] | None = None,
    opening_session_source_close_a: dict[str, Any] | None = None,
    opening_session_source_close_b: dict[str, Any] | None = None,
    opening_session_destination_close_a: dict[str, Any] | None = None,
    opening_session_destination_close_b: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Bind the immutable v4 opening and both live-plane opening catalogs."""
    private_runtime.validate_private_runtime_authority(runtime_authority)
    if (
        runtime_authority["policy_schema"]
        != private_runtime.LEGACY_PRIVATE_STATE_POLICY_SCHEMA_V5
    ):
        raise BulkloadError(
            "private SQLite close request requires the exact repaired v5 "
            "producer authority"
        )
    validate_codex_private_sqlite_compose_plan(opening_plan)
    if accept_opening_plan != opening_plan["plan_sha256"]:
        raise BulkloadError("accepted private SQLite opening-plan digest differs")
    validate_codex_private_sqlite_compose_plan_against_inputs(
        opening_plan,
        opening_compatibility_plan,
        opening_source_a_directory,
        opening_source_b_directory,
        opening_destination_a_directory,
        opening_destination_b_directory,
        adapter_registry=opening_adapter_registry,
        path_map=opening_path_map,
        session_union_plan=session_union_plan,
        session_source_a=session_source_a,
        session_source_b=session_source_b,
        session_destination_a=session_destination_a,
        session_destination_b=session_destination_b,
        session_prefix_request=opening_session_prefix_request,
        session_source_prefix_a=opening_session_source_prefix_a,
        session_source_prefix_b=opening_session_source_prefix_b,
        session_destination_prefix_a=opening_session_destination_prefix_a,
        session_destination_prefix_b=opening_session_destination_prefix_b,
        session_close_request=opening_session_close_request,
        session_source_close_a=opening_session_source_close_a,
        session_source_close_b=opening_session_source_close_b,
        session_destination_close_a=opening_session_destination_close_a,
        session_destination_close_b=opening_session_destination_close_b,
    )
    validate_codex_session_union_plan(session_union_plan)
    if accept_session_union_plan != session_union_plan["plan_sha256"]:
        raise BulkloadError("accepted Codex session union-plan digest differs")
    expected_session_binding = {
        "plan_sha256": session_union_plan["plan_sha256"],
        "source": session_union_plan["source"],
        "destination": session_union_plan["destination"],
        "prefix_evidence": session_union_plan["prefix_evidence"],
        "ready_for_attended_copy": True,
        "executed": False,
        "verified": False,
    }
    if opening_plan["session_union"] != expected_session_binding:
        raise BulkloadError(
            "private SQLite opening does not bind the exact session union plan"
        )
    if (
        session_union_plan["intent"]["ready_for_attended_copy"] is not True
        or session_union_plan["intent"]["blockers"]
    ):
        raise BulkloadError("Codex session union plan is not a closed ready plan")

    session_opening = {
        "source": _session_opening_binding(
            session_source_a,
            session_source_b,
            role="source",
            session_plan_custody=session_union_plan["source"],
        ),
        "destination": _session_opening_binding(
            session_destination_a,
            session_destination_b,
            role="destination",
            session_plan_custody=session_union_plan["destination"],
        ),
    }
    created_at = utc_now()
    epoch = _writer_stop_epoch(
        writer_stop_epoch_id,
        writer_stop_epoch_at,
        operator_acknowledged_writers_stopped=(acknowledge_provider_writers_stopped),
        created_at=created_at,
    )
    request: dict[str, Any] = {
        "schema": PRIVATE_SQLITE_CLOSE_REQUEST_SCHEMA,
        "created_at": created_at,
        "runtime_authority": deepcopy(runtime_authority),
        "opening_inputs_revalidated": True,
        "opening_plan": _opening_plan_binding(opening_plan),
        "private_opening": {
            role: _private_opening_binding(opening_plan["private_opening"][role])
            for role in ("source", "destination")
        },
        "session_union": _session_union_binding(session_union_plan),
        "session_opening": session_opening,
        "writer_stop_epoch": epoch,
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
    validate_codex_private_sqlite_close_request(request)
    validate_codex_private_sqlite_close_request_against_openings(
        request,
        opening_plan,
        session_union_plan,
        session_source_a,
        session_source_b,
        session_destination_a,
        session_destination_b,
        runtime_authority,
    )
    return request


def _validate_private_opening_binding(
    value: Any,
    *,
    role: str,
) -> set[str]:
    outer = _require_exact_keys(
        value,
        {"binding", "binding_sha256"},
        f"private SQLite {role} opening binding",
    )
    binding = _require_exact_keys(
        outer["binding"],
        {
            "role",
            "host",
            "host_authority_id",
            "codex_version",
            "capture_ids",
            "capture_sha256s",
            "quiescence_attestation_ids",
            "stable_projection",
            "stable_projection_sha256",
        },
        f"private SQLite {role} opening",
    )
    if binding["role"] != role:
        raise BulkloadError("private SQLite opening role differs")
    _require_uuid(binding["host_authority_id"], "private host authority ID")
    evidence_ids: list[str] = []
    for key in ("capture_ids", "quiescence_attestation_ids"):
        items = binding[key]
        if not isinstance(items, list) or len(items) != 2:
            raise BulkloadError(f"private SQLite {role} {key} are invalid")
        evidence_ids.extend(
            _uuid_hex(item, f"private SQLite {role} {key}") for item in items
        )
        if len(set(items)) != 2:
            raise BulkloadError(f"private SQLite {role} {key} are reused")
    capture_sha256s = binding["capture_sha256s"]
    if not isinstance(capture_sha256s, list) or len(capture_sha256s) != 2:
        raise BulkloadError("private SQLite capture digests are invalid")
    for digest in capture_sha256s:
        _require_sha256(digest, "private SQLite capture digest")
    if len(set(capture_sha256s)) != 2:
        raise BulkloadError("private SQLite capture digests are reused")
    _require_sha256(
        binding["stable_projection_sha256"],
        "private SQLite stable projection digest",
    )
    if (
        sha256_bytes(canonical_bytes(binding["stable_projection"]))
        != binding["stable_projection_sha256"]
        or binding["stable_projection"].get("role") != role
        or binding["stable_projection"].get("host") != binding["host"]
        or binding["stable_projection"].get("host_authority_id")
        != binding["host_authority_id"]
        or binding["stable_projection"].get("codex_version") != binding["codex_version"]
    ):
        raise BulkloadError("private SQLite stable projection binding differs")
    _require_sha256(outer["binding_sha256"], "private opening binding digest")
    if sha256_bytes(canonical_bytes(binding)) != outer["binding_sha256"]:
        raise BulkloadError("private SQLite opening binding digest mismatch")
    return set(evidence_ids)


def _validate_session_stable_projection(
    value: Any,
    *,
    role: str,
) -> dict[str, Any]:
    projection = _require_exact_keys(
        value,
        set(_SESSION_STABLE_FIELDS),
        f"private SQLite session {role} stable projection",
    )
    if (
        projection["role"] != role
        or not isinstance(projection["host"], str)
        or not projection["host"]
        or not isinstance(projection["host_authority_id"], str)
        or not isinstance(projection["root"], str)
        or not isinstance(projection["resolved_root"], str)
        or not isinstance(projection["budgets"], dict)
    ):
        raise BulkloadError(
            f"private SQLite session {role} stable projection is invalid"
        )
    _require_uuid(
        projection["host_authority_id"],
        f"private SQLite session {role} host authority",
    )
    _require_sha256(
        projection["catalog_sha256"],
        f"private SQLite session {role} catalog digest",
    )
    catalog = _require_exact_keys(
        projection["catalog"],
        {
            "directories",
            "sessions",
            "non_private_file_count",
            "non_private_directory_count",
        },
        f"private SQLite session {role} catalog",
    )
    if sha256_bytes(canonical_bytes(catalog)) != projection["catalog_sha256"]:
        raise BulkloadError(f"private SQLite session {role} catalog digest mismatch")
    return projection


def validate_codex_private_sqlite_close_request(
    request: dict[str, Any],
) -> None:
    """Structurally validate a self-contained cross-plane close request."""
    value = _require_exact_keys(
        request,
        {
            "schema",
            "created_at",
            "runtime_authority",
            "opening_inputs_revalidated",
            "opening_plan",
            "private_opening",
            "session_union",
            "session_opening",
            "writer_stop_epoch",
            "required_closing",
            "close_request_sha256",
        },
        "private SQLite close request",
    )
    if value["schema"] != PRIVATE_SQLITE_CLOSE_REQUEST_SCHEMA:
        raise BulkloadError("unsupported private SQLite close-request schema")
    private_runtime.validate_private_runtime_authority(value["runtime_authority"])
    if (
        value["runtime_authority"]["policy_schema"]
        != private_runtime.LEGACY_PRIVATE_STATE_POLICY_SCHEMA_V5
    ):
        raise BulkloadError(
            "private SQLite close request requires the exact repaired v5 "
            "producer authority"
        )
    if value["opening_inputs_revalidated"] is not True:
        raise BulkloadError(
            "private SQLite close request lacks complete opening recomputation"
        )
    created = _parse_utc_seconds(
        value["created_at"],
        "private SQLite close-request timestamp",
    )
    opening = _require_exact_keys(
        value["opening_plan"],
        {
            "schema",
            "plan_sha256",
            "body_sha256",
            "accepted_inputs",
            "accepted_inputs_sha256",
        },
        "private SQLite opening-plan binding",
    )
    if opening["schema"] != PRIVATE_SQLITE_PLAN_SCHEMA:
        raise BulkloadError("private SQLite close request does not bind v4")
    for key in ("plan_sha256", "body_sha256", "accepted_inputs_sha256"):
        _require_sha256(opening[key], f"private SQLite opening {key}")
    if (
        sha256_bytes(canonical_bytes(opening["accepted_inputs"]))
        != opening["accepted_inputs_sha256"]
    ):
        raise BulkloadError("private SQLite accepted-input binding differs")

    private_opening = _require_exact_keys(
        value["private_opening"],
        {"source", "destination"},
        "private SQLite private opening",
    )
    private_ids: set[str] = set()
    for role in ("source", "destination"):
        role_ids = _validate_private_opening_binding(
            private_opening[role],
            role=role,
        )
        if private_ids & role_ids:
            raise BulkloadError(
                "private SQLite opening evidence IDs are not globally distinct"
            )
        private_ids |= role_ids
    if len(private_ids) != 8:
        raise BulkloadError(
            "private SQLite opening evidence IDs are not globally distinct"
        )

    session = _require_exact_keys(
        value["session_union"],
        {
            "schema",
            "plan_sha256",
            "body_sha256",
            "source",
            "destination",
            "prefix_evidence",
            "evidence_capture_ids",
        },
        "private SQLite session-union binding",
    )
    if session["schema"] != CODEX_SESSION_PLAN_SCHEMA:
        raise BulkloadError("private SQLite session-union schema differs")
    _require_sha256(session["plan_sha256"], "session-union plan digest")
    _require_sha256(session["body_sha256"], "session-union body digest")
    session_ids = validate_codex_session_union_evidence_binding(
        session["source"],
        session["destination"],
        session["prefix_evidence"],
    )
    if not isinstance(session["evidence_capture_ids"], list) or session[
        "evidence_capture_ids"
    ] != sorted(session_ids):
        raise BulkloadError("private SQLite session evidence-ID binding differs")
    normalized_session_ids = {
        _uuid_hex(item, "private SQLite session evidence ID") for item in session_ids
    }
    if private_ids & normalized_session_ids:
        raise BulkloadError("private and session evidence reuse an evidence ID")
    accepted_session_digest = opening["accepted_inputs"].get(
        "session_union_plan_sha256"
    )
    if accepted_session_digest != session["plan_sha256"]:
        raise BulkloadError("private SQLite accepted session digest differs")

    session_opening = _require_exact_keys(
        value["session_opening"],
        {"source", "destination"},
        "private SQLite session opening",
    )
    for role in ("source", "destination"):
        role_opening = _require_exact_keys(
            session_opening[role],
            {
                "stable_projection",
                "stable_projection_sha256",
                "opening_capture_ids",
                "opening_snapshot_sha256s",
            },
            f"private SQLite session {role} opening",
        )
        projection = _validate_session_stable_projection(
            role_opening["stable_projection"],
            role=role,
        )
        _require_sha256(
            role_opening["stable_projection_sha256"],
            f"private SQLite session {role} stable projection digest",
        )
        if (
            sha256_bytes(canonical_bytes(projection))
            != role_opening["stable_projection_sha256"]
        ):
            raise BulkloadError(
                f"private SQLite session {role} projection digest mismatch"
            )
        capture_ids = role_opening["opening_capture_ids"]
        if (
            not isinstance(capture_ids, list)
            or len(capture_ids) != 2
            or len(set(capture_ids)) != 2
        ):
            raise BulkloadError(
                f"private SQLite session {role} opening capture IDs are invalid"
            )
        for capture_id in capture_ids:
            _uuid_hex(capture_id, f"private SQLite session {role} capture ID")
        snapshot_sha256s = role_opening["opening_snapshot_sha256s"]
        if (
            not isinstance(snapshot_sha256s, list)
            or len(snapshot_sha256s) != 2
            or len(set(snapshot_sha256s)) != 2
        ):
            raise BulkloadError(
                f"private SQLite session {role} opening snapshot digests are invalid"
            )
        for digest in snapshot_sha256s:
            _require_sha256(
                digest,
                f"private SQLite session {role} opening snapshot digest",
            )
        if _session_custody_from_opening(projection, capture_ids) != session[role]:
            raise BulkloadError(
                f"private SQLite session {role} opening custody differs"
            )
        private_binding = private_opening[role]["binding"]
        if projection["host_authority_id"] != private_binding["host_authority_id"]:
            raise BulkloadError(
                "private SQLite session and private host authorities are cross-wired"
            )

    epoch = _require_exact_keys(
        value["writer_stop_epoch"],
        {
            "epoch_id",
            "stopped_at",
            "operator_acknowledged_writers_stopped",
            "provider_writer_proof",
        },
        "private SQLite writer-stop epoch",
    )
    epoch_hex = _uuid_hex(
        _require_uuid(epoch["epoch_id"], "writer-stop epoch ID"),
        "writer-stop epoch ID",
    )
    stopped = _parse_utc_seconds(
        epoch["stopped_at"],
        "writer-stop epoch timestamp",
    )
    epoch_age = (created - stopped).total_seconds()
    if (
        epoch["operator_acknowledged_writers_stopped"] is not True
        or epoch["provider_writer_proof"] is not False
        or epoch_age < 0
        or epoch_age > MAX_WRITER_STOP_EPOCH_AGE_SECONDS
    ):
        raise BulkloadError("private SQLite writer-stop epoch is invalid")
    if epoch_hex in private_ids | normalized_session_ids:
        raise BulkloadError("writer-stop epoch reuses an evidence identity")
    if value["required_closing"] != {
        "planes": ["private", "session"],
        "roles": ["source", "destination"],
        "passes_per_role": 2,
    }:
        raise BulkloadError("private SQLite required closing set differs")
    if len(canonical_bytes(value)) > MAX_PRIVATE_SQLITE_CLOSE_BYTES:
        raise BulkloadError("private SQLite close request exceeds its byte budget")
    _require_sha256(
        value["close_request_sha256"],
        "private SQLite close-request digest",
    )
    require_digest(value, "close_request_sha256")


def validate_codex_private_sqlite_close_request_against_openings(
    request: dict[str, Any],
    opening_plan: dict[str, Any],
    session_union_plan: dict[str, Any],
    session_source_a: dict[str, Any],
    session_source_b: dict[str, Any],
    session_destination_a: dict[str, Any],
    session_destination_b: dict[str, Any],
    runtime_authority: dict[str, Any],
) -> None:
    """Rebind a persisted request to the exact accepted opening bodies."""
    validate_codex_private_sqlite_close_request(request)
    validate_codex_private_sqlite_compose_plan(opening_plan)
    validate_codex_session_union_plan(session_union_plan)
    private_runtime.validate_private_runtime_authority(runtime_authority)
    expected_session_opening = {
        "source": _session_opening_binding(
            session_source_a,
            session_source_b,
            role="source",
            session_plan_custody=session_union_plan["source"],
        ),
        "destination": _session_opening_binding(
            session_destination_a,
            session_destination_b,
            role="destination",
            session_plan_custody=session_union_plan["destination"],
        ),
    }
    if (
        request["runtime_authority"] != runtime_authority
        or request["opening_plan"] != _opening_plan_binding(opening_plan)
        or request["private_opening"]
        != {
            role: _private_opening_binding(opening_plan["private_opening"][role])
            for role in ("source", "destination")
        }
        or request["session_union"] != _session_union_binding(session_union_plan)
        or request["session_opening"] != expected_session_opening
    ):
        raise BulkloadError(
            "private SQLite close request differs from its opening bodies"
        )


def validate_codex_private_sqlite_close_request_against_inputs(
    request: dict[str, Any],
    opening_plan: dict[str, Any],
    session_union_plan: dict[str, Any],
    session_source_a: dict[str, Any],
    session_source_b: dict[str, Any],
    session_destination_a: dict[str, Any],
    session_destination_b: dict[str, Any],
    runtime_authority: dict[str, Any],
    *,
    opening_compatibility_plan: dict[str, Any],
    opening_source_a_directory: Path,
    opening_source_b_directory: Path,
    opening_destination_a_directory: Path,
    opening_destination_b_directory: Path,
    opening_adapter_registry: dict[str, Any],
    opening_path_map: dict[str, Any],
    opening_session_prefix_request: dict[str, Any] | None = None,
    opening_session_source_prefix_a: dict[str, Any] | None = None,
    opening_session_source_prefix_b: dict[str, Any] | None = None,
    opening_session_destination_prefix_a: dict[str, Any] | None = None,
    opening_session_destination_prefix_b: dict[str, Any] | None = None,
    opening_session_close_request: dict[str, Any] | None = None,
    opening_session_source_close_a: dict[str, Any] | None = None,
    opening_session_source_close_b: dict[str, Any] | None = None,
    opening_session_destination_close_a: dict[str, Any] | None = None,
    opening_session_destination_close_b: dict[str, Any] | None = None,
) -> None:
    """Recompute every v4 input, then bind the close to that exact opening."""
    validate_codex_private_sqlite_compose_plan_against_inputs(
        opening_plan,
        opening_compatibility_plan,
        opening_source_a_directory,
        opening_source_b_directory,
        opening_destination_a_directory,
        opening_destination_b_directory,
        adapter_registry=opening_adapter_registry,
        path_map=opening_path_map,
        session_union_plan=session_union_plan,
        session_source_a=session_source_a,
        session_source_b=session_source_b,
        session_destination_a=session_destination_a,
        session_destination_b=session_destination_b,
        session_prefix_request=opening_session_prefix_request,
        session_source_prefix_a=opening_session_source_prefix_a,
        session_source_prefix_b=opening_session_source_prefix_b,
        session_destination_prefix_a=opening_session_destination_prefix_a,
        session_destination_prefix_b=opening_session_destination_prefix_b,
        session_close_request=opening_session_close_request,
        session_source_close_a=opening_session_source_close_a,
        session_source_close_b=opening_session_source_close_b,
        session_destination_close_a=opening_session_destination_close_a,
        session_destination_close_b=opening_session_destination_close_b,
    )
    validate_codex_private_sqlite_close_request_against_openings(
        request,
        opening_plan,
        session_union_plan,
        session_source_a,
        session_source_b,
        session_destination_a,
        session_destination_b,
        runtime_authority,
    )


def validate_codex_private_sqlite_private_reclose_capture(
    capture: dict[str, Any],
    close_request: dict[str, Any],
    *,
    role: str,
) -> dict[str, str]:
    """Bind one fresh private bundle manifest to the close request and epoch."""
    validate_codex_private_capture(capture)
    validate_codex_private_sqlite_close_request(close_request)
    if role not in {"source", "destination"} or capture["role"] != role:
        raise BulkloadError("private SQLite private re-close role differs")
    quiescence = capture["quiescence"]
    if (
        quiescence["purpose"] != "close"
        or quiescence["capture_role"] != role
        or quiescence["provider_writer_proof"] is not False
        or quiescence["accepted_inputs"]
        != {
            "plan_sha256": close_request["close_request_sha256"],
            "apply_receipt_sha256": None,
            "journal_sha256": None,
        }
    ):
        raise BulkloadError(
            "private SQLite private re-close quiescence binding differs"
        )
    expected_projection = close_request["private_opening"][role]["binding"][
        "stable_projection"
    ]
    if _stable_private_projection(capture) != expected_projection:
        raise BulkloadError(
            f"private SQLite private {role} re-close differs from opening"
        )
    captured_at = _parse_utc_seconds(
        capture["captured_at"],
        f"private SQLite private {role} re-close timestamp",
    )
    stopped_at = _parse_utc_seconds(
        close_request["writer_stop_epoch"]["stopped_at"],
        "private SQLite writer-stop epoch timestamp",
    )
    if captured_at < stopped_at:
        raise BulkloadError(
            "private SQLite private re-close predates the writer-stop epoch"
        )
    opening_ids = {
        _uuid_hex(item, "private SQLite opening evidence ID")
        for binding in close_request["private_opening"].values()
        for key in ("capture_ids", "quiescence_attestation_ids")
        for item in binding["binding"][key]
    }
    opening_ids |= {
        _uuid_hex(item, "private SQLite session evidence ID")
        for item in close_request["session_union"]["evidence_capture_ids"]
    }
    opening_ids.add(
        _uuid_hex(
            close_request["writer_stop_epoch"]["epoch_id"],
            "private SQLite writer-stop epoch ID",
        )
    )
    capture_id = _uuid_hex(
        capture["capture_id"],
        "private SQLite private re-close capture ID",
    )
    attestation_id = _uuid_hex(
        quiescence["attestation_id"],
        "private SQLite private re-close attestation ID",
    )
    if (
        capture_id == attestation_id
        or capture_id in opening_ids
        or attestation_id in opening_ids
    ):
        raise BulkloadError(
            "private SQLite private re-close reuses an evidence identity"
        )
    return {
        "capture_id": capture["capture_id"],
        "capture_sha256": capture["capture_sha256"],
        "quiescence_attestation_id": quiescence["attestation_id"],
        "quiescence_attestation_sha256": quiescence["attestation_sha256"],
    }


def validate_codex_private_sqlite_private_reclose_set(
    close_request: dict[str, Any],
    source_a_directory: Path,
    source_b_directory: Path,
    destination_a_directory: Path,
    destination_b_directory: Path,
) -> dict[str, list[dict[str, str]]]:
    """Validate private source/destination A/B bundles against one close."""
    validate_codex_private_sqlite_close_request(close_request)
    directories = {
        "source": (source_a_directory, source_b_directory),
        "destination": (destination_a_directory, destination_b_directory),
    }
    bindings: dict[str, list[dict[str, str]]] = {}
    all_ids: list[str] = []
    all_capture_digests: list[str] = []
    for role, role_directories in directories.items():
        role_bindings: list[dict[str, str]] = []
        for directory in role_directories:
            capture, _ = read_codex_private_bundle(directory, role)
            binding = validate_codex_private_sqlite_private_reclose_capture(
                capture,
                close_request,
                role=role,
            )
            role_bindings.append(binding)
            all_ids.extend(
                (
                    binding["capture_id"],
                    binding["quiescence_attestation_id"],
                )
            )
            all_capture_digests.append(binding["capture_sha256"])
        bindings[role] = role_bindings
    normalized_ids = [
        _uuid_hex(item, "private SQLite private re-close evidence ID")
        for item in all_ids
    ]
    if len(set(normalized_ids)) != 8 or len(set(all_capture_digests)) != 4:
        raise BulkloadError(
            "private SQLite private re-close passes are not globally distinct"
        )
    return bindings


def _snapshot_matches_session_opening(
    snapshot: dict[str, Any],
    request: dict[str, Any],
    *,
    role: str,
) -> None:
    validate_codex_session_snapshot(snapshot)
    expected = request["session_opening"][role]["stable_projection"]
    observed = _session_stable_projection(snapshot)
    if (
        snapshot["complete"] is not True
        or snapshot["writers_quiesced"] is not True
        or observed != expected
    ):
        raise BulkloadError(
            f"private SQLite session {role} re-close differs from opening custody"
        )
    known_ids = {
        _uuid_hex(item, "private SQLite session opening evidence ID")
        for item in request["session_union"]["evidence_capture_ids"]
    }
    private_ids = {
        _uuid_hex(item, "private SQLite private opening evidence ID")
        for private_binding in request["private_opening"].values()
        for key in ("capture_ids", "quiescence_attestation_ids")
        for item in private_binding["binding"][key]
    }
    capture_id = _uuid_hex(
        snapshot["capture_id"],
        "private SQLite session re-close capture ID",
    )
    epoch_id = _uuid_hex(
        request["writer_stop_epoch"]["epoch_id"],
        "private SQLite writer-stop epoch ID",
    )
    if capture_id in known_ids | private_ids | {epoch_id}:
        raise BulkloadError(
            "private SQLite session re-close reuses an evidence identity"
        )
    if _parse_utc_seconds(
        snapshot["captured_at"],
        "private SQLite session re-close capture timestamp",
    ) < _parse_utc_seconds(
        request["writer_stop_epoch"]["stopped_at"],
        "private SQLite writer-stop epoch timestamp",
    ):
        raise BulkloadError(
            "private SQLite session re-close predates the writer-stop epoch"
        )


def capture_codex_private_sqlite_session_reclose(
    root: Path,
    *,
    role: str,
    close_request: dict[str, Any],
    accept_close_request: str,
    writer_stop_epoch_id: str,
    acknowledge_writers_quiesced: bool,
) -> dict[str, Any]:
    """Capture a live session root directly; no snapshot wrapper input exists."""
    validate_codex_private_sqlite_close_request(close_request)
    if accept_close_request != close_request["close_request_sha256"]:
        raise BulkloadError("accepted private SQLite close-request digest differs")
    if role not in {"source", "destination"}:
        raise BulkloadError("private SQLite session re-close role is invalid")
    if writer_stop_epoch_id != close_request["writer_stop_epoch"]["epoch_id"]:
        raise BulkloadError("private SQLite writer-stop epoch acceptance differs")
    opening = close_request["session_opening"][role]["stable_projection"]
    root = root.expanduser()
    root_text = os.fspath(root)
    if (
        not root.is_absolute()
        or os.path.normpath(root_text) != root_text
        or root_text != opening["root"]
    ):
        raise BulkloadError(
            "private SQLite session re-close root differs from opening custody"
        )
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
    _snapshot_matches_session_opening(snapshot, close_request, role=role)
    value: dict[str, Any] = {
        "schema": PRIVATE_SQLITE_SESSION_RECLOSE_SCHEMA,
        "created_at": utc_now(),
        "close_request_sha256": close_request["close_request_sha256"],
        "writer_stop_epoch": deepcopy(close_request["writer_stop_epoch"]),
        "role": role,
        "opening_stable_projection_sha256": close_request["session_opening"][role][
            "stable_projection_sha256"
        ],
        "snapshot": snapshot,
    }
    value["reclose_capture_sha256"] = object_digest(
        value,
        "reclose_capture_sha256",
    )
    validate_codex_private_sqlite_session_reclose_capture(value)
    validate_codex_private_sqlite_session_reclose_capture_against_request(
        value,
        close_request,
        role=role,
    )
    return value


def validate_codex_private_sqlite_session_reclose_against_live(
    capture: dict[str, Any],
    root: Path,
    close_request: dict[str, Any],
    *,
    role: str,
    accept_close_request: str,
    writer_stop_epoch_id: str,
    acknowledge_writers_quiesced: bool,
) -> None:
    """Recapture after publication and require the same stable live catalog."""
    validate_codex_private_sqlite_session_reclose_capture_against_request(
        capture,
        close_request,
        role=role,
    )
    observed = capture_codex_private_sqlite_session_reclose(
        root,
        role=role,
        close_request=close_request,
        accept_close_request=accept_close_request,
        writer_stop_epoch_id=writer_stop_epoch_id,
        acknowledge_writers_quiesced=acknowledge_writers_quiesced,
    )
    if _session_stable_projection(observed["snapshot"]) != _session_stable_projection(
        capture["snapshot"]
    ):
        raise BulkloadError(
            "private SQLite session live root changed after re-close publication"
        )


def validate_codex_private_sqlite_session_reclose_capture(
    capture: dict[str, Any],
) -> None:
    """Structurally validate one directly captured session re-close wrapper."""
    value = _require_exact_keys(
        capture,
        {
            "schema",
            "created_at",
            "close_request_sha256",
            "writer_stop_epoch",
            "role",
            "opening_stable_projection_sha256",
            "snapshot",
            "reclose_capture_sha256",
        },
        "private SQLite session re-close capture",
    )
    if value["schema"] != PRIVATE_SQLITE_SESSION_RECLOSE_SCHEMA:
        raise BulkloadError("unsupported private SQLite session re-close schema")
    created = _parse_utc_seconds(
        value["created_at"],
        "private SQLite session re-close timestamp",
    )
    _require_sha256(
        value["close_request_sha256"],
        "private SQLite session re-close request digest",
    )
    _require_sha256(
        value["opening_stable_projection_sha256"],
        "private SQLite session opening projection digest",
    )
    epoch = _require_exact_keys(
        value["writer_stop_epoch"],
        {
            "epoch_id",
            "stopped_at",
            "operator_acknowledged_writers_stopped",
            "provider_writer_proof",
        },
        "private SQLite session re-close writer-stop epoch",
    )
    _require_uuid(epoch["epoch_id"], "private SQLite writer-stop epoch ID")
    stopped = _parse_utc_seconds(
        epoch["stopped_at"],
        "private SQLite writer-stop epoch timestamp",
    )
    if (
        epoch["operator_acknowledged_writers_stopped"] is not True
        or epoch["provider_writer_proof"] is not False
        or stopped > created
    ):
        raise BulkloadError(
            "private SQLite session re-close writer-stop epoch is invalid"
        )
    if value["role"] not in {"source", "destination"}:
        raise BulkloadError("private SQLite session re-close role is invalid")
    if not isinstance(value["snapshot"], dict):
        raise BulkloadError("private SQLite session re-close snapshot is invalid")
    validate_codex_session_snapshot(value["snapshot"])
    if value["snapshot"]["role"] != value["role"]:
        raise BulkloadError("private SQLite session re-close snapshot role differs")
    if len(canonical_bytes(value)) > MAX_PRIVATE_SQLITE_CLOSE_BYTES:
        raise BulkloadError(
            "private SQLite session re-close capture exceeds its byte budget"
        )
    _require_sha256(
        value["reclose_capture_sha256"],
        "private SQLite session re-close digest",
    )
    require_digest(value, "reclose_capture_sha256")


def validate_codex_private_sqlite_session_reclose_capture_against_request(
    capture: dict[str, Any],
    close_request: dict[str, Any],
    *,
    role: str,
) -> dict[str, Any]:
    """Bind one re-close wrapper to the request, epoch, role, and opening."""
    validate_codex_private_sqlite_session_reclose_capture(capture)
    validate_codex_private_sqlite_close_request(close_request)
    if role not in {"source", "destination"} or capture["role"] != role:
        raise BulkloadError("private SQLite session re-close requested role differs")
    if (
        capture["close_request_sha256"] != close_request["close_request_sha256"]
        or capture["writer_stop_epoch"] != close_request["writer_stop_epoch"]
        or capture["opening_stable_projection_sha256"]
        != close_request["session_opening"][role]["stable_projection_sha256"]
    ):
        raise BulkloadError(
            "private SQLite session re-close request or epoch binding differs"
        )
    _snapshot_matches_session_opening(
        capture["snapshot"],
        close_request,
        role=role,
    )
    return capture["snapshot"]


def validate_codex_private_sqlite_session_reclose_set(
    close_request: dict[str, Any],
    source_a: dict[str, Any],
    source_b: dict[str, Any],
    destination_a: dict[str, Any],
    destination_b: dict[str, Any],
) -> dict[str, list[dict[str, str]]]:
    """Validate fresh A/B session closure for both cross-plane roles."""
    validate_codex_private_sqlite_close_request(close_request)
    by_role = {
        "source": (source_a, source_b),
        "destination": (destination_a, destination_b),
    }
    snapshots: list[dict[str, Any]] = []
    bindings: dict[str, list[dict[str, str]]] = {}
    for role, captures in by_role.items():
        role_bindings: list[dict[str, str]] = []
        for capture in captures:
            snapshot = (
                validate_codex_private_sqlite_session_reclose_capture_against_request(
                    capture,
                    close_request,
                    role=role,
                )
            )
            snapshots.append(snapshot)
            role_bindings.append(
                {
                    "capture_id": snapshot["capture_id"],
                    "snapshot_sha256": snapshot["snapshot_sha256"],
                    "reclose_capture_sha256": capture["reclose_capture_sha256"],
                }
            )
        bindings[role] = role_bindings
    capture_ids = [snapshot["capture_id"] for snapshot in snapshots]
    snapshot_digests = [snapshot["snapshot_sha256"] for snapshot in snapshots]
    wrapper_digests = [
        capture["reclose_capture_sha256"]
        for captures in by_role.values()
        for capture in captures
    ]
    if (
        len(set(capture_ids)) != 4
        or len(set(snapshot_digests)) != 4
        or len(set(wrapper_digests)) != 4
    ):
        raise BulkloadError(
            "private SQLite session re-close passes are not globally distinct"
        )
    for role, captures in by_role.items():
        first = captures[0]["snapshot"]
        second = captures[1]["snapshot"]
        if _session_stable_projection(first) != _session_stable_projection(second):
            raise BulkloadError(f"private SQLite session {role} re-close passes differ")
    return bindings
