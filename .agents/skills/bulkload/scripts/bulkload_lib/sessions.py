"""Read-only Codex rollout catalogs and absent-only union plans."""

from __future__ import annotations

from dataclasses import dataclass
import hashlib
import json
import math
import os
from pathlib import Path, PurePosixPath
import re
import secrets
import socket
import stat
from typing import Any
import unicodedata
import uuid

from .model import (
    BulkloadError,
    canonical_bytes,
    normalize_relative,
    object_digest,
    require_digest,
    sha256_bytes,
    utc_now,
)

CODEX_SESSION_SNAPSHOT_SCHEMA = "dev.tinyland.bulkload.codex-sessions.v2"
CODEX_SESSION_PLAN_SCHEMA = "dev.tinyland.bulkload.codex-session-union-plan.v2"
DEFAULT_MAX_SESSION_FILES = 10_000
DEFAULT_MAX_SESSION_ENTRIES = 20_000
DEFAULT_MAX_SESSION_DIRECTORIES = 10_000
DEFAULT_MAX_SESSION_BYTES = 64 * 1024 * 1024 * 1024
DEFAULT_MAX_SESSION_FILE_BYTES = 1024 * 1024 * 1024
DEFAULT_MAX_SESSION_RECORD_BYTES = 8 * 1024 * 1024
DEFAULT_MAX_SESSION_RECORDS_PER_FILE = 1_000_000
DEFAULT_MAX_SESSION_PATH_BYTES = 4096
DEFAULT_MAX_SESSION_PATH_COMPONENTS = 32
DEFAULT_MAX_SESSION_CATALOG_BYTES = 64 * 1024 * 1024
DEFAULT_MAX_SESSION_ERRORS = 128
DEFAULT_MAX_SESSION_OUTPUT_BYTES = 72 * 1024 * 1024
MAX_CODEX_SESSION_PLAN_BYTES = 256 * 1024 * 1024
MAX_CODEX_ROOT_LINEAGE = 256

_BUDGET_LIMITS = {
    "max_files": 100_000,
    "max_entries": 1_000_000,
    "max_directories": 100_000,
    "max_total_bytes": 1024 * 1024 * 1024 * 1024,
    "max_file_bytes": 16 * 1024 * 1024 * 1024,
    "max_record_bytes": 64 * 1024 * 1024,
    "max_records_per_file": 10_000_000,
    "max_path_bytes": 4096,
    "max_path_components": 256,
    "max_catalog_bytes": 256 * 1024 * 1024,
    "max_errors": 1024,
    "max_output_bytes": 256 * 1024 * 1024,
}
MAX_CODEX_SESSION_SNAPSHOT_BYTES = _BUDGET_LIMITS["max_output_bytes"]

_SESSION_ID = re.compile(
    r"(?P<id>[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-"
    r"[0-9a-f]{4}-[0-9a-f]{12})\.jsonl$"
)


class _CaptureHalt(Exception):
    """Stop a bounded scan after a terminal budget or error condition."""


class _CaptureBudgetError(BulkloadError):
    """A configured capture budget was exhausted."""


def _capture_budgets(
    *,
    max_files: int,
    max_entries: int,
    max_directories: int,
    max_bytes: int,
    max_file_bytes: int,
    max_record_bytes: int,
    max_records_per_file: int,
    max_path_bytes: int,
    max_path_components: int,
    max_catalog_bytes: int,
    max_errors: int,
    max_output_bytes: int,
) -> dict[str, int]:
    budgets = {
        "max_files": max_files,
        "max_entries": max_entries,
        "max_directories": max_directories,
        "max_total_bytes": max_bytes,
        "max_file_bytes": max_file_bytes,
        "max_record_bytes": max_record_bytes,
        "max_records_per_file": max_records_per_file,
        "max_path_bytes": max_path_bytes,
        "max_path_components": max_path_components,
        "max_catalog_bytes": max_catalog_bytes,
        "max_errors": max_errors,
        "max_output_bytes": max_output_bytes,
    }
    for name, maximum in _BUDGET_LIMITS.items():
        value = budgets[name]
        if type(value) is not int or value < 1 or value > maximum:
            raise BulkloadError(
                f"Codex session {name} must be a positive integer no greater "
                f"than {maximum}"
            )
    if budgets["max_catalog_bytes"] > budgets["max_output_bytes"]:
        raise BulkloadError("Codex session capture budgets are inconsistent")
    return budgets


def _record_error(
    errors: list[str],
    error: BaseException | str,
    max_errors: int,
) -> None:
    message = str(error)
    if len(errors) >= max_errors:
        raise _CaptureHalt
    errors.append(message)
    if len(errors) >= max_errors:
        raise _CaptureHalt


def _strict_json_object(payload: bytes, path: Path) -> dict[str, Any]:
    def reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        value: dict[str, Any] = {}
        for key, item in pairs:
            if key in value:
                raise ValueError("duplicate JSON key")
            value[key] = item
        return value

    def reject_nonfinite(_value: str) -> None:
        raise ValueError("non-finite JSON value")

    def parse_finite_float(value: str) -> float:
        parsed = float(value)
        if not math.isfinite(parsed):
            raise ValueError("non-finite JSON value")
        return parsed

    try:
        text = payload.decode("utf-8")
        value = json.loads(
            text,
            object_pairs_hook=reject_duplicate_keys,
            parse_constant=reject_nonfinite,
            parse_float=parse_finite_float,
        )
    except (
        UnicodeDecodeError,
        json.JSONDecodeError,
        OverflowError,
        RecursionError,
        ValueError,
    ) as error:
        raise BulkloadError(
            f"Codex rollout is not strict UTF-8 JSONL: {path}"
        ) from error
    if not isinstance(value, dict):
        raise BulkloadError(f"Codex rollout record is not a JSON object: {path}")
    return value


def _canonical_session_id(value: Any, path: Path) -> str:
    if not isinstance(value, str):
        raise BulkloadError(f"Codex rollout session UUID is invalid: {path}")
    try:
        parsed = str(uuid.UUID(value))
    except ValueError as error:
        raise BulkloadError(f"Codex rollout session UUID is invalid: {path}") from error
    if parsed != value:
        raise BulkloadError(f"Codex rollout session UUID is not canonical: {path}")
    return parsed


def _canonical_host_authority_id(value: Any) -> str:
    if not isinstance(value, str):
        raise BulkloadError("Codex host authority ID is invalid")
    try:
        parsed = str(uuid.UUID(value))
    except ValueError as error:
        raise BulkloadError("Codex host authority ID is invalid") from error
    if parsed != value:
        raise BulkloadError("Codex host authority ID is not canonical")
    return parsed


def _portable_path_key(path: str) -> str:
    return unicodedata.normalize("NFC", path).casefold()


def _utf8_size(value: str, label: str) -> int:
    try:
        return len(value.encode("utf-8"))
    except UnicodeEncodeError as error:
        raise BulkloadError(f"{label} must be valid UTF-8") from error


def _validate_relative_path(relative_path: str, budgets: dict[str, int]) -> str:
    normalized = normalize_relative(relative_path)
    parts = PurePosixPath(normalized).parts
    if len(parts) > budgets["max_path_components"]:
        raise _CaptureBudgetError(
            "Codex session relative-path component budget exceeded "
            f"({budgets['max_path_components']})"
        )
    if (
        _utf8_size(normalized, "Codex session relative path")
        > budgets["max_path_bytes"]
    ):
        raise _CaptureBudgetError(
            "Codex session relative-path byte budget exceeded "
            f"({budgets['max_path_bytes']})"
        )
    return normalized


def _stable_user_regular(
    info: os.stat_result,
    path: Path,
    *,
    role: str,
) -> bool:
    if not stat.S_ISREG(info.st_mode):
        raise BulkloadError(f"Codex rollout is not a regular file: {path}")
    if info.st_uid != os.getuid():
        raise BulkloadError(f"Codex rollout is not owned by the current user: {path}")
    if info.st_nlink != 1:
        raise BulkloadError(f"Codex rollout hardlinks are unsupported: {path}")
    mode = stat.S_IMODE(info.st_mode)
    if role == "destination":
        if mode != 0o600:
            raise BulkloadError(
                f"destination Codex rollout mode must be exactly 0600: {path}"
            )
    elif mode & 0o022 or not mode & stat.S_IRUSR:
        raise BulkloadError(
            f"Codex rollout is not owner-readable and non-writable-by-others: {path}"
        )
    return not mode & 0o077


def _stable_stat(
    info: os.stat_result,
) -> tuple[int, int, int, int, int, int, int, int]:
    return (
        info.st_dev,
        info.st_ino,
        info.st_uid,
        stat.S_IMODE(info.st_mode),
        info.st_nlink,
        info.st_size,
        info.st_mtime_ns,
        info.st_ctime_ns,
    )


def _identity_record(info: os.stat_result | tuple[int, ...]) -> dict[str, int]:
    return {
        "device": info.st_dev if isinstance(info, os.stat_result) else info[0],
        "inode": info.st_ino if isinstance(info, os.stat_result) else info[1],
    }


def _directory_lineage(descriptor: int) -> list[dict[str, int]]:
    current = os.dup(descriptor)
    lineage: list[dict[str, int]] = []
    try:
        for _ in range(MAX_CODEX_ROOT_LINEAGE):
            current_info = os.fstat(current)
            current_identity = _identity_record(current_info)
            lineage.append(current_identity)
            parent = _open_directory(
                "..",
                parent_descriptor=current,
                path=Path(".."),
            )
            parent_info = os.fstat(parent)
            if _identity_record(parent_info) == current_identity:
                os.close(parent)
                return lineage
            os.close(current)
            current = parent
    finally:
        os.close(current)
    raise BulkloadError(
        f"Codex session root lineage exceeds {MAX_CODEX_ROOT_LINEAGE} directories"
    )


def _capture_rollout(
    directory_descriptor: int,
    name: str,
    path: Path,
    relative_path: str,
    session_id: str,
    role: str,
    budgets: dict[str, int],
    expected_info: os.stat_result,
) -> dict[str, Any]:
    _stable_user_regular(expected_info, path, role=role)
    if expected_info.st_size < 1 or expected_info.st_size > budgets["max_file_bytes"]:
        raise _CaptureBudgetError(
            "Codex session file byte budget exceeded "
            f"({budgets['max_file_bytes']}): {path}"
        )
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0)
    descriptor = os.open(name, flags, dir_fd=directory_descriptor)
    digest = hashlib.sha256()
    line_count = 0
    bytes_read = 0
    observed_session_id: str | None = None
    try:
        before_fd = os.fstat(descriptor)
        _stable_user_regular(before_fd, path, role=role)
        if _stable_stat(before_fd) != _stable_stat(expected_info):
            raise BulkloadError(f"Codex rollout changed before capture: {path}")
        with os.fdopen(descriptor, "rb", closefd=False) as source:
            while bytes_read < before_fd.st_size:
                remaining = before_fd.st_size - bytes_read
                raw_line = source.readline(
                    min(budgets["max_record_bytes"] + 1, remaining)
                )
                if not raw_line:
                    raise BulkloadError(f"Codex rollout changed during capture: {path}")
                if len(raw_line) > budgets["max_record_bytes"]:
                    raise _CaptureBudgetError(
                        f"Codex rollout record byte budget exceeded "
                        f"({budgets['max_record_bytes']}): {path}"
                    )
                line_count += 1
                if line_count > budgets["max_records_per_file"]:
                    raise _CaptureBudgetError(
                        "Codex rollout record-count budget exceeded "
                        f"({budgets['max_records_per_file']}): {path}"
                    )
                bytes_read += len(raw_line)
                digest.update(raw_line)
                if not raw_line.endswith(b"\n"):
                    raise BulkloadError(
                        f"Codex rollout has an unterminated record: {path}"
                    )
                if raw_line == b"\n":
                    raise BulkloadError(f"Codex rollout has an empty record: {path}")
                record = _strict_json_object(raw_line, path)
                if line_count == 1:
                    payload = record.get("payload")
                    if record.get("type") != "session_meta" or not isinstance(
                        payload, dict
                    ):
                        raise BulkloadError(
                            f"Codex rollout first record is not session_meta: {path}"
                        )
                    observed_session_id = _canonical_session_id(
                        payload.get("id"),
                        path,
                    )
                    if observed_session_id != session_id:
                        raise BulkloadError(
                            "Codex rollout filename and session_meta identity "
                            f"differ: {path}"
                        )
                elif record.get("type") == "session_meta":
                    raise BulkloadError(
                        f"Codex rollout contains multiple session_meta records: {path}"
                    )
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
    if observed_session_id is None:
        raise BulkloadError(
            f"Codex rollout does not contain one canonical session_meta: {path}"
        )
    return {
        "session_id": session_id,
        "relative_path": relative_path,
        "sha256": digest.hexdigest(),
        "size": bytes_read,
        "mode": f"{stat.S_IMODE(before_fd.st_mode):04o}",
        "records": line_count,
    }


def _validate_stable_directory(
    path: Path,
    info: os.stat_result,
    *,
    role: str,
) -> bool:
    if not stat.S_ISDIR(info.st_mode):
        raise BulkloadError(f"Codex session path is not a directory: {path}")
    if info.st_uid != os.getuid():
        raise BulkloadError(
            f"Codex session directory is not owned by the current user: {path}"
        )
    mode = stat.S_IMODE(info.st_mode)
    if role == "destination":
        if mode != 0o700:
            raise BulkloadError(
                f"destination Codex session directory mode must be exactly 0700: {path}"
            )
    elif mode & 0o022 or mode & 0o500 != 0o500:
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


def _open_relative_directory(root_descriptor: int, relative_path: str) -> int:
    descriptor = os.dup(root_descriptor)
    try:
        if relative_path:
            for component in PurePosixPath(relative_path).parts:
                child = _open_directory(
                    component,
                    parent_descriptor=descriptor,
                    path=Path(relative_path),
                )
                os.close(descriptor)
                descriptor = child
        return descriptor
    except BaseException:
        os.close(descriptor)
        raise


@dataclass
class _DirectoryFrame:
    descriptor: int
    relative_path: str
    expected: tuple[int, ...]
    before: tuple[int, ...] | None = None
    entries: list[str] | None = None
    index: int = 0


def capture_codex_sessions(
    root: Path,
    *,
    role: str,
    acknowledge_writers_quiesced: bool,
    host_authority_id: str,
    max_files: int = DEFAULT_MAX_SESSION_FILES,
    max_entries: int = DEFAULT_MAX_SESSION_ENTRIES,
    max_directories: int = DEFAULT_MAX_SESSION_DIRECTORIES,
    max_bytes: int = DEFAULT_MAX_SESSION_BYTES,
    max_file_bytes: int = DEFAULT_MAX_SESSION_FILE_BYTES,
    max_record_bytes: int = DEFAULT_MAX_SESSION_RECORD_BYTES,
    max_records_per_file: int = DEFAULT_MAX_SESSION_RECORDS_PER_FILE,
    max_path_bytes: int = DEFAULT_MAX_SESSION_PATH_BYTES,
    max_path_components: int = DEFAULT_MAX_SESSION_PATH_COMPONENTS,
    max_catalog_bytes: int = DEFAULT_MAX_SESSION_CATALOG_BYTES,
    max_errors: int = DEFAULT_MAX_SESSION_ERRORS,
    max_output_bytes: int = DEFAULT_MAX_SESSION_OUTPUT_BYTES,
) -> dict[str, Any]:
    """Capture one bounded, content-addressed Codex rollout inventory."""
    if role not in {"source", "destination"}:
        raise BulkloadError("Codex session capture role must be source or destination")
    if acknowledge_writers_quiesced is not True:
        raise BulkloadError(
            "Codex session capture requires explicit writer-quiescence acknowledgement"
        )
    host_authority_id = _canonical_host_authority_id(host_authority_id)
    budgets = _capture_budgets(
        max_files=max_files,
        max_entries=max_entries,
        max_directories=max_directories,
        max_bytes=max_bytes,
        max_file_bytes=max_file_bytes,
        max_record_bytes=max_record_bytes,
        max_records_per_file=max_records_per_file,
        max_path_bytes=max_path_bytes,
        max_path_components=max_path_components,
        max_catalog_bytes=max_catalog_bytes,
        max_errors=max_errors,
        max_output_bytes=max_output_bytes,
    )
    root = root.expanduser()
    root_text = os.fspath(root)
    if (
        not root.is_absolute()
        or os.path.normpath(root_text) != root_text
        or root_text == root.anchor
        or "\x00" in root_text
        or _utf8_size(root_text, "Codex session root") > budgets["max_path_bytes"]
    ):
        raise BulkloadError(
            "Codex session root must be a normalized non-root absolute path"
        )
    sessions: list[dict[str, Any]] = []
    directories: list[dict[str, Any]] = []
    errors: list[str] = []
    total_bytes = 0
    attempted_bytes = 0
    attempted_files = 0
    non_private_directories = 0
    stack: list[_DirectoryFrame] = []
    directory_states: dict[str, tuple[int, ...]] = {}
    root_descriptor: int | None = None
    root_path_info: os.stat_result | None = None
    root_descriptor_identity: tuple[int, ...] | None = None
    resolved_root: str | None = None
    root_lineage: list[dict[str, int]] = []
    observed_entries = 0
    observed_directories = 0

    try:
        root_path_info = root.lstat()
        if not _validate_stable_directory(root, root_path_info, role=role):
            non_private_directories += 1
        root_descriptor = _open_directory(
            root,
            parent_descriptor=None,
            path=root,
        )
        root_descriptor_info = os.fstat(root_descriptor)
        _validate_stable_directory(root, root_descriptor_info, role=role)
        if _stable_stat(root_descriptor_info) != _stable_stat(root_path_info):
            raise BulkloadError(f"Codex session root changed before capture: {root}")
        root_descriptor_identity = _stable_stat(root_descriptor_info)
        resolved_root_path = root.resolve(strict=True)
        resolved_root_text = str(resolved_root_path)
        if (
            _utf8_size(
                resolved_root_text,
                "Codex session resolved root",
            )
            > budgets["max_path_bytes"]
        ):
            raise _CaptureBudgetError(
                "Codex session resolved-root byte budget exceeded "
                f"({budgets['max_path_bytes']})"
            )
        resolved_root_info = resolved_root_path.stat()
        if _stable_stat(resolved_root_info)[:2] != root_descriptor_identity[:2]:
            raise BulkloadError(
                f"Codex session resolved root authority differs: {root}"
            )
        resolved_root = resolved_root_text
        root_lineage = _directory_lineage(root_descriptor)
        observed_directories = 1
        stack.append(
            _DirectoryFrame(
                descriptor=os.dup(root_descriptor),
                relative_path="",
                expected=root_descriptor_identity,
            )
        )
    except (OSError, BulkloadError) as error:
        try:
            _record_error(errors, error, budgets["max_errors"])
        except _CaptureHalt:
            pass

    try:
        while stack:
            frame = stack[-1]
            directory = root / frame.relative_path
            if frame.entries is None:
                try:
                    before_directory = os.fstat(frame.descriptor)
                    _validate_stable_directory(
                        directory,
                        before_directory,
                        role=role,
                    )
                    frame.before = _stable_stat(before_directory)
                    if frame.before != frame.expected:
                        raise BulkloadError(
                            f"Codex session directory changed before capture: "
                            f"{directory}"
                        )
                    with os.scandir(frame.descriptor) as iterator:
                        names: list[str] = []
                        for entry in iterator:
                            observed_entries += 1
                            if observed_entries > budgets["max_entries"]:
                                raise _CaptureBudgetError(
                                    "Codex session entry budget exceeded "
                                    f"({budgets['max_entries']})"
                                )
                            names.append(entry.name)
                        frame.entries = sorted(names)
                except (OSError, BulkloadError) as error:
                    try:
                        _record_error(
                            errors,
                            f"cannot enumerate Codex session directory "
                            f"{directory}: {error}",
                            budgets["max_errors"],
                        )
                    except _CaptureHalt:
                        raise
                    os.close(frame.descriptor)
                    stack.pop()
                    if isinstance(error, _CaptureBudgetError):
                        raise _CaptureHalt
                    continue

            if frame.index >= len(frame.entries):
                try:
                    after_directory = os.fstat(frame.descriptor)
                    if _stable_stat(after_directory) != frame.before:
                        raise BulkloadError(
                            f"Codex session directory changed during capture: "
                            f"{directory}"
                        )
                    if frame.before is not None:
                        directory_states[frame.relative_path] = frame.before
                except (OSError, BulkloadError) as error:
                    _record_error(
                        errors,
                        f"cannot revalidate Codex session directory "
                        f"{directory}: {error}",
                        budgets["max_errors"],
                    )
                os.close(frame.descriptor)
                stack.pop()
                continue

            name = frame.entries[frame.index]
            frame.index += 1
            relative_path = (
                f"{frame.relative_path}/{name}" if frame.relative_path else name
            )
            try:
                relative_path = _validate_relative_path(relative_path, budgets)
            except (BulkloadError, UnicodeEncodeError) as error:
                _record_error(errors, error, budgets["max_errors"])
                if isinstance(error, _CaptureBudgetError):
                    raise _CaptureHalt
                continue
            path = root / relative_path
            try:
                info = os.stat(
                    name,
                    dir_fd=frame.descriptor,
                    follow_symlinks=False,
                )
                if stat.S_ISLNK(info.st_mode):
                    raise BulkloadError(
                        f"unsupported Codex session filesystem entry: {path}"
                    )
                if stat.S_ISDIR(info.st_mode):
                    if observed_directories >= budgets["max_directories"]:
                        raise _CaptureBudgetError(
                            "Codex session directory budget exceeded "
                            f"({budgets['max_directories']})"
                        )
                    if not _validate_stable_directory(path, info, role=role):
                        non_private_directories += 1
                    child_descriptor = _open_directory(
                        name,
                        parent_descriptor=frame.descriptor,
                        path=path,
                    )
                    try:
                        child_info = os.fstat(child_descriptor)
                        _validate_stable_directory(
                            path,
                            child_info,
                            role=role,
                        )
                        if _stable_stat(child_info) != _stable_stat(info):
                            raise BulkloadError(
                                f"Codex session directory changed before "
                                f"capture: {path}"
                            )
                    except BaseException:
                        os.close(child_descriptor)
                        raise
                    observed_directories += 1
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
                match = _SESSION_ID.search(name)
                if not name.startswith("rollout-") or match is None:
                    raise BulkloadError(
                        f"unexpected regular file in Codex session store: {path}"
                    )
                session_id = _canonical_session_id(match.group("id"), path)
                if attempted_files >= budgets["max_files"]:
                    raise _CaptureBudgetError(
                        f"Codex session file budget exceeded ({budgets['max_files']})"
                    )
                attempted_files += 1
                if info.st_size > budgets["max_total_bytes"] - attempted_bytes:
                    raise _CaptureBudgetError(
                        "Codex session total byte budget exceeded "
                        f"({budgets['max_total_bytes']})"
                    )
                attempted_bytes += info.st_size
                record = _capture_rollout(
                    frame.descriptor,
                    name,
                    path,
                    relative_path,
                    session_id,
                    role,
                    budgets,
                    info,
                )
                if record["size"] > budgets["max_total_bytes"] - total_bytes:
                    raise _CaptureBudgetError(
                        "Codex session total byte budget exceeded "
                        f"({budgets['max_total_bytes']})"
                    )
                sessions.append(record)
                total_bytes += record["size"]
            except (OSError, BulkloadError) as error:
                _record_error(errors, error, budgets["max_errors"])
                if isinstance(error, _CaptureBudgetError):
                    raise _CaptureHalt
    except _CaptureHalt:
        pass
    finally:
        while stack:
            frame = stack.pop()
            try:
                os.close(frame.descriptor)
            except OSError:
                pass

    if root_descriptor is not None:
        revalidation_halted = False
        try:
            for relative_path, expected in sorted(directory_states.items()):
                try:
                    descriptor = _open_relative_directory(
                        root_descriptor,
                        relative_path,
                    )
                    try:
                        if _stable_stat(os.fstat(descriptor)) != expected:
                            raise BulkloadError(
                                "Codex session directory changed after its scan: "
                                f"{root / relative_path}"
                            )
                    finally:
                        os.close(descriptor)
                except (OSError, BulkloadError) as error:
                    try:
                        _record_error(
                            errors,
                            error,
                            budgets["max_errors"],
                        )
                    except _CaptureHalt:
                        revalidation_halted = True
                        break
            if not revalidation_halted:
                try:
                    if (
                        root_descriptor_identity is None
                        or _stable_stat(os.fstat(root_descriptor))
                        != root_descriptor_identity
                    ):
                        raise BulkloadError(
                            f"Codex session root changed during capture: {root}"
                        )
                except (OSError, BulkloadError) as error:
                    try:
                        _record_error(errors, error, budgets["max_errors"])
                    except _CaptureHalt:
                        revalidation_halted = True
            if not revalidation_halted:
                try:
                    if (
                        resolved_root is None
                        or str(root.resolve(strict=True)) != resolved_root
                        or _directory_lineage(root_descriptor) != root_lineage
                    ):
                        raise BulkloadError(
                            f"Codex session root lineage changed during capture: {root}"
                        )
                except (OSError, BulkloadError) as error:
                    try:
                        _record_error(errors, error, budgets["max_errors"])
                    except _CaptureHalt:
                        revalidation_halted = True
            if not revalidation_halted:
                try:
                    if root_path_info is None or _stable_stat(
                        root.lstat()
                    ) != _stable_stat(root_path_info):
                        raise BulkloadError(
                            f"Codex session root path changed during capture: {root}"
                        )
                except (OSError, BulkloadError) as error:
                    try:
                        _record_error(errors, error, budgets["max_errors"])
                    except _CaptureHalt:
                        revalidation_halted = True
            if not revalidation_halted:
                try:
                    reopened = _open_directory(
                        root,
                        parent_descriptor=None,
                        path=root,
                    )
                    try:
                        if _stable_stat(os.fstat(reopened)) != root_descriptor_identity:
                            raise BulkloadError(
                                "Codex session root authority changed during "
                                f"capture: {root}"
                            )
                    finally:
                        os.close(reopened)
                except (OSError, BulkloadError) as error:
                    try:
                        _record_error(errors, error, budgets["max_errors"])
                    except _CaptureHalt:
                        pass
        finally:
            os.close(root_descriptor)

    directories = [
        {
            "relative_path": relative_path,
            "mode": f"{state[3]:04o}",
            "identity": _identity_record(state),
        }
        for relative_path, state in sorted(directory_states.items())
        if relative_path
    ]
    sessions.sort(key=lambda item: (item["session_id"], item["relative_path"]))
    ids: set[str] = set()
    paths: set[str] = set()
    portable_paths: dict[str, str] = {}
    directory_paths: set[str] = set()
    portable_directory_paths: dict[str, str] = {}
    identity_collision = False
    for item in directories:
        relative_path = item["relative_path"]
        portable_key = _portable_path_key(relative_path)
        try:
            if relative_path in directory_paths:
                identity_collision = True
                _record_error(
                    errors,
                    f"duplicate Codex session directory path: {relative_path}",
                    budgets["max_errors"],
                )
            if (
                portable_key in portable_directory_paths
                and portable_directory_paths[portable_key] != relative_path
            ):
                identity_collision = True
                _record_error(
                    errors,
                    "Codex session portable directory namespace collision: "
                    f"{portable_directory_paths[portable_key]} and {relative_path}",
                    budgets["max_errors"],
                )
            portable_directory_paths[portable_key] = relative_path
            directory_paths.add(relative_path)
        except _CaptureHalt:
            break
    for item in sessions:
        try:
            if item["session_id"] in ids:
                identity_collision = True
                _record_error(
                    errors,
                    f"duplicate Codex session UUID: {item['session_id']}",
                    budgets["max_errors"],
                )
            if item["relative_path"] in paths:
                identity_collision = True
                _record_error(
                    errors,
                    f"duplicate Codex session relative path: {item['relative_path']}",
                    budgets["max_errors"],
                )
            portable_key = _portable_path_key(item["relative_path"])
            if portable_key in portable_directory_paths:
                identity_collision = True
                _record_error(
                    errors,
                    "Codex session file/directory namespace collision: "
                    f"{item['relative_path']} and "
                    f"{portable_directory_paths[portable_key]}",
                    budgets["max_errors"],
                )
            if (
                portable_key in portable_paths
                and portable_paths[portable_key] != item["relative_path"]
            ):
                identity_collision = True
                _record_error(
                    errors,
                    "Codex session portable-path namespace collision: "
                    f"{portable_paths[portable_key]} and {item['relative_path']}",
                    budgets["max_errors"],
                )
            portable_paths[portable_key] = item["relative_path"]
        except _CaptureHalt:
            break
        ids.add(item["session_id"])
        paths.add(item["relative_path"])

    if identity_collision:
        sessions = []
        directories = []
        total_bytes = 0
        non_private_directories = 0
    non_private_files = sum(int(int(item["mode"], 8) & 0o077 != 0) for item in sessions)
    catalog = {
        "directories": directories,
        "sessions": sessions,
        "non_private_file_count": non_private_files,
        "non_private_directory_count": non_private_directories,
    }
    if len(canonical_bytes(catalog)) > budgets["max_catalog_bytes"]:
        try:
            _record_error(
                errors,
                "Codex session catalog byte budget exceeded "
                f"({budgets['max_catalog_bytes']})",
                budgets["max_errors"],
            )
        except _CaptureHalt:
            pass
        sessions = []
        directories = []
        total_bytes = 0
        non_private_files = 0
        non_private_directories = 0
        catalog = {
            "directories": directories,
            "sessions": sessions,
            "non_private_file_count": non_private_files,
            "non_private_directory_count": non_private_directories,
        }

    snapshot: dict[str, Any] = {
        "schema": CODEX_SESSION_SNAPSHOT_SCHEMA,
        "captured_at": utc_now(),
        "capture_id": secrets.token_hex(16),
        "host": socket.gethostname(),
        "host_authority_id": host_authority_id,
        "root": str(root),
        "resolved_root": resolved_root,
        "root_identity": (
            {
                "device": root_descriptor_identity[0],
                "inode": root_descriptor_identity[1],
            }
            if root_descriptor_identity is not None
            else None
        ),
        "root_lineage": root_lineage,
        "role": role,
        "writers_quiesced": True,
        "budgets": budgets,
        "complete": not errors,
        "errors": errors,
        "directories": directories,
        "sessions": sessions,
        "total_bytes": total_bytes,
        "non_private_file_count": non_private_files,
        "non_private_directory_count": non_private_directories,
    }
    snapshot["catalog_sha256"] = sha256_bytes(canonical_bytes(catalog))
    snapshot["snapshot_sha256"] = object_digest(snapshot, "snapshot_sha256")
    if len(canonical_bytes(snapshot)) > budgets["max_output_bytes"]:
        snapshot.update(
            {
                "complete": False,
                "errors": [
                    "Codex session snapshot output byte budget exceeded "
                    f"({budgets['max_output_bytes']})"
                ],
                "directories": [],
                "sessions": [],
                "total_bytes": 0,
                "non_private_file_count": 0,
                "non_private_directory_count": 0,
            }
        )
        empty_catalog = {
            "directories": [],
            "sessions": [],
            "non_private_file_count": 0,
            "non_private_directory_count": 0,
        }
        snapshot["catalog_sha256"] = sha256_bytes(canonical_bytes(empty_catalog))
        snapshot["snapshot_sha256"] = object_digest(snapshot, "snapshot_sha256")
        if len(canonical_bytes(snapshot)) > budgets["max_output_bytes"]:
            raise BulkloadError(
                "Codex session snapshot output budget is too small for a "
                "bounded failure envelope"
            )
    return snapshot


def _valid_identity_record(value: Any) -> bool:
    return (
        isinstance(value, dict)
        and set(value) == {"device", "inode"}
        and type(value.get("device")) is int
        and 0 <= value["device"] < 2**128
        and type(value.get("inode")) is int
        and 0 < value["inode"] < 2**128
    )


def validate_codex_session_snapshot(snapshot: dict[str, Any]) -> None:
    expected_snapshot_keys = {
        "schema",
        "captured_at",
        "capture_id",
        "host",
        "host_authority_id",
        "root",
        "resolved_root",
        "root_identity",
        "root_lineage",
        "role",
        "writers_quiesced",
        "budgets",
        "complete",
        "errors",
        "directories",
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
    budgets = snapshot.get("budgets")
    if not isinstance(budgets, dict) or set(budgets) != set(_BUDGET_LIMITS):
        raise BulkloadError("Codex session snapshot budgets are invalid")
    validated_budgets = _capture_budgets(
        max_files=budgets.get("max_files"),
        max_entries=budgets.get("max_entries"),
        max_directories=budgets.get("max_directories"),
        max_bytes=budgets.get("max_total_bytes"),
        max_file_bytes=budgets.get("max_file_bytes"),
        max_record_bytes=budgets.get("max_record_bytes"),
        max_records_per_file=budgets.get("max_records_per_file"),
        max_path_bytes=budgets.get("max_path_bytes"),
        max_path_components=budgets.get("max_path_components"),
        max_catalog_bytes=budgets.get("max_catalog_bytes"),
        max_errors=budgets.get("max_errors"),
        max_output_bytes=budgets.get("max_output_bytes"),
    )
    if budgets != validated_budgets:
        raise BulkloadError("Codex session snapshot budgets are not canonical")
    root = snapshot.get("root")
    root_path = Path(root) if isinstance(root, str) else None
    resolved_root = snapshot.get("resolved_root")
    resolved_root_path = Path(resolved_root) if isinstance(resolved_root, str) else None
    valid_resolved_root = resolved_root is None or (
        resolved_root_path is not None
        and resolved_root_path.is_absolute()
        and os.path.normpath(resolved_root) == resolved_root
        and resolved_root != resolved_root_path.anchor
        and "\x00" not in resolved_root
        and _utf8_size(resolved_root, "Codex session resolved root")
        <= budgets["max_path_bytes"]
    )
    root_identity = snapshot.get("root_identity")
    valid_root_identity = root_identity is None or _valid_identity_record(root_identity)
    root_lineage = snapshot.get("root_lineage")
    valid_root_lineage = (
        isinstance(root_lineage, list)
        and len(root_lineage) <= MAX_CODEX_ROOT_LINEAGE
        and all(_valid_identity_record(identity) for identity in root_lineage)
        and len({(identity["device"], identity["inode"]) for identity in root_lineage})
        == len(root_lineage)
    )
    errors = snapshot.get("errors")
    if (
        not isinstance(snapshot.get("captured_at"), str)
        or not snapshot["captured_at"]
        or not isinstance(snapshot.get("host"), str)
        or not snapshot["host"]
        or _utf8_size(snapshot["host"], "Codex session host") > 255
        or not isinstance(snapshot.get("host_authority_id"), str)
        or _canonical_host_authority_id(snapshot.get("host_authority_id"))
        != snapshot.get("host_authority_id")
        or root_path is None
        or not root_path.is_absolute()
        or os.path.normpath(root) != root
        or root == root_path.anchor
        or "\x00" in root
        or _utf8_size(root, "Codex session root") > budgets["max_path_bytes"]
        or not valid_resolved_root
        or not valid_root_identity
        or not valid_root_lineage
        or (snapshot.get("complete") is True and root_identity is None)
        or (snapshot.get("complete") is True and resolved_root is None)
        or (
            snapshot.get("complete") is True
            and (not root_lineage or root_lineage[0] != root_identity)
        )
        or snapshot.get("role") not in {"source", "destination"}
        or snapshot.get("writers_quiesced") is not True
        or not isinstance(snapshot.get("complete"), bool)
        or not isinstance(errors, list)
        or len(errors) > budgets["max_errors"]
        or any(not isinstance(error, str) or not error for error in errors)
        or any(
            _utf8_size(error, "Codex session error")
            > budgets["max_path_bytes"] * 2 + 4096
            for error in errors
        )
        or snapshot["complete"] != (not errors)
    ):
        raise BulkloadError("Codex session snapshot envelope is invalid")
    directories = snapshot.get("directories")
    if (
        not isinstance(directories, list)
        or len(directories) >= budgets["max_directories"]
    ):
        raise BulkloadError("Codex session snapshot directories must be a list")
    seen_directories: set[str] = set()
    seen_portable_directories: set[str] = set()
    for item in directories:
        if not isinstance(item, dict) or set(item) != {
            "relative_path",
            "mode",
            "identity",
        }:
            raise BulkloadError("Codex session directory record is invalid")
        relative_path = item["relative_path"]
        if not isinstance(relative_path, str):
            raise BulkloadError("Codex session directory path is invalid")
        try:
            normalized_relative = _validate_relative_path(relative_path, budgets)
        except BulkloadError as error:
            raise BulkloadError("Codex session directory path is invalid") from error
        portable_path = _portable_path_key(normalized_relative)
        mode_valid = isinstance(item["mode"], str) and re.fullmatch(
            r"0[0-7]{3}", item["mode"]
        )
        mode = int(item["mode"], 8) if mode_valid else -1
        if (
            normalized_relative != relative_path
            or relative_path in seen_directories
            or portable_path in seen_portable_directories
            or not mode_valid
            or not _valid_identity_record(item["identity"])
            or (
                snapshot["role"] == "source" and (mode & 0o022 or mode & 0o500 != 0o500)
            )
            or (snapshot["role"] == "destination" and mode != 0o700)
        ):
            raise BulkloadError("Codex session directory record is invalid")
        seen_directories.add(relative_path)
        seen_portable_directories.add(portable_path)
    if directories != sorted(directories, key=lambda item: item["relative_path"]):
        raise BulkloadError("Codex session directories are not canonically ordered")
    if snapshot["complete"]:
        for relative_path in seen_directories:
            parents = PurePosixPath(relative_path).parents
            for parent in parents:
                parent_text = parent.as_posix()
                if parent_text != "." and parent_text not in seen_directories:
                    raise BulkloadError(
                        "Codex session directory parent closure is incomplete"
                    )
    sessions = snapshot.get("sessions")
    if not isinstance(sessions, list) or len(sessions) > budgets["max_files"]:
        raise BulkloadError("Codex session snapshot sessions must be a list")
    if not isinstance(snapshot.get("capture_id"), str) or not re.fullmatch(
        r"[0-9a-f]{32}", snapshot["capture_id"]
    ):
        raise BulkloadError("Codex session capture_id is invalid")
    seen_ids: set[str] = set()
    seen_paths: set[str] = set()
    seen_portable_paths: set[str] = set()
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
        if not isinstance(relative_path, str):
            raise BulkloadError("Codex session record identity is invalid")
        try:
            normalized_relative = _validate_relative_path(relative_path, budgets)
            canonical_session_id = _canonical_session_id(
                session_id,
                Path(relative_path),
            )
        except BulkloadError as error:
            raise BulkloadError("Codex session record identity is invalid") from error
        relative_name = PurePosixPath(normalized_relative).name
        relative_match = _SESSION_ID.search(relative_name)
        if (
            canonical_session_id != session_id
            or not _SESSION_ID.fullmatch(f"{session_id}.jsonl")
            or not relative_name.startswith("rollout-")
            or relative_match is None
            or relative_match.group("id") != session_id
        ):
            raise BulkloadError("Codex session record identity is invalid")
        portable_path = _portable_path_key(relative_path)
        if (
            session_id in seen_ids
            or relative_path in seen_paths
            or portable_path in seen_portable_paths
            or portable_path in seen_portable_directories
        ):
            raise BulkloadError("Codex session snapshot contains duplicate identity")
        if not isinstance(item["sha256"], str) or not re.fullmatch(
            r"[0-9a-f]{64}", item["sha256"]
        ):
            raise BulkloadError("Codex session record digest is invalid")
        mode_valid = isinstance(item["mode"], str) and re.fullmatch(
            r"0[0-7]{3}", item["mode"]
        )
        mode = int(item["mode"], 8) if mode_valid else -1
        if (
            type(item["size"]) is not int
            or item["size"] <= 0
            or item["size"] > budgets["max_file_bytes"]
            or type(item["records"]) is not int
            or item["records"] <= 0
            or item["records"] > budgets["max_records_per_file"]
            or not mode_valid
            or (
                snapshot["role"] == "source"
                and (mode & 0o022 or not mode & stat.S_IRUSR)
            )
            or (snapshot["role"] == "destination" and mode != 0o600)
        ):
            raise BulkloadError("Codex session record bounds or mode are invalid")
        seen_ids.add(session_id)
        seen_paths.add(relative_path)
        seen_portable_paths.add(portable_path)
        if snapshot["complete"]:
            for parent in PurePosixPath(relative_path).parents:
                parent_text = parent.as_posix()
                if parent_text != "." and parent_text not in seen_directories:
                    raise BulkloadError(
                        "Codex session file parent closure is incomplete"
                    )
        if item["size"] > budgets["max_total_bytes"] - computed_bytes:
            raise BulkloadError("Codex session total byte budget exceeded")
        computed_bytes += item["size"]
    if sessions != sorted(
        sessions,
        key=lambda item: (item["session_id"], item["relative_path"]),
    ):
        raise BulkloadError("Codex session records are not canonically ordered")
    if snapshot.get("total_bytes") != computed_bytes:
        raise BulkloadError("Codex session total_bytes mismatch")
    non_private_file_count = sum(
        int(int(item["mode"], 8) & 0o077 != 0) for item in sessions
    )
    if snapshot.get("non_private_file_count") != non_private_file_count:
        raise BulkloadError("Codex session non-private file count mismatch")
    if (
        type(snapshot.get("non_private_directory_count")) is not int
        or snapshot["non_private_directory_count"] < 0
        or snapshot["non_private_directory_count"] > budgets["max_directories"]
    ):
        raise BulkloadError("Codex session non-private directory count is invalid")
    if snapshot["role"] == "destination" and (
        non_private_file_count != 0 or snapshot["non_private_directory_count"] != 0
    ):
        raise BulkloadError("destination Codex session custody is not private")
    catalog = _catalog_body(snapshot)
    if len(canonical_bytes(catalog)) > budgets["max_catalog_bytes"]:
        raise BulkloadError("Codex session catalog byte budget exceeded")
    if snapshot.get("catalog_sha256") != sha256_bytes(canonical_bytes(catalog)):
        raise BulkloadError("Codex session catalog digest mismatch")
    if len(canonical_bytes(snapshot)) > budgets["max_output_bytes"]:
        raise BulkloadError("Codex session snapshot output byte budget exceeded")
    require_digest(snapshot, "snapshot_sha256")


def _catalog_body(snapshot: dict[str, Any]) -> dict[str, Any]:
    return {
        "directories": snapshot["directories"],
        "sessions": snapshot["sessions"],
        "non_private_file_count": snapshot["non_private_file_count"],
        "non_private_directory_count": snapshot["non_private_directory_count"],
    }


def _validate_stable_capture_pair(
    first: dict[str, Any],
    second: dict[str, Any],
    *,
    role: str,
) -> None:
    for snapshot in (first, second):
        validate_codex_session_snapshot(snapshot)
        if not snapshot["complete"]:
            raise BulkloadError("Codex session planning requires complete captures")
        if snapshot["role"] != role:
            raise BulkloadError(f"Codex session {role} capture has the wrong role")
        if snapshot["writers_quiesced"] is not True:
            raise BulkloadError(
                f"Codex session {role} capture lacks quiescence custody"
            )
    if first["capture_id"] == second["capture_id"]:
        raise BulkloadError(f"Codex session {role} passes must be distinct captures")
    if (
        first["host"] != second["host"]
        or first["host_authority_id"] != second["host_authority_id"]
        or first["root"] != second["root"]
        or first["resolved_root"] != second["resolved_root"]
        or first["root_identity"] != second["root_identity"]
        or first["root_lineage"] != second["root_lineage"]
        or first["budgets"] != second["budgets"]
        or first["catalog_sha256"] != second["catalog_sha256"]
        or canonical_bytes(_catalog_body(first))
        != canonical_bytes(_catalog_body(second))
    ):
        raise BulkloadError(f"Codex session {role} pass A and pass B differ")


def _classify_codex_session_union(
    source_sessions: list[dict[str, Any]],
    destination_sessions: list[dict[str, Any]],
    source_directories: list[dict[str, Any]],
    destination_directories: list[dict[str, Any]],
) -> dict[str, list[dict[str, Any]]]:
    source_by_id = {item["session_id"]: item for item in source_sessions}
    destination_by_id = {item["session_id"]: item for item in destination_sessions}
    source_by_path = {
        _portable_path_key(item["relative_path"]): item for item in source_sessions
    }
    destination_by_path = {
        _portable_path_key(item["relative_path"]): item for item in destination_sessions
    }
    source_directories_by_path = {
        _portable_path_key(item["relative_path"]): item for item in source_directories
    }
    destination_directories_by_path = {
        _portable_path_key(item["relative_path"]): item
        for item in destination_directories
    }
    copy_if_absent: list[dict[str, Any]] = []
    exact_common: list[dict[str, Any]] = []
    preserve_destination: list[dict[str, Any]] = []
    blockers: list[dict[str, Any]] = []

    for path_key in sorted(set(source_by_path) & set(destination_by_path)):
        source = source_by_path[path_key]
        destination = destination_by_path[path_key]
        if source["session_id"] != destination["session_id"]:
            blockers.append(
                {
                    "code": "relative-path-namespace-collision",
                    "source_session_id": source["session_id"],
                    "destination_session_id": destination["session_id"],
                    "source_relative_path": source["relative_path"],
                    "destination_relative_path": destination["relative_path"],
                }
            )

    for path_key in sorted(set(source_by_path) & set(destination_directories_by_path)):
        source = source_by_path[path_key]
        destination = destination_directories_by_path[path_key]
        blockers.append(
            {
                "code": "relative-path-type-collision",
                "source_claim_type": "file",
                "destination_claim_type": "directory",
                "source_relative_path": source["relative_path"],
                "destination_relative_path": destination["relative_path"],
            }
        )

    for path_key in sorted(set(source_directories_by_path) & set(destination_by_path)):
        source = source_directories_by_path[path_key]
        destination = destination_by_path[path_key]
        blockers.append(
            {
                "code": "relative-path-type-collision",
                "source_claim_type": "directory",
                "destination_claim_type": "file",
                "source_relative_path": source["relative_path"],
                "destination_relative_path": destination["relative_path"],
            }
        )

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
    if blockers:
        copy_if_absent = []
    return {
        "copy_if_absent": copy_if_absent,
        "exact_common": exact_common,
        "preserve_destination": preserve_destination,
        "blockers": blockers,
    }


def compile_codex_session_union_plan(
    source_a: dict[str, Any],
    source_b: dict[str, Any],
    destination_a: dict[str, Any],
    destination_b: dict[str, Any],
) -> dict[str, Any]:
    """Compile a read-only, absent-only UUID union report."""
    _validate_stable_capture_pair(source_a, source_b, role="source")
    _validate_stable_capture_pair(
        destination_a,
        destination_b,
        role="destination",
    )
    capture_ids = {
        source_a["capture_id"],
        source_b["capture_id"],
        destination_a["capture_id"],
        destination_b["capture_id"],
    }
    if len(capture_ids) != 4:
        raise BulkloadError("Codex session planning requires four distinct captures")
    if source_a["host_authority_id"] == destination_a["host_authority_id"]:
        source_root_identity = (
            source_a["root_identity"]["device"],
            source_a["root_identity"]["inode"],
        )
        destination_root_identity = (
            destination_a["root_identity"]["device"],
            destination_a["root_identity"]["inode"],
        )
        source_lineage = {
            (identity["device"], identity["inode"])
            for identity in source_a["root_lineage"]
        }
        destination_lineage = {
            (identity["device"], identity["inode"])
            for identity in destination_a["root_lineage"]
        }
        source_resolved = Path(source_a["resolved_root"])
        destination_resolved = Path(destination_a["resolved_root"])
        paths_overlap = False
        for candidate, possible_parent in (
            (source_resolved, destination_resolved),
            (destination_resolved, source_resolved),
        ):
            try:
                candidate.relative_to(possible_parent)
                paths_overlap = True
            except ValueError:
                pass
        if (
            source_root_identity in destination_lineage
            or destination_root_identity in source_lineage
            or paths_overlap
        ):
            raise BulkloadError(
                "Codex session source and destination roots overlap; "
                "source and destination must differ"
            )
    classified = _classify_codex_session_union(
        source_a["sessions"],
        destination_a["sessions"],
        source_a["directories"],
        destination_a["directories"],
    )

    intent = {
        "ready_for_attended_copy": not classified["blockers"],
        "copy_if_absent": classified["copy_if_absent"],
        "exact_common": classified["exact_common"],
        "preserve_destination": classified["preserve_destination"],
        "custody_findings": {
            "source_non_private_files": source_a["non_private_file_count"],
            "source_non_private_directories": source_a["non_private_directory_count"],
            "destination_non_private_files": destination_a["non_private_file_count"],
            "destination_non_private_directories": destination_a[
                "non_private_directory_count"
            ],
            "source_writers_quiesced": source_a["writers_quiesced"],
            "destination_writers_quiesced": destination_a["writers_quiesced"],
        },
        "blockers": classified["blockers"],
    }
    plan: dict[str, Any] = {
        "schema": CODEX_SESSION_PLAN_SCHEMA,
        "created_at": utc_now(),
        "source": {
            "host": source_a["host"],
            "host_authority_id": source_a["host_authority_id"],
            "root": source_a["root"],
            "resolved_root": source_a["resolved_root"],
            "root_identity": source_a["root_identity"],
            "root_lineage": source_a["root_lineage"],
            "catalog_sha256": source_a["catalog_sha256"],
            "capture_ids": [source_a["capture_id"], source_b["capture_id"]],
        },
        "destination": {
            "host": destination_a["host"],
            "host_authority_id": destination_a["host_authority_id"],
            "root": destination_a["root"],
            "resolved_root": destination_a["resolved_root"],
            "root_identity": destination_a["root_identity"],
            "root_lineage": destination_a["root_lineage"],
            "catalog_sha256": destination_a["catalog_sha256"],
            "capture_ids": [
                destination_a["capture_id"],
                destination_b["capture_id"],
            ],
        },
        "intent": intent,
    }
    plan["plan_sha256"] = object_digest(plan, "plan_sha256")
    if len(canonical_bytes(plan)) > MAX_CODEX_SESSION_PLAN_BYTES:
        raise BulkloadError("Codex session plan output byte budget exceeded")
    return plan
