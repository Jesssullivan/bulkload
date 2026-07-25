"""Read-only Codex rollout catalogs and absent-only union plans."""

from __future__ import annotations

from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import secrets
import socket
import stat
from typing import Any

from .model import (
    BulkloadError,
    canonical_bytes,
    normalize_relative,
    object_digest,
    require_digest,
    sha256_bytes,
    utc_now,
)

CODEX_SESSION_SNAPSHOT_SCHEMA = "dev.tinyland.bulkload.codex-sessions.v1"
CODEX_SESSION_PLAN_SCHEMA = "dev.tinyland.bulkload.codex-session-union-plan.v1"
DEFAULT_MAX_SESSION_FILES = 10_000
DEFAULT_MAX_SESSION_BYTES = 64 * 1024 * 1024 * 1024
DEFAULT_MAX_SESSION_RECORD_BYTES = 256 * 1024 * 1024

_SESSION_ID = re.compile(
    r"(?P<id>[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-"
    r"[0-9a-fA-F]{4}-[0-9a-fA-F]{12})\.jsonl$"
)


def _stable_user_regular(info: os.stat_result, path: Path) -> bool:
    if not stat.S_ISREG(info.st_mode):
        raise BulkloadError(f"Codex rollout is not a regular file: {path}")
    if info.st_uid != os.getuid():
        raise BulkloadError(f"Codex rollout is not owned by the current user: {path}")
    mode = stat.S_IMODE(info.st_mode)
    if mode & 0o022 or not mode & stat.S_IRUSR:
        raise BulkloadError(
            f"Codex rollout is not owner-readable and non-writable-by-others: {path}"
        )
    return not mode & 0o077


def _stable_stat(
    info: os.stat_result,
) -> tuple[int, int, int, int, int, int, int]:
    return (
        info.st_dev,
        info.st_ino,
        info.st_uid,
        stat.S_IMODE(info.st_mode),
        info.st_size,
        info.st_mtime_ns,
        info.st_ctime_ns,
    )


def _capture_rollout(
    directory_descriptor: int,
    name: str,
    path: Path,
    relative_path: str,
    session_id: str,
    max_record_bytes: int,
    expected_info: os.stat_result,
) -> dict[str, Any]:
    _stable_user_regular(expected_info, path)
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0)
    descriptor = os.open(name, flags, dir_fd=directory_descriptor)
    digest = hashlib.sha256()
    line_count = 0
    bytes_read = 0
    first_record: dict[str, Any] | None = None
    try:
        before_fd = os.fstat(descriptor)
        _stable_user_regular(before_fd, path)
        if _stable_stat(before_fd) != _stable_stat(expected_info):
            raise BulkloadError(f"Codex rollout changed before capture: {path}")
        with os.fdopen(descriptor, "rb", closefd=False) as source:
            while raw_line := source.readline(max_record_bytes + 1):
                if len(raw_line) > max_record_bytes:
                    raise BulkloadError(
                        f"Codex rollout record byte budget exceeded "
                        f"({max_record_bytes}): {path}"
                    )
                line_count += 1
                bytes_read += len(raw_line)
                digest.update(raw_line)
                if not raw_line.endswith(b"\n"):
                    raise BulkloadError(
                        f"Codex rollout has an unterminated record: {path}"
                    )
                if raw_line == b"\n":
                    raise BulkloadError(f"Codex rollout has an empty record: {path}")
                try:
                    record = json.loads(raw_line)
                except (UnicodeDecodeError, json.JSONDecodeError) as error:
                    raise BulkloadError(
                        f"Codex rollout is not valid UTF-8 JSONL: {path}"
                    ) from error
                if not isinstance(record, dict):
                    raise BulkloadError(
                        f"Codex rollout record is not a JSON object: {path}"
                    )
                if first_record is None:
                    first_record = record
            after_fd = os.fstat(descriptor)
    finally:
        os.close(descriptor)
    after_entry = os.stat(
        name,
        dir_fd=directory_descriptor,
        follow_symlinks=False,
    )
    if (
        _stable_stat(before_fd) != _stable_stat(after_fd)
        or _stable_stat(before_fd) != _stable_stat(after_entry)
        or bytes_read != before_fd.st_size
    ):
        raise BulkloadError(f"Codex rollout changed during capture: {path}")
    if first_record is None:
        raise BulkloadError(f"Codex rollout is empty: {path}")
    payload = first_record.get("payload")
    if (
        first_record.get("type") != "session_meta"
        or not isinstance(payload, dict)
        or str(payload.get("id", "")).lower() != session_id
    ):
        raise BulkloadError(
            f"Codex rollout filename and session_meta identity differ: {path}"
        )
    return {
        "session_id": session_id,
        "relative_path": relative_path,
        "sha256": digest.hexdigest(),
        "size": bytes_read,
        "mode": f"{stat.S_IMODE(before_fd.st_mode):04o}",
        "records": line_count,
    }


def _validate_stable_directory(path: Path, info: os.stat_result) -> bool:
    if not stat.S_ISDIR(info.st_mode):
        raise BulkloadError(f"Codex session path is not a directory: {path}")
    if info.st_uid != os.getuid():
        raise BulkloadError(
            f"Codex session directory is not owned by the current user: {path}"
        )
    mode = stat.S_IMODE(info.st_mode)
    if mode & 0o022 or mode & 0o500 != 0o500:
        raise BulkloadError(
            f"Codex session directory is not owner-accessible and "
            f"non-writable-by-others: {path}"
        )
    return not mode & 0o077


def _open_directory(
    name: str | Path,
    *,
    parent_descriptor: int | None,
    path: Path,
) -> int:
    flags = (
        os.O_RDONLY
        | getattr(os, "O_DIRECTORY", 0)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_CLOEXEC", 0)
    )
    try:
        if parent_descriptor is None:
            return os.open(name, flags)
        return os.open(name, flags, dir_fd=parent_descriptor)
    except OSError as error:
        raise BulkloadError(
            f"cannot open Codex session directory without following links: {path}"
        ) from error


@dataclass
class _DirectoryFrame:
    descriptor: int
    relative_path: str
    expected: tuple[int, ...]
    before: tuple[int, ...] | None = None
    entries: list[os.DirEntry[str]] | None = None
    index: int = 0


def capture_codex_sessions(
    root: Path,
    *,
    max_files: int = DEFAULT_MAX_SESSION_FILES,
    max_bytes: int = DEFAULT_MAX_SESSION_BYTES,
    max_record_bytes: int = DEFAULT_MAX_SESSION_RECORD_BYTES,
) -> dict[str, Any]:
    """Capture one bounded, content-addressed Codex rollout inventory."""
    if max_files <= 0 or max_bytes <= 0 or max_record_bytes <= 0:
        raise BulkloadError("Codex session capture budgets must be positive")
    root = root.expanduser().resolve()
    sessions: list[dict[str, Any]] = []
    errors: list[str] = []
    total_bytes = 0
    non_private_directories = 0
    stack: list[_DirectoryFrame] = []
    root_descriptor: int | None = None
    try:
        root_path_info = root.lstat()
        if not _validate_stable_directory(root, root_path_info):
            non_private_directories += 1
        root_descriptor = _open_directory(
            root,
            parent_descriptor=None,
            path=root,
        )
        root_descriptor_info = os.fstat(root_descriptor)
        _validate_stable_directory(root, root_descriptor_info)
        if _stable_stat(root_descriptor_info) != _stable_stat(root_path_info):
            raise BulkloadError(f"Codex session root changed before capture: {root}")
        stack.append(
            _DirectoryFrame(
                descriptor=root_descriptor,
                relative_path="",
                expected=_stable_stat(root_descriptor_info),
            )
        )
        root_descriptor = None
    except (OSError, BulkloadError) as error:
        errors.append(str(error))
    finally:
        if root_descriptor is not None:
            os.close(root_descriptor)

    try:
        while stack:
            frame = stack[-1]
            directory = root / frame.relative_path
            if frame.entries is None:
                try:
                    before_directory = os.fstat(frame.descriptor)
                    _validate_stable_directory(directory, before_directory)
                    frame.before = _stable_stat(before_directory)
                    if frame.before != frame.expected:
                        raise BulkloadError(
                            f"Codex session directory changed before capture: "
                            f"{directory}"
                        )
                    with os.scandir(frame.descriptor) as iterator:
                        frame.entries = sorted(
                            iterator,
                            key=lambda entry: entry.name,
                        )
                except (OSError, BulkloadError) as error:
                    errors.append(
                        f"cannot enumerate Codex session directory {directory}: {error}"
                    )
                    os.close(frame.descriptor)
                    stack.pop()
                    continue

            if frame.index >= len(frame.entries):
                try:
                    after_directory = os.fstat(frame.descriptor)
                    if _stable_stat(after_directory) != frame.before:
                        errors.append(
                            f"Codex session directory changed during capture: "
                            f"{directory}"
                        )
                except OSError as error:
                    errors.append(
                        f"cannot revalidate Codex session directory {directory}: "
                        f"{error}"
                    )
                os.close(frame.descriptor)
                stack.pop()
                continue

            entry = frame.entries[frame.index]
            frame.index += 1
            relative_path = (
                f"{frame.relative_path}/{entry.name}"
                if frame.relative_path
                else entry.name
            )
            try:
                relative_path = normalize_relative(relative_path)
            except BulkloadError as error:
                errors.append(str(error))
                continue
            path = root / relative_path
            try:
                info = entry.stat(follow_symlinks=False)
                if stat.S_ISDIR(info.st_mode):
                    if not _validate_stable_directory(path, info):
                        non_private_directories += 1
                    child_descriptor = _open_directory(
                        entry.name,
                        parent_descriptor=frame.descriptor,
                        path=path,
                    )
                    try:
                        child_info = os.fstat(child_descriptor)
                        _validate_stable_directory(path, child_info)
                        if _stable_stat(child_info) != _stable_stat(info):
                            raise BulkloadError(
                                f"Codex session directory changed before "
                                f"capture: {path}"
                            )
                    except BaseException:
                        os.close(child_descriptor)
                        raise
                    stack.append(
                        _DirectoryFrame(
                            descriptor=child_descriptor,
                            relative_path=relative_path,
                            expected=_stable_stat(child_info),
                        )
                    )
                    continue
                if not stat.S_ISREG(info.st_mode):
                    raise BulkloadError(
                        f"unsupported Codex session filesystem entry: {path}"
                    )
                match = _SESSION_ID.search(entry.name)
                if not entry.name.startswith("rollout-") or match is None:
                    raise BulkloadError(
                        f"unexpected regular file in Codex session store: {path}"
                    )
                if len(sessions) >= max_files:
                    raise BulkloadError(
                        f"Codex session file budget exceeded ({max_files})"
                    )
                if info.st_size > max_bytes - total_bytes:
                    raise BulkloadError(
                        f"Codex session byte budget exceeded ({max_bytes})"
                    )
                record = _capture_rollout(
                    frame.descriptor,
                    entry.name,
                    path,
                    relative_path,
                    match.group("id").lower(),
                    max_record_bytes,
                    info,
                )
                if record["size"] > max_bytes - total_bytes:
                    raise BulkloadError(
                        f"Codex session byte budget exceeded ({max_bytes})"
                    )
                sessions.append(record)
                total_bytes += record["size"]
            except (OSError, BulkloadError) as error:
                errors.append(str(error))
    finally:
        while stack:
            frame = stack.pop()
            try:
                os.close(frame.descriptor)
            except OSError:
                pass

    sessions.sort(key=lambda item: (item["session_id"], item["relative_path"]))
    ids: set[str] = set()
    paths: set[str] = set()
    for item in sessions:
        if item["session_id"] in ids:
            errors.append(f"duplicate Codex session UUID: {item['session_id']}")
        if item["relative_path"] in paths:
            errors.append(
                f"duplicate Codex session relative path: {item['relative_path']}"
            )
        ids.add(item["session_id"])
        paths.add(item["relative_path"])

    snapshot: dict[str, Any] = {
        "schema": CODEX_SESSION_SNAPSHOT_SCHEMA,
        "captured_at": utc_now(),
        "capture_id": secrets.token_hex(16),
        "host": socket.gethostname(),
        "root": str(root),
        "complete": not errors,
        "errors": errors,
        "sessions": sessions,
        "total_bytes": total_bytes,
        "non_private_file_count": sum(
            int(int(item["mode"], 8) & 0o077 != 0) for item in sessions
        ),
        "non_private_directory_count": non_private_directories,
    }
    catalog = {
        "sessions": sessions,
        "non_private_file_count": snapshot["non_private_file_count"],
        "non_private_directory_count": snapshot["non_private_directory_count"],
    }
    snapshot["catalog_sha256"] = sha256_bytes(canonical_bytes(catalog))
    snapshot["snapshot_sha256"] = object_digest(snapshot, "snapshot_sha256")
    return snapshot


def validate_codex_session_snapshot(snapshot: dict[str, Any]) -> None:
    expected_snapshot_keys = {
        "schema",
        "captured_at",
        "capture_id",
        "host",
        "root",
        "complete",
        "errors",
        "sessions",
        "total_bytes",
        "non_private_file_count",
        "non_private_directory_count",
        "catalog_sha256",
        "snapshot_sha256",
    }
    if set(snapshot) != expected_snapshot_keys:
        raise BulkloadError("Codex session snapshot has unexpected fields")
    if snapshot.get("schema") != CODEX_SESSION_SNAPSHOT_SCHEMA:
        raise BulkloadError("unsupported Codex session snapshot schema")
    require_digest(snapshot, "snapshot_sha256")
    errors = snapshot.get("errors")
    if (
        not isinstance(snapshot.get("captured_at"), str)
        or not isinstance(snapshot.get("host"), str)
        or not snapshot["host"]
        or not isinstance(snapshot.get("root"), str)
        or not Path(snapshot["root"]).is_absolute()
        or not isinstance(snapshot.get("complete"), bool)
        or not isinstance(errors, list)
        or any(not isinstance(error, str) or not error for error in errors)
        or snapshot["complete"] != (not errors)
    ):
        raise BulkloadError("Codex session snapshot envelope is invalid")
    sessions = snapshot.get("sessions")
    if not isinstance(sessions, list):
        raise BulkloadError("Codex session snapshot sessions must be a list")
    catalog = {
        "sessions": sessions,
        "non_private_file_count": snapshot.get("non_private_file_count"),
        "non_private_directory_count": snapshot.get("non_private_directory_count"),
    }
    if snapshot.get("catalog_sha256") != sha256_bytes(canonical_bytes(catalog)):
        raise BulkloadError("Codex session catalog digest mismatch")
    if not isinstance(snapshot.get("capture_id"), str) or not re.fullmatch(
        r"[0-9a-f]{32}", snapshot["capture_id"]
    ):
        raise BulkloadError("Codex session capture_id is invalid")
    seen_ids: set[str] = set()
    seen_paths: set[str] = set()
    computed_bytes = 0
    for item in sessions:
        if not isinstance(item, dict):
            raise BulkloadError("Codex session record must be an object")
        expected_keys = {
            "session_id",
            "relative_path",
            "sha256",
            "size",
            "mode",
            "records",
        }
        if set(item) != expected_keys:
            raise BulkloadError("Codex session record has unexpected fields")
        session_id = item["session_id"]
        relative_path = item["relative_path"]
        try:
            normalized_relative = normalize_relative(relative_path)
        except BulkloadError as error:
            raise BulkloadError("Codex session record identity is invalid") from error
        relative_name = PurePosixPath(normalized_relative).name
        relative_match = _SESSION_ID.search(relative_name)
        if (
            not isinstance(session_id, str)
            or not _SESSION_ID.fullmatch(f"{session_id}.jsonl")
            or session_id != session_id.lower()
            or not isinstance(relative_path, str)
            or not relative_name.startswith("rollout-")
            or relative_match is None
            or relative_match.group("id").lower() != session_id
        ):
            raise BulkloadError("Codex session record identity is invalid")
        if session_id in seen_ids or relative_path in seen_paths:
            raise BulkloadError("Codex session snapshot contains duplicate identity")
        if not isinstance(item["sha256"], str) or not re.fullmatch(
            r"[0-9a-f]{64}", item["sha256"]
        ):
            raise BulkloadError("Codex session record digest is invalid")
        if (
            not isinstance(item["size"], int)
            or item["size"] <= 0
            or not isinstance(item["records"], int)
            or item["records"] <= 0
            or not isinstance(item["mode"], str)
            or not re.fullmatch(r"0[0-7]{3}", item["mode"])
            or int(item["mode"], 8) & 0o022
            or not int(item["mode"], 8) & 0o400
        ):
            raise BulkloadError("Codex session record bounds or mode are invalid")
        seen_ids.add(session_id)
        seen_paths.add(relative_path)
        computed_bytes += item["size"]
    if snapshot.get("total_bytes") != computed_bytes:
        raise BulkloadError("Codex session total_bytes mismatch")
    if snapshot.get("non_private_file_count") != sum(
        int(int(item["mode"], 8) & 0o077 != 0) for item in sessions
    ):
        raise BulkloadError("Codex session non-private file count mismatch")
    if (
        not isinstance(snapshot.get("non_private_directory_count"), int)
        or snapshot["non_private_directory_count"] < 0
    ):
        raise BulkloadError("Codex session non-private directory count is invalid")


def compile_codex_session_union_plan(
    source_a: dict[str, Any],
    source_b: dict[str, Any],
    destination: dict[str, Any],
) -> dict[str, Any]:
    """Compile a read-only, absent-only UUID union report."""
    for snapshot in (source_a, source_b, destination):
        validate_codex_session_snapshot(snapshot)
        if not snapshot.get("complete"):
            raise BulkloadError("Codex session planning requires complete captures")
    if source_a["capture_id"] == source_b["capture_id"]:
        raise BulkloadError("Codex session source passes must be distinct captures")
    if (
        source_a["host"] != source_b["host"]
        or source_a["root"] != source_b["root"]
        or source_a["catalog_sha256"] != source_b["catalog_sha256"]
    ):
        raise BulkloadError("Codex session source pass A and pass B differ")
    if (source_a["host"], source_a["root"]) == (
        destination["host"],
        destination["root"],
    ):
        raise BulkloadError("Codex session source and destination must differ")

    source_by_id = {item["session_id"]: item for item in source_a["sessions"]}
    destination_by_id = {item["session_id"]: item for item in destination["sessions"]}
    copy_if_absent: list[dict[str, Any]] = []
    exact_common: list[dict[str, Any]] = []
    preserve_destination: list[dict[str, Any]] = []
    blockers: list[dict[str, Any]] = []

    for session_id in sorted(source_by_id):
        source = source_by_id[session_id]
        target = destination_by_id.get(session_id)
        if target is None:
            copy_if_absent.append(
                {
                    "action": "copy-if-absent",
                    "session_id": session_id,
                    "source_relative_path": source["relative_path"],
                    "destination_relative_path": source["relative_path"],
                    "sha256": source["sha256"],
                    "size": source["size"],
                    "destination_mode": "0600",
                }
            )
        elif source["sha256"] == target["sha256"] and source["size"] == target["size"]:
            exact_common.append(
                {
                    "session_id": session_id,
                    "source_relative_path": source["relative_path"],
                    "destination_relative_path": target["relative_path"],
                    "sha256": source["sha256"],
                }
            )
        else:
            blockers.append(
                {
                    "code": "same-uuid-different-bytes",
                    "session_id": session_id,
                    "source_relative_path": source["relative_path"],
                    "destination_relative_path": target["relative_path"],
                    "source_sha256": source["sha256"],
                    "destination_sha256": target["sha256"],
                    "source_size": source["size"],
                    "destination_size": target["size"],
                }
            )

    for session_id in sorted(set(destination_by_id) - set(source_by_id)):
        target = destination_by_id[session_id]
        preserve_destination.append(
            {
                "session_id": session_id,
                "relative_path": target["relative_path"],
                "sha256": target["sha256"],
                "size": target["size"],
            }
        )

    intent = {
        "ready_for_attended_copy": not blockers,
        "copy_if_absent": copy_if_absent,
        "exact_common": exact_common,
        "preserve_destination": preserve_destination,
        "custody_findings": {
            "source_non_private_files": source_a["non_private_file_count"],
            "source_non_private_directories": source_a["non_private_directory_count"],
            "destination_non_private_files": destination["non_private_file_count"],
            "destination_non_private_directories": destination[
                "non_private_directory_count"
            ],
        },
        "blockers": blockers,
    }
    plan: dict[str, Any] = {
        "schema": CODEX_SESSION_PLAN_SCHEMA,
        "created_at": utc_now(),
        "source": {
            "host": source_a["host"],
            "root": source_a["root"],
            "catalog_sha256": source_a["catalog_sha256"],
            "capture_ids": [source_a["capture_id"], source_b["capture_id"]],
        },
        "destination": {
            "host": destination["host"],
            "root": destination["root"],
            "catalog_sha256": destination["catalog_sha256"],
            "capture_id": destination["capture_id"],
        },
        "intent": intent,
    }
    plan["plan_sha256"] = object_digest(plan, "plan_sha256")
    return plan
