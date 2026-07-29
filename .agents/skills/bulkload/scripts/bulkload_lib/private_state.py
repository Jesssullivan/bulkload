"""Private, typed Codex auth and SQLite capture and compatibility planning."""

from __future__ import annotations

import ctypes
import errno
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import sqlite3
import stat
import sys
import time
from typing import Any
from urllib.parse import quote
import uuid

from .model import (
    BulkloadError,
    canonical_bytes,
    object_digest,
    sha256_bytes,
    utc_now,
)

PRIVATE_CAPTURE_SCHEMA = "dev.tinyland.bulkload.codex-private-state-capture.v1"
PRIVATE_PLAN_SCHEMA = "dev.tinyland.bulkload.codex-private-state-plan.v1"
PRIVATE_MANIFEST = "private-state-manifest.json"
AUTH_BASENAME = "auth.json"
SQLITE_DIRECTORY = "sqlite"
MAX_AUTH_BYTES = 1024 * 1024
MAX_PRIVATE_MANIFEST_BYTES = 8 * 1024 * 1024
MAX_PRIVATE_PLAN_BYTES = 8 * 1024 * 1024
MAX_PRIVATE_ROOT_LINEAGE = 256
DEFAULT_MAX_SQLITE_FAMILIES = 32
DEFAULT_MAX_TOTAL_SQLITE_BYTES = 16 * 1024 * 1024 * 1024
DEFAULT_BACKUP_TIMEOUT_SECONDS = 60 * 60
DEFAULT_MAX_THREAD_ENTRIES = 1_000_000
DEFAULT_MAX_THREAD_INDEX_BYTES = 512 * 1024 * 1024
DEFAULT_MAX_METADATA_ENTRIES = 100_000
DEFAULT_MAX_METADATA_BYTES = 256 * 1024 * 1024
SQLITE_BASENAME = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]*\.sqlite$")
COUNTED_TABLES = frozenset(
    {
        "agent_job_items",
        "agent_jobs",
        "jobs",
        "stage1_outputs",
        "thread_dynamic_tools",
        "thread_goal_continuation_deferrals",
        "thread_goals",
        "thread_spawn_edges",
        "threads",
    }
)


def _identity(info: os.stat_result) -> dict[str, int]:
    return {
        "device": info.st_dev,
        "inode": info.st_ino,
        "uid": info.st_uid,
        "mode": stat.S_IMODE(info.st_mode),
        "links": info.st_nlink,
    }


def _stable_identity(info: os.stat_result) -> tuple[int, int, int, int, int]:
    return (
        info.st_dev,
        info.st_ino,
        info.st_uid,
        stat.S_IMODE(info.st_mode),
        info.st_nlink,
    )


def _stable_artifact_stat(info: os.stat_result) -> tuple[int, ...]:
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


def _directory_identity(info: os.stat_result) -> tuple[int, int, int, int]:
    return (
        info.st_dev,
        info.st_ino,
        info.st_uid,
        stat.S_IMODE(info.st_mode),
    )


def _require_uuid(value: str, label: str) -> str:
    try:
        parsed = uuid.UUID(value)
    except (AttributeError, ValueError) as error:
        raise BulkloadError(f"{label} must be a canonical UUID") from error
    if str(parsed) != value:
        raise BulkloadError(f"{label} must be a canonical UUID")
    return value


def _resolve_private_directory(path: Path, label: str) -> tuple[Path, os.stat_result]:
    try:
        resolved = path.expanduser().resolve(strict=True)
        info = resolved.stat()
    except (OSError, RuntimeError, ValueError) as error:
        raise BulkloadError(f"cannot resolve {label}") from error
    if (
        not stat.S_ISDIR(info.st_mode)
        or info.st_uid != os.getuid()
        or stat.S_IMODE(info.st_mode) & 0o077
    ):
        raise BulkloadError(f"{label} must be an owner-private directory")
    return resolved, info


def _fsync_directory_descriptor(descriptor: int) -> None:
    os.fsync(descriptor)


def _open_private_directory_descriptor(
    path: Path,
    expected: os.stat_result,
    label: str,
) -> int:
    flags = (
        os.O_RDONLY
        | getattr(os, "O_DIRECTORY", 0)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_CLOEXEC", 0)
    )
    descriptor = os.open(path, flags)
    opened = os.fstat(descriptor)
    if _directory_identity(opened) != _directory_identity(expected):
        os.close(descriptor)
        raise BulkloadError(f"{label} changed while opening")
    return descriptor


def _revalidate_directory_binding(
    path: Path,
    descriptor: int,
    expected: os.stat_result,
    label: str,
) -> None:
    try:
        opened = os.fstat(descriptor)
        entry = path.stat(follow_symlinks=False)
    except OSError as error:
        raise BulkloadError(f"cannot revalidate {label}") from error
    expected_identity = _directory_identity(expected)
    if (
        _directory_identity(opened) != expected_identity
        or _directory_identity(entry) != expected_identity
    ):
        raise BulkloadError(f"{label} changed")


def _directory_identity_lineage(descriptor: int) -> set[tuple[int, int]]:
    current = os.dup(descriptor)
    lineage: set[tuple[int, int]] = set()
    try:
        for _ in range(MAX_PRIVATE_ROOT_LINEAGE):
            current_info = os.fstat(current)
            current_identity = (current_info.st_dev, current_info.st_ino)
            if current_identity in lineage:
                raise BulkloadError("private directory lineage contains a cycle")
            lineage.add(current_identity)
            parent = os.open(
                "..",
                os.O_RDONLY
                | getattr(os, "O_DIRECTORY", 0)
                | getattr(os, "O_NOFOLLOW", 0)
                | getattr(os, "O_CLOEXEC", 0),
                dir_fd=current,
            )
            parent_info = os.fstat(parent)
            if (parent_info.st_dev, parent_info.st_ino) == current_identity:
                os.close(parent)
                return lineage
            os.close(current)
            current = parent
    finally:
        os.close(current)
    raise BulkloadError("private directory lineage is unbounded")


def _hash_regular_file(path: Path) -> tuple[str, int]:
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0)
    descriptor = os.open(path, flags)
    try:
        before = os.fstat(descriptor)
        if (
            not stat.S_ISREG(before.st_mode)
            or before.st_uid != os.getuid()
            or before.st_nlink != 1
        ):
            raise BulkloadError(f"private artifact custody is invalid: {path.name}")
        digest = hashlib.sha256()
        size = 0
        while True:
            payload = os.read(descriptor, 1024 * 1024)
            if not payload:
                break
            digest.update(payload)
            size += len(payload)
        after = os.fstat(descriptor)
        entry = path.stat(follow_symlinks=False)
        if (
            _stable_artifact_stat(after) != _stable_artifact_stat(before)
            or _stable_artifact_stat(entry) != _stable_artifact_stat(before)
            or size != before.st_size
        ):
            raise BulkloadError(f"private artifact changed while hashing: {path.name}")
        return digest.hexdigest(), size
    finally:
        os.close(descriptor)


def _hash_regular_file_at(
    parent_descriptor: int,
    name: str,
    label: str,
) -> tuple[str, int]:
    descriptor = os.open(
        name,
        os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0),
        dir_fd=parent_descriptor,
    )
    try:
        before = os.fstat(descriptor)
        if (
            not stat.S_ISREG(before.st_mode)
            or before.st_uid != os.getuid()
            or before.st_nlink != 1
        ):
            raise BulkloadError(f"{label} custody is invalid")
        digest = hashlib.sha256()
        size = 0
        while True:
            payload = os.read(descriptor, 1024 * 1024)
            if not payload:
                break
            digest.update(payload)
            size += len(payload)
        after = os.fstat(descriptor)
        entry = os.stat(name, dir_fd=parent_descriptor, follow_symlinks=False)
        if (
            _stable_artifact_stat(after) != _stable_artifact_stat(before)
            or _stable_artifact_stat(entry) != _stable_artifact_stat(before)
            or size != before.st_size
        ):
            raise BulkloadError(f"{label} changed while hashing")
        return digest.hexdigest(), size
    finally:
        os.close(descriptor)


def _strict_json_object(payload: bytes, label: str) -> dict[str, Any]:
    def unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        value: dict[str, Any] = {}
        for key, item in pairs:
            if key in value:
                raise ValueError(f"duplicate JSON key {key!r} is forbidden")
            value[key] = item
        return value

    def reject_nonfinite(value: str) -> None:
        raise ValueError(f"non-finite JSON number {value} is forbidden")

    try:
        value = json.loads(
            payload,
            object_pairs_hook=unique_object,
            parse_constant=reject_nonfinite,
        )
    except (
        UnicodeDecodeError,
        json.JSONDecodeError,
        OverflowError,
        RecursionError,
        ValueError,
    ) as error:
        raise BulkloadError(f"{label} must contain one strict JSON object") from error
    if not isinstance(value, dict):
        raise BulkloadError(f"{label} must contain one strict JSON object")
    return value


def _read_private_json_file_at(
    parent_descriptor: int,
    name: str,
    label: str,
    *,
    max_bytes: int,
) -> dict[str, Any]:
    descriptor = os.open(
        name,
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
            or before.st_size < 1
            or before.st_size > max_bytes
        ):
            raise BulkloadError(f"{label} custody is invalid")
        payload = b""
        while len(payload) <= max_bytes:
            block = os.read(descriptor, min(65536, max_bytes + 1 - len(payload)))
            if not block:
                break
            payload += block
        after = os.fstat(descriptor)
        entry = os.stat(name, dir_fd=parent_descriptor, follow_symlinks=False)
        if (
            len(payload) != before.st_size
            or _stable_artifact_stat(after) != _stable_artifact_stat(before)
            or _stable_artifact_stat(entry) != _stable_artifact_stat(before)
        ):
            raise BulkloadError(f"{label} changed while reading")
        return _strict_json_object(payload, label)
    finally:
        os.close(descriptor)


def _write_exclusive(path: Path, payload: bytes, mode: int = 0o600) -> None:
    flags = (
        os.O_WRONLY
        | os.O_CREAT
        | os.O_EXCL
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_CLOEXEC", 0)
    )
    descriptor = os.open(path, flags, mode)
    try:
        os.fchmod(descriptor, mode)
        view = memoryview(payload)
        while view:
            written = os.write(descriptor, view)
            if written <= 0:
                raise BulkloadError("private artifact write made no progress")
            view = view[written:]
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def _write_exclusive_at(
    parent_descriptor: int,
    name: str,
    payload: bytes,
    mode: int = 0o600,
) -> None:
    flags = (
        os.O_WRONLY
        | os.O_CREAT
        | os.O_EXCL
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_CLOEXEC", 0)
    )
    descriptor = os.open(name, flags, mode, dir_fd=parent_descriptor)
    try:
        os.fchmod(descriptor, mode)
        view = memoryview(payload)
        while view:
            written = os.write(descriptor, view)
            if written <= 0:
                raise BulkloadError("private artifact write made no progress")
            view = view[written:]
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def _verify_private_payload_at(
    parent_descriptor: int,
    name: str,
    expected_payload: bytes,
) -> None:
    descriptor = os.open(
        name,
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
            or before.st_size != len(expected_payload)
        ):
            raise BulkloadError("published private evidence custody is invalid")
        payload = b""
        while len(payload) <= len(expected_payload):
            block = os.read(
                descriptor,
                min(65536, len(expected_payload) + 1 - len(payload)),
            )
            if not block:
                break
            payload += block
        after = os.fstat(descriptor)
        entry = os.stat(name, dir_fd=parent_descriptor, follow_symlinks=False)
        if (
            payload != expected_payload
            or _stable_artifact_stat(after) != _stable_artifact_stat(before)
            or _stable_artifact_stat(entry) != _stable_artifact_stat(before)
        ):
            raise BulkloadError("published private evidence verification failed")
    finally:
        os.close(descriptor)


def _rename_noreplace_at(
    source_descriptor: int,
    source_name: str,
    destination_descriptor: int,
    destination_name: str,
) -> None:
    libc = ctypes.CDLL(None, use_errno=True)
    source = os.fsencode(source_name)
    destination = os.fsencode(destination_name)
    if sys.platform == "darwin":
        rename = libc.renameatx_np
        rename.argtypes = (
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_uint,
        )
        rename.restype = ctypes.c_int
        result = rename(
            source_descriptor,
            source,
            destination_descriptor,
            destination,
            0x00000004,
        )
    elif sys.platform.startswith("linux"):
        try:
            rename = libc.renameat2
        except AttributeError as error:
            raise BulkloadError(
                "atomic no-replace private publication is unavailable"
            ) from error
        rename.argtypes = (
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_uint,
        )
        rename.restype = ctypes.c_int
        result = rename(
            source_descriptor,
            source,
            destination_descriptor,
            destination,
            1,
        )
    else:
        raise BulkloadError(f"private publication is unsupported on {sys.platform}")
    if result == 0:
        return
    error_number = ctypes.get_errno()
    if error_number == errno.EEXIST:
        raise BulkloadError("private output already exists")
    raise OSError(error_number, os.strerror(error_number), destination_name)


def _copy_auth(
    source_parent_descriptor: int,
    source_name: str,
    destination_parent_descriptor: int,
    destination_name: str,
) -> dict[str, Any]:
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0)
    descriptor = os.open(source_name, flags, dir_fd=source_parent_descriptor)
    try:
        before = os.fstat(descriptor)
        if (
            not stat.S_ISREG(before.st_mode)
            or before.st_uid != os.getuid()
            or stat.S_IMODE(before.st_mode) != 0o600
            or before.st_nlink != 1
            or before.st_size < 1
            or before.st_size > MAX_AUTH_BYTES
        ):
            raise BulkloadError(
                "auth.json must be a bounded 0600 current-user single-link file"
            )
        payload = b""
        while len(payload) <= MAX_AUTH_BYTES:
            block = os.read(descriptor, min(65536, MAX_AUTH_BYTES + 1 - len(payload)))
            if not block:
                break
            payload += block
        if len(payload) != before.st_size:
            raise BulkloadError("auth.json changed while reading")
        after = os.fstat(descriptor)
        entry = os.stat(
            source_name,
            dir_fd=source_parent_descriptor,
            follow_symlinks=False,
        )
        if _stable_artifact_stat(after) != _stable_artifact_stat(
            before
        ) or _stable_artifact_stat(entry) != _stable_artifact_stat(before):
            raise BulkloadError("auth.json changed while reading")
        _strict_json_object(payload, AUTH_BASENAME)
        _write_exclusive_at(
            destination_parent_descriptor,
            destination_name,
            payload,
        )
        return {
            "basename": AUTH_BASENAME,
            "snapshot_path": AUTH_BASENAME,
            "sha256": sha256_bytes(payload),
            "size": len(payload),
            "source_identity": _identity(before),
        }
    finally:
        os.close(descriptor)


def _discover_sqlite(
    root: Path | int,
    *,
    max_families: int,
    max_total_bytes: int,
) -> list[tuple[str, os.stat_result]]:
    if max_families < 1:
        raise BulkloadError("max SQLite families must be positive")
    if max_total_bytes < 1:
        raise BulkloadError("max total SQLite bytes must be positive")
    families: list[tuple[str, os.stat_result]] = []
    total = 0
    with os.scandir(root) as entries:
        for entry in entries:
            if not SQLITE_BASENAME.fullmatch(entry.name):
                continue
            info = entry.stat(follow_symlinks=False)
            if (
                entry.is_symlink()
                or not stat.S_ISREG(info.st_mode)
                or info.st_uid != os.getuid()
                or info.st_nlink != 1
                or stat.S_IMODE(info.st_mode) & 0o022
            ):
                raise BulkloadError(f"SQLite family has unsafe custody: {entry.name}")
            total += info.st_size
            families.append((entry.name, info))
    families.sort(key=lambda item: item[0])
    if not families:
        raise BulkloadError("no provider-owned SQLite families were discovered")
    if len(families) > max_families:
        raise BulkloadError("SQLite family count exceeds the configured budget")
    if total > max_total_bytes:
        raise BulkloadError("SQLite source bytes exceed the configured budget")
    return families


def _sqlite_uri(path: Path) -> str:
    return f"file:{quote(os.fspath(path), safe='/')}?mode=ro"


def _schema_metadata(
    connection: sqlite3.Connection,
    *,
    max_thread_entries: int,
    max_thread_index_bytes: int,
    max_metadata_entries: int,
    max_metadata_bytes: int,
    deadline: float,
) -> dict[str, Any]:
    metadata_entries = 0
    metadata_bytes = 0

    def charge(value: Any) -> None:
        nonlocal metadata_entries, metadata_bytes
        if time.monotonic() > deadline:
            raise BulkloadError("SQLite metadata exceeded its time budget")
        metadata_entries += 1
        metadata_bytes += len(canonical_bytes(value))
        if (
            metadata_entries > max_metadata_entries
            or metadata_bytes > max_metadata_bytes
        ):
            raise BulkloadError("SQLite metadata exceeds its capture budget")

    schema: list[dict[str, Any]] = []
    for row in connection.execute(
        """
        SELECT type, name, tbl_name, sql
        FROM sqlite_schema
        WHERE name NOT LIKE 'sqlite_%'
        ORDER BY type, name
        """
    ):
        item = {
            "type": row[0],
            "name": row[1],
            "table": row[2],
            "sql": row[3] or "",
        }
        charge(item)
        schema.append(item)
    tables: list[dict[str, Any]] = []
    table_names = sorted(item["name"] for item in schema if item["type"] == "table")
    for name in table_names:
        escaped = name.replace('"', '""')
        columns: list[dict[str, Any]] = []
        for row in connection.execute(f'PRAGMA table_info("{escaped}")'):
            column = {
                "position": row[0],
                "name": row[1],
                "type": row[2],
                "not_null": bool(row[3]),
                "default": row[4],
                "primary_key_position": row[5],
            }
            charge(column)
            columns.append(column)
        entry: dict[str, Any] = {
            "name": name,
            "columns_sha256": sha256_bytes(canonical_bytes(columns)),
        }
        if name in COUNTED_TABLES:
            entry["row_count"] = int(
                connection.execute(f'SELECT count(*) FROM "{escaped}"').fetchone()[0]
            )
        charge(entry)
        tables.append(entry)

    migrations: list[list[Any]] = []
    if "_sqlx_migrations" in table_names:
        for row in connection.execute(
            """
            SELECT version, description, hex(checksum), success
            FROM _sqlx_migrations
            ORDER BY version
            """
        ):
            migration = [int(row[0]), str(row[1]), str(row[2]), bool(row[3])]
            charge(migration)
            migrations.append(migration)

    thread_count = 0
    thread_index_bytes = 0
    thread_ids_digest = hashlib.sha256()
    thread_paths_digest = hashlib.sha256()
    thread_ids_digest.update(b"[")
    thread_paths_digest.update(b"[")
    if "threads" in table_names:
        first = True
        for row in connection.execute(
            "SELECT id, rollout_path FROM threads ORDER BY id"
        ):
            if time.monotonic() > deadline:
                raise BulkloadError("SQLite metadata exceeded its time budget")
            thread_id = str(row[0])
            rollout_path = str(row[1])
            thread_count += 1
            thread_index_bytes += len(thread_id.encode("utf-8")) + len(
                rollout_path.encode("utf-8")
            )
            if (
                thread_count > max_thread_entries
                or thread_index_bytes > max_thread_index_bytes
            ):
                raise BulkloadError("SQLite thread index exceeds its capture budget")
            if not first:
                thread_ids_digest.update(b",")
                thread_paths_digest.update(b",")
            thread_ids_digest.update(canonical_bytes(thread_id))
            thread_paths_digest.update(canonical_bytes([thread_id, rollout_path]))
            first = False
    thread_ids_digest.update(b"]")
    thread_paths_digest.update(b"]")

    return {
        "schema_sha256": sha256_bytes(canonical_bytes(schema)),
        "tables": tables,
        "migration_count": len(migrations),
        "latest_migration": migrations[-1][0] if migrations else None,
        "migrations_sha256": sha256_bytes(canonical_bytes(migrations)),
        "thread_count": thread_count,
        "thread_index_bytes": thread_index_bytes,
        "metadata_entries": metadata_entries,
        "metadata_bytes": metadata_bytes,
        "thread_ids_sha256": thread_ids_digest.hexdigest(),
        "thread_paths_sha256": thread_paths_digest.hexdigest(),
    }


def _online_backup(
    source: Path,
    destination: Path,
    expected: os.stat_result,
    *,
    timeout_seconds: int,
    max_snapshot_bytes: int,
    max_thread_entries: int,
    max_thread_index_bytes: int,
    max_metadata_entries: int,
    max_metadata_bytes: int,
) -> dict[str, Any]:
    if timeout_seconds < 1:
        raise BulkloadError("SQLite backup timeout must be positive")
    if max_snapshot_bytes < 1:
        raise BulkloadError("SQLite snapshot byte budget is exhausted")
    _write_exclusive(destination, b"")
    source_connection: sqlite3.Connection | None = None
    destination_connection: sqlite3.Connection | None = None
    started = time.monotonic()

    def progress(_: int, __: int, ___: int) -> None:
        if time.monotonic() - started > timeout_seconds:
            raise BulkloadError("SQLite online backup exceeded its time budget")

    try:
        source_connection = sqlite3.connect(
            _sqlite_uri(source),
            uri=True,
            timeout=min(timeout_seconds, 60),
        )
        source_connection.execute("PRAGMA query_only=ON")
        page_count = int(source_connection.execute("PRAGMA page_count").fetchone()[0])
        page_size = int(source_connection.execute("PRAGMA page_size").fetchone()[0])
        logical_bytes = page_count * page_size
        if logical_bytes > max_snapshot_bytes:
            raise BulkloadError(
                f"SQLite snapshot exceeds the configured byte budget: {source.name}"
            )
        destination_connection = sqlite3.connect(destination)
        source_connection.backup(
            destination_connection,
            pages=1024,
            progress=progress,
            sleep=0.05,
        )
        destination_connection.commit()
        destination_connection.set_progress_handler(
            lambda: int(time.monotonic() - started > timeout_seconds),
            10_000,
        )
        journal_mode = str(
            destination_connection.execute("PRAGMA journal_mode=DELETE").fetchone()[0]
        ).lower()
        if journal_mode != "delete":
            raise BulkloadError(
                f"SQLite backup journal normalization failed: {source.name}"
            )
        quick = [
            str(row[0])
            for row in destination_connection.execute("PRAGMA quick_check(1)")
        ]
        if quick != ["ok"]:
            raise BulkloadError(f"SQLite backup quick_check failed: {source.name}")
        metadata = _schema_metadata(
            destination_connection,
            max_thread_entries=max_thread_entries,
            max_thread_index_bytes=max_thread_index_bytes,
            max_metadata_entries=max_metadata_entries,
            max_metadata_bytes=max_metadata_bytes,
            deadline=started + timeout_seconds,
        )
        metadata["user_version"] = int(
            destination_connection.execute("PRAGMA user_version").fetchone()[0]
        )
        metadata["application_id"] = int(
            destination_connection.execute("PRAGMA application_id").fetchone()[0]
        )
        metadata["logical_source_bytes"] = logical_bytes
        metadata["snapshot_journal_mode"] = journal_mode
        metadata["quick_check"] = "ok"
    except sqlite3.Error as error:
        raise BulkloadError(f"SQLite online backup failed: {source.name}") from error
    finally:
        if destination_connection is not None:
            destination_connection.close()
        if source_connection is not None:
            source_connection.close()

    current = source.stat(follow_symlinks=False)
    if _stable_identity(current) != _stable_identity(expected):
        raise BulkloadError(f"SQLite source identity changed: {source.name}")
    destination.chmod(0o600)
    descriptor = os.open(destination, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    digest, size = _hash_regular_file(destination)
    if size > max_snapshot_bytes:
        raise BulkloadError(
            f"SQLite snapshot exceeds the configured byte budget: {source.name}"
        )
    metadata.update(
        {
            "basename": source.name,
            "snapshot_path": f"{SQLITE_DIRECTORY}/{source.name}",
            "sha256": digest,
            "snapshot_size": size,
            "source_size": expected.st_size,
            "source_identity": _identity(expected),
            "copy_method": "sqlite-online-backup-api",
        }
    )
    return metadata


def _path_is_within(candidate: Path, root: Path) -> bool:
    try:
        candidate.relative_to(root)
        return True
    except ValueError:
        return False


def capture_codex_private_state(
    codex_home: Path,
    output_directory: Path,
    *,
    role: str,
    host_authority_id: str,
    codex_version: str,
    sqlite_home: Path | None = None,
    include_auth: bool,
    include_sqlite: bool,
    acknowledge_private_capture: bool,
    max_sqlite_families: int = DEFAULT_MAX_SQLITE_FAMILIES,
    max_total_sqlite_bytes: int = DEFAULT_MAX_TOTAL_SQLITE_BYTES,
    backup_timeout_seconds: int = DEFAULT_BACKUP_TIMEOUT_SECONDS,
    max_thread_entries: int = DEFAULT_MAX_THREAD_ENTRIES,
    max_thread_index_bytes: int = DEFAULT_MAX_THREAD_INDEX_BYTES,
    max_metadata_entries: int = DEFAULT_MAX_METADATA_ENTRIES,
    max_metadata_bytes: int = DEFAULT_MAX_METADATA_BYTES,
) -> dict[str, Any]:
    """Publish one private capture directory without installing live state."""
    if role not in {"source", "destination"}:
        raise BulkloadError("private capture role must be source or destination")
    _require_uuid(host_authority_id, "host authority ID")
    if not codex_version or len(codex_version.encode("utf-8")) > 128:
        raise BulkloadError("Codex version must be a bounded non-empty string")
    if not include_auth and not include_sqlite:
        raise BulkloadError("private capture requires an explicit state class")
    if not acknowledge_private_capture:
        raise BulkloadError("private capture requires explicit acknowledgement")
    if (
        max_sqlite_families < 1
        or max_total_sqlite_bytes < 1
        or backup_timeout_seconds < 1
        or max_thread_entries < 1
        or max_thread_index_bytes < 1
        or max_metadata_entries < 1
        or max_metadata_bytes < 1
    ):
        raise BulkloadError("private capture budgets must be positive")

    codex_root, codex_info = _resolve_private_directory(codex_home, "Codex home")
    sqlite_root: Path | None = None
    sqlite_info: os.stat_result | None = None
    sqlite_authority_source: str | None = None
    if include_sqlite:
        if sqlite_home is None:
            raise BulkloadError(
                "SQLite capture requires an explicit effective SQLite home"
            )
        sqlite_authority = sqlite_home
        sqlite_authority_source = "explicit"
        sqlite_root, sqlite_info = _resolve_private_directory(
            sqlite_authority,
            "Codex SQLite home",
        )
    requested = output_directory.expanduser()
    if requested.name in {"", ".", ".."}:
        raise BulkloadError("private output directory must have a leaf name")
    parent, parent_info = _resolve_private_directory(
        requested.parent,
        "private output parent",
    )
    target = parent / requested.name
    if target.exists() or target.is_symlink():
        raise BulkloadError("private output already exists")
    if _path_is_within(target, codex_root) or (
        sqlite_root is not None and _path_is_within(target, sqlite_root)
    ):
        raise BulkloadError("private output must be outside captured roots")
    staging_name = f".{requested.name}.bulkload-private-{secrets.token_hex(12)}"
    staging = parent / staging_name
    codex_descriptor = _open_private_directory_descriptor(
        codex_root,
        codex_info,
        "Codex home",
    )
    sqlite_descriptor = -1
    try:
        if sqlite_root is not None and sqlite_info is not None:
            sqlite_descriptor = _open_private_directory_descriptor(
                sqlite_root,
                sqlite_info,
                "Codex SQLite home",
            )
        parent_descriptor = _open_private_directory_descriptor(
            parent,
            parent_info,
            "private output parent",
        )
    except BaseException:
        if sqlite_descriptor >= 0:
            os.close(sqlite_descriptor)
        os.close(codex_descriptor)
        raise
    staging_descriptor = -1
    published = False
    try:
        input_bindings: list[tuple[Path, int, os.stat_result, str]] = [
            (codex_root, codex_descriptor, codex_info, "Codex home")
        ]
        if (
            sqlite_root is not None
            and sqlite_info is not None
            and sqlite_descriptor >= 0
        ):
            input_bindings.append(
                (
                    sqlite_root,
                    sqlite_descriptor,
                    sqlite_info,
                    "Codex SQLite home",
                )
            )
        output_lineage = _directory_identity_lineage(parent_descriptor)
        captured_root_identities = {
            (info.st_dev, info.st_ino) for _, _, info, _ in input_bindings
        }
        if output_lineage & captured_root_identities:
            raise BulkloadError(
                "private output parent aliases or descends from a captured root"
            )
        for path, descriptor, info, label in (
            *input_bindings,
            (
                parent,
                parent_descriptor,
                parent_info,
                "private output parent",
            ),
        ):
            _revalidate_directory_binding(path, descriptor, info, label)
        os.mkdir(staging_name, 0o700, dir_fd=parent_descriptor)
        staging_info = os.stat(
            staging_name,
            dir_fd=parent_descriptor,
            follow_symlinks=False,
        )
        staging_descriptor = os.open(
            staging_name,
            os.O_RDONLY
            | getattr(os, "O_DIRECTORY", 0)
            | getattr(os, "O_NOFOLLOW", 0)
            | getattr(os, "O_CLOEXEC", 0),
            dir_fd=parent_descriptor,
        )
        if (
            not stat.S_ISDIR(staging_info.st_mode)
            or staging_info.st_uid != os.getuid()
            or stat.S_IMODE(staging_info.st_mode) != 0o700
            or _directory_identity(os.fstat(staging_descriptor))
            != _directory_identity(staging_info)
        ):
            raise BulkloadError("private staging directory custody is invalid")

        auth: dict[str, Any] | None = None
        if include_auth:
            _revalidate_directory_binding(
                staging,
                staging_descriptor,
                staging_info,
                "private staging directory",
            )
            _revalidate_directory_binding(
                codex_root,
                codex_descriptor,
                codex_info,
                "Codex home",
            )
            auth = _copy_auth(
                codex_descriptor,
                AUTH_BASENAME,
                staging_descriptor,
                AUTH_BASENAME,
            )
            _revalidate_directory_binding(
                codex_root,
                codex_descriptor,
                codex_info,
                "Codex home",
            )
            _revalidate_directory_binding(
                staging,
                staging_descriptor,
                staging_info,
                "private staging directory",
            )

        families: list[dict[str, Any]] = []
        if include_sqlite:
            assert sqlite_root is not None
            assert sqlite_info is not None
            assert sqlite_descriptor >= 0
            sqlite_output = staging / SQLITE_DIRECTORY
            os.mkdir(SQLITE_DIRECTORY, 0o700, dir_fd=staging_descriptor)
            sqlite_output_info = os.stat(
                SQLITE_DIRECTORY,
                dir_fd=staging_descriptor,
                follow_symlinks=False,
            )
            sqlite_output_descriptor = os.open(
                SQLITE_DIRECTORY,
                os.O_RDONLY
                | getattr(os, "O_DIRECTORY", 0)
                | getattr(os, "O_NOFOLLOW", 0)
                | getattr(os, "O_CLOEXEC", 0),
                dir_fd=staging_descriptor,
            )
            discovered = _discover_sqlite(
                sqlite_descriptor,
                max_families=max_sqlite_families,
                max_total_bytes=max_total_sqlite_bytes,
            )
            try:
                if (
                    not stat.S_ISDIR(sqlite_output_info.st_mode)
                    or sqlite_output_info.st_uid != os.getuid()
                    or stat.S_IMODE(sqlite_output_info.st_mode) != 0o700
                    or _directory_identity(os.fstat(sqlite_output_descriptor))
                    != _directory_identity(sqlite_output_info)
                ):
                    raise BulkloadError(
                        "private SQLite staging directory custody is invalid"
                    )
                remaining_sqlite_bytes = max_total_sqlite_bytes
                for basename, expected in discovered:
                    _revalidate_directory_binding(
                        sqlite_root,
                        sqlite_descriptor,
                        sqlite_info,
                        "Codex SQLite home",
                    )
                    _revalidate_directory_binding(
                        sqlite_output,
                        sqlite_output_descriptor,
                        sqlite_output_info,
                        "private SQLite staging directory",
                    )
                    family = _online_backup(
                        sqlite_root / basename,
                        sqlite_output / basename,
                        expected,
                        timeout_seconds=backup_timeout_seconds,
                        max_snapshot_bytes=remaining_sqlite_bytes,
                        max_thread_entries=max_thread_entries,
                        max_thread_index_bytes=max_thread_index_bytes,
                        max_metadata_entries=max_metadata_entries,
                        max_metadata_bytes=max_metadata_bytes,
                    )
                    _revalidate_directory_binding(
                        sqlite_root,
                        sqlite_descriptor,
                        sqlite_info,
                        "Codex SQLite home",
                    )
                    _revalidate_directory_binding(
                        sqlite_output,
                        sqlite_output_descriptor,
                        sqlite_output_info,
                        "private SQLite staging directory",
                    )
                    families.append(family)
                    remaining_sqlite_bytes -= family["snapshot_size"]
                final_discovery = _discover_sqlite(
                    sqlite_descriptor,
                    max_families=max_sqlite_families,
                    max_total_bytes=max_total_sqlite_bytes,
                )
                initial_authority = {
                    basename: _stable_identity(info) for basename, info in discovered
                }
                final_authority = {
                    basename: _stable_identity(info)
                    for basename, info in final_discovery
                }
                if final_authority != initial_authority:
                    raise BulkloadError(
                        "SQLite family authority changed during private capture"
                    )
                _fsync_directory_descriptor(sqlite_output_descriptor)
            finally:
                os.close(sqlite_output_descriptor)

        capture: dict[str, Any] = {
            "schema": PRIVATE_CAPTURE_SCHEMA,
            "capture_id": str(uuid.uuid4()),
            "captured_at": utc_now(),
            "role": role,
            "host_authority_id": host_authority_id,
            "host": os.uname().nodename,
            "codex_version": codex_version,
            "codex_home": {
                "resolved_path": os.fspath(codex_root),
                "identity": _identity(codex_info),
            },
            "sqlite_home": (
                {
                    "resolved_path": os.fspath(sqlite_root),
                    "identity": _identity(sqlite_info),
                    "authority_source": sqlite_authority_source,
                }
                if sqlite_root is not None and sqlite_info is not None
                else None
            ),
            "selected_state_classes": [
                name
                for name, selected in (
                    ("auth", include_auth),
                    ("sqlite", include_sqlite),
                )
                if selected
            ],
            "budgets": {
                "max_auth_bytes": MAX_AUTH_BYTES,
                "max_manifest_bytes": MAX_PRIVATE_MANIFEST_BYTES,
                "max_sqlite_families": max_sqlite_families,
                "max_total_sqlite_bytes": max_total_sqlite_bytes,
                "backup_timeout_seconds": backup_timeout_seconds,
                "max_thread_entries": max_thread_entries,
                "max_thread_index_bytes": max_thread_index_bytes,
                "max_metadata_entries": max_metadata_entries,
                "max_metadata_bytes": max_metadata_bytes,
            },
            "auth": auth,
            "sqlite_families": families,
            "copy_method": {
                "auth": "pinned-private-file" if auth is not None else None,
                "sqlite": ("sqlite-online-backup-api" if include_sqlite else None),
                "raw_wal_shm_copy": False,
            },
            "complete": True,
            "ready_for_apply": False,
        }
        capture["capture_sha256"] = object_digest(capture, "capture_sha256")
        manifest_payload = canonical_bytes(capture) + b"\n"
        if len(manifest_payload) > MAX_PRIVATE_MANIFEST_BYTES:
            raise BulkloadError(
                "private capture manifest exceeds the configured byte budget"
            )
        _write_exclusive_at(
            staging_descriptor,
            PRIVATE_MANIFEST,
            manifest_payload,
        )
        _fsync_directory_descriptor(staging_descriptor)
        for path, descriptor, info, label in (
            *input_bindings,
            (
                staging,
                staging_descriptor,
                staging_info,
                "private staging directory",
            ),
            (
                parent,
                parent_descriptor,
                parent_info,
                "private output parent",
            ),
        ):
            _revalidate_directory_binding(path, descriptor, info, label)
        _rename_noreplace_at(
            parent_descriptor,
            staging_name,
            parent_descriptor,
            target.name,
        )
        published = True
        _fsync_directory_descriptor(parent_descriptor)
        _revalidate_directory_binding(
            target,
            staging_descriptor,
            staging_info,
            "published private capture",
        )
        for path, descriptor, info, label in (
            *input_bindings,
            (
                parent,
                parent_descriptor,
                parent_info,
                "private output parent",
            ),
        ):
            _revalidate_directory_binding(path, descriptor, info, label)
        published_capture, _ = _read_private_bundle(target, role)
        if published_capture["capture_sha256"] != capture["capture_sha256"]:
            raise BulkloadError("published private capture digest mismatch")
        return capture
    except BaseException as error:
        if published:
            held = os.fstat(parent_descriptor)
            preserved = (
                f"directory-device={held.st_dev} directory-inode={held.st_ino} "
                f"leaf={target.name}"
            )
        else:
            try:
                _revalidate_directory_binding(
                    parent,
                    parent_descriptor,
                    parent_info,
                    "private output parent",
                )
            except BulkloadError:
                held = os.fstat(parent_descriptor)
                preserved = (
                    f"directory-device={held.st_dev} "
                    f"directory-inode={held.st_ino} leaf={staging_name}"
                )
            else:
                preserved = os.fspath(staging)
        raise BulkloadError(
            "private capture failed "
            f"({error}); preserved owner-private artifact: {preserved}"
        ) from error
    finally:
        if staging_descriptor >= 0:
            os.close(staging_descriptor)
        os.close(parent_descriptor)
        if sqlite_descriptor >= 0:
            os.close(sqlite_descriptor)
        os.close(codex_descriptor)


def _require_exact_keys(value: dict[str, Any], expected: set[str], label: str) -> None:
    if set(value) != expected:
        raise BulkloadError(f"{label} keys differ from the exact contract")


def _require_mapping(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise BulkloadError(f"{label} must be an object")
    return value


def _require_sha256(value: Any, label: str) -> None:
    if (
        not isinstance(value, str)
        or len(value) != 64
        or any(character not in "0123456789abcdef" for character in value)
    ):
        raise BulkloadError(f"{label} must be a lowercase SHA-256 digest")


def _validate_recorded_identity(value: Any, label: str) -> None:
    identity = _require_mapping(value, label)
    _require_exact_keys(
        identity,
        {"device", "inode", "uid", "mode", "links"},
        label,
    )
    if not all(
        isinstance(identity[key], int) and identity[key] >= 0 for key in identity
    ):
        raise BulkloadError(f"{label} values must be non-negative integers")


def validate_codex_private_capture(value: dict[str, Any]) -> None:
    _require_exact_keys(
        value,
        {
            "schema",
            "capture_id",
            "capture_sha256",
            "captured_at",
            "role",
            "host_authority_id",
            "host",
            "codex_version",
            "codex_home",
            "sqlite_home",
            "selected_state_classes",
            "budgets",
            "auth",
            "sqlite_families",
            "copy_method",
            "complete",
            "ready_for_apply",
        },
        "private capture",
    )
    if value["schema"] != PRIVATE_CAPTURE_SCHEMA:
        raise BulkloadError("unsupported private capture schema")
    _require_uuid(value["capture_id"], "capture ID")
    _require_uuid(value["host_authority_id"], "host authority ID")
    if value["role"] not in {"source", "destination"}:
        raise BulkloadError("invalid private capture role")
    if value["complete"] is not True or value["ready_for_apply"] is not False:
        raise BulkloadError("private capture readiness contract is invalid")
    for key in ("captured_at", "host", "codex_version"):
        if not isinstance(value[key], str) or not value[key]:
            raise BulkloadError(f"private capture {key} is invalid")
    codex_home = _require_mapping(value["codex_home"], "Codex home authority")
    _require_exact_keys(
        codex_home,
        {"resolved_path", "identity"},
        "Codex home authority",
    )
    if (
        not isinstance(codex_home["resolved_path"], str)
        or not Path(codex_home["resolved_path"]).is_absolute()
    ):
        raise BulkloadError("Codex home authority path must be absolute")
    _validate_recorded_identity(codex_home["identity"], "Codex home identity")
    selected = value["selected_state_classes"]
    if (
        not isinstance(selected, list)
        or not all(isinstance(item, str) for item in selected)
        or selected != sorted(set(selected))
        or not selected
        or not set(selected) <= {"auth", "sqlite"}
    ):
        raise BulkloadError("private capture state-class selection is invalid")
    if ("auth" in selected) != (value["auth"] is not None):
        raise BulkloadError("private capture auth selection is inconsistent")
    if ("sqlite" in selected) != (value["sqlite_home"] is not None):
        raise BulkloadError("private capture SQLite selection is inconsistent")
    if value["sqlite_home"] is not None:
        sqlite_home = _require_mapping(
            value["sqlite_home"],
            "Codex SQLite authority",
        )
        _require_exact_keys(
            sqlite_home,
            {"resolved_path", "identity", "authority_source"},
            "Codex SQLite authority",
        )
        if (
            not isinstance(sqlite_home["resolved_path"], str)
            or not Path(sqlite_home["resolved_path"]).is_absolute()
            or sqlite_home["authority_source"] != "explicit"
        ):
            raise BulkloadError("Codex SQLite authority is invalid")
        _validate_recorded_identity(
            sqlite_home["identity"],
            "Codex SQLite identity",
        )
    budgets = _require_mapping(value["budgets"], "private capture budgets")
    _require_exact_keys(
        budgets,
        {
            "max_auth_bytes",
            "max_manifest_bytes",
            "max_sqlite_families",
            "max_total_sqlite_bytes",
            "backup_timeout_seconds",
            "max_thread_entries",
            "max_thread_index_bytes",
            "max_metadata_entries",
            "max_metadata_bytes",
        },
        "private capture budgets",
    )
    if not all(isinstance(item, int) and item > 0 for item in budgets.values()):
        raise BulkloadError("private capture budgets must be positive integers")
    if (
        budgets["max_auth_bytes"] != MAX_AUTH_BYTES
        or budgets["max_manifest_bytes"] != MAX_PRIVATE_MANIFEST_BYTES
    ):
        raise BulkloadError("private capture fixed budgets differ from the contract")
    copy_method = _require_mapping(
        value["copy_method"],
        "private capture copy methods",
    )
    _require_exact_keys(
        copy_method,
        {"auth", "sqlite", "raw_wal_shm_copy"},
        "private capture copy methods",
    )
    if copy_method["raw_wal_shm_copy"] is not False:
        raise BulkloadError("raw SQLite companion copying is forbidden")
    if copy_method["auth"] != ("pinned-private-file" if "auth" in selected else None):
        raise BulkloadError("private capture auth copy method is inconsistent")
    if copy_method["sqlite"] != (
        "sqlite-online-backup-api" if "sqlite" in selected else None
    ):
        raise BulkloadError("private capture SQLite copy method is inconsistent")
    families = value["sqlite_families"]
    if not isinstance(families, list):
        raise BulkloadError("private capture SQLite families must be a list")
    basenames = [item.get("basename") for item in families if isinstance(item, dict)]
    if len(basenames) != len(families) or basenames != sorted(set(basenames)):
        raise BulkloadError("private capture SQLite families are not canonical")
    if ("sqlite" in selected) != bool(families):
        raise BulkloadError("private capture SQLite family selection is inconsistent")
    for item in families:
        _require_exact_keys(
            item,
            {
                "application_id",
                "basename",
                "copy_method",
                "latest_migration",
                "logical_source_bytes",
                "migration_count",
                "migrations_sha256",
                "metadata_entries",
                "metadata_bytes",
                "quick_check",
                "schema_sha256",
                "sha256",
                "snapshot_journal_mode",
                "snapshot_path",
                "snapshot_size",
                "source_identity",
                "source_size",
                "tables",
                "thread_count",
                "thread_index_bytes",
                "thread_ids_sha256",
                "thread_paths_sha256",
                "user_version",
            },
            "private capture SQLite family",
        )
        if not SQLITE_BASENAME.fullmatch(item["basename"]):
            raise BulkloadError("private capture has an invalid SQLite basename")
        if item["snapshot_path"] != f"{SQLITE_DIRECTORY}/{item['basename']}":
            raise BulkloadError("private capture SQLite path is invalid")
        if item.get("copy_method") != "sqlite-online-backup-api":
            raise BulkloadError("private capture did not use SQLite backup API")
        if item.get("quick_check") != "ok":
            raise BulkloadError("private capture SQLite quick_check is not green")
        if item.get("snapshot_journal_mode") != "delete":
            raise BulkloadError("private capture SQLite journal mode is invalid")
        for key in (
            "sha256",
            "schema_sha256",
            "migrations_sha256",
            "thread_ids_sha256",
            "thread_paths_sha256",
        ):
            _require_sha256(item[key], f"private SQLite {key}")
        for key in (
            "snapshot_size",
            "source_size",
            "logical_source_bytes",
            "migration_count",
            "metadata_entries",
            "metadata_bytes",
            "thread_count",
            "thread_index_bytes",
            "user_version",
            "application_id",
        ):
            if not isinstance(item[key], int) or item[key] < 0:
                raise BulkloadError(f"private SQLite {key} is invalid")
        if not isinstance(item["tables"], list):
            raise BulkloadError("private SQLite tables must be a list")
        if item["latest_migration"] is not None and not isinstance(
            item["latest_migration"],
            int,
        ):
            raise BulkloadError("private SQLite latest migration is invalid")
        _validate_recorded_identity(
            item["source_identity"],
            "private SQLite source identity",
        )
    if value["auth"] is not None:
        auth = _require_mapping(value["auth"], "private capture auth")
        _require_exact_keys(
            auth,
            {
                "basename",
                "snapshot_path",
                "sha256",
                "size",
                "source_identity",
            },
            "private capture auth",
        )
        if auth["basename"] != AUTH_BASENAME or auth["snapshot_path"] != AUTH_BASENAME:
            raise BulkloadError("private capture auth basename is invalid")
        _require_sha256(auth["sha256"], "private auth digest")
        if not isinstance(auth["size"], int) or not 0 < auth["size"] <= MAX_AUTH_BYTES:
            raise BulkloadError("private capture auth size is invalid")
        _validate_recorded_identity(
            auth["source_identity"],
            "private auth source identity",
        )
    _require_sha256(value["capture_sha256"], "private capture digest")
    if object_digest(value, "capture_sha256") != value["capture_sha256"]:
        raise BulkloadError("private capture digest mismatch")


def _read_private_bundle(path: Path, expected_role: str) -> tuple[dict[str, Any], Path]:
    root, root_info = _resolve_private_directory(
        path,
        f"{expected_role} private bundle",
    )
    root_descriptor = _open_private_directory_descriptor(
        root,
        root_info,
        f"{expected_role} private bundle",
    )
    try:
        value = _read_private_json_file_at(
            root_descriptor,
            PRIVATE_MANIFEST,
            f"{expected_role} private manifest",
            max_bytes=MAX_PRIVATE_MANIFEST_BYTES,
        )
        validate_codex_private_capture(value)
        if value["role"] != expected_role:
            raise BulkloadError(f"{expected_role} private bundle has the wrong role")

        expected_root_entries = {PRIVATE_MANIFEST}
        if value["auth"] is not None:
            expected_root_entries.add(AUTH_BASENAME)
        if value["sqlite_families"]:
            expected_root_entries.add(SQLITE_DIRECTORY)
        observed_root_entries = {entry.name for entry in os.scandir(root_descriptor)}
        if observed_root_entries != expected_root_entries:
            raise BulkloadError(
                f"{expected_role} private bundle namespace is not exact"
            )
        if value["auth"] is not None:
            _verify_private_artifact_at(
                root_descriptor,
                AUTH_BASENAME,
                value["auth"],
            )
        if value["sqlite_families"]:
            sqlite_info = os.stat(
                SQLITE_DIRECTORY,
                dir_fd=root_descriptor,
                follow_symlinks=False,
            )
            if (
                not stat.S_ISDIR(sqlite_info.st_mode)
                or sqlite_info.st_uid != os.getuid()
                or stat.S_IMODE(sqlite_info.st_mode) != 0o700
            ):
                raise BulkloadError(
                    f"{expected_role} private SQLite directory custody is invalid"
                )
            sqlite_descriptor = os.open(
                SQLITE_DIRECTORY,
                os.O_RDONLY
                | getattr(os, "O_DIRECTORY", 0)
                | getattr(os, "O_NOFOLLOW", 0)
                | getattr(os, "O_CLOEXEC", 0),
                dir_fd=root_descriptor,
            )
            try:
                if _directory_identity(os.fstat(sqlite_descriptor)) != (
                    _directory_identity(sqlite_info)
                ):
                    raise BulkloadError(
                        f"{expected_role} private SQLite directory changed"
                    )
                declared_families = {
                    item["basename"] for item in value["sqlite_families"]
                }
                observed_families = {
                    entry.name for entry in os.scandir(sqlite_descriptor)
                }
                if observed_families != declared_families:
                    raise BulkloadError(
                        f"{expected_role} private SQLite namespace is not exact"
                    )
                for item in value["sqlite_families"]:
                    _verify_private_artifact_at(
                        sqlite_descriptor,
                        item["basename"],
                        item,
                    )
            finally:
                os.close(sqlite_descriptor)
        _revalidate_directory_binding(
            root,
            root_descriptor,
            root_info,
            f"{expected_role} private bundle",
        )
        return value, root
    finally:
        os.close(root_descriptor)


def revalidate_codex_private_bundles(
    source_directory: Path,
    destination_directory: Path,
    *,
    expected_source_capture_sha256: str,
    expected_destination_capture_sha256: str,
) -> None:
    source, _ = _read_private_bundle(source_directory, "source")
    destination, _ = _read_private_bundle(
        destination_directory,
        "destination",
    )
    if source["capture_sha256"] != expected_source_capture_sha256:
        raise BulkloadError("source private capture changed during planning")
    if destination["capture_sha256"] != expected_destination_capture_sha256:
        raise BulkloadError("destination private capture changed during planning")


def _verify_private_artifact(path: Path, item: dict[str, Any]) -> None:
    observed, size = _hash_regular_file(path)
    expected_size = item.get("size", item.get("snapshot_size"))
    if observed != item["sha256"] or size != expected_size:
        raise BulkloadError("private bundle artifact digest mismatch")
    if stat.S_IMODE(path.stat(follow_symlinks=False).st_mode) != 0o600:
        raise BulkloadError("private bundle artifact mode is not 0600")


def _verify_private_artifact_at(
    parent_descriptor: int,
    name: str,
    item: dict[str, Any],
) -> None:
    observed, size = _hash_regular_file_at(
        parent_descriptor,
        name,
        "private bundle artifact",
    )
    expected_size = item.get("size", item.get("snapshot_size"))
    if observed != item["sha256"] or size != expected_size:
        raise BulkloadError("private bundle artifact digest mismatch")
    info = os.stat(name, dir_fd=parent_descriptor, follow_symlinks=False)
    if stat.S_IMODE(info.st_mode) != 0o600:
        raise BulkloadError("private bundle artifact mode is not 0600")


def _sqlite_family_map(capture: dict[str, Any]) -> dict[str, dict[str, Any]]:
    return {item["basename"]: item for item in capture["sqlite_families"]}


def _capture_live_roots(capture: dict[str, Any]) -> list[str]:
    roots = {capture["codex_home"]["resolved_path"]}
    if capture["sqlite_home"] is not None:
        roots.add(capture["sqlite_home"]["resolved_path"])
    return sorted(roots)


def _state_thread_relation(
    source: Path,
    destination: Path,
    *,
    max_thread_entries: int,
    max_thread_index_bytes: int,
    timeout_seconds: int,
) -> dict[str, Any]:
    deadline = time.monotonic() + timeout_seconds

    def index(path: Path) -> tuple[dict[str, str], str]:
        connection = sqlite3.connect(_sqlite_uri(path), uri=True)
        try:
            connection.execute("PRAGMA query_only=ON")
            connection.set_progress_handler(
                lambda: int(time.monotonic() > deadline),
                10_000,
            )
            tables = {
                str(row[0])
                for row in connection.execute(
                    "SELECT name FROM sqlite_schema WHERE type='table'"
                )
            }
            if "threads" not in tables:
                return {}, sha256_bytes(canonical_bytes([]))
            values: dict[str, str] = {}
            total_bytes = 0
            digest = hashlib.sha256()
            digest.update(b"[")
            first = True
            for row in connection.execute(
                "SELECT id, rollout_path FROM threads ORDER BY id"
            ):
                if time.monotonic() > deadline:
                    raise BulkloadError(
                        "SQLite thread relation exceeded its time budget"
                    )
                thread_id = str(row[0])
                rollout_path = str(row[1])
                total_bytes += len(thread_id.encode("utf-8")) + len(
                    rollout_path.encode("utf-8")
                )
                if (
                    len(values) >= max_thread_entries
                    or total_bytes > max_thread_index_bytes
                ):
                    raise BulkloadError(
                        "SQLite thread relation exceeds its planning budget"
                    )
                if thread_id in values:
                    raise BulkloadError("SQLite thread IDs are not unique")
                values[thread_id] = rollout_path
                if not first:
                    digest.update(b",")
                digest.update(canonical_bytes(thread_id))
                first = False
            digest.update(b"]")
            return values, digest.hexdigest()
        except sqlite3.Error as error:
            raise BulkloadError("SQLite thread relation query failed") from error
        finally:
            connection.close()

    source_index, source_digest = index(source)
    destination_index, destination_digest = index(destination)
    source_ids = set(source_index)
    destination_ids = set(destination_index)
    shared = source_ids & destination_ids
    different_paths = sum(
        source_index[item] != destination_index[item] for item in shared
    )
    return {
        "source_threads": len(source_ids),
        "destination_threads": len(destination_ids),
        "shared_threads": len(shared),
        "source_only_threads": len(source_ids - destination_ids),
        "destination_only_threads": len(destination_ids - source_ids),
        "shared_path_mismatches": different_paths,
        "source_ids_sha256": source_digest,
        "destination_ids_sha256": destination_digest,
        "session_union_plan_required": bool(source_ids - destination_ids)
        or different_paths > 0,
    }


def compile_codex_private_state_plan(
    source_directory: Path,
    destination_directory: Path,
) -> dict[str, Any]:
    """Compile a compatibility plan. This function never installs state."""
    source, source_root = _read_private_bundle(source_directory, "source")
    destination, destination_root = _read_private_bundle(
        destination_directory,
        "destination",
    )
    blockers: list[dict[str, Any]] = []
    if source["host_authority_id"] == destination["host_authority_id"]:
        blockers.append({"code": "same-host-authority"})
    if source["codex_version"] != destination["codex_version"]:
        blockers.append(
            {
                "code": "codex-version-mismatch",
                "source": source["codex_version"],
                "destination": destination["codex_version"],
            }
        )

    source_selected = set(source["selected_state_classes"])
    destination_selected = set(destination["selected_state_classes"])
    if source_selected != destination_selected:
        blockers.append(
            {
                "code": "state-class-selection-mismatch",
                "source": sorted(source_selected),
                "destination": sorted(destination_selected),
            }
        )
    source_auth = source["auth"]
    destination_auth = destination["auth"]
    if "auth" not in source_selected and "auth" not in destination_selected:
        auth_action: dict[str, Any] = {"action": "not-selected"}
    elif source_auth is None:
        auth_action = {"action": "blocked"}
        blockers.append({"code": "source-auth-not-captured"})
    else:
        auth_action = {
            "action": (
                "preserve-identical"
                if destination_auth is not None
                and source_auth["sha256"] == destination_auth["sha256"]
                else "install-source-after-destination-backup"
            ),
            "source_sha256": source_auth["sha256"],
            "destination_sha256": (
                destination_auth["sha256"] if destination_auth is not None else None
            ),
            "executor_implemented": False,
        }
        if auth_action["action"] != "preserve-identical":
            blockers.append({"code": "auth-installer-not-implemented"})

    source_families = _sqlite_family_map(source)
    destination_families = _sqlite_family_map(destination)
    missing_source = sorted(set(destination_families) - set(source_families))
    missing_destination = sorted(set(source_families) - set(destination_families))
    if missing_source or missing_destination:
        blockers.append(
            {
                "code": "sqlite-family-set-mismatch",
                "missing_from_source": missing_source,
                "missing_from_destination": missing_destination,
            }
        )

    family_plans: list[dict[str, Any]] = []
    for basename in sorted(set(source_families) & set(destination_families)):
        source_family = source_families[basename]
        destination_family = destination_families[basename]
        schema_compatible = (
            source_family["schema_sha256"] == destination_family["schema_sha256"]
            and source_family["migrations_sha256"]
            == destination_family["migrations_sha256"]
        )
        header_compatible = (
            source_family["user_version"] == destination_family["user_version"]
            and source_family["application_id"] == destination_family["application_id"]
        )
        compatible = schema_compatible and header_compatible
        if not schema_compatible:
            blockers.append(
                {
                    "code": "sqlite-schema-mismatch",
                    "basename": basename,
                    "source_schema_sha256": source_family["schema_sha256"],
                    "destination_schema_sha256": destination_family["schema_sha256"],
                    "source_latest_migration": source_family["latest_migration"],
                    "destination_latest_migration": destination_family[
                        "latest_migration"
                    ],
                }
            )
        if not header_compatible:
            blockers.append(
                {
                    "code": "sqlite-header-mismatch",
                    "basename": basename,
                    "source_user_version": source_family["user_version"],
                    "destination_user_version": destination_family["user_version"],
                    "source_application_id": source_family["application_id"],
                    "destination_application_id": destination_family["application_id"],
                }
            )
        strategy = "unsupported-family"
        if basename.startswith("state_"):
            strategy = "thread-and-edge-union"
        elif basename.startswith("logs_"):
            strategy = "append-log-union"
        elif basename.startswith("goals_"):
            strategy = "thread-keyed-goal-union"
        elif basename.startswith("memories_"):
            strategy = "thread-keyed-memory-union"
        blockers.append(
            {
                "code": "sqlite-composer-not-implemented",
                "basename": basename,
                "strategy": strategy,
            }
        )
        entry: dict[str, Any] = {
            "basename": basename,
            "schema_compatible": schema_compatible,
            "header_compatible": header_compatible,
            "compatible": compatible,
            "strategy": strategy,
            "source_sha256": source_family["sha256"],
            "destination_sha256": destination_family["sha256"],
            "executor_implemented": False,
        }
        if basename.startswith("state_"):
            source_state = source_root / SQLITE_DIRECTORY / basename
            destination_state = destination_root / SQLITE_DIRECTORY / basename
            entry["thread_relation"] = _state_thread_relation(
                source_state,
                destination_state,
                max_thread_entries=min(
                    source["budgets"]["max_thread_entries"],
                    destination["budgets"]["max_thread_entries"],
                ),
                max_thread_index_bytes=min(
                    source["budgets"]["max_thread_index_bytes"],
                    destination["budgets"]["max_thread_index_bytes"],
                ),
                timeout_seconds=min(
                    source["budgets"]["backup_timeout_seconds"],
                    destination["budgets"]["backup_timeout_seconds"],
                ),
            )
            _verify_private_artifact(source_state, source_family)
            _verify_private_artifact(destination_state, destination_family)
        family_plans.append(entry)

    plan: dict[str, Any] = {
        "schema": PRIVATE_PLAN_SCHEMA,
        "created_at": utc_now(),
        "source_capture_sha256": source["capture_sha256"],
        "destination_capture_sha256": destination["capture_sha256"],
        "source_host_authority_id": source["host_authority_id"],
        "destination_host_authority_id": destination["host_authority_id"],
        "codex_version": {
            "source": source["codex_version"],
            "destination": destination["codex_version"],
        },
        "auth": auth_action,
        "sqlite_families": family_plans,
        "protected_live_roots": {
            "source": _capture_live_roots(source),
            "destination": _capture_live_roots(destination),
        },
        "blockers": blockers,
        "ready_for_apply": False,
        "implementation": "capture-and-compatibility-plan",
    }
    revalidate_codex_private_bundles(
        source_directory,
        destination_directory,
        expected_source_capture_sha256=source["capture_sha256"],
        expected_destination_capture_sha256=destination["capture_sha256"],
    )
    plan["plan_sha256"] = object_digest(plan, "plan_sha256")
    return plan


def write_private_json_noreplace(
    output: Path,
    value: dict[str, Any],
    *,
    protected_directories: tuple[Path, ...] = (),
    recorded_protected_directories: tuple[Path, ...] = (),
) -> None:
    requested = output.expanduser()
    if requested.name in {"", ".", ".."}:
        raise BulkloadError("private evidence output must have a leaf name")
    parent, parent_info = _resolve_private_directory(
        requested.parent,
        "private evidence output parent",
    )
    target = parent / requested.name
    protected_identities: set[tuple[int, int]] = set()
    for protected in protected_directories:
        resolved, protected_info = _resolve_private_directory(
            protected,
            "private input bundle",
        )
        protected_identities.add((protected_info.st_dev, protected_info.st_ino))
        if target == resolved or _path_is_within(target, resolved):
            raise BulkloadError("private evidence output overlaps an input bundle")
    for protected in recorded_protected_directories:
        expanded = protected.expanduser()
        if not expanded.is_absolute():
            raise BulkloadError("recorded private root must be absolute")
        lexical = Path(os.path.abspath(expanded))
        if target == lexical or _path_is_within(target, lexical):
            raise BulkloadError("private evidence output overlaps a recorded live root")
        try:
            resolved, protected_info = _resolve_private_directory(
                protected,
                "recorded private root",
            )
        except BulkloadError:
            continue
        protected_identities.add((protected_info.st_dev, protected_info.st_ino))
        if target == resolved or _path_is_within(target, resolved):
            raise BulkloadError("private evidence output overlaps a recorded live root")
    if target.exists() or target.is_symlink():
        raise BulkloadError("private evidence output already exists")
    payload = canonical_bytes(value) + b"\n"
    if len(payload) > MAX_PRIVATE_PLAN_BYTES:
        raise BulkloadError("private evidence plan exceeds the configured byte budget")
    parent_descriptor = _open_private_directory_descriptor(
        parent,
        parent_info,
        "private evidence output parent",
    )
    temporary_name = f".{target.name}.bulkload-private-{secrets.token_hex(12)}"
    published = False
    try:
        if _directory_identity_lineage(parent_descriptor) & protected_identities:
            raise BulkloadError(
                "private evidence output aliases or descends from a protected root"
            )
        _revalidate_directory_binding(
            parent,
            parent_descriptor,
            parent_info,
            "private evidence output parent",
        )
        _write_exclusive_at(parent_descriptor, temporary_name, payload)
        _revalidate_directory_binding(
            parent,
            parent_descriptor,
            parent_info,
            "private evidence output parent",
        )
        _rename_noreplace_at(
            parent_descriptor,
            temporary_name,
            parent_descriptor,
            target.name,
        )
        published = True
        _fsync_directory_descriptor(parent_descriptor)
        _verify_private_payload_at(parent_descriptor, target.name, payload)
        _revalidate_directory_binding(
            parent,
            parent_descriptor,
            parent_info,
            "private evidence output parent",
        )
    except BaseException as error:
        if published:
            held = os.fstat(parent_descriptor)
            preserved = (
                f"directory-device={held.st_dev} directory-inode={held.st_ino} "
                f"leaf={target.name}"
            )
        else:
            try:
                _revalidate_directory_binding(
                    parent,
                    parent_descriptor,
                    parent_info,
                    "private evidence output parent",
                )
            except BulkloadError:
                held = os.fstat(parent_descriptor)
                preserved = (
                    f"directory-device={held.st_dev} "
                    f"directory-inode={held.st_ino} leaf={temporary_name}"
                )
            else:
                preserved = os.fspath(parent / temporary_name)
        raise BulkloadError(
            "private evidence publication failed "
            f"({error}); preserved owner-private artifact: {preserved}"
        ) from error
    finally:
        os.close(parent_descriptor)
