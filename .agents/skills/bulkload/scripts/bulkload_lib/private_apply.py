"""Fail-closed private Codex auth install with SQLite preservation proofs."""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import secrets
import stat
from typing import Any
import uuid

from .model import BulkloadError, canonical_bytes, object_digest, sha256_bytes, utc_now
from . import private_quiescence
from . import private_runtime
from . import private_state


PRIVATE_INSTALL_PLAN_SCHEMA = "dev.tinyland.bulkload.codex-private-install-plan.v3"
PRIVATE_APPLY_RECEIPT_SCHEMA = "dev.tinyland.bulkload.codex-private-apply-receipt.v3"
PRIVATE_VERIFY_RECEIPT_SCHEMA = "dev.tinyland.bulkload.codex-private-verify-receipt.v3"
PRIVATE_ROLLBACK_RECEIPT_SCHEMA = (
    "dev.tinyland.bulkload.codex-private-rollback-receipt.v3"
)
PRIVATE_RECOVERY_RECEIPT_SCHEMA = (
    "dev.tinyland.bulkload.codex-private-recovery-receipt.v3"
)
PRIVATE_JOURNAL_SCHEMA = "dev.tinyland.bulkload.codex-private-journal.v3"
MAX_PRIVATE_RECEIPT_BYTES = 8 * 1024 * 1024
INSTALL_IMPLEMENTATION = private_runtime.PRIVATE_INSTALL_IMPLEMENTATION

_JOURNAL_CHAIN_FIELDS = {
    "event",
    "recorded_at",
    "sequence",
    "previous_event_sha256",
    "event_sha256",
}
_JOURNAL_PREPARED_COMMON_FIELDS = _JOURNAL_CHAIN_FIELDS | {
    "schema",
    "operation",
    "quiescence_id",
    "quiescence_attestation",
    "bulkload_lock",
    "plan_sha256",
    "compatibility_plan_sha256",
    "destination_host_authority_id",
    "destination_codex_home",
    "destination_codex_home_identity",
    "destination_sqlite_home",
    "destination_sqlite_home_identity",
    "selected_state_classes",
    "codex_version",
    "install_plan_path",
    "compatibility_plan_path",
    "source_directory",
    "destination_before_directory",
    "post_capture_directory",
    "recovery_capture_directory",
    "journal_path",
    "receipt_path",
    "receipt_staging_leaf",
    "output_parent_authorities",
    "auth_before_sha256",
    "auth_before_size",
    "auth_after_sha256",
    "auth_after_size",
    "sqlite_mutations",
}
_JOURNAL_APPLY_PREPARED_FIELDS = _JOURNAL_PREPARED_COMMON_FIELDS | {
    "run_id",
    "rollback_directory",
    "rollback_capture_id",
    "rollback_capture_sha256",
    "auth_install_staging_leaf",
}
_JOURNAL_ROLLBACK_PREPARED_FIELDS = _JOURNAL_PREPARED_COMMON_FIELDS | {
    "rollback_id",
    "apply_receipt_sha256",
    "apply_receipt_path",
    "rollback_source_directory",
    "rollback_preflight_directory",
    "rollback_preflight_capture_id",
    "rollback_preflight_capture_sha256",
    "auth_staging_leaf",
}
_JOURNAL_EVENT_FIELDS = {
    "mutation-intent": _JOURNAL_CHAIN_FIELDS
    | {
        "run_id",
        "staging_leaf",
        "staging_sha256",
        "staging_size",
        "staging_identity",
    },
    "auth-installed": _JOURNAL_CHAIN_FIELDS
    | {"run_id", "auth_sha256", "auth_identity"},
    "no-live-mutation": _JOURNAL_CHAIN_FIELDS | {"run_id"},
    "offline-selected-state-verified": _JOURNAL_CHAIN_FIELDS
    | {
        "run_id",
        "post_capture_id",
        "post_capture_sha256",
        "provider_runtime_acceptance_verified",
    },
    "receipt-frozen": _JOURNAL_CHAIN_FIELDS
    | {"run_id", "receipt_path", "receipt_staging_leaf"},
    "automatic-rollback-staging": _JOURNAL_CHAIN_FIELDS
    | {"run_id", "staging_leaf", "staging_sha256", "staging_size"},
    "automatic-rollback-mutation-intent": _JOURNAL_CHAIN_FIELDS
    | {
        "run_id",
        "staging_leaf",
        "staging_sha256",
        "staging_size",
        "staging_identity",
    },
    "automatic-rollback-verified": _JOURNAL_CHAIN_FIELDS
    | {"run_id", "recovery_capture_id", "recovery_capture_sha256"},
    "rollback-mutation-intent": _JOURNAL_CHAIN_FIELDS
    | {
        "rollback_id",
        "staging_leaf",
        "staging_sha256",
        "staging_size",
        "staging_identity",
    },
    "auth-restored": _JOURNAL_CHAIN_FIELDS | {"rollback_id", "auth_sha256"},
    "rollback-offline-state-verified": _JOURNAL_CHAIN_FIELDS
    | {
        "rollback_id",
        "post_capture_id",
        "post_capture_sha256",
        "provider_runtime_acceptance_verified",
    },
    "rollback-receipt-frozen": _JOURNAL_CHAIN_FIELDS
    | {"rollback_id", "receipt_path", "receipt_staging_leaf"},
    "automatic-reinstall-staging": _JOURNAL_CHAIN_FIELDS
    | {"rollback_id", "staging_leaf", "staging_sha256", "staging_size"},
    "automatic-reinstall-mutation-intent": _JOURNAL_CHAIN_FIELDS
    | {
        "rollback_id",
        "staging_leaf",
        "staging_sha256",
        "staging_size",
        "staging_identity",
    },
    "automatic-reinstall-verified": _JOURNAL_CHAIN_FIELDS
    | {"rollback_id", "recovery_capture_id", "recovery_capture_sha256"},
    "recovery-prepared": _JOURNAL_CHAIN_FIELDS
    | {
        "operation",
        "operation_id",
        "recovery_id",
        "quiescence_id",
        "quiescence_attestation",
        "bulkload_lock",
        "plan_sha256",
        "apply_receipt_sha256",
        "accepted_journal_sha256",
        "preflight_capture_directory",
        "post_capture_directory",
        "receipt_path",
        "auth_staging_leaf",
        "receipt_staging_leaf",
        "observed_state",
        "output_parent_authorities",
    },
    "recovery-mutation-intent": _JOURNAL_CHAIN_FIELDS
    | {
        "operation",
        "operation_id",
        "recovery_id",
        "staging_leaf",
        "staging_sha256",
        "staging_size",
        "staging_identity",
    },
    "recovery-restored-before": _JOURNAL_CHAIN_FIELDS
    | {
        "operation",
        "operation_id",
        "recovery_id",
        "auth_sha256",
        "auth_identity",
    },
    "recovery-confirmed-before": _JOURNAL_CHAIN_FIELDS
    | {
        "operation",
        "operation_id",
        "recovery_id",
        "auth_sha256",
        "auth_identity",
    },
    "recovery-staging-cleaned": _JOURNAL_CHAIN_FIELDS
    | {
        "operation",
        "operation_id",
        "recovery_id",
        "cleaned_staging_leaves",
    },
    "recovery-offline-state-verified": _JOURNAL_CHAIN_FIELDS
    | {
        "operation",
        "operation_id",
        "recovery_id",
        "post_capture_id",
        "post_capture_sha256",
        "provider_runtime_acceptance_verified",
    },
    "recovery-receipt-frozen": _JOURNAL_CHAIN_FIELDS
    | {
        "operation",
        "operation_id",
        "recovery_id",
        "receipt_path",
        "receipt_staging_leaf",
    },
    "journal-tail-quarantined": _JOURNAL_CHAIN_FIELDS
    | {
        "operation",
        "operation_id",
        "accepted_journal_sha256",
        "complete_prefix_sha256",
        "complete_prefix_size",
        "tail_sha256",
        "tail_size",
        "quarantine_leaf",
    },
}


def _require_mapping(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise BulkloadError(f"{label} must be an object")
    return value


def _require_exact_keys(
    value: dict[str, Any],
    expected: set[str],
    label: str,
) -> None:
    if set(value) != expected:
        raise BulkloadError(f"{label} fields are not exact")


def _require_sha256(value: Any, label: str) -> str:
    if (
        not isinstance(value, str)
        or len(value) != 64
        or any(character not in "0123456789abcdef" for character in value)
    ):
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


def _require_directory_identity(value: Any, label: str) -> dict[str, int]:
    identity = _require_mapping(value, label)
    _require_exact_keys(
        identity,
        {"device", "inode", "uid", "mode"},
        label,
    )
    if not all(isinstance(item, int) and item >= 0 for item in identity.values()):
        raise BulkloadError(f"{label} is invalid")
    return identity


def _require_file_identity(value: Any, label: str) -> dict[str, int]:
    identity = _require_mapping(value, label)
    _require_exact_keys(
        identity,
        {"device", "inode", "uid", "mode", "links"},
        label,
    )
    if not all(isinstance(item, int) and item >= 0 for item in identity.values()):
        raise BulkloadError(f"{label} is invalid")
    return identity


def _validate_bulkload_lock_record(value: Any) -> dict[str, Any]:
    record = _require_mapping(value, "private bulkload lock record")
    _require_exact_keys(
        record,
        {
            "scope",
            "method",
            "provider_writer_proof",
            "roots",
            "scope_sha256",
        },
        "private bulkload lock record",
    )
    roots = record["roots"]
    if not isinstance(roots, list) or not roots:
        raise BulkloadError("private bulkload lock roots are invalid")
    expected_payload = private_quiescence._lock_scope_payload_from_records(roots)
    if (
        record["scope"] != private_quiescence.PRIVATE_BULKLOAD_LOCK_SCOPE
        or record["method"] != private_quiescence.PRIVATE_BULKLOAD_LOCK_METHOD
        or record["provider_writer_proof"] is not False
        or record["roots"] != expected_payload["roots"]
    ):
        raise BulkloadError("private bulkload lock record differs")
    _require_sha256(
        record["scope_sha256"],
        "private bulkload lock scope digest",
    )
    if sha256_bytes(canonical_bytes(expected_payload)) != record["scope_sha256"]:
        raise BulkloadError("private bulkload lock scope digest differs")
    return record


def _validate_quiescence_binding(
    value: Any,
    lock_value: Any,
    *,
    expected_purpose: str,
    expected_plan_sha256: str,
    expected_apply_receipt_sha256: str | None,
    expected_journal_sha256: str | None,
    expected_host_authority_id: str,
    expected_codex_version: str,
    expected_selected_state_classes: list[str],
    expected_codex_home: dict[str, Any],
    expected_sqlite_home: dict[str, Any] | None,
    expected_operation_output_path: str,
) -> dict[str, Any]:
    binding = _require_mapping(
        value,
        "private quiescence attestation binding",
    )
    _require_exact_keys(
        binding,
        {"path", "attestation"},
        "private quiescence attestation binding",
    )
    if not isinstance(binding["path"], str) or not Path(binding["path"]).is_absolute():
        raise BulkloadError("private quiescence attestation path is invalid")
    attestation = _require_mapping(
        binding["attestation"],
        "private quiescence attestation",
    )
    private_quiescence.validate_codex_private_quiescence_attestation(attestation)
    if (
        attestation["purpose"] != expected_purpose
        or attestation["capture_role"] is not None
        or attestation["host_authority_id"] != expected_host_authority_id
        or attestation["codex_version"] != expected_codex_version
        or attestation["selected_state_classes"] != expected_selected_state_classes
        or attestation["codex_home"] != expected_codex_home
        or attestation["sqlite_home"] != expected_sqlite_home
        or attestation["operation_output"]["path"] != expected_operation_output_path
        or attestation["accepted_inputs"]
        != {
            "plan_sha256": expected_plan_sha256,
            "apply_receipt_sha256": expected_apply_receipt_sha256,
            "journal_sha256": expected_journal_sha256,
        }
        or attestation["claim"] != private_quiescence.QUIESCENCE_CLAIM
        or attestation["provider_writer_proof"] is not False
    ):
        raise BulkloadError("private quiescence attestation claims differ")
    lock_record = _validate_bulkload_lock_record(lock_value)
    if attestation["bulkload_lock_scope_sha256"] != lock_record["scope_sha256"]:
        raise BulkloadError("private quiescence and bulkload lock scopes differ")
    return binding


def _destination_quiescence_expectations(
    value: Any,
    *,
    expected_host_authority_id: str,
    expected_codex_version: str,
    expected_selected_state_classes: list[str],
) -> tuple[dict[str, Any], dict[str, Any] | None]:
    binding = _require_mapping(value, "private destination binding")
    _require_exact_keys(
        binding,
        {
            "host",
            "host_authority_id",
            "codex_version",
            "codex_home",
            "sqlite_home",
        },
        "private destination binding",
    )
    if (
        not isinstance(binding["host"], str)
        or not binding["host"]
        or binding["host_authority_id"] != expected_host_authority_id
        or binding["codex_version"] != expected_codex_version
    ):
        raise BulkloadError("private destination binding differs")
    selected = _canonical_state_classes(expected_selected_state_classes)
    if selected != expected_selected_state_classes:
        raise BulkloadError("private destination state classes are not canonical")
    codex_home = _require_mapping(
        binding["codex_home"],
        "private destination Codex home",
    )
    _require_exact_keys(
        codex_home,
        {"resolved_path", "identity"},
        "private destination Codex home",
    )
    if (
        not isinstance(codex_home["resolved_path"], str)
        or not Path(codex_home["resolved_path"]).is_absolute()
    ):
        raise BulkloadError("private destination Codex home path is invalid")
    _require_directory_identity(
        codex_home["identity"],
        "private destination Codex home identity",
    )
    sqlite_home: dict[str, Any] | None = None
    if "sqlite" in selected:
        sqlite_binding = _require_mapping(
            binding["sqlite_home"],
            "private destination SQLite home",
        )
        _require_exact_keys(
            sqlite_binding,
            {"resolved_path", "identity", "authority_source"},
            "private destination SQLite home",
        )
        if (
            not isinstance(sqlite_binding["resolved_path"], str)
            or not Path(sqlite_binding["resolved_path"]).is_absolute()
            or sqlite_binding["authority_source"] != "explicit"
        ):
            raise BulkloadError("private destination SQLite home is invalid")
        _require_directory_identity(
            sqlite_binding["identity"],
            "private destination SQLite home identity",
        )
        sqlite_home = {
            "resolved_path": sqlite_binding["resolved_path"],
            "identity": sqlite_binding["identity"],
        }
    elif binding["sqlite_home"] is not None:
        raise BulkloadError("private destination SQLite binding is inconsistent")
    return codex_home, sqlite_home


def _validate_receipt_output_binding(
    receipt_path: Any,
    quiescence: dict[str, Any],
    *,
    expected_receipt_path: Path | None,
) -> str:
    if (
        not isinstance(receipt_path, str)
        or not Path(receipt_path).is_absolute()
        or quiescence["attestation"]["operation_output"]["path"] != receipt_path
    ):
        raise BulkloadError("private receipt output binding differs")
    if expected_receipt_path is None:
        return receipt_path
    requested = expected_receipt_path.expanduser()
    if requested.name in {"", ".", ".."}:
        raise BulkloadError("private receipt path requires a leaf name")
    parent, parent_info = private_state._resolve_private_directory(
        requested.parent,
        "private receipt parent",
    )
    expected_output = {
        "path": os.fspath(parent / requested.name),
        "parent_resolved_path": os.fspath(parent),
        "parent_identity": private_state._directory_identity_record(parent_info),
        "leaf": requested.name,
    }
    if (
        receipt_path != expected_output["path"]
        or quiescence["attestation"]["operation_output"] != expected_output
    ):
        raise BulkloadError("private receipt path differs from its actual authority")
    return receipt_path


def _validate_quiescence_output_authority(
    quiescence: dict[str, Any],
    receipt_path: str,
    authority_value: Any,
    *,
    label: str,
) -> None:
    authority = _require_mapping(
        authority_value,
        f"{label} output authority",
    )
    _require_exact_keys(
        authority,
        {"resolved_path", "identity"},
        f"{label} output authority",
    )
    identity = _require_directory_identity(
        authority["identity"],
        f"{label} output parent identity",
    )
    if (
        not isinstance(receipt_path, str)
        or not Path(receipt_path).is_absolute()
        or not isinstance(authority["resolved_path"], str)
        or not Path(authority["resolved_path"]).is_absolute()
    ):
        raise BulkloadError(f"{label} output path is invalid")
    path = Path(receipt_path)
    expected = {
        "path": receipt_path,
        "parent_resolved_path": authority["resolved_path"],
        "parent_identity": identity,
        "leaf": path.name,
    }
    if (
        path.parent != Path(authority["resolved_path"])
        or quiescence["attestation"]["operation_output"] != expected
    ):
        raise BulkloadError(f"{label} quiescence output authority differs")


def _open_operation_quiescence(
    attestation_path: Path,
    *,
    accept_attestation: str,
    purpose: str,
    plan: dict[str, Any],
    codex_home: Path,
    sqlite_home: Path | None,
    operation_output: Path,
    apply_receipt_sha256: str | None = None,
    journal_sha256: str | None = None,
) -> tuple[
    private_runtime.PinnedPrivateRuntimeAuthority,
    private_quiescence.CodexPrivateBulkloadLock,
    private_quiescence.PinnedCodexPrivateQuiescenceAttestation,
]:
    runtime = private_runtime.open_pinned_private_runtime_authority(
        plan["runtime_authority"]
    )
    try:
        lock = private_quiescence.acquire_codex_private_bulkload_lock(
            codex_home,
            sqlite_home,
        )
    except BaseException:
        runtime.close()
        raise
    try:
        attestation = private_quiescence.open_codex_private_quiescence_attestation(
            attestation_path,
            accept_attestation=accept_attestation,
            expected_purpose=purpose,
            expected_host_authority_id=plan["destination_host_authority_id"],
            expected_codex_version=plan["codex_version"],
            expected_selected_state_classes=plan["selected_state_classes"],
            expected_codex_home=codex_home,
            expected_sqlite_home=sqlite_home,
            expected_operation_output=operation_output,
            expected_plan_sha256=plan["plan_sha256"],
            expected_apply_receipt_sha256=apply_receipt_sha256,
            expected_journal_sha256=journal_sha256,
        )
        attestation.assert_bulkload_lock(lock)
        runtime.revalidate()
        return runtime, lock, attestation
    except BaseException:
        lock.close()
        runtime.close()
        raise


def _operation_quiescence_records(
    attestation_path: Path,
    attestation: private_quiescence.PinnedCodexPrivateQuiescenceAttestation,
    lock: private_quiescence.CodexPrivateBulkloadLock,
) -> tuple[dict[str, Any], dict[str, Any]]:
    requested = attestation_path.expanduser()
    resolved_path = requested.parent.resolve() / requested.name
    return (
        {
            "path": os.fspath(resolved_path),
            "attestation": attestation.value,
        },
        lock.record,
    )


def _canonical_state_classes(value: Any) -> list[str]:
    if (
        not isinstance(value, list)
        or not value
        or not all(isinstance(item, str) for item in value)
        or value != sorted(set(value))
        or not set(value) <= {"auth", "sqlite"}
    ):
        raise BulkloadError("private install state classes are invalid")
    return value


def _canonical_blockers(value: Any) -> list[dict[str, Any]]:
    if not isinstance(value, list):
        raise BulkloadError("private install blockers must be a list")
    blockers: list[dict[str, Any]] = []
    encodings: list[bytes] = []
    for item_value in value:
        item = _require_mapping(item_value, "private install blocker")
        code = item.get("code")
        if code in {
            "destination-auth-required-for-rollback",
            "destination-sqlite-preservation-not-captured",
        }:
            _require_exact_keys(item, {"code"}, "private install blocker")
        elif code == "sqlite-content-diff-requires-versioned-composer":
            _require_exact_keys(
                item,
                {
                    "code",
                    "basename",
                    "strategy",
                    "source_sha256",
                    "destination_sha256",
                },
                "private install blocker",
            )
            if not private_state.SQLITE_BASENAME.fullmatch(item["basename"]):
                raise BulkloadError("private install blocker basename is invalid")
            _require_sha256(item["source_sha256"], "source SQLite digest")
            _require_sha256(
                item["destination_sha256"],
                "destination SQLite digest",
            )
        elif isinstance(code, str):
            private_state._validate_private_plan_blocker(item)
        else:
            raise BulkloadError("private install blocker is invalid")
        blockers.append(item)
        encodings.append(canonical_bytes(item))
    if len(encodings) != len(set(encodings)):
        raise BulkloadError("private install blockers contain duplicates")
    return blockers


def _capture_family_map(capture: dict[str, Any]) -> dict[str, dict[str, Any]]:
    return {item["basename"]: item for item in capture["sqlite_families"]}


def _compatibility_semantics(
    plan: dict[str, Any],
) -> dict[str, Any]:
    return {
        key: item
        for key, item in plan.items()
        if key not in {"created_at", "plan_sha256"}
    }


def compile_codex_private_install_plan(
    compatibility_plan: dict[str, Any],
    source_directory: Path,
    destination_directory: Path,
    *,
    accept_compatibility_plan: str,
    runtime_authority: dict[str, Any],
) -> dict[str, Any]:
    """Compile a narrow auth installer plan; never mutate live state."""
    private_state.validate_codex_private_state_plan(compatibility_plan)
    if compatibility_plan["plan_sha256"] != accept_compatibility_plan:
        raise BulkloadError("accepted private compatibility plan digest differs")
    private_state.validate_codex_private_state_plan_against_bundles(
        compatibility_plan,
        source_directory,
        destination_directory,
    )
    private_runtime.validate_private_runtime_authority(runtime_authority)
    if compatibility_plan["runtime_authority"] != runtime_authority:
        raise BulkloadError(
            "private compatibility and install runtime authorities differ"
        )
    source, _ = private_state.read_codex_private_bundle(
        source_directory,
        "source",
    )
    destination, _ = private_state.read_codex_private_bundle(
        destination_directory,
        "destination",
    )
    source_selected = list(source["selected_state_classes"])
    destination_selected = list(destination["selected_state_classes"])
    input_matrix = (tuple(source_selected), tuple(destination_selected))
    supported_input_matrix = {
        (("auth",), ("auth", "sqlite")),
        (("auth", "sqlite"), ("auth", "sqlite")),
    }
    if input_matrix not in supported_input_matrix:
        raise BulkloadError("private auth install input-state matrix is unsupported")
    split_auth_source = source_selected == ["auth"] and destination_selected == [
        "auth",
        "sqlite",
    ]
    structural_blockers = {
        item["code"]
        for item in compatibility_plan["blockers"]
        if item["code"]
        in {
            "same-host-authority",
            "codex-version-mismatch",
        }
    }
    if (
        any(
            item["code"] == "state-class-selection-mismatch"
            for item in compatibility_plan["blockers"]
        )
        and not split_auth_source
    ):
        structural_blockers.add("state-class-selection-mismatch")
    if structural_blockers:
        raise BulkloadError(
            "private install compilation requires compatible topology: "
            + ", ".join(sorted(structural_blockers))
        )

    blockers: list[dict[str, Any]] = []
    destination_family_names = sorted(_capture_family_map(destination))
    for item in compatibility_plan["blockers"]:
        code = item["code"]
        if code == "auth-installer-not-implemented":
            continue
        if not split_auth_source and code in {
            "sqlite-composer-not-implemented",
            "sqlite-family-set-mismatch",
            "sqlite-schema-mismatch",
            "sqlite-header-mismatch",
        }:
            # The compatibility plan retains these source/destination SQLite
            # findings. This narrower installer consumes no source SQLite:
            # destination families are independently captured and preserved
            # exact, while sqlite_union_ready remains false. Discharge only the
            # four validated compatibility codes named here.
            continue
        if split_auth_source and code == "state-class-selection-mismatch":
            if item != {
                "code": "state-class-selection-mismatch",
                "source": ["auth"],
                "destination": ["auth", "sqlite"],
            }:
                raise BulkloadError("private auth split selection blocker differs")
            continue
        if split_auth_source and code == "sqlite-family-set-mismatch":
            if item != {
                "code": "sqlite-family-set-mismatch",
                "missing_from_source": destination_family_names,
                "missing_from_destination": [],
            }:
                raise BulkloadError("private auth split SQLite blocker differs")
            continue
        blockers.append(dict(item))
    selected = destination_selected

    source_auth = source["auth"]
    destination_auth = destination["auth"]
    if "auth" not in selected:
        auth = {
            "action": "not-selected",
            "mutates": False,
            "executor_implemented": True,
        }
    elif source_auth is None or destination_auth is None:
        auth = {
            "action": "blocked",
            "mutates": False,
            "executor_implemented": False,
        }
        blockers.append({"code": "destination-auth-required-for-rollback"})
    elif source_auth["sha256"] == destination_auth["sha256"]:
        auth = {
            "action": "preserve-identical",
            "source_sha256": source_auth["sha256"],
            "destination_sha256": destination_auth["sha256"],
            "source_size": source_auth["size"],
            "destination_size": destination_auth["size"],
            "mutates": False,
            "executor_implemented": True,
        }
    else:
        auth = {
            "action": "replace-existing-after-fresh-full-backup",
            "source_sha256": source_auth["sha256"],
            "destination_sha256": destination_auth["sha256"],
            "source_size": source_auth["size"],
            "destination_size": destination_auth["size"],
            "mutates": True,
            "executor_implemented": True,
        }

    destination_families = _capture_family_map(destination)
    family_plans: list[dict[str, Any]] = []
    for basename in sorted(destination_families):
        destination_family = destination_families[basename]
        family_plans.append(
            {
                "basename": basename,
                "action": "preserve-destination-exact",
                "destination_sha256": destination_family["sha256"],
                "destination_live_sha256": destination_family["source_sha256"],
                "destination_size": destination_family["snapshot_size"],
                "destination_schema_sha256": destination_family["schema_sha256"],
                "destination_migrations_sha256": destination_family[
                    "migrations_sha256"
                ],
                "destination_user_version": destination_family["user_version"],
                "destination_application_id": destination_family["application_id"],
                "mutates": False,
                "executor_implemented": True,
            }
        )

    if auth["mutates"] and ("sqlite" not in selected or not destination_families):
        blockers.append({"code": "destination-sqlite-preservation-not-captured"})

    # This slice never composes source SQLite. A typed auth-only source capture
    # may be composed with an independently attested destination auth+SQLite
    # capture. The complete destination family catalog is then held exact before
    # and after auth replacement. Cross-host SQLite remains a later no-loss lane.
    blockers = _canonical_blockers(blockers)
    ready = not blockers
    intent = (
        "replace-auth-preserve-destination-sqlite-exact"
        if auth["mutates"]
        else "verification-only"
    )
    plan: dict[str, Any] = {
        "schema": PRIVATE_INSTALL_PLAN_SCHEMA,
        "created_at": utc_now(),
        "runtime_authority": json.loads(json.dumps(runtime_authority)),
        "input_state_classes": {
            "source": source_selected,
            "destination": destination_selected,
        },
        "compatibility_plan_sha256": compatibility_plan["plan_sha256"],
        "source_capture_id": source["capture_id"],
        "destination_before_capture_id": destination["capture_id"],
        "source_capture_sha256": source["capture_sha256"],
        "destination_before_capture_sha256": destination["capture_sha256"],
        "destination_sqlite_live_namespace_sha256": destination[
            "sqlite_live_namespace_sha256"
        ],
        "source_host_authority_id": source["host_authority_id"],
        "destination_host_authority_id": destination["host_authority_id"],
        "codex_version": source["codex_version"],
        "selected_state_classes": selected,
        "destination_binding": {
            "host": destination["host"],
            "host_authority_id": destination["host_authority_id"],
            "codex_version": destination["codex_version"],
            "codex_home": destination["codex_home"],
            "sqlite_home": destination["sqlite_home"],
        },
        "auth": auth,
        "sqlite_families": family_plans,
        "blockers": blockers,
        "intent": intent,
        "quiescence_required": True,
        "runtime_acceptance_required": True,
        "sqlite_union_ready": False,
        "ready_for_apply": ready,
        "implementation": INSTALL_IMPLEMENTATION,
    }
    plan["plan_sha256"] = object_digest(plan, "plan_sha256")
    validate_codex_private_install_plan(plan)
    return plan


def validate_codex_private_install_plan(value: dict[str, Any]) -> None:
    _require_exact_keys(
        value,
        {
            "schema",
            "created_at",
            "runtime_authority",
            "input_state_classes",
            "compatibility_plan_sha256",
            "source_capture_id",
            "destination_before_capture_id",
            "source_capture_sha256",
            "destination_before_capture_sha256",
            "destination_sqlite_live_namespace_sha256",
            "source_host_authority_id",
            "destination_host_authority_id",
            "codex_version",
            "selected_state_classes",
            "destination_binding",
            "auth",
            "sqlite_families",
            "blockers",
            "intent",
            "quiescence_required",
            "runtime_acceptance_required",
            "sqlite_union_ready",
            "ready_for_apply",
            "implementation",
            "plan_sha256",
        },
        "private install plan",
    )
    if value["schema"] != PRIVATE_INSTALL_PLAN_SCHEMA:
        raise BulkloadError("unsupported private install plan schema")
    if not isinstance(value["created_at"], str) or not value["created_at"]:
        raise BulkloadError("private install plan timestamp is invalid")
    private_runtime.validate_private_runtime_authority(value["runtime_authority"])
    input_state_classes = _require_mapping(
        value["input_state_classes"],
        "private install input state classes",
    )
    _require_exact_keys(
        input_state_classes,
        {"source", "destination"},
        "private install input state classes",
    )
    source_selected = _canonical_state_classes(input_state_classes["source"])
    destination_selected = _canonical_state_classes(input_state_classes["destination"])
    if (tuple(source_selected), tuple(destination_selected)) not in {
        (("auth",), ("auth", "sqlite")),
        (("auth", "sqlite"), ("auth", "sqlite")),
    }:
        raise BulkloadError("private install input-state matrix is unsupported")
    for key in (
        "compatibility_plan_sha256",
        "source_capture_sha256",
        "destination_before_capture_sha256",
        "plan_sha256",
    ):
        _require_sha256(value[key], f"private install plan {key}")
    if "sqlite" in value["selected_state_classes"]:
        _require_sha256(
            value["destination_sqlite_live_namespace_sha256"],
            "private install destination SQLite namespace digest",
        )
    elif value["destination_sqlite_live_namespace_sha256"] is not None:
        raise BulkloadError(
            "private install unselected SQLite namespace digest must be null"
        )
    for key in (
        "source_capture_id",
        "destination_before_capture_id",
        "source_host_authority_id",
        "destination_host_authority_id",
    ):
        _require_uuid(value[key], f"private install plan {key}")
    if value["source_capture_id"] == value["destination_before_capture_id"]:
        raise BulkloadError("private install capture IDs must be distinct")
    if value["source_host_authority_id"] == value["destination_host_authority_id"]:
        raise BulkloadError("private install host authorities must be distinct")
    if (
        not isinstance(value["codex_version"], str)
        or not value["codex_version"]
        or len(value["codex_version"].encode("utf-8")) > 128
    ):
        raise BulkloadError("private install Codex version is invalid")
    selected = _canonical_state_classes(value["selected_state_classes"])
    if selected != destination_selected:
        raise BulkloadError(
            "private install operation selection differs from destination input"
        )

    binding = _require_mapping(
        value["destination_binding"],
        "private install destination binding",
    )
    _require_exact_keys(
        binding,
        {
            "host",
            "host_authority_id",
            "codex_version",
            "codex_home",
            "sqlite_home",
        },
        "private install destination binding",
    )
    if not isinstance(binding["host"], str) or not binding["host"]:
        raise BulkloadError("private install destination host is invalid")
    if binding["host_authority_id"] != value["destination_host_authority_id"]:
        raise BulkloadError("private install destination authority is inconsistent")
    if binding["codex_version"] != value["codex_version"]:
        raise BulkloadError("private install destination version is inconsistent")
    codex_home = _require_mapping(
        binding["codex_home"],
        "private install Codex home",
    )
    _require_exact_keys(
        codex_home,
        {"resolved_path", "identity"},
        "private install Codex home",
    )
    if (
        not isinstance(codex_home["resolved_path"], str)
        or not Path(codex_home["resolved_path"]).is_absolute()
    ):
        raise BulkloadError("private install Codex home path is invalid")
    _require_directory_identity(
        codex_home["identity"],
        "private install Codex home identity",
    )
    if "sqlite" in selected:
        sqlite_home = _require_mapping(
            binding["sqlite_home"],
            "private install SQLite home",
        )
        _require_exact_keys(
            sqlite_home,
            {"resolved_path", "identity", "authority_source"},
            "private install SQLite home",
        )
        if (
            not isinstance(sqlite_home["resolved_path"], str)
            or not Path(sqlite_home["resolved_path"]).is_absolute()
            or sqlite_home["authority_source"] != "explicit"
        ):
            raise BulkloadError("private install SQLite home is invalid")
        _require_directory_identity(
            sqlite_home["identity"],
            "private install SQLite home identity",
        )
    elif binding["sqlite_home"] is not None:
        raise BulkloadError("private install SQLite binding is inconsistent")

    auth = _require_mapping(value["auth"], "private install auth")
    action = auth.get("action")
    if action in {"not-selected", "blocked"}:
        _require_exact_keys(
            auth,
            {"action", "mutates", "executor_implemented"},
            "private install auth",
        )
        if auth["mutates"] is not False:
            raise BulkloadError("private install blocked auth cannot mutate")
        if action == "not-selected" and auth["executor_implemented"] is not True:
            raise BulkloadError("private install auth readiness is invalid")
        if action == "blocked" and auth["executor_implemented"] is not False:
            raise BulkloadError("private install auth readiness is invalid")
    elif action in {
        "preserve-identical",
        "replace-existing-after-fresh-full-backup",
    }:
        _require_exact_keys(
            auth,
            {
                "action",
                "source_sha256",
                "destination_sha256",
                "source_size",
                "destination_size",
                "mutates",
                "executor_implemented",
            },
            "private install auth",
        )
        _require_sha256(auth["source_sha256"], "private install source auth digest")
        _require_sha256(
            auth["destination_sha256"],
            "private install destination auth digest",
        )
        if not all(
            isinstance(auth[key], int) and 0 < auth[key] <= private_state.MAX_AUTH_BYTES
            for key in ("source_size", "destination_size")
        ):
            raise BulkloadError("private install auth size is invalid")
        replacing = action == "replace-existing-after-fresh-full-backup"
        if (auth["source_sha256"] == auth["destination_sha256"]) is replacing:
            raise BulkloadError("private install auth action is inconsistent")
        if auth["mutates"] is not replacing or auth["executor_implemented"] is not True:
            raise BulkloadError("private install auth readiness is invalid")
    else:
        raise BulkloadError("private install auth action is invalid")
    if ("auth" in selected) != (action != "not-selected"):
        raise BulkloadError("private install auth selection is inconsistent")

    families = value["sqlite_families"]
    if not isinstance(families, list):
        raise BulkloadError("private install SQLite families must be a list")
    basenames: list[str] = []
    for item_value in families:
        item = _require_mapping(item_value, "private install SQLite family")
        _require_exact_keys(
            item,
            {
                "basename",
                "action",
                "destination_sha256",
                "destination_live_sha256",
                "destination_size",
                "destination_schema_sha256",
                "destination_migrations_sha256",
                "destination_user_version",
                "destination_application_id",
                "mutates",
                "executor_implemented",
            },
            "private install SQLite family",
        )
        basename = item["basename"]
        if not isinstance(basename, str) or not private_state.SQLITE_BASENAME.fullmatch(
            basename
        ):
            raise BulkloadError("private install SQLite basename is invalid")
        basenames.append(basename)
        for key in (
            "destination_sha256",
            "destination_live_sha256",
            "destination_schema_sha256",
            "destination_migrations_sha256",
        ):
            _require_sha256(item[key], f"private install SQLite {key}")
        if not all(
            isinstance(item[key], int) and item[key] >= 0
            for key in (
                "destination_size",
                "destination_user_version",
                "destination_application_id",
            )
        ):
            raise BulkloadError("private install SQLite metadata is invalid")
        if item["mutates"] is not False:
            raise BulkloadError("private install must not mutate SQLite")
        if item["action"] != "preserve-destination-exact":
            raise BulkloadError("private install SQLite action is invalid")
        if item["executor_implemented"] is not True:
            raise BulkloadError("private SQLite preservation is invalid")
    if basenames != sorted(set(basenames)):
        raise BulkloadError("private install SQLite families are not canonical")
    if ("sqlite" in selected) != bool(families):
        raise BulkloadError("private install SQLite selection is inconsistent")

    blockers = _canonical_blockers(value["blockers"])
    preservation_missing = bool(auth["mutates"]) and (
        "sqlite" not in selected or not families
    )
    preservation_blocked = any(
        item["code"] == "destination-sqlite-preservation-not-captured"
        for item in blockers
    )
    if preservation_missing != preservation_blocked:
        raise BulkloadError(
            "private install destination SQLite preservation blocker is inconsistent"
        )
    ready = not blockers
    if value["ready_for_apply"] is not ready:
        raise BulkloadError("private install readiness is inconsistent")
    if value["intent"] not in {
        "replace-auth-preserve-destination-sqlite-exact",
        "verification-only",
    }:
        raise BulkloadError("private install intent is invalid")
    if (
        value["intent"] == "replace-auth-preserve-destination-sqlite-exact"
    ) is not bool(auth["mutates"]):
        raise BulkloadError("private install intent does not match auth action")
    if (
        value["quiescence_required"] is not True
        or value["runtime_acceptance_required"] is not True
        or value["sqlite_union_ready"] is not False
        or value["implementation"] != INSTALL_IMPLEMENTATION
    ):
        raise BulkloadError("private install implementation contract is invalid")
    if object_digest(value, "plan_sha256") != value["plan_sha256"]:
        raise BulkloadError("private install plan digest mismatch")


def validate_codex_private_install_plan_against_inputs(
    value: dict[str, Any],
    compatibility_plan: dict[str, Any],
    source_directory: Path,
    destination_directory: Path,
) -> None:
    """Reject a digest-valid install plan not derived from the exact inputs."""
    validate_codex_private_install_plan(value)
    recomputed = compile_codex_private_install_plan(
        compatibility_plan,
        source_directory,
        destination_directory,
        accept_compatibility_plan=compatibility_plan["plan_sha256"],
        runtime_authority=value["runtime_authority"],
    )
    if _compatibility_semantics(value) != _compatibility_semantics(recomputed):
        raise BulkloadError("private install plan differs from exact input semantics")


def read_codex_private_install_plan(path: Path) -> dict[str, Any]:
    value = private_state.read_private_json_document(
        path,
        label="private install plan",
    )
    validate_codex_private_install_plan(value)
    return value


def _family_projection(item: dict[str, Any]) -> dict[str, Any]:
    return {
        "sha256": item["sha256"],
        "source_sha256": item["source_sha256"],
        "snapshot_size": item["snapshot_size"],
        "schema_sha256": item["schema_sha256"],
        "migrations_sha256": item["migrations_sha256"],
        "user_version": item["user_version"],
        "application_id": item["application_id"],
    }


def validate_destination_capture_for_install(
    capture: dict[str, Any],
    plan: dict[str, Any],
    *,
    expected_auth_sha256: str | None,
    forbidden_capture_ids: set[str] = frozenset(),
) -> None:
    """Bind a fresh destination capture to one accepted install plan."""
    private_state.validate_codex_private_capture(capture)
    if capture["role"] != "destination":
        raise BulkloadError("private install evidence has the wrong role")
    if capture["capture_id"] in forbidden_capture_ids:
        raise BulkloadError("private install evidence reused an older capture")
    if (
        capture["host_authority_id"] != plan["destination_host_authority_id"]
        or capture["codex_version"] != plan["codex_version"]
        or capture["selected_state_classes"] != plan["selected_state_classes"]
        or capture["codex_home"] != plan["destination_binding"]["codex_home"]
        or capture["sqlite_home"] != plan["destination_binding"]["sqlite_home"]
        or capture["sqlite_live_namespace_sha256"]
        != plan["destination_sqlite_live_namespace_sha256"]
    ):
        raise BulkloadError("fresh destination capture binding differs from plan")
    if expected_auth_sha256 is None:
        if capture["auth"] is not None:
            raise BulkloadError("fresh destination auth selection differs from plan")
    elif capture["auth"] is None or capture["auth"]["sha256"] != expected_auth_sha256:
        raise BulkloadError("fresh destination auth differs from expected state")

    expected_families = {item["basename"]: item for item in plan["sqlite_families"]}
    observed_families = _capture_family_map(capture)
    if set(observed_families) != set(expected_families):
        raise BulkloadError("fresh destination SQLite family set differs from plan")
    for basename, expected in expected_families.items():
        observed = observed_families[basename]
        expected_projection = {
            "sha256": expected["destination_sha256"],
            "source_sha256": expected["destination_live_sha256"],
            "snapshot_size": expected["destination_size"],
            "schema_sha256": expected["destination_schema_sha256"],
            "migrations_sha256": expected["destination_migrations_sha256"],
            "user_version": expected["destination_user_version"],
            "application_id": expected["destination_application_id"],
        }
        if _family_projection(observed) != expected_projection:
            raise BulkloadError(f"fresh destination SQLite family drifted: {basename}")


def _capture_destination(
    plan: dict[str, Any],
    destination_before: dict[str, Any],
    *,
    codex_home: Path,
    sqlite_home: Path | None,
    output_directory: Path,
    quiescence: dict[str, Any],
    protected_directories: tuple[Path, ...] = (),
    recorded_protected_directories: tuple[Path, ...] = (),
) -> dict[str, Any]:
    selected = set(plan["selected_state_classes"])
    budgets = destination_before["budgets"]
    return private_state.capture_codex_private_state(
        codex_home,
        output_directory,
        role="destination",
        host_authority_id=plan["destination_host_authority_id"],
        codex_version=plan["codex_version"],
        sqlite_home=sqlite_home,
        include_auth="auth" in selected,
        include_sqlite="sqlite" in selected,
        acknowledge_private_capture=True,
        quiescence=quiescence,
        max_sqlite_families=budgets["max_sqlite_families"],
        max_total_sqlite_bytes=budgets["max_total_sqlite_bytes"],
        backup_timeout_seconds=budgets["backup_timeout_seconds"],
        max_thread_entries=budgets["max_thread_entries"],
        max_thread_index_bytes=budgets["max_thread_index_bytes"],
        max_metadata_entries=budgets["max_metadata_entries"],
        max_metadata_bytes=budgets["max_metadata_bytes"],
        protected_directories=protected_directories,
        recorded_protected_directories=recorded_protected_directories,
    )


def _path_is_within(candidate: Path, root: Path) -> bool:
    try:
        candidate.relative_to(root)
        return True
    except ValueError:
        return False


def _resolved_output_target(path: Path, label: str) -> tuple[Path, Path]:
    requested = path.expanduser()
    if requested.name in {"", ".", ".."}:
        raise BulkloadError(f"{label} must have a leaf name")
    parent, _ = private_state._resolve_private_directory(
        requested.parent,
        f"{label} parent",
    )
    target = parent / requested.name
    if target.exists() or target.is_symlink():
        raise BulkloadError(f"{label} already exists")
    return target, parent


def _validate_output_topology(
    outputs: dict[str, Path],
    protected: dict[str, Path],
    *,
    recorded_protected: tuple[Path, ...] = (),
) -> dict[str, Path]:
    resolved_outputs = {
        label: _resolved_output_target(path, label)[0]
        for label, path in outputs.items()
    }
    resolved_protected: dict[str, Path] = {}
    protected_directory_identities: set[tuple[int, int]] = set()
    for label, path in protected.items():
        try:
            resolved = path.expanduser().resolve(strict=True)
            info = resolved.stat()
        except (OSError, RuntimeError, ValueError) as error:
            raise BulkloadError(f"cannot resolve protected {label}") from error
        resolved_protected[label] = resolved
        if stat.S_ISDIR(info.st_mode):
            protected_directory_identities.add((info.st_dev, info.st_ino))

    recorded_lexical: list[Path] = []
    for index, path in enumerate(recorded_protected):
        expanded = path.expanduser()
        if not expanded.is_absolute():
            raise BulkloadError("recorded protected root must be absolute")
        lexical = Path(os.path.abspath(expanded))
        recorded_lexical.append(lexical)
        if not lexical.exists() and not lexical.is_symlink():
            continue
        try:
            resolved, info = private_state._resolve_private_directory(
                lexical,
                f"recorded protected root {index}",
            )
        except BulkloadError as error:
            raise BulkloadError(
                "cannot safely resolve recorded protected root"
            ) from error
        resolved_protected[f"recorded root {index}"] = resolved
        protected_directory_identities.add((info.st_dev, info.st_ino))

    for output_label, output in resolved_outputs.items():
        for protected_label, root in resolved_protected.items():
            if (
                output == root
                or _path_is_within(output, root)
                or _path_is_within(root, output)
            ):
                raise BulkloadError(
                    f"{output_label} overlaps protected {protected_label}"
                )
        for root in recorded_lexical:
            if (
                output == root
                or _path_is_within(output, root)
                or _path_is_within(root, output)
            ):
                raise BulkloadError(
                    f"{output_label} overlaps a recorded protected root"
                )
        _, parent = _resolved_output_target(output, output_label)
        parent_info = parent.stat()
        parent_descriptor = private_state._open_private_directory_descriptor(
            parent,
            parent_info,
            f"{output_label} parent",
        )
        try:
            if (
                private_state._directory_identity_lineage(parent_descriptor)
                & protected_directory_identities
            ):
                raise BulkloadError(
                    f"{output_label} aliases or descends from a protected root"
                )
        finally:
            os.close(parent_descriptor)
    items = list(resolved_outputs.items())
    for index, (left_label, left) in enumerate(items):
        for right_label, right in items[index + 1 :]:
            if (
                left == right
                or _path_is_within(left, right)
                or _path_is_within(right, left)
            ):
                raise BulkloadError(
                    f"{left_label} overlaps private output {right_label}"
                )
    return resolved_outputs


def _recorded_live_roots(compatibility_plan: dict[str, Any]) -> tuple[Path, ...]:
    return tuple(
        Path(root)
        for role_roots in compatibility_plan["protected_live_roots"].values()
        for root in role_roots
    )


def _output_parent_authorities(
    outputs: dict[str, Path],
) -> dict[str, dict[str, Any]]:
    authorities: dict[str, dict[str, Any]] = {}
    for label, target in sorted(outputs.items()):
        _, parent = _resolved_output_target(target, label)
        info = parent.stat()
        authorities[label] = {
            "resolved_path": os.fspath(parent),
            "identity": {
                "device": info.st_dev,
                "inode": info.st_ino,
                "uid": info.st_uid,
                "mode": stat.S_IMODE(info.st_mode),
            },
        }
    return authorities


def _revalidate_output_parent_authorities(
    authorities: dict[str, dict[str, Any]],
) -> None:
    for label, authority in authorities.items():
        parent, info = private_state._resolve_private_directory(
            Path(authority["resolved_path"]),
            f"{label} parent",
        )
        observed_identity = {
            "device": info.st_dev,
            "inode": info.st_ino,
            "uid": info.st_uid,
            "mode": stat.S_IMODE(info.st_mode),
        }
        if (
            os.fspath(parent) != authority["resolved_path"]
            or observed_identity != authority["identity"]
        ):
            raise BulkloadError(f"{label} output-parent authority changed")


def _read_descriptor_payload(
    descriptor: int,
    expected: dict[str, Any],
    label: str,
) -> bytes:
    before = os.fstat(descriptor)
    expected_size = expected.get("size", expected.get("snapshot_size"))
    if (
        not stat.S_ISREG(before.st_mode)
        or before.st_uid != os.getuid()
        or stat.S_IMODE(before.st_mode) != 0o600
        or before.st_nlink != 1
        or before.st_size != expected_size
    ):
        raise BulkloadError(f"{label} custody is invalid")
    os.lseek(descriptor, 0, os.SEEK_SET)
    payload = b""
    while len(payload) <= expected_size:
        block = os.read(
            descriptor,
            min(65536, expected_size + 1 - len(payload)),
        )
        if not block:
            break
        payload += block
    after = os.fstat(descriptor)
    if (
        len(payload) != expected_size
        or private_state._stable_artifact_stat(after)
        != private_state._stable_artifact_stat(before)
        or sha256_bytes(payload) != expected["sha256"]
    ):
        raise BulkloadError(f"{label} changed")
    return payload


def _open_pinned_auth(
    root: Path,
    expected: dict[str, Any],
    label: str,
) -> tuple[int, int, os.stat_result, bytes]:
    resolved, root_info = private_state._resolve_private_directory(root, label)
    root_descriptor = private_state._open_private_directory_descriptor(
        resolved,
        root_info,
        label,
    )
    try:
        auth_descriptor = os.open(
            private_state.AUTH_BASENAME,
            os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0),
            dir_fd=root_descriptor,
        )
    except BaseException:
        os.close(root_descriptor)
        raise
    try:
        auth_info = os.fstat(auth_descriptor)
        entry = os.stat(
            private_state.AUTH_BASENAME,
            dir_fd=root_descriptor,
            follow_symlinks=False,
        )
        if private_state._stable_artifact_stat(entry) != (
            private_state._stable_artifact_stat(auth_info)
        ):
            raise BulkloadError(f"{label} auth binding changed")
        payload = _read_descriptor_payload(auth_descriptor, expected, f"{label} auth")
        private_state._revalidate_directory_binding(
            resolved,
            root_descriptor,
            root_info,
            label,
        )
        return root_descriptor, auth_descriptor, root_info, payload
    except BaseException:
        os.close(auth_descriptor)
        os.close(root_descriptor)
        raise


def _revalidate_pinned_auth(
    root: Path,
    root_descriptor: int,
    root_info: os.stat_result,
    auth_descriptor: int,
    expected: dict[str, Any],
    label: str,
) -> bytes:
    private_state._revalidate_directory_binding(
        root,
        root_descriptor,
        root_info,
        label,
    )
    entry = os.stat(
        private_state.AUTH_BASENAME,
        dir_fd=root_descriptor,
        follow_symlinks=False,
    )
    opened = os.fstat(auth_descriptor)
    if private_state._stable_artifact_stat(entry) != (
        private_state._stable_artifact_stat(opened)
    ):
        raise BulkloadError(f"{label} auth binding changed")
    return _read_descriptor_payload(auth_descriptor, expected, f"{label} auth")


def _live_auth_record(
    root_descriptor: int,
    label: str,
) -> tuple[dict[str, Any], bytes]:
    descriptor = os.open(
        private_state.AUTH_BASENAME,
        os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0),
        dir_fd=root_descriptor,
    )
    try:
        info = os.fstat(descriptor)
        if (
            not stat.S_ISREG(info.st_mode)
            or info.st_uid != os.getuid()
            or stat.S_IMODE(info.st_mode) != 0o600
            or info.st_nlink != 1
            or info.st_size < 1
            or info.st_size > private_state.MAX_AUTH_BYTES
        ):
            raise BulkloadError(f"{label} custody is invalid")
        expected = {
            "sha256": "",
            "size": info.st_size,
        }
        os.lseek(descriptor, 0, os.SEEK_SET)
        payload = b""
        while len(payload) <= info.st_size:
            block = os.read(
                descriptor,
                min(65536, info.st_size + 1 - len(payload)),
            )
            if not block:
                break
            payload += block
        after = os.fstat(descriptor)
        entry = os.stat(
            private_state.AUTH_BASENAME,
            dir_fd=root_descriptor,
            follow_symlinks=False,
        )
        if (
            len(payload) != info.st_size
            or private_state._stable_artifact_stat(after)
            != private_state._stable_artifact_stat(info)
            or private_state._stable_artifact_stat(entry)
            != private_state._stable_artifact_stat(info)
        ):
            raise BulkloadError(f"{label} changed while reading")
        private_state._strict_json_object(payload, label)
        expected["sha256"] = sha256_bytes(payload)
        expected["identity"] = private_state._identity(info)
        return expected, payload
    finally:
        os.close(descriptor)


def _assert_live_auth(
    root_descriptor: int,
    *,
    expected_sha256: str,
    expected_size: int,
    expected_identity: dict[str, int] | None,
    label: str,
) -> bytes:
    record, payload = _live_auth_record(root_descriptor, label)
    if (
        record["sha256"] != expected_sha256
        or record["size"] != expected_size
        or (expected_identity is not None and record["identity"] != expected_identity)
    ):
        raise BulkloadError(f"{label} differs from accepted state")
    return payload


def _stage_auth(
    destination_descriptor: int,
    payload: bytes,
    *,
    staging_name: str,
) -> None:
    private_state._write_exclusive_at(
        destination_descriptor,
        staging_name,
        payload,
    )
    private_state._verify_private_payload_at(
        destination_descriptor,
        staging_name,
        payload,
    )


def _auth_staging_name(run_id: str, label: str) -> str:
    return (
        f".{private_state.AUTH_BASENAME}.bulkload-{label}-"
        f"{run_id}-{secrets.token_hex(8)}"
    )


def _receipt_staging_name(operation_id: str) -> str:
    _require_uuid(operation_id, "private receipt operation ID")
    return f".bulkload-private-receipt-{operation_id}"


def _journal_tail_quarantine_name(accepted_sha256: str) -> str:
    _require_sha256(accepted_sha256, "accepted private journal digest")
    return f".bulkload-private-journal-tail-{accepted_sha256}"


def _journal_tail_repair_staging_name(accepted_sha256: str) -> str:
    _require_sha256(accepted_sha256, "accepted private journal digest")
    return f".bulkload-private-journal-repair-{accepted_sha256}"


def _commit_staged_auth(
    destination_descriptor: int,
    staging_name: str,
) -> None:
    os.replace(
        staging_name,
        private_state.AUTH_BASENAME,
        src_dir_fd=destination_descriptor,
        dst_dir_fd=destination_descriptor,
    )


def _publish_mutation_receipt(
    target: Path,
    value: dict[str, Any],
    *,
    staging_leaf: str,
    protected_directories: tuple[Path, ...],
    recorded_protected_directories: tuple[Path, ...],
    commit_state: dict[str, bool],
) -> None:
    """Publish a mutation receipt and expose the exact no-replace commit point."""
    if set(commit_state) != {"attempted", "published"} or any(commit_state.values()):
        raise BulkloadError("private receipt publication state is invalid")
    if (
        not staging_leaf
        or "/" in staging_leaf
        or staging_leaf in {".", "..", target.name}
    ):
        raise BulkloadError("private receipt staging leaf is invalid")
    resolved_target, parent = _resolved_output_target(
        target,
        "private mutation receipt",
    )
    payload = canonical_bytes(value) + b"\n"
    if len(payload) > MAX_PRIVATE_RECEIPT_BYTES:
        raise BulkloadError("private mutation receipt exceeds the byte budget")

    protected_identities: set[tuple[int, int]] = set()
    for protected in protected_directories:
        resolved, info = private_state._resolve_private_directory(
            protected,
            "private receipt protected directory",
        )
        protected_identities.add((info.st_dev, info.st_ino))
        if (
            resolved_target == resolved
            or _path_is_within(resolved_target, resolved)
            or _path_is_within(resolved, resolved_target)
        ):
            raise BulkloadError("private mutation receipt overlaps protected state")
    for protected in recorded_protected_directories:
        expanded = protected.expanduser()
        if not expanded.is_absolute():
            raise BulkloadError("recorded private root must be absolute")
        lexical = Path(os.path.abspath(expanded))
        if (
            resolved_target == lexical
            or _path_is_within(resolved_target, lexical)
            or _path_is_within(lexical, resolved_target)
        ):
            raise BulkloadError(
                "private mutation receipt overlaps a recorded live root"
            )
        if not lexical.exists() and not lexical.is_symlink():
            continue
        resolved, info = private_state._resolve_private_directory(
            lexical,
            "recorded private root",
        )
        protected_identities.add((info.st_dev, info.st_ino))
        if (
            resolved_target == resolved
            or _path_is_within(resolved_target, resolved)
            or _path_is_within(resolved, resolved_target)
        ):
            raise BulkloadError(
                "private mutation receipt overlaps a recorded live root"
            )

    parent_info = parent.stat()
    parent_descriptor = private_state._open_private_directory_descriptor(
        parent,
        parent_info,
        "private mutation receipt parent",
    )
    try:
        if (
            private_state._directory_identity_lineage(parent_descriptor)
            & protected_identities
        ):
            raise BulkloadError(
                "private mutation receipt aliases or descends from protected state"
            )
        if os.path.lexists(parent / staging_leaf) or os.path.lexists(
            parent / resolved_target.name
        ):
            raise BulkloadError("private mutation receipt output already exists")
        private_state._revalidate_directory_binding(
            parent,
            parent_descriptor,
            parent_info,
            "private mutation receipt parent",
        )
        private_state._write_exclusive_at(
            parent_descriptor,
            staging_leaf,
            payload,
        )
        private_state._verify_private_payload_at(
            parent_descriptor,
            staging_leaf,
            payload,
        )
        private_state._revalidate_directory_binding(
            parent,
            parent_descriptor,
            parent_info,
            "private mutation receipt parent",
        )
        commit_state["attempted"] = True
        private_state._rename_noreplace_at(
            parent_descriptor,
            staging_leaf,
            parent_descriptor,
            resolved_target.name,
        )
        commit_state["published"] = True
        private_state._fsync_directory_descriptor(parent_descriptor)
        private_state._verify_private_payload_at(
            parent_descriptor,
            resolved_target.name,
            payload,
        )
        private_state._revalidate_directory_binding(
            parent,
            parent_descriptor,
            parent_info,
            "private mutation receipt parent",
        )
    finally:
        os.close(parent_descriptor)


def _classify_mutation_receipt_publication(
    target: Path,
    value: dict[str, Any],
    expected_parent_authority: dict[str, Any],
) -> str:
    """Return exact, absent, or unknown after an attempted receipt commit."""
    requested = target.expanduser()
    try:
        parent, parent_info = private_state._resolve_private_directory(
            requested.parent,
            "private mutation receipt parent",
        )
        parent_descriptor = private_state._open_private_directory_descriptor(
            parent,
            parent_info,
            "private mutation receipt parent",
        )
    except (BulkloadError, OSError, RuntimeError, ValueError):
        return "unknown"
    try:
        observed_parent_identity = {
            "device": parent_info.st_dev,
            "inode": parent_info.st_ino,
            "uid": parent_info.st_uid,
            "mode": stat.S_IMODE(parent_info.st_mode),
        }
        if (
            os.fspath(parent) != expected_parent_authority["resolved_path"]
            or observed_parent_identity != expected_parent_authority["identity"]
        ):
            return "unknown"
        try:
            os.stat(
                requested.name,
                dir_fd=parent_descriptor,
                follow_symlinks=False,
            )
        except FileNotFoundError:
            return "absent"
        except OSError:
            return "unknown"
        expected_payload = canonical_bytes(value) + b"\n"
        try:
            private_state._verify_private_payload_at(
                parent_descriptor,
                requested.name,
                expected_payload,
            )
        except (BulkloadError, OSError):
            return "unknown"
        return "exact"
    finally:
        os.close(parent_descriptor)


def _write_all(descriptor: int, payload: bytes) -> None:
    view = memoryview(payload)
    while view:
        written = os.write(descriptor, view)
        if written <= 0:
            raise BulkloadError("private journal write made no progress")
        view = view[written:]


def _require_journal_absolute_path(value: Any, label: str) -> str:
    if not isinstance(value, str) or not Path(value).is_absolute():
        raise BulkloadError(f"{label} must be an absolute path")
    return value


def _require_journal_size(value: Any, label: str) -> int:
    if type(value) is not int or value < 1 or value > private_state.MAX_AUTH_BYTES:
        raise BulkloadError(f"{label} is invalid")
    return value


def _require_auth_staging_leaf(value: Any, label: str) -> str:
    if (
        not isinstance(value, str)
        or not value.startswith(f".{private_state.AUTH_BASENAME}.bulkload-")
        or "/" in value
        or value in {".", "..", private_state.AUTH_BASENAME}
    ):
        raise BulkloadError(f"{label} is invalid")
    return value


def _require_receipt_staging_leaf(
    value: Any,
    operation_id: str,
    label: str,
) -> str:
    expected = _receipt_staging_name(operation_id)
    if value != expected:
        raise BulkloadError(f"{label} differs from its operation ID")
    return expected


def _validate_journal_auth_pair(
    event: dict[str, Any],
    *,
    digest_key: str,
    size_key: str,
    label: str,
) -> None:
    digest = event[digest_key]
    size = event[size_key]
    if digest is None or size is None:
        if digest is not None or size is not None:
            raise BulkloadError(f"{label} is incomplete")
        return
    _require_sha256(digest, f"{label} digest")
    _require_journal_size(size, f"{label} size")


def _validate_private_journal_event_shape(
    event: dict[str, Any],
    *,
    prepared: dict[str, Any] | None,
) -> None:
    event_name = event.get("event")
    if not isinstance(event_name, str) or not event_name:
        raise BulkloadError("private journal event name is invalid")
    if not isinstance(event.get("recorded_at"), str) or not event["recorded_at"]:
        raise BulkloadError("private journal event timestamp is invalid")

    if prepared is None:
        operation = event.get("operation")
        if operation == "apply":
            expected = _JOURNAL_APPLY_PREPARED_FIELDS
            expected_event = "prepared"
            operation_id_key = "run_id"
            expected_labels = {
                "rollback capture",
                "post capture",
                "recovery capture",
                "apply journal",
                "apply receipt",
            }
        elif operation == "rollback":
            expected = _JOURNAL_ROLLBACK_PREPARED_FIELDS
            expected_event = "rollback-prepared"
            operation_id_key = "rollback_id"
            expected_labels = {
                "rollback preflight capture",
                "rollback post capture",
                "rollback recovery capture",
                "rollback journal",
                "rollback receipt",
            }
        else:
            raise BulkloadError("private journal prepared operation is invalid")
        _require_exact_keys(event, expected, "private journal prepared event")
        operation_id = _require_uuid(
            event[operation_id_key],
            "private journal operation ID",
        )
        _require_uuid(event["quiescence_id"], "private journal quiescence ID")
        _require_uuid(
            event["destination_host_authority_id"],
            "private journal destination authority ID",
        )
        for key in (
            "plan_sha256",
            "compatibility_plan_sha256",
            "event_sha256",
        ):
            _require_sha256(event[key], f"private journal {key}")
        if (
            event["schema"] != PRIVATE_JOURNAL_SCHEMA
            or event["event"] != expected_event
            or event["sequence"] != 0
            or event["previous_event_sha256"] is not None
            or event["sqlite_mutations"] != 0
            or not isinstance(event["codex_version"], str)
            or not event["codex_version"]
        ):
            raise BulkloadError("private journal prepared claim is invalid")
        selected = _canonical_state_classes(event["selected_state_classes"])
        _require_directory_identity(
            event["destination_codex_home_identity"],
            "private journal destination Codex identity",
        )
        for key in (
            "destination_codex_home",
            "install_plan_path",
            "compatibility_plan_path",
            "source_directory",
            "destination_before_directory",
            "post_capture_directory",
            "recovery_capture_directory",
            "journal_path",
            "receipt_path",
        ):
            _require_journal_absolute_path(
                event[key],
                f"private journal {key}",
            )
        if event["destination_sqlite_home"] is not None:
            _require_journal_absolute_path(
                event["destination_sqlite_home"],
                "private journal destination SQLite home",
            )
            _require_directory_identity(
                event["destination_sqlite_home_identity"],
                "private journal destination SQLite identity",
            )
        elif event["destination_sqlite_home_identity"] is not None:
            raise BulkloadError(
                "private journal unselected SQLite identity must be null"
            )
        _validate_journal_auth_pair(
            event,
            digest_key="auth_before_sha256",
            size_key="auth_before_size",
            label="private journal before auth",
        )
        _validate_journal_auth_pair(
            event,
            digest_key="auth_after_sha256",
            size_key="auth_after_size",
            label="private journal after auth",
        )
        _require_receipt_staging_leaf(
            event["receipt_staging_leaf"],
            operation_id,
            "private journal receipt staging leaf",
        )
        _validate_recovery_output_authorities(
            event["output_parent_authorities"],
            expected_labels=expected_labels,
        )
        codex_home = {
            "resolved_path": event["destination_codex_home"],
            "identity": event["destination_codex_home_identity"],
        }
        sqlite_home = (
            {
                "resolved_path": event["destination_sqlite_home"],
                "identity": event["destination_sqlite_home_identity"],
            }
            if event["destination_sqlite_home"] is not None
            else None
        )
        quiescence = _validate_quiescence_binding(
            event["quiescence_attestation"],
            event["bulkload_lock"],
            expected_purpose=operation,
            expected_plan_sha256=event["plan_sha256"],
            expected_apply_receipt_sha256=(
                event["apply_receipt_sha256"] if operation == "rollback" else None
            ),
            expected_journal_sha256=None,
            expected_host_authority_id=event["destination_host_authority_id"],
            expected_codex_version=event["codex_version"],
            expected_selected_state_classes=selected,
            expected_codex_home=codex_home,
            expected_sqlite_home=sqlite_home,
            expected_operation_output_path=event["receipt_path"],
        )
        if (
            event["quiescence_id"] != quiescence["attestation"]["attestation_id"]
            or event["destination_host_authority_id"]
            != quiescence["attestation"]["host_authority_id"]
            or event["codex_version"] != quiescence["attestation"]["codex_version"]
            or event["selected_state_classes"]
            != quiescence["attestation"]["selected_state_classes"]
        ):
            raise BulkloadError(
                "private journal quiescence binding differs from preparation"
            )
        receipt_label = "apply receipt" if operation == "apply" else "rollback receipt"
        _validate_quiescence_output_authority(
            quiescence,
            event["receipt_path"],
            event["output_parent_authorities"][receipt_label],
            label=f"private journal {receipt_label}",
        )
        if operation == "apply":
            _require_journal_absolute_path(
                event["rollback_directory"],
                "private journal rollback directory",
            )
            _require_uuid(
                event["rollback_capture_id"],
                "private journal rollback capture ID",
            )
            _require_sha256(
                event["rollback_capture_sha256"],
                "private journal rollback capture digest",
            )
            if event["auth_install_staging_leaf"] is not None:
                _require_auth_staging_leaf(
                    event["auth_install_staging_leaf"],
                    "private journal install staging leaf",
                )
        else:
            _require_sha256(
                event["apply_receipt_sha256"],
                "private journal apply receipt digest",
            )
            for key in (
                "apply_receipt_path",
                "rollback_source_directory",
                "rollback_preflight_directory",
            ):
                _require_journal_absolute_path(
                    event[key],
                    f"private journal {key}",
                )
            _require_uuid(
                event["rollback_preflight_capture_id"],
                "private journal rollback preflight capture ID",
            )
            _require_sha256(
                event["rollback_preflight_capture_sha256"],
                "private journal rollback preflight digest",
            )
            _require_auth_staging_leaf(
                event["auth_staging_leaf"],
                "private journal rollback staging leaf",
            )
        return

    expected = _JOURNAL_EVENT_FIELDS.get(event_name)
    if expected is None:
        raise BulkloadError(f"private journal event {event_name!r} is unknown")
    _require_exact_keys(event, expected, f"private journal {event_name} event")
    _require_sha256(event["event_sha256"], "private journal event digest")

    if "run_id" in event:
        _require_uuid(event["run_id"], "private journal run ID")
    if "rollback_id" in event:
        _require_uuid(event["rollback_id"], "private journal rollback ID")
    if "staging_leaf" in event:
        _require_auth_staging_leaf(
            event["staging_leaf"],
            "private journal auth staging leaf",
        )
    if "staging_sha256" in event:
        _require_sha256(
            event["staging_sha256"],
            "private journal staging digest",
        )
        _require_journal_size(
            event["staging_size"],
            "private journal staging size",
        )
    if "staging_identity" in event:
        _require_file_identity(
            event["staging_identity"],
            "private journal staging identity",
        )
    if "auth_sha256" in event:
        _require_sha256(event["auth_sha256"], "private journal auth digest")
    if "auth_identity" in event:
        _require_file_identity(
            event["auth_identity"],
            "private journal auth identity",
        )
    if "post_capture_id" in event:
        _require_uuid(
            event["post_capture_id"],
            "private journal post-capture ID",
        )
        _require_sha256(
            event["post_capture_sha256"],
            "private journal post-capture digest",
        )
        if event["provider_runtime_acceptance_verified"] is not False:
            raise BulkloadError("private journal runtime verification claim is invalid")
    if "recovery_capture_id" in event:
        _require_uuid(
            event["recovery_capture_id"],
            "private journal recovery capture ID",
        )
        _require_sha256(
            event["recovery_capture_sha256"],
            "private journal recovery capture digest",
        )
    if event_name in {"receipt-frozen", "rollback-receipt-frozen"}:
        operation_id = (
            event["run_id"] if event_name == "receipt-frozen" else event["rollback_id"]
        )
        _require_journal_absolute_path(
            event["receipt_path"],
            "private journal receipt path",
        )
        _require_receipt_staging_leaf(
            event["receipt_staging_leaf"],
            operation_id,
            "private journal receipt staging leaf",
        )
    if event_name == "recovery-prepared":
        if event["operation"] not in {"apply", "rollback"}:
            raise BulkloadError("private journal recovery operation is invalid")
        for key in ("plan_sha256", "accepted_journal_sha256"):
            _require_sha256(event[key], f"private journal recovery {key}")
        if event["operation"] == "rollback":
            _require_sha256(
                event["apply_receipt_sha256"],
                "private journal recovery apply receipt digest",
            )
        elif event["apply_receipt_sha256"] is not None:
            raise BulkloadError("private apply recovery must not bind an apply receipt")
        _require_uuid(event["operation_id"], "private journal recovery operation ID")
        recovery_id = _require_uuid(
            event["recovery_id"],
            "private journal recovery ID",
        )
        _require_uuid(
            event["quiescence_id"],
            "private journal recovery quiescence ID",
        )
        for key in (
            "preflight_capture_directory",
            "post_capture_directory",
            "receipt_path",
        ):
            _require_journal_absolute_path(
                event[key],
                f"private journal recovery {key}",
            )
        _require_auth_staging_leaf(
            event["auth_staging_leaf"],
            "private journal recovery auth staging leaf",
        )
        _require_receipt_staging_leaf(
            event["receipt_staging_leaf"],
            recovery_id,
            "private journal recovery receipt staging leaf",
        )
        if event["observed_state"] not in {"before", "after"}:
            raise BulkloadError("private journal recovery observed state is invalid")
        _validate_recovery_output_authorities(
            event["output_parent_authorities"],
            expected_labels={
                "recovery preflight capture",
                "recovery post capture",
                "recovery receipt",
            },
        )
        quiescence = _validate_quiescence_binding(
            event["quiescence_attestation"],
            event["bulkload_lock"],
            expected_purpose="recover",
            expected_plan_sha256=event["plan_sha256"],
            expected_apply_receipt_sha256=event["apply_receipt_sha256"],
            expected_journal_sha256=event["accepted_journal_sha256"],
            expected_host_authority_id=prepared["destination_host_authority_id"],
            expected_codex_version=prepared["codex_version"],
            expected_selected_state_classes=_canonical_state_classes(
                prepared["selected_state_classes"]
            ),
            expected_codex_home={
                "resolved_path": prepared["destination_codex_home"],
                "identity": prepared["destination_codex_home_identity"],
            },
            expected_sqlite_home=(
                {
                    "resolved_path": prepared["destination_sqlite_home"],
                    "identity": prepared["destination_sqlite_home_identity"],
                }
                if prepared["destination_sqlite_home"] is not None
                else None
            ),
            expected_operation_output_path=event["receipt_path"],
        )
        if event["quiescence_id"] != quiescence["attestation"]["attestation_id"]:
            raise BulkloadError("private recovery journal quiescence ID differs")
        _validate_quiescence_output_authority(
            quiescence,
            event["receipt_path"],
            event["output_parent_authorities"]["recovery receipt"],
            label="private recovery journal receipt",
        )
    if event_name.startswith("recovery-") and event_name != "recovery-prepared":
        if event["operation"] not in {"apply", "rollback"}:
            raise BulkloadError("private journal recovery operation is invalid")
        _require_uuid(event["operation_id"], "private journal recovery operation ID")
        _require_uuid(event["recovery_id"], "private journal recovery ID")
    if event_name == "recovery-staging-cleaned":
        leaves = event["cleaned_staging_leaves"]
        if (
            not isinstance(leaves, list)
            or leaves != sorted(set(leaves))
            or not all(isinstance(leaf, str) for leaf in leaves)
        ):
            raise BulkloadError("private journal cleaned staging leaves are invalid")
        for leaf in leaves:
            _require_auth_staging_leaf(
                leaf,
                "private journal cleaned auth staging leaf",
            )
    if event_name == "recovery-receipt-frozen":
        _require_journal_absolute_path(
            event["receipt_path"],
            "private journal recovery receipt path",
        )
        _require_receipt_staging_leaf(
            event["receipt_staging_leaf"],
            event["recovery_id"],
            "private journal recovery receipt staging leaf",
        )
    if event_name == "journal-tail-quarantined":
        if event["operation"] not in {"apply", "rollback"}:
            raise BulkloadError("private journal tail operation is invalid")
        _require_uuid(
            event["operation_id"],
            "private journal tail operation ID",
        )
        for key in (
            "accepted_journal_sha256",
            "complete_prefix_sha256",
            "tail_sha256",
        ):
            _require_sha256(event[key], f"private journal tail {key}")
        for key in ("complete_prefix_size", "tail_size"):
            if (
                type(event[key]) is not int
                or event[key] < 1
                or event[key] > MAX_PRIVATE_RECEIPT_BYTES
            ):
                raise BulkloadError(f"private journal tail {key} is invalid")
        if event["quarantine_leaf"] != _journal_tail_quarantine_name(
            event["accepted_journal_sha256"]
        ):
            raise BulkloadError("private journal tail quarantine leaf differs")


def _validate_journal_prefix(
    names: list[str],
    expected: list[str],
    label: str,
) -> None:
    if len(names) > len(expected) or names != expected[: len(names)]:
        raise BulkloadError(f"{label} event order is invalid")


def _validate_private_journal_events(events: list[dict[str, Any]]) -> None:
    if not events or len(events) > 512:
        raise BulkloadError("private journal event count is invalid")
    previous: str | None = None
    encoded_prefix = b""
    seen_tail_digests: set[str] = set()
    seen_quarantine_leaves: set[str] = set()
    for sequence, event in enumerate(events):
        if (
            event.get("sequence") != sequence
            or event.get("previous_event_sha256") != previous
        ):
            raise BulkloadError("private journal chain is invalid")
        _require_sha256(event.get("event_sha256"), "private journal event digest")
        if object_digest(event, "event_sha256") != event["event_sha256"]:
            raise BulkloadError("private journal event digest mismatch")
        _validate_private_journal_event_shape(
            event,
            prepared=None if sequence == 0 else events[0],
        )
        if event["event"] == "journal-tail-quarantined":
            operation_id_key = (
                "run_id" if events[0]["operation"] == "apply" else "rollback_id"
            )
            if (
                event["operation"] != events[0]["operation"]
                or event["operation_id"] != events[0][operation_id_key]
                or event["complete_prefix_sha256"] != sha256_bytes(encoded_prefix)
                or event["complete_prefix_size"] != len(encoded_prefix)
                or event["accepted_journal_sha256"] in seen_tail_digests
                or event["quarantine_leaf"] in seen_quarantine_leaves
            ):
                raise BulkloadError("private journal tail authority differs")
            seen_tail_digests.add(event["accepted_journal_sha256"])
            seen_quarantine_leaves.add(event["quarantine_leaf"])
        previous = event["event_sha256"]
        encoded_prefix += canonical_bytes(event) + b"\n"

    prepared = events[0]
    operation = prepared["operation"]
    operation_id_key = "run_id" if operation == "apply" else "rollback_id"
    operation_id = prepared[operation_id_key]
    semantic_events = [
        event for event in events if event["event"] != "journal-tail-quarantined"
    ]
    recovery_index = next(
        (
            index
            for index, event in enumerate(semantic_events[1:], start=1)
            if event["event"] == "recovery-prepared"
        ),
        len(semantic_events),
    )
    base = semantic_events[1:recovery_index]
    if any(event["event"].startswith("recovery-") for event in base):
        raise BulkloadError("private journal recovery boundary is invalid")

    for event in base:
        if event.get(operation_id_key) != operation_id:
            raise BulkloadError("private journal operation ID binding differs")

    if operation == "apply":
        compensation_names = [
            "automatic-rollback-staging",
            "automatic-rollback-mutation-intent",
            "automatic-rollback-verified",
        ]
        compensation_index = next(
            (
                index
                for index, event in enumerate(base)
                if event["event"] in compensation_names
            ),
            len(base),
        )
        normal = base[:compensation_index]
        compensation = base[compensation_index:]
        if prepared["auth_install_staging_leaf"] is None:
            _validate_journal_prefix(
                [event["event"] for event in normal],
                [
                    "no-live-mutation",
                    "offline-selected-state-verified",
                    "receipt-frozen",
                ],
                "private apply",
            )
            if compensation:
                raise BulkloadError(
                    "non-mutating private apply has compensation events"
                )
        else:
            _validate_journal_prefix(
                [event["event"] for event in normal],
                [
                    "mutation-intent",
                    "auth-installed",
                    "offline-selected-state-verified",
                    "receipt-frozen",
                ],
                "private apply",
            )
            if compensation:
                if not normal:
                    raise BulkloadError(
                        "private apply compensation lacks mutation authority"
                    )
                _validate_journal_prefix(
                    [event["event"] for event in compensation],
                    compensation_names,
                    "private apply compensation",
                )
        if normal and normal[0]["event"] == "mutation-intent":
            intent = normal[0]
            if (
                intent["staging_leaf"] != prepared["auth_install_staging_leaf"]
                or intent["staging_sha256"] != prepared["auth_after_sha256"]
                or intent["staging_size"] != prepared["auth_after_size"]
            ):
                raise BulkloadError("private apply staging authority differs")
            if len(normal) > 1 and (
                normal[1]["auth_sha256"] != prepared["auth_after_sha256"]
                or normal[1]["auth_identity"] != intent["staging_identity"]
            ):
                raise BulkloadError("private apply installed authority differs")
        if normal and normal[-1]["event"] == "receipt-frozen":
            if (
                normal[-1]["receipt_path"] != prepared["receipt_path"]
                or normal[-1]["receipt_staging_leaf"]
                != prepared["receipt_staging_leaf"]
            ):
                raise BulkloadError("private apply receipt authority differs")
        if compensation:
            staging = compensation[0]
            if (
                staging["staging_sha256"] != prepared["auth_before_sha256"]
                or staging["staging_size"] != prepared["auth_before_size"]
            ):
                raise BulkloadError(
                    "private apply compensation staging authority differs"
                )
            if len(compensation) > 1:
                intent = compensation[1]
                if (
                    intent["staging_leaf"] != staging["staging_leaf"]
                    or intent["staging_sha256"] != staging["staging_sha256"]
                    or intent["staging_size"] != staging["staging_size"]
                ):
                    raise BulkloadError("private apply compensation intent differs")
    else:
        compensation_names = [
            "automatic-reinstall-staging",
            "automatic-reinstall-mutation-intent",
            "automatic-reinstall-verified",
        ]
        compensation_index = next(
            (
                index
                for index, event in enumerate(base)
                if event["event"] in compensation_names
            ),
            len(base),
        )
        normal = base[:compensation_index]
        compensation = base[compensation_index:]
        _validate_journal_prefix(
            [event["event"] for event in normal],
            [
                "rollback-mutation-intent",
                "auth-restored",
                "rollback-offline-state-verified",
                "rollback-receipt-frozen",
            ],
            "private rollback",
        )
        if compensation:
            if not normal:
                raise BulkloadError(
                    "private rollback compensation lacks mutation authority"
                )
            _validate_journal_prefix(
                [event["event"] for event in compensation],
                compensation_names,
                "private rollback compensation",
            )
        if normal:
            intent = normal[0]
            if (
                intent["staging_leaf"] != prepared["auth_staging_leaf"]
                or intent["staging_sha256"] != prepared["auth_after_sha256"]
                or intent["staging_size"] != prepared["auth_after_size"]
            ):
                raise BulkloadError("private rollback staging authority differs")
            if len(normal) > 1 and (
                normal[1]["auth_sha256"] != prepared["auth_after_sha256"]
            ):
                raise BulkloadError("private rollback restored authority differs")
        if normal and normal[-1]["event"] == "rollback-receipt-frozen":
            if (
                normal[-1]["receipt_path"] != prepared["receipt_path"]
                or normal[-1]["receipt_staging_leaf"]
                != prepared["receipt_staging_leaf"]
            ):
                raise BulkloadError("private rollback receipt authority differs")
        if compensation:
            staging = compensation[0]
            if (
                staging["staging_sha256"] != prepared["auth_before_sha256"]
                or staging["staging_size"] != prepared["auth_before_size"]
            ):
                raise BulkloadError(
                    "private rollback compensation staging authority differs"
                )
            if len(compensation) > 1:
                intent = compensation[1]
                if (
                    intent["staging_leaf"] != staging["staging_leaf"]
                    or intent["staging_sha256"] != staging["staging_sha256"]
                    or intent["staging_size"] != staging["staging_size"]
                ):
                    raise BulkloadError("private rollback compensation intent differs")

    recovery_events = semantic_events[recovery_index:]
    used_recovery_ids: set[str] = set()
    used_quiescence_ids = {prepared["quiescence_id"]}
    cursor = 0
    while cursor < len(recovery_events):
        opener = recovery_events[cursor]
        if opener["event"] != "recovery-prepared":
            raise BulkloadError("private journal recovery attempt lacks preparation")
        next_cursor = next(
            (
                index
                for index in range(cursor + 1, len(recovery_events))
                if recovery_events[index]["event"] == "recovery-prepared"
            ),
            len(recovery_events),
        )
        attempt = recovery_events[cursor:next_cursor]
        recovery_id = opener["recovery_id"]
        if recovery_id in used_recovery_ids:
            raise BulkloadError("private journal recovery ID was reused")
        if opener["quiescence_id"] in used_quiescence_ids:
            raise BulkloadError("private journal recovery quiescence ID was reused")
        used_recovery_ids.add(recovery_id)
        used_quiescence_ids.add(opener["quiescence_id"])
        if opener["operation"] != operation or opener["operation_id"] != operation_id:
            raise BulkloadError("private journal recovery root binding differs")
        expected_names = (
            [
                "recovery-prepared",
                "recovery-mutation-intent",
                "recovery-restored-before",
                "recovery-staging-cleaned",
                "recovery-offline-state-verified",
                "recovery-receipt-frozen",
            ]
            if opener["observed_state"] == "after"
            else [
                "recovery-prepared",
                "recovery-confirmed-before",
                "recovery-staging-cleaned",
                "recovery-offline-state-verified",
                "recovery-receipt-frozen",
            ]
        )
        _validate_journal_prefix(
            [event["event"] for event in attempt],
            expected_names,
            "private recovery attempt",
        )
        for event in attempt[1:]:
            if (
                event["operation"] != operation
                or event["operation_id"] != operation_id
                or event["recovery_id"] != recovery_id
            ):
                raise BulkloadError("private journal recovery attempt binding differs")
        if len(attempt) > 1 and attempt[1]["event"] == "recovery-mutation-intent":
            intent = attempt[1]
            if (
                intent["staging_leaf"] != opener["auth_staging_leaf"]
                or intent["staging_sha256"] != prepared["auth_before_sha256"]
                or intent["staging_size"] != prepared["auth_before_size"]
            ):
                raise BulkloadError(
                    "private journal recovery staging authority differs"
                )
            if len(attempt) > 2 and (
                attempt[2]["auth_sha256"] != prepared["auth_before_sha256"]
                or attempt[2]["auth_identity"] != intent["staging_identity"]
            ):
                raise BulkloadError(
                    "private journal recovery restored authority differs"
                )
        elif len(attempt) > 1 and (
            attempt[1]["auth_sha256"] != prepared["auth_before_sha256"]
        ):
            raise BulkloadError("private journal recovery confirmed authority differs")
        if attempt[-1]["event"] == "recovery-receipt-frozen":
            if (
                attempt[-1]["receipt_path"] != opener["receipt_path"]
                or attempt[-1]["receipt_staging_leaf"] != opener["receipt_staging_leaf"]
            ):
                raise BulkloadError(
                    "private journal recovery receipt authority differs"
                )
        cursor = next_cursor


def _create_journal(
    path: Path,
    prepared: dict[str, Any],
) -> tuple[int, int, Path, os.stat_result]:
    first = {
        **prepared,
        "sequence": 0,
        "previous_event_sha256": None,
    }
    first["event_sha256"] = object_digest(first, "event_sha256")
    _validate_private_journal_events([first])
    target, parent = _resolved_output_target(path, "private apply journal")
    parent_info = parent.stat()
    parent_descriptor = private_state._open_private_directory_descriptor(
        parent,
        parent_info,
        "private apply journal parent",
    )
    try:
        descriptor = os.open(
            target.name,
            os.O_RDWR
            | os.O_CREAT
            | os.O_EXCL
            | getattr(os, "O_NOFOLLOW", 0)
            | getattr(os, "O_CLOEXEC", 0),
            0o600,
            dir_fd=parent_descriptor,
        )
    except BaseException:
        os.close(parent_descriptor)
        raise
    try:
        os.fchmod(descriptor, 0o600)
        _write_all(descriptor, canonical_bytes(first) + b"\n")
        os.fsync(descriptor)
        private_state._fsync_directory_descriptor(parent_descriptor)
        return descriptor, parent_descriptor, target, parent_info
    except BaseException:
        os.close(descriptor)
        os.close(parent_descriptor)
        raise


def _read_journal_payload_from_descriptor(
    descriptor: int,
    *,
    allow_torn_tail: bool,
) -> tuple[bytes, bytes | None]:
    before = os.fstat(descriptor)
    if (
        not stat.S_ISREG(before.st_mode)
        or before.st_uid != os.getuid()
        or stat.S_IMODE(before.st_mode) != 0o600
        or before.st_nlink != 1
        or before.st_size < 1
        or before.st_size > MAX_PRIVATE_RECEIPT_BYTES
    ):
        raise BulkloadError("private journal custody is invalid")
    os.lseek(descriptor, 0, os.SEEK_SET)
    payload = b""
    while len(payload) <= MAX_PRIVATE_RECEIPT_BYTES:
        block = os.read(
            descriptor,
            min(1024 * 1024, MAX_PRIVATE_RECEIPT_BYTES + 1 - len(payload)),
        )
        if not block:
            break
        payload += block
    after = os.fstat(descriptor)
    if len(payload) != before.st_size or private_state._stable_artifact_stat(
        after
    ) != private_state._stable_artifact_stat(before):
        raise BulkloadError("private journal changed while reading")
    if payload.endswith(b"\n"):
        complete_payload = payload
        tail = None
    else:
        if not allow_torn_tail:
            raise BulkloadError("private journal is truncated")
        boundary = payload.rfind(b"\n")
        if boundary < 0:
            raise BulkloadError("private journal has no complete event")
        complete_payload = payload[: boundary + 1]
        tail = payload[boundary + 1 :]
        if not tail:
            raise BulkloadError("private journal tail is invalid")
    os.lseek(descriptor, 0, os.SEEK_END)
    return complete_payload, tail


def _decode_journal_events(payload: bytes) -> list[dict[str, Any]]:
    if not payload.endswith(b"\n"):
        raise BulkloadError("private journal complete prefix is invalid")
    lines = payload[:-1].split(b"\n")
    if not lines or len(lines) > 512 or any(not line for line in lines):
        raise BulkloadError("private journal event count is invalid")
    events: list[dict[str, Any]] = []
    previous: str | None = None
    for sequence, line in enumerate(lines):
        value = private_state._strict_json_object(
            line,
            f"private journal event {sequence}",
        )
        if canonical_bytes(value) != line:
            raise BulkloadError("private journal event is not canonical")
        if (
            value.get("sequence") != sequence
            or value.get("previous_event_sha256") != previous
            or not isinstance(value.get("event"), str)
            or not value["event"]
            or not isinstance(value.get("recorded_at"), str)
            or not value["recorded_at"]
        ):
            raise BulkloadError("private journal chain is invalid")
        _require_sha256(value.get("event_sha256"), "private journal event digest")
        if object_digest(value, "event_sha256") != value["event_sha256"]:
            raise BulkloadError("private journal event digest mismatch")
        if sequence == 0:
            if (
                value.get("schema") != PRIVATE_JOURNAL_SCHEMA
                or value.get("operation") not in {"apply", "rollback"}
                or value["event"]
                not in {
                    "prepared",
                    "rollback-prepared",
                }
            ):
                raise BulkloadError("private journal prepared event is invalid")
        previous = value["event_sha256"]
        events.append(value)
    _validate_private_journal_events(events)
    return events


def _read_journal_events_from_descriptor(
    descriptor: int,
) -> list[dict[str, Any]]:
    payload, tail = _read_journal_payload_from_descriptor(
        descriptor,
        allow_torn_tail=False,
    )
    if tail is not None:
        raise BulkloadError("private journal has an unexpected tail")
    return _decode_journal_events(payload)


def _read_journal_events_with_tail_from_descriptor(
    descriptor: int,
) -> tuple[list[dict[str, Any]], bytes, bytes | None]:
    payload, tail = _read_journal_payload_from_descriptor(
        descriptor,
        allow_torn_tail=True,
    )
    return _decode_journal_events(payload), payload, tail


def _append_journal(descriptor: int, event: dict[str, Any]) -> None:
    info = os.fstat(descriptor)
    if (
        not stat.S_ISREG(info.st_mode)
        or info.st_uid != os.getuid()
        or stat.S_IMODE(info.st_mode) != 0o600
        or info.st_nlink != 1
    ):
        raise BulkloadError("private apply journal custody is invalid")
    events = _read_journal_events_from_descriptor(descriptor)
    appended = {
        **event,
        "sequence": len(events),
        "previous_event_sha256": events[-1]["event_sha256"],
    }
    appended["event_sha256"] = object_digest(appended, "event_sha256")
    _validate_private_journal_events([*events, appended])
    os.lseek(descriptor, 0, os.SEEK_END)
    _write_all(descriptor, canonical_bytes(appended) + b"\n")
    os.fsync(descriptor)


def _journal_digest(
    descriptor: int,
    parent_descriptor: int,
    leaf: str,
) -> tuple[str, int]:
    before = os.fstat(descriptor)
    entry = os.stat(leaf, dir_fd=parent_descriptor, follow_symlinks=False)
    if (
        not stat.S_ISREG(before.st_mode)
        or before.st_uid != os.getuid()
        or stat.S_IMODE(before.st_mode) != 0o600
        or before.st_nlink != 1
        or private_state._stable_artifact_stat(entry)
        != private_state._stable_artifact_stat(before)
    ):
        raise BulkloadError("private journal custody is invalid")
    os.lseek(descriptor, 0, os.SEEK_SET)
    digest = hashlib.sha256()
    size = 0
    while True:
        payload = os.read(descriptor, 1024 * 1024)
        if not payload:
            break
        digest.update(payload)
        size += len(payload)
    after = os.fstat(descriptor)
    current = os.stat(leaf, dir_fd=parent_descriptor, follow_symlinks=False)
    if (
        private_state._stable_artifact_stat(after)
        != private_state._stable_artifact_stat(before)
        or private_state._stable_artifact_stat(current)
        != private_state._stable_artifact_stat(before)
        or size != before.st_size
    ):
        raise BulkloadError("private journal changed while hashing")
    os.lseek(descriptor, 0, os.SEEK_END)
    return digest.hexdigest(), size


def _open_pinned_private_journal(
    path: Path,
    *,
    accept_sha256: str,
) -> tuple[
    int,
    int,
    Path,
    os.stat_result,
    list[dict[str, Any]],
    int,
    tuple[bytes, bytes] | None,
]:
    _require_sha256(accept_sha256, "accepted private journal digest")
    requested = path.expanduser()
    parent, parent_info = private_state._resolve_private_directory(
        requested.parent,
        "private journal parent",
    )
    parent_descriptor = private_state._open_private_directory_descriptor(
        parent,
        parent_info,
        "private journal parent",
    )
    descriptor = -1
    try:
        descriptor = os.open(
            requested.name,
            os.O_RDWR | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0),
            dir_fd=parent_descriptor,
        )
        entry = os.stat(
            requested.name,
            dir_fd=parent_descriptor,
            follow_symlinks=False,
        )
        opened = os.fstat(descriptor)
        if private_state._stable_artifact_stat(entry) != (
            private_state._stable_artifact_stat(opened)
        ):
            raise BulkloadError("private journal binding changed")
        observed_sha256, observed_size = _journal_digest(
            descriptor,
            parent_descriptor,
            requested.name,
        )
        if observed_sha256 != accept_sha256:
            raise BulkloadError("accepted private journal digest differs")
        events, complete_payload, tail = _read_journal_events_with_tail_from_descriptor(
            descriptor
        )
        return (
            descriptor,
            parent_descriptor,
            parent,
            parent_info,
            events,
            observed_size,
            ((complete_payload, tail) if tail is not None else None),
        )
    except BaseException:
        if descriptor >= 0:
            os.close(descriptor)
        os.close(parent_descriptor)
        raise


def _publish_or_verify_private_artifact_at(
    parent_descriptor: int,
    leaf: str,
    payload: bytes,
) -> None:
    try:
        private_state._write_exclusive_at(
            parent_descriptor,
            leaf,
            payload,
        )
    except FileExistsError:
        private_state._verify_private_payload_at(
            parent_descriptor,
            leaf,
            payload,
        )
    else:
        private_state._fsync_directory_descriptor(parent_descriptor)
        private_state._verify_private_payload_at(
            parent_descriptor,
            leaf,
            payload,
        )


def _verify_private_artifact_digest_at(
    parent_descriptor: int,
    leaf: str,
    *,
    expected_sha256: str,
    expected_size: int,
    label: str,
) -> None:
    descriptor = os.open(
        leaf,
        os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0),
        dir_fd=parent_descriptor,
    )
    try:
        before = os.fstat(descriptor)
        if (
            not stat.S_ISREG(before.st_mode)
            or before.st_uid != os.getuid()
            or stat.S_IMODE(before.st_mode) != 0o600
            or before.st_nlink != 1
            or before.st_size != expected_size
        ):
            raise BulkloadError(f"{label} custody is invalid")
        digest = hashlib.sha256()
        size = 0
        while size <= expected_size:
            block = os.read(
                descriptor,
                min(65536, expected_size + 1 - size),
            )
            if not block:
                break
            digest.update(block)
            size += len(block)
        after = os.fstat(descriptor)
        entry = os.stat(
            leaf,
            dir_fd=parent_descriptor,
            follow_symlinks=False,
        )
        if (
            size != expected_size
            or digest.hexdigest() != expected_sha256
            or private_state._stable_artifact_stat(after)
            != private_state._stable_artifact_stat(before)
            or private_state._stable_artifact_stat(entry)
            != private_state._stable_artifact_stat(before)
        ):
            raise BulkloadError(f"{label} differs from its journal authority")
    finally:
        os.close(descriptor)


def _verify_journal_tail_quarantines(
    events: list[dict[str, Any]],
    parent_descriptor: int,
) -> None:
    for event in events:
        if event["event"] != "journal-tail-quarantined":
            continue
        _verify_private_artifact_digest_at(
            parent_descriptor,
            event["quarantine_leaf"],
            expected_sha256=event["tail_sha256"],
            expected_size=event["tail_size"],
            label="private journal tail quarantine",
        )


def _repair_pinned_private_journal_tail(
    pinned: tuple[
        int,
        int,
        Path,
        os.stat_result,
        list[dict[str, Any]],
        int,
        tuple[bytes, bytes] | None,
    ],
    *,
    journal_leaf: str,
    accept_sha256: str,
) -> tuple[
    int,
    int,
    Path,
    os.stat_result,
    list[dict[str, Any]],
    int,
    tuple[bytes, bytes] | None,
]:
    tail_authority = pinned[6]
    if tail_authority is None:
        _verify_journal_tail_quarantines(pinned[4], pinned[1])
        return pinned
    descriptor, parent_descriptor, parent, parent_info, events, size, _ = pinned
    complete_payload, tail = tail_authority
    if size != len(complete_payload) + len(tail):
        raise BulkloadError("private journal tail size differs")
    _require_sha256(accept_sha256, "accepted private journal digest")
    observed_sha256, observed_size = _journal_digest(
        descriptor,
        parent_descriptor,
        journal_leaf,
    )
    if observed_sha256 != accept_sha256 or observed_size != size:
        raise BulkloadError("private journal changed before tail quarantine")
    operation = events[0]["operation"]
    operation_id = events[0]["run_id" if operation == "apply" else "rollback_id"]
    quarantine_leaf = _journal_tail_quarantine_name(accept_sha256)
    marker = {
        "event": "journal-tail-quarantined",
        "recorded_at": utc_now(),
        "operation": operation,
        "operation_id": operation_id,
        "accepted_journal_sha256": accept_sha256,
        "complete_prefix_sha256": sha256_bytes(complete_payload),
        "complete_prefix_size": len(complete_payload),
        "tail_sha256": sha256_bytes(tail),
        "tail_size": len(tail),
        "quarantine_leaf": quarantine_leaf,
        "sequence": len(events),
        "previous_event_sha256": events[-1]["event_sha256"],
    }
    marker["event_sha256"] = object_digest(marker, "event_sha256")
    repaired_events = [*events, marker]
    _validate_private_journal_events(repaired_events)
    repaired_payload = complete_payload + canonical_bytes(marker) + b"\n"
    if len(repaired_payload) > MAX_PRIVATE_RECEIPT_BYTES:
        raise BulkloadError("repaired private journal exceeds its size bound")

    private_state._revalidate_directory_binding(
        parent,
        parent_descriptor,
        parent_info,
        "private journal parent",
    )
    _publish_or_verify_private_artifact_at(
        parent_descriptor,
        quarantine_leaf,
        tail,
    )
    _verify_private_artifact_digest_at(
        parent_descriptor,
        quarantine_leaf,
        expected_sha256=marker["tail_sha256"],
        expected_size=marker["tail_size"],
        label="private journal tail quarantine",
    )
    repair_staging_leaf = _journal_tail_repair_staging_name(accept_sha256)
    _publish_or_verify_private_artifact_at(
        parent_descriptor,
        repair_staging_leaf,
        repaired_payload,
    )
    current_sha256, current_size = _journal_digest(
        descriptor,
        parent_descriptor,
        journal_leaf,
    )
    if current_sha256 != accept_sha256 or current_size != size:
        raise BulkloadError("private journal changed before atomic tail repair")
    os.replace(
        repair_staging_leaf,
        journal_leaf,
        src_dir_fd=parent_descriptor,
        dst_dir_fd=parent_descriptor,
    )
    private_state._fsync_directory_descriptor(parent_descriptor)
    private_state._verify_private_payload_at(
        parent_descriptor,
        journal_leaf,
        repaired_payload,
    )
    replacement_descriptor = -1
    try:
        replacement_descriptor = os.open(
            journal_leaf,
            os.O_RDWR | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0),
            dir_fd=parent_descriptor,
        )
        replacement_events = _read_journal_events_from_descriptor(
            replacement_descriptor
        )
        if replacement_events != repaired_events:
            raise BulkloadError("repaired private journal events differ")
        replacement_sha256, replacement_size = _journal_digest(
            replacement_descriptor,
            parent_descriptor,
            journal_leaf,
        )
        if replacement_sha256 != sha256_bytes(
            repaired_payload
        ) or replacement_size != len(repaired_payload):
            raise BulkloadError("repaired private journal digest differs")
    except BaseException:
        if replacement_descriptor >= 0:
            os.close(replacement_descriptor)
        raise
    os.close(descriptor)
    _verify_journal_tail_quarantines(
        replacement_events,
        parent_descriptor,
    )
    return (
        replacement_descriptor,
        parent_descriptor,
        parent,
        parent_info,
        replacement_events,
        replacement_size,
        None,
    )


def _revalidate_pinned_private_journal(
    pinned: tuple[
        int,
        int,
        Path,
        os.stat_result,
        list[dict[str, Any]],
        int,
        tuple[bytes, bytes] | None,
    ],
    *,
    leaf: str,
) -> tuple[str, int]:
    descriptor, parent_descriptor, parent, parent_info, _, _, tail = pinned
    if tail is not None:
        raise BulkloadError("private journal tail remains unrepaired")
    private_state._revalidate_directory_binding(
        parent,
        parent_descriptor,
        parent_info,
        "private journal parent",
    )
    events = _read_journal_events_from_descriptor(descriptor)
    if events[0] != pinned[4][0]:
        raise BulkloadError("private journal prepared authority changed")
    return _journal_digest(descriptor, parent_descriptor, leaf)


def _expected_destination_auth(plan: dict[str, Any]) -> tuple[str | None, int | None]:
    auth = plan["auth"]
    if auth["action"] == "not-selected":
        return None, None
    if auth["action"] == "blocked":
        raise BulkloadError("blocked private auth cannot be applied")
    return auth["destination_sha256"], auth["destination_size"]


def _expected_installed_auth(plan: dict[str, Any]) -> tuple[str | None, int | None]:
    auth = plan["auth"]
    if auth["action"] == "not-selected":
        return None, None
    if auth["action"] == "blocked":
        raise BulkloadError("blocked private auth cannot be applied")
    return auth["source_sha256"], auth["source_size"]


def _open_pinned_document(
    path: Path,
    value: dict[str, Any],
    label: str,
) -> tuple[int, int, Path, os.stat_result, dict[str, Any]]:
    requested = path.expanduser()
    parent, parent_info = private_state._resolve_private_directory(
        requested.parent,
        f"{label} parent",
    )
    parent_descriptor = private_state._open_private_directory_descriptor(
        parent,
        parent_info,
        f"{label} parent",
    )
    try:
        descriptor = os.open(
            requested.name,
            os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0),
            dir_fd=parent_descriptor,
        )
    except BaseException:
        os.close(parent_descriptor)
        raise
    expected_payload = canonical_bytes(value) + b"\n"
    expected = {
        "sha256": sha256_bytes(expected_payload),
        "size": len(expected_payload),
    }
    try:
        payload = _read_descriptor_payload(descriptor, expected, label)
        if payload != expected_payload:
            raise BulkloadError(f"{label} is not canonical")
        return descriptor, parent_descriptor, parent, parent_info, expected
    except BaseException:
        os.close(descriptor)
        os.close(parent_descriptor)
        raise


def _revalidate_pinned_document(
    descriptor: int,
    parent_descriptor: int,
    parent: Path,
    parent_info: os.stat_result,
    expected: dict[str, Any],
    label: str,
) -> None:
    private_state._revalidate_directory_binding(
        parent,
        parent_descriptor,
        parent_info,
        f"{label} parent",
    )
    _read_descriptor_payload(descriptor, expected, label)


def _close_descriptors(descriptors: list[int]) -> None:
    for descriptor in reversed(descriptors):
        try:
            os.close(descriptor)
        except OSError:
            pass


def _fresh_capture_expected_auth(
    plan: dict[str, Any],
    *,
    installed: bool,
) -> str | None:
    digest, _ = (
        _expected_installed_auth(plan)
        if installed
        else _expected_destination_auth(plan)
    )
    return digest


def _build_apply_receipt(
    plan: dict[str, Any],
    *,
    run_id: str,
    quiescence_id: str,
    quiescence_attestation: dict[str, Any],
    bulkload_lock: dict[str, Any],
    rollback_directory: Path,
    rollback_capture: dict[str, Any],
    post_capture_directory: Path,
    post_capture: dict[str, Any],
    journal_path: Path,
    journal_sha256: str,
    journal_size: int,
    receipt_path: Path,
) -> dict[str, Any]:
    receipt: dict[str, Any] = {
        "schema": PRIVATE_APPLY_RECEIPT_SCHEMA,
        "applied_at": utc_now(),
        "run_id": run_id,
        "quiescence_id": quiescence_id,
        "quiescence_attestation": quiescence_attestation,
        "bulkload_lock": bulkload_lock,
        "plan_sha256": plan["plan_sha256"],
        "compatibility_plan_sha256": plan["compatibility_plan_sha256"],
        "source_capture_id": plan["source_capture_id"],
        "destination_before_capture_id": plan["destination_before_capture_id"],
        "destination_preflight_capture_id": rollback_capture["capture_id"],
        "destination_preflight_capture_sha256": rollback_capture["capture_sha256"],
        "destination_post_capture_id": post_capture["capture_id"],
        "destination_post_capture_sha256": post_capture["capture_sha256"],
        "destination_host_authority_id": plan["destination_host_authority_id"],
        "destination_binding": plan["destination_binding"],
        "selected_state_classes": plan["selected_state_classes"],
        "rollback_directory": os.fspath(rollback_directory),
        "post_capture_directory": os.fspath(post_capture_directory),
        "receipt_path": os.fspath(receipt_path),
        "journal": {
            "path": os.fspath(journal_path),
            "sha256": journal_sha256,
            "size": journal_size,
        },
        "auth": {
            "action": plan["auth"]["action"],
            "before_sha256": (plan["auth"].get("destination_sha256")),
            "installed_sha256": plan["auth"].get("source_sha256"),
            "mutation_applied": plan["auth"]["mutates"],
        },
        "sqlite_families": [
            {
                "basename": item["basename"],
                "action": item["action"],
                "sha256": item["destination_sha256"],
                "size": item["destination_size"],
                "mutation_applied": False,
            }
            for item in plan["sqlite_families"]
        ],
        "automatic_rollback_performed": False,
        "offline_verified": True,
        "provider_runtime_acceptance_verified": False,
    }
    receipt["receipt_sha256"] = object_digest(receipt, "receipt_sha256")
    return receipt


def validate_codex_private_apply_receipt(
    value: dict[str, Any],
    *,
    expected_receipt_path: Path | None = None,
) -> None:
    _require_exact_keys(
        value,
        {
            "schema",
            "applied_at",
            "run_id",
            "quiescence_id",
            "quiescence_attestation",
            "bulkload_lock",
            "plan_sha256",
            "compatibility_plan_sha256",
            "source_capture_id",
            "destination_before_capture_id",
            "destination_preflight_capture_id",
            "destination_preflight_capture_sha256",
            "destination_post_capture_id",
            "destination_post_capture_sha256",
            "destination_host_authority_id",
            "destination_binding",
            "selected_state_classes",
            "rollback_directory",
            "post_capture_directory",
            "receipt_path",
            "journal",
            "auth",
            "sqlite_families",
            "automatic_rollback_performed",
            "offline_verified",
            "provider_runtime_acceptance_verified",
            "receipt_sha256",
        },
        "private apply receipt",
    )
    if value["schema"] != PRIVATE_APPLY_RECEIPT_SCHEMA:
        raise BulkloadError("unsupported private apply receipt schema")
    if not isinstance(value["applied_at"], str) or not value["applied_at"]:
        raise BulkloadError("private apply receipt timestamp is invalid")
    for key in (
        "run_id",
        "quiescence_id",
        "source_capture_id",
        "destination_before_capture_id",
        "destination_preflight_capture_id",
        "destination_post_capture_id",
        "destination_host_authority_id",
    ):
        _require_uuid(value[key], f"private apply receipt {key}")
    capture_ids = {
        value["source_capture_id"],
        value["destination_before_capture_id"],
        value["destination_preflight_capture_id"],
        value["destination_post_capture_id"],
    }
    if len(capture_ids) != 4:
        raise BulkloadError("private apply receipt reused a capture ID")
    for key in (
        "plan_sha256",
        "compatibility_plan_sha256",
        "destination_preflight_capture_sha256",
        "destination_post_capture_sha256",
        "receipt_sha256",
    ):
        _require_sha256(value[key], f"private apply receipt {key}")
    selected = _canonical_state_classes(value["selected_state_classes"])
    if selected != value["selected_state_classes"]:
        raise BulkloadError("private apply state classes are not canonical")
    destination_binding = _require_mapping(
        value["destination_binding"],
        "private apply destination binding",
    )
    destination_codex_version = destination_binding.get("codex_version")
    codex_home, sqlite_home = _destination_quiescence_expectations(
        destination_binding,
        expected_host_authority_id=value["destination_host_authority_id"],
        expected_codex_version=destination_codex_version,
        expected_selected_state_classes=selected,
    )
    quiescence = _validate_quiescence_binding(
        value["quiescence_attestation"],
        value["bulkload_lock"],
        expected_purpose="apply",
        expected_plan_sha256=value["plan_sha256"],
        expected_apply_receipt_sha256=None,
        expected_journal_sha256=None,
        expected_host_authority_id=value["destination_host_authority_id"],
        expected_codex_version=destination_codex_version,
        expected_selected_state_classes=selected,
        expected_codex_home=codex_home,
        expected_sqlite_home=sqlite_home,
        expected_operation_output_path=value["receipt_path"],
    )
    if value["quiescence_id"] != quiescence["attestation"]["attestation_id"]:
        raise BulkloadError("private apply receipt quiescence ID differs")
    _validate_receipt_output_binding(
        value["receipt_path"],
        quiescence,
        expected_receipt_path=expected_receipt_path,
    )
    for key in ("rollback_directory", "post_capture_directory"):
        if not isinstance(value[key], str) or not Path(value[key]).is_absolute():
            raise BulkloadError("private apply receipt path is invalid")

    binding = _require_mapping(
        value["destination_binding"],
        "private apply destination binding",
    )
    _require_exact_keys(
        binding,
        {
            "host",
            "host_authority_id",
            "codex_version",
            "codex_home",
            "sqlite_home",
        },
        "private apply destination binding",
    )
    if binding["host_authority_id"] != value["destination_host_authority_id"]:
        raise BulkloadError("private apply destination binding is inconsistent")
    journal = _require_mapping(value["journal"], "private apply journal")
    _require_exact_keys(
        journal,
        {"path", "sha256", "size"},
        "private apply journal",
    )
    if not isinstance(journal["path"], str) or not Path(journal["path"]).is_absolute():
        raise BulkloadError("private apply journal path is invalid")
    _require_sha256(journal["sha256"], "private apply journal digest")
    if not isinstance(journal["size"], int) or journal["size"] < 1:
        raise BulkloadError("private apply journal size is invalid")

    auth = _require_mapping(value["auth"], "private apply auth")
    _require_exact_keys(
        auth,
        {
            "action",
            "before_sha256",
            "installed_sha256",
            "mutation_applied",
        },
        "private apply auth",
    )
    if auth["action"] == "not-selected":
        if (
            auth["before_sha256"] is not None
            or auth["installed_sha256"] is not None
            or auth["mutation_applied"] is not False
        ):
            raise BulkloadError("private apply auth receipt is inconsistent")
    elif auth["action"] in {
        "preserve-identical",
        "replace-existing-after-fresh-full-backup",
    }:
        _require_sha256(auth["before_sha256"], "private apply before auth digest")
        _require_sha256(
            auth["installed_sha256"],
            "private apply installed auth digest",
        )
        expected_mutation = auth["action"] == "replace-existing-after-fresh-full-backup"
        if auth["mutation_applied"] is not expected_mutation:
            raise BulkloadError("private apply auth mutation is inconsistent")
    else:
        raise BulkloadError("private apply auth action is invalid")

    families = value["sqlite_families"]
    if not isinstance(families, list):
        raise BulkloadError("private apply SQLite receipt is invalid")
    basenames: list[str] = []
    for item_value in families:
        item = _require_mapping(item_value, "private apply SQLite family")
        _require_exact_keys(
            item,
            {"basename", "action", "sha256", "size", "mutation_applied"},
            "private apply SQLite family",
        )
        if (
            not isinstance(item["basename"], str)
            or not private_state.SQLITE_BASENAME.fullmatch(item["basename"])
            or item["action"] != "preserve-destination-exact"
            or item["mutation_applied"] is not False
            or not isinstance(item["size"], int)
            or item["size"] < 0
        ):
            raise BulkloadError("private apply SQLite receipt is invalid")
        _require_sha256(item["sha256"], "private apply SQLite digest")
        basenames.append(item["basename"])
    if basenames != sorted(set(basenames)):
        raise BulkloadError("private apply SQLite families are not canonical")
    if (
        value["automatic_rollback_performed"] is not False
        or value["offline_verified"] is not True
        or value["provider_runtime_acceptance_verified"] is not False
    ):
        raise BulkloadError("private apply receipt claim is invalid")
    if object_digest(value, "receipt_sha256") != value["receipt_sha256"]:
        raise BulkloadError("private apply receipt digest mismatch")


def read_codex_private_apply_receipt(path: Path) -> dict[str, Any]:
    value = private_state.read_private_json_document(
        path,
        label="private apply receipt",
        max_bytes=MAX_PRIVATE_RECEIPT_BYTES,
    )
    validate_codex_private_apply_receipt(
        value,
        expected_receipt_path=path,
    )
    return value


def apply_codex_private_install(
    install_plan_path: Path,
    compatibility_plan_path: Path,
    source_directory: Path,
    destination_before_directory: Path,
    *,
    destination_codex_home: Path,
    destination_sqlite_home: Path | None,
    rollback_directory: Path,
    post_capture_directory: Path,
    recovery_capture_directory: Path,
    journal_path: Path,
    receipt_path: Path,
    accept_plan: str,
    destination_host_authority_id: str,
    codex_version: str,
    quiescence_attestation_path: Path,
    accept_quiescence_attestation: str,
    acknowledge_private_apply: bool,
) -> dict[str, Any]:
    """Apply one auth-only mutation while preserving every SQLite family."""
    if os.geteuid() == 0:
        raise BulkloadError("private Codex apply refuses root")
    if not acknowledge_private_apply:
        raise BulkloadError("private Codex apply requires explicit acknowledgement")

    install_plan = read_codex_private_install_plan(install_plan_path)
    compatibility_plan = private_state.read_codex_private_state_plan(
        compatibility_plan_path
    )
    if install_plan["plan_sha256"] != accept_plan:
        raise BulkloadError("accepted private install plan digest differs")
    if not install_plan["ready_for_apply"] or install_plan["blockers"]:
        raise BulkloadError("private install plan is not ready for apply")
    if install_plan["destination_host_authority_id"] != destination_host_authority_id:
        raise BulkloadError("destination host authority acknowledgement differs")
    if install_plan["codex_version"] != codex_version:
        raise BulkloadError("destination Codex version acknowledgement differs")
    validate_codex_private_install_plan_against_inputs(
        install_plan,
        compatibility_plan,
        source_directory,
        destination_before_directory,
    )

    source_capture, source_root = private_state.read_codex_private_bundle(
        source_directory,
        "source",
    )
    destination_before, destination_before_root = (
        private_state.read_codex_private_bundle(
            destination_before_directory,
            "destination",
        )
    )
    codex_root, codex_root_info = private_state._resolve_private_directory(
        destination_codex_home,
        "destination Codex home",
    )
    if (
        os.fspath(codex_root)
        != install_plan["destination_binding"]["codex_home"]["resolved_path"]
        or private_state._directory_identity_record(codex_root_info)
        != install_plan["destination_binding"]["codex_home"]["identity"]
    ):
        raise BulkloadError("destination Codex home differs from accepted binding")
    sqlite_root: Path | None = None
    sqlite_root_info: os.stat_result | None = None
    if "sqlite" in install_plan["selected_state_classes"]:
        if destination_sqlite_home is None:
            raise BulkloadError("destination SQLite home is required")
        sqlite_root, sqlite_root_info = private_state._resolve_private_directory(
            destination_sqlite_home,
            "destination SQLite home",
        )
        binding = install_plan["destination_binding"]["sqlite_home"]
        if (
            os.fspath(sqlite_root) != binding["resolved_path"]
            or private_state._directory_identity_record(sqlite_root_info)
            != binding["identity"]
        ):
            raise BulkloadError("destination SQLite home differs from accepted binding")
    elif destination_sqlite_home is not None:
        raise BulkloadError("destination SQLite home was not selected")

    outputs = _validate_output_topology(
        {
            "rollback capture": rollback_directory,
            "post capture": post_capture_directory,
            "recovery capture": recovery_capture_directory,
            "apply journal": journal_path,
            "apply receipt": receipt_path,
        },
        {
            "source bundle": source_root,
            "destination-before bundle": destination_before_root,
            "destination Codex home": codex_root,
            **(
                {"destination SQLite home": sqlite_root}
                if sqlite_root is not None
                else {}
            ),
            "install plan": install_plan_path,
            "compatibility plan": compatibility_plan_path,
        },
        recorded_protected=_recorded_live_roots(compatibility_plan),
    )
    rollback_target = outputs["rollback capture"]
    post_target = outputs["post capture"]
    recovery_target = outputs["recovery capture"]
    journal_target = outputs["apply journal"]
    receipt_target = outputs["apply receipt"]
    output_parent_authorities = _output_parent_authorities(outputs)

    descriptors: list[int] = []
    journal_descriptor = -1
    journal_parent_descriptor = -1
    mutation_applied = False
    mutation_may_have_committed = False
    run_id = str(uuid.uuid4())
    source_payload: bytes | None = None
    rollback_payload: bytes | None = None
    destination_descriptor = -1
    destination_auth_descriptor = -1
    destination_auth_info: os.stat_result | None = None
    install_staged_identity: dict[str, int] | None = None
    installed_auth_identity: dict[str, int] | None = None
    rollback_completed = False
    receipt_publication = {"attempted": False, "published": False}
    receipt_staging_leaf = _receipt_staging_name(run_id)
    install_staging_name = (
        _auth_staging_name(run_id, "install")
        if install_plan["auth"]["mutates"]
        else None
    )
    bulkload_lock: private_quiescence.CodexPrivateBulkloadLock | None = None
    quiescence_context: (
        private_quiescence.PinnedCodexPrivateQuiescenceAttestation | None
    ) = None
    runtime_context: private_runtime.PinnedPrivateRuntimeAuthority | None = None
    try:
        runtime_context, bulkload_lock, quiescence_context = _open_operation_quiescence(
            quiescence_attestation_path,
            accept_attestation=accept_quiescence_attestation,
            purpose="apply",
            plan=install_plan,
            codex_home=codex_root,
            sqlite_home=sqlite_root,
            operation_output=receipt_target,
        )
        quiescence_attestation, bulkload_lock_record = _operation_quiescence_records(
            quiescence_attestation_path,
            quiescence_context,
            bulkload_lock,
        )
        quiescence_id = quiescence_context.attestation_id
        capture_quiescence = private_state.private_quiescence_capture_record(
            quiescence_context.value
        )
        install_document = _open_pinned_document(
            install_plan_path,
            install_plan,
            "private install plan",
        )
        compatibility_document = _open_pinned_document(
            compatibility_plan_path,
            compatibility_plan,
            "private compatibility plan",
        )
        descriptors.extend(
            [
                install_document[0],
                install_document[1],
                compatibility_document[0],
                compatibility_document[1],
            ]
        )

        destination_descriptor = private_state._open_private_directory_descriptor(
            codex_root,
            codex_root_info,
            "destination Codex home",
        )
        descriptors.append(destination_descriptor)
        sqlite_descriptor = -1
        if sqlite_root is not None and sqlite_root_info is not None:
            sqlite_descriptor = private_state._open_private_directory_descriptor(
                sqlite_root,
                sqlite_root_info,
                "destination SQLite home",
            )
            descriptors.append(sqlite_descriptor)

        _revalidate_output_parent_authorities(output_parent_authorities)
        rollback_capture = _capture_destination(
            install_plan,
            destination_before,
            codex_home=codex_root,
            sqlite_home=sqlite_root,
            output_directory=rollback_target,
            quiescence=capture_quiescence,
            protected_directories=(source_root, destination_before_root),
            recorded_protected_directories=_recorded_live_roots(compatibility_plan),
        )
        _revalidate_output_parent_authorities(output_parent_authorities)
        validate_destination_capture_for_install(
            rollback_capture,
            install_plan,
            expected_auth_sha256=_fresh_capture_expected_auth(
                install_plan,
                installed=False,
            ),
            forbidden_capture_ids={
                install_plan["source_capture_id"],
                install_plan["destination_before_capture_id"],
            },
        )

        auth_action = install_plan["auth"]["action"]
        if auth_action != "not-selected":
            source_auth = source_capture["auth"]
            rollback_auth = rollback_capture["auth"]
            if source_auth is None or rollback_auth is None:
                raise BulkloadError("private apply auth evidence is incomplete")
            source_pinned = _open_pinned_auth(
                source_root,
                source_auth,
                "source private bundle",
            )
            rollback_pinned = _open_pinned_auth(
                rollback_target,
                rollback_auth,
                "rollback private bundle",
            )
            descriptors.extend(
                [
                    source_pinned[0],
                    source_pinned[1],
                    rollback_pinned[0],
                    rollback_pinned[1],
                ]
            )
            source_payload = source_pinned[3]
            rollback_payload = rollback_pinned[3]

            destination_auth_descriptor = os.open(
                private_state.AUTH_BASENAME,
                os.O_RDONLY
                | getattr(os, "O_NOFOLLOW", 0)
                | getattr(os, "O_CLOEXEC", 0),
                dir_fd=destination_descriptor,
            )
            descriptors.append(destination_auth_descriptor)
            destination_auth_info = os.fstat(destination_auth_descriptor)
            _assert_live_auth(
                destination_descriptor,
                expected_sha256=rollback_auth["sha256"],
                expected_size=rollback_auth["size"],
                expected_identity=rollback_auth["source_identity"],
                label="destination auth before apply",
            )
            _read_descriptor_payload(
                destination_auth_descriptor,
                rollback_auth,
                "destination auth before apply",
            )

        prepared = {
            "schema": PRIVATE_JOURNAL_SCHEMA,
            "operation": "apply",
            "event": "prepared",
            "recorded_at": utc_now(),
            "run_id": run_id,
            "quiescence_id": quiescence_id,
            "quiescence_attestation": quiescence_attestation,
            "bulkload_lock": bulkload_lock_record,
            "plan_sha256": install_plan["plan_sha256"],
            "compatibility_plan_sha256": install_plan["compatibility_plan_sha256"],
            "destination_host_authority_id": install_plan[
                "destination_host_authority_id"
            ],
            "destination_codex_home": os.fspath(codex_root),
            "destination_codex_home_identity": (
                private_state._directory_identity_record(codex_root_info)
            ),
            "destination_sqlite_home": (
                os.fspath(sqlite_root) if sqlite_root is not None else None
            ),
            "destination_sqlite_home_identity": (
                private_state._directory_identity_record(sqlite_root_info)
                if sqlite_root_info is not None
                else None
            ),
            "selected_state_classes": install_plan["selected_state_classes"],
            "codex_version": install_plan["codex_version"],
            "install_plan_path": os.fspath(
                install_document[2] / install_plan_path.name
            ),
            "compatibility_plan_path": os.fspath(
                compatibility_document[2] / compatibility_plan_path.name
            ),
            "source_directory": os.fspath(source_root),
            "destination_before_directory": os.fspath(destination_before_root),
            "rollback_directory": os.fspath(rollback_target),
            "rollback_capture_id": rollback_capture["capture_id"],
            "rollback_capture_sha256": rollback_capture["capture_sha256"],
            "post_capture_directory": os.fspath(post_target),
            "recovery_capture_directory": os.fspath(recovery_target),
            "journal_path": os.fspath(journal_target),
            "receipt_path": os.fspath(receipt_target),
            "receipt_staging_leaf": receipt_staging_leaf,
            "output_parent_authorities": output_parent_authorities,
            "auth_before_sha256": install_plan["auth"].get("destination_sha256"),
            "auth_before_size": install_plan["auth"].get("destination_size"),
            "auth_after_sha256": install_plan["auth"].get("source_sha256"),
            "auth_after_size": install_plan["auth"].get("source_size"),
            "auth_install_staging_leaf": install_staging_name,
            "sqlite_mutations": 0,
        }
        _revalidate_output_parent_authorities(output_parent_authorities)
        (
            journal_descriptor,
            journal_parent_descriptor,
            journal_target,
            _,
        ) = _create_journal(journal_target, prepared)
        descriptors.extend([journal_descriptor, journal_parent_descriptor])

        _revalidate_pinned_document(
            install_document[0],
            install_document[1],
            install_document[2],
            install_document[3],
            install_document[4],
            "private install plan",
        )
        _revalidate_pinned_document(
            compatibility_document[0],
            compatibility_document[1],
            compatibility_document[2],
            compatibility_document[3],
            compatibility_document[4],
            "private compatibility plan",
        )
        private_state.revalidate_codex_private_bundles(
            source_root,
            destination_before_root,
            expected_source_capture_sha256=install_plan["source_capture_sha256"],
            expected_destination_capture_sha256=install_plan[
                "destination_before_capture_sha256"
            ],
        )
        private_state._revalidate_directory_binding(
            codex_root,
            destination_descriptor,
            codex_root_info,
            "destination Codex home",
        )
        if sqlite_root is not None and sqlite_root_info is not None:
            private_state._revalidate_directory_binding(
                sqlite_root,
                sqlite_descriptor,
                sqlite_root_info,
                "destination SQLite home",
            )

        if install_plan["auth"]["mutates"]:
            assert source_payload is not None
            assert rollback_payload is not None
            assert destination_auth_info is not None
            source_auth = source_capture["auth"]
            rollback_auth = rollback_capture["auth"]
            assert source_auth is not None
            assert rollback_auth is not None
            _read_descriptor_payload(
                destination_auth_descriptor,
                rollback_auth,
                "destination auth immediately before apply",
            )
            _assert_live_auth(
                destination_descriptor,
                expected_sha256=rollback_auth["sha256"],
                expected_size=rollback_auth["size"],
                expected_identity=rollback_auth["source_identity"],
                label="destination auth immediately before apply",
            )
            assert install_staging_name is not None
            _stage_auth(
                destination_descriptor,
                source_payload,
                staging_name=install_staging_name,
            )
            staged_info = os.stat(
                install_staging_name,
                dir_fd=destination_descriptor,
                follow_symlinks=False,
            )
            staged_identity = private_state._identity(staged_info)
            install_staged_identity = staged_identity
            _append_journal(
                journal_descriptor,
                {
                    "event": "mutation-intent",
                    "recorded_at": utc_now(),
                    "run_id": run_id,
                    "staging_leaf": install_staging_name,
                    "staging_sha256": source_auth["sha256"],
                    "staging_size": source_auth["size"],
                    "staging_identity": staged_identity,
                },
            )
            _read_descriptor_payload(
                source_pinned[1],
                source_auth,
                "source auth immediately before apply",
            )
            _assert_live_auth(
                destination_descriptor,
                expected_sha256=rollback_auth["sha256"],
                expected_size=rollback_auth["size"],
                expected_identity=rollback_auth["source_identity"],
                label="destination auth at install commit",
            )
            quiescence_context.revalidate(
                require_operation_output_absent=True,
            )
            quiescence_context.assert_bulkload_lock(bulkload_lock)
            runtime_context.revalidate()
            mutation_may_have_committed = True
            _commit_staged_auth(destination_descriptor, install_staging_name)
            mutation_applied = True
            private_state._fsync_directory_descriptor(destination_descriptor)
            installed_record, _ = _live_auth_record(
                destination_descriptor,
                "installed destination auth",
            )
            if (
                installed_record["sha256"] != source_auth["sha256"]
                or installed_record["size"] != source_auth["size"]
            ):
                raise BulkloadError("installed destination auth verification failed")
            if installed_record["identity"] != staged_identity:
                raise BulkloadError("installed destination auth identity is invalid")
            installed_auth_identity = installed_record["identity"]
            quiescence_context.revalidate(
                require_operation_output_absent=True,
                require_observed_state=False,
            )
            bulkload_lock.revalidate()
            runtime_context.revalidate()
            _append_journal(
                journal_descriptor,
                {
                    "event": "auth-installed",
                    "recorded_at": utc_now(),
                    "run_id": run_id,
                    "auth_sha256": source_auth["sha256"],
                    "auth_identity": installed_record["identity"],
                },
            )
        else:
            _append_journal(
                journal_descriptor,
                {
                    "event": "no-live-mutation",
                    "recorded_at": utc_now(),
                    "run_id": run_id,
                },
            )
            quiescence_context.revalidate(
                require_operation_output_absent=True,
            )
            quiescence_context.assert_bulkload_lock(bulkload_lock)

        _revalidate_output_parent_authorities(output_parent_authorities)
        post_capture = _capture_destination(
            install_plan,
            destination_before,
            codex_home=codex_root,
            sqlite_home=sqlite_root,
            output_directory=post_target,
            quiescence=capture_quiescence,
            protected_directories=(
                source_root,
                destination_before_root,
                rollback_target,
            ),
            recorded_protected_directories=_recorded_live_roots(compatibility_plan),
        )
        _revalidate_output_parent_authorities(output_parent_authorities)
        validate_destination_capture_for_install(
            post_capture,
            install_plan,
            expected_auth_sha256=_fresh_capture_expected_auth(
                install_plan,
                installed=True,
            ),
            forbidden_capture_ids={
                install_plan["source_capture_id"],
                install_plan["destination_before_capture_id"],
                rollback_capture["capture_id"],
            },
        )
        _append_journal(
            journal_descriptor,
            {
                "event": "offline-selected-state-verified",
                "recorded_at": utc_now(),
                "run_id": run_id,
                "post_capture_id": post_capture["capture_id"],
                "post_capture_sha256": post_capture["capture_sha256"],
                "provider_runtime_acceptance_verified": False,
            },
        )
        _append_journal(
            journal_descriptor,
            {
                "event": "receipt-frozen",
                "recorded_at": utc_now(),
                "run_id": run_id,
                "receipt_path": os.fspath(receipt_target),
                "receipt_staging_leaf": receipt_staging_leaf,
            },
        )
        journal_sha256, journal_size = _journal_digest(
            journal_descriptor,
            journal_parent_descriptor,
            journal_target.name,
        )
        receipt = _build_apply_receipt(
            install_plan,
            run_id=run_id,
            quiescence_id=quiescence_id,
            quiescence_attestation=quiescence_attestation,
            bulkload_lock=bulkload_lock_record,
            rollback_directory=rollback_target,
            rollback_capture=rollback_capture,
            post_capture_directory=post_target,
            post_capture=post_capture,
            journal_path=journal_target,
            journal_sha256=journal_sha256,
            journal_size=journal_size,
            receipt_path=receipt_target,
        )
        validate_codex_private_apply_receipt(
            receipt,
            expected_receipt_path=receipt_target,
        )
        quiescence_context.revalidate(
            require_operation_output_absent=True,
            require_observed_state=not mutation_applied,
        )
        quiescence_context.assert_bulkload_lock(bulkload_lock)
        runtime_context.revalidate()
        _revalidate_output_parent_authorities(output_parent_authorities)
        _publish_mutation_receipt(
            receipt_target,
            receipt,
            protected_directories=(
                source_root,
                destination_before_root,
                rollback_target,
                post_target,
            ),
            recorded_protected_directories=_recorded_live_roots(compatibility_plan),
            staging_leaf=receipt_staging_leaf,
            commit_state=receipt_publication,
        )
        quiescence_context.revalidate(require_observed_state=False)
        bulkload_lock.revalidate()
        runtime_context.revalidate()
        _revalidate_output_parent_authorities(output_parent_authorities)
        return receipt
    except BaseException as error:
        rollback_error: BaseException | None = None
        receipt_outcome = (
            _classify_mutation_receipt_publication(
                receipt_target,
                receipt,
                output_parent_authorities["apply receipt"],
            )
            if receipt_publication["attempted"] and "receipt" in locals()
            else "absent"
        )
        if receipt_publication["published"] or receipt_outcome == "exact":
            raise BulkloadError(
                "private Codex apply receipt reached its no-replace commit point; "
                "live installed state was retained and must not be inverted "
                f"({error}); receipt={receipt_target} journal={journal_target}"
            ) from error
        if receipt_outcome == "unknown":
            raise BulkloadError(
                "private Codex apply receipt publication is indeterminate; "
                "live installed state was retained for typed recovery "
                f"({error}); receipt={receipt_target} journal={journal_target}"
            ) from error
        if mutation_may_have_committed and not mutation_applied:
            try:
                observed, _ = _live_auth_record(
                    destination_descriptor,
                    "destination auth after uncertain apply commit",
                )
                source_auth = source_capture["auth"]
                rollback_auth = rollback_capture["auth"]
                assert source_auth is not None
                assert rollback_auth is not None
                if (
                    observed["sha256"] == source_auth["sha256"]
                    and observed["size"] == source_auth["size"]
                    and install_staged_identity is not None
                    and observed["identity"] == install_staged_identity
                ):
                    mutation_applied = True
                    installed_auth_identity = install_staged_identity
                elif (
                    observed["sha256"] == rollback_auth["sha256"]
                    and observed["size"] == rollback_auth["size"]
                    and observed["identity"] == rollback_auth["source_identity"]
                ):
                    mutation_applied = False
                else:
                    rollback_error = BulkloadError(
                        "uncertain apply commit left an unclassified auth identity"
                    )
            except BaseException as caught:
                rollback_error = caught
        if mutation_applied:
            try:
                if journal_descriptor < 0:
                    raise BulkloadError("private apply journal is not pinned")
                assert rollback_payload is not None
                source_auth = source_capture["auth"]
                rollback_capture_value, _ = private_state.read_codex_private_bundle(
                    rollback_target,
                    "destination",
                )
                rollback_auth = rollback_capture_value["auth"]
                assert source_auth is not None
                assert rollback_auth is not None
                if (
                    rollback_capture_value["capture_id"]
                    != rollback_capture["capture_id"]
                    or rollback_capture_value["capture_sha256"]
                    != rollback_capture["capture_sha256"]
                ):
                    raise BulkloadError(
                        "rollback capture changed before automatic rollback"
                    )
                _assert_live_auth(
                    destination_descriptor,
                    expected_sha256=source_auth["sha256"],
                    expected_size=source_auth["size"],
                    expected_identity=installed_auth_identity,
                    label="destination auth before automatic rollback",
                )
                rollback_staging = _auth_staging_name(run_id, "rollback")
                _append_journal(
                    journal_descriptor,
                    {
                        "event": "automatic-rollback-staging",
                        "recorded_at": utc_now(),
                        "run_id": run_id,
                        "staging_leaf": rollback_staging,
                        "staging_sha256": rollback_auth["sha256"],
                        "staging_size": rollback_auth["size"],
                    },
                )
                _stage_auth(
                    destination_descriptor,
                    rollback_payload,
                    staging_name=rollback_staging,
                )
                rollback_staging_info = os.stat(
                    rollback_staging,
                    dir_fd=destination_descriptor,
                    follow_symlinks=False,
                )
                rollback_staging_identity = private_state._identity(
                    rollback_staging_info
                )
                _append_journal(
                    journal_descriptor,
                    {
                        "event": "automatic-rollback-mutation-intent",
                        "recorded_at": utc_now(),
                        "run_id": run_id,
                        "staging_leaf": rollback_staging,
                        "staging_sha256": rollback_auth["sha256"],
                        "staging_size": rollback_auth["size"],
                        "staging_identity": rollback_staging_identity,
                    },
                )
                _read_descriptor_payload(
                    rollback_pinned[1],
                    rollback_auth,
                    "rollback auth immediately before automatic rollback",
                )
                _assert_live_auth(
                    destination_descriptor,
                    expected_sha256=source_auth["sha256"],
                    expected_size=source_auth["size"],
                    expected_identity=installed_auth_identity,
                    label="destination auth at automatic rollback commit",
                )
                quiescence_context.revalidate(
                    require_operation_output_absent=True,
                    require_observed_state=False,
                )
                quiescence_context.assert_bulkload_lock(bulkload_lock)
                # A runtime-path drift discovered after the forward commit is
                # itself a reason to restore the pre-apply bytes. The executing
                # code was loaded from the still-held pre-import authority; do
                # not let replacement of its on-disk names veto compensating
                # rollback. No success receipt is published on this path.
                _commit_staged_auth(destination_descriptor, rollback_staging)
                mutation_applied = False
                private_state._fsync_directory_descriptor(destination_descriptor)
                _assert_live_auth(
                    destination_descriptor,
                    expected_sha256=rollback_auth["sha256"],
                    expected_size=rollback_auth["size"],
                    expected_identity=rollback_staging_identity,
                    label="destination auth after automatic rollback",
                )
                _revalidate_output_parent_authorities(output_parent_authorities)
                recovery_capture = _capture_destination(
                    install_plan,
                    destination_before,
                    codex_home=codex_root,
                    sqlite_home=sqlite_root,
                    output_directory=recovery_target,
                    quiescence=capture_quiescence,
                    protected_directories=(
                        source_root,
                        destination_before_root,
                        rollback_target,
                    ),
                    recorded_protected_directories=(
                        *_recorded_live_roots(compatibility_plan),
                        post_target,
                    ),
                )
                _revalidate_output_parent_authorities(output_parent_authorities)
                validate_destination_capture_for_install(
                    recovery_capture,
                    install_plan,
                    expected_auth_sha256=rollback_auth["sha256"],
                    forbidden_capture_ids={
                        install_plan["source_capture_id"],
                        install_plan["destination_before_capture_id"],
                        rollback_capture_value["capture_id"],
                    },
                )
                _append_journal(
                    journal_descriptor,
                    {
                        "event": "automatic-rollback-verified",
                        "recorded_at": utc_now(),
                        "run_id": run_id,
                        "recovery_capture_id": recovery_capture["capture_id"],
                        "recovery_capture_sha256": recovery_capture["capture_sha256"],
                    },
                )
                rollback_completed = True
            except BaseException as caught:
                rollback_error = caught
        detail = (
            f"; automatic rollback failed: {rollback_error}"
            if rollback_error is not None
            else (
                "; automatic rollback restored destination state"
                if rollback_completed
                else ""
            )
        )
        raise BulkloadError(
            "private Codex apply failed "
            f"({error}){detail}; rollback={rollback_target} "
            f"journal={journal_target}"
        ) from error
    finally:
        _close_descriptors(descriptors)
        if quiescence_context is not None:
            quiescence_context.close()
        if bulkload_lock is not None:
            bulkload_lock.close()
        if runtime_context is not None:
            runtime_context.close()


def _validate_recovery_output_authorities(
    value: Any,
    *,
    expected_labels: set[str],
) -> dict[str, dict[str, Any]]:
    authorities = _require_mapping(value, "private journal output authorities")
    if set(authorities) != expected_labels:
        raise BulkloadError("private journal output authorities are incomplete")
    for label, authority_value in authorities.items():
        authority = _require_mapping(
            authority_value,
            f"private journal {label} parent authority",
        )
        _require_exact_keys(
            authority,
            {"resolved_path", "identity"},
            f"private journal {label} parent authority",
        )
        if (
            not isinstance(authority["resolved_path"], str)
            or not Path(authority["resolved_path"]).is_absolute()
        ):
            raise BulkloadError("private journal output parent path is invalid")
        _require_directory_identity(
            authority["identity"],
            f"private journal {label} parent identity",
        )
    return authorities


def _validate_recovery_prepared_event(
    prepared: dict[str, Any],
    *,
    plan: dict[str, Any],
    journal_path: Path,
) -> str:
    operation = prepared.get("operation")
    common = {
        "schema",
        "operation",
        "event",
        "recorded_at",
        "quiescence_id",
        "quiescence_attestation",
        "bulkload_lock",
        "plan_sha256",
        "compatibility_plan_sha256",
        "destination_host_authority_id",
        "destination_codex_home",
        "destination_codex_home_identity",
        "destination_sqlite_home",
        "destination_sqlite_home_identity",
        "selected_state_classes",
        "codex_version",
        "install_plan_path",
        "compatibility_plan_path",
        "source_directory",
        "destination_before_directory",
        "post_capture_directory",
        "recovery_capture_directory",
        "journal_path",
        "receipt_path",
        "receipt_staging_leaf",
        "output_parent_authorities",
        "auth_before_sha256",
        "auth_before_size",
        "auth_after_sha256",
        "auth_after_size",
        "sqlite_mutations",
        "sequence",
        "previous_event_sha256",
        "event_sha256",
    }
    if operation == "apply":
        expected = common | {
            "run_id",
            "rollback_directory",
            "rollback_capture_id",
            "rollback_capture_sha256",
            "auth_install_staging_leaf",
        }
        operation_id = prepared.get("run_id")
        expected_labels = {
            "rollback capture",
            "post capture",
            "recovery capture",
            "apply journal",
            "apply receipt",
        }
        expected_event = "prepared"
    elif operation == "rollback":
        expected = common | {
            "rollback_id",
            "apply_receipt_sha256",
            "apply_receipt_path",
            "rollback_source_directory",
            "rollback_preflight_directory",
            "rollback_preflight_capture_id",
            "rollback_preflight_capture_sha256",
            "auth_staging_leaf",
        }
        operation_id = prepared.get("rollback_id")
        expected_labels = {
            "rollback preflight capture",
            "rollback post capture",
            "rollback recovery capture",
            "rollback journal",
            "rollback receipt",
        }
        expected_event = "rollback-prepared"
    else:
        raise BulkloadError("private recovery journal operation is invalid")
    _require_exact_keys(prepared, expected, "private recovery prepared event")
    _require_uuid(operation_id, "private recovery operation ID")
    _require_uuid(prepared["quiescence_id"], "private recovery original quiescence ID")
    for key in ("plan_sha256", "compatibility_plan_sha256", "event_sha256"):
        _require_sha256(prepared[key], f"private recovery journal {key}")
    if (
        prepared["schema"] != PRIVATE_JOURNAL_SCHEMA
        or prepared["event"] != expected_event
        or prepared["sequence"] != 0
        or prepared["previous_event_sha256"] is not None
        or prepared["plan_sha256"] != plan["plan_sha256"]
        or prepared["compatibility_plan_sha256"] != plan["compatibility_plan_sha256"]
        or prepared["destination_host_authority_id"]
        != plan["destination_host_authority_id"]
        or prepared["selected_state_classes"] != plan["selected_state_classes"]
        or prepared["codex_version"] != plan["codex_version"]
        or prepared["destination_codex_home"]
        != plan["destination_binding"]["codex_home"]["resolved_path"]
        or prepared["destination_codex_home_identity"]
        != plan["destination_binding"]["codex_home"]["identity"]
        or prepared["destination_sqlite_home"]
        != plan["destination_binding"]["sqlite_home"]["resolved_path"]
        or prepared["destination_sqlite_home_identity"]
        != plan["destination_binding"]["sqlite_home"]["identity"]
        or prepared["sqlite_mutations"] != 0
    ):
        raise BulkloadError("private recovery prepared authority differs")
    _require_directory_identity(
        prepared["destination_codex_home_identity"],
        "private recovery destination Codex identity",
    )
    for key in (
        "destination_codex_home",
        "install_plan_path",
        "compatibility_plan_path",
        "source_directory",
        "destination_before_directory",
        "post_capture_directory",
        "recovery_capture_directory",
        "journal_path",
        "receipt_path",
    ):
        if not isinstance(prepared[key], str) or not Path(prepared[key]).is_absolute():
            raise BulkloadError(f"private recovery journal {key} is invalid")
    if prepared["destination_sqlite_home"] is not None and (
        not isinstance(prepared["destination_sqlite_home"], str)
        or not Path(prepared["destination_sqlite_home"]).is_absolute()
    ):
        raise BulkloadError("private recovery journal SQLite home is invalid")
    _require_directory_identity(
        prepared["destination_sqlite_home_identity"],
        "private recovery destination SQLite identity",
    )
    if os.fspath(journal_path.expanduser().resolve()) != prepared["journal_path"]:
        raise BulkloadError("private recovery journal path differs from its ledger")
    for key in (
        "auth_before_sha256",
        "auth_after_sha256",
    ):
        _require_sha256(prepared[key], f"private recovery journal {key}")
    for key in ("auth_before_size", "auth_after_size"):
        if (
            not isinstance(prepared[key], int)
            or not 0 < prepared[key] <= private_state.MAX_AUTH_BYTES
        ):
            raise BulkloadError("private recovery journal auth size is invalid")
    _validate_recovery_output_authorities(
        prepared["output_parent_authorities"],
        expected_labels=expected_labels,
    )
    codex_home, sqlite_home = _destination_quiescence_expectations(
        plan["destination_binding"],
        expected_host_authority_id=plan["destination_host_authority_id"],
        expected_codex_version=plan["codex_version"],
        expected_selected_state_classes=plan["selected_state_classes"],
    )
    quiescence = _validate_quiescence_binding(
        prepared["quiescence_attestation"],
        prepared["bulkload_lock"],
        expected_purpose=operation,
        expected_plan_sha256=prepared["plan_sha256"],
        expected_apply_receipt_sha256=(
            prepared["apply_receipt_sha256"] if operation == "rollback" else None
        ),
        expected_journal_sha256=None,
        expected_host_authority_id=plan["destination_host_authority_id"],
        expected_codex_version=plan["codex_version"],
        expected_selected_state_classes=plan["selected_state_classes"],
        expected_codex_home=codex_home,
        expected_sqlite_home=sqlite_home,
        expected_operation_output_path=prepared["receipt_path"],
    )
    if (
        prepared["quiescence_id"] != quiescence["attestation"]["attestation_id"]
        or prepared["destination_host_authority_id"]
        != quiescence["attestation"]["host_authority_id"]
        or prepared["codex_version"] != quiescence["attestation"]["codex_version"]
        or prepared["selected_state_classes"]
        != quiescence["attestation"]["selected_state_classes"]
    ):
        raise BulkloadError("private recovery prepared quiescence binding differs")
    receipt_label = "apply receipt" if operation == "apply" else "rollback receipt"
    _validate_quiescence_output_authority(
        quiescence,
        prepared["receipt_path"],
        prepared["output_parent_authorities"][receipt_label],
        label=f"private recovery {receipt_label}",
    )
    return operation_id


def _journal_staging_identities(
    events: list[dict[str, Any]],
    *,
    event_name: str,
    expected_sha256: str,
    expected_size: int,
) -> list[dict[str, int]]:
    identities: list[dict[str, int]] = []
    for event in events:
        if event.get("event") != event_name:
            continue
        if (
            event.get("staging_sha256") != expected_sha256
            or event.get("staging_size") != expected_size
        ):
            raise BulkloadError("private journal staging authority is inconsistent")
        identities.append(
            _require_file_identity(
                event.get("staging_identity"),
                "private journal staging identity",
            )
        )
    return identities


def _cleanup_journaled_auth_staging(
    destination_descriptor: int,
    records: list[dict[str, Any]],
) -> list[str]:
    cleaned: list[str] = []
    seen: set[str] = set()
    for record in records:
        leaf = record["leaf"]
        if leaf in seen:
            continue
        seen.add(leaf)
        if (
            not isinstance(leaf, str)
            or not leaf.startswith(f".{private_state.AUTH_BASENAME}.bulkload-")
            or "/" in leaf
            or leaf in {".", "..", private_state.AUTH_BASENAME}
        ):
            raise BulkloadError("private journal auth staging leaf is invalid")
        try:
            descriptor = os.open(
                leaf,
                os.O_RDONLY
                | getattr(os, "O_NOFOLLOW", 0)
                | getattr(os, "O_CLOEXEC", 0),
                dir_fd=destination_descriptor,
            )
        except FileNotFoundError:
            continue
        try:
            expected = {
                "sha256": record["sha256"],
                "size": record["size"],
            }
            _read_descriptor_payload(
                descriptor,
                expected,
                "journaled auth staging artifact",
            )
            info = os.fstat(descriptor)
            if (
                record.get("identity") is not None
                and private_state._identity(info) != record["identity"]
            ):
                raise BulkloadError("journaled auth staging identity differs")
            entry = os.stat(
                leaf,
                dir_fd=destination_descriptor,
                follow_symlinks=False,
            )
            if private_state._stable_artifact_stat(entry) != (
                private_state._stable_artifact_stat(info)
            ):
                raise BulkloadError("journaled auth staging binding changed")
        finally:
            os.close(descriptor)
        os.unlink(leaf, dir_fd=destination_descriptor)
        cleaned.append(leaf)
    if cleaned:
        private_state._fsync_directory_descriptor(destination_descriptor)
    return sorted(cleaned)


def _build_private_recovery_receipt(
    plan: dict[str, Any],
    *,
    recovery_id: str,
    quiescence_id: str,
    quiescence_attestation: dict[str, Any],
    bulkload_lock: dict[str, Any],
    operation: str,
    operation_id: str,
    apply_receipt_sha256: str | None,
    accepted_journal_sha256: str,
    final_journal_sha256: str,
    final_journal_size: int,
    journal_path: Path,
    original_receipt_path: Path,
    original_receipt_observation: str,
    original_receipt_sha256: str | None,
    decision: str,
    preflight_directory: Path,
    preflight_capture: dict[str, Any],
    post_directory: Path,
    post_capture: dict[str, Any],
    observed_auth: dict[str, Any],
    final_auth: dict[str, Any],
    mutation_applied: bool,
    cleaned_staging_leaves: list[str],
    receipt_path: Path,
) -> dict[str, Any]:
    receipt: dict[str, Any] = {
        "schema": PRIVATE_RECOVERY_RECEIPT_SCHEMA,
        "recovered_at": utc_now(),
        "recovery_id": recovery_id,
        "quiescence_id": quiescence_id,
        "quiescence_attestation": quiescence_attestation,
        "bulkload_lock": bulkload_lock,
        "operation": operation,
        "operation_id": operation_id,
        "plan_sha256": plan["plan_sha256"],
        "compatibility_plan_sha256": plan["compatibility_plan_sha256"],
        "apply_receipt_sha256": apply_receipt_sha256,
        "destination_host_authority_id": plan["destination_host_authority_id"],
        "destination_binding": plan["destination_binding"],
        "selected_state_classes": plan["selected_state_classes"],
        "journal": {
            "path": os.fspath(journal_path),
            "accepted_sha256": accepted_journal_sha256,
            "final_sha256": final_journal_sha256,
            "final_size": final_journal_size,
        },
        "original_receipt": {
            "path": os.fspath(original_receipt_path),
            "observation": original_receipt_observation,
            "sha256": original_receipt_sha256,
        },
        "decision": decision,
        "receipt_path": os.fspath(receipt_path),
        "preflight_directory": os.fspath(preflight_directory),
        "preflight_capture_id": preflight_capture["capture_id"],
        "preflight_capture_sha256": preflight_capture["capture_sha256"],
        "post_directory": os.fspath(post_directory),
        "post_capture_id": post_capture["capture_id"],
        "post_capture_sha256": post_capture["capture_sha256"],
        "auth": {
            "observed_sha256": observed_auth["sha256"],
            "observed_size": observed_auth["size"],
            "observed_identity": observed_auth["identity"],
            "final_sha256": final_auth["sha256"],
            "final_size": final_auth["size"],
            "final_identity": final_auth["identity"],
            "mutation_applied": mutation_applied,
        },
        "sqlite_families": [
            {
                "basename": item["basename"],
                "sha256": item["destination_sha256"],
                "source_sha256": item["destination_live_sha256"],
                "size": item["destination_size"],
                "mutation_applied": False,
            }
            for item in plan["sqlite_families"]
        ],
        "sqlite_mutations": 0,
        "cleaned_staging_leaves": cleaned_staging_leaves,
        "offline_verified": True,
        "provider_runtime_acceptance_verified": False,
    }
    receipt["receipt_sha256"] = object_digest(receipt, "receipt_sha256")
    return receipt


def validate_codex_private_recovery_receipt(
    value: dict[str, Any],
    *,
    expected_receipt_path: Path | None = None,
) -> None:
    _require_exact_keys(
        value,
        {
            "schema",
            "recovered_at",
            "recovery_id",
            "quiescence_id",
            "quiescence_attestation",
            "bulkload_lock",
            "operation",
            "operation_id",
            "plan_sha256",
            "compatibility_plan_sha256",
            "apply_receipt_sha256",
            "destination_host_authority_id",
            "destination_binding",
            "selected_state_classes",
            "journal",
            "original_receipt",
            "decision",
            "receipt_path",
            "preflight_directory",
            "preflight_capture_id",
            "preflight_capture_sha256",
            "post_directory",
            "post_capture_id",
            "post_capture_sha256",
            "auth",
            "sqlite_families",
            "sqlite_mutations",
            "cleaned_staging_leaves",
            "offline_verified",
            "provider_runtime_acceptance_verified",
            "receipt_sha256",
        },
        "private recovery receipt",
    )
    if value["schema"] != PRIVATE_RECOVERY_RECEIPT_SCHEMA:
        raise BulkloadError("unsupported private recovery receipt schema")
    if not isinstance(value["recovered_at"], str) or not value["recovered_at"]:
        raise BulkloadError("private recovery receipt timestamp is invalid")
    for key in (
        "recovery_id",
        "quiescence_id",
        "operation_id",
        "destination_host_authority_id",
        "preflight_capture_id",
        "post_capture_id",
    ):
        _require_uuid(value[key], f"private recovery receipt {key}")
    if value["preflight_capture_id"] == value["post_capture_id"]:
        raise BulkloadError("private recovery receipt reused a capture ID")
    for key in (
        "plan_sha256",
        "compatibility_plan_sha256",
        "preflight_capture_sha256",
        "post_capture_sha256",
        "receipt_sha256",
    ):
        _require_sha256(value[key], f"private recovery receipt {key}")
    if value["operation"] not in {"apply", "rollback"}:
        raise BulkloadError("private recovery receipt operation is invalid")
    if value["operation"] == "rollback":
        _require_sha256(
            value["apply_receipt_sha256"],
            "private recovery apply receipt digest",
        )
    elif value["apply_receipt_sha256"] is not None:
        raise BulkloadError("apply recovery must not bind an apply receipt")
    for key in ("preflight_directory", "post_directory"):
        if not isinstance(value[key], str) or not Path(value[key]).is_absolute():
            raise BulkloadError("private recovery receipt path is invalid")
    journal = _require_mapping(value["journal"], "private recovery journal")
    _require_exact_keys(
        journal,
        {"path", "accepted_sha256", "final_sha256", "final_size"},
        "private recovery journal",
    )
    if not isinstance(journal["path"], str) or not Path(journal["path"]).is_absolute():
        raise BulkloadError("private recovery journal path is invalid")
    _require_sha256(journal["accepted_sha256"], "accepted private journal digest")
    _require_sha256(journal["final_sha256"], "final private journal digest")
    if not isinstance(journal["final_size"], int) or journal["final_size"] < 1:
        raise BulkloadError("private recovery journal size is invalid")
    selected = _canonical_state_classes(value["selected_state_classes"])
    destination_binding = _require_mapping(
        value["destination_binding"],
        "private recovery destination binding",
    )
    destination_codex_version = destination_binding.get("codex_version")
    codex_home, sqlite_home = _destination_quiescence_expectations(
        destination_binding,
        expected_host_authority_id=value["destination_host_authority_id"],
        expected_codex_version=destination_codex_version,
        expected_selected_state_classes=selected,
    )
    quiescence = _validate_quiescence_binding(
        value["quiescence_attestation"],
        value["bulkload_lock"],
        expected_purpose="recover",
        expected_plan_sha256=value["plan_sha256"],
        expected_apply_receipt_sha256=value["apply_receipt_sha256"],
        expected_journal_sha256=journal["accepted_sha256"],
        expected_host_authority_id=value["destination_host_authority_id"],
        expected_codex_version=destination_codex_version,
        expected_selected_state_classes=selected,
        expected_codex_home=codex_home,
        expected_sqlite_home=sqlite_home,
        expected_operation_output_path=value["receipt_path"],
    )
    if value["quiescence_id"] != quiescence["attestation"]["attestation_id"]:
        raise BulkloadError("private recovery receipt quiescence ID differs")
    _validate_receipt_output_binding(
        value["receipt_path"],
        quiescence,
        expected_receipt_path=expected_receipt_path,
    )
    if (
        value["destination_host_authority_id"]
        != quiescence["attestation"]["host_authority_id"]
    ):
        raise BulkloadError("private recovery receipt quiescence authority differs")
    original = _require_mapping(
        value["original_receipt"],
        "private recovery original receipt",
    )
    _require_exact_keys(
        original,
        {"path", "observation", "sha256"},
        "private recovery original receipt",
    )
    if (
        not isinstance(original["path"], str)
        or not Path(original["path"]).is_absolute()
    ):
        raise BulkloadError("private recovery original receipt path is invalid")
    if original["observation"] == "valid":
        _require_sha256(original["sha256"], "private recovery original receipt digest")
    elif original["observation"] == "absent":
        if original["sha256"] is not None:
            raise BulkloadError("absent private recovery receipt digest must be null")
    else:
        raise BulkloadError("private recovery original receipt observation is invalid")
    decision = value["decision"]
    if decision not in {
        "confirmed-before-noop",
        "restored-before",
        "confirmed-committed",
    }:
        raise BulkloadError("private recovery decision is invalid")
    if (decision == "confirmed-committed") != (original["observation"] == "valid"):
        raise BulkloadError("private recovery decision and receipt disagree")
    auth = _require_mapping(value["auth"], "private recovery auth")
    _require_exact_keys(
        auth,
        {
            "observed_sha256",
            "observed_size",
            "observed_identity",
            "final_sha256",
            "final_size",
            "final_identity",
            "mutation_applied",
        },
        "private recovery auth",
    )
    for key in ("observed_sha256", "final_sha256"):
        _require_sha256(auth[key], f"private recovery auth {key}")
    for key in ("observed_size", "final_size"):
        if (
            not isinstance(auth[key], int)
            or not 0 < auth[key] <= private_state.MAX_AUTH_BYTES
        ):
            raise BulkloadError("private recovery auth size is invalid")
    _require_file_identity(
        auth["observed_identity"],
        "private recovery observed auth identity",
    )
    _require_file_identity(
        auth["final_identity"],
        "private recovery final auth identity",
    )
    if auth["mutation_applied"] is not (decision == "restored-before"):
        raise BulkloadError("private recovery auth mutation claim is invalid")
    families = value["sqlite_families"]
    if not isinstance(families, list):
        raise BulkloadError("private recovery SQLite families must be a list")
    basenames: list[str] = []
    for family_value in families:
        family = _require_mapping(family_value, "private recovery SQLite family")
        _require_exact_keys(
            family,
            {
                "basename",
                "sha256",
                "source_sha256",
                "size",
                "mutation_applied",
            },
            "private recovery SQLite family",
        )
        if not private_state.SQLITE_BASENAME.fullmatch(family["basename"]):
            raise BulkloadError("private recovery SQLite basename is invalid")
        basenames.append(family["basename"])
        _require_sha256(family["sha256"], "private recovery SQLite digest")
        _require_sha256(
            family["source_sha256"],
            "private recovery live SQLite digest",
        )
        if (
            not isinstance(family["size"], int)
            or family["size"] < 0
            or family["mutation_applied"] is not False
        ):
            raise BulkloadError("private recovery SQLite claim is invalid")
    if basenames != sorted(set(basenames)):
        raise BulkloadError("private recovery SQLite families are not canonical")
    cleaned = value["cleaned_staging_leaves"]
    if (
        not isinstance(cleaned, list)
        or not all(isinstance(item, str) for item in cleaned)
        or cleaned != sorted(set(cleaned))
    ):
        raise BulkloadError("private recovery cleaned staging list is invalid")
    if (
        value["sqlite_mutations"] != 0
        or value["offline_verified"] is not True
        or value["provider_runtime_acceptance_verified"] is not False
    ):
        raise BulkloadError("private recovery receipt claim is invalid")
    if object_digest(value, "receipt_sha256") != value["receipt_sha256"]:
        raise BulkloadError("private recovery receipt digest mismatch")


def read_codex_private_recovery_receipt(path: Path) -> dict[str, Any]:
    value = private_state.read_private_json_document(
        path,
        label="private recovery receipt",
    )
    validate_codex_private_recovery_receipt(
        value,
        expected_receipt_path=path,
    )
    return value


def _validate_recovery_receipt_against_plan(
    receipt: dict[str, Any],
    plan: dict[str, Any],
    *,
    expected_receipt_path: Path | None = None,
) -> None:
    validate_codex_private_recovery_receipt(
        receipt,
        expected_receipt_path=expected_receipt_path,
    )
    if (
        receipt["plan_sha256"] != plan["plan_sha256"]
        or receipt["compatibility_plan_sha256"] != plan["compatibility_plan_sha256"]
        or receipt["destination_host_authority_id"]
        != plan["destination_host_authority_id"]
        or receipt["destination_binding"] != plan["destination_binding"]
        or receipt["selected_state_classes"] != plan["selected_state_classes"]
    ):
        raise BulkloadError("private recovery receipt differs from accepted plan")
    expected_families = [
        {
            "basename": item["basename"],
            "sha256": item["destination_sha256"],
            "source_sha256": item["destination_live_sha256"],
            "size": item["destination_size"],
            "mutation_applied": False,
        }
        for item in plan["sqlite_families"]
    ]
    if receipt["sqlite_families"] != expected_families:
        raise BulkloadError("private recovery SQLite receipt differs from plan")


def _validate_apply_receipt_against_plan(
    receipt: dict[str, Any],
    plan: dict[str, Any],
    *,
    expected_receipt_path: Path | None = None,
) -> None:
    validate_codex_private_apply_receipt(
        receipt,
        expected_receipt_path=expected_receipt_path,
    )
    if (
        receipt["plan_sha256"] != plan["plan_sha256"]
        or receipt["compatibility_plan_sha256"] != plan["compatibility_plan_sha256"]
        or receipt["source_capture_id"] != plan["source_capture_id"]
        or receipt["destination_before_capture_id"]
        != plan["destination_before_capture_id"]
        or receipt["destination_host_authority_id"]
        != plan["destination_host_authority_id"]
        or receipt["destination_binding"] != plan["destination_binding"]
        or receipt["selected_state_classes"] != plan["selected_state_classes"]
        or receipt["auth"]["action"] != plan["auth"]["action"]
        or receipt["auth"]["before_sha256"] != plan["auth"].get("destination_sha256")
        or receipt["auth"]["installed_sha256"] != plan["auth"].get("source_sha256")
    ):
        raise BulkloadError("private apply receipt differs from accepted plan")
    expected_families = [
        {
            "basename": item["basename"],
            "action": item["action"],
            "sha256": item["destination_sha256"],
            "size": item["destination_size"],
            "mutation_applied": False,
        }
        for item in plan["sqlite_families"]
    ]
    if receipt["sqlite_families"] != expected_families:
        raise BulkloadError("private apply SQLite receipt differs from plan")


def _build_verify_receipt(
    plan: dict[str, Any],
    apply_receipt: dict[str, Any],
    *,
    verification_id: str,
    quiescence_id: str,
    quiescence_attestation: dict[str, Any],
    bulkload_lock: dict[str, Any],
    capture_directory: Path,
    capture: dict[str, Any],
    receipt_path: Path,
) -> dict[str, Any]:
    receipt: dict[str, Any] = {
        "schema": PRIVATE_VERIFY_RECEIPT_SCHEMA,
        "verified_at": utc_now(),
        "verification_id": verification_id,
        "quiescence_id": quiescence_id,
        "quiescence_attestation": quiescence_attestation,
        "bulkload_lock": bulkload_lock,
        "plan_sha256": plan["plan_sha256"],
        "apply_receipt_sha256": apply_receipt["receipt_sha256"],
        "apply_run_id": apply_receipt["run_id"],
        "destination_host_authority_id": plan["destination_host_authority_id"],
        "destination_binding": plan["destination_binding"],
        "destination_capture_directory": os.fspath(capture_directory),
        "destination_capture_id": capture["capture_id"],
        "destination_capture_sha256": capture["capture_sha256"],
        "selected_state_classes": plan["selected_state_classes"],
        "receipt_path": os.fspath(receipt_path),
        "auth_sha256": plan["auth"].get("source_sha256"),
        "sqlite_families": [
            {
                "basename": item["basename"],
                "sha256": item["destination_sha256"],
                "size": item["destination_size"],
            }
            for item in plan["sqlite_families"]
        ],
        "quiescence_claim": "operator-attested-procedural-fence",
        "offline_verified": True,
        "provider_runtime_acceptance_verified": False,
    }
    receipt["receipt_sha256"] = object_digest(receipt, "receipt_sha256")
    return receipt


def validate_codex_private_verify_receipt(
    value: dict[str, Any],
    *,
    expected_receipt_path: Path | None = None,
) -> None:
    _require_exact_keys(
        value,
        {
            "schema",
            "verified_at",
            "verification_id",
            "quiescence_id",
            "quiescence_attestation",
            "bulkload_lock",
            "plan_sha256",
            "apply_receipt_sha256",
            "apply_run_id",
            "destination_host_authority_id",
            "destination_binding",
            "destination_capture_directory",
            "destination_capture_id",
            "destination_capture_sha256",
            "selected_state_classes",
            "receipt_path",
            "auth_sha256",
            "sqlite_families",
            "quiescence_claim",
            "offline_verified",
            "provider_runtime_acceptance_verified",
            "receipt_sha256",
        },
        "private verify receipt",
    )
    if value["schema"] != PRIVATE_VERIFY_RECEIPT_SCHEMA:
        raise BulkloadError("unsupported private verify receipt schema")
    if not isinstance(value["verified_at"], str) or not value["verified_at"]:
        raise BulkloadError("private verify receipt timestamp is invalid")
    for key in (
        "verification_id",
        "quiescence_id",
        "apply_run_id",
        "destination_host_authority_id",
        "destination_capture_id",
    ):
        _require_uuid(value[key], f"private verify receipt {key}")
    for key in (
        "plan_sha256",
        "apply_receipt_sha256",
        "destination_capture_sha256",
        "receipt_sha256",
    ):
        _require_sha256(value[key], f"private verify receipt {key}")
    selected = _canonical_state_classes(value["selected_state_classes"])
    destination_binding = _require_mapping(
        value["destination_binding"],
        "private verify destination binding",
    )
    destination_codex_version = destination_binding.get("codex_version")
    codex_home, sqlite_home = _destination_quiescence_expectations(
        destination_binding,
        expected_host_authority_id=value["destination_host_authority_id"],
        expected_codex_version=destination_codex_version,
        expected_selected_state_classes=selected,
    )
    quiescence = _validate_quiescence_binding(
        value["quiescence_attestation"],
        value["bulkload_lock"],
        expected_purpose="verify",
        expected_plan_sha256=value["plan_sha256"],
        expected_apply_receipt_sha256=value["apply_receipt_sha256"],
        expected_journal_sha256=None,
        expected_host_authority_id=value["destination_host_authority_id"],
        expected_codex_version=destination_codex_version,
        expected_selected_state_classes=selected,
        expected_codex_home=codex_home,
        expected_sqlite_home=sqlite_home,
        expected_operation_output_path=value["receipt_path"],
    )
    if value["quiescence_id"] != quiescence["attestation"]["attestation_id"]:
        raise BulkloadError("private verify receipt quiescence ID differs")
    _validate_receipt_output_binding(
        value["receipt_path"],
        quiescence,
        expected_receipt_path=expected_receipt_path,
    )
    if (
        not isinstance(value["destination_capture_directory"], str)
        or not Path(value["destination_capture_directory"]).is_absolute()
    ):
        raise BulkloadError("private verify capture path is invalid")
    if "auth" in selected:
        _require_sha256(value["auth_sha256"], "private verify auth digest")
    elif value["auth_sha256"] is not None:
        raise BulkloadError("private verify auth selection is inconsistent")
    families = value["sqlite_families"]
    if not isinstance(families, list):
        raise BulkloadError("private verify SQLite families are invalid")
    basenames: list[str] = []
    for item_value in families:
        item = _require_mapping(item_value, "private verify SQLite family")
        _require_exact_keys(
            item,
            {"basename", "sha256", "size"},
            "private verify SQLite family",
        )
        if (
            not isinstance(item["basename"], str)
            or not private_state.SQLITE_BASENAME.fullmatch(item["basename"])
            or not isinstance(item["size"], int)
            or item["size"] < 0
        ):
            raise BulkloadError("private verify SQLite family is invalid")
        _require_sha256(item["sha256"], "private verify SQLite digest")
        basenames.append(item["basename"])
    if basenames != sorted(set(basenames)):
        raise BulkloadError("private verify SQLite families are not canonical")
    if ("sqlite" in selected) != bool(families):
        raise BulkloadError("private verify SQLite selection is inconsistent")
    if (
        value["quiescence_claim"] != "operator-attested-procedural-fence"
        or value["offline_verified"] is not True
        or value["provider_runtime_acceptance_verified"] is not False
    ):
        raise BulkloadError("private verify receipt claim is invalid")
    if object_digest(value, "receipt_sha256") != value["receipt_sha256"]:
        raise BulkloadError("private verify receipt digest mismatch")


def read_codex_private_verify_receipt(path: Path) -> dict[str, Any]:
    value = private_state.read_private_json_document(
        path,
        label="private verify receipt",
    )
    validate_codex_private_verify_receipt(
        value,
        expected_receipt_path=path,
    )
    return value


def _validate_verify_receipt_against_plan(
    receipt: dict[str, Any],
    plan: dict[str, Any],
    apply_receipt: dict[str, Any],
    *,
    expected_receipt_path: Path | None = None,
) -> None:
    validate_codex_private_verify_receipt(
        receipt,
        expected_receipt_path=expected_receipt_path,
    )
    expected_families = [
        {
            "basename": item["basename"],
            "sha256": item["destination_sha256"],
            "size": item["destination_size"],
        }
        for item in plan["sqlite_families"]
    ]
    if (
        receipt["plan_sha256"] != plan["plan_sha256"]
        or receipt["apply_receipt_sha256"] != apply_receipt["receipt_sha256"]
        or receipt["apply_run_id"] != apply_receipt["run_id"]
        or receipt["destination_host_authority_id"]
        != plan["destination_host_authority_id"]
        or receipt["destination_binding"] != plan["destination_binding"]
        or receipt["selected_state_classes"] != plan["selected_state_classes"]
        or receipt["auth_sha256"] != plan["auth"].get("source_sha256")
        or receipt["sqlite_families"] != expected_families
    ):
        raise BulkloadError("private verify receipt differs from accepted plan")


def verify_codex_private_install(
    install_plan_path: Path,
    compatibility_plan_path: Path,
    source_directory: Path,
    destination_before_directory: Path,
    apply_receipt_path: Path,
    *,
    destination_codex_home: Path,
    destination_sqlite_home: Path | None,
    capture_directory: Path,
    receipt_path: Path,
    accept_plan: str,
    accept_apply_receipt: str,
    destination_host_authority_id: str,
    codex_version: str,
    quiescence_attestation_path: Path,
    accept_quiescence_attestation: str,
    acknowledge_private_verify: bool,
) -> dict[str, Any]:
    """Create an independent offline selected-state verification receipt."""
    if os.geteuid() == 0:
        raise BulkloadError("private Codex verify refuses root")
    if not acknowledge_private_verify:
        raise BulkloadError("private Codex verify requires explicit acknowledgement")

    plan = read_codex_private_install_plan(install_plan_path)
    compatibility = private_state.read_codex_private_state_plan(compatibility_plan_path)
    apply_receipt = read_codex_private_apply_receipt(apply_receipt_path)
    if plan["plan_sha256"] != accept_plan:
        raise BulkloadError("accepted private install plan digest differs")
    if apply_receipt["receipt_sha256"] != accept_apply_receipt:
        raise BulkloadError("accepted private apply receipt digest differs")
    if (
        destination_host_authority_id != plan["destination_host_authority_id"]
        or codex_version != plan["codex_version"]
    ):
        raise BulkloadError("private verify destination acknowledgement differs")
    validate_codex_private_install_plan_against_inputs(
        plan,
        compatibility,
        source_directory,
        destination_before_directory,
    )
    _validate_apply_receipt_against_plan(
        apply_receipt,
        plan,
        expected_receipt_path=apply_receipt_path,
    )
    destination_before, destination_before_root = (
        private_state.read_codex_private_bundle(
            destination_before_directory,
            "destination",
        )
    )
    _, source_root = private_state.read_codex_private_bundle(
        source_directory,
        "source",
    )
    codex_root, codex_info = private_state._resolve_private_directory(
        destination_codex_home,
        "destination Codex home",
    )
    if (
        os.fspath(codex_root)
        != plan["destination_binding"]["codex_home"]["resolved_path"]
        or private_state._directory_identity_record(codex_info)
        != plan["destination_binding"]["codex_home"]["identity"]
    ):
        raise BulkloadError("private verify destination Codex binding differs")
    sqlite_root: Path | None = None
    if "sqlite" in plan["selected_state_classes"]:
        if destination_sqlite_home is None:
            raise BulkloadError("private verify SQLite home is required")
        sqlite_root, sqlite_info = private_state._resolve_private_directory(
            destination_sqlite_home,
            "destination SQLite home",
        )
        if (
            os.fspath(sqlite_root)
            != plan["destination_binding"]["sqlite_home"]["resolved_path"]
            or private_state._directory_identity_record(sqlite_info)
            != plan["destination_binding"]["sqlite_home"]["identity"]
        ):
            raise BulkloadError("private verify SQLite binding differs")
    elif destination_sqlite_home is not None:
        raise BulkloadError("private verify SQLite home was not selected")

    outputs = _validate_output_topology(
        {
            "verification capture": capture_directory,
            "verification receipt": receipt_path,
        },
        {
            "source bundle": source_root,
            "destination-before bundle": destination_before_root,
            "apply receipt": apply_receipt_path,
            "apply rollback bundle": Path(apply_receipt["rollback_directory"]),
            "apply post capture": Path(apply_receipt["post_capture_directory"]),
            "apply journal": Path(apply_receipt["journal"]["path"]),
            "destination Codex home": codex_root,
            **(
                {"destination SQLite home": sqlite_root}
                if sqlite_root is not None
                else {}
            ),
        },
        recorded_protected=_recorded_live_roots(compatibility),
    )
    capture_target = outputs["verification capture"]
    receipt_target = outputs["verification receipt"]
    output_parent_authorities = _output_parent_authorities(outputs)
    runtime_context, bulkload_lock, quiescence_context = _open_operation_quiescence(
        quiescence_attestation_path,
        accept_attestation=accept_quiescence_attestation,
        purpose="verify",
        plan=plan,
        codex_home=codex_root,
        sqlite_home=sqlite_root,
        operation_output=receipt_target,
        apply_receipt_sha256=apply_receipt["receipt_sha256"],
    )
    try:
        if quiescence_context.attestation_id == apply_receipt["quiescence_id"]:
            raise BulkloadError(
                "independent verify requires a fresh quiescence attestation"
            )
        quiescence_attestation, bulkload_lock_record = _operation_quiescence_records(
            quiescence_attestation_path,
            quiescence_context,
            bulkload_lock,
        )
        capture_quiescence = private_state.private_quiescence_capture_record(
            quiescence_context.value
        )
        _revalidate_output_parent_authorities(output_parent_authorities)
        capture = _capture_destination(
            plan,
            destination_before,
            codex_home=codex_root,
            sqlite_home=sqlite_root,
            output_directory=capture_target,
            quiescence=capture_quiescence,
            protected_directories=(
                source_root,
                destination_before_root,
                Path(apply_receipt["rollback_directory"]),
                Path(apply_receipt["post_capture_directory"]),
            ),
            recorded_protected_directories=_recorded_live_roots(compatibility),
        )
        _revalidate_output_parent_authorities(output_parent_authorities)
        validate_destination_capture_for_install(
            capture,
            plan,
            expected_auth_sha256=_fresh_capture_expected_auth(
                plan,
                installed=True,
            ),
            forbidden_capture_ids={
                plan["source_capture_id"],
                plan["destination_before_capture_id"],
                apply_receipt["destination_preflight_capture_id"],
                apply_receipt["destination_post_capture_id"],
            },
        )
        receipt = _build_verify_receipt(
            plan,
            apply_receipt,
            verification_id=str(uuid.uuid4()),
            quiescence_id=quiescence_context.attestation_id,
            quiescence_attestation=quiescence_attestation,
            bulkload_lock=bulkload_lock_record,
            capture_directory=capture_target,
            capture=capture,
            receipt_path=receipt_target,
        )
        _validate_verify_receipt_against_plan(
            receipt,
            plan,
            apply_receipt,
            expected_receipt_path=receipt_target,
        )
        quiescence_context.revalidate(
            require_operation_output_absent=True,
        )
        quiescence_context.assert_bulkload_lock(bulkload_lock)
        runtime_context.revalidate()
        _revalidate_output_parent_authorities(output_parent_authorities)
        private_state.write_private_json_noreplace(
            receipt_target,
            receipt,
            protected_directories=(
                source_root,
                destination_before_root,
                capture_target,
            ),
            recorded_protected_directories=(
                codex_root,
                *((sqlite_root,) if sqlite_root is not None else ()),
            ),
        )
        quiescence_context.revalidate()
        bulkload_lock.revalidate()
        runtime_context.revalidate()
        _revalidate_output_parent_authorities(output_parent_authorities)
        published = private_state.read_private_json_document(
            receipt_target,
            label="private verify receipt",
            max_bytes=MAX_PRIVATE_RECEIPT_BYTES,
        )
        _validate_verify_receipt_against_plan(
            published,
            plan,
            apply_receipt,
            expected_receipt_path=receipt_target,
        )
        if published["receipt_sha256"] != receipt["receipt_sha256"]:
            raise BulkloadError("published private verify receipt differs")
        private_state.revalidate_codex_private_bundles(
            source_root,
            destination_before_root,
            expected_source_capture_sha256=plan["source_capture_sha256"],
            expected_destination_capture_sha256=(
                plan["destination_before_capture_sha256"]
            ),
        )
        return receipt
    finally:
        quiescence_context.close()
        bulkload_lock.close()
        runtime_context.close()


def _validate_apply_recovery_evidence(
    receipt: dict[str, Any],
    *,
    rollback_directory: Path,
) -> tuple[dict[str, Any], Path]:
    rollback_capture, rollback_root = private_state.read_codex_private_bundle(
        rollback_directory,
        "destination",
    )
    if (
        os.fspath(rollback_root) != receipt["rollback_directory"]
        or rollback_capture["capture_id"] != receipt["destination_preflight_capture_id"]
        or rollback_capture["capture_sha256"]
        != receipt["destination_preflight_capture_sha256"]
    ):
        raise BulkloadError("private rollback bundle differs from apply receipt")
    journal_path = Path(receipt["journal"]["path"])
    journal_sha256, journal_size = private_state._hash_regular_file(journal_path)
    journal_mode = stat.S_IMODE(journal_path.stat(follow_symlinks=False).st_mode)
    if (
        journal_sha256 != receipt["journal"]["sha256"]
        or journal_size != receipt["journal"]["size"]
        or journal_mode != 0o600
    ):
        raise BulkloadError("private apply journal differs from receipt")
    return rollback_capture, rollback_root


def _open_pinned_apply_journal(
    receipt: dict[str, Any],
    plan: dict[str, Any],
) -> tuple[int, int, Path, os.stat_result]:
    requested = Path(receipt["journal"]["path"])
    parent, parent_info = private_state._resolve_private_directory(
        requested.parent,
        "private apply journal parent",
    )
    parent_descriptor = private_state._open_private_directory_descriptor(
        parent,
        parent_info,
        "private apply journal parent",
    )
    try:
        descriptor = os.open(
            requested.name,
            os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0),
            dir_fd=parent_descriptor,
        )
        observed_sha256, observed_size = _journal_digest(
            descriptor,
            parent_descriptor,
            requested.name,
        )
        if (
            observed_sha256 != receipt["journal"]["sha256"]
            or observed_size != receipt["journal"]["size"]
        ):
            raise BulkloadError("private apply journal differs from receipt")
        events = _read_journal_events_from_descriptor(descriptor)
        prepared = events[0]
        _validate_recovery_prepared_event(
            prepared,
            plan=plan,
            journal_path=requested,
        )
        if (
            prepared["operation"] != "apply"
            or prepared["run_id"] != receipt["run_id"]
            or prepared["quiescence_id"] != receipt["quiescence_id"]
            or prepared["receipt_path"] != receipt["receipt_path"]
            or prepared["rollback_directory"] != receipt["rollback_directory"]
            or prepared["rollback_capture_id"]
            != receipt["destination_preflight_capture_id"]
            or prepared["rollback_capture_sha256"]
            != receipt["destination_preflight_capture_sha256"]
            or prepared["post_capture_directory"] != receipt["post_capture_directory"]
        ):
            raise BulkloadError("private apply journal authority differs from receipt")
        return descriptor, parent_descriptor, parent, parent_info
    except BaseException:
        try:
            os.close(descriptor)
        except (OSError, UnboundLocalError):
            pass
        os.close(parent_descriptor)
        raise


def _build_rollback_receipt(
    plan: dict[str, Any],
    apply_receipt: dict[str, Any],
    *,
    rollback_id: str,
    quiescence_id: str,
    quiescence_attestation: dict[str, Any],
    bulkload_lock: dict[str, Any],
    preflight_directory: Path,
    preflight_capture: dict[str, Any],
    post_directory: Path,
    post_capture: dict[str, Any],
    journal_path: Path,
    journal_sha256: str,
    journal_size: int,
    receipt_path: Path,
) -> dict[str, Any]:
    receipt: dict[str, Any] = {
        "schema": PRIVATE_ROLLBACK_RECEIPT_SCHEMA,
        "rolled_back_at": utc_now(),
        "rollback_id": rollback_id,
        "quiescence_id": quiescence_id,
        "quiescence_attestation": quiescence_attestation,
        "bulkload_lock": bulkload_lock,
        "plan_sha256": plan["plan_sha256"],
        "apply_receipt_sha256": apply_receipt["receipt_sha256"],
        "apply_run_id": apply_receipt["run_id"],
        "destination_host_authority_id": plan["destination_host_authority_id"],
        "destination_binding": plan["destination_binding"],
        "selected_state_classes": plan["selected_state_classes"],
        "rollback_source_capture_id": apply_receipt["destination_preflight_capture_id"],
        "rollback_preflight_directory": os.fspath(preflight_directory),
        "rollback_preflight_capture_id": preflight_capture["capture_id"],
        "rollback_preflight_capture_sha256": preflight_capture["capture_sha256"],
        "rollback_post_directory": os.fspath(post_directory),
        "rollback_post_capture_id": post_capture["capture_id"],
        "rollback_post_capture_sha256": post_capture["capture_sha256"],
        "receipt_path": os.fspath(receipt_path),
        "journal": {
            "path": os.fspath(journal_path),
            "sha256": journal_sha256,
            "size": journal_size,
        },
        "auth": {
            "restored_sha256": plan["auth"]["destination_sha256"],
            "replaced_sha256": plan["auth"]["source_sha256"],
            "mutation_applied": True,
        },
        "sqlite_mutations": 0,
        "automatic_reinstall_performed": False,
        "offline_restored": True,
        "provider_runtime_acceptance_verified": False,
    }
    receipt["receipt_sha256"] = object_digest(receipt, "receipt_sha256")
    return receipt


def validate_codex_private_rollback_receipt(
    value: dict[str, Any],
    *,
    expected_receipt_path: Path | None = None,
) -> None:
    _require_exact_keys(
        value,
        {
            "schema",
            "rolled_back_at",
            "rollback_id",
            "quiescence_id",
            "quiescence_attestation",
            "bulkload_lock",
            "plan_sha256",
            "apply_receipt_sha256",
            "apply_run_id",
            "destination_host_authority_id",
            "destination_binding",
            "selected_state_classes",
            "rollback_source_capture_id",
            "rollback_preflight_directory",
            "rollback_preflight_capture_id",
            "rollback_preflight_capture_sha256",
            "rollback_post_directory",
            "rollback_post_capture_id",
            "rollback_post_capture_sha256",
            "receipt_path",
            "journal",
            "auth",
            "sqlite_mutations",
            "automatic_reinstall_performed",
            "offline_restored",
            "provider_runtime_acceptance_verified",
            "receipt_sha256",
        },
        "private rollback receipt",
    )
    if value["schema"] != PRIVATE_ROLLBACK_RECEIPT_SCHEMA:
        raise BulkloadError("unsupported private rollback receipt schema")
    if not isinstance(value["rolled_back_at"], str) or not value["rolled_back_at"]:
        raise BulkloadError("private rollback receipt timestamp is invalid")
    for key in (
        "rollback_id",
        "quiescence_id",
        "apply_run_id",
        "destination_host_authority_id",
        "rollback_source_capture_id",
        "rollback_preflight_capture_id",
        "rollback_post_capture_id",
    ):
        _require_uuid(value[key], f"private rollback receipt {key}")
    if (
        len(
            {
                value["rollback_source_capture_id"],
                value["rollback_preflight_capture_id"],
                value["rollback_post_capture_id"],
            }
        )
        != 3
    ):
        raise BulkloadError("private rollback receipt reused a capture ID")
    for key in (
        "plan_sha256",
        "apply_receipt_sha256",
        "rollback_preflight_capture_sha256",
        "rollback_post_capture_sha256",
        "receipt_sha256",
    ):
        _require_sha256(value[key], f"private rollback receipt {key}")
    selected = _canonical_state_classes(value["selected_state_classes"])
    destination_binding = _require_mapping(
        value["destination_binding"],
        "private rollback destination binding",
    )
    destination_codex_version = destination_binding.get("codex_version")
    codex_home, sqlite_home = _destination_quiescence_expectations(
        destination_binding,
        expected_host_authority_id=value["destination_host_authority_id"],
        expected_codex_version=destination_codex_version,
        expected_selected_state_classes=selected,
    )
    quiescence = _validate_quiescence_binding(
        value["quiescence_attestation"],
        value["bulkload_lock"],
        expected_purpose="rollback",
        expected_plan_sha256=value["plan_sha256"],
        expected_apply_receipt_sha256=value["apply_receipt_sha256"],
        expected_journal_sha256=None,
        expected_host_authority_id=value["destination_host_authority_id"],
        expected_codex_version=destination_codex_version,
        expected_selected_state_classes=selected,
        expected_codex_home=codex_home,
        expected_sqlite_home=sqlite_home,
        expected_operation_output_path=value["receipt_path"],
    )
    if value["quiescence_id"] != quiescence["attestation"]["attestation_id"]:
        raise BulkloadError("private rollback receipt quiescence ID differs")
    _validate_receipt_output_binding(
        value["receipt_path"],
        quiescence,
        expected_receipt_path=expected_receipt_path,
    )
    for key in (
        "rollback_preflight_directory",
        "rollback_post_directory",
    ):
        if not isinstance(value[key], str) or not Path(value[key]).is_absolute():
            raise BulkloadError("private rollback receipt path is invalid")
    journal = _require_mapping(value["journal"], "private rollback journal")
    _require_exact_keys(
        journal,
        {"path", "sha256", "size"},
        "private rollback journal",
    )
    if not isinstance(journal["path"], str) or not Path(journal["path"]).is_absolute():
        raise BulkloadError("private rollback journal path is invalid")
    _require_sha256(journal["sha256"], "private rollback journal digest")
    if not isinstance(journal["size"], int) or journal["size"] < 1:
        raise BulkloadError("private rollback journal size is invalid")
    auth = _require_mapping(value["auth"], "private rollback auth")
    _require_exact_keys(
        auth,
        {"restored_sha256", "replaced_sha256", "mutation_applied"},
        "private rollback auth",
    )
    _require_sha256(auth["restored_sha256"], "restored auth digest")
    _require_sha256(auth["replaced_sha256"], "replaced auth digest")
    if (
        auth["restored_sha256"] == auth["replaced_sha256"]
        or auth["mutation_applied"] is not True
        or value["sqlite_mutations"] != 0
        or value["automatic_reinstall_performed"] is not False
        or value["offline_restored"] is not True
        or value["provider_runtime_acceptance_verified"] is not False
    ):
        raise BulkloadError("private rollback receipt claim is invalid")
    if object_digest(value, "receipt_sha256") != value["receipt_sha256"]:
        raise BulkloadError("private rollback receipt digest mismatch")


def _validate_rollback_receipt_against_plan(
    receipt: dict[str, Any],
    plan: dict[str, Any],
    apply_receipt: dict[str, Any],
    *,
    expected_receipt_path: Path | None = None,
) -> None:
    validate_codex_private_rollback_receipt(
        receipt,
        expected_receipt_path=expected_receipt_path,
    )
    if (
        receipt["plan_sha256"] != plan["plan_sha256"]
        or receipt["apply_receipt_sha256"] != apply_receipt["receipt_sha256"]
        or receipt["apply_run_id"] != apply_receipt["run_id"]
        or receipt["destination_host_authority_id"]
        != plan["destination_host_authority_id"]
        or receipt["destination_binding"] != plan["destination_binding"]
        or receipt["selected_state_classes"] != plan["selected_state_classes"]
        or receipt["rollback_source_capture_id"]
        != apply_receipt["destination_preflight_capture_id"]
        or receipt["auth"]["restored_sha256"] != plan["auth"]["destination_sha256"]
        or receipt["auth"]["replaced_sha256"] != plan["auth"]["source_sha256"]
    ):
        raise BulkloadError("private rollback receipt differs from accepted plan")


def read_codex_private_rollback_receipt(path: Path) -> dict[str, Any]:
    value = private_state.read_private_json_document(
        path,
        label="private rollback receipt",
    )
    validate_codex_private_rollback_receipt(
        value,
        expected_receipt_path=path,
    )
    return value


def rollback_codex_private_install(
    install_plan_path: Path,
    compatibility_plan_path: Path,
    source_directory: Path,
    destination_before_directory: Path,
    apply_receipt_path: Path,
    rollback_directory: Path,
    *,
    destination_codex_home: Path,
    destination_sqlite_home: Path | None,
    preflight_capture_directory: Path,
    post_capture_directory: Path,
    recovery_capture_directory: Path,
    journal_path: Path,
    receipt_path: Path,
    accept_plan: str,
    accept_apply_receipt: str,
    destination_host_authority_id: str,
    codex_version: str,
    quiescence_attestation_path: Path,
    accept_quiescence_attestation: str,
    acknowledge_private_rollback: bool,
    acknowledge_no_post_apply_writes: bool,
) -> dict[str, Any]:
    """Restore the captured destination auth if all selected state is unchanged."""
    if os.geteuid() == 0:
        raise BulkloadError("private Codex rollback refuses root")
    if not acknowledge_private_rollback:
        raise BulkloadError("private rollback requires explicit acknowledgement")
    if not acknowledge_no_post_apply_writes:
        raise BulkloadError("private rollback requires a no-post-apply-write claim")

    plan = read_codex_private_install_plan(install_plan_path)
    compatibility = private_state.read_codex_private_state_plan(compatibility_plan_path)
    apply_receipt = read_codex_private_apply_receipt(apply_receipt_path)
    if plan["plan_sha256"] != accept_plan:
        raise BulkloadError("accepted private install plan digest differs")
    if apply_receipt["receipt_sha256"] != accept_apply_receipt:
        raise BulkloadError("accepted private apply receipt digest differs")
    if (
        destination_host_authority_id != plan["destination_host_authority_id"]
        or codex_version != plan["codex_version"]
    ):
        raise BulkloadError("private rollback destination acknowledgement differs")
    if not plan["auth"]["mutates"]:
        raise BulkloadError("private install receipt has no auth mutation to roll back")
    validate_codex_private_install_plan_against_inputs(
        plan,
        compatibility,
        source_directory,
        destination_before_directory,
    )
    _validate_apply_receipt_against_plan(
        apply_receipt,
        plan,
        expected_receipt_path=apply_receipt_path,
    )
    rollback_capture, rollback_root = _validate_apply_recovery_evidence(
        apply_receipt,
        rollback_directory=rollback_directory,
    )
    source_capture, source_root = private_state.read_codex_private_bundle(
        source_directory,
        "source",
    )
    destination_before, destination_before_root = (
        private_state.read_codex_private_bundle(
            destination_before_directory,
            "destination",
        )
    )
    codex_root, codex_info = private_state._resolve_private_directory(
        destination_codex_home,
        "destination Codex home",
    )
    if (
        os.fspath(codex_root)
        != plan["destination_binding"]["codex_home"]["resolved_path"]
        or private_state._directory_identity_record(codex_info)
        != plan["destination_binding"]["codex_home"]["identity"]
    ):
        raise BulkloadError("private rollback destination Codex binding differs")
    sqlite_root: Path | None = None
    sqlite_info: os.stat_result | None = None
    if "sqlite" in plan["selected_state_classes"]:
        if destination_sqlite_home is None:
            raise BulkloadError("private rollback SQLite home is required")
        sqlite_root, sqlite_info = private_state._resolve_private_directory(
            destination_sqlite_home,
            "destination SQLite home",
        )
        if (
            os.fspath(sqlite_root)
            != plan["destination_binding"]["sqlite_home"]["resolved_path"]
            or private_state._directory_identity_record(sqlite_info)
            != plan["destination_binding"]["sqlite_home"]["identity"]
        ):
            raise BulkloadError("private rollback SQLite binding differs")
    elif destination_sqlite_home is not None:
        raise BulkloadError("private rollback SQLite home was not selected")

    outputs = _validate_output_topology(
        {
            "rollback preflight capture": preflight_capture_directory,
            "rollback post capture": post_capture_directory,
            "rollback recovery capture": recovery_capture_directory,
            "rollback journal": journal_path,
            "rollback receipt": receipt_path,
        },
        {
            "source bundle": source_root,
            "destination-before bundle": destination_before_root,
            "rollback bundle": rollback_root,
            "apply receipt": apply_receipt_path,
            "apply post capture": Path(apply_receipt["post_capture_directory"]),
            "apply journal": Path(apply_receipt["journal"]["path"]),
            "destination Codex home": codex_root,
            **(
                {"destination SQLite home": sqlite_root}
                if sqlite_root is not None
                else {}
            ),
        },
        recorded_protected=_recorded_live_roots(compatibility),
    )
    preflight_target = outputs["rollback preflight capture"]
    post_target = outputs["rollback post capture"]
    recovery_target = outputs["rollback recovery capture"]
    journal_target = outputs["rollback journal"]
    receipt_target = outputs["rollback receipt"]
    output_parent_authorities = _output_parent_authorities(outputs)

    runtime_context, bulkload_lock, quiescence_context = _open_operation_quiescence(
        quiescence_attestation_path,
        accept_attestation=accept_quiescence_attestation,
        purpose="rollback",
        plan=plan,
        codex_home=codex_root,
        sqlite_home=sqlite_root,
        operation_output=receipt_target,
        apply_receipt_sha256=apply_receipt["receipt_sha256"],
    )
    try:
        if quiescence_context.attestation_id == apply_receipt["quiescence_id"]:
            raise BulkloadError(
                "manual rollback requires a fresh quiescence attestation"
            )
        quiescence_attestation, bulkload_lock_record = _operation_quiescence_records(
            quiescence_attestation_path,
            quiescence_context,
            bulkload_lock,
        )
        capture_quiescence = private_state.private_quiescence_capture_record(
            quiescence_context.value
        )
        _revalidate_output_parent_authorities(output_parent_authorities)
        preflight_capture = _capture_destination(
            plan,
            destination_before,
            codex_home=codex_root,
            sqlite_home=sqlite_root,
            output_directory=preflight_target,
            quiescence=capture_quiescence,
            protected_directories=(
                source_root,
                destination_before_root,
                rollback_root,
                Path(apply_receipt["post_capture_directory"]),
            ),
            recorded_protected_directories=_recorded_live_roots(compatibility),
        )
        _revalidate_output_parent_authorities(output_parent_authorities)
        validate_destination_capture_for_install(
            preflight_capture,
            plan,
            expected_auth_sha256=_fresh_capture_expected_auth(
                plan,
                installed=True,
            ),
            forbidden_capture_ids={
                plan["source_capture_id"],
                plan["destination_before_capture_id"],
                apply_receipt["destination_preflight_capture_id"],
                apply_receipt["destination_post_capture_id"],
            },
        )
    except BaseException:
        quiescence_context.close()
        bulkload_lock.close()
        runtime_context.close()
        raise
    quiescence_id = quiescence_context.attestation_id

    descriptors: list[int] = []
    journal_descriptor = -1
    rollback_committed = False
    rollback_may_have_committed = False
    reinstall_completed = False
    rollback_id = str(uuid.uuid4())
    rollback_auth = rollback_capture["auth"]
    source_auth = source_capture["auth"]
    if rollback_auth is None or source_auth is None:
        raise BulkloadError("private rollback auth evidence is incomplete")
    restore_payload: bytes | None = None
    source_payload: bytes | None = None
    destination_descriptor = -1
    restore_staged_identity: dict[str, int] | None = None
    restored_auth_identity: dict[str, int] | None = None
    receipt_publication = {"attempted": False, "published": False}
    receipt_staging_leaf = _receipt_staging_name(rollback_id)
    try:
        apply_journal_pinned = _open_pinned_apply_journal(
            apply_receipt,
            plan,
        )
        descriptors.extend([apply_journal_pinned[0], apply_journal_pinned[1]])
        rollback_pinned = _open_pinned_auth(
            rollback_root,
            rollback_auth,
            "rollback private bundle",
        )
        source_pinned = _open_pinned_auth(
            source_root,
            source_auth,
            "source private bundle",
        )
        descriptors.extend(
            [
                rollback_pinned[0],
                rollback_pinned[1],
                source_pinned[0],
                source_pinned[1],
            ]
        )
        restore_payload = rollback_pinned[3]
        source_payload = source_pinned[3]
        destination_descriptor = private_state._open_private_directory_descriptor(
            codex_root,
            codex_info,
            "destination Codex home",
        )
        descriptors.append(destination_descriptor)
        sqlite_descriptor = -1
        if sqlite_root is not None and sqlite_info is not None:
            sqlite_descriptor = private_state._open_private_directory_descriptor(
                sqlite_root,
                sqlite_info,
                "destination SQLite home",
            )
            descriptors.append(sqlite_descriptor)
        _assert_live_auth(
            destination_descriptor,
            expected_sha256=source_auth["sha256"],
            expected_size=source_auth["size"],
            expected_identity=preflight_capture["auth"]["source_identity"],
            label="destination auth before manual rollback",
        )

        staging_name = _auth_staging_name(rollback_id, "manual-rollback")
        prepared = {
            "schema": PRIVATE_JOURNAL_SCHEMA,
            "operation": "rollback",
            "event": "rollback-prepared",
            "recorded_at": utc_now(),
            "rollback_id": rollback_id,
            "quiescence_id": quiescence_id,
            "quiescence_attestation": quiescence_attestation,
            "bulkload_lock": bulkload_lock_record,
            "plan_sha256": plan["plan_sha256"],
            "compatibility_plan_sha256": plan["compatibility_plan_sha256"],
            "apply_receipt_sha256": apply_receipt["receipt_sha256"],
            "destination_host_authority_id": plan["destination_host_authority_id"],
            "destination_codex_home": os.fspath(codex_root),
            "destination_codex_home_identity": (
                private_state._directory_identity_record(codex_info)
            ),
            "destination_sqlite_home": (
                os.fspath(sqlite_root) if sqlite_root is not None else None
            ),
            "destination_sqlite_home_identity": (
                private_state._directory_identity_record(sqlite_info)
                if sqlite_info is not None
                else None
            ),
            "selected_state_classes": plan["selected_state_classes"],
            "codex_version": plan["codex_version"],
            "install_plan_path": os.fspath(install_plan_path.expanduser().resolve()),
            "compatibility_plan_path": os.fspath(
                compatibility_plan_path.expanduser().resolve()
            ),
            "source_directory": os.fspath(source_root),
            "destination_before_directory": os.fspath(destination_before_root),
            "apply_receipt_path": os.fspath(apply_receipt_path.expanduser().resolve()),
            "rollback_source_directory": os.fspath(rollback_root),
            "rollback_preflight_directory": os.fspath(preflight_target),
            "rollback_preflight_capture_id": preflight_capture["capture_id"],
            "rollback_preflight_capture_sha256": preflight_capture["capture_sha256"],
            "post_capture_directory": os.fspath(post_target),
            "recovery_capture_directory": os.fspath(recovery_target),
            "journal_path": os.fspath(journal_target),
            "receipt_path": os.fspath(receipt_target),
            "receipt_staging_leaf": receipt_staging_leaf,
            "output_parent_authorities": output_parent_authorities,
            "auth_before_sha256": source_auth["sha256"],
            "auth_before_size": source_auth["size"],
            "auth_after_sha256": rollback_auth["sha256"],
            "auth_after_size": rollback_auth["size"],
            "auth_staging_leaf": staging_name,
            "sqlite_mutations": 0,
        }
        _revalidate_output_parent_authorities(output_parent_authorities)
        (
            journal_descriptor,
            journal_parent_descriptor,
            journal_target,
            _,
        ) = _create_journal(journal_target, prepared)
        descriptors.extend([journal_descriptor, journal_parent_descriptor])
        _stage_auth(
            destination_descriptor,
            restore_payload,
            staging_name=staging_name,
        )
        staged_info = os.stat(
            staging_name,
            dir_fd=destination_descriptor,
            follow_symlinks=False,
        )
        restore_staged_identity = private_state._identity(staged_info)
        _append_journal(
            journal_descriptor,
            {
                "event": "rollback-mutation-intent",
                "recorded_at": utc_now(),
                "rollback_id": rollback_id,
                "staging_leaf": staging_name,
                "staging_sha256": rollback_auth["sha256"],
                "staging_size": rollback_auth["size"],
                "staging_identity": restore_staged_identity,
            },
        )
        _read_descriptor_payload(
            rollback_pinned[1],
            rollback_auth,
            "rollback auth immediately before restore",
        )
        _assert_live_auth(
            destination_descriptor,
            expected_sha256=source_auth["sha256"],
            expected_size=source_auth["size"],
            expected_identity=preflight_capture["auth"]["source_identity"],
            label="destination auth immediately before manual rollback",
        )
        apply_journal_sha256, apply_journal_size = _journal_digest(
            apply_journal_pinned[0],
            apply_journal_pinned[1],
            Path(apply_receipt["journal"]["path"]).name,
        )
        if (
            apply_journal_sha256 != apply_receipt["journal"]["sha256"]
            or apply_journal_size != apply_receipt["journal"]["size"]
        ):
            raise BulkloadError("private apply journal changed before rollback commit")
        quiescence_context.revalidate(
            require_operation_output_absent=True,
        )
        quiescence_context.assert_bulkload_lock(bulkload_lock)
        runtime_context.revalidate()
        rollback_may_have_committed = True
        _commit_staged_auth(destination_descriptor, staging_name)
        rollback_committed = True
        private_state._fsync_directory_descriptor(destination_descriptor)
        _assert_live_auth(
            destination_descriptor,
            expected_sha256=rollback_auth["sha256"],
            expected_size=rollback_auth["size"],
            expected_identity=None,
            label="destination auth after manual rollback",
        )
        restored_record, _ = _live_auth_record(
            destination_descriptor,
            "destination auth restored by manual rollback",
        )
        if restored_record["identity"] != restore_staged_identity:
            raise BulkloadError("restored destination auth identity is invalid")
        restored_auth_identity = restored_record["identity"]
        quiescence_context.revalidate(
            require_operation_output_absent=True,
            require_observed_state=False,
        )
        bulkload_lock.revalidate()
        runtime_context.revalidate()
        _append_journal(
            journal_descriptor,
            {
                "event": "auth-restored",
                "recorded_at": utc_now(),
                "rollback_id": rollback_id,
                "auth_sha256": rollback_auth["sha256"],
            },
        )
        _revalidate_output_parent_authorities(output_parent_authorities)
        post_capture = _capture_destination(
            plan,
            destination_before,
            codex_home=codex_root,
            sqlite_home=sqlite_root,
            output_directory=post_target,
            quiescence=capture_quiescence,
            protected_directories=(
                source_root,
                destination_before_root,
                rollback_root,
                preflight_target,
                Path(apply_receipt["post_capture_directory"]),
            ),
            recorded_protected_directories=_recorded_live_roots(compatibility),
        )
        _revalidate_output_parent_authorities(output_parent_authorities)
        validate_destination_capture_for_install(
            post_capture,
            plan,
            expected_auth_sha256=rollback_auth["sha256"],
            forbidden_capture_ids={
                plan["source_capture_id"],
                plan["destination_before_capture_id"],
                apply_receipt["destination_preflight_capture_id"],
                apply_receipt["destination_post_capture_id"],
                preflight_capture["capture_id"],
            },
        )
        _append_journal(
            journal_descriptor,
            {
                "event": "rollback-offline-state-verified",
                "recorded_at": utc_now(),
                "rollback_id": rollback_id,
                "post_capture_id": post_capture["capture_id"],
                "post_capture_sha256": post_capture["capture_sha256"],
                "provider_runtime_acceptance_verified": False,
            },
        )
        _append_journal(
            journal_descriptor,
            {
                "event": "rollback-receipt-frozen",
                "recorded_at": utc_now(),
                "rollback_id": rollback_id,
                "receipt_path": os.fspath(receipt_target),
                "receipt_staging_leaf": receipt_staging_leaf,
            },
        )
        journal_sha256, journal_size = _journal_digest(
            journal_descriptor,
            journal_parent_descriptor,
            journal_target.name,
        )
        receipt = _build_rollback_receipt(
            plan,
            apply_receipt,
            rollback_id=rollback_id,
            quiescence_id=quiescence_id,
            quiescence_attestation=quiescence_attestation,
            bulkload_lock=bulkload_lock_record,
            preflight_directory=preflight_target,
            preflight_capture=preflight_capture,
            post_directory=post_target,
            post_capture=post_capture,
            journal_path=journal_target,
            journal_sha256=journal_sha256,
            journal_size=journal_size,
            receipt_path=receipt_target,
        )
        _validate_rollback_receipt_against_plan(
            receipt,
            plan,
            apply_receipt,
            expected_receipt_path=receipt_target,
        )
        quiescence_context.revalidate(
            require_operation_output_absent=True,
            require_observed_state=False,
        )
        quiescence_context.assert_bulkload_lock(bulkload_lock)
        runtime_context.revalidate()
        _revalidate_output_parent_authorities(output_parent_authorities)
        _publish_mutation_receipt(
            receipt_target,
            receipt,
            staging_leaf=receipt_staging_leaf,
            protected_directories=(
                source_root,
                destination_before_root,
                rollback_root,
                preflight_target,
                post_target,
            ),
            recorded_protected_directories=_recorded_live_roots(compatibility),
            commit_state=receipt_publication,
        )
        quiescence_context.revalidate(require_observed_state=False)
        bulkload_lock.revalidate()
        runtime_context.revalidate()
        _revalidate_output_parent_authorities(output_parent_authorities)
        return receipt
    except BaseException as error:
        reinstall_error: BaseException | None = None
        receipt_outcome = (
            _classify_mutation_receipt_publication(
                receipt_target,
                receipt,
                output_parent_authorities["rollback receipt"],
            )
            if receipt_publication["attempted"] and "receipt" in locals()
            else "absent"
        )
        if receipt_publication["published"] or receipt_outcome == "exact":
            raise BulkloadError(
                "private Codex rollback receipt reached its no-replace commit point; "
                "live rolled-back state was retained and must not be inverted "
                f"({error}); receipt={receipt_target} journal={journal_target}"
            ) from error
        if receipt_outcome == "unknown":
            raise BulkloadError(
                "private Codex rollback receipt publication is indeterminate; "
                "live rolled-back state was retained for typed recovery "
                f"({error}); receipt={receipt_target} journal={journal_target}"
            ) from error
        if rollback_may_have_committed and not rollback_committed:
            try:
                observed, _ = _live_auth_record(
                    destination_descriptor,
                    "destination auth after uncertain rollback commit",
                )
                if (
                    observed["sha256"] == rollback_auth["sha256"]
                    and observed["size"] == rollback_auth["size"]
                    and restore_staged_identity is not None
                    and observed["identity"] == restore_staged_identity
                ):
                    rollback_committed = True
                    restored_auth_identity = restore_staged_identity
                elif (
                    observed["sha256"] == source_auth["sha256"]
                    and observed["size"] == source_auth["size"]
                    and observed["identity"]
                    == preflight_capture["auth"]["source_identity"]
                ):
                    rollback_committed = False
                else:
                    reinstall_error = BulkloadError(
                        "uncertain rollback commit left an unclassified auth identity"
                    )
            except BaseException as caught:
                reinstall_error = caught
        if rollback_committed:
            try:
                if journal_descriptor < 0:
                    raise BulkloadError("private rollback journal is not pinned")
                assert source_payload is not None
                _assert_live_auth(
                    destination_descriptor,
                    expected_sha256=rollback_auth["sha256"],
                    expected_size=rollback_auth["size"],
                    expected_identity=restored_auth_identity,
                    label="destination auth before automatic reinstall",
                )
                staging_name = _auth_staging_name(
                    rollback_id,
                    "automatic-reinstall",
                )
                _append_journal(
                    journal_descriptor,
                    {
                        "event": "automatic-reinstall-staging",
                        "recorded_at": utc_now(),
                        "rollback_id": rollback_id,
                        "staging_leaf": staging_name,
                        "staging_sha256": source_auth["sha256"],
                        "staging_size": source_auth["size"],
                    },
                )
                _stage_auth(
                    destination_descriptor,
                    source_payload,
                    staging_name=staging_name,
                )
                reinstall_staging_info = os.stat(
                    staging_name,
                    dir_fd=destination_descriptor,
                    follow_symlinks=False,
                )
                reinstall_staging_identity = private_state._identity(
                    reinstall_staging_info
                )
                _append_journal(
                    journal_descriptor,
                    {
                        "event": "automatic-reinstall-mutation-intent",
                        "recorded_at": utc_now(),
                        "rollback_id": rollback_id,
                        "staging_leaf": staging_name,
                        "staging_sha256": source_auth["sha256"],
                        "staging_size": source_auth["size"],
                        "staging_identity": reinstall_staging_identity,
                    },
                )
                _read_descriptor_payload(
                    source_pinned[1],
                    source_auth,
                    "source auth immediately before automatic reinstall",
                )
                _assert_live_auth(
                    destination_descriptor,
                    expected_sha256=rollback_auth["sha256"],
                    expected_size=rollback_auth["size"],
                    expected_identity=restored_auth_identity,
                    label="destination auth at automatic reinstall commit",
                )
                quiescence_context.revalidate(
                    require_operation_output_absent=True,
                    require_observed_state=False,
                )
                quiescence_context.assert_bulkload_lock(bulkload_lock)
                # Preserve the same compensation rule as apply: an authority
                # drift after rollback must not prevent restoring the accepted
                # applied bytes. This path always exits with a failed rollback
                # and never publishes a success receipt.
                _commit_staged_auth(destination_descriptor, staging_name)
                rollback_committed = False
                private_state._fsync_directory_descriptor(destination_descriptor)
                _assert_live_auth(
                    destination_descriptor,
                    expected_sha256=source_auth["sha256"],
                    expected_size=source_auth["size"],
                    expected_identity=reinstall_staging_identity,
                    label="destination auth after automatic reinstall",
                )
                _revalidate_output_parent_authorities(output_parent_authorities)
                recovery_capture = _capture_destination(
                    plan,
                    destination_before,
                    codex_home=codex_root,
                    sqlite_home=sqlite_root,
                    output_directory=recovery_target,
                    quiescence=capture_quiescence,
                    protected_directories=(
                        source_root,
                        destination_before_root,
                        rollback_root,
                        preflight_target,
                        Path(apply_receipt["post_capture_directory"]),
                    ),
                    recorded_protected_directories=(
                        *_recorded_live_roots(compatibility),
                        post_target,
                    ),
                )
                _revalidate_output_parent_authorities(output_parent_authorities)
                validate_destination_capture_for_install(
                    recovery_capture,
                    plan,
                    expected_auth_sha256=source_auth["sha256"],
                    forbidden_capture_ids={
                        plan["source_capture_id"],
                        plan["destination_before_capture_id"],
                        apply_receipt["destination_preflight_capture_id"],
                        apply_receipt["destination_post_capture_id"],
                        preflight_capture["capture_id"],
                    },
                )
                _append_journal(
                    journal_descriptor,
                    {
                        "event": "automatic-reinstall-verified",
                        "recorded_at": utc_now(),
                        "rollback_id": rollback_id,
                        "recovery_capture_id": recovery_capture["capture_id"],
                        "recovery_capture_sha256": recovery_capture["capture_sha256"],
                    },
                )
                reinstall_completed = True
            except BaseException as caught:
                reinstall_error = caught
        detail = (
            f"; automatic reinstall failed: {reinstall_error}"
            if reinstall_error is not None
            else (
                "; automatic reinstall restored applied state"
                if reinstall_completed
                else ""
            )
        )
        raise BulkloadError(
            f"private Codex rollback failed ({error}){detail}; journal={journal_target}"
        ) from error
    finally:
        _close_descriptors(descriptors)
        quiescence_context.close()
        bulkload_lock.close()
        runtime_context.close()


def recover_codex_private_mutation(
    install_plan_path: Path,
    compatibility_plan_path: Path,
    source_directory: Path,
    destination_before_directory: Path,
    journal_path: Path,
    *,
    apply_receipt_path: Path | None,
    destination_codex_home: Path,
    destination_sqlite_home: Path | None,
    preflight_capture_directory: Path,
    post_capture_directory: Path,
    receipt_path: Path,
    accept_plan: str,
    accept_journal: str,
    accept_apply_receipt: str | None,
    destination_host_authority_id: str,
    codex_version: str,
    quiescence_attestation_path: Path,
    accept_quiescence_attestation: str,
    acknowledge_private_recovery: bool,
) -> dict[str, Any]:
    """Hold the full cooperating lock while recovering one private mutation."""
    if os.geteuid() == 0:
        raise BulkloadError("private Codex recovery refuses root")
    if not acknowledge_private_recovery:
        raise BulkloadError("private recovery requires explicit acknowledgement")
    plan = read_codex_private_install_plan(install_plan_path)
    if plan["plan_sha256"] != accept_plan:
        raise BulkloadError("accepted private install plan digest differs")
    if (
        destination_host_authority_id != plan["destination_host_authority_id"]
        or codex_version != plan["codex_version"]
    ):
        raise BulkloadError("private recovery destination acknowledgement differs")
    runtime_context, bulkload_lock, quiescence_context = _open_operation_quiescence(
        quiescence_attestation_path,
        accept_attestation=accept_quiescence_attestation,
        purpose="recover",
        plan=plan,
        codex_home=destination_codex_home,
        sqlite_home=destination_sqlite_home,
        operation_output=receipt_path,
        apply_receipt_sha256=accept_apply_receipt,
        journal_sha256=accept_journal,
    )
    try:
        return _recover_codex_private_mutation_locked(
            install_plan_path,
            compatibility_plan_path,
            source_directory,
            destination_before_directory,
            journal_path,
            apply_receipt_path=apply_receipt_path,
            destination_codex_home=destination_codex_home,
            destination_sqlite_home=destination_sqlite_home,
            preflight_capture_directory=preflight_capture_directory,
            post_capture_directory=post_capture_directory,
            receipt_path=receipt_path,
            accept_plan=accept_plan,
            accept_journal=accept_journal,
            accept_apply_receipt=accept_apply_receipt,
            destination_host_authority_id=destination_host_authority_id,
            codex_version=codex_version,
            quiescence_attestation_path=quiescence_attestation_path,
            quiescence_context=quiescence_context,
            bulkload_lock=bulkload_lock,
            runtime_context=runtime_context,
        )
    finally:
        quiescence_context.close()
        bulkload_lock.close()
        runtime_context.close()


def _recover_codex_private_mutation_locked(
    install_plan_path: Path,
    compatibility_plan_path: Path,
    source_directory: Path,
    destination_before_directory: Path,
    journal_path: Path,
    *,
    apply_receipt_path: Path | None,
    destination_codex_home: Path,
    destination_sqlite_home: Path | None,
    preflight_capture_directory: Path,
    post_capture_directory: Path,
    receipt_path: Path,
    accept_plan: str,
    accept_journal: str,
    accept_apply_receipt: str | None,
    destination_host_authority_id: str,
    codex_version: str,
    quiescence_attestation_path: Path,
    quiescence_context: (private_quiescence.PinnedCodexPrivateQuiescenceAttestation),
    bulkload_lock: private_quiescence.CodexPrivateBulkloadLock,
    runtime_context: private_runtime.PinnedPrivateRuntimeAuthority,
) -> dict[str, Any]:
    """Converge an unreceipted auth mutation under pinned live authorities."""
    plan = read_codex_private_install_plan(install_plan_path)
    compatibility = private_state.read_codex_private_state_plan(compatibility_plan_path)
    if plan["plan_sha256"] != accept_plan:
        raise BulkloadError("accepted private install plan digest differs")
    if not plan["ready_for_apply"] or plan["blockers"] or not plan["auth"]["mutates"]:
        raise BulkloadError("private recovery requires one ready auth-mutating plan")
    if (
        destination_host_authority_id != plan["destination_host_authority_id"]
        or codex_version != plan["codex_version"]
    ):
        raise BulkloadError("private recovery destination acknowledgement differs")
    validate_codex_private_install_plan_against_inputs(
        plan,
        compatibility,
        source_directory,
        destination_before_directory,
    )
    quiescence_attestation, bulkload_lock_record = _operation_quiescence_records(
        quiescence_attestation_path,
        quiescence_context,
        bulkload_lock,
    )
    quiescence_id = quiescence_context.attestation_id
    capture_quiescence = private_state.private_quiescence_capture_record(
        quiescence_context.value
    )

    journal_pinned = _open_pinned_private_journal(
        journal_path,
        accept_sha256=accept_journal,
    )
    events = journal_pinned[4]
    prepared = events[0]
    try:
        operation_id = _validate_recovery_prepared_event(
            prepared,
            plan=plan,
            journal_path=journal_path,
        )
        operation = prepared["operation"]
        prior_quiescence_ids = {
            event.get("quiescence_id")
            for event in events
            if isinstance(event.get("quiescence_id"), str)
        }
        if quiescence_id in prior_quiescence_ids:
            raise BulkloadError("private recovery requires a fresh quiescence ID")
        quiescence_context.revalidate(
            require_operation_output_absent=True,
        )
        quiescence_context.assert_bulkload_lock(bulkload_lock)
    except BaseException:
        _close_descriptors([journal_pinned[0], journal_pinned[1]])
        raise
    descriptors: list[int] = [journal_pinned[0], journal_pinned[1]]
    events = journal_pinned[4]

    source_capture, source_root = private_state.read_codex_private_bundle(
        source_directory,
        "source",
    )
    destination_before, destination_before_root = (
        private_state.read_codex_private_bundle(
            destination_before_directory,
            "destination",
        )
    )
    exact_inputs = {
        "install_plan_path": install_plan_path,
        "compatibility_plan_path": compatibility_plan_path,
        "source_directory": source_root,
        "destination_before_directory": destination_before_root,
    }
    for key, path in exact_inputs.items():
        if os.fspath(path.expanduser().resolve()) != prepared[key]:
            _close_descriptors(descriptors)
            raise BulkloadError(f"private recovery journal {key} differs")

    codex_root, codex_info = private_state._resolve_private_directory(
        destination_codex_home,
        "destination Codex home",
    )
    if (
        os.fspath(codex_root) != prepared["destination_codex_home"]
        or private_state._directory_identity_record(codex_info)
        != prepared["destination_codex_home_identity"]
        or os.fspath(codex_root)
        != plan["destination_binding"]["codex_home"]["resolved_path"]
        or private_state._directory_identity_record(codex_info)
        != plan["destination_binding"]["codex_home"]["identity"]
    ):
        _close_descriptors(descriptors)
        raise BulkloadError("private recovery destination Codex binding differs")
    sqlite_root: Path | None = None
    sqlite_info: os.stat_result | None = None
    if "sqlite" in plan["selected_state_classes"]:
        if destination_sqlite_home is None:
            _close_descriptors(descriptors)
            raise BulkloadError("private recovery SQLite home is required")
        sqlite_root, sqlite_info = private_state._resolve_private_directory(
            destination_sqlite_home,
            "destination SQLite home",
        )
        if (
            os.fspath(sqlite_root) != prepared["destination_sqlite_home"]
            or os.fspath(sqlite_root)
            != plan["destination_binding"]["sqlite_home"]["resolved_path"]
            or private_state._directory_identity_record(sqlite_info)
            != plan["destination_binding"]["sqlite_home"]["identity"]
        ):
            _close_descriptors(descriptors)
            raise BulkloadError("private recovery SQLite binding differs")
    elif destination_sqlite_home is not None:
        _close_descriptors(descriptors)
        raise BulkloadError("private recovery SQLite home was not selected")

    apply_receipt: dict[str, Any] | None = None
    rollback_capture: dict[str, Any]
    rollback_root: Path
    before_auth: dict[str, Any]
    after_auth: dict[str, Any]
    restore_auth: dict[str, Any]
    restore_root: Path
    before_identity: dict[str, int]
    apply_receipt_sha256: str | None = None
    if operation == "apply":
        if apply_receipt_path is not None or accept_apply_receipt is not None:
            _close_descriptors(descriptors)
            raise BulkloadError("apply recovery must not accept an apply receipt")
        rollback_capture, rollback_root = private_state.read_codex_private_bundle(
            Path(prepared["rollback_directory"]),
            "destination",
        )
        if (
            rollback_capture["capture_id"] != prepared["rollback_capture_id"]
            or rollback_capture["capture_sha256"] != prepared["rollback_capture_sha256"]
        ):
            _close_descriptors(descriptors)
            raise BulkloadError("private apply recovery rollback bundle differs")
        validate_destination_capture_for_install(
            rollback_capture,
            plan,
            expected_auth_sha256=plan["auth"]["destination_sha256"],
            forbidden_capture_ids={
                plan["source_capture_id"],
                plan["destination_before_capture_id"],
            },
        )
        before_auth = rollback_capture["auth"]
        after_auth = source_capture["auth"]
        restore_auth = before_auth
        restore_root = rollback_root
        before_identity = before_auth["source_identity"]
    else:
        if apply_receipt_path is None or accept_apply_receipt is None:
            _close_descriptors(descriptors)
            raise BulkloadError("rollback recovery requires its apply receipt")
        apply_receipt = read_codex_private_apply_receipt(apply_receipt_path)
        if (
            apply_receipt["receipt_sha256"] != accept_apply_receipt
            or apply_receipt["receipt_sha256"] != prepared["apply_receipt_sha256"]
            or os.fspath(apply_receipt_path.expanduser().resolve())
            != prepared["apply_receipt_path"]
        ):
            _close_descriptors(descriptors)
            raise BulkloadError("private rollback recovery apply receipt differs")
        _validate_apply_receipt_against_plan(
            apply_receipt,
            plan,
            expected_receipt_path=apply_receipt_path,
        )
        apply_receipt_sha256 = apply_receipt["receipt_sha256"]
        rollback_capture, rollback_root = _validate_apply_recovery_evidence(
            apply_receipt,
            rollback_directory=Path(prepared["rollback_source_directory"]),
        )
        rollback_preflight, rollback_preflight_root = (
            private_state.read_codex_private_bundle(
                Path(prepared["rollback_preflight_directory"]),
                "destination",
            )
        )
        if (
            rollback_preflight["capture_id"]
            != prepared["rollback_preflight_capture_id"]
            or rollback_preflight["capture_sha256"]
            != prepared["rollback_preflight_capture_sha256"]
        ):
            _close_descriptors(descriptors)
            raise BulkloadError("private rollback recovery preflight differs")
        validate_destination_capture_for_install(
            rollback_preflight,
            plan,
            expected_auth_sha256=plan["auth"]["source_sha256"],
            forbidden_capture_ids={
                plan["source_capture_id"],
                plan["destination_before_capture_id"],
                apply_receipt["destination_preflight_capture_id"],
                apply_receipt["destination_post_capture_id"],
            },
        )
        before_auth = rollback_preflight["auth"]
        after_auth = rollback_capture["auth"]
        restore_auth = source_capture["auth"]
        restore_root = source_root
        before_identity = before_auth["source_identity"]
    if (
        before_auth is None
        or after_auth is None
        or restore_auth is None
        or before_auth["sha256"] != prepared["auth_before_sha256"]
        or before_auth["size"] != prepared["auth_before_size"]
        or after_auth["sha256"] != prepared["auth_after_sha256"]
        or after_auth["size"] != prepared["auth_after_size"]
    ):
        _close_descriptors(descriptors)
        raise BulkloadError("private recovery auth evidence differs from journal")

    _revalidate_output_parent_authorities(prepared["output_parent_authorities"])
    original_receipt_path = Path(prepared["receipt_path"])
    original_receipt: dict[str, Any] | None = None
    if os.path.lexists(original_receipt_path):
        try:
            if operation == "apply":
                original_receipt = read_codex_private_apply_receipt(
                    original_receipt_path
                )
                _validate_apply_receipt_against_plan(
                    original_receipt,
                    plan,
                    expected_receipt_path=original_receipt_path,
                )
                if original_receipt["run_id"] != operation_id:
                    raise BulkloadError("private apply receipt operation ID differs")
            else:
                original_receipt = read_codex_private_rollback_receipt(
                    original_receipt_path
                )
                _validate_rollback_receipt_against_plan(
                    original_receipt,
                    plan,
                    apply_receipt,
                    expected_receipt_path=original_receipt_path,
                )
                if (
                    original_receipt["rollback_id"] != operation_id
                    or original_receipt["apply_receipt_sha256"] != apply_receipt_sha256
                ):
                    raise BulkloadError("private rollback receipt authority differs")
            if (
                original_receipt["journal"]["path"] != prepared["journal_path"]
                or original_receipt["journal"]["sha256"] != accept_journal
                or original_receipt["journal"]["size"] != journal_pinned[5]
            ):
                raise BulkloadError("private recovery receipt journal binding differs")
        except (BulkloadError, OSError) as error:
            _close_descriptors(descriptors)
            raise BulkloadError(
                "private recovery found an invalid or foreign original receipt"
            ) from error

    original_artifacts: list[Path] = [
        Path(prepared["post_capture_directory"]),
        Path(prepared["recovery_capture_directory"]),
        original_receipt_path,
    ]
    for event in events:
        if event.get("event") != "recovery-prepared":
            continue
        original_artifacts.extend(
            Path(event[key])
            for key in (
                "preflight_capture_directory",
                "post_capture_directory",
                "receipt_path",
            )
        )
    if operation == "apply":
        original_artifacts.append(rollback_root)
    else:
        original_artifacts.extend(
            [
                rollback_root,
                Path(prepared["rollback_preflight_directory"]),
                apply_receipt_path,
            ]
        )
    requested_outputs = {
        "recovery preflight capture": preflight_capture_directory,
        "recovery post capture": post_capture_directory,
        "recovery receipt": receipt_path,
    }
    for output_label, output_path in requested_outputs.items():
        output_absolute = Path(os.path.abspath(output_path.expanduser()))
        for artifact in original_artifacts:
            artifact_absolute = Path(os.path.abspath(artifact.expanduser()))
            if (
                output_absolute == artifact_absolute
                or _path_is_within(output_absolute, artifact_absolute)
                or _path_is_within(artifact_absolute, output_absolute)
            ):
                _close_descriptors(descriptors)
                raise BulkloadError(
                    f"{output_label} overlaps original recovery evidence"
                )
    protected: dict[str, Path] = {
        "source bundle": source_root,
        "destination-before bundle": destination_before_root,
        "install plan": install_plan_path,
        "compatibility plan": compatibility_plan_path,
        "operation journal": journal_path,
        "restore bundle": restore_root,
    }
    for index, artifact in enumerate(original_artifacts):
        if artifact is not None and os.path.lexists(artifact):
            protected[f"original evidence {index}"] = artifact
    outputs = _validate_output_topology(
        requested_outputs,
        protected,
        recorded_protected=_recorded_live_roots(compatibility),
    )
    preflight_target = outputs["recovery preflight capture"]
    post_target = outputs["recovery post capture"]
    recovery_receipt_target = outputs["recovery receipt"]
    recovery_output_authorities = _output_parent_authorities(outputs)

    recovery_id = str(uuid.uuid4())
    recovery_staging_leaf = _auth_staging_name(recovery_id, "recovery")
    recovery_receipt_staging_leaf = _receipt_staging_name(recovery_id)
    destination_descriptor = -1
    receipt_publication = {"attempted": False, "published": False}
    try:
        install_document = _open_pinned_document(
            install_plan_path,
            plan,
            "private recovery install plan",
        )
        compatibility_document = _open_pinned_document(
            compatibility_plan_path,
            compatibility,
            "private recovery compatibility plan",
        )
        restore_pinned = _open_pinned_auth(
            restore_root,
            restore_auth,
            "private recovery restore bundle",
        )
        descriptors.extend(
            [
                install_document[0],
                install_document[1],
                compatibility_document[0],
                compatibility_document[1],
                restore_pinned[0],
                restore_pinned[1],
            ]
        )
        destination_descriptor = private_state._open_private_directory_descriptor(
            codex_root,
            codex_info,
            "private recovery destination Codex home",
        )
        descriptors.append(destination_descriptor)
        if sqlite_root is not None and sqlite_info is not None:
            sqlite_descriptor = private_state._open_private_directory_descriptor(
                sqlite_root,
                sqlite_info,
                "private recovery destination SQLite home",
            )
            descriptors.append(sqlite_descriptor)

        observed_auth, _ = _live_auth_record(
            destination_descriptor,
            "private recovery observed auth",
        )
        before_identities = [before_identity]
        after_identities = _journal_staging_identities(
            events,
            event_name=(
                "mutation-intent"
                if operation == "apply"
                else "rollback-mutation-intent"
            ),
            expected_sha256=after_auth["sha256"],
            expected_size=after_auth["size"],
        )
        before_identities.extend(
            _journal_staging_identities(
                events,
                event_name=(
                    "automatic-rollback-mutation-intent"
                    if operation == "apply"
                    else "automatic-reinstall-mutation-intent"
                ),
                expected_sha256=before_auth["sha256"],
                expected_size=before_auth["size"],
            )
        )
        before_identities.extend(
            _journal_staging_identities(
                events,
                event_name="recovery-mutation-intent",
                expected_sha256=before_auth["sha256"],
                expected_size=before_auth["size"],
            )
        )
        live_is_before = (
            observed_auth["sha256"] == before_auth["sha256"]
            and observed_auth["size"] == before_auth["size"]
            and observed_auth["identity"] in before_identities
        )
        live_is_after = (
            observed_auth["sha256"] == after_auth["sha256"]
            and observed_auth["size"] == after_auth["size"]
            and observed_auth["identity"] in after_identities
        )
        if original_receipt is not None:
            receipt_post_path = Path(
                original_receipt[
                    (
                        "post_capture_directory"
                        if operation == "apply"
                        else "rollback_post_directory"
                    )
                ]
            )
            receipt_post, _ = private_state.read_codex_private_bundle(
                receipt_post_path,
                "destination",
            )
            expected_post_id = original_receipt[
                (
                    "destination_post_capture_id"
                    if operation == "apply"
                    else "rollback_post_capture_id"
                )
            ]
            expected_post_sha = original_receipt[
                (
                    "destination_post_capture_sha256"
                    if operation == "apply"
                    else "rollback_post_capture_sha256"
                )
            ]
            if (
                receipt_post["capture_id"] != expected_post_id
                or receipt_post["capture_sha256"] != expected_post_sha
            ):
                raise BulkloadError("private recovery original post capture differs")
            validate_destination_capture_for_install(
                receipt_post,
                plan,
                expected_auth_sha256=after_auth["sha256"],
            )
            if receipt_post["auth"]["source_identity"] not in after_identities:
                after_identities.append(receipt_post["auth"]["source_identity"])
            live_is_after = (
                observed_auth["sha256"] == after_auth["sha256"]
                and observed_auth["size"] == after_auth["size"]
                and observed_auth["identity"] in after_identities
            )
            if not live_is_after:
                raise BulkloadError(
                    "valid private mutation receipt exists but live auth is not exact"
                )
        elif not live_is_before and not live_is_after:
            raise BulkloadError(
                "unreceipted private mutation has an unknown live auth state"
            )

        quiescence_context.revalidate(
            require_operation_output_absent=True,
        )
        quiescence_context.assert_bulkload_lock(bulkload_lock)
        runtime_context.revalidate()
        previous_journal_descriptor = journal_pinned[0]
        journal_pinned = _repair_pinned_private_journal_tail(
            journal_pinned,
            journal_leaf=journal_path.name,
            accept_sha256=accept_journal,
        )
        if journal_pinned[0] != previous_journal_descriptor:
            descriptors[0] = journal_pinned[0]
        events = journal_pinned[4]
        if original_receipt is None:
            _append_journal(
                journal_pinned[0],
                {
                    "event": "recovery-prepared",
                    "recorded_at": utc_now(),
                    "operation": operation,
                    "operation_id": operation_id,
                    "recovery_id": recovery_id,
                    "quiescence_id": quiescence_id,
                    "quiescence_attestation": quiescence_attestation,
                    "bulkload_lock": bulkload_lock_record,
                    "plan_sha256": plan["plan_sha256"],
                    "apply_receipt_sha256": apply_receipt_sha256,
                    "accepted_journal_sha256": accept_journal,
                    "preflight_capture_directory": os.fspath(preflight_target),
                    "post_capture_directory": os.fspath(post_target),
                    "receipt_path": os.fspath(recovery_receipt_target),
                    "auth_staging_leaf": recovery_staging_leaf,
                    "receipt_staging_leaf": recovery_receipt_staging_leaf,
                    "observed_state": "before" if live_is_before else "after",
                    "output_parent_authorities": recovery_output_authorities,
                },
            )
        _revalidate_output_parent_authorities(recovery_output_authorities)
        _revalidate_pinned_private_journal(
            journal_pinned,
            leaf=journal_path.name,
        )
        preflight_capture = _capture_destination(
            plan,
            destination_before,
            codex_home=codex_root,
            sqlite_home=sqlite_root,
            output_directory=preflight_target,
            protected_directories=tuple(
                path
                for path in (
                    source_root,
                    destination_before_root,
                    restore_root,
                    rollback_root,
                )
                if path.exists() and path.is_dir()
            ),
            recorded_protected_directories=(
                *_recorded_live_roots(compatibility),
                post_target,
            ),
            quiescence=capture_quiescence,
        )
        _revalidate_output_parent_authorities(recovery_output_authorities)
        validate_destination_capture_for_install(
            preflight_capture,
            plan,
            expected_auth_sha256=observed_auth["sha256"],
            forbidden_capture_ids={
                plan["source_capture_id"],
                plan["destination_before_capture_id"],
            },
        )
        if preflight_capture["auth"]["source_identity"] != observed_auth["identity"]:
            raise BulkloadError("private recovery preflight auth identity changed")

        mutation_applied = False
        if original_receipt is None and live_is_after:
            quiescence_context.revalidate(
                require_operation_output_absent=True,
            )
            quiescence_context.assert_bulkload_lock(bulkload_lock)
            restore_payload = restore_pinned[3]
            _stage_auth(
                destination_descriptor,
                restore_payload,
                staging_name=recovery_staging_leaf,
            )
            staging_info = os.stat(
                recovery_staging_leaf,
                dir_fd=destination_descriptor,
                follow_symlinks=False,
            )
            recovery_staging_identity = private_state._identity(staging_info)
            _append_journal(
                journal_pinned[0],
                {
                    "event": "recovery-mutation-intent",
                    "recorded_at": utc_now(),
                    "operation": operation,
                    "operation_id": operation_id,
                    "recovery_id": recovery_id,
                    "staging_leaf": recovery_staging_leaf,
                    "staging_sha256": before_auth["sha256"],
                    "staging_size": before_auth["size"],
                    "staging_identity": recovery_staging_identity,
                },
            )
            _read_descriptor_payload(
                restore_pinned[1],
                restore_auth,
                "private recovery auth immediately before restore",
            )
            _assert_live_auth(
                destination_descriptor,
                expected_sha256=after_auth["sha256"],
                expected_size=after_auth["size"],
                expected_identity=observed_auth["identity"],
                label="private recovery auth at restore commit",
            )
            quiescence_context.revalidate(
                require_operation_output_absent=True,
            )
            quiescence_context.assert_bulkload_lock(bulkload_lock)
            runtime_context.revalidate()
            commit_attempted = True
            try:
                _commit_staged_auth(
                    destination_descriptor,
                    recovery_staging_leaf,
                )
            except BaseException:
                current, _ = _live_auth_record(
                    destination_descriptor,
                    "private recovery auth after uncertain commit",
                )
                if (
                    current["sha256"] == before_auth["sha256"]
                    and current["size"] == before_auth["size"]
                    and current["identity"] == recovery_staging_identity
                ):
                    mutation_applied = True
                elif (
                    current["sha256"] == after_auth["sha256"]
                    and current["size"] == after_auth["size"]
                    and current["identity"] == observed_auth["identity"]
                ):
                    commit_attempted = False
                else:
                    raise BulkloadError("private recovery auth commit is indeterminate")
                if not commit_attempted:
                    raise
            else:
                mutation_applied = True
            quiescence_context.revalidate(
                require_operation_output_absent=True,
                require_observed_state=False,
            )
            quiescence_context.assert_bulkload_lock(bulkload_lock)
            runtime_context.revalidate()
            if not mutation_applied:
                raise BulkloadError("private recovery did not restore auth")
            private_state._fsync_directory_descriptor(destination_descriptor)
            final_auth, _ = _live_auth_record(
                destination_descriptor,
                "private recovery restored auth",
            )
            if (
                final_auth["sha256"] != before_auth["sha256"]
                or final_auth["size"] != before_auth["size"]
                or final_auth["identity"] != recovery_staging_identity
            ):
                raise BulkloadError("private recovery restored auth differs")
            _append_journal(
                journal_pinned[0],
                {
                    "event": "recovery-restored-before",
                    "recorded_at": utc_now(),
                    "operation": operation,
                    "operation_id": operation_id,
                    "recovery_id": recovery_id,
                    "auth_sha256": final_auth["sha256"],
                    "auth_identity": final_auth["identity"],
                },
            )
            decision = "restored-before"
        else:
            final_auth = observed_auth
            decision = (
                "confirmed-committed"
                if original_receipt is not None
                else "confirmed-before-noop"
            )
            if original_receipt is None:
                _append_journal(
                    journal_pinned[0],
                    {
                        "event": "recovery-confirmed-before",
                        "recorded_at": utc_now(),
                        "operation": operation,
                        "operation_id": operation_id,
                        "recovery_id": recovery_id,
                        "auth_sha256": final_auth["sha256"],
                        "auth_identity": final_auth["identity"],
                    },
                )

        staging_records: list[dict[str, Any]] = []
        primary_leaf = prepared[
            (
                "auth_install_staging_leaf"
                if operation == "apply"
                else "auth_staging_leaf"
            )
        ]
        staging_records.append(
            {
                "leaf": primary_leaf,
                "sha256": after_auth["sha256"],
                "size": after_auth["size"],
                "identity": (after_identities[0] if after_identities else None),
            }
        )
        for event in events:
            if event["event"] == "recovery-prepared":
                staging_records.append(
                    {
                        "leaf": event["auth_staging_leaf"],
                        "sha256": before_auth["sha256"],
                        "size": before_auth["size"],
                        "identity": None,
                    }
                )
                continue
            if not isinstance(event.get("staging_leaf"), str):
                continue
            if event.get("staging_sha256") is None:
                continue
            staging_records.append(
                {
                    "leaf": event["staging_leaf"],
                    "sha256": event["staging_sha256"],
                    "size": event["staging_size"],
                    "identity": event.get("staging_identity"),
                }
            )
        cleaned = (
            []
            if original_receipt is not None
            else _cleanup_journaled_auth_staging(
                destination_descriptor,
                staging_records,
            )
        )
        if original_receipt is None:
            _append_journal(
                journal_pinned[0],
                {
                    "event": "recovery-staging-cleaned",
                    "recorded_at": utc_now(),
                    "operation": operation,
                    "operation_id": operation_id,
                    "recovery_id": recovery_id,
                    "cleaned_staging_leaves": cleaned,
                },
            )

        _revalidate_output_parent_authorities(recovery_output_authorities)
        post_capture = _capture_destination(
            plan,
            destination_before,
            codex_home=codex_root,
            sqlite_home=sqlite_root,
            output_directory=post_target,
            protected_directories=tuple(
                path
                for path in (
                    source_root,
                    destination_before_root,
                    restore_root,
                    rollback_root,
                    preflight_target,
                )
                if path.exists() and path.is_dir()
            ),
            recorded_protected_directories=_recorded_live_roots(compatibility),
            quiescence=capture_quiescence,
        )
        _revalidate_output_parent_authorities(recovery_output_authorities)
        validate_destination_capture_for_install(
            post_capture,
            plan,
            expected_auth_sha256=final_auth["sha256"],
            forbidden_capture_ids={
                plan["source_capture_id"],
                plan["destination_before_capture_id"],
                preflight_capture["capture_id"],
            },
        )
        if post_capture["auth"]["source_identity"] != final_auth["identity"]:
            raise BulkloadError("private recovery post-capture auth identity changed")
        if original_receipt is None:
            _append_journal(
                journal_pinned[0],
                {
                    "event": "recovery-offline-state-verified",
                    "recorded_at": utc_now(),
                    "operation": operation,
                    "operation_id": operation_id,
                    "recovery_id": recovery_id,
                    "post_capture_id": post_capture["capture_id"],
                    "post_capture_sha256": post_capture["capture_sha256"],
                    "provider_runtime_acceptance_verified": False,
                },
            )
            _append_journal(
                journal_pinned[0],
                {
                    "event": "recovery-receipt-frozen",
                    "recorded_at": utc_now(),
                    "operation": operation,
                    "operation_id": operation_id,
                    "recovery_id": recovery_id,
                    "receipt_path": os.fspath(recovery_receipt_target),
                    "receipt_staging_leaf": recovery_receipt_staging_leaf,
                },
            )
        final_journal_sha256, final_journal_size = _journal_digest(
            journal_pinned[0],
            journal_pinned[1],
            journal_path.name,
        )
        recovery_receipt = _build_private_recovery_receipt(
            plan,
            recovery_id=recovery_id,
            quiescence_id=quiescence_id,
            quiescence_attestation=quiescence_attestation,
            bulkload_lock=bulkload_lock_record,
            operation=operation,
            operation_id=operation_id,
            apply_receipt_sha256=apply_receipt_sha256,
            accepted_journal_sha256=accept_journal,
            final_journal_sha256=final_journal_sha256,
            final_journal_size=final_journal_size,
            journal_path=journal_path.expanduser().resolve(),
            original_receipt_path=original_receipt_path,
            original_receipt_observation=(
                "valid" if original_receipt is not None else "absent"
            ),
            original_receipt_sha256=(
                original_receipt["receipt_sha256"]
                if original_receipt is not None
                else None
            ),
            decision=decision,
            preflight_directory=preflight_target,
            preflight_capture=preflight_capture,
            post_directory=post_target,
            post_capture=post_capture,
            observed_auth=observed_auth,
            final_auth=final_auth,
            mutation_applied=mutation_applied,
            cleaned_staging_leaves=cleaned,
            receipt_path=recovery_receipt_target,
        )
        _validate_recovery_receipt_against_plan(
            recovery_receipt,
            plan,
            expected_receipt_path=recovery_receipt_target,
        )
        quiescence_context.revalidate(
            require_operation_output_absent=True,
            require_observed_state=not mutation_applied,
        )
        quiescence_context.assert_bulkload_lock(bulkload_lock)
        runtime_context.revalidate()
        _publish_mutation_receipt(
            recovery_receipt_target,
            recovery_receipt,
            staging_leaf=recovery_receipt_staging_leaf,
            protected_directories=tuple(
                path
                for path in (
                    source_root,
                    destination_before_root,
                    restore_root,
                    rollback_root,
                    preflight_target,
                    post_target,
                )
                if path.exists() and path.is_dir()
            ),
            recorded_protected_directories=_recorded_live_roots(compatibility),
            commit_state=receipt_publication,
        )
        quiescence_context.revalidate(
            require_observed_state=not mutation_applied,
        )
        quiescence_context.assert_bulkload_lock(bulkload_lock)
        runtime_context.revalidate()
        return recovery_receipt
    except BaseException as error:
        receipt_outcome = (
            _classify_mutation_receipt_publication(
                recovery_receipt_target,
                recovery_receipt,
                recovery_output_authorities["recovery receipt"],
            )
            if receipt_publication["attempted"] and "recovery_receipt" in locals()
            else "absent"
        )
        if receipt_publication["published"] or receipt_outcome == "exact":
            raise BulkloadError(
                "private recovery receipt reached its no-replace commit point; "
                "live state was retained "
                f"({error}); receipt={recovery_receipt_target}"
            ) from error
        if receipt_outcome == "unknown":
            raise BulkloadError(
                "private recovery receipt publication is indeterminate; "
                f"live state was retained ({error}); "
                f"receipt={recovery_receipt_target}"
            ) from error
        raise
    finally:
        _close_descriptors(descriptors)
