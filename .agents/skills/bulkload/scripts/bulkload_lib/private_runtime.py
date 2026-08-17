"""Pinned source and policy authority for private Codex mutations."""

from __future__ import annotations

import json
import os
from pathlib import Path
import stat
from typing import Any

from .model import BulkloadError, canonical_bytes, sha256_bytes


PRIVATE_STATE_POLICY_NAME = "codex-private-state-policy.v6.json"
PRIVATE_STATE_POLICY_SCHEMA = "dev.tinyland.bulkload.codex-private-state-policy.v6"
LEGACY_PRIVATE_STATE_POLICY_SCHEMA_V4 = (
    "dev.tinyland.bulkload.codex-private-state-policy.v4"
)
LEGACY_PRIVATE_STATE_POLICY_SCHEMA_V5 = (
    "dev.tinyland.bulkload.codex-private-state-policy.v5"
)
PRIVATE_RUNTIME_AUTHORITY_SCHEMA = (
    "dev.tinyland.bulkload.codex-private-runtime-authority.v1"
)
PRIVATE_INSTALL_IMPLEMENTATION = "auth-atomic-replace-sqlite-compose-request-v6"
PRIVATE_RUNTIME_SOURCE_KEYS = {
    "scripts/bulkload_lib/cli.py",
    "scripts/bulkload_lib/private_apply.py",
    "scripts/bulkload_lib/private_quiescence.py",
    "scripts/bulkload_lib/private_sqlite_action_plan.py",
    "scripts/bulkload_lib/private_sqlite_close.py",
    "scripts/bulkload_lib/private_sqlite_plan.py",
    "scripts/bulkload_lib/private_sqlite_request.py",
    "scripts/bulkload_lib/private_state.py",
    "scripts/bulkload_lib/sessions.py",
}
LEGACY_PRIVATE_RUNTIME_SOURCE_DIGESTS_V4 = {
    "scripts/bulkload_lib/cli.py": (
        "a22ba444f296c275b959c8ed746dffc0d05ac6b274302cdee19c107063c4571f"
    ),
    "scripts/bulkload_lib/private_apply.py": (
        "4cfbc1173468884c2098da2397bf9b5c1f8c51251bc85ed66ed3170524a507de"
    ),
    "scripts/bulkload_lib/private_quiescence.py": (
        "a7107156552e65db59e93c849d4e89c4322fc614aa27018c580f10768a510296"
    ),
    "scripts/bulkload_lib/private_sqlite_plan.py": (
        "f67cc27b729914ae321ad81d1fa6934c9acea66b0ae8f5737b6f1de3da908ddd"
    ),
    "scripts/bulkload_lib/private_state.py": (
        "942d8254d2fc3f232cd59e82e1b37ff250ba560c0d2465d4806638512e1b9815"
    ),
    "scripts/bulkload_lib/sessions.py": (
        "30342175e2aea9f3c08a801523529b99438e2447241918297c804124268279f7"
    ),
}
ACCEPTED_H5_PRIVATE_RUNTIME_SOURCE_DIGESTS_V4 = {
    "scripts/bulkload_lib/cli.py": (
        "90a534ebfec72ea7492990447a148f14049aa6ee80d10e20cf8d11b00773d193"
    ),
    "scripts/bulkload_lib/private_apply.py": (
        "4cfbc1173468884c2098da2397bf9b5c1f8c51251bc85ed66ed3170524a507de"
    ),
    "scripts/bulkload_lib/private_quiescence.py": (
        "a7107156552e65db59e93c849d4e89c4322fc614aa27018c580f10768a510296"
    ),
    "scripts/bulkload_lib/private_sqlite_plan.py": (
        "f67cc27b729914ae321ad81d1fa6934c9acea66b0ae8f5737b6f1de3da908ddd"
    ),
    "scripts/bulkload_lib/private_state.py": (
        "942d8254d2fc3f232cd59e82e1b37ff250ba560c0d2465d4806638512e1b9815"
    ),
    "scripts/bulkload_lib/sessions.py": (
        "35982ce66e2bcfc2b5c82ffc47560f43665aa5fe680681be57df994b8503b735"
    ),
}
LEGACY_PRIVATE_RUNTIME_AUTHORITY_V4 = {
    "schema": PRIVATE_RUNTIME_AUTHORITY_SCHEMA,
    "policy_schema": LEGACY_PRIVATE_STATE_POLICY_SCHEMA_V4,
    "policy_sha256": (
        "6167a5a60ec621400c564f689be8c6617caa06ea9b2b80197664e48e111f22fc"
    ),
    "runtime_source_sha256": (
        "14ecf2c208cefb06978a1073cc023400d6b0d498327e4d858b6c8aa978ea0c2b"
    ),
    "source_digests": LEGACY_PRIVATE_RUNTIME_SOURCE_DIGESTS_V4,
}
ACCEPTED_H5_PRIVATE_RUNTIME_AUTHORITY_V4 = {
    "schema": PRIVATE_RUNTIME_AUTHORITY_SCHEMA,
    "policy_schema": LEGACY_PRIVATE_STATE_POLICY_SCHEMA_V4,
    "policy_sha256": (
        "d721c5bad5a046a5a67ec04a708bbf3401016be0a5bd8159a6fb6a41fc5269a3"
    ),
    "runtime_source_sha256": (
        "17e2de0a56e1b18baf5d9b070c33f5390dd50620ee6908eb575c2842c5c63ef3"
    ),
    "source_digests": ACCEPTED_H5_PRIVATE_RUNTIME_SOURCE_DIGESTS_V4,
}
_ACCEPTED_PRIVATE_RUNTIME_AUTHORITIES_V4 = frozenset(
    canonical_bytes(authority)
    for authority in (
        LEGACY_PRIVATE_RUNTIME_AUTHORITY_V4,
        ACCEPTED_H5_PRIVATE_RUNTIME_AUTHORITY_V4,
    )
)
ACCEPTED_H6_PRIVATE_RUNTIME_SOURCE_DIGESTS_V5 = {
    "scripts/bulkload_lib/cli.py": (
        "5d5aa2b9d9a8f04413a49dff92a8c4eb545d54a2970d9b5371b7b52bac9e4238"
    ),
    "scripts/bulkload_lib/private_apply.py": (
        "4cfbc1173468884c2098da2397bf9b5c1f8c51251bc85ed66ed3170524a507de"
    ),
    "scripts/bulkload_lib/private_quiescence.py": (
        "7b7e3e110f030a536f2b74bfc0079d59681b5af740f68859e96596f22d3d0423"
    ),
    "scripts/bulkload_lib/private_sqlite_action_plan.py": (
        "7da422b4fd63b8c9fbf78797f9a27684648cdf165ac93c4dca26bb3ddf4ba2e3"
    ),
    "scripts/bulkload_lib/private_sqlite_close.py": (
        "676f71c01fc1151a1019bbef2f7b42d4cf0e0b933ce3487f2529efe4e9a1d864"
    ),
    "scripts/bulkload_lib/private_sqlite_plan.py": (
        "e45dc732fe1a208bbbea1455d25435754d49397d9a0bea471d5bd5265321398c"
    ),
    "scripts/bulkload_lib/private_state.py": (
        "67a764a4ca8783517c94bd98d5f41af7f42e35a40755361225d316fc9419ba01"
    ),
    "scripts/bulkload_lib/sessions.py": (
        "35982ce66e2bcfc2b5c82ffc47560f43665aa5fe680681be57df994b8503b735"
    ),
}
ACCEPTED_H6_PRIVATE_RUNTIME_AUTHORITY_V5 = {
    "schema": PRIVATE_RUNTIME_AUTHORITY_SCHEMA,
    "policy_schema": LEGACY_PRIVATE_STATE_POLICY_SCHEMA_V5,
    "policy_sha256": (
        "78318633ef06ca12d6dc7e72199c07dc6f5cfdf0e9cf13bc5bd3f3e7c2ddbcf0"
    ),
    "runtime_source_sha256": (
        "cc9f96adb8189e0a41133244231edf51dd837fa75459728355b93eeb981c7392"
    ),
    "source_digests": ACCEPTED_H6_PRIVATE_RUNTIME_SOURCE_DIGESTS_V5,
}
_ACCEPTED_PRIVATE_RUNTIME_AUTHORITIES_V5 = frozenset(
    (canonical_bytes(ACCEPTED_H6_PRIVATE_RUNTIME_AUTHORITY_V5),)
)
PRIVATE_ALLOWED_CODEX_CLI_COMMANDS = [
    "codex-capture",
    "codex-close-capture",
    "codex-close-request",
    "codex-plan",
    "codex-prefix-proof",
    "codex-prefix-request",
    "codex-prefix-requests",
    "codex-private-apply",
    "codex-private-capture",
    "codex-private-install-plan",
    "codex-private-plan",
    "codex-private-quiescence-attest",
    "codex-private-recover",
    "codex-private-rollback",
    "codex-private-sqlite-capacity-observe",
    "codex-private-sqlite-compose-request",
    "codex-private-verify",
]
PRIVATE_FORBIDDEN_COMMANDS = [
    "codex-private-combined-apply",
    "codex-private-sqlite-compose",
    "codex-state-apply",
]
PRIVATE_STATE_CLASSES = {
    "auth_file": {
        "authority": "CODEX_HOME/auth.json",
        "classification": "copy-eligible",
        "copy_method": "pinned-private-file-atomic-replace",
        "default": "opt-in",
        "destination_evidence": "fresh-auth-and-sqlite-preservation-capture",
        "accepted_input_matrices": [
            {
                "destination": ["auth", "sqlite"],
                "source": ["auth"],
            },
            {
                "destination": ["auth", "sqlite"],
                "source": ["auth", "sqlite"],
            },
        ],
        "installer_implemented": True,
        "preferred_source_state_classes": ["auth"],
        "quiescence": "operator-attested-plus-cooperating-bulkload-lock",
        "reader_implemented": True,
        "runtime_acceptance_required": True,
        "source_input": "auth-only-capture-allowed",
    },
    "sqlite_families": {
        "auth_install_action": "preserve-destination-exact",
        "authority": "explicit-effective-sqlite-home",
        "classification": "copy-eligible",
        "composer_implemented": False,
        "copy_method": "sqlite-immutable-backup-api",
        "default": "opt-in",
        "preservation_implemented": True,
        "live_sidecars": "reject-wal-shm-journal",
        "post_plan_close_required": True,
        "provider_writer_proof": False,
        "publisher_implemented": False,
        "raw_database_wal_shm_copy": False,
        "reader_implemented": True,
        "runtime_acceptance_required": True,
        "session_union_execution_verified": False,
        "source_sqlite_required_for_auth_install": False,
        "legacy_sqlite_opening_validator_implemented": True,
        "legacy_sqlite_close_action_validator_implemented": True,
        "sqlite_compose_plan_implemented": False,
        "sqlite_compose_action_plan_implemented": False,
        "sqlite_compose_request_implemented": True,
        "capacity_observation_implemented": True,
        "workspace_reservation_implemented": False,
        "sqlite_compose_request_scope": "exact-accepted-h6-v5-input-only",
        "post_plan_close_implemented": True,
        "wal_aware_capture": False,
    },
}
MAX_PRIVATE_POLICY_BYTES = 1024 * 1024
MAX_PRIVATE_RUNTIME_SOURCE_BYTES = 16 * 1024 * 1024
_PROCESS_RUNTIME_AUTHORITY: Any | None = None


def _require_exact_keys(value: dict[str, Any], expected: set[str], label: str) -> None:
    if set(value) != expected:
        raise BulkloadError(f"{label} keys differ from the exact contract")


def _require_sha256(value: Any, label: str) -> str:
    if (
        not isinstance(value, str)
        or len(value) != 64
        or any(character not in "0123456789abcdef" for character in value)
    ):
        raise BulkloadError(f"{label} must be a lowercase SHA-256 digest")
    return value


def _unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, item in pairs:
        if key in value:
            raise ValueError(f"duplicate JSON key {key!r} is forbidden")
        value[key] = item
    return value


def _reject_nonfinite(value: str) -> None:
    raise ValueError(f"non-finite JSON number {value} is forbidden")


def _strict_json_object(payload: bytes, label: str) -> dict[str, Any]:
    try:
        text = payload.decode("utf-8")
        value = json.loads(
            text,
            object_pairs_hook=_unique_object,
            parse_constant=_reject_nonfinite,
        )
    except (UnicodeDecodeError, json.JSONDecodeError, ValueError) as error:
        raise BulkloadError(f"{label} is not strict JSON") from error
    if not isinstance(value, dict):
        raise BulkloadError(f"{label} must contain one JSON object")
    if canonical_bytes(value) + b"\n" != payload:
        raise BulkloadError(f"{label} is not canonical JSON with one newline")
    return value


def _skill_root() -> Path:
    return Path(__file__).resolve().parents[2]


def _runtime_paths(skill_root: Path) -> list[Path]:
    scripts_root = skill_root / "scripts"
    return sorted(scripts_root.rglob("*.py"))


def _relative(skill_root: Path, path: Path) -> str:
    try:
        return path.relative_to(skill_root).as_posix()
    except ValueError as error:
        raise BulkloadError("private runtime source escapes the skill root") from error


def _read_bounded_descriptor(
    descriptor: int,
    *,
    maximum: int,
    label: str,
) -> bytes:
    try:
        before = os.fstat(descriptor)
        if (
            not stat.S_ISREG(before.st_mode)
            or before.st_size < 1
            or before.st_size > maximum
        ):
            raise BulkloadError(f"{label} custody or size is invalid")
        os.lseek(descriptor, 0, os.SEEK_SET)
        payload = b""
        while len(payload) <= maximum:
            block = os.read(
                descriptor,
                min(65536, maximum + 1 - len(payload)),
            )
            if not block:
                break
            payload += block
        after = os.fstat(descriptor)
    except OSError as error:
        raise BulkloadError(f"cannot read {label}") from error
    stable_before = (
        before.st_dev,
        before.st_ino,
        before.st_mode,
        before.st_nlink,
        before.st_size,
        before.st_mtime_ns,
        before.st_ctime_ns,
    )
    stable_after = (
        after.st_dev,
        after.st_ino,
        after.st_mode,
        after.st_nlink,
        after.st_size,
        after.st_mtime_ns,
        after.st_ctime_ns,
    )
    if len(payload) != before.st_size or stable_before != stable_after:
        raise BulkloadError(f"{label} changed while reading")
    return payload


def _open_runtime_file(path: Path, *, maximum: int, label: str) -> tuple[int, bytes]:
    try:
        descriptor = os.open(
            path,
            os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0),
        )
    except OSError as error:
        raise BulkloadError(f"cannot open {label}") from error
    try:
        return descriptor, _read_bounded_descriptor(
            descriptor,
            maximum=maximum,
            label=label,
        )
    except BaseException:
        os.close(descriptor)
        raise


def _runtime_digest(payloads: dict[str, bytes]) -> str:
    inventory = {
        path: sha256_bytes(payload) for path, payload in sorted(payloads.items())
    }
    return sha256_bytes(canonical_bytes(inventory))


def _validate_policy(
    policy: dict[str, Any],
    *,
    payloads: dict[str, bytes],
    runtime_sha256: str,
) -> None:
    _require_exact_keys(
        policy,
        {
            "schema",
            "implementation",
            "readiness",
            "source_digests",
            "runtime_source_sha256",
            "allowed_codex_cli_commands",
            "state_classes",
            "forbidden_commands",
        },
        "private-state policy",
    )
    if (
        policy["schema"] != PRIVATE_STATE_POLICY_SCHEMA
        or policy["implementation"] != PRIVATE_INSTALL_IMPLEMENTATION
        or policy["readiness"]
        != {
            "auth_install": True,
            "sqlite_compose_action_plan": False,
            "sqlite_compose_request": True,
            "sqlite_capacity_observation": True,
            "sqlite_compose_plan": False,
            "sqlite_compose": False,
            "sqlite_publish": False,
            "combined": False,
        }
    ):
        raise BulkloadError("private-state policy readiness differs")
    source_digests = policy["source_digests"]
    if (
        not isinstance(source_digests, dict)
        or set(source_digests) != PRIVATE_RUNTIME_SOURCE_KEYS
    ):
        raise BulkloadError("private-state policy source inventory differs")
    for path, expected in source_digests.items():
        _require_sha256(expected, f"private-state policy {path}")
        observed = sha256_bytes(payloads[path])
        if observed != expected:
            raise BulkloadError(f"private-state policy source digest differs: {path}")
    _require_sha256(
        policy["runtime_source_sha256"],
        "private-state policy runtime digest",
    )
    if policy["runtime_source_sha256"] != runtime_sha256:
        raise BulkloadError("private-state policy runtime closure differs")
    if policy["allowed_codex_cli_commands"] != PRIVATE_ALLOWED_CODEX_CLI_COMMANDS:
        raise BulkloadError("private-state policy CLI allowlist differs")
    if policy["state_classes"] != PRIVATE_STATE_CLASSES:
        raise BulkloadError("private-state policy state classes differ")
    if policy["forbidden_commands"] != PRIVATE_FORBIDDEN_COMMANDS:
        raise BulkloadError("private-state policy forbidden commands differ")


def validate_private_runtime_authority(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise BulkloadError("private runtime authority must be an object")
    _require_exact_keys(
        value,
        {
            "schema",
            "policy_schema",
            "policy_sha256",
            "runtime_source_sha256",
            "source_digests",
        },
        "private runtime authority",
    )
    if value["schema"] != PRIVATE_RUNTIME_AUTHORITY_SCHEMA:
        raise BulkloadError("private runtime authority schema differs")
    for key in ("policy_sha256", "runtime_source_sha256"):
        _require_sha256(value[key], f"private runtime authority {key}")
    sources = value["source_digests"]
    if not isinstance(sources, dict) or any(
        not isinstance(path, str) for path in sources
    ):
        raise BulkloadError("private runtime authority source inventory differs")
    for path, digest in sources.items():
        _require_sha256(digest, f"private runtime authority {path}")
    if value["policy_schema"] == LEGACY_PRIVATE_STATE_POLICY_SCHEMA_V4:
        if canonical_bytes(value) not in _ACCEPTED_PRIVATE_RUNTIME_AUTHORITIES_V4:
            raise BulkloadError(
                "legacy private runtime authority differs from exact v4 closures"
            )
        return value
    if value["policy_schema"] == LEGACY_PRIVATE_STATE_POLICY_SCHEMA_V5:
        if canonical_bytes(value) not in _ACCEPTED_PRIVATE_RUNTIME_AUTHORITIES_V5:
            raise BulkloadError(
                "legacy private runtime authority differs from exact accepted-H6 "
                "v5 closure"
            )
        return value
    if (
        value["policy_schema"] != PRIVATE_STATE_POLICY_SCHEMA
        or set(sources) != PRIVATE_RUNTIME_SOURCE_KEYS
    ):
        raise BulkloadError("private runtime authority source inventory differs")
    return value


class PinnedPrivateRuntimeAuthority:
    """Hold exact runtime and policy artifacts open across one operation."""

    def __init__(
        self,
        *,
        skill_root: Path,
        paths: dict[str, Path],
        descriptors: dict[str, int],
        payloads: dict[str, bytes],
        record: dict[str, Any],
    ) -> None:
        self._skill_root = skill_root
        self._paths = paths
        self._descriptors = descriptors
        self._payloads = payloads
        self._record = record
        self._closed = False

    @property
    def record(self) -> dict[str, Any]:
        return json.loads(json.dumps(self._record))

    def revalidate(self) -> None:
        if self._closed:
            raise BulkloadError("private runtime authority is closed")
        observed_paths = {
            _relative(self._skill_root, path): path
            for path in _runtime_paths(self._skill_root)
        }
        observed_paths[f"references/{PRIVATE_STATE_POLICY_NAME}"] = (
            self._skill_root / "references" / PRIVATE_STATE_POLICY_NAME
        )
        if observed_paths != self._paths:
            raise BulkloadError("private runtime source inventory changed")
        payloads: dict[str, bytes] = {}
        for relative, descriptor in self._descriptors.items():
            maximum = (
                MAX_PRIVATE_POLICY_BYTES
                if relative == f"references/{PRIVATE_STATE_POLICY_NAME}"
                else MAX_PRIVATE_RUNTIME_SOURCE_BYTES
            )
            payload = _read_bounded_descriptor(
                descriptor,
                maximum=maximum,
                label=f"private runtime artifact {relative}",
            )
            try:
                entry = os.stat(
                    self._paths[relative],
                    follow_symlinks=False,
                )
                opened = os.fstat(descriptor)
            except OSError as error:
                raise BulkloadError(
                    f"cannot revalidate private runtime artifact {relative}"
                ) from error
            if (
                entry.st_dev,
                entry.st_ino,
                entry.st_size,
                entry.st_mtime_ns,
                entry.st_ctime_ns,
            ) != (
                opened.st_dev,
                opened.st_ino,
                opened.st_size,
                opened.st_mtime_ns,
                opened.st_ctime_ns,
            ):
                raise BulkloadError(
                    f"private runtime artifact binding changed: {relative}"
                )
            if payload != self._payloads[relative]:
                raise BulkloadError(
                    f"private runtime artifact content changed: {relative}"
                )
            payloads[relative] = payload
        policy_relative = f"references/{PRIVATE_STATE_POLICY_NAME}"
        policy_payload = payloads.pop(policy_relative)
        policy = _strict_json_object(policy_payload, "private-state policy")
        runtime_sha256 = _runtime_digest(payloads)
        _validate_policy(
            policy,
            payloads=payloads,
            runtime_sha256=runtime_sha256,
        )
        record = {
            "schema": PRIVATE_RUNTIME_AUTHORITY_SCHEMA,
            "policy_schema": policy["schema"],
            "policy_sha256": sha256_bytes(policy_payload),
            "runtime_source_sha256": runtime_sha256,
            "source_digests": policy["source_digests"],
        }
        validate_private_runtime_authority(record)
        if record != self._record:
            raise BulkloadError("private runtime authority changed")

    def close(self) -> None:
        if self._closed:
            return
        self._closed = True
        for descriptor in reversed(tuple(self._descriptors.values())):
            try:
                os.close(descriptor)
            except OSError:
                pass

    def __enter__(self) -> "PinnedPrivateRuntimeAuthority":
        self.revalidate()
        return self

    def __exit__(self, *_: object) -> None:
        self.close()


class ProcessPrivateRuntimeAuthorityLease:
    """Non-owning lease on the pre-import process runtime authority."""

    def __init__(self, authority: Any) -> None:
        self._authority = authority
        self._closed = False

    @property
    def record(self) -> dict[str, Any]:
        if self._closed:
            raise BulkloadError("private runtime authority lease is closed")
        return json.loads(json.dumps(self._authority.record))

    def revalidate(self) -> None:
        if self._closed:
            raise BulkloadError("private runtime authority lease is closed")
        self._authority.revalidate()

    def close(self) -> None:
        self._closed = True

    def __enter__(self) -> "ProcessPrivateRuntimeAuthorityLease":
        self.revalidate()
        return self

    def __exit__(self, *_: object) -> None:
        self.close()


def bind_process_private_runtime_authority(authority: Any) -> None:
    """Bind the stdlib bootstrap authority before private command dispatch."""
    global _PROCESS_RUNTIME_AUTHORITY
    if _PROCESS_RUNTIME_AUTHORITY is not None:
        if _PROCESS_RUNTIME_AUTHORITY is authority:
            return
        raise BulkloadError("private process runtime authority is already bound")
    try:
        record = authority.record
        authority.revalidate()
    except (AttributeError, TypeError) as error:
        raise BulkloadError("private process runtime authority is invalid") from error
    validate_private_runtime_authority(record)
    _PROCESS_RUNTIME_AUTHORITY = authority


def open_pinned_private_runtime_authority(
    expected: dict[str, Any] | None = None,
) -> ProcessPrivateRuntimeAuthorityLease:
    if _PROCESS_RUNTIME_AUTHORITY is None:
        raise BulkloadError(
            "private runtime authority requires the pre-import bootstrap binding"
        )
    lease = ProcessPrivateRuntimeAuthorityLease(_PROCESS_RUNTIME_AUTHORITY)
    try:
        record = lease.record
        validate_private_runtime_authority(record)
        if expected is not None:
            validate_private_runtime_authority(expected)
            if record != expected:
                raise BulkloadError(
                    "process runtime differs from accepted plan authority"
                )
        lease.revalidate()
        return lease
    except BaseException:
        lease.close()
        raise


def _open_disk_private_runtime_authority_for_tests(
    expected: dict[str, Any] | None = None,
) -> PinnedPrivateRuntimeAuthority:
    """Open current disk bytes only for isolated validator and test fixtures."""
    skill_root = _skill_root()
    paths = {_relative(skill_root, path): path for path in _runtime_paths(skill_root)}
    policy_relative = f"references/{PRIVATE_STATE_POLICY_NAME}"
    paths[policy_relative] = skill_root / "references" / PRIVATE_STATE_POLICY_NAME
    descriptors: dict[str, int] = {}
    payloads: dict[str, bytes] = {}
    try:
        for relative, path in sorted(paths.items()):
            descriptor, payload = _open_runtime_file(
                path,
                maximum=(
                    MAX_PRIVATE_POLICY_BYTES
                    if relative == policy_relative
                    else MAX_PRIVATE_RUNTIME_SOURCE_BYTES
                ),
                label=f"private runtime artifact {relative}",
            )
            descriptors[relative] = descriptor
            payloads[relative] = payload
        policy_payload = payloads.pop(policy_relative)
        policy = _strict_json_object(policy_payload, "private-state policy")
        runtime_sha256 = _runtime_digest(payloads)
        _validate_policy(
            policy,
            payloads=payloads,
            runtime_sha256=runtime_sha256,
        )
        record = {
            "schema": PRIVATE_RUNTIME_AUTHORITY_SCHEMA,
            "policy_schema": policy["schema"],
            "policy_sha256": sha256_bytes(policy_payload),
            "runtime_source_sha256": runtime_sha256,
            "source_digests": policy["source_digests"],
        }
        validate_private_runtime_authority(record)
        if expected is not None:
            validate_private_runtime_authority(expected)
            if record != expected:
                raise BulkloadError(
                    "current private runtime differs from accepted plan authority"
                )
        payloads[policy_relative] = policy_payload
        context = PinnedPrivateRuntimeAuthority(
            skill_root=skill_root,
            paths=paths,
            descriptors=descriptors,
            payloads=payloads,
            record=record,
        )
        context.revalidate()
        return context
    except BaseException:
        for descriptor in reversed(tuple(descriptors.values())):
            try:
                os.close(descriptor)
            except OSError:
                pass
        raise


def _unbind_process_private_runtime_authority_for_tests(authority: Any) -> None:
    """Release a test fixture binding; operational code must never call this."""
    global _PROCESS_RUNTIME_AUTHORITY
    if _PROCESS_RUNTIME_AUTHORITY is not authority:
        raise BulkloadError("private process runtime test authority differs")
    _PROCESS_RUNTIME_AUTHORITY = None


def current_private_runtime_authority() -> dict[str, Any]:
    with open_pinned_private_runtime_authority() as pinned:
        return pinned.record
