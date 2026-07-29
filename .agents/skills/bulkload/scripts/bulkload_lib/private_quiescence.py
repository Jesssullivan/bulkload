"""Bounded operator attestation and cooperating-bulkload exclusion.

The attestation in this module records an operator's procedural assertion that
non-cooperating Codex writers are quiesced.  Neither the attestation nor the
advisory directory locks below are a provider-writer proof.
"""

from __future__ import annotations

import copy
from datetime import UTC, datetime, timedelta
import errno
import fcntl
import os
from pathlib import Path
import stat
from typing import Any, Sequence
import uuid

from . import private_state
from .model import BulkloadError, canonical_bytes, object_digest, sha256_bytes

PRIVATE_QUIESCENCE_ATTESTATION_SCHEMA = (
    "dev.tinyland.bulkload.codex-private-quiescence-attestation.v2"
)
LEGACY_PRIVATE_QUIESCENCE_ATTESTATION_SCHEMA = (
    "dev.tinyland.bulkload.codex-private-quiescence-attestation.v1"
)
QUIESCENCE_CLAIM = "operator-attested-procedural-fence"
PRIVATE_BULKLOAD_LOCK_SCOPE = "cooperating-bulkload-processes-only"
PRIVATE_BULKLOAD_LOCK_METHOD = "posix-flock-exclusive-nonblocking-directory-descriptors"
MAX_PRIVATE_QUIESCENCE_ATTESTATION_BYTES = 1024 * 1024
MIN_PRIVATE_QUIESCENCE_TTL_SECONDS = 30
DEFAULT_PRIVATE_QUIESCENCE_TTL_SECONDS = 300
MAX_PRIVATE_QUIESCENCE_TTL_SECONDS = 900
PRIVATE_QUIESCENCE_PURPOSES = frozenset(
    {"capture", "close", "apply", "verify", "rollback", "recover"}
)
PRIVATE_STATE_CLASS_ORDER = ("auth", "sqlite")
_ATTESTATION_KEYS = {
    "schema",
    "attestation_id",
    "attestation_sha256",
    "attested_at",
    "expires_at",
    "purpose",
    "capture_role",
    "claim",
    "provider_writer_proof",
    "uid",
    "host",
    "host_authority_id",
    "codex_version",
    "selected_state_classes",
    "codex_home",
    "sqlite_home",
    "observed_auth",
    "observed_sqlite",
    "accepted_inputs",
    "operation_output",
    "bulkload_lock_scope_sha256",
}
_ACCEPTED_INPUT_KEYS = {
    "plan_sha256",
    "apply_receipt_sha256",
    "journal_sha256",
}


def _now_utc() -> datetime:
    return datetime.now(UTC)


def _format_timestamp(value: datetime) -> str:
    return (
        value.astimezone(UTC)
        .replace(microsecond=0)
        .isoformat()
        .replace(
            "+00:00",
            "Z",
        )
    )


def _parse_timestamp(value: Any, label: str) -> datetime:
    if not isinstance(value, str) or not value.endswith("Z"):
        raise BulkloadError(f"{label} must be a canonical UTC timestamp")
    try:
        parsed = datetime.fromisoformat(value[:-1] + "+00:00")
    except ValueError as error:
        raise BulkloadError(f"{label} must be a canonical UTC timestamp") from error
    if _format_timestamp(parsed) != value:
        raise BulkloadError(f"{label} must be a canonical UTC timestamp")
    return parsed


def _require_exact_keys(value: dict[str, Any], expected: set[str], label: str) -> None:
    if set(value) != expected:
        raise BulkloadError(f"{label} keys differ from the exact contract")


def _require_mapping(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise BulkloadError(f"{label} must be an object")
    return value


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


def _validate_directory_identity(value: Any, label: str) -> dict[str, int]:
    identity = _require_mapping(value, label)
    _require_exact_keys(identity, {"device", "inode", "uid", "mode"}, label)
    if not all(type(identity[key]) is int and identity[key] >= 0 for key in identity):
        raise BulkloadError(f"{label} values must be non-negative integers")
    if identity["mode"] & 0o077:
        raise BulkloadError(f"{label} must record an owner-private directory")
    return identity


def _validate_file_identity(value: Any, label: str) -> dict[str, int]:
    identity = _require_mapping(value, label)
    _require_exact_keys(
        identity,
        {"device", "inode", "uid", "mode", "links"},
        label,
    )
    if not all(type(identity[key]) is int and identity[key] >= 0 for key in identity):
        raise BulkloadError(f"{label} values must be non-negative integers")
    return identity


def _directory_record(path: Path, info: os.stat_result) -> dict[str, Any]:
    return {
        "resolved_path": os.fspath(path),
        "identity": private_state._directory_identity_record(info),
    }


def _validate_directory_record(value: Any, label: str) -> dict[str, Any]:
    record = _require_mapping(value, label)
    _require_exact_keys(record, {"resolved_path", "identity"}, label)
    if (
        not isinstance(record["resolved_path"], str)
        or not Path(record["resolved_path"]).is_absolute()
        or Path(record["resolved_path"])
        != Path(os.path.abspath(record["resolved_path"]))
    ):
        raise BulkloadError(f"{label} resolved path must be canonical and absolute")
    _validate_directory_identity(record["identity"], f"{label} identity")
    return record


def _canonical_state_classes(value: Sequence[str]) -> list[str]:
    if isinstance(value, (str, bytes)):
        raise BulkloadError("private quiescence state classes must be a sequence")
    selected = list(value)
    if (
        not selected
        or any(not isinstance(item, str) for item in selected)
        or any(item not in PRIVATE_STATE_CLASS_ORDER for item in selected)
        or len(selected) != len(set(selected))
    ):
        raise BulkloadError(
            "private quiescence state classes must be a non-empty auth/sqlite subset"
        )
    return [item for item in PRIVATE_STATE_CLASS_ORDER if item in selected]


def _validate_purpose_inputs(
    purpose: Any,
    capture_role: Any,
    accepted_inputs: Any,
) -> dict[str, str | None]:
    if not isinstance(purpose, str) or purpose not in PRIVATE_QUIESCENCE_PURPOSES:
        raise BulkloadError("private quiescence purpose is invalid")
    inputs = _require_mapping(accepted_inputs, "private quiescence accepted inputs")
    _require_exact_keys(
        inputs,
        _ACCEPTED_INPUT_KEYS,
        "private quiescence accepted inputs",
    )
    for key, value in inputs.items():
        if value is not None:
            _require_sha256(value, f"private quiescence {key}")

    plan = inputs["plan_sha256"]
    receipt = inputs["apply_receipt_sha256"]
    journal = inputs["journal_sha256"]
    if purpose in {"capture", "close"}:
        if capture_role not in {"source", "destination"}:
            raise BulkloadError(
                f"{purpose} quiescence requires a source or destination role"
            )
        if purpose == "capture" and any(item is not None for item in inputs.values()):
            raise BulkloadError("capture quiescence cannot accept operation inputs")
        if purpose == "close" and (
            plan is None or receipt is not None or journal is not None
        ):
            raise BulkloadError(
                "close quiescence requires only the exact close-request digest"
            )
    else:
        if capture_role is not None:
            raise BulkloadError("non-capture quiescence role must be null")
        if plan is None:
            raise BulkloadError(f"{purpose} quiescence requires a plan digest")
        if purpose == "apply" and (receipt is not None or journal is not None):
            raise BulkloadError("apply quiescence accepts only a plan digest")
        if purpose == "verify" and (receipt is None or journal is not None):
            raise BulkloadError(
                "verify quiescence requires plan and apply-receipt digests"
            )
        if purpose == "rollback" and (receipt is None or journal is not None):
            raise BulkloadError(
                "rollback quiescence requires plan and apply-receipt digests"
            )
        if purpose == "recover" and journal is None:
            raise BulkloadError("recover quiescence requires plan and journal digests")
    return inputs


def _validate_operation_output(value: Any, uid: int) -> dict[str, Any]:
    binding = _require_mapping(value, "private quiescence operation output")
    _require_exact_keys(
        binding,
        {"path", "parent_resolved_path", "parent_identity", "leaf"},
        "private quiescence operation output",
    )
    if (
        not isinstance(binding["path"], str)
        or not isinstance(binding["parent_resolved_path"], str)
        or not isinstance(binding["leaf"], str)
        or binding["leaf"] in {"", ".", ".."}
        or Path(binding["leaf"]).name != binding["leaf"]
    ):
        raise BulkloadError("private quiescence operation output path is invalid")
    parent = Path(binding["parent_resolved_path"])
    path = Path(binding["path"])
    if (
        not parent.is_absolute()
        or not path.is_absolute()
        or parent != Path(os.path.abspath(parent))
        or path != parent / binding["leaf"]
    ):
        raise BulkloadError("private quiescence operation output binding is invalid")
    identity = _validate_directory_identity(
        binding["parent_identity"],
        "private quiescence operation output parent identity",
    )
    if identity["uid"] != uid:
        raise BulkloadError(
            "private quiescence operation output parent owner is invalid"
        )
    return binding


def _lock_scope_payload_from_records(
    records: Sequence[dict[str, Any]],
) -> dict[str, Any]:
    deduplicated: dict[tuple[int, int], dict[str, Any]] = {}
    for record in records:
        validated = _validate_directory_record(
            record,
            "private bulkload lock root",
        )
        identity = validated["identity"]
        key = (identity["device"], identity["inode"])
        existing = deduplicated.get(key)
        if existing is None or validated["resolved_path"] < existing["resolved_path"]:
            deduplicated[key] = copy.deepcopy(validated)
    roots = sorted(
        deduplicated.values(),
        key=lambda item: (
            item["identity"]["device"],
            item["identity"]["inode"],
            item["resolved_path"],
        ),
    )
    if not roots:
        raise BulkloadError("private bulkload lock scope must contain a live root")
    return {
        "scope": PRIVATE_BULKLOAD_LOCK_SCOPE,
        "method": PRIVATE_BULKLOAD_LOCK_METHOD,
        "provider_writer_proof": False,
        "roots": roots,
    }


def _lock_scope_sha256_from_records(records: Sequence[dict[str, Any]]) -> str:
    return sha256_bytes(canonical_bytes(_lock_scope_payload_from_records(records)))


def _artifact_root_records(value: dict[str, Any]) -> list[dict[str, Any]]:
    records = [value["codex_home"]]
    if value["sqlite_home"] is not None:
        records.append(value["sqlite_home"])
    return records


def _observe_auth(root_descriptor: int) -> dict[str, Any]:
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0)
    try:
        descriptor = os.open(
            private_state.AUTH_BASENAME,
            flags,
            dir_fd=root_descriptor,
        )
    except OSError as error:
        raise BulkloadError("cannot open quiesced auth.json") from error
    try:
        before = os.fstat(descriptor)
        if (
            not stat.S_ISREG(before.st_mode)
            or before.st_uid != os.getuid()
            or stat.S_IMODE(before.st_mode) != 0o600
            or before.st_nlink != 1
            or before.st_size < 1
            or before.st_size > private_state.MAX_AUTH_BYTES
        ):
            raise BulkloadError("quiesced auth.json custody is invalid")
        payload = b""
        while len(payload) <= private_state.MAX_AUTH_BYTES:
            block = os.read(
                descriptor,
                min(
                    65536,
                    private_state.MAX_AUTH_BYTES + 1 - len(payload),
                ),
            )
            if not block:
                break
            payload += block
        after = os.fstat(descriptor)
        try:
            entry = os.stat(
                private_state.AUTH_BASENAME,
                dir_fd=root_descriptor,
                follow_symlinks=False,
            )
        except OSError as error:
            raise BulkloadError("cannot revalidate quiesced auth.json") from error
        if (
            len(payload) != before.st_size
            or private_state._stable_artifact_stat(after)
            != private_state._stable_artifact_stat(before)
            or private_state._stable_artifact_stat(entry)
            != private_state._stable_artifact_stat(before)
        ):
            raise BulkloadError("quiesced auth.json changed while observing")
        private_state._strict_json_object(payload, private_state.AUTH_BASENAME)
        return {
            "sha256": sha256_bytes(payload),
            "size": len(payload),
            "identity": private_state._identity(before),
        }
    finally:
        os.close(descriptor)


def _observe_sqlite(root_descriptor: int) -> dict[str, Any]:
    records, namespace_sha256 = private_state._sqlite_live_namespace(
        root_descriptor,
        max_families=private_state.DEFAULT_MAX_SQLITE_FAMILIES,
    )
    family_count = sum(
        1 for item in records if private_state.SQLITE_BASENAME.fullmatch(item["name"])
    )
    if family_count < 1:
        raise BulkloadError(
            "quiesced SQLite observation requires a provider-owned family"
        )
    return {
        "live_namespace_sha256": namespace_sha256,
        "family_count": family_count,
        "live_sidecars_absent": True,
    }


def validate_codex_private_quiescence_attestation(
    value: dict[str, Any],
) -> None:
    """Validate the exact v1 structure without claiming that it is still live."""
    if not isinstance(value, dict):
        raise BulkloadError("private quiescence attestation must be an object")
    _require_exact_keys(
        value,
        _ATTESTATION_KEYS,
        "private quiescence attestation",
    )
    if value["schema"] not in {
        LEGACY_PRIVATE_QUIESCENCE_ATTESTATION_SCHEMA,
        PRIVATE_QUIESCENCE_ATTESTATION_SCHEMA,
    }:
        raise BulkloadError("private quiescence attestation schema is invalid")
    _require_uuid(value["attestation_id"], "private quiescence attestation ID")
    _require_sha256(
        value["attestation_sha256"],
        "private quiescence attestation digest",
    )
    attested_at = _parse_timestamp(
        value["attested_at"],
        "private quiescence attested_at",
    )
    expires_at = _parse_timestamp(
        value["expires_at"],
        "private quiescence expires_at",
    )
    ttl = int((expires_at - attested_at).total_seconds())
    if (
        expires_at <= attested_at
        or ttl < MIN_PRIVATE_QUIESCENCE_TTL_SECONDS
        or ttl > MAX_PRIVATE_QUIESCENCE_TTL_SECONDS
    ):
        raise BulkloadError("private quiescence attestation TTL is invalid")
    if value["claim"] != QUIESCENCE_CLAIM:
        raise BulkloadError("private quiescence claim is invalid")
    if value["provider_writer_proof"] is not False:
        raise BulkloadError(
            "private quiescence attestation cannot claim provider-writer proof"
        )
    if type(value["uid"]) is not int or value["uid"] < 0:
        raise BulkloadError("private quiescence UID is invalid")
    if (
        not isinstance(value["host"], str)
        or not value["host"]
        or len(value["host"].encode("utf-8")) > 255
    ):
        raise BulkloadError("private quiescence host is invalid")
    _require_uuid(
        value["host_authority_id"],
        "private quiescence host authority ID",
    )
    if (
        not isinstance(value["codex_version"], str)
        or not value["codex_version"]
        or len(value["codex_version"].encode("utf-8")) > 128
    ):
        raise BulkloadError("private quiescence Codex version is invalid")
    if not isinstance(value["selected_state_classes"], list):
        raise BulkloadError("private quiescence state classes must be a list")
    selected = _canonical_state_classes(value["selected_state_classes"])
    if selected != value["selected_state_classes"]:
        raise BulkloadError("private quiescence state classes are not canonical")
    _validate_purpose_inputs(
        value["purpose"],
        value["capture_role"],
        value["accepted_inputs"],
    )
    if (
        value["purpose"] == "close"
        and value["schema"] != PRIVATE_QUIESCENCE_ATTESTATION_SCHEMA
    ):
        raise BulkloadError("close quiescence requires the v2 attestation schema")

    codex = _validate_directory_record(
        value["codex_home"],
        "private quiescence Codex home",
    )
    if codex["identity"]["uid"] != value["uid"]:
        raise BulkloadError("private quiescence Codex home owner is invalid")
    sqlite: dict[str, Any] | None = None
    if value["sqlite_home"] is not None:
        sqlite = _validate_directory_record(
            value["sqlite_home"],
            "private quiescence SQLite home",
        )
        if sqlite["identity"]["uid"] != value["uid"]:
            raise BulkloadError("private quiescence SQLite home owner is invalid")
    if ("sqlite" in selected) != (sqlite is not None):
        raise BulkloadError("private quiescence SQLite root selection is inconsistent")

    auth = value["observed_auth"]
    if "auth" in selected:
        auth = _require_mapping(auth, "private quiescence auth observation")
        _require_exact_keys(
            auth,
            {"sha256", "size", "identity"},
            "private quiescence auth observation",
        )
        _require_sha256(auth["sha256"], "private quiescence auth digest")
        if (
            type(auth["size"]) is not int
            or auth["size"] < 1
            or auth["size"] > private_state.MAX_AUTH_BYTES
        ):
            raise BulkloadError("private quiescence auth size is invalid")
        auth_identity = _validate_file_identity(
            auth["identity"],
            "private quiescence auth identity",
        )
        if (
            auth_identity["uid"] != value["uid"]
            or auth_identity["mode"] != 0o600
            or auth_identity["links"] != 1
        ):
            raise BulkloadError("private quiescence auth custody is invalid")
    elif auth is not None:
        raise BulkloadError("unselected auth observation must be null")

    sqlite_observation = value["observed_sqlite"]
    if "sqlite" in selected:
        sqlite_observation = _require_mapping(
            sqlite_observation,
            "private quiescence SQLite observation",
        )
        _require_exact_keys(
            sqlite_observation,
            {
                "live_namespace_sha256",
                "family_count",
                "live_sidecars_absent",
            },
            "private quiescence SQLite observation",
        )
        _require_sha256(
            sqlite_observation["live_namespace_sha256"],
            "private quiescence SQLite namespace digest",
        )
        if (
            type(sqlite_observation["family_count"]) is not int
            or sqlite_observation["family_count"] < 1
            or sqlite_observation["family_count"]
            > private_state.DEFAULT_MAX_SQLITE_FAMILIES
            or sqlite_observation["live_sidecars_absent"] is not True
        ):
            raise BulkloadError("private quiescence SQLite observation is invalid")
    elif sqlite_observation is not None:
        raise BulkloadError("unselected SQLite observation must be null")

    _validate_operation_output(value["operation_output"], value["uid"])
    expected_scope = _lock_scope_sha256_from_records(_artifact_root_records(value))
    _require_sha256(
        value["bulkload_lock_scope_sha256"],
        "private quiescence bulkload lock scope digest",
    )
    if value["bulkload_lock_scope_sha256"] != expected_scope:
        raise BulkloadError("private quiescence bulkload lock scope differs")
    if object_digest(value, "attestation_sha256") != value["attestation_sha256"]:
        raise BulkloadError("private quiescence attestation digest mismatch")


def _read_exact_attestation_descriptor(
    parent_descriptor: int,
    leaf: str,
) -> tuple[int, os.stat_result, bytes, dict[str, Any]]:
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0)
    try:
        descriptor = os.open(leaf, flags, dir_fd=parent_descriptor)
    except OSError as error:
        raise BulkloadError("cannot open private quiescence attestation") from error
    try:
        before = os.fstat(descriptor)
        if (
            not stat.S_ISREG(before.st_mode)
            or before.st_uid != os.getuid()
            or stat.S_IMODE(before.st_mode) != 0o600
            or before.st_nlink != 1
            or before.st_size < 1
            or before.st_size > MAX_PRIVATE_QUIESCENCE_ATTESTATION_BYTES
        ):
            raise BulkloadError("private quiescence attestation custody is invalid")
        payload = b""
        while len(payload) <= MAX_PRIVATE_QUIESCENCE_ATTESTATION_BYTES:
            block = os.read(
                descriptor,
                min(
                    65536,
                    MAX_PRIVATE_QUIESCENCE_ATTESTATION_BYTES + 1 - len(payload),
                ),
            )
            if not block:
                break
            payload += block
        after = os.fstat(descriptor)
        try:
            entry = os.stat(leaf, dir_fd=parent_descriptor, follow_symlinks=False)
        except OSError as error:
            raise BulkloadError(
                "cannot revalidate private quiescence attestation"
            ) from error
        if (
            len(payload) != before.st_size
            or private_state._stable_artifact_stat(after)
            != private_state._stable_artifact_stat(before)
            or private_state._stable_artifact_stat(entry)
            != private_state._stable_artifact_stat(before)
        ):
            raise BulkloadError("private quiescence attestation changed while reading")
        value = private_state._strict_json_object(
            payload,
            "private quiescence attestation",
        )
        validate_codex_private_quiescence_attestation(value)
        if payload != canonical_bytes(value) + b"\n":
            raise BulkloadError("private quiescence attestation is not canonical JSON")
        return descriptor, before, payload, value
    except BaseException:
        os.close(descriptor)
        raise


def _open_attestation_parent(
    path: Path,
) -> tuple[Path, os.stat_result, int, str]:
    requested = path.expanduser()
    if requested.name in {"", ".", ".."}:
        raise BulkloadError("private quiescence attestation needs a leaf name")
    parent, parent_info = private_state._resolve_private_directory(
        requested.parent,
        "private quiescence attestation parent",
    )
    parent_descriptor = private_state._open_private_directory_descriptor(
        parent,
        parent_info,
        "private quiescence attestation parent",
    )
    return parent, parent_info, parent_descriptor, requested.name


def read_codex_private_quiescence_attestation(
    path: Path,
) -> dict[str, Any]:
    """Read and validate one canonical, owner-private attestation."""
    parent, parent_info, parent_descriptor, leaf = _open_attestation_parent(path)
    descriptor = -1
    try:
        descriptor, _, _, value = _read_exact_attestation_descriptor(
            parent_descriptor,
            leaf,
        )
        private_state._revalidate_directory_binding(
            parent,
            parent_descriptor,
            parent_info,
            "private quiescence attestation parent",
        )
        return copy.deepcopy(value)
    finally:
        if descriptor >= 0:
            os.close(descriptor)
        os.close(parent_descriptor)


def _resolve_operation_output(
    path: Path,
) -> tuple[dict[str, Any], Path, os.stat_result]:
    requested = path.expanduser()
    if requested.name in {"", ".", ".."}:
        raise BulkloadError("private operation output requires a leaf name")
    parent, parent_info = private_state._resolve_private_directory(
        requested.parent,
        "private operation output parent",
    )
    target = parent / requested.name
    if target.exists() or target.is_symlink():
        raise BulkloadError("private operation output must be create-only and absent")
    return (
        {
            "path": os.fspath(target),
            "parent_resolved_path": os.fspath(parent),
            "parent_identity": private_state._directory_identity_record(parent_info),
            "leaf": requested.name,
        },
        parent,
        parent_info,
    )


def _path_matches_directory_record(
    expected_path: Path,
    record: dict[str, Any],
    label: str,
) -> tuple[Path, os.stat_result, int]:
    resolved, info = private_state._resolve_private_directory(expected_path, label)
    if (
        os.fspath(resolved) != record["resolved_path"]
        or private_state._directory_identity_record(info) != record["identity"]
    ):
        raise BulkloadError(f"{label} differs from the accepted attestation")
    descriptor = private_state._open_private_directory_descriptor(
        resolved,
        info,
        label,
    )
    return resolved, info, descriptor


class PinnedCodexPrivateQuiescenceAttestation:
    """Pinned attestation and live-root context for repeated revalidation."""

    def __init__(
        self,
        *,
        value: dict[str, Any],
        artifact_path: Path,
        artifact_parent: Path,
        artifact_parent_info: os.stat_result,
        artifact_parent_descriptor: int,
        artifact_leaf: str,
        artifact_descriptor: int,
        artifact_info: os.stat_result,
        artifact_payload: bytes,
        codex_root: Path,
        codex_info: os.stat_result,
        codex_descriptor: int,
        sqlite_root: Path | None,
        sqlite_info: os.stat_result | None,
        sqlite_descriptor: int,
        output_parent: Path,
        output_parent_info: os.stat_result,
        output_parent_descriptor: int,
    ) -> None:
        self._value = copy.deepcopy(value)
        self._artifact_path = artifact_path
        self._artifact_parent = artifact_parent
        self._artifact_parent_info = artifact_parent_info
        self._artifact_parent_descriptor = artifact_parent_descriptor
        self._artifact_leaf = artifact_leaf
        self._artifact_descriptor = artifact_descriptor
        self._artifact_info = artifact_info
        self._artifact_payload = artifact_payload
        self._codex_root = codex_root
        self._codex_info = codex_info
        self._codex_descriptor = codex_descriptor
        self._sqlite_root = sqlite_root
        self._sqlite_info = sqlite_info
        self._sqlite_descriptor = sqlite_descriptor
        self._output_parent = output_parent
        self._output_parent_info = output_parent_info
        self._output_parent_descriptor = output_parent_descriptor
        self._closed = False

    @property
    def value(self) -> dict[str, Any]:
        return copy.deepcopy(self._value)

    @property
    def attestation_sha256(self) -> str:
        return self._value["attestation_sha256"]

    @property
    def attestation_id(self) -> str:
        return self._value["attestation_id"]

    @property
    def scope_sha256(self) -> str:
        return self._value["bulkload_lock_scope_sha256"]

    def _require_open(self) -> None:
        if self._closed:
            raise BulkloadError("private quiescence context is closed")

    def revalidate(
        self,
        *,
        require_operation_output_absent: bool = False,
        require_observed_state: bool = True,
    ) -> None:
        """Recheck custody and optionally the attested pre-operation state."""
        self._require_open()
        private_state._revalidate_directory_binding(
            self._artifact_parent,
            self._artifact_parent_descriptor,
            self._artifact_parent_info,
            "private quiescence attestation parent",
        )
        try:
            before = os.fstat(self._artifact_descriptor)
            os.lseek(self._artifact_descriptor, 0, os.SEEK_SET)
            payload = b""
            while len(payload) <= MAX_PRIVATE_QUIESCENCE_ATTESTATION_BYTES:
                block = os.read(
                    self._artifact_descriptor,
                    min(
                        65536,
                        MAX_PRIVATE_QUIESCENCE_ATTESTATION_BYTES + 1 - len(payload),
                    ),
                )
                if not block:
                    break
                payload += block
            after = os.fstat(self._artifact_descriptor)
            entry = os.stat(
                self._artifact_leaf,
                dir_fd=self._artifact_parent_descriptor,
                follow_symlinks=False,
            )
        except OSError as error:
            raise BulkloadError(
                "cannot revalidate private quiescence attestation"
            ) from error
        expected_stat = private_state._stable_artifact_stat(self._artifact_info)
        if (
            private_state._stable_artifact_stat(before) != expected_stat
            or private_state._stable_artifact_stat(after) != expected_stat
            or private_state._stable_artifact_stat(entry) != expected_stat
            or payload != self._artifact_payload
        ):
            raise BulkloadError("private quiescence attestation custody changed")
        value = private_state._strict_json_object(
            payload,
            "private quiescence attestation",
        )
        validate_codex_private_quiescence_attestation(value)
        if value != self._value or payload != canonical_bytes(value) + b"\n":
            raise BulkloadError("private quiescence attestation content changed")
        if value["uid"] != os.getuid() or value["host"] != os.uname().nodename:
            raise BulkloadError("private quiescence host custody changed")
        if _now_utc() >= _parse_timestamp(
            value["expires_at"],
            "private quiescence expires_at",
        ):
            raise BulkloadError("private quiescence attestation expired")

        private_state._revalidate_directory_binding(
            self._codex_root,
            self._codex_descriptor,
            self._codex_info,
            "private quiescence Codex home",
        )
        if require_observed_state and "auth" in value["selected_state_classes"]:
            if _observe_auth(self._codex_descriptor) != value["observed_auth"]:
                raise BulkloadError(
                    "private quiescence auth observation no longer matches"
                )
        if self._sqlite_root is not None and self._sqlite_info is not None:
            private_state._revalidate_directory_binding(
                self._sqlite_root,
                self._sqlite_descriptor,
                self._sqlite_info,
                "private quiescence SQLite home",
            )
            if (
                require_observed_state
                and _observe_sqlite(self._sqlite_descriptor) != value["observed_sqlite"]
            ):
                raise BulkloadError(
                    "private quiescence SQLite observation no longer matches"
                )
        private_state._revalidate_directory_binding(
            self._output_parent,
            self._output_parent_descriptor,
            self._output_parent_info,
            "private quiescence operation output parent",
        )
        if require_operation_output_absent:
            try:
                os.stat(
                    value["operation_output"]["leaf"],
                    dir_fd=self._output_parent_descriptor,
                    follow_symlinks=False,
                )
            except FileNotFoundError:
                pass
            except OSError as error:
                raise BulkloadError(
                    "cannot revalidate private operation output"
                ) from error
            else:
                raise BulkloadError(
                    "private operation output is no longer create-only and absent"
                )

    def assert_bulkload_lock(self, lock: "CodexPrivateBulkloadLock") -> None:
        """Require a live, matching cooperating-bulkload lock scope."""
        self._require_open()
        lock.revalidate()
        if lock.scope_sha256 != self.scope_sha256:
            raise BulkloadError(
                "private bulkload lock differs from attested live-root scope"
            )

    def close(self) -> None:
        if self._closed:
            return
        self._closed = True
        descriptors = [
            self._output_parent_descriptor,
            self._sqlite_descriptor,
            self._codex_descriptor,
            self._artifact_descriptor,
            self._artifact_parent_descriptor,
        ]
        for descriptor in descriptors:
            if descriptor >= 0:
                try:
                    os.close(descriptor)
                except OSError:
                    pass

    def __enter__(self) -> "PinnedCodexPrivateQuiescenceAttestation":
        self._require_open()
        return self

    def __exit__(self, *_: object) -> None:
        self.close()


def open_codex_private_quiescence_attestation(
    path: Path,
    *,
    accept_attestation: str,
    expected_purpose: str,
    expected_host_authority_id: str,
    expected_codex_version: str,
    expected_selected_state_classes: Sequence[str],
    expected_codex_home: Path,
    expected_sqlite_home: Path | None,
    expected_operation_output: Path,
    expected_capture_role: str | None = None,
    expected_plan_sha256: str | None = None,
    expected_apply_receipt_sha256: str | None = None,
    expected_journal_sha256: str | None = None,
) -> PinnedCodexPrivateQuiescenceAttestation:
    """Open exact accepted evidence and pin every path authority it binds."""
    _require_sha256(
        accept_attestation,
        "accepted private quiescence attestation digest",
    )
    selected = _canonical_state_classes(expected_selected_state_classes)
    expected_inputs = {
        "plan_sha256": expected_plan_sha256,
        "apply_receipt_sha256": expected_apply_receipt_sha256,
        "journal_sha256": expected_journal_sha256,
    }
    _validate_purpose_inputs(
        expected_purpose,
        expected_capture_role,
        expected_inputs,
    )
    parent: Path | None = None
    parent_info: os.stat_result | None = None
    parent_descriptor = -1
    artifact_descriptor = -1
    codex_descriptor = -1
    sqlite_descriptor = -1
    output_parent_descriptor = -1
    try:
        parent, parent_info, parent_descriptor, leaf = _open_attestation_parent(path)
        (
            artifact_descriptor,
            artifact_info,
            payload,
            value,
        ) = _read_exact_attestation_descriptor(parent_descriptor, leaf)
        if value["attestation_sha256"] != accept_attestation:
            raise BulkloadError(
                "private quiescence attestation digest was not exactly accepted"
            )
        expected_scalars = {
            "purpose": expected_purpose,
            "capture_role": expected_capture_role,
            "host_authority_id": expected_host_authority_id,
            "codex_version": expected_codex_version,
            "selected_state_classes": selected,
            "accepted_inputs": expected_inputs,
        }
        for key, expected in expected_scalars.items():
            if value[key] != expected:
                raise BulkloadError(
                    f"private quiescence attestation {key} differs from expected"
                )
        if value["uid"] != os.getuid() or value["host"] != os.uname().nodename:
            raise BulkloadError("private quiescence attestation is for another host")
        if _now_utc() >= _parse_timestamp(
            value["expires_at"],
            "private quiescence expires_at",
        ):
            raise BulkloadError("private quiescence attestation expired")

        codex_root, codex_info, codex_descriptor = _path_matches_directory_record(
            expected_codex_home,
            value["codex_home"],
            "private quiescence Codex home",
        )
        sqlite_root: Path | None = None
        sqlite_info: os.stat_result | None = None
        if expected_sqlite_home is None:
            if value["sqlite_home"] is not None:
                raise BulkloadError(
                    "private quiescence attestation unexpectedly binds SQLite"
                )
        else:
            if value["sqlite_home"] is None:
                raise BulkloadError(
                    "private quiescence attestation omits expected SQLite"
                )
            (
                sqlite_root,
                sqlite_info,
                sqlite_descriptor,
            ) = _path_matches_directory_record(
                expected_sqlite_home,
                value["sqlite_home"],
                "private quiescence SQLite home",
            )

        output_binding, output_parent, output_parent_info = _resolve_operation_output(
            expected_operation_output
        )
        if output_binding != value["operation_output"]:
            raise BulkloadError(
                "private quiescence operation output differs from expected"
            )
        output_parent_descriptor = private_state._open_private_directory_descriptor(
            output_parent,
            output_parent_info,
            "private quiescence operation output parent",
        )
        artifact_path = parent / leaf
        context = PinnedCodexPrivateQuiescenceAttestation(
            value=value,
            artifact_path=artifact_path,
            artifact_parent=parent,
            artifact_parent_info=parent_info,
            artifact_parent_descriptor=parent_descriptor,
            artifact_leaf=leaf,
            artifact_descriptor=artifact_descriptor,
            artifact_info=artifact_info,
            artifact_payload=payload,
            codex_root=codex_root,
            codex_info=codex_info,
            codex_descriptor=codex_descriptor,
            sqlite_root=sqlite_root,
            sqlite_info=sqlite_info,
            sqlite_descriptor=sqlite_descriptor,
            output_parent=output_parent,
            output_parent_info=output_parent_info,
            output_parent_descriptor=output_parent_descriptor,
        )
        context.revalidate(require_operation_output_absent=True)
        parent_descriptor = -1
        artifact_descriptor = -1
        codex_descriptor = -1
        sqlite_descriptor = -1
        output_parent_descriptor = -1
        return context
    finally:
        for descriptor in (
            output_parent_descriptor,
            sqlite_descriptor,
            codex_descriptor,
            artifact_descriptor,
            parent_descriptor,
        ):
            if descriptor >= 0:
                try:
                    os.close(descriptor)
                except OSError:
                    pass


def _operation_output_is_outside_roots(
    target_parent: Path,
    target_parent_info: os.stat_result,
    leaf: str,
    roots: Sequence[tuple[Path, os.stat_result]],
) -> None:
    descriptor = private_state._open_private_directory_descriptor(
        target_parent,
        target_parent_info,
        "private operation output parent",
    )
    try:
        protected_identities = {(info.st_dev, info.st_ino) for _, info in roots}
        if private_state._directory_identity_lineage(descriptor) & protected_identities:
            raise BulkloadError(
                "private operation output aliases or descends from a live root"
            )
        target = target_parent / leaf
        for root, _ in roots:
            try:
                target.relative_to(root)
            except ValueError:
                continue
            raise BulkloadError("private operation output overlaps a live root")
    finally:
        os.close(descriptor)


def create_codex_private_quiescence_attestation(
    codex_home: Path,
    output: Path,
    *,
    purpose: str,
    host_authority_id: str,
    codex_version: str,
    selected_state_classes: Sequence[str],
    sqlite_home: Path | None,
    create_only_output: Path,
    acknowledge_writers_quiesced: bool,
    capture_role: str | None = None,
    accepted_plan_sha256: str | None = None,
    accepted_apply_receipt_sha256: str | None = None,
    accepted_journal_sha256: str | None = None,
    ttl_seconds: int = DEFAULT_PRIVATE_QUIESCENCE_TTL_SECONDS,
) -> dict[str, Any]:
    """Create one short-lived, create-only operator attestation.

    ``acknowledge_writers_quiesced`` is an attended operator assertion.  The
    function additionally excludes cooperating bulkload processes, but cannot
    establish that Codex or any other provider writer honors that advisory lock.
    """
    if acknowledge_writers_quiesced is not True:
        raise BulkloadError(
            "private quiescence attestation requires explicit operator acknowledgement"
        )
    _require_uuid(host_authority_id, "private quiescence host authority ID")
    if (
        not isinstance(codex_version, str)
        or not codex_version
        or len(codex_version.encode("utf-8")) > 128
    ):
        raise BulkloadError("private quiescence Codex version is invalid")
    if (
        type(ttl_seconds) is not int
        or ttl_seconds < MIN_PRIVATE_QUIESCENCE_TTL_SECONDS
        or ttl_seconds > MAX_PRIVATE_QUIESCENCE_TTL_SECONDS
    ):
        raise BulkloadError("private quiescence TTL is outside the contract")
    selected = _canonical_state_classes(selected_state_classes)
    accepted_inputs = {
        "plan_sha256": accepted_plan_sha256,
        "apply_receipt_sha256": accepted_apply_receipt_sha256,
        "journal_sha256": accepted_journal_sha256,
    }
    _validate_purpose_inputs(purpose, capture_role, accepted_inputs)
    if ("sqlite" in selected) != (sqlite_home is not None):
        raise BulkloadError(
            "private quiescence SQLite selection requires one explicit SQLite home"
        )

    with acquire_codex_private_bulkload_lock(codex_home, sqlite_home) as lock:
        codex_root, codex_info = private_state._resolve_private_directory(
            codex_home,
            "private quiescence Codex home",
        )
        codex_descriptor = private_state._open_private_directory_descriptor(
            codex_root,
            codex_info,
            "private quiescence Codex home",
        )
        sqlite_root: Path | None = None
        sqlite_info: os.stat_result | None = None
        sqlite_descriptor = -1
        try:
            if sqlite_home is not None:
                sqlite_root, sqlite_info = private_state._resolve_private_directory(
                    sqlite_home,
                    "private quiescence SQLite home",
                )
                sqlite_descriptor = private_state._open_private_directory_descriptor(
                    sqlite_root,
                    sqlite_info,
                    "private quiescence SQLite home",
                )
            root_pairs = [(codex_root, codex_info)]
            root_records = [_directory_record(codex_root, codex_info)]
            if sqlite_root is not None and sqlite_info is not None:
                root_pairs.append((sqlite_root, sqlite_info))
                root_records.append(_directory_record(sqlite_root, sqlite_info))
            scope_sha256 = _lock_scope_sha256_from_records(root_records)
            if scope_sha256 != lock.scope_sha256:
                raise BulkloadError(
                    "private bulkload lock did not bind the resolved live roots"
                )

            operation_binding, operation_parent, operation_parent_info = (
                _resolve_operation_output(create_only_output)
            )
            _operation_output_is_outside_roots(
                operation_parent,
                operation_parent_info,
                operation_binding["leaf"],
                root_pairs,
            )
            requested_attestation = output.expanduser()
            if (
                requested_attestation.parent.resolve() / requested_attestation.name
                == Path(operation_binding["path"])
            ):
                raise BulkloadError(
                    "private attestation and operation output must be distinct"
                )

            observed_auth = (
                _observe_auth(codex_descriptor) if "auth" in selected else None
            )
            observed_sqlite = (
                _observe_sqlite(sqlite_descriptor) if "sqlite" in selected else None
            )
            lock.revalidate()
            now = _now_utc().astimezone(UTC).replace(microsecond=0)
            value: dict[str, Any] = {
                "schema": PRIVATE_QUIESCENCE_ATTESTATION_SCHEMA,
                "attestation_id": str(uuid.uuid4()),
                "attestation_sha256": "",
                "attested_at": _format_timestamp(now),
                "expires_at": _format_timestamp(now + timedelta(seconds=ttl_seconds)),
                "purpose": purpose,
                "capture_role": capture_role,
                "claim": QUIESCENCE_CLAIM,
                "provider_writer_proof": False,
                "uid": os.getuid(),
                "host": os.uname().nodename,
                "host_authority_id": host_authority_id,
                "codex_version": codex_version,
                "selected_state_classes": selected,
                "codex_home": _directory_record(codex_root, codex_info),
                "sqlite_home": (
                    _directory_record(sqlite_root, sqlite_info)
                    if sqlite_root is not None and sqlite_info is not None
                    else None
                ),
                "observed_auth": observed_auth,
                "observed_sqlite": observed_sqlite,
                "accepted_inputs": accepted_inputs,
                "operation_output": operation_binding,
                "bulkload_lock_scope_sha256": scope_sha256,
            }
            value["attestation_sha256"] = object_digest(
                value,
                "attestation_sha256",
            )
            validate_codex_private_quiescence_attestation(value)
            private_state.write_private_json_noreplace(
                output,
                value,
                recorded_protected_directories=tuple(root for root, _ in root_pairs),
            )
            lock.revalidate()
            private_state._revalidate_directory_binding(
                codex_root,
                codex_descriptor,
                codex_info,
                "private quiescence Codex home",
            )
            if (
                observed_auth is not None
                and _observe_auth(codex_descriptor) != observed_auth
            ):
                raise BulkloadError(
                    "auth.json changed while publishing quiescence attestation"
                )
            if (
                observed_sqlite is not None
                and _observe_sqlite(sqlite_descriptor) != observed_sqlite
            ):
                raise BulkloadError(
                    "SQLite changed while publishing quiescence attestation"
                )
            operation_descriptor = private_state._open_private_directory_descriptor(
                operation_parent,
                operation_parent_info,
                "private operation output parent",
            )
            try:
                try:
                    os.stat(
                        operation_binding["leaf"],
                        dir_fd=operation_descriptor,
                        follow_symlinks=False,
                    )
                except FileNotFoundError:
                    pass
                else:
                    raise BulkloadError(
                        "private operation output appeared during attestation"
                    )
            finally:
                os.close(operation_descriptor)
            read_back = read_codex_private_quiescence_attestation(output)
            if read_back != value:
                raise BulkloadError("published private quiescence attestation differs")
            return copy.deepcopy(value)
        finally:
            if sqlite_descriptor >= 0:
                os.close(sqlite_descriptor)
            os.close(codex_descriptor)


class CodexPrivateBulkloadLock:
    """Exclusive advisory locks honored only by cooperating bulkload processes."""

    def __init__(
        self,
        roots: list[tuple[Path, os.stat_result, int]],
    ) -> None:
        self._roots = roots
        payload = _lock_scope_payload_from_records(
            [_directory_record(path, info) for path, info, _ in roots]
        )
        scope_sha256 = sha256_bytes(canonical_bytes(payload))
        self._record = {**payload, "scope_sha256": scope_sha256}
        self._closed = False

    @property
    def record(self) -> dict[str, Any]:
        return copy.deepcopy(self._record)

    @property
    def scope_sha256(self) -> str:
        return self._record["scope_sha256"]

    def _require_open(self) -> None:
        if self._closed:
            raise BulkloadError("private bulkload lock is closed")

    def revalidate(self) -> None:
        """Revalidate held directory authorities, not provider writer state."""
        self._require_open()
        for path, info, descriptor in self._roots:
            private_state._revalidate_directory_binding(
                path,
                descriptor,
                info,
                "private bulkload lock root",
            )
        payload = _lock_scope_payload_from_records(
            [_directory_record(path, info) for path, info, _ in self._roots]
        )
        if sha256_bytes(canonical_bytes(payload)) != self.scope_sha256:
            raise BulkloadError("private bulkload lock scope changed")

    def close(self) -> None:
        if self._closed:
            return
        self._closed = True
        for _, _, descriptor in reversed(self._roots):
            try:
                fcntl.flock(descriptor, fcntl.LOCK_UN)
            except OSError:
                pass
            try:
                os.close(descriptor)
            except OSError:
                pass
        self._roots = []

    def __enter__(self) -> "CodexPrivateBulkloadLock":
        self._require_open()
        return self

    def __exit__(self, *_: object) -> None:
        self.close()


def acquire_codex_private_bulkload_lock(
    codex_home: Path,
    sqlite_home: Path | None = None,
) -> CodexPrivateBulkloadLock:
    """Lock sorted/deduplicated live-root directory FDs without blocking.

    These locks have no state root or lock file.  They serialize only bulkload
    processes that use this API; Codex/provider writers do not honor them.
    """
    requested: list[tuple[Path, os.stat_result]] = []
    for path, label in (
        (codex_home, "private bulkload Codex home"),
        (sqlite_home, "private bulkload SQLite home"),
    ):
        if path is None:
            continue
        resolved, info = private_state._resolve_private_directory(path, label)
        requested.append((resolved, info))
    deduplicated: dict[tuple[int, int], tuple[Path, os.stat_result]] = {}
    for path, info in requested:
        key = (info.st_dev, info.st_ino)
        existing = deduplicated.get(key)
        if existing is None or os.fspath(path) < os.fspath(existing[0]):
            deduplicated[key] = (path, info)
    ordered = sorted(
        deduplicated.values(),
        key=lambda item: (item[1].st_dev, item[1].st_ino, os.fspath(item[0])),
    )
    if not ordered:
        raise BulkloadError("private bulkload lock requires a Codex home")

    held: list[tuple[Path, os.stat_result, int]] = []
    try:
        for path, info in ordered:
            descriptor = private_state._open_private_directory_descriptor(
                path,
                info,
                "private bulkload lock root",
            )
            try:
                fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except OSError as error:
                os.close(descriptor)
                if error.errno in {errno.EACCES, errno.EAGAIN}:
                    raise BulkloadError(
                        "private bulkload live-root lock is contended"
                    ) from error
                raise BulkloadError(
                    "cannot acquire private bulkload live-root lock"
                ) from error
            held.append((path, info, descriptor))
        lock = CodexPrivateBulkloadLock(held)
        lock.revalidate()
        return lock
    except BaseException:
        for _, _, descriptor in reversed(held):
            try:
                fcntl.flock(descriptor, fcntl.LOCK_UN)
            except OSError:
                pass
            try:
                os.close(descriptor)
            except OSError:
                pass
        raise
