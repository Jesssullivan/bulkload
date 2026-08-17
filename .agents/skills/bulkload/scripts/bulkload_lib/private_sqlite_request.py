"""Non-actionable SQLite compose requests and volatile capacity observations.

This module does not compose, reserve, publish, install, or activate SQLite
state.  A request records exact intent and a deterministic preflight budget.
A separate observation records one short-lived view of caller-available
filesystem capacity.  Neither artifact is execution authority.
"""

from __future__ import annotations

from copy import deepcopy
from datetime import UTC, datetime, timedelta
import os
from pathlib import Path
import re
import stat
from typing import Any, Sequence
import uuid

from . import private_runtime
from .model import (
    BulkloadError,
    canonical_bytes,
    object_digest,
    sha256_bytes,
    utc_now,
)
from .private_sqlite_action_plan import (
    PRIVATE_SQLITE_ACTION_PLAN_SCHEMA,
    validate_codex_private_sqlite_action_plan,
)
from .private_sqlite_plan import MAX_SQLITE_PLAN_BYTES


PRIVATE_SQLITE_COMPOSE_REQUEST_SCHEMA = (
    "dev.tinyland.bulkload.codex-private-sqlite-compose-request.v6"
)
PRIVATE_SQLITE_CAPACITY_OBSERVATION_SCHEMA = (
    "dev.tinyland.bulkload.codex-private-sqlite-capacity-observation.v6"
)
PRIVATE_SQLITE_COMPOSE_REQUEST_IMPLEMENTATION = (
    "sqlite-compose-request-capacity-preflight-only-v6"
)
PRIVATE_SQLITE_CAPACITY_OBSERVATION_IMPLEMENTATION = (
    "sqlite-compose-capacity-observation-only-v6"
)

CAPACITY_DERIVATION = (
    "input-snapshots-plus-double-classified-rows-three-copy-contingency-v1"
)
DEFAULT_CAPACITY_OBSERVATION_TTL_SECONDS = 300
MAX_CAPACITY_OBSERVATION_TTL_SECONDS = 900
MAX_PRIVATE_SQLITE_REQUEST_BYTES = 8 * 1024 * 1024
MAX_CHECKED_BYTES = (1 << 63) - 1
SQLITE_MAX_PAGE_BYTES = 65536
CAPACITY_SAFETY_MARGIN_BYTES = 1024 * 1024 * 1024
CAPACITY_METADATA_FLOOR_BYTES = 64 * 1024 * 1024
STAGING_PREFIX = ".bulkload-sqlite-compose-"
SEALED_OUTPUT_PREFIX = "bulkload-sqlite-compose-"
MAX_DIRECTORY_LINEAGE = 128
REQUIRED_PROTECTED_NAMESPACE_ROLES = {
    "opening-private-bundle": {
        "source_a",
        "source_b",
        "destination_a",
        "destination_b",
    },
    "closing-private-bundle": {
        "source_a",
        "source_b",
        "destination_a",
        "destination_b",
    },
    "request-runtime-root": {"consumer"},
    "request-output-parent": {"private-json"},
    "live-private-root": {
        "source:codex_home",
        "source:sqlite_home",
        "destination:codex_home",
        "destination:sqlite_home",
    },
    "live-session-root": {
        "opening_session_source_a",
        "opening_session_source_b",
        "opening_session_destination_a",
        "opening_session_destination_b",
        "session_source_a",
        "session_source_b",
        "session_destination_a",
        "session_destination_b",
    },
}
CONTAINER_PROTECTED_NAMESPACE_CLASSES = {
    "closing-private-bundle",
    "opening-private-bundle",
    "protocol-evidence-parent",
}
STORAGE_EVIDENCE_NAMESPACE_CLASSES = {
    "closing-private-bundle",
    "opening-private-bundle",
    "protocol-evidence-parent",
    "request-output-parent",
}
LIVE_RUNTIME_NAMESPACE_CLASSES = {
    "live-private-root",
    "live-session-root",
    "request-runtime-root",
}

_SHA256 = re.compile(r"[0-9a-f]{64}")
_LEAF = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,254}")
_UTC_SECONDS = re.compile(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z")


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
        raise BulkloadError(f"{label} must be a UTC timestamp")
    try:
        parsed = datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=UTC)
    except ValueError as error:
        raise BulkloadError(f"{label} must be a UTC timestamp") from error
    if _format_timestamp(parsed) != value:
        raise BulkloadError(f"{label} must be a UTC timestamp")
    return parsed


def _format_timestamp(value: datetime) -> str:
    return value.astimezone(UTC).isoformat(timespec="seconds").replace("+00:00", "Z")


def _checked_add(*values: int, label: str) -> int:
    total = 0
    for value in values:
        if type(value) is not int or value < 0:
            raise BulkloadError(f"{label} contains an invalid byte count")
        total += value
        if total > MAX_CHECKED_BYTES:
            raise BulkloadError(f"{label} exceeds the checked integer range")
    return total


def _checked_multiply(left: int, right: int, *, label: str) -> int:
    if type(left) is not int or type(right) is not int or left < 0 or right < 0:
        raise BulkloadError(f"{label} contains an invalid multiplier")
    value = left * right
    if value > MAX_CHECKED_BYTES:
        raise BulkloadError(f"{label} exceeds the checked integer range")
    return value


def _canonical_leaf(value: str, label: str) -> str:
    if (
        not isinstance(value, str)
        or "\x00" in value
        or "/" in value
        or value in {"", ".", ".."}
        or _LEAF.fullmatch(value) is None
    ):
        raise BulkloadError(f"{label} must be one safe leaf name")
    if value.startswith(STAGING_PREFIX):
        raise BulkloadError(f"{label} overlaps the reserved staging namespace")
    return value


def derived_compose_output_leaf(action_plan_sha256: str) -> str:
    _require_sha256(action_plan_sha256, "compose action-plan digest")
    return f"{SEALED_OUTPUT_PREFIX}{action_plan_sha256}"


def _derived_staging_prefix(final_leaf: str) -> str:
    _canonical_leaf(final_leaf, "compose final leaf")
    return f".{final_leaf}-staging-"


def _normalized_absolute(path: Path, label: str) -> Path:
    expanded = path.expanduser()
    if "\x00" in os.fspath(expanded):
        raise BulkloadError(f"{label} must not contain NUL")
    if ".." in expanded.parts:
        raise BulkloadError(f"{label} must not contain parent traversal")
    try:
        return Path(os.path.abspath(os.fspath(expanded)))
    except (OSError, TypeError, ValueError) as error:
        raise BulkloadError(f"cannot normalize {label}") from error


def _directory_identity(info: os.stat_result) -> dict[str, int]:
    return {
        "device": info.st_dev,
        "inode": info.st_ino,
        "uid": info.st_uid,
        "mode": stat.S_IMODE(info.st_mode),
    }


def _linux_mount_id(descriptor: int) -> int | None:
    if not os.path.exists("/proc/self/fdinfo"):
        return None
    try:
        payload = Path(f"/proc/self/fdinfo/{descriptor}").read_text()
    except OSError as error:
        raise BulkloadError("cannot bind the Linux mount identity") from error
    for line in payload.splitlines():
        if line.startswith("mnt_id:"):
            try:
                return int(line.split(":", 1)[1].strip())
            except ValueError as error:
                raise BulkloadError("Linux mount identity is invalid") from error
    raise BulkloadError("Linux mount identity is unavailable")


def _mount_identity(descriptor: int) -> dict[str, int | None]:
    info = os.fstat(descriptor)
    filesystem = os.fstatvfs(descriptor)
    filesystem_id = getattr(filesystem, "f_fsid", None)
    if filesystem_id is not None:
        filesystem_id = int(filesystem_id)
    linux_mount_id = _linux_mount_id(descriptor)
    if filesystem_id is None and linux_mount_id is None:
        raise BulkloadError("filesystem mount identity is unavailable")
    return {
        "device": info.st_dev,
        "filesystem_id": filesystem_id,
        "linux_mount_id": linux_mount_id,
    }


def _open_absolute_directory(
    path: Path,
    label: str,
    *,
    require_private: bool,
) -> tuple[Path, int, os.stat_result]:
    normalized = _normalized_absolute(path, label)
    flags = (
        os.O_RDONLY
        | getattr(os, "O_DIRECTORY", 0)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_CLOEXEC", 0)
    )
    try:
        descriptor = os.open(normalized.anchor or "/", flags)
    except OSError as error:
        raise BulkloadError(f"cannot open {label} anchor") from error
    traversed = Path(normalized.anchor or "/")
    try:
        components = (
            normalized.parts[1:] if normalized.is_absolute() else normalized.parts
        )
        for component in components:
            try:
                child = os.open(component, flags, dir_fd=descriptor)
            except OSError as error:
                raise BulkloadError(
                    f"{label} contains a non-directory or symlink component: "
                    f"{traversed / component}"
                ) from error
            os.close(descriptor)
            descriptor = child
            traversed /= component
        info = os.fstat(descriptor)
        if not stat.S_ISDIR(info.st_mode):
            raise BulkloadError(f"{label} must be a directory")
        if require_private and (
            info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o700
        ):
            raise BulkloadError(f"{label} must be an owner-private 0700 directory")
        return normalized, descriptor, info
    except BaseException:
        os.close(descriptor)
        raise


def _require_absent_path_without_symlink_ancestors(
    path: Path,
    label: str,
) -> None:
    normalized = _normalized_absolute(path, label)
    flags = (
        os.O_RDONLY
        | getattr(os, "O_DIRECTORY", 0)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_CLOEXEC", 0)
    )
    descriptor = os.open(normalized.anchor or "/", flags)
    try:
        components = normalized.parts[1:]
        for index, component in enumerate(components):
            final = index == len(components) - 1
            if final:
                try:
                    os.stat(
                        component,
                        dir_fd=descriptor,
                        follow_symlinks=False,
                    )
                except FileNotFoundError:
                    return
                except OSError as error:
                    raise BulkloadError(f"cannot observe {label}") from error
                raise BulkloadError(f"{label} unexpectedly exists")
            try:
                child = os.open(component, flags, dir_fd=descriptor)
            except FileNotFoundError:
                return
            except OSError as error:
                raise BulkloadError(
                    f"{label} contains a non-directory or symlink component"
                ) from error
            os.close(descriptor)
            descriptor = child
    finally:
        os.close(descriptor)
    raise BulkloadError(f"{label} unexpectedly exists")


def _lineage(descriptor: int) -> list[dict[str, Any]]:
    flags = (
        os.O_RDONLY
        | getattr(os, "O_DIRECTORY", 0)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_CLOEXEC", 0)
    )
    current = os.dup(descriptor)
    records: list[dict[str, Any]] = []
    seen: set[bytes] = set()
    try:
        for _ in range(MAX_DIRECTORY_LINEAGE):
            info = os.fstat(current)
            record = {
                "identity": _directory_identity(info),
                "mount": _mount_identity(current),
            }
            key = canonical_bytes(record)
            if key in seen:
                raise BulkloadError("directory lineage contains a cycle")
            seen.add(key)
            records.append(record)
            parent = os.open("..", flags, dir_fd=current)
            parent_info = os.fstat(parent)
            parent_mount = _mount_identity(parent)
            if (
                parent_info.st_dev,
                parent_info.st_ino,
                parent_mount,
            ) == (
                info.st_dev,
                info.st_ino,
                record["mount"],
            ):
                os.close(parent)
                return records
            os.close(current)
            current = parent
    finally:
        os.close(current)
    raise BulkloadError("directory lineage exceeds its bound")


def _identity_key(record: dict[str, Any]) -> tuple[int, int]:
    identity = record["identity"]
    return (identity["device"], identity["inode"])


def _path_is_within(candidate: Path, root: Path) -> bool:
    try:
        candidate.relative_to(root)
    except ValueError:
        return False
    return True


def _assert_leaf_absent(descriptor: int, leaf: str, label: str) -> None:
    try:
        os.stat(leaf, dir_fd=descriptor, follow_symlinks=False)
    except FileNotFoundError:
        return
    except OSError as error:
        raise BulkloadError(f"cannot observe {label}") from error
    raise BulkloadError(f"{label} already exists")


def _assert_staging_namespace_empty(descriptor: int, prefix: str) -> None:
    try:
        with os.scandir(descriptor) as entries:
            occupied = sorted(
                entry.name for entry in entries if entry.name.startswith(prefix)
            )
    except OSError as error:
        raise BulkloadError("cannot observe the compose staging namespace") from error
    if occupied:
        raise BulkloadError("compose staging namespace is not empty")


class _ProtectedDirectory:
    def __init__(
        self,
        *,
        namespace_class: str,
        role: str,
        path: Path,
        require_private: bool,
        allow_absent: bool,
    ) -> None:
        if not namespace_class or not role:
            raise BulkloadError("protected namespace class and role are required")
        self.namespace_class = namespace_class
        self.role = role
        self.path = _normalized_absolute(path, "protected namespace")
        self.require_private = require_private
        self.allow_absent = allow_absent
        self.descriptor = -1
        self.identity: dict[str, int] | None = None
        self.mount: dict[str, int | None] | None = None
        self.lineage: list[dict[str, Any]] | None = None
        try:
            _, descriptor, info = _open_absolute_directory(
                self.path,
                "protected namespace",
                require_private=require_private,
            )
        except BulkloadError:
            if not allow_absent:
                raise
            _require_absent_path_without_symlink_ancestors(
                self.path,
                "protected namespace",
            )
            self.exists = False
            return
        self.exists = True
        self.descriptor = descriptor
        self.identity = _directory_identity(info)
        self.mount = _mount_identity(descriptor)
        self.lineage = _lineage(descriptor)

    def record(self) -> dict[str, Any]:
        return {
            "class": self.namespace_class,
            "role": self.role,
            "path": os.fspath(self.path),
            "exists": self.exists,
            "require_private": self.require_private,
            "identity": deepcopy(self.identity),
            "mount": deepcopy(self.mount),
            "lineage": deepcopy(self.lineage),
            "lineage_sha256": (
                sha256_bytes(canonical_bytes(self.lineage))
                if self.lineage is not None
                else None
            ),
        }

    def revalidate(self) -> None:
        if not self.exists:
            _require_absent_path_without_symlink_ancestors(
                self.path,
                "protected namespace",
            )
            return
        assert self.identity is not None
        assert self.mount is not None
        if (
            _directory_identity(os.fstat(self.descriptor)) != self.identity
            or _mount_identity(self.descriptor) != self.mount
        ):
            raise BulkloadError("protected namespace descriptor changed")
        reopened = -1
        try:
            _, reopened, info = _open_absolute_directory(
                self.path,
                "protected namespace",
                require_private=self.require_private,
            )
            if (
                _directory_identity(info) != self.identity
                or _mount_identity(reopened) != self.mount
                or sha256_bytes(canonical_bytes(_lineage(reopened)))
                != self.record()["lineage_sha256"]
            ):
                raise BulkloadError("protected namespace path binding changed")
        finally:
            if reopened >= 0:
                os.close(reopened)

    def close(self) -> None:
        if self.descriptor >= 0:
            os.close(self.descriptor)
            self.descriptor = -1


class PinnedComposeWorkspace:
    """Hold one absent, owner-private compose target namespace by descriptor."""

    def __init__(
        self,
        parent: Path,
        final_leaf: str,
        *,
        existing_roots: Sequence[tuple[str, str, Path, bool]],
        recorded_roots: Sequence[tuple[str, str, Path]],
    ) -> None:
        self.final_leaf = _canonical_leaf(final_leaf, "compose final leaf")
        self.staging_prefix = _derived_staging_prefix(self.final_leaf)
        self.parent, self.descriptor, info = _open_absolute_directory(
            parent,
            "compose workspace parent",
            require_private=True,
        )
        self.identity = _directory_identity(info)
        self.mount = _mount_identity(self.descriptor)
        self.lineage = _lineage(self.descriptor)
        self._protected: list[_ProtectedDirectory] = []
        try:
            specifications = [
                (namespace_class, role, path, require_private, False)
                for namespace_class, role, path, require_private in existing_roots
            ]
            specifications.extend(
                (namespace_class, role, path, True, True)
                for namespace_class, role, path in recorded_roots
            )
            seen: set[tuple[str, str, str]] = set()
            for (
                namespace_class,
                role,
                path,
                require_private,
                allow_absent,
            ) in sorted(
                specifications,
                key=lambda item: (item[0], item[1], os.fspath(item[2])),
            ):
                normalized = _normalized_absolute(path, "protected namespace")
                key = (namespace_class, role, os.fspath(normalized))
                if key in seen:
                    continue
                seen.add(key)
                if _path_is_within(self.parent, normalized) or _path_is_within(
                    normalized, self.parent
                ):
                    raise BulkloadError(
                        "compose workspace overlaps a protected namespace"
                    )
                protected = _ProtectedDirectory(
                    namespace_class=namespace_class,
                    role=role,
                    path=normalized,
                    require_private=require_private,
                    allow_absent=allow_absent,
                )
                self._assert_no_descriptor_overlap(protected)
                self._protected.append(protected)
            self._assert_storage_live_separation()
            self._assert_evidence_parent_separation()
            self._assert_output_parent_separation()
            _assert_leaf_absent(
                self.descriptor,
                self.final_leaf,
                "compose final target",
            )
            _assert_staging_namespace_empty(self.descriptor, self.staging_prefix)
        except BaseException:
            self.close()
            raise

    def _assert_no_descriptor_overlap(self, protected: _ProtectedDirectory) -> None:
        if not protected.exists:
            return
        assert protected.identity is not None
        assert protected.lineage is not None
        workspace_key = (self.identity["device"], self.identity["inode"])
        protected_key = (
            protected.identity["device"],
            protected.identity["inode"],
        )
        workspace_lineage = {_identity_key(record) for record in self.lineage}
        protected_lineage = {_identity_key(record) for record in protected.lineage}
        if (
            workspace_key == protected_key
            or workspace_key in protected_lineage
            or protected_key in workspace_lineage
        ):
            raise BulkloadError(
                "compose workspace aliases, contains, or descends from "
                "a protected namespace"
            )

    def _assert_evidence_parent_separation(self) -> None:
        evidence_classes = {
            "protocol-evidence-parent",
            "request-output-parent",
        }
        container_classes = {
            "closing-private-bundle",
            "opening-private-bundle",
        }
        evidence = [
            item for item in self._protected if item.namespace_class in evidence_classes
        ]
        for parent in evidence:
            assert parent.exists
            assert parent.identity is not None
            assert parent.lineage is not None
            parent_key = (parent.identity["device"], parent.identity["inode"])
            parent_lineage = {_identity_key(record) for record in parent.lineage}
            for protected in self._protected:
                if protected.namespace_class in evidence_classes:
                    continue
                parent_within = _path_is_within(parent.path, protected.path)
                protected_within = _path_is_within(protected.path, parent.path)
                if parent_within or (
                    protected_within
                    and protected.namespace_class not in container_classes
                ):
                    raise BulkloadError(
                        "compose evidence parent overlaps a protected namespace"
                    )
                if not protected.exists:
                    continue
                assert protected.identity is not None
                assert protected.lineage is not None
                protected_key = (
                    protected.identity["device"],
                    protected.identity["inode"],
                )
                protected_lineage = {
                    _identity_key(record) for record in protected.lineage
                }
                parent_descends = protected_key in parent_lineage
                protected_descends = parent_key in protected_lineage
                if (
                    parent_key == protected_key
                    or parent_descends
                    or (
                        protected_descends
                        and protected.namespace_class not in container_classes
                    )
                ):
                    raise BulkloadError(
                        "compose evidence parent aliases a protected namespace"
                    )

    def _assert_storage_live_separation(self) -> None:
        storage = [
            item
            for item in self._protected
            if item.namespace_class in STORAGE_EVIDENCE_NAMESPACE_CLASSES
        ]
        live = [
            item
            for item in self._protected
            if item.namespace_class in LIVE_RUNTIME_NAMESPACE_CLASSES
        ]
        for stored in storage:
            for mutable in live:
                if _path_is_within(
                    stored.path,
                    mutable.path,
                ) or _path_is_within(mutable.path, stored.path):
                    raise BulkloadError(
                        "compose storage/evidence namespace overlaps a "
                        "live/runtime namespace"
                    )
                if not stored.exists or not mutable.exists:
                    continue
                assert stored.identity is not None
                assert stored.lineage is not None
                assert mutable.identity is not None
                assert mutable.lineage is not None
                stored_key = (
                    stored.identity["device"],
                    stored.identity["inode"],
                )
                mutable_key = (
                    mutable.identity["device"],
                    mutable.identity["inode"],
                )
                stored_lineage = {_identity_key(record) for record in stored.lineage}
                mutable_lineage = {_identity_key(record) for record in mutable.lineage}
                if (
                    stored_key == mutable_key
                    or stored_key in mutable_lineage
                    or mutable_key in stored_lineage
                ):
                    raise BulkloadError(
                        "compose storage/evidence namespace aliases, contains, "
                        "or descends from a live/runtime namespace"
                    )

    def _assert_output_parent_separation(self) -> None:
        outputs = [
            item
            for item in self._protected
            if item.namespace_class == "request-output-parent"
        ]
        if len(outputs) != 1:
            raise BulkloadError(
                "compose request requires one protected output evidence parent"
            )
        output = outputs[0]
        assert output.exists
        assert output.identity is not None
        assert output.lineage is not None
        output_key = (output.identity["device"], output.identity["inode"])
        output_lineage = {_identity_key(record) for record in output.lineage}
        for protected in self._protected:
            if protected is output:
                continue
            lexical_equal = output.path == protected.path
            if (
                lexical_equal
                and protected.namespace_class == "protocol-evidence-parent"
            ):
                continue
            output_within = _path_is_within(output.path, protected.path)
            protected_within = _path_is_within(protected.path, output.path)
            if output_within or (
                protected_within
                and protected.namespace_class
                not in CONTAINER_PROTECTED_NAMESPACE_CLASSES
            ):
                raise BulkloadError(
                    "compose request output parent overlaps a protected namespace"
                )
            if not protected.exists:
                continue
            assert protected.identity is not None
            assert protected.lineage is not None
            protected_key = (
                protected.identity["device"],
                protected.identity["inode"],
            )
            protected_lineage = {_identity_key(record) for record in protected.lineage}
            output_descends = protected_key in output_lineage
            protected_descends = output_key in protected_lineage
            if (
                output_key == protected_key
                or output_descends
                or (
                    protected_descends
                    and protected.namespace_class
                    not in CONTAINER_PROTECTED_NAMESPACE_CLASSES
                )
            ):
                raise BulkloadError(
                    "compose request output parent aliases a protected namespace"
                )

    def record(self) -> dict[str, Any]:
        protected = sorted(
            (item.record() for item in self._protected),
            key=canonical_bytes,
        )
        return {
            "resolved_parent": os.fspath(self.parent),
            "parent_identity": deepcopy(self.identity),
            "mount": deepcopy(self.mount),
            "lineage": deepcopy(self.lineage),
            "lineage_sha256": sha256_bytes(canonical_bytes(self.lineage)),
            "final_leaf": self.final_leaf,
            "target_observed_absent": True,
            "staging_prefix": self.staging_prefix,
            "staging_namespace_observed_empty": True,
            "same_parent_staging_required": True,
            "protected_namespaces": protected,
            "protected_namespaces_sha256": sha256_bytes(canonical_bytes(protected)),
        }

    def output_parent_binding(
        self,
    ) -> tuple[Path, int, tuple[int, int, int, int]]:
        outputs = [
            item
            for item in self._protected
            if item.namespace_class == "request-output-parent"
        ]
        if len(outputs) != 1 or not outputs[0].exists:
            raise BulkloadError("compose request output-parent binding differs")
        output = outputs[0]
        output.revalidate()
        info = os.fstat(output.descriptor)
        expected = (
            info.st_dev,
            info.st_ino,
            info.st_uid,
            stat.S_IMODE(info.st_mode),
        )
        return output.path, output.descriptor, expected

    def revalidate(self) -> None:
        if (
            _directory_identity(os.fstat(self.descriptor)) != self.identity
            or _mount_identity(self.descriptor) != self.mount
            or sha256_bytes(canonical_bytes(_lineage(self.descriptor)))
            != sha256_bytes(canonical_bytes(self.lineage))
        ):
            raise BulkloadError("compose workspace descriptor changed")
        reopened = -1
        try:
            _, reopened, info = _open_absolute_directory(
                self.parent,
                "compose workspace parent",
                require_private=True,
            )
            if (
                _directory_identity(info) != self.identity
                or _mount_identity(reopened) != self.mount
            ):
                raise BulkloadError("compose workspace path binding changed")
        finally:
            if reopened >= 0:
                os.close(reopened)
        for protected in self._protected:
            protected.revalidate()
            self._assert_no_descriptor_overlap(protected)
        self._assert_storage_live_separation()
        self._assert_evidence_parent_separation()
        self._assert_output_parent_separation()
        _assert_leaf_absent(
            self.descriptor,
            self.final_leaf,
            "compose final target",
        )
        _assert_staging_namespace_empty(self.descriptor, self.staging_prefix)

    def observe_capacity(self) -> dict[str, int]:
        self.revalidate()
        filesystem = os.fstatvfs(self.descriptor)
        fragment_size = int(filesystem.f_frsize or filesystem.f_bsize)
        available_blocks = int(filesystem.f_bavail)
        available_inodes = int(filesystem.f_favail)
        if fragment_size < 1 or available_blocks < 0 or available_inodes < 0:
            raise BulkloadError("caller-available filesystem capacity is invalid")
        available_bytes = _checked_multiply(
            fragment_size,
            available_blocks,
            label="caller-available filesystem capacity",
        )
        return {
            "fragment_size": fragment_size,
            "available_blocks": available_blocks,
            "available_bytes": available_bytes,
            "available_inodes": available_inodes,
        }

    def close(self) -> None:
        for protected in reversed(getattr(self, "_protected", [])):
            protected.close()
        if getattr(self, "descriptor", -1) >= 0:
            os.close(self.descriptor)
            self.descriptor = -1

    def __enter__(self) -> PinnedComposeWorkspace:
        return self

    def __exit__(self, exc_type: Any, exc: Any, traceback: Any) -> None:
        del exc_type, exc, traceback
        self.close()


def _validate_runtime_authority(value: Any, label: str) -> dict[str, Any]:
    try:
        return private_runtime.validate_private_runtime_authority(value)
    except BulkloadError as error:
        raise BulkloadError(f"{label} is invalid") from error


def _validate_v6_request_runtime_authority(
    value: Any,
    label: str,
) -> dict[str, Any]:
    authority = _validate_runtime_authority(value, label)
    if authority["policy_schema"] != private_runtime.PRIVATE_STATE_POLICY_SCHEMA:
        raise BulkloadError(f"{label} must use the active v6 consumer policy")
    return authority


def _validate_exact_action_producer(action_plan: dict[str, Any]) -> None:
    validate_codex_private_sqlite_action_plan(action_plan)
    if (
        action_plan["runtime_authority"]
        != private_runtime.ACCEPTED_H6_PRIVATE_RUNTIME_AUTHORITY_V5
    ):
        raise BulkloadError(
            "compose request requires the exact accepted-H6 v5 producer closure"
        )


def _action_binding(action_plan: dict[str, Any]) -> dict[str, Any]:
    return {
        "schema": action_plan["schema"],
        "action_plan_sha256": action_plan["action_plan_sha256"],
        "body_sha256": sha256_bytes(canonical_bytes(action_plan)),
        "opening_plan_sha256": action_plan["accepted_inputs"]["opening_plan"][
            "plan_sha256"
        ],
        "opening_inputs_sha256": action_plan["accepted_inputs"]["opening_plan"][
            "accepted_inputs_sha256"
        ],
        "close_request_sha256": action_plan["accepted_inputs"]["close_request"][
            "close_request_sha256"
        ],
        "close_body_sha256": action_plan["accepted_inputs"]["close_request"][
            "body_sha256"
        ],
    }


def _capacity_requirement(action_plan: dict[str, Any]) -> dict[str, Any]:
    _validate_exact_action_producer(action_plan)
    if (
        action_plan["readiness"]["descriptive_action_complete"] is not True
        or action_plan["blockers"]
    ):
        raise BulkloadError("compose request requires a complete action plan")
    family_count = len(action_plan["sqlite_families"])
    if family_count < 1:
        raise BulkloadError("compose request requires at least one SQLite family")
    snapshot_bytes = 0
    classified_bytes = 0
    for family in action_plan["sqlite_families"]:
        snapshot_bytes = _checked_add(
            snapshot_bytes,
            family["source_artifact"]["snapshot_size"],
            family["destination_artifact"]["snapshot_size"],
            label="compose input snapshot bytes",
        )
        for table in family["tables"]:
            expected = table["expected_output"]
            if expected is None:
                raise BulkloadError("compose request expected output is incomplete")
            classified_bytes = _checked_add(
                classified_bytes,
                expected["classified_bytes"],
                label="compose classified row bytes",
            )
    regular_output = _checked_add(
        snapshot_bytes,
        _checked_multiply(
            classified_bytes,
            2,
            label="compose classified-row contingency",
        ),
        label="compose regular output bound",
    )
    page_rounding = _checked_multiply(
        family_count,
        SQLITE_MAX_PAGE_BYTES,
        label="compose SQLite page rounding",
    )
    manifest_receipt = _checked_multiply(
        MAX_SQLITE_PLAN_BYTES,
        2,
        label="compose manifest and receipt bound",
    )
    required_inodes = _checked_add(
        family_count,
        8,
        label="compose inode requirement",
    )
    filesystem_metadata = max(
        CAPACITY_METADATA_FLOOR_BYTES,
        _checked_multiply(
            required_inodes,
            1024 * 1024,
            label="compose filesystem metadata bound",
        ),
    )
    required_bytes = _checked_add(
        regular_output,
        regular_output,
        regular_output,
        page_rounding,
        manifest_receipt,
        filesystem_metadata,
        CAPACITY_SAFETY_MARGIN_BYTES,
        label="compose total preflight requirement",
    )
    return {
        "derivation": CAPACITY_DERIVATION,
        "input_snapshot_bytes": snapshot_bytes,
        "classified_row_bytes": classified_bytes,
        "regular_output_upper_bound_bytes": regular_output,
        "temporary_duplicate_bytes": regular_output,
        "sqlite_journal_contingency_bytes": regular_output,
        "sqlite_page_rounding_bytes": page_rounding,
        "manifest_receipt_bytes": manifest_receipt,
        "filesystem_metadata_bytes": filesystem_metadata,
        "safety_margin_bytes": CAPACITY_SAFETY_MARGIN_BYTES,
        "required_bytes": required_bytes,
        "required_inodes": required_inodes,
    }


def _validate_capacity_requirement(value: Any) -> dict[str, Any]:
    requirement = _require_exact_keys(
        value,
        {
            "derivation",
            "input_snapshot_bytes",
            "classified_row_bytes",
            "regular_output_upper_bound_bytes",
            "temporary_duplicate_bytes",
            "sqlite_journal_contingency_bytes",
            "sqlite_page_rounding_bytes",
            "manifest_receipt_bytes",
            "filesystem_metadata_bytes",
            "safety_margin_bytes",
            "required_bytes",
            "required_inodes",
        },
        "compose capacity requirement",
    )
    if requirement["derivation"] != CAPACITY_DERIVATION:
        raise BulkloadError("compose capacity derivation differs")
    for key, item in requirement.items():
        if key == "derivation":
            continue
        if type(item) is not int or item < 0 or item > MAX_CHECKED_BYTES:
            raise BulkloadError(f"compose capacity {key} is invalid")
    if requirement["required_inodes"] < 1:
        raise BulkloadError("compose capacity inode requirement is invalid")
    expected_regular = _checked_add(
        requirement["input_snapshot_bytes"],
        _checked_multiply(
            requirement["classified_row_bytes"],
            2,
            label="compose classified-row contingency",
        ),
        label="compose regular output bound",
    )
    expected_total = _checked_add(
        expected_regular,
        requirement["temporary_duplicate_bytes"],
        requirement["sqlite_journal_contingency_bytes"],
        requirement["sqlite_page_rounding_bytes"],
        requirement["manifest_receipt_bytes"],
        requirement["filesystem_metadata_bytes"],
        requirement["safety_margin_bytes"],
        label="compose total preflight requirement",
    )
    if (
        requirement["regular_output_upper_bound_bytes"] != expected_regular
        or requirement["temporary_duplicate_bytes"] != expected_regular
        or requirement["sqlite_journal_contingency_bytes"] != expected_regular
        or requirement["safety_margin_bytes"] != CAPACITY_SAFETY_MARGIN_BYTES
        or requirement["required_bytes"] != expected_total
    ):
        raise BulkloadError("compose capacity requirement differs")
    return requirement


def _validate_mount(value: Any, label: str) -> dict[str, Any]:
    mount = _require_exact_keys(
        value,
        {"device", "filesystem_id", "linux_mount_id"},
        label,
    )
    if type(mount["device"]) is not int:
        raise BulkloadError(f"{label} device is invalid")
    for key in ("filesystem_id", "linux_mount_id"):
        if mount[key] is not None and type(mount[key]) is not int:
            raise BulkloadError(f"{label} {key} is invalid")
    if mount["filesystem_id"] is None and mount["linux_mount_id"] is None:
        raise BulkloadError(f"{label} is ambiguous")
    return mount


def _validate_recorded_lineage(
    value: Any,
    label: str,
    *,
    leaf_identity: dict[str, Any],
    leaf_mount: dict[str, Any],
) -> set[tuple[int, int]]:
    if not isinstance(value, list) or not value or len(value) > MAX_DIRECTORY_LINEAGE:
        raise BulkloadError(f"{label} is invalid")
    identities: set[tuple[int, int]] = set()
    first_identity: dict[str, Any] | None = None
    first_mount: dict[str, Any] | None = None
    for index, item in enumerate(value):
        record = _require_exact_keys(
            item,
            {"identity", "mount"},
            f"{label} record",
        )
        identity = _require_exact_keys(
            record["identity"],
            {"device", "inode", "uid", "mode"},
            f"{label} identity",
        )
        if any(type(identity[key]) is not int for key in identity):
            raise BulkloadError(f"{label} identity is invalid")
        mount = _validate_mount(record["mount"], f"{label} mount")
        if mount["device"] != identity["device"]:
            raise BulkloadError(f"{label} mount device differs")
        key = (identity["device"], identity["inode"])
        if key in identities:
            raise BulkloadError(f"{label} contains a cycle")
        identities.add(key)
        if index == 0:
            first_identity = identity
            first_mount = mount
    if first_identity != leaf_identity or first_mount != leaf_mount:
        raise BulkloadError(f"{label} leaf binding differs")
    return identities


def _recorded_bindings_overlap(
    left: dict[str, Any],
    left_lineage: set[tuple[int, int]],
    right: dict[str, Any],
    right_lineage: set[tuple[int, int]],
) -> bool:
    left_key = (left["identity"]["device"], left["identity"]["inode"])
    right_key = (right["identity"]["device"], right["identity"]["inode"])
    return (
        left_key == right_key or left_key in right_lineage or right_key in left_lineage
    )


def _validate_workspace(value: Any) -> dict[str, Any]:
    workspace = _require_exact_keys(
        value,
        {
            "resolved_parent",
            "parent_identity",
            "mount",
            "lineage",
            "lineage_sha256",
            "final_leaf",
            "target_observed_absent",
            "staging_prefix",
            "staging_namespace_observed_empty",
            "same_parent_staging_required",
            "protected_namespaces",
            "protected_namespaces_sha256",
        },
        "compose workspace",
    )
    path = workspace["resolved_parent"]
    if not isinstance(path, str) or not Path(path).is_absolute():
        raise BulkloadError("compose workspace path is invalid")
    normalized_parent = _normalized_absolute(
        Path(path),
        "compose workspace",
    )
    if os.fspath(normalized_parent) != path:
        raise BulkloadError("compose workspace path is not canonical")
    identity = _require_exact_keys(
        workspace["parent_identity"],
        {"device", "inode", "uid", "mode"},
        "compose workspace identity",
    )
    if any(type(identity[key]) is not int for key in identity):
        raise BulkloadError("compose workspace identity is invalid")
    if identity["uid"] != os.getuid() or identity["mode"] != 0o700:
        raise BulkloadError("compose workspace identity is not owner-private")
    workspace_mount = _validate_mount(
        workspace["mount"],
        "compose workspace mount",
    )
    workspace_lineage = _validate_recorded_lineage(
        workspace["lineage"],
        "compose workspace lineage",
        leaf_identity=identity,
        leaf_mount=workspace_mount,
    )
    _require_sha256(workspace["lineage_sha256"], "compose workspace lineage")
    if (
        sha256_bytes(canonical_bytes(workspace["lineage"]))
        != workspace["lineage_sha256"]
    ):
        raise BulkloadError("compose workspace lineage digest mismatch")
    _canonical_leaf(workspace["final_leaf"], "compose final leaf")
    if (
        workspace["target_observed_absent"] is not True
        or workspace["staging_prefix"]
        != _derived_staging_prefix(workspace["final_leaf"])
        or workspace["staging_namespace_observed_empty"] is not True
        or workspace["same_parent_staging_required"] is not True
    ):
        raise BulkloadError("compose workspace absence policy differs")
    protected = workspace["protected_namespaces"]
    if not isinstance(protected, list) or protected != sorted(
        protected, key=canonical_bytes
    ):
        raise BulkloadError("compose protected namespaces are not canonical")
    records_by_class: dict[str, list[dict[str, Any]]] = {}
    recorded_lineages: dict[tuple[str, str, str], set[tuple[int, int]]] = {}
    path_bindings: dict[
        str,
        tuple[bool, Any, Any, Any, Any],
    ] = {}
    seen_records: set[tuple[str, str, str]] = set()
    for item in protected:
        record = _require_exact_keys(
            item,
            {
                "class",
                "role",
                "path",
                "exists",
                "require_private",
                "identity",
                "mount",
                "lineage",
                "lineage_sha256",
            },
            "compose protected namespace",
        )
        if (
            not isinstance(record["class"], str)
            or not record["class"]
            or not isinstance(record["role"], str)
            or not record["role"]
            or not isinstance(record["path"], str)
            or not Path(record["path"]).is_absolute()
            or type(record["exists"]) is not bool
            or type(record["require_private"]) is not bool
        ):
            raise BulkloadError("compose protected namespace is invalid")
        normalized = _normalized_absolute(
            Path(record["path"]),
            "compose protected namespace",
        )
        if os.fspath(normalized) != record["path"]:
            raise BulkloadError("compose protected namespace path is not canonical")
        key = (record["class"], record["role"], record["path"])
        if key in seen_records:
            raise BulkloadError("compose protected namespace is duplicated")
        seen_records.add(key)
        records_by_class.setdefault(record["class"], []).append(record)
        if _path_is_within(Path(path), normalized) or _path_is_within(
            normalized,
            Path(path),
        ):
            raise BulkloadError("compose workspace overlaps a protected namespace")
        if record["exists"]:
            protected_identity = _require_exact_keys(
                record["identity"],
                {"device", "inode", "uid", "mode"},
                "compose protected namespace identity",
            )
            if any(
                type(protected_identity[key]) is not int for key in protected_identity
            ):
                raise BulkloadError("compose protected namespace identity is invalid")
            if record["require_private"] and (
                protected_identity["uid"] != os.getuid()
                or protected_identity["mode"] != 0o700
            ):
                raise BulkloadError("compose protected namespace is not owner-private")
            protected_mount = _validate_mount(
                record["mount"],
                "compose protected namespace mount",
            )
            lineage = _validate_recorded_lineage(
                record["lineage"],
                "compose protected namespace lineage",
                leaf_identity=protected_identity,
                leaf_mount=protected_mount,
            )
            _require_sha256(
                record["lineage_sha256"],
                "compose protected namespace lineage",
            )
            if (
                sha256_bytes(canonical_bytes(record["lineage"]))
                != record["lineage_sha256"]
            ):
                raise BulkloadError(
                    "compose protected namespace lineage digest mismatch"
                )
            recorded_lineages[key] = lineage
            if _recorded_bindings_overlap(
                {
                    "identity": identity,
                },
                workspace_lineage,
                record,
                lineage,
            ):
                raise BulkloadError(
                    "compose workspace aliases, contains, or descends from "
                    "a protected namespace"
                )
        elif any(
            record[key] is not None
            for key in ("identity", "mount", "lineage", "lineage_sha256")
        ):
            raise BulkloadError("absent protected namespace records identity")
        binding = (
            record["exists"],
            record["identity"],
            record["mount"],
            record["lineage"],
            record["lineage_sha256"],
        )
        previous_binding = path_bindings.setdefault(record["path"], binding)
        if previous_binding != binding:
            raise BulkloadError("compose protected namespace path bindings differ")
    if set(records_by_class) != {
        *REQUIRED_PROTECTED_NAMESPACE_ROLES,
        "protocol-evidence-parent",
    }:
        raise BulkloadError("compose protected namespace classes differ")
    for namespace_class, roles in REQUIRED_PROTECTED_NAMESPACE_ROLES.items():
        records = records_by_class[namespace_class]
        if {record["role"] for record in records} != roles or len(records) != len(
            roles
        ):
            raise BulkloadError(f"compose protected {namespace_class} roles differ")
    protocol_records = records_by_class["protocol-evidence-parent"]
    if not protocol_records or any(
        record["role"] != "private-json" for record in protocol_records
    ):
        raise BulkloadError("compose protocol-evidence parent roles differ")
    for namespace_class in (
        "opening-private-bundle",
        "closing-private-bundle",
        "protocol-evidence-parent",
        "request-output-parent",
    ):
        if any(
            record["exists"] is not True or record["require_private"] is not True
            for record in records_by_class[namespace_class]
        ):
            raise BulkloadError(f"compose protected {namespace_class} custody differs")
    runtime_records = records_by_class["request-runtime-root"]
    if (
        runtime_records[0]["exists"] is not True
        or runtime_records[0]["require_private"] is not False
    ):
        raise BulkloadError("compose request runtime-root custody differs")
    if any(
        record["require_private"] is not True
        for namespace_class in ("live-private-root", "live-session-root")
        for record in records_by_class[namespace_class]
    ):
        raise BulkloadError("compose recorded live-root custody differs")
    for storage_class in STORAGE_EVIDENCE_NAMESPACE_CLASSES:
        for stored in records_by_class[storage_class]:
            stored_path = Path(stored["path"])
            for live_class in LIVE_RUNTIME_NAMESPACE_CLASSES:
                for mutable in records_by_class[live_class]:
                    mutable_path = Path(mutable["path"])
                    if _path_is_within(
                        stored_path,
                        mutable_path,
                    ) or _path_is_within(mutable_path, stored_path):
                        raise BulkloadError(
                            "compose storage/evidence namespace overlaps a "
                            "live/runtime namespace"
                        )
                    if (
                        stored["exists"]
                        and mutable["exists"]
                        and _recorded_bindings_overlap(
                            stored,
                            recorded_lineages[
                                (
                                    stored["class"],
                                    stored["role"],
                                    stored["path"],
                                )
                            ],
                            mutable,
                            recorded_lineages[
                                (
                                    mutable["class"],
                                    mutable["role"],
                                    mutable["path"],
                                )
                            ],
                        )
                    ):
                        raise BulkloadError(
                            "compose storage/evidence namespace aliases, "
                            "contains, or descends from a live/runtime namespace"
                        )
    evidence_classes = {
        "protocol-evidence-parent",
        "request-output-parent",
    }
    evidence_records = [
        record
        for namespace_class in evidence_classes
        for record in records_by_class[namespace_class]
    ]
    for parent in evidence_records:
        parent_path = Path(parent["path"])
        for namespace_class, records in records_by_class.items():
            if namespace_class in evidence_classes:
                continue
            for record in records:
                protected_path = Path(record["path"])
                parent_within = _path_is_within(parent_path, protected_path)
                protected_within = _path_is_within(
                    protected_path,
                    parent_path,
                )
                if parent_within or (
                    protected_within
                    and namespace_class
                    not in {
                        "closing-private-bundle",
                        "opening-private-bundle",
                    }
                ):
                    raise BulkloadError(
                        "compose evidence parent overlaps a protected namespace"
                    )
                if parent["exists"] and record["exists"]:
                    parent_lineage = recorded_lineages[
                        (parent["class"], parent["role"], parent["path"])
                    ]
                    protected_lineage = recorded_lineages[
                        (record["class"], record["role"], record["path"])
                    ]
                    parent_key = (
                        parent["identity"]["device"],
                        parent["identity"]["inode"],
                    )
                    protected_key = (
                        record["identity"]["device"],
                        record["identity"]["inode"],
                    )
                    parent_descends = protected_key in parent_lineage
                    protected_descends = parent_key in protected_lineage
                    if (
                        parent_key == protected_key
                        or parent_descends
                        or (
                            protected_descends
                            and namespace_class
                            not in {
                                "closing-private-bundle",
                                "opening-private-bundle",
                            }
                        )
                    ):
                        raise BulkloadError(
                            "compose evidence parent aliases a protected namespace"
                        )
    output = records_by_class["request-output-parent"][0]
    output_path = Path(output["path"])
    for namespace_class, records in records_by_class.items():
        for record in records:
            if record is output:
                continue
            other_path = Path(record["path"])
            if (
                namespace_class == "protocol-evidence-parent"
                and other_path == output_path
            ):
                continue
            output_within = _path_is_within(output_path, other_path)
            other_within = _path_is_within(other_path, output_path)
            if output_within or (
                other_within
                and namespace_class not in CONTAINER_PROTECTED_NAMESPACE_CLASSES
            ):
                raise BulkloadError(
                    "compose request output parent overlaps a protected namespace"
                )
            if output["exists"] and record["exists"]:
                output_lineage = recorded_lineages[
                    (output["class"], output["role"], output["path"])
                ]
                other_lineage = recorded_lineages[
                    (record["class"], record["role"], record["path"])
                ]
                output_key = (
                    output["identity"]["device"],
                    output["identity"]["inode"],
                )
                other_key = (
                    record["identity"]["device"],
                    record["identity"]["inode"],
                )
                output_descends = other_key in output_lineage
                other_descends = output_key in other_lineage
                if (
                    output_key == other_key
                    or output_descends
                    or (
                        other_descends
                        and namespace_class not in CONTAINER_PROTECTED_NAMESPACE_CLASSES
                    )
                ):
                    raise BulkloadError(
                        "compose request output parent aliases a protected namespace"
                    )
    _require_sha256(
        workspace["protected_namespaces_sha256"],
        "compose protected namespaces digest",
    )
    if (
        sha256_bytes(canonical_bytes(protected))
        != workspace["protected_namespaces_sha256"]
    ):
        raise BulkloadError("compose protected namespaces digest mismatch")
    return workspace


def compile_codex_private_sqlite_compose_request(
    action_plan: dict[str, Any],
    workspace: dict[str, Any],
    *,
    accept_action_plan: str,
    request_runtime_authority: dict[str, Any],
    request_id: str | None = None,
    created_at: str | None = None,
) -> dict[str, Any]:
    """Compile one semantic, non-actionable v6 compose request."""
    _validate_exact_action_producer(action_plan)
    if accept_action_plan != action_plan["action_plan_sha256"]:
        raise BulkloadError("accepted private SQLite action-plan digest differs")
    _validate_workspace(workspace)
    if workspace["final_leaf"] != derived_compose_output_leaf(
        action_plan["action_plan_sha256"]
    ):
        raise BulkloadError("compose request output leaf differs from its action plan")
    _validate_v6_request_runtime_authority(
        request_runtime_authority,
        "compose request runtime authority",
    )
    if (
        request_runtime_authority
        == private_runtime.ACCEPTED_H6_PRIVATE_RUNTIME_AUTHORITY_V5
    ):
        raise BulkloadError(
            "compose request consumer and accepted-H6 v5 producer must be distinct"
        )
    identifier = request_id or str(uuid.uuid4())
    _require_uuid(identifier, "compose request ID")
    timestamp = created_at or utc_now()
    _parse_timestamp(timestamp, "compose request created_at")
    request: dict[str, Any] = {
        "schema": PRIVATE_SQLITE_COMPOSE_REQUEST_SCHEMA,
        "created_at": timestamp,
        "request_id": identifier,
        "action_plan": _action_binding(action_plan),
        "action_plan_producer_runtime_authority": deepcopy(
            action_plan["runtime_authority"]
        ),
        "request_runtime_authority": deepcopy(request_runtime_authority),
        "required_composer_runtime_authority": None,
        "output_intent": {
            "workspace": deepcopy(workspace),
            "create_only": True,
            "replace": False,
            "delete": False,
            "future_directory_mode": 0o700,
            "future_file_mode": 0o600,
            "same_mount_required": True,
            "randomized_staging_sibling_required": True,
        },
        "capacity_requirement": _capacity_requirement(action_plan),
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
    validate_codex_private_sqlite_compose_request(request)
    return request


def validate_codex_private_sqlite_compose_request(value: dict[str, Any]) -> None:
    request = _require_exact_keys(
        value,
        {
            "schema",
            "created_at",
            "request_id",
            "action_plan",
            "action_plan_producer_runtime_authority",
            "request_runtime_authority",
            "required_composer_runtime_authority",
            "output_intent",
            "capacity_requirement",
            "capacity_observation",
            "claims",
            "implementation",
            "request_body_sha256",
            "request_sha256",
        },
        "private SQLite compose request",
    )
    if request["schema"] != PRIVATE_SQLITE_COMPOSE_REQUEST_SCHEMA:
        raise BulkloadError("private SQLite compose-request schema differs")
    _parse_timestamp(request["created_at"], "compose request created_at")
    _require_uuid(request["request_id"], "compose request ID")
    action = _require_exact_keys(
        request["action_plan"],
        {
            "schema",
            "action_plan_sha256",
            "body_sha256",
            "opening_plan_sha256",
            "opening_inputs_sha256",
            "close_request_sha256",
            "close_body_sha256",
        },
        "compose request action-plan binding",
    )
    if action["schema"] != PRIVATE_SQLITE_ACTION_PLAN_SCHEMA:
        raise BulkloadError("compose request action-plan schema differs")
    for key in action:
        if key != "schema":
            _require_sha256(action[key], f"compose request action-plan {key}")
    _validate_runtime_authority(
        request["action_plan_producer_runtime_authority"],
        "compose request action producer",
    )
    if (
        request["action_plan_producer_runtime_authority"]
        != private_runtime.ACCEPTED_H6_PRIVATE_RUNTIME_AUTHORITY_V5
    ):
        raise BulkloadError("compose request action producer differs")
    _validate_v6_request_runtime_authority(
        request["request_runtime_authority"],
        "compose request runtime",
    )
    if (
        request["request_runtime_authority"]
        == request["action_plan_producer_runtime_authority"]
        or request["required_composer_runtime_authority"] is not None
    ):
        raise BulkloadError("compose request runtime separation differs")
    output = _require_exact_keys(
        request["output_intent"],
        {
            "workspace",
            "create_only",
            "replace",
            "delete",
            "future_directory_mode",
            "future_file_mode",
            "same_mount_required",
            "randomized_staging_sibling_required",
        },
        "compose request output intent",
    )
    _validate_workspace(output["workspace"])
    if (
        output["create_only"] is not True
        or output["replace"] is not False
        or output["delete"] is not False
        or output["future_directory_mode"] != 0o700
        or output["future_file_mode"] != 0o600
        or output["same_mount_required"] is not True
        or output["randomized_staging_sibling_required"] is not True
    ):
        raise BulkloadError("compose request output intent is unsafe")
    _validate_capacity_requirement(request["capacity_requirement"])
    capacity = _require_exact_keys(
        request["capacity_observation"],
        {
            "required",
            "maximum_age_seconds",
            "space_reserved",
            "future_write_guaranteed",
            "quota_proof",
        },
        "compose request capacity-observation policy",
    )
    if capacity != {
        "required": True,
        "maximum_age_seconds": DEFAULT_CAPACITY_OBSERVATION_TTL_SECONDS,
        "space_reserved": False,
        "future_write_guaranteed": False,
        "quota_proof": False,
    }:
        raise BulkloadError("compose request capacity-observation policy differs")
    claims = _require_exact_keys(
        request["claims"],
        {
            "request_complete",
            "request_runtime_bound",
            "composer_runtime_bound",
            "executed",
            "ready_for_internal_offline_compose",
            "ready_for_offline_compose",
            "composer_implemented",
            "compose_authorized",
            "publication_authorized",
            "install_authorized",
            "combined_authorized",
            "apply_authorized",
            "provider_runtime_acceptance",
            "provider_writer_proof",
        },
        "compose request claims",
    )
    positive_claims = {"request_complete", "request_runtime_bound"}
    if any(claims[key] is not True for key in positive_claims) or any(
        claims[key] is not False for key in claims if key not in positive_claims
    ):
        raise BulkloadError("compose request claims differ")
    if request["implementation"] != PRIVATE_SQLITE_COMPOSE_REQUEST_IMPLEMENTATION:
        raise BulkloadError("compose request implementation differs")
    _require_sha256(
        request["request_body_sha256"],
        "compose request body digest",
    )
    body = {
        key: item
        for key, item in request.items()
        if key not in {"request_body_sha256", "request_sha256"}
    }
    if sha256_bytes(canonical_bytes(body)) != request["request_body_sha256"]:
        raise BulkloadError("compose request body digest mismatch")
    _require_sha256(request["request_sha256"], "compose request digest")
    if object_digest(request, "request_sha256") != request["request_sha256"]:
        raise BulkloadError("compose request digest mismatch")
    if len(canonical_bytes(request)) > MAX_PRIVATE_SQLITE_REQUEST_BYTES:
        raise BulkloadError("compose request exceeds its byte budget")


def validate_codex_private_sqlite_compose_request_against_action(
    request: dict[str, Any],
    action_plan: dict[str, Any],
    workspace: dict[str, Any],
    expected_request_runtime_authority: dict[str, Any],
) -> None:
    """Recompute all stable v6 request semantics except UUID/timestamp."""
    validate_codex_private_sqlite_compose_request(request)
    _validate_exact_action_producer(action_plan)
    _validate_v6_request_runtime_authority(
        expected_request_runtime_authority,
        "expected compose-request runtime authority",
    )
    if request["request_runtime_authority"] != expected_request_runtime_authority:
        raise BulkloadError("compose request runtime authority differs")
    expected = compile_codex_private_sqlite_compose_request(
        action_plan,
        workspace,
        accept_action_plan=request["action_plan"]["action_plan_sha256"],
        request_runtime_authority=request["request_runtime_authority"],
        request_id=request["request_id"],
        created_at=request["created_at"],
    )
    if request != expected:
        raise BulkloadError("compose request differs from its exact inputs")


def compile_codex_private_sqlite_capacity_observation(
    request: dict[str, Any],
    workspace: dict[str, Any],
    capacity: dict[str, int],
    *,
    accept_request: str,
    observation_runtime_authority: dict[str, Any],
    host_authority_id: str,
    observation_id: str | None = None,
    created_at: str | None = None,
    ttl_seconds: int = DEFAULT_CAPACITY_OBSERVATION_TTL_SECONDS,
) -> dict[str, Any]:
    """Compile one volatile capacity observation; reserve no resources."""
    validate_codex_private_sqlite_compose_request(request)
    if accept_request != request["request_sha256"]:
        raise BulkloadError("accepted compose-request digest differs")
    _validate_workspace(workspace)
    if workspace != request["output_intent"]["workspace"]:
        raise BulkloadError("capacity workspace differs from the compose request")
    _validate_v6_request_runtime_authority(
        observation_runtime_authority,
        "capacity-observation runtime authority",
    )
    if observation_runtime_authority != request["request_runtime_authority"]:
        raise BulkloadError("capacity-observation runtime authority differs")
    _require_uuid(host_authority_id, "capacity host authority ID")
    identifier = observation_id or str(uuid.uuid4())
    _require_uuid(identifier, "capacity observation ID")
    if (
        type(ttl_seconds) is not int
        or ttl_seconds < 1
        or ttl_seconds > MAX_CAPACITY_OBSERVATION_TTL_SECONDS
        or ttl_seconds != request["capacity_observation"]["maximum_age_seconds"]
    ):
        raise BulkloadError("capacity observation TTL differs")
    timestamp = created_at or utc_now()
    created = _parse_timestamp(timestamp, "capacity observation created_at")
    expires = _format_timestamp(created + timedelta(seconds=ttl_seconds))
    filesystem = _require_exact_keys(
        capacity,
        {
            "fragment_size",
            "available_blocks",
            "available_bytes",
            "available_inodes",
        },
        "capacity observation filesystem",
    )
    for key, item in filesystem.items():
        if type(item) is not int or item < 0 or item > MAX_CHECKED_BYTES:
            raise BulkloadError(f"capacity observation {key} is invalid")
    if (
        filesystem["fragment_size"] < 1
        or _checked_multiply(
            filesystem["fragment_size"],
            filesystem["available_blocks"],
            label="capacity observation available bytes",
        )
        != filesystem["available_bytes"]
    ):
        raise BulkloadError("capacity observation byte calculation differs")
    requirement = request["capacity_requirement"]
    sufficient = bool(
        filesystem["available_bytes"] >= requirement["required_bytes"]
        and filesystem["available_inodes"] >= requirement["required_inodes"]
    )
    observation: dict[str, Any] = {
        "schema": PRIVATE_SQLITE_CAPACITY_OBSERVATION_SCHEMA,
        "created_at": timestamp,
        "expires_at": expires,
        "observation_id": identifier,
        "host_authority_id": host_authority_id,
        "request": {
            "schema": request["schema"],
            "request_sha256": request["request_sha256"],
            "body_sha256": sha256_bytes(canonical_bytes(request)),
        },
        "observation_runtime_authority": deepcopy(observation_runtime_authority),
        "workspace": deepcopy(workspace),
        "requirement": deepcopy(requirement),
        "filesystem": deepcopy(filesystem),
        "claims": {
            "capacity_sufficient_observed": sufficient,
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
    validate_codex_private_sqlite_capacity_observation(observation)
    return observation


def validate_codex_private_sqlite_capacity_observation(
    value: dict[str, Any],
) -> None:
    observation = _require_exact_keys(
        value,
        {
            "schema",
            "created_at",
            "expires_at",
            "observation_id",
            "host_authority_id",
            "request",
            "observation_runtime_authority",
            "workspace",
            "requirement",
            "filesystem",
            "claims",
            "implementation",
            "observation_body_sha256",
            "observation_sha256",
        },
        "private SQLite capacity observation",
    )
    if observation["schema"] != PRIVATE_SQLITE_CAPACITY_OBSERVATION_SCHEMA:
        raise BulkloadError("capacity-observation schema differs")
    created = _parse_timestamp(
        observation["created_at"],
        "capacity observation created_at",
    )
    expires = _parse_timestamp(
        observation["expires_at"],
        "capacity observation expires_at",
    )
    if expires - created != timedelta(seconds=DEFAULT_CAPACITY_OBSERVATION_TTL_SECONDS):
        raise BulkloadError("capacity observation TTL differs")
    _require_uuid(observation["observation_id"], "capacity observation ID")
    _require_uuid(observation["host_authority_id"], "capacity host authority ID")
    request = _require_exact_keys(
        observation["request"],
        {"schema", "request_sha256", "body_sha256"},
        "capacity observation request binding",
    )
    if request["schema"] != PRIVATE_SQLITE_COMPOSE_REQUEST_SCHEMA:
        raise BulkloadError("capacity observation request schema differs")
    _require_sha256(request["request_sha256"], "capacity request digest")
    _require_sha256(request["body_sha256"], "capacity request body digest")
    _validate_v6_request_runtime_authority(
        observation["observation_runtime_authority"],
        "capacity observation runtime",
    )
    _validate_workspace(observation["workspace"])
    requirement = _validate_capacity_requirement(observation["requirement"])
    filesystem = _require_exact_keys(
        observation["filesystem"],
        {
            "fragment_size",
            "available_blocks",
            "available_bytes",
            "available_inodes",
        },
        "capacity observation filesystem",
    )
    for key, item in filesystem.items():
        if type(item) is not int or item < 0 or item > MAX_CHECKED_BYTES:
            raise BulkloadError(f"capacity observation {key} is invalid")
    if (
        filesystem["fragment_size"] < 1
        or _checked_multiply(
            filesystem["fragment_size"],
            filesystem["available_blocks"],
            label="capacity observation available bytes",
        )
        != filesystem["available_bytes"]
    ):
        raise BulkloadError("capacity observation byte calculation differs")
    sufficient = bool(
        filesystem["available_bytes"] >= requirement["required_bytes"]
        and filesystem["available_inodes"] >= requirement["required_inodes"]
    )
    claims = _require_exact_keys(
        observation["claims"],
        {
            "capacity_sufficient_observed",
            "space_reserved",
            "future_write_guaranteed",
            "quota_proof",
            "executed",
            "ready_for_internal_offline_compose",
            "compose_authorized",
        },
        "capacity observation claims",
    )
    if claims["capacity_sufficient_observed"] is not sufficient or any(
        claims[key] is not False
        for key in claims
        if key != "capacity_sufficient_observed"
    ):
        raise BulkloadError("capacity observation claims differ")
    if (
        observation["implementation"]
        != PRIVATE_SQLITE_CAPACITY_OBSERVATION_IMPLEMENTATION
    ):
        raise BulkloadError("capacity observation implementation differs")
    _require_sha256(
        observation["observation_body_sha256"],
        "capacity observation body digest",
    )
    body = {
        key: item
        for key, item in observation.items()
        if key not in {"observation_body_sha256", "observation_sha256"}
    }
    if sha256_bytes(canonical_bytes(body)) != observation["observation_body_sha256"]:
        raise BulkloadError("capacity observation body digest mismatch")
    _require_sha256(
        observation["observation_sha256"],
        "capacity observation digest",
    )
    if (
        object_digest(observation, "observation_sha256")
        != observation["observation_sha256"]
    ):
        raise BulkloadError("capacity observation digest mismatch")
    if len(canonical_bytes(observation)) > MAX_PRIVATE_SQLITE_REQUEST_BYTES:
        raise BulkloadError("capacity observation exceeds its byte budget")


def validate_codex_private_sqlite_capacity_observation_against_request(
    observation: dict[str, Any],
    request: dict[str, Any],
    workspace: dict[str, Any],
    expected_observation_runtime_authority: dict[str, Any],
) -> None:
    """Bind one observation to the exact request and current workspace."""
    validate_codex_private_sqlite_capacity_observation(observation)
    validate_codex_private_sqlite_compose_request(request)
    _validate_v6_request_runtime_authority(
        expected_observation_runtime_authority,
        "expected capacity-observation runtime authority",
    )
    if (
        observation["request"]
        != {
            "schema": request["schema"],
            "request_sha256": request["request_sha256"],
            "body_sha256": sha256_bytes(canonical_bytes(request)),
        }
        or observation["workspace"] != workspace
        or observation["workspace"] != request["output_intent"]["workspace"]
        or observation["requirement"] != request["capacity_requirement"]
        or observation["observation_runtime_authority"]
        != request["request_runtime_authority"]
        or observation["observation_runtime_authority"]
        != expected_observation_runtime_authority
    ):
        raise BulkloadError("capacity observation differs from its request")
