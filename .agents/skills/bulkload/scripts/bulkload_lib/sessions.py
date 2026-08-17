"""Read-only Codex rollout catalogs and evidence-bound union plans."""

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
CODEX_SESSION_PREFIX_REQUEST_SCHEMA = (
    "dev.tinyland.bulkload.codex-session-prefix-request.v1"
)
CODEX_SESSION_PREFIX_PROOF_SCHEMA = (
    "dev.tinyland.bulkload.codex-session-prefix-proof.v1"
)
CODEX_SESSION_CLOSE_REQUEST_SCHEMA = (
    "dev.tinyland.bulkload.codex-session-close-request.v1"
)
CODEX_SESSION_CLOSE_CAPTURE_SCHEMA = (
    "dev.tinyland.bulkload.codex-session-close-capture.v1"
)
CODEX_SESSION_PLAN_SCHEMA = "dev.tinyland.bulkload.codex-session-union-plan.v3"
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
    else:
        if mode & 0o7000:
            raise BulkloadError(f"Codex rollout has privileged permission bits: {path}")
        if mode & 0o022 or not mode & stat.S_IRUSR:
            raise BulkloadError(
                f"Codex rollout is not owner-readable and "
                f"non-writable-by-others: {path}"
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
    else:
        if mode & 0o7000:
            raise BulkloadError(
                f"Codex session directory has privileged permission bits: {path}"
            )
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
        _validate_stable_directory(root, root_path_info, role=role)
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
                    _validate_stable_directory(path, info, role=role)
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
    non_private_files = sum(int(int(item["mode"], 8) & 0o077 != 0) for item in sessions)
    non_private_directories = sum(
        int(int(item["mode"], 8) & 0o077 != 0) for item in directories
    )
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
    if (
        type(snapshot.get("total_bytes")) is not int
        or snapshot["total_bytes"] != computed_bytes
    ):
        raise BulkloadError("Codex session total_bytes mismatch")
    non_private_file_count = sum(
        int(int(item["mode"], 8) & 0o077 != 0) for item in sessions
    )
    if (
        type(snapshot.get("non_private_file_count")) is not int
        or snapshot["non_private_file_count"] != non_private_file_count
    ):
        raise BulkloadError("Codex session non-private file count mismatch")
    non_private_directory_count = sum(
        int(int(item["mode"], 8) & 0o077 != 0) for item in directories
    )
    if (
        type(snapshot.get("non_private_directory_count")) is not int
        or snapshot["non_private_directory_count"] != non_private_directory_count
    ):
        raise BulkloadError("Codex session non-private directory count mismatch")
    if snapshot["role"] == "destination" and (
        non_private_file_count != 0 or non_private_directory_count != 0
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


def _capture_pair_binding(
    first: dict[str, Any],
    second: dict[str, Any],
) -> dict[str, Any]:
    return {
        "host": first["host"],
        "host_authority_id": first["host_authority_id"],
        "root": first["root"],
        "resolved_root": first["resolved_root"],
        "root_identity": first["root_identity"],
        "root_lineage": first["root_lineage"],
        "catalog_sha256": first["catalog_sha256"],
        "capture_ids": [first["capture_id"], second["capture_id"]],
    }


def _validate_union_capture_set(
    source_a: dict[str, Any],
    source_b: dict[str, Any],
    destination_a: dict[str, Any],
    destination_b: dict[str, Any],
) -> None:
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
    if source_a["host_authority_id"] != destination_a["host_authority_id"]:
        return
    source_root_identity = (
        source_a["root_identity"]["device"],
        source_a["root_identity"]["inode"],
    )
    destination_root_identity = (
        destination_a["root_identity"]["device"],
        destination_a["root_identity"]["inode"],
    )
    source_lineage = {
        (identity["device"], identity["inode"]) for identity in source_a["root_lineage"]
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


def _validate_closing_capture_pair(
    opening_a: dict[str, Any],
    opening_b: dict[str, Any],
    closing_a: dict[str, Any],
    closing_b: dict[str, Any],
    *,
    role: str,
) -> None:
    _validate_stable_capture_pair(closing_a, closing_b, role=role)
    stable_fields = (
        "role",
        "host",
        "host_authority_id",
        "root",
        "resolved_root",
        "root_identity",
        "root_lineage",
        "budgets",
        "catalog_sha256",
    )
    if (
        any(opening_a[field] != closing_a[field] for field in stable_fields)
        or any(opening_b[field] != closing_b[field] for field in stable_fields)
        or canonical_bytes(_catalog_body(opening_a))
        != canonical_bytes(_catalog_body(closing_a))
        or canonical_bytes(_catalog_body(opening_b))
        != canonical_bytes(_catalog_body(closing_b))
    ):
        raise BulkloadError(f"Codex session {role} opening and close captures differ")


def _proof_evidence_binding(proof: dict[str, Any]) -> dict[str, str]:
    return {
        "capture_id": proof["capture_id"],
        "proof_sha256": proof["proof_sha256"],
    }


def _prefix_requests(
    source_sessions: list[dict[str, Any]],
    destination_sessions: list[dict[str, Any]],
) -> list[dict[str, Any]]:
    source_by_id = {item["session_id"]: item for item in source_sessions}
    destination_by_id = {item["session_id"]: item for item in destination_sessions}
    requests: list[dict[str, Any]] = []
    for session_id in sorted(set(source_by_id) & set(destination_by_id)):
        source = source_by_id[session_id]
        destination = destination_by_id[session_id]
        if (
            source["sha256"] == destination["sha256"]
            and source["size"] == destination["size"]
        ) or source["size"] == destination["size"]:
            continue
        if source["size"] > destination["size"]:
            longer_role, longer = "source", source
            shorter_role, shorter = "destination", destination
        else:
            longer_role, longer = "destination", destination
            shorter_role, shorter = "source", source
        requests.append(
            {
                "session_id": session_id,
                "longer_role": longer_role,
                "longer_relative_path": longer["relative_path"],
                "longer_sha256": longer["sha256"],
                "longer_size": longer["size"],
                "shorter_role": shorter_role,
                "shorter_relative_path": shorter["relative_path"],
                "shorter_sha256": shorter["sha256"],
                "shorter_size": shorter["size"],
            }
        )
    return requests


def compile_codex_session_prefix_request(
    source_a: dict[str, Any],
    source_b: dict[str, Any],
    destination_a: dict[str, Any],
    destination_b: dict[str, Any],
) -> dict[str, Any]:
    """Compile immutable requests for byte-prefix observations."""
    _validate_union_capture_set(
        source_a,
        source_b,
        destination_a,
        destination_b,
    )
    request: dict[str, Any] = {
        "schema": CODEX_SESSION_PREFIX_REQUEST_SCHEMA,
        "created_at": utc_now(),
        "source": _capture_pair_binding(source_a, source_b),
        "destination": _capture_pair_binding(destination_a, destination_b),
        "requests": _prefix_requests(
            source_a["sessions"],
            destination_a["sessions"],
        ),
    }
    request["request_sha256"] = object_digest(request, "request_sha256")
    if len(canonical_bytes(request)) > MAX_CODEX_SESSION_PLAN_BYTES:
        raise BulkloadError("Codex session prefix request output byte budget exceeded")
    return request


def compile_codex_session_prefix_requests(
    source_a: dict[str, Any],
    source_b: dict[str, Any],
    destination_a: dict[str, Any],
    destination_b: dict[str, Any],
) -> dict[str, Any]:
    """Compatibility spelling for callers treating the manifest as a set."""
    return compile_codex_session_prefix_request(
        source_a,
        source_b,
        destination_a,
        destination_b,
    )


def _valid_prefix_binding(value: Any) -> bool:
    if not isinstance(value, dict) or set(value) != {
        "host",
        "host_authority_id",
        "root",
        "resolved_root",
        "root_identity",
        "root_lineage",
        "catalog_sha256",
        "capture_ids",
    }:
        return False
    capture_ids = value.get("capture_ids")
    return (
        isinstance(value.get("host"), str)
        and bool(value["host"])
        and "\x00" not in value["host"]
        and _utf8_size(value["host"], "Codex session prefix host") <= 255
        and isinstance(value.get("host_authority_id"), str)
        and _canonical_host_authority_id(value["host_authority_id"])
        == value["host_authority_id"]
        and isinstance(value.get("root"), str)
        and "\x00" not in value["root"]
        and _utf8_size(value["root"], "Codex session prefix root")
        <= _BUDGET_LIMITS["max_path_bytes"]
        and Path(value["root"]).is_absolute()
        and os.path.normpath(value["root"]) == value["root"]
        and value["root"] != Path(value["root"]).anchor
        and isinstance(value.get("resolved_root"), str)
        and "\x00" not in value["resolved_root"]
        and _utf8_size(
            value["resolved_root"],
            "Codex session prefix resolved root",
        )
        <= _BUDGET_LIMITS["max_path_bytes"]
        and Path(value["resolved_root"]).is_absolute()
        and os.path.normpath(value["resolved_root"]) == value["resolved_root"]
        and value["resolved_root"] != Path(value["resolved_root"]).anchor
        and _valid_identity_record(value.get("root_identity"))
        and isinstance(value.get("root_lineage"), list)
        and 0 < len(value["root_lineage"]) <= MAX_CODEX_ROOT_LINEAGE
        and value["root_lineage"][0] == value["root_identity"]
        and all(_valid_identity_record(item) for item in value["root_lineage"])
        and len({(item["device"], item["inode"]) for item in value["root_lineage"]})
        == len(value["root_lineage"])
        and isinstance(value.get("catalog_sha256"), str)
        and re.fullmatch(r"[0-9a-f]{64}", value["catalog_sha256"]) is not None
        and isinstance(capture_ids, list)
        and len(capture_ids) == 2
        and len(set(capture_ids)) == 2
        and all(
            isinstance(capture_id, str) and re.fullmatch(r"[0-9a-f]{32}", capture_id)
            for capture_id in capture_ids
        )
    )


def validate_codex_session_prefix_request(request: dict[str, Any]) -> None:
    if set(request) != {
        "schema",
        "created_at",
        "source",
        "destination",
        "requests",
        "request_sha256",
    }:
        raise BulkloadError("Codex session prefix request has unexpected fields")
    if request.get("schema") != CODEX_SESSION_PREFIX_REQUEST_SCHEMA:
        raise BulkloadError("unsupported Codex session prefix request schema")
    if (
        not isinstance(request.get("created_at"), str)
        or not request["created_at"]
        or not _valid_prefix_binding(request.get("source"))
        or not _valid_prefix_binding(request.get("destination"))
    ):
        raise BulkloadError("Codex session prefix request envelope is invalid")
    capture_ids = [
        *request["source"]["capture_ids"],
        *request["destination"]["capture_ids"],
    ]
    if len(set(capture_ids)) != 4:
        raise BulkloadError(
            "Codex session prefix request requires four distinct captures"
        )
    requests = request.get("requests")
    if not isinstance(requests, list) or len(requests) > _BUDGET_LIMITS["max_files"]:
        raise BulkloadError("Codex session prefix requests are invalid")
    seen: set[str] = set()
    path_budgets = {
        "max_path_bytes": _BUDGET_LIMITS["max_path_bytes"],
        "max_path_components": _BUDGET_LIMITS["max_path_components"],
    }
    for item in requests:
        if not isinstance(item, dict) or set(item) != {
            "session_id",
            "longer_role",
            "longer_relative_path",
            "longer_sha256",
            "longer_size",
            "shorter_role",
            "shorter_relative_path",
            "shorter_sha256",
            "shorter_size",
        }:
            raise BulkloadError("Codex session prefix request record is invalid")
        try:
            session_id = _canonical_session_id(
                item["session_id"],
                Path(str(item.get("longer_relative_path", ""))),
            )
            longer_path = _validate_relative_path(
                item["longer_relative_path"],
                path_budgets,
            )
            shorter_path = _validate_relative_path(
                item["shorter_relative_path"],
                path_budgets,
            )
        except (BulkloadError, TypeError) as error:
            raise BulkloadError(
                "Codex session prefix request record is invalid"
            ) from error
        if (
            session_id != item["session_id"]
            or session_id in seen
            or longer_path != item["longer_relative_path"]
            or shorter_path != item["shorter_relative_path"]
            or item["longer_role"] not in {"source", "destination"}
            or item["shorter_role"] not in {"source", "destination"}
            or item["longer_role"] == item["shorter_role"]
            or not isinstance(item["longer_sha256"], str)
            or re.fullmatch(r"[0-9a-f]{64}", item["longer_sha256"]) is None
            or not isinstance(item["shorter_sha256"], str)
            or re.fullmatch(r"[0-9a-f]{64}", item["shorter_sha256"]) is None
            or type(item["longer_size"]) is not int
            or type(item["shorter_size"]) is not int
            or item["shorter_size"] < 1
            or item["longer_size"] <= item["shorter_size"]
            or item["longer_size"] > _BUDGET_LIMITS["max_file_bytes"]
        ):
            raise BulkloadError("Codex session prefix request record is invalid")
        seen.add(session_id)
    if requests != sorted(requests, key=lambda item: item["session_id"]):
        raise BulkloadError("Codex session prefix requests are not canonically ordered")
    if len(canonical_bytes(request)) > MAX_CODEX_SESSION_PLAN_BYTES:
        raise BulkloadError("Codex session prefix request output byte budget exceeded")
    require_digest(request, "request_sha256")


def _validate_prefix_request_against_captures(
    request: dict[str, Any],
    source_a: dict[str, Any],
    source_b: dict[str, Any],
    destination_a: dict[str, Any],
    destination_b: dict[str, Any],
) -> None:
    validate_codex_session_prefix_request(request)
    _validate_union_capture_set(
        source_a,
        source_b,
        destination_a,
        destination_b,
    )
    if (
        request["source"] != _capture_pair_binding(source_a, source_b)
        or request["destination"] != _capture_pair_binding(destination_a, destination_b)
        or request["requests"]
        != _prefix_requests(source_a["sessions"], destination_a["sessions"])
    ):
        raise BulkloadError(
            "Codex session prefix request does not bind the supplied captures"
        )


def _capture_requested_prefix(
    root_descriptor: int,
    root: Path,
    *,
    role: str,
    request: dict[str, Any],
) -> dict[str, Any]:
    relative_path = request["longer_relative_path"]
    parent_text = PurePosixPath(relative_path).parent.as_posix()
    if parent_text == ".":
        parent_text = ""
    parent_descriptor = _open_relative_directory(root_descriptor, parent_text)
    name = PurePosixPath(relative_path).name
    path = root / relative_path
    try:
        parent_before = _stable_stat(os.fstat(parent_descriptor))
        entry_before = os.stat(
            name,
            dir_fd=parent_descriptor,
            follow_symlinks=False,
        )
        _stable_user_regular(entry_before, path, role=role)
        if entry_before.st_size != request["longer_size"]:
            raise BulkloadError(
                f"Codex rollout size changed before prefix proof: {path}"
            )
        flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0)
        descriptor = os.open(name, flags, dir_fd=parent_descriptor)
        full_digest = hashlib.sha256()
        prefix_digest = hashlib.sha256()
        prefix_remaining = request["shorter_size"]
        prefix_last_byte: int | None = None
        bytes_read = 0
        try:
            descriptor_before = os.fstat(descriptor)
            _stable_user_regular(descriptor_before, path, role=role)
            if _stable_stat(descriptor_before) != _stable_stat(entry_before):
                raise BulkloadError(
                    f"Codex rollout changed before prefix proof: {path}"
                )
            while True:
                block = os.read(descriptor, 1024 * 1024)
                if not block:
                    break
                bytes_read += len(block)
                if bytes_read > request["longer_size"]:
                    raise BulkloadError(
                        f"Codex rollout grew during prefix proof: {path}"
                    )
                full_digest.update(block)
                if prefix_remaining:
                    prefix_block = block[:prefix_remaining]
                    prefix_digest.update(prefix_block)
                    prefix_remaining -= len(prefix_block)
                    if prefix_block:
                        prefix_last_byte = prefix_block[-1]
            descriptor_after = os.fstat(descriptor)
        finally:
            os.close(descriptor)
        entry_after = os.stat(
            name,
            dir_fd=parent_descriptor,
            follow_symlinks=False,
        )
        if (
            _stable_stat(descriptor_before) != _stable_stat(descriptor_after)
            or _stable_stat(descriptor_before) != _stable_stat(entry_after)
            or bytes_read != request["longer_size"]
            or prefix_remaining != 0
            or full_digest.hexdigest() != request["longer_sha256"]
        ):
            raise BulkloadError(f"Codex rollout changed during prefix proof: {path}")
        if _stable_stat(os.fstat(parent_descriptor)) != parent_before:
            raise BulkloadError(
                f"Codex session directory changed during prefix proof: {path.parent}"
            )
    finally:
        os.close(parent_descriptor)
    reopened_parent = _open_relative_directory(root_descriptor, parent_text)
    try:
        if _stable_stat(os.fstat(reopened_parent)) != parent_before:
            raise BulkloadError(
                f"Codex session directory changed during prefix proof: {path.parent}"
            )
    finally:
        os.close(reopened_parent)
    return {
        "session_id": request["session_id"],
        "relative_path": relative_path,
        "sha256": full_digest.hexdigest(),
        "size": bytes_read,
        "prefix_sha256": prefix_digest.hexdigest(),
        "prefix_size": request["shorter_size"],
        "prefix_ends_at_record": prefix_last_byte == ord("\n"),
    }


def capture_codex_session_prefix_proof(
    root: Path,
    *,
    role: str,
    prefix_request: dict[str, Any],
    source_a: dict[str, Any],
    source_b: dict[str, Any],
    destination_a: dict[str, Any],
    destination_b: dict[str, Any],
    acknowledge_writers_quiesced: bool,
) -> dict[str, Any]:
    """Capture one repeated, role-bound observation of requested prefixes."""
    if role not in {"source", "destination"}:
        raise BulkloadError("Codex session prefix proof role is invalid")
    if acknowledge_writers_quiesced is not True:
        raise BulkloadError(
            "Codex session prefix proof requires explicit writer quiescence"
        )
    _validate_prefix_request_against_captures(
        prefix_request,
        source_a,
        source_b,
        destination_a,
        destination_b,
    )
    snapshot = source_a if role == "source" else destination_a
    requested = [
        item for item in prefix_request["requests"] if item["longer_role"] == role
    ]
    if not requested:
        raise BulkloadError(f"Codex session prefix request has no {role} observations")
    sessions = {item["session_id"]: item for item in snapshot["sessions"]}
    for item in requested:
        session = sessions.get(item["session_id"])
        if (
            session is None
            or session["relative_path"] != item["longer_relative_path"]
            or session["sha256"] != item["longer_sha256"]
            or session["size"] != item["longer_size"]
        ):
            raise BulkloadError(
                "Codex session prefix request is absent from the bound catalog"
            )

    root = root.expanduser()
    root_text = os.fspath(root)
    if (
        not root.is_absolute()
        or os.path.normpath(root_text) != root_text
        or root_text != snapshot["root"]
    ):
        raise BulkloadError("Codex session prefix proof root does not match capture")
    root_info = root.lstat()
    _validate_stable_directory(root, root_info, role=role)
    root_descriptor = _open_directory(root, parent_descriptor=None, path=root)
    try:
        descriptor_info = os.fstat(root_descriptor)
        _validate_stable_directory(root, descriptor_info, role=role)
        expected_root_identity = (
            snapshot["root_identity"]["device"],
            snapshot["root_identity"]["inode"],
        )
        if (
            _stable_stat(descriptor_info) != _stable_stat(root_info)
            or (descriptor_info.st_dev, descriptor_info.st_ino)
            != expected_root_identity
            or str(root.resolve(strict=True)) != snapshot["resolved_root"]
            or _directory_lineage(root_descriptor) != snapshot["root_lineage"]
        ):
            raise BulkloadError("Codex session prefix proof root authority changed")
        proofs = [
            _capture_requested_prefix(
                root_descriptor,
                root,
                role=role,
                request=item,
            )
            for item in requested
        ]
        descriptor_after = os.fstat(root_descriptor)
        if (
            _stable_stat(descriptor_after) != _stable_stat(descriptor_info)
            or _stable_stat(root.lstat()) != _stable_stat(root_info)
            or str(root.resolve(strict=True)) != snapshot["resolved_root"]
            or _directory_lineage(root_descriptor) != snapshot["root_lineage"]
        ):
            raise BulkloadError(
                "Codex session prefix proof root authority changed during capture"
            )
        reopened = _open_directory(root, parent_descriptor=None, path=root)
        try:
            if _stable_stat(os.fstat(reopened)) != _stable_stat(descriptor_info):
                raise BulkloadError(
                    "Codex session prefix proof root authority changed during capture"
                )
        finally:
            os.close(reopened)
    finally:
        os.close(root_descriptor)
    proof: dict[str, Any] = {
        "schema": CODEX_SESSION_PREFIX_PROOF_SCHEMA,
        "captured_at": utc_now(),
        "capture_id": secrets.token_hex(16),
        "role": role,
        "writers_quiesced": True,
        "request_sha256": prefix_request["request_sha256"],
        "binding": _capture_pair_binding(
            source_a if role == "source" else destination_a,
            source_b if role == "source" else destination_b,
        ),
        "proofs": proofs,
    }
    proof["proofs_sha256"] = sha256_bytes(canonical_bytes(proofs))
    proof["proof_sha256"] = object_digest(proof, "proof_sha256")
    if len(canonical_bytes(proof)) > MAX_CODEX_SESSION_PLAN_BYTES:
        raise BulkloadError("Codex session prefix proof output byte budget exceeded")
    return proof


def validate_codex_session_prefix_proof(proof: dict[str, Any]) -> None:
    if set(proof) != {
        "schema",
        "captured_at",
        "capture_id",
        "role",
        "writers_quiesced",
        "request_sha256",
        "binding",
        "proofs",
        "proofs_sha256",
        "proof_sha256",
    }:
        raise BulkloadError("Codex session prefix proof has unexpected fields")
    if proof.get("schema") != CODEX_SESSION_PREFIX_PROOF_SCHEMA:
        raise BulkloadError("unsupported Codex session prefix proof schema")
    if (
        not isinstance(proof.get("captured_at"), str)
        or not proof["captured_at"]
        or not isinstance(proof.get("capture_id"), str)
        or re.fullmatch(r"[0-9a-f]{32}", proof["capture_id"]) is None
        or proof.get("role") not in {"source", "destination"}
        or proof.get("writers_quiesced") is not True
        or not isinstance(proof.get("request_sha256"), str)
        or re.fullmatch(r"[0-9a-f]{64}", proof["request_sha256"]) is None
        or not _valid_prefix_binding(proof.get("binding"))
    ):
        raise BulkloadError("Codex session prefix proof envelope is invalid")
    proofs = proof.get("proofs")
    if not isinstance(proofs, list) or len(proofs) > _BUDGET_LIMITS["max_files"]:
        raise BulkloadError("Codex session prefix proof records are invalid")
    seen: set[str] = set()
    path_budgets = {
        "max_path_bytes": _BUDGET_LIMITS["max_path_bytes"],
        "max_path_components": _BUDGET_LIMITS["max_path_components"],
    }
    for item in proofs:
        if not isinstance(item, dict) or set(item) != {
            "session_id",
            "relative_path",
            "sha256",
            "size",
            "prefix_sha256",
            "prefix_size",
            "prefix_ends_at_record",
        }:
            raise BulkloadError("Codex session prefix proof record is invalid")
        try:
            session_id = _canonical_session_id(
                item["session_id"],
                Path(str(item.get("relative_path", ""))),
            )
            relative_path = _validate_relative_path(
                item["relative_path"],
                path_budgets,
            )
        except (BulkloadError, TypeError) as error:
            raise BulkloadError(
                "Codex session prefix proof record is invalid"
            ) from error
        if (
            session_id != item["session_id"]
            or session_id in seen
            or relative_path != item["relative_path"]
            or not isinstance(item["sha256"], str)
            or re.fullmatch(r"[0-9a-f]{64}", item["sha256"]) is None
            or not isinstance(item["prefix_sha256"], str)
            or re.fullmatch(r"[0-9a-f]{64}", item["prefix_sha256"]) is None
            or type(item["size"]) is not int
            or type(item["prefix_size"]) is not int
            or item["size"] <= item["prefix_size"]
            or item["prefix_size"] < 1
            or item["size"] > _BUDGET_LIMITS["max_file_bytes"]
            or type(item["prefix_ends_at_record"]) is not bool
        ):
            raise BulkloadError("Codex session prefix proof record is invalid")
        seen.add(session_id)
    if proofs != sorted(proofs, key=lambda item: item["session_id"]):
        raise BulkloadError("Codex session prefix proofs are not canonically ordered")
    if proof.get("proofs_sha256") != sha256_bytes(canonical_bytes(proofs)):
        raise BulkloadError("Codex session prefix proof record digest mismatch")
    if len(canonical_bytes(proof)) > MAX_CODEX_SESSION_PLAN_BYTES:
        raise BulkloadError("Codex session prefix proof output byte budget exceeded")
    require_digest(proof, "proof_sha256")


def _validate_prefix_proof_pair(
    first: dict[str, Any],
    second: dict[str, Any],
    *,
    role: str,
    request: dict[str, Any],
    binding: dict[str, Any],
) -> dict[str, dict[str, Any]]:
    expected_requests = [
        item for item in request["requests"] if item["longer_role"] == role
    ]
    if not expected_requests:
        raise BulkloadError(f"Codex session {role} prefix proof is unexpected")
    for proof in (first, second):
        validate_codex_session_prefix_proof(proof)
        if (
            proof["role"] != role
            or proof["request_sha256"] != request["request_sha256"]
            or proof["binding"] != binding
        ):
            raise BulkloadError(f"Codex session {role} prefix proof binding is invalid")
    if first["capture_id"] == second["capture_id"]:
        raise BulkloadError(
            f"Codex session {role} prefix passes must be distinct captures"
        )
    if first["proofs"] != second["proofs"]:
        raise BulkloadError(f"Codex session {role} prefix pass A and pass B differ")
    expected_by_id = {item["session_id"]: item for item in expected_requests}
    proof_by_id = {item["session_id"]: item for item in first["proofs"]}
    if set(proof_by_id) != set(expected_by_id):
        raise BulkloadError(f"Codex session {role} prefix proof set is incomplete")
    for session_id, item in proof_by_id.items():
        expected = expected_by_id[session_id]
        if (
            item["relative_path"] != expected["longer_relative_path"]
            or item["sha256"] != expected["longer_sha256"]
            or item["size"] != expected["longer_size"]
            or item["prefix_size"] != expected["shorter_size"]
        ):
            raise BulkloadError(f"Codex session {role} prefix proof claim is invalid")
    return proof_by_id


def _validate_required_prefix_proofs(
    request: dict[str, Any],
    source_a: dict[str, Any],
    source_b: dict[str, Any],
    destination_a: dict[str, Any],
    destination_b: dict[str, Any],
    *,
    source_prefix_a: dict[str, Any] | None,
    source_prefix_b: dict[str, Any] | None,
    destination_prefix_a: dict[str, Any] | None,
    destination_prefix_b: dict[str, Any] | None,
) -> tuple[
    dict[str, dict[str, Any]],
    list[dict[str, str]],
    list[dict[str, str]],
    list[str],
]:
    prefix_proofs: dict[str, dict[str, Any]] = {}
    proof_bindings_by_role: dict[str, list[dict[str, str]]] = {
        "source": [],
        "destination": [],
    }
    proof_capture_ids: list[str] = []
    for role, first, second, binding in (
        (
            "source",
            source_prefix_a,
            source_prefix_b,
            _capture_pair_binding(source_a, source_b),
        ),
        (
            "destination",
            destination_prefix_a,
            destination_prefix_b,
            _capture_pair_binding(destination_a, destination_b),
        ),
    ):
        role_requests = [
            item for item in request["requests"] if item["longer_role"] == role
        ]
        if role_requests and (first is None or second is None):
            raise BulkloadError(
                f"Codex session {role} prefix proof requires pass A and pass B"
            )
        if not role_requests and (first is not None or second is not None):
            raise BulkloadError(f"Codex session {role} prefix proof is unexpected")
        if not role_requests:
            continue
        validated = _validate_prefix_proof_pair(
            first,
            second,
            role=role,
            request=request,
            binding=binding,
        )
        prefix_proofs.update(validated)
        proof_bindings_by_role[role] = [
            _proof_evidence_binding(first),
            _proof_evidence_binding(second),
        ]
        proof_capture_ids.extend([first["capture_id"], second["capture_id"]])
    return (
        prefix_proofs,
        proof_bindings_by_role["source"],
        proof_bindings_by_role["destination"],
        proof_capture_ids,
    )


def _close_custody_binding(
    first: dict[str, Any],
    second: dict[str, Any],
    *,
    role: str,
) -> dict[str, Any]:
    return {
        "role": role,
        **_capture_pair_binding(first, second),
        "budgets": first["budgets"],
    }


def _validate_close_custody(value: Any, *, role: str) -> None:
    expected_keys = {
        "role",
        "host",
        "host_authority_id",
        "root",
        "resolved_root",
        "root_identity",
        "root_lineage",
        "catalog_sha256",
        "capture_ids",
        "budgets",
    }
    if not isinstance(value, dict) or set(value) != expected_keys:
        raise BulkloadError(f"Codex session {role} close custody is invalid")
    binding = {
        key: item for key, item in value.items() if key not in {"role", "budgets"}
    }
    budgets = value["budgets"]
    if (
        value["role"] != role
        or not _valid_prefix_binding(binding)
        or not isinstance(budgets, dict)
        or set(budgets) != set(_BUDGET_LIMITS)
    ):
        raise BulkloadError(f"Codex session {role} close custody is invalid")
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
        raise BulkloadError(f"Codex session {role} close custody is invalid")


def _validate_proof_evidence_bindings(value: Any, *, role: str) -> None:
    if not isinstance(value, list) or len(value) not in {0, 2}:
        raise BulkloadError(f"Codex session {role} proof bindings are invalid")
    seen: set[str] = set()
    for item in value:
        if (
            not isinstance(item, dict)
            or set(item) != {"capture_id", "proof_sha256"}
            or not isinstance(item["capture_id"], str)
            or re.fullmatch(r"[0-9a-f]{32}", item["capture_id"]) is None
            or item["capture_id"] in seen
            or not isinstance(item["proof_sha256"], str)
            or re.fullmatch(r"[0-9a-f]{64}", item["proof_sha256"]) is None
        ):
            raise BulkloadError(f"Codex session {role} proof bindings are invalid")
        seen.add(item["capture_id"])


def compile_codex_session_close_request(
    prefix_request: dict[str, Any],
    source_a: dict[str, Any],
    source_b: dict[str, Any],
    destination_a: dict[str, Any],
    destination_b: dict[str, Any],
    *,
    source_prefix_a: dict[str, Any] | None = None,
    source_prefix_b: dict[str, Any] | None = None,
    destination_prefix_a: dict[str, Any] | None = None,
    destination_prefix_b: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Bind every required prefix proof before any close capture begins."""
    _validate_prefix_request_against_captures(
        prefix_request,
        source_a,
        source_b,
        destination_a,
        destination_b,
    )
    if not prefix_request["requests"]:
        raise BulkloadError("Codex session close request requires prefix observations")
    (
        _,
        source_proof_bindings,
        destination_proof_bindings,
        proof_capture_ids,
    ) = _validate_required_prefix_proofs(
        prefix_request,
        source_a,
        source_b,
        destination_a,
        destination_b,
        source_prefix_a=source_prefix_a,
        source_prefix_b=source_prefix_b,
        destination_prefix_a=destination_prefix_a,
        destination_prefix_b=destination_prefix_b,
    )
    opening_capture_ids = [
        source_a["capture_id"],
        source_b["capture_id"],
        destination_a["capture_id"],
        destination_b["capture_id"],
    ]
    if len(set([*opening_capture_ids, *proof_capture_ids])) != len(
        [*opening_capture_ids, *proof_capture_ids]
    ):
        raise BulkloadError(
            "Codex session opening and proof captures must be globally distinct"
        )
    close_request: dict[str, Any] = {
        "schema": CODEX_SESSION_CLOSE_REQUEST_SCHEMA,
        "created_at": utc_now(),
        "prefix_request_sha256": prefix_request["request_sha256"],
        "source": _close_custody_binding(source_a, source_b, role="source"),
        "destination": _close_custody_binding(
            destination_a,
            destination_b,
            role="destination",
        ),
        "source_prefix_proofs": source_proof_bindings,
        "destination_prefix_proofs": destination_proof_bindings,
    }
    close_request["close_request_sha256"] = object_digest(
        close_request,
        "close_request_sha256",
    )
    if len(canonical_bytes(close_request)) > MAX_CODEX_SESSION_PLAN_BYTES:
        raise BulkloadError("Codex session close request output byte budget exceeded")
    return close_request


def validate_codex_session_close_request(
    close_request: dict[str, Any],
) -> None:
    if set(close_request) != {
        "schema",
        "created_at",
        "prefix_request_sha256",
        "source",
        "destination",
        "source_prefix_proofs",
        "destination_prefix_proofs",
        "close_request_sha256",
    }:
        raise BulkloadError("Codex session close request has unexpected fields")
    if close_request.get("schema") != CODEX_SESSION_CLOSE_REQUEST_SCHEMA:
        raise BulkloadError("unsupported Codex session close request schema")
    if (
        not isinstance(close_request.get("created_at"), str)
        or not close_request["created_at"]
        or not isinstance(close_request.get("prefix_request_sha256"), str)
        or re.fullmatch(
            r"[0-9a-f]{64}",
            close_request["prefix_request_sha256"],
        )
        is None
    ):
        raise BulkloadError("Codex session close request envelope is invalid")
    _validate_close_custody(close_request.get("source"), role="source")
    _validate_close_custody(
        close_request.get("destination"),
        role="destination",
    )
    _validate_proof_evidence_bindings(
        close_request.get("source_prefix_proofs"),
        role="source",
    )
    _validate_proof_evidence_bindings(
        close_request.get("destination_prefix_proofs"),
        role="destination",
    )
    if not (
        close_request["source_prefix_proofs"]
        or close_request["destination_prefix_proofs"]
    ):
        raise BulkloadError("Codex session close request has no prefix proofs")
    capture_ids = [
        *close_request["source"]["capture_ids"],
        *close_request["destination"]["capture_ids"],
        *[item["capture_id"] for item in close_request["source_prefix_proofs"]],
        *[item["capture_id"] for item in close_request["destination_prefix_proofs"]],
    ]
    if len(set(capture_ids)) != len(capture_ids):
        raise BulkloadError(
            "Codex session close request capture IDs are not globally distinct"
        )
    if len(canonical_bytes(close_request)) > MAX_CODEX_SESSION_PLAN_BYTES:
        raise BulkloadError("Codex session close request output byte budget exceeded")
    require_digest(close_request, "close_request_sha256")


def _validate_close_request_against_inputs(
    close_request: dict[str, Any],
    prefix_request: dict[str, Any],
    source_a: dict[str, Any],
    source_b: dict[str, Any],
    destination_a: dict[str, Any],
    destination_b: dict[str, Any],
    *,
    source_prefix_a: dict[str, Any] | None,
    source_prefix_b: dict[str, Any] | None,
    destination_prefix_a: dict[str, Any] | None,
    destination_prefix_b: dict[str, Any] | None,
) -> tuple[
    dict[str, dict[str, Any]],
    list[dict[str, str]],
    list[dict[str, str]],
    list[str],
]:
    validate_codex_session_close_request(close_request)
    _validate_prefix_request_against_captures(
        prefix_request,
        source_a,
        source_b,
        destination_a,
        destination_b,
    )
    (
        prefix_proofs,
        source_proof_bindings,
        destination_proof_bindings,
        proof_capture_ids,
    ) = _validate_required_prefix_proofs(
        prefix_request,
        source_a,
        source_b,
        destination_a,
        destination_b,
        source_prefix_a=source_prefix_a,
        source_prefix_b=source_prefix_b,
        destination_prefix_a=destination_prefix_a,
        destination_prefix_b=destination_prefix_b,
    )
    if (
        close_request["prefix_request_sha256"] != prefix_request["request_sha256"]
        or close_request["source"]
        != _close_custody_binding(source_a, source_b, role="source")
        or close_request["destination"]
        != _close_custody_binding(
            destination_a,
            destination_b,
            role="destination",
        )
        or close_request["source_prefix_proofs"] != source_proof_bindings
        or close_request["destination_prefix_proofs"] != destination_proof_bindings
    ):
        raise BulkloadError(
            "Codex session close request does not bind the supplied prefix evidence"
        )
    return (
        prefix_proofs,
        source_proof_bindings,
        destination_proof_bindings,
        proof_capture_ids,
    )


def capture_codex_session_close_capture(
    root: Path,
    *,
    role: str,
    close_request: dict[str, Any],
    acknowledge_writers_quiesced: bool,
) -> dict[str, Any]:
    """Capture a fresh v2 snapshot chained after a validated close request."""
    validate_codex_session_close_request(close_request)
    if role not in {"source", "destination"}:
        raise BulkloadError("Codex session close capture role is invalid")
    custody = close_request[role]
    root = root.expanduser()
    root_text = os.fspath(root)
    if (
        not root.is_absolute()
        or os.path.normpath(root_text) != root_text
        or root_text != custody["root"]
    ):
        raise BulkloadError("Codex session close capture root does not match custody")
    budgets = custody["budgets"]
    snapshot = capture_codex_sessions(
        root,
        role=role,
        acknowledge_writers_quiesced=acknowledge_writers_quiesced,
        host_authority_id=custody["host_authority_id"],
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
    validate_codex_session_snapshot(snapshot)
    if (
        not snapshot["complete"]
        or snapshot["role"] != role
        or snapshot["host"] != custody["host"]
        or snapshot["host_authority_id"] != custody["host_authority_id"]
        or snapshot["root"] != custody["root"]
        or snapshot["resolved_root"] != custody["resolved_root"]
        or snapshot["root_identity"] != custody["root_identity"]
        or snapshot["root_lineage"] != custody["root_lineage"]
        or snapshot["budgets"] != custody["budgets"]
        or snapshot["catalog_sha256"] != custody["catalog_sha256"]
    ):
        raise BulkloadError(
            f"Codex session {role} close capture differs from requested custody"
        )
    close_capture: dict[str, Any] = {
        "schema": CODEX_SESSION_CLOSE_CAPTURE_SCHEMA,
        "created_at": utc_now(),
        "close_request_sha256": close_request["close_request_sha256"],
        "snapshot": snapshot,
    }
    close_capture["close_capture_sha256"] = object_digest(
        close_capture,
        "close_capture_sha256",
    )
    if len(canonical_bytes(close_capture)) > MAX_CODEX_SESSION_PLAN_BYTES:
        raise BulkloadError("Codex session close capture output byte budget exceeded")
    return close_capture


def validate_codex_session_close_capture(
    close_capture: dict[str, Any],
) -> None:
    if set(close_capture) != {
        "schema",
        "created_at",
        "close_request_sha256",
        "snapshot",
        "close_capture_sha256",
    }:
        raise BulkloadError("Codex session close capture has unexpected fields")
    if close_capture.get("schema") != CODEX_SESSION_CLOSE_CAPTURE_SCHEMA:
        raise BulkloadError("unsupported Codex session close capture schema")
    if (
        not isinstance(close_capture.get("created_at"), str)
        or not close_capture["created_at"]
        or not isinstance(close_capture.get("close_request_sha256"), str)
        or re.fullmatch(
            r"[0-9a-f]{64}",
            close_capture["close_request_sha256"],
        )
        is None
        or not isinstance(close_capture.get("snapshot"), dict)
    ):
        raise BulkloadError("Codex session close capture envelope is invalid")
    validate_codex_session_snapshot(close_capture["snapshot"])
    if len(canonical_bytes(close_capture)) > MAX_CODEX_SESSION_PLAN_BYTES:
        raise BulkloadError("Codex session close capture output byte budget exceeded")
    require_digest(close_capture, "close_capture_sha256")


def _validate_close_capture_against_request(
    close_capture: dict[str, Any],
    close_request: dict[str, Any],
    *,
    role: str,
) -> dict[str, Any]:
    validate_codex_session_close_capture(close_capture)
    snapshot = close_capture["snapshot"]
    custody = close_request[role]
    if (
        close_capture["close_request_sha256"] != close_request["close_request_sha256"]
        or snapshot["role"] != role
        or snapshot["host"] != custody["host"]
        or snapshot["host_authority_id"] != custody["host_authority_id"]
        or snapshot["root"] != custody["root"]
        or snapshot["resolved_root"] != custody["resolved_root"]
        or snapshot["root_identity"] != custody["root_identity"]
        or snapshot["root_lineage"] != custody["root_lineage"]
        or snapshot["budgets"] != custody["budgets"]
        or snapshot["catalog_sha256"] != custody["catalog_sha256"]
    ):
        raise BulkloadError(f"Codex session {role} close capture binding is invalid")
    return snapshot


def _close_capture_evidence_binding(
    close_capture: dict[str, Any],
) -> dict[str, str]:
    snapshot = close_capture["snapshot"]
    return {
        "capture_id": snapshot["capture_id"],
        "snapshot_sha256": snapshot["snapshot_sha256"],
        "close_capture_sha256": close_capture["close_capture_sha256"],
    }


def _classify_codex_session_union(
    source_sessions: list[dict[str, Any]],
    destination_sessions: list[dict[str, Any]],
    source_directories: list[dict[str, Any]],
    destination_directories: list[dict[str, Any]],
    *,
    prefix_proofs: dict[str, dict[str, Any]] | None = None,
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
    promote_source_superset: list[dict[str, Any]] = []
    preserve_destination_superset: list[dict[str, Any]] = []
    blockers: list[dict[str, Any]] = []
    prefix_proofs = prefix_proofs or {}

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
        elif source["size"] != target["size"]:
            if source["size"] > target["size"]:
                longer_role, longer = "source", source
                shorter = target
            else:
                longer_role, longer = "destination", target
                shorter = source
            proof = prefix_proofs.get(session_id)
            blocker = {
                "session_id": session_id,
                "source_relative_path": source["relative_path"],
                "destination_relative_path": target["relative_path"],
                "source_sha256": source["sha256"],
                "destination_sha256": target["sha256"],
                "source_size": source["size"],
                "destination_size": target["size"],
            }
            if proof is None:
                blockers.append(
                    {
                        "code": "same-uuid-prefix-proof-required",
                        **blocker,
                    }
                )
            elif (
                proof["prefix_sha256"] != shorter["sha256"]
                or proof["prefix_size"] != shorter["size"]
                or proof["sha256"] != longer["sha256"]
                or proof["size"] != longer["size"]
                or proof["prefix_ends_at_record"] is not True
            ):
                blockers.append(
                    {
                        "code": "same-uuid-divergent-bytes",
                        **blocker,
                    }
                )
            elif longer_role == "source":
                promote_source_superset.append(
                    {
                        "action": "replace-with-proven-source-superset",
                        "session_id": session_id,
                        "source_relative_path": longer["relative_path"],
                        "destination_relative_path": shorter["relative_path"],
                        "source_sha256": longer["sha256"],
                        "source_size": longer["size"],
                        "destination_before_sha256": shorter["sha256"],
                        "destination_before_size": shorter["size"],
                        "destination_mode": "0600",
                    }
                )
            else:
                preserve_destination_superset.append(
                    {
                        "session_id": session_id,
                        "source_relative_path": shorter["relative_path"],
                        "destination_relative_path": longer["relative_path"],
                        "source_sha256": shorter["sha256"],
                        "source_size": shorter["size"],
                        "destination_sha256": longer["sha256"],
                        "destination_size": longer["size"],
                        "reason": "destination-is-proven-superset",
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
        promote_source_superset = []
    return {
        "copy_if_absent": copy_if_absent,
        "exact_common": exact_common,
        "preserve_destination": preserve_destination,
        "promote_source_superset": promote_source_superset,
        "preserve_destination_superset": preserve_destination_superset,
        "blockers": blockers,
    }


def compile_codex_session_union_plan(
    source_a: dict[str, Any],
    source_b: dict[str, Any],
    destination_a: dict[str, Any],
    destination_b: dict[str, Any],
    *,
    prefix_request: dict[str, Any] | None = None,
    source_prefix_a: dict[str, Any] | None = None,
    source_prefix_b: dict[str, Any] | None = None,
    destination_prefix_a: dict[str, Any] | None = None,
    destination_prefix_b: dict[str, Any] | None = None,
    close_request: dict[str, Any] | None = None,
    source_close_a: dict[str, Any] | None = None,
    source_close_b: dict[str, Any] | None = None,
    destination_close_a: dict[str, Any] | None = None,
    destination_close_b: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Compile a read-only UUID union report with optional prefix evidence."""
    _validate_union_capture_set(
        source_a,
        source_b,
        destination_a,
        destination_b,
    )
    proof_arguments = (
        source_prefix_a,
        source_prefix_b,
        destination_prefix_a,
        destination_prefix_b,
    )
    close_arguments = (
        source_close_a,
        source_close_b,
        destination_close_a,
        destination_close_b,
    )
    if prefix_request is None and any(
        item is not None for item in (close_request, *proof_arguments, *close_arguments)
    ):
        raise BulkloadError("Codex session prefix evidence requires a prefix request")
    prefix_proofs: dict[str, dict[str, Any]] = {}
    source_proof_bindings: list[dict[str, str]] = []
    destination_proof_bindings: list[dict[str, str]] = []
    source_close_bindings: list[dict[str, str]] = []
    destination_close_bindings: list[dict[str, str]] = []
    all_capture_ids = [
        source_a["capture_id"],
        source_b["capture_id"],
        destination_a["capture_id"],
        destination_b["capture_id"],
    ]
    if prefix_request is not None:
        _validate_prefix_request_against_captures(
            prefix_request,
            source_a,
            source_b,
            destination_a,
            destination_b,
        )
        has_observations = bool(prefix_request["requests"])
        if has_observations and (
            close_request is None or any(item is None for item in close_arguments)
        ):
            raise BulkloadError(
                "Codex session prefix planning requires a close request and "
                "source and destination close pass A and pass B"
            )
        if not has_observations and any(
            item is not None
            for item in (close_request, *proof_arguments, *close_arguments)
        ):
            raise BulkloadError(
                "Codex session prefix evidence is unexpected without observations"
            )
        if has_observations:
            (
                prefix_proofs,
                source_proof_bindings,
                destination_proof_bindings,
                proof_capture_ids,
            ) = _validate_close_request_against_inputs(
                close_request,
                prefix_request,
                source_a,
                source_b,
                destination_a,
                destination_b,
                source_prefix_a=source_prefix_a,
                source_prefix_b=source_prefix_b,
                destination_prefix_a=destination_prefix_a,
                destination_prefix_b=destination_prefix_b,
            )
            source_close_snapshot_a = _validate_close_capture_against_request(
                source_close_a,
                close_request,
                role="source",
            )
            source_close_snapshot_b = _validate_close_capture_against_request(
                source_close_b,
                close_request,
                role="source",
            )
            destination_close_snapshot_a = _validate_close_capture_against_request(
                destination_close_a,
                close_request,
                role="destination",
            )
            destination_close_snapshot_b = _validate_close_capture_against_request(
                destination_close_b,
                close_request,
                role="destination",
            )
            _validate_closing_capture_pair(
                source_a,
                source_b,
                source_close_snapshot_a,
                source_close_snapshot_b,
                role="source",
            )
            _validate_closing_capture_pair(
                destination_a,
                destination_b,
                destination_close_snapshot_a,
                destination_close_snapshot_b,
                role="destination",
            )
            source_close_bindings = [
                _close_capture_evidence_binding(source_close_a),
                _close_capture_evidence_binding(source_close_b),
            ]
            destination_close_bindings = [
                _close_capture_evidence_binding(destination_close_a),
                _close_capture_evidence_binding(destination_close_b),
            ]
            all_capture_ids.extend(
                [
                    *proof_capture_ids,
                    source_close_snapshot_a["capture_id"],
                    source_close_snapshot_b["capture_id"],
                    destination_close_snapshot_a["capture_id"],
                    destination_close_snapshot_b["capture_id"],
                ]
            )
    if len(set(all_capture_ids)) != len(all_capture_ids):
        raise BulkloadError(
            "Codex session opening, proof, and close captures must be globally distinct"
        )
    classified = _classify_codex_session_union(
        source_a["sessions"],
        destination_a["sessions"],
        source_a["directories"],
        destination_a["directories"],
        prefix_proofs=prefix_proofs,
    )

    intent = {
        "ready_for_attended_copy": not classified["blockers"],
        "copy_if_absent": classified["copy_if_absent"],
        "promote_source_superset": classified["promote_source_superset"],
        "exact_common": classified["exact_common"],
        "preserve_destination": classified["preserve_destination"],
        "preserve_destination_superset": classified["preserve_destination_superset"],
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
        "source": _capture_pair_binding(source_a, source_b),
        "destination": _capture_pair_binding(destination_a, destination_b),
        "prefix_evidence": {
            "request_sha256": (
                prefix_request["request_sha256"] if prefix_request is not None else None
            ),
            "close_request_sha256": (
                close_request["close_request_sha256"]
                if close_request is not None
                else None
            ),
            "source_prefix_proofs": source_proof_bindings,
            "destination_prefix_proofs": destination_proof_bindings,
            "source_close_snapshots": source_close_bindings,
            "destination_close_snapshots": destination_close_bindings,
        },
        "intent": intent,
    }
    plan["plan_sha256"] = object_digest(plan, "plan_sha256")
    if len(canonical_bytes(plan)) > MAX_CODEX_SESSION_PLAN_BYTES:
        raise BulkloadError("Codex session plan output byte budget exceeded")
    return plan
