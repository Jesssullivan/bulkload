"""Read-only oracle for offline private Codex SQLite bundle candidates.

This module has no CLI, publisher, writer, installer, or live-state mutation
surface.  It deliberately implements its own typed-row walk.  A future
composer must not import this module, and this module must never import the
composer.
"""

from __future__ import annotations

from contextlib import contextmanager
from copy import deepcopy
from datetime import UTC, datetime
import hashlib
import json
import os
from pathlib import Path
from urllib.parse import quote
import re
import sqlite3
import stat
import struct
import time
from typing import Any, Iterator
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
from .private_sqlite_plan import (
    MAX_SQLITE_PLAN_BYTES,
    MAX_SQLITE_PLAN_ROW_BYTES,
    MAX_SQLITE_PLAN_ROWS,
    MAX_SQLITE_PLAN_SECONDS,
    MAX_SQLITE_VALUE_BYTES,
    validate_codex_private_sqlite_compose_plan,
)
from .private_sqlite_protocol import (
    COMPOSED_BUNDLE_MANIFEST_SCHEMA,
    COMPOSITION_RECEIPT_SCHEMA,
    MAX_FAILURES,
    MAX_PROTOCOL_BYTES,
    MAX_TABLES,
    MAX_TREE_ENTRIES,
    PRESEAL_RECEIPT_EXCLUDED_NODE_IDS,
    SQLITE_FAMILY_BASENAME,
    VERIFIER_ORACLE_REPORT_IMPLEMENTATION,
    VERIFIER_ORACLE_REPORT_SCHEMA,
    validate_composed_bundle_manifest,
    validate_composition_receipt,
    validate_verifier_oracle_report,
)
from .private_sqlite_request import (
    PRIVATE_SQLITE_CAPACITY_OBSERVATION_SCHEMA,
    PRIVATE_SQLITE_COMPOSE_REQUEST_SCHEMA,
    validate_codex_private_sqlite_capacity_observation,
    validate_codex_private_sqlite_capacity_observation_against_request,
    validate_codex_private_sqlite_compose_request,
    validate_codex_private_sqlite_compose_request_against_action,
)


PRIVATE_SQLITE_VERIFIER_IMPLEMENTATION = VERIFIER_ORACLE_REPORT_IMPLEMENTATION
PRIVATE_SQLITE_VERIFIER_SOURCE_PATH = "scripts/bulkload_lib/private_sqlite_verifier.py"
_SQLITE_FETCH_OVERHEAD_BYTES = 1024 * 1024
MAX_VERIFIER_RETAINED_ROWS = 100_000
MAX_VERIFIER_RETAINED_IDENTITY_BYTES = MAX_PROTOCOL_BYTES
MAX_SQLITE_SCHEMA_ACTIONS = MAX_TREE_ENTRIES
MAX_SQLITE_SCHEMA_COLUMNS = 1024
MAX_SQLITE_SCHEMA_SQL_BYTES = MAX_SQLITE_PLAN_BYTES
MAX_SQLITE_MIGRATION_BYTES = MAX_SQLITE_PLAN_BYTES
_MAX_SQLITE_SCHEMA_FETCH_BYTES = (
    MAX_SQLITE_SCHEMA_SQL_BYTES + _SQLITE_FETCH_OVERHEAD_BYTES
)
_MAX_SQLITE_SCHEMA_QUERY_BYTES = 64 * 1024
_IDENTIFIER = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
_UTC_SECONDS = re.compile(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z")


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


def _parse_utc(value: Any, label: str) -> datetime:
    if not isinstance(value, str) or _UTC_SECONDS.fullmatch(value) is None:
        raise BulkloadError(f"{label} must be canonical UTC seconds")
    try:
        parsed = datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=UTC)
    except ValueError as error:
        raise BulkloadError(f"{label} is invalid") from error
    return parsed


def _unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, item in pairs:
        if key in value:
            raise ValueError(f"duplicate JSON key {key!r} is forbidden")
        value[key] = item
    return value


def _reject_nonfinite(value: str) -> None:
    raise ValueError(f"non-finite JSON number {value} is forbidden")


def _identity(info: os.stat_result) -> dict[str, int]:
    return {
        "device": int(info.st_dev),
        "inode": int(info.st_ino),
        "uid": int(info.st_uid),
        "mode": stat.S_IMODE(info.st_mode),
    }


def _stable(info: os.stat_result) -> tuple[int, ...]:
    return (
        int(info.st_dev),
        int(info.st_ino),
        int(info.st_mode),
        int(info.st_nlink),
        int(info.st_uid),
        int(info.st_size),
        int(info.st_mtime_ns),
        int(info.st_ctime_ns),
    )


def _mount(descriptor: int) -> dict[str, int | None]:
    info = os.fstat(descriptor)
    filesystem = os.fstatvfs(descriptor)
    filesystem_id = getattr(filesystem, "f_fsid", None)
    if filesystem_id is not None:
        filesystem_id = int(filesystem_id)
    linux_mount_id: int | None = None
    if os.path.exists("/proc/self/fdinfo"):
        try:
            payload = Path(f"/proc/self/fdinfo/{descriptor}").read_text(
                encoding="utf-8"
            )
        except OSError as error:
            raise BulkloadError("cannot bind verifier mount identity") from error
        for line in payload.splitlines():
            if line.startswith("mnt_id:"):
                try:
                    linux_mount_id = int(line.split(":", 1)[1].strip())
                except ValueError as error:
                    raise BulkloadError(
                        "verifier Linux mount identity is invalid"
                    ) from error
                break
        if linux_mount_id is None:
            raise BulkloadError("verifier Linux mount identity is unavailable")
    if filesystem_id is None and linux_mount_id is None:
        raise BulkloadError("verifier mount identity is unavailable")
    return {
        "device": int(info.st_dev),
        "filesystem_id": filesystem_id,
        "linux_mount_id": linux_mount_id,
    }


def _lineage(descriptor: int) -> list[dict[str, Any]]:
    flags = (
        os.O_RDONLY
        | getattr(os, "O_DIRECTORY", 0)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_CLOEXEC", 0)
    )
    current = os.dup(descriptor)
    result: list[dict[str, Any]] = []
    seen: set[bytes] = set()
    try:
        for _ in range(128):
            info = os.fstat(current)
            record = {"identity": _identity(info), "mount": _mount(current)}
            encoded = canonical_bytes(record)
            if encoded in seen:
                raise BulkloadError("verifier directory lineage contains a cycle")
            seen.add(encoded)
            result.append(record)
            parent = os.open("..", flags, dir_fd=current)
            parent_info = os.fstat(parent)
            parent_mount = _mount(parent)
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
                return result
            os.close(current)
            current = parent
    finally:
        os.close(current)
    raise BulkloadError("verifier directory lineage exceeds its bound")


def _open_private_directory_at(
    parent_descriptor: int,
    leaf: str,
    *,
    label: str,
) -> tuple[int, os.stat_result]:
    if not isinstance(leaf, str) or not leaf or "/" in leaf or leaf in {".", ".."}:
        raise BulkloadError(f"{label} leaf is invalid")
    flags = (
        os.O_RDONLY
        | getattr(os, "O_DIRECTORY", 0)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_CLOEXEC", 0)
    )
    try:
        descriptor = os.open(leaf, flags, dir_fd=parent_descriptor)
    except OSError as error:
        raise BulkloadError(f"cannot open {label}") from error
    try:
        info = os.fstat(descriptor)
        entry = os.stat(leaf, dir_fd=parent_descriptor, follow_symlinks=False)
        if (
            not stat.S_ISDIR(info.st_mode)
            or info.st_uid != os.getuid()
            or stat.S_IMODE(info.st_mode) != 0o700
            or _stable(info) != _stable(entry)
        ):
            raise BulkloadError(f"{label} custody is invalid")
        return descriptor, info
    except BaseException:
        os.close(descriptor)
        raise


def _open_absolute_parent(path: Path) -> tuple[Path, int, os.stat_result]:
    raw = os.fspath(path)
    if (
        not path.is_absolute()
        or "\x00" in raw
        or ".." in path.parts
        or path.name in {"", ".", ".."}
    ):
        raise BulkloadError("offline bundle candidate path must be absolute")
    normalized = Path(os.path.abspath(raw))
    if os.fspath(normalized) != raw:
        raise BulkloadError("offline bundle candidate path is invalid")
    parent = normalized.parent
    flags = (
        os.O_RDONLY
        | getattr(os, "O_DIRECTORY", 0)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_CLOEXEC", 0)
    )
    descriptor = os.open(parent.anchor or "/", flags)
    traversed = Path(parent.anchor or "/")
    try:
        for component in parent.parts[1:]:
            try:
                child = os.open(component, flags, dir_fd=descriptor)
            except OSError as error:
                raise BulkloadError(
                    f"offline bundle candidate parent traverses a non-directory: "
                    f"{traversed / component}"
                ) from error
            os.close(descriptor)
            descriptor = child
            traversed /= component
        info = os.fstat(descriptor)
        if not stat.S_ISDIR(info.st_mode):
            raise BulkloadError("offline bundle candidate parent is not a directory")
        return normalized, descriptor, info
    except BaseException:
        os.close(descriptor)
        raise


def _read_private_json_at(
    parent_descriptor: int,
    leaf: str,
    *,
    label: str,
) -> tuple[dict[str, Any], bytes, os.stat_result, int]:
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0)
    try:
        descriptor = os.open(leaf, flags, dir_fd=parent_descriptor)
    except OSError as error:
        raise BulkloadError(f"cannot open {label}") from error
    try:
        before = os.fstat(descriptor)
        entry = os.stat(leaf, dir_fd=parent_descriptor, follow_symlinks=False)
        if (
            not stat.S_ISREG(before.st_mode)
            or before.st_uid != os.getuid()
            or before.st_nlink != 1
            or stat.S_IMODE(before.st_mode) != 0o600
            or before.st_size < 2
            or before.st_size > MAX_PROTOCOL_BYTES
            or _stable(before) != _stable(entry)
        ):
            raise BulkloadError(f"{label} custody or size is invalid")
        payload = b""
        while len(payload) <= MAX_PROTOCOL_BYTES:
            block = os.read(
                descriptor,
                min(65536, MAX_PROTOCOL_BYTES + 1 - len(payload)),
            )
            if not block:
                break
            payload += block
        after = os.fstat(descriptor)
        if len(payload) != before.st_size or _stable(before) != _stable(after):
            raise BulkloadError(f"{label} changed while reading")
        try:
            value = json.loads(
                payload.decode("utf-8"),
                object_pairs_hook=_unique_object,
                parse_constant=_reject_nonfinite,
            )
        except (UnicodeDecodeError, json.JSONDecodeError, ValueError) as error:
            raise BulkloadError(f"{label} is not strict JSON") from error
        if not isinstance(value, dict) or canonical_bytes(value) + b"\n" != payload:
            raise BulkloadError(f"{label} is not canonical JSON with one newline")
        return value, payload, before, descriptor
    except BaseException:
        os.close(descriptor)
        raise


def _require_before_deadline(deadline: float, label: str) -> None:
    if time.monotonic() > deadline:
        raise BulkloadError(f"{label} exceeded its deadline")


def _bounded_directory_entries(
    descriptor: int,
    *,
    label: str,
    deadline: float,
) -> list[str]:
    _require_before_deadline(deadline, label)
    entries: list[str] = []
    flags = (
        os.O_RDONLY
        | getattr(os, "O_DIRECTORY", 0)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_CLOEXEC", 0)
    )
    try:
        scan_descriptor = os.open(".", flags, dir_fd=descriptor)
    except OSError as error:
        raise BulkloadError(f"cannot open {label} for enumeration") from error
    try:
        with os.scandir(scan_descriptor) as iterator:
            for entry in iterator:
                _require_before_deadline(deadline, label)
                entries.append(entry.name)
                if len(entries) > MAX_TREE_ENTRIES:
                    raise BulkloadError(f"{label} entry budget exceeded")
    except OSError as error:
        raise BulkloadError(f"cannot enumerate {label}") from error
    finally:
        os.close(scan_descriptor)
    _require_before_deadline(deadline, label)
    return sorted(entries)


def _hash_descriptor(
    descriptor: int,
    maximum: int,
    *,
    deadline: float,
) -> tuple[str, int]:
    _require_before_deadline(deadline, "offline SQLite candidate hash")
    before = os.fstat(descriptor)
    if before.st_size < 0 or before.st_size > maximum:
        raise BulkloadError("offline SQLite candidate exceeds its byte budget")
    digest = hashlib.sha256()
    size = 0
    while size <= maximum:
        _require_before_deadline(deadline, "offline SQLite candidate hash")
        block = os.pread(
            descriptor,
            min(1024 * 1024, maximum + 1 - size),
            size,
        )
        if not block:
            break
        digest.update(block)
        size += len(block)
        _require_before_deadline(deadline, "offline SQLite candidate hash")
    after = os.fstat(descriptor)
    _require_before_deadline(deadline, "offline SQLite candidate hash")
    if size != before.st_size or _stable(before) != _stable(after):
        raise BulkloadError("offline SQLite candidate changed while hashing")
    return digest.hexdigest(), size


def _sqlite_health_checks(
    connection: sqlite3.Connection,
    *,
    deadline: float,
) -> tuple[bool, int]:
    _require_before_deadline(deadline, "verifier SQLite health checks")

    def _interrupt() -> int:
        return int(time.monotonic() > deadline)

    integrity_cursor: sqlite3.Cursor | None = None
    foreign_key_cursor: sqlite3.Cursor | None = None
    connection.set_progress_handler(_interrupt, 1000)
    try:
        integrity_cursor = connection.execute("PRAGMA integrity_check(1)")
        integrity_row = integrity_cursor.fetchone()
        _require_before_deadline(deadline, "verifier SQLite integrity check")
        integrity_extra = (
            integrity_cursor.fetchone() if integrity_row == ("ok",) else None
        )
        _require_before_deadline(deadline, "verifier SQLite integrity check")
        integrity_ok = integrity_row == ("ok",) and integrity_extra is None

        foreign_key_cursor = connection.execute("PRAGMA foreign_key_check")
        foreign_key_violation = foreign_key_cursor.fetchone()
        _require_before_deadline(deadline, "verifier SQLite foreign-key check")
        return integrity_ok, int(foreign_key_violation is not None)
    except sqlite3.Error as error:
        if time.monotonic() > deadline or "interrupted" in str(error).casefold():
            raise BulkloadError(
                "verifier SQLite health checks exceeded their deadline"
            ) from error
        raise BulkloadError("verifier SQLite health checks failed") from error
    finally:
        for cursor in (foreign_key_cursor, integrity_cursor):
            if cursor is not None:
                cursor.close()
        connection.set_progress_handler(None, 0)


@contextmanager
def _open_immutable_database(
    sqlite_descriptor: int,
    basename: str,
    *,
    maximum_bytes: int,
) -> Iterator[tuple[sqlite3.Connection, int, os.stat_result]]:
    if SQLITE_FAMILY_BASENAME.fullmatch(basename) is None:
        raise BulkloadError("offline SQLite candidate basename is invalid")
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0)
    try:
        descriptor = os.open(basename, flags, dir_fd=sqlite_descriptor)
    except OSError as error:
        raise BulkloadError("cannot open offline SQLite candidate") from error
    connection: sqlite3.Connection | None = None
    try:
        before = os.fstat(descriptor)
        entry = os.stat(
            basename,
            dir_fd=sqlite_descriptor,
            follow_symlinks=False,
        )
        if (
            not stat.S_ISREG(before.st_mode)
            or before.st_uid != os.getuid()
            or before.st_nlink != 1
            or stat.S_IMODE(before.st_mode) != 0o600
            or before.st_size < 1
            or before.st_size > maximum_bytes
            or _stable(before) != _stable(entry)
        ):
            raise BulkloadError("offline SQLite candidate custody is invalid")
        descriptor_path = f"/dev/fd/{descriptor}"
        if not Path(descriptor_path).exists():
            raise BulkloadError("descriptor-backed SQLite access is unavailable")
        uri = f"file:{quote(descriptor_path, safe='/')}?mode=ro&immutable=1"
        connection = sqlite3.connect(uri, uri=True)
        connection.execute("PRAGMA query_only=ON")
        try:
            connection.enable_load_extension(False)
        except sqlite3.NotSupportedError:
            pass
        yield connection, descriptor, before
        after = os.fstat(descriptor)
        path_after = os.stat(
            basename,
            dir_fd=sqlite_descriptor,
            follow_symlinks=False,
        )
        if _stable(before) != _stable(after) or _stable(before) != _stable(path_after):
            raise BulkloadError("offline SQLite candidate changed during observation")
    except sqlite3.DatabaseError as error:
        raise BulkloadError("cannot inspect offline SQLite candidate") from error
    finally:
        if connection is not None:
            connection.close()
        os.close(descriptor)


def _quote_identifier(value: Any) -> str:
    if not isinstance(value, str) or _IDENTIFIER.fullmatch(value) is None:
        raise BulkloadError("verifier SQLite identifier is invalid")
    return '"' + '""'.join(value.split('"')) + '"'


def _schema_sql_digest(value: Any) -> str | None:
    if value is None:
        return None
    if not isinstance(value, str):
        raise BulkloadError("verifier SQLite schema SQL is not text")
    return sha256_bytes(value.encode("utf-8"))


def _sql_contains_keyword(value: Any, keyword: str) -> bool:
    """Scan SQL tokens without importing the producer's schema classifier."""
    if not isinstance(value, str):
        return False
    cursor = 0
    while cursor < len(value):
        character = value[cursor]
        pair = value[cursor : cursor + 2]
        if pair == "--":
            newline = value.find("\n", cursor + 2)
            cursor = len(value) if newline < 0 else newline + 1
            continue
        if pair == "/*":
            close = value.find("*/", cursor + 2)
            cursor = len(value) if close < 0 else close + 2
            continue
        if character in {"'", '"', "`", "["}:
            close_character = "]" if character == "[" else character
            cursor += 1
            while cursor < len(value):
                if value[cursor] != close_character:
                    cursor += 1
                elif (
                    close_character != "]"
                    and cursor + 1 < len(value)
                    and value[cursor + 1] == close_character
                ):
                    cursor += 2
                else:
                    cursor += 1
                    break
            continue
        if character.isalpha() or character == "_":
            end = cursor + 1
            while end < len(value) and (value[end].isalnum() or value[end] == "_"):
                end += 1
            if value[cursor:end].casefold() == keyword.casefold():
                return True
            cursor = end
            continue
        cursor += 1
    return False


def _declared_affinity(declared_type: str) -> str:
    upper = declared_type.upper()
    if "INT" in upper:
        return "INTEGER"
    if any(token in upper for token in ("CHAR", "CLOB", "TEXT")):
        return "TEXT"
    if "BLOB" in upper or not upper:
        return "BLOB"
    if any(token in upper for token in ("REAL", "FLOA", "DOUB")):
        return "REAL"
    return "NUMERIC"


def _dedupe_records(values: list[dict[str, Any]]) -> list[dict[str, Any]]:
    by_body = {canonical_bytes(value): value for value in values}
    return [by_body[body] for body in sorted(by_body)]


_SCHEMA_COUNT_LABELS = {
    "internal_tables": "internal-table",
    "schema_records": "schema-object",
    "table_objects": "table-object",
    "tables": "table",
    "columns": "column",
    "indexes": "index",
    "index_terms": "index-term",
    "foreign_keys": "foreign-key",
    "migrations": "migration",
}


def _schema_scan_limits(
    action_family: dict[str, Any],
    opening_family: dict[str, Any],
) -> dict[str, int]:
    """Derive hard scan ceilings from the already accepted action/opening pair."""
    source_schema = opening_family.get("source_schema")
    action_tables = action_family.get("tables")
    if not isinstance(source_schema, dict) or not isinstance(action_tables, list):
        raise BulkloadError("verifier SQLite schema limit authority is invalid")
    required_lists = {
        "schema_records": source_schema.get("schema_records"),
        "tables": source_schema.get("tables"),
        "views": source_schema.get("views"),
        "omitted_table_objects": source_schema.get("omitted_table_objects"),
        "classification_blockers": source_schema.get("classification_blockers"),
    }
    if any(not isinstance(value, list) for value in required_lists.values()):
        raise BulkloadError("verifier SQLite opening schema limits are invalid")

    source_tables = required_lists["tables"]
    table_names: list[str] = []
    columns = indexes = index_terms = foreign_keys = 0
    maximum_table_width = 0
    maximum_index_width = 0
    for table in source_tables:
        if not isinstance(table, dict):
            raise BulkloadError("verifier SQLite opening table limit is invalid")
        name = table.get("name")
        table_columns = table.get("columns")
        table_indexes = table.get("indexes")
        table_foreign_keys = table.get("foreign_keys")
        if (
            not isinstance(name, str)
            or _IDENTIFIER.fullmatch(name) is None
            or not isinstance(table_columns, list)
            or not isinstance(table_indexes, list)
            or not isinstance(table_foreign_keys, list)
        ):
            raise BulkloadError("verifier SQLite opening table limit is invalid")
        table_names.append(name)
        columns += len(table_columns)
        indexes += len(table_indexes)
        foreign_keys += len(table_foreign_keys)
        maximum_table_width = max(maximum_table_width, len(table_columns))
        for index in table_indexes:
            terms = index.get("terms") if isinstance(index, dict) else None
            if not isinstance(terms, list):
                raise BulkloadError("verifier SQLite opening index limit is invalid")
            index_terms += len(terms)
            maximum_index_width = max(maximum_index_width, len(terms))
    if table_names != sorted(set(table_names)):
        raise BulkloadError("verifier SQLite opening table limit is not canonical")

    action_table_names = [
        table.get("name") if isinstance(table, dict) else None
        for table in action_tables
    ]
    if (
        any(
            not isinstance(name, str) or _IDENTIFIER.fullmatch(name) is None
            for name in action_table_names
        )
        or action_table_names != sorted(set(action_table_names))
        or not set(action_table_names) <= set(table_names)
    ):
        raise BulkloadError("verifier SQLite action table limit is invalid")

    migration_relation = opening_family.get("migration_relation")
    if isinstance(migration_relation, dict):
        migration_count = migration_relation.get("source_count")
        if type(migration_count) is not int or migration_count < 0:
            raise BulkloadError("verifier SQLite opening migration limit is invalid")
    else:
        migration = action_family.get("migration")
        empty_migrations_sha256 = sha256_bytes(canonical_bytes([]))
        if (
            not isinstance(migration, dict)
            or migration.get("source_migrations_sha256") != empty_migrations_sha256
        ):
            raise BulkloadError("verifier SQLite opening migration limit is absent")
        # Internal hand-built zero-migration fixtures intentionally omit the
        # rest of the opening relation. Public inputs pass the full validator.
        migration_count = 0

    internal_tables = sum(
        int(
            isinstance(blocker, dict)
            and blocker.get("code") == "sqlite-internal-table-state-not-classified"
        )
        for blocker in required_lists["classification_blockers"]
    )
    counts = {
        "internal_tables": internal_tables,
        "schema_records": len(required_lists["schema_records"]),
        "table_objects": (
            len(source_tables)
            + len(required_lists["views"])
            + len(required_lists["omitted_table_objects"])
        ),
        "tables": len(source_tables),
        "columns": columns,
        "indexes": indexes,
        "index_terms": index_terms,
        "foreign_keys": foreign_keys,
        "migrations": migration_count,
    }
    if (
        counts["tables"] > MAX_TABLES
        or maximum_table_width > MAX_SQLITE_SCHEMA_COLUMNS
        or maximum_index_width > MAX_SQLITE_SCHEMA_COLUMNS
        or len(action_tables) > MAX_TABLES
        or any(value > MAX_SQLITE_SCHEMA_ACTIONS for value in counts.values())
        or sum(counts.values()) > MAX_SQLITE_SCHEMA_ACTIONS
    ):
        raise BulkloadError(
            "verifier SQLite accepted schema exceeds its structural action limit"
        )
    return {
        **counts,
        "actions": sum(counts.values()),
        "schema_sql_bytes": MAX_SQLITE_SCHEMA_SQL_BYTES,
        "migration_bytes": MAX_SQLITE_MIGRATION_BYTES,
    }


def _charge_schema_budget(
    budget: dict[str, int],
    limits: dict[str, int],
    key: str,
    amount: int = 1,
) -> None:
    if (
        key not in {*_SCHEMA_COUNT_LABELS, "schema_sql_bytes", "migration_bytes"}
        or type(amount) is not int
        or amount < 0
        or type(budget.get(key)) is not int
        or type(limits.get(key)) is not int
        or budget[key] < 0
        or limits[key] < 0
    ):
        raise BulkloadError("verifier SQLite schema budget state is invalid")
    budget[key] += amount
    if budget[key] > limits[key]:
        label = _SCHEMA_COUNT_LABELS.get(key, key.replace("_", "-"))
        raise BulkloadError(f"verifier SQLite {label} budget exceeded")
    if key in _SCHEMA_COUNT_LABELS:
        if (
            type(budget.get("actions")) is not int
            or type(limits.get("actions")) is not int
        ):
            raise BulkloadError("verifier SQLite schema budget state is invalid")
        budget["actions"] += amount
        if budget["actions"] > limits["actions"]:
            raise BulkloadError("verifier SQLite structural action budget exceeded")


def _charge_schema_text(
    value: Any,
    *,
    budget: dict[str, int],
    limits: dict[str, int],
    key: str,
    label: str,
    allow_none: bool = False,
) -> None:
    if value is None and allow_none:
        return
    if not isinstance(value, str):
        raise BulkloadError(f"{label} is not text")
    try:
        size = len(value.encode("utf-8"))
    except UnicodeEncodeError as error:
        raise BulkloadError(f"{label} is not UTF-8 encodable") from error
    _charge_schema_budget(budget, limits, key, size)


def _install_sqlite_schema_limits(
    connection: sqlite3.Connection,
) -> list[tuple[int, int]]:
    limits = (
        (
            sqlite3.SQLITE_LIMIT_LENGTH,
            _MAX_SQLITE_SCHEMA_FETCH_BYTES,
            "length",
        ),
        (
            sqlite3.SQLITE_LIMIT_SQL_LENGTH,
            _MAX_SQLITE_SCHEMA_QUERY_BYTES,
            "SQL length",
        ),
        (
            sqlite3.SQLITE_LIMIT_COLUMN,
            MAX_SQLITE_SCHEMA_COLUMNS,
            "column",
        ),
    )
    previous: list[tuple[int, int]] = []
    try:
        for category, maximum, label in limits:
            prior = connection.setlimit(category, maximum)
            previous.append((category, prior))
            if connection.getlimit(category) != maximum:
                raise BulkloadError(
                    f"verifier SQLite {label} limit could not be installed"
                )
        return previous
    except BaseException:
        for category, prior in reversed(previous):
            connection.setlimit(category, prior)
        raise


def _restore_sqlite_schema_limits(
    connection: sqlite3.Connection,
    previous: list[tuple[int, int]],
) -> None:
    for category, prior in reversed(previous):
        connection.setlimit(category, prior)


def _scan_schema_independently(
    connection: sqlite3.Connection,
    *,
    deadline: float,
    limits: dict[str, int],
) -> tuple[dict[str, Any], list[dict[str, Any]], list[list[Any]]]:
    """Reconstruct the schema contract without a producer implementation."""
    if sqlite3.sqlite_version_info < (3, 37, 0):
        raise BulkloadError("verifier requires SQLite 3.37.0 or newer")
    expected_limit_keys = {
        *_SCHEMA_COUNT_LABELS,
        "actions",
        "schema_sql_bytes",
        "migration_bytes",
    }
    if (
        set(limits) != expected_limit_keys
        or any(type(value) is not int or value < 0 for value in limits.values())
        or limits["actions"] != sum(limits[key] for key in _SCHEMA_COUNT_LABELS)
        or limits["actions"] > MAX_SQLITE_SCHEMA_ACTIONS
        or limits["schema_sql_bytes"] != MAX_SQLITE_SCHEMA_SQL_BYTES
        or limits["migration_bytes"] != MAX_SQLITE_MIGRATION_BYTES
    ):
        raise BulkloadError("verifier SQLite schema scan limits are invalid")
    blockers: list[dict[str, Any]] = []
    budget = {key: 0 for key in expected_limit_keys}

    def _interrupt() -> int:
        return int(time.monotonic() > deadline)

    prior_schema_limits = _install_sqlite_schema_limits(connection)
    progress_installed = False
    try:
        connection.set_progress_handler(_interrupt, 1000)
        progress_installed = True
        for (name,) in connection.execute(
            "SELECT name FROM sqlite_schema WHERE type='table' AND name LIKE 'sqlite_%'"
        ):
            _require_before_deadline(deadline, "verifier SQLite schema scan")
            _charge_schema_budget(budget, limits, "internal_tables")
            blockers.append(
                {
                    "code": "sqlite-internal-table-state-not-classified",
                    "name": str(name),
                }
            )
        schema_cursor = connection.execute(
            "SELECT type,name,tbl_name,sql FROM sqlite_schema "
            "WHERE name NOT LIKE 'sqlite_%'"
        )
        schema_records: list[dict[str, Any]] = []
        sql_by_object: dict[tuple[str, str], Any] = {}
        for object_type, name, table_name, sql in schema_cursor:
            _require_before_deadline(deadline, "verifier SQLite schema scan")
            _charge_schema_budget(budget, limits, "schema_records")
            _charge_schema_text(
                sql,
                budget=budget,
                limits=limits,
                key="schema_sql_bytes",
                label="verifier SQLite schema SQL",
                allow_none=True,
            )
            if (
                not isinstance(object_type, str)
                or not isinstance(name, str)
                or not isinstance(table_name, str)
                or _IDENTIFIER.fullmatch(name) is None
                or _IDENTIFIER.fullmatch(table_name) is None
            ):
                blockers.append({"code": "sqlite-unsupported-schema-identifier"})
                continue
            key = (object_type, name)
            if key in sql_by_object:
                blockers.append({"code": "sqlite-duplicate-schema-object"})
                continue
            sql_by_object[key] = sql
            schema_records.append(
                {
                    "type": object_type,
                    "name": name,
                    "table": table_name,
                    "sql_sha256": _schema_sql_digest(sql),
                }
            )
        schema_records.sort(key=lambda record: (record["type"], record["name"]))
        raw_schema_sha256 = sha256_bytes(canonical_bytes(schema_records))
        try:
            table_list = connection.execute(
                "SELECT schema,name,type,ncol,wr,strict "
                "FROM pragma_table_list "
                "WHERE schema='main' AND name NOT LIKE 'sqlite_%'"
            )
        except sqlite3.DatabaseError as error:
            raise BulkloadError(
                "verifier SQLite table-list authority is unavailable"
            ) from error
        tables: list[dict[str, Any]] = []
        table_names: list[str] = []
        omitted: list[dict[str, str]] = []
        collations: set[str] = set()
        for (
            schema_name,
            name,
            object_type,
            column_count,
            without_rowid,
            strict,
        ) in table_list:
            _require_before_deadline(deadline, "verifier SQLite schema scan")
            _charge_schema_budget(budget, limits, "table_objects")
            if (
                schema_name != "main"
                or not isinstance(name, str)
                or _IDENTIFIER.fullmatch(name) is None
                or object_type not in {"table", "view", "virtual", "shadow"}
            ):
                blockers.append({"code": "sqlite-unsupported-table-object"})
                continue
            if object_type in {"virtual", "shadow"}:
                omitted.append({"name": name, "type": object_type})
                blockers.append(
                    {
                        "code": "sqlite-virtual-or-shadow-table-unsupported",
                        "name": name,
                        "type": object_type,
                    }
                )
                continue
            if object_type == "view":
                continue
            _charge_schema_budget(budget, limits, "tables")
            create_sql = sql_by_object.get(("table", name))
            if _sql_contains_keyword(create_sql, "COLLATE"):
                blockers.append(
                    {
                        "code": "sqlite-column-collation-semantics-not-classified",
                        "table": name,
                    }
                )
            columns: list[dict[str, Any]] = []
            for (
                column_id,
                column_name,
                declared_type,
                not_null,
                default,
                primary_key,
                hidden,
            ) in connection.execute(
                'SELECT cid,name,type,"notnull",dflt_value,pk,hidden '
                "FROM pragma_table_xinfo(?)",
                (name,),
            ):
                _require_before_deadline(deadline, "verifier SQLite column scan")
                _charge_schema_budget(budget, limits, "columns")
                _charge_schema_text(
                    default,
                    budget=budget,
                    limits=limits,
                    key="schema_sql_bytes",
                    label="verifier SQLite column default SQL",
                    allow_none=True,
                )
                if (
                    not isinstance(column_name, str)
                    or _IDENTIFIER.fullmatch(column_name) is None
                    or hidden not in {0, 1, 2, 3}
                ):
                    blockers.append(
                        {"code": "sqlite-unsupported-column", "table": name}
                    )
                    continue
                declared = str(declared_type or "")
                columns.append(
                    {
                        "cid": int(column_id),
                        "name": column_name,
                        "declared_type": declared,
                        "affinity": _declared_affinity(declared),
                        "not_null": bool(not_null),
                        "default_sql_sha256": _schema_sql_digest(default),
                        "primary_key_position": int(primary_key),
                        "hidden": int(hidden),
                        "generated_kind": {
                            0: "none",
                            1: "hidden",
                            2: "virtual",
                            3: "stored",
                        }[int(hidden)],
                    }
                )
            columns.sort(key=lambda column: column["cid"])
            indexes: list[dict[str, Any]] = []
            for _, index_name, unique, origin, partial in connection.execute(
                'SELECT seq,name,"unique",origin,partial FROM pragma_index_list(?)',
                (name,),
            ):
                _require_before_deadline(deadline, "verifier SQLite index scan")
                _charge_schema_budget(budget, limits, "indexes")
                if (
                    not isinstance(index_name, str)
                    or _IDENTIFIER.fullmatch(index_name) is None
                ):
                    blockers.append({"code": "sqlite-unsupported-index", "table": name})
                    continue
                terms: list[dict[str, Any]] = []
                for (
                    sequence,
                    column_id,
                    term_name,
                    descending,
                    collation,
                    key_term,
                ) in connection.execute(
                    'SELECT seqno,cid,name,"desc",coll,key FROM pragma_index_xinfo(?)',
                    (index_name,),
                ):
                    _require_before_deadline(
                        deadline,
                        "verifier SQLite index-term scan",
                    )
                    _charge_schema_budget(budget, limits, "index_terms")
                    if term_name is not None and (
                        not isinstance(term_name, str)
                        or _IDENTIFIER.fullmatch(term_name) is None
                    ):
                        blockers.append(
                            {
                                "code": "sqlite-unsupported-index-term",
                                "table": name,
                            }
                        )
                    if collation is not None:
                        collations.add(str(collation))
                    terms.append(
                        {
                            "sequence": int(sequence),
                            "cid": int(column_id),
                            "name": term_name,
                            "descending": bool(descending),
                            "collation": collation,
                            "key": bool(key_term),
                        }
                    )
                terms.sort(key=lambda term: term["sequence"])
                indexes.append(
                    {
                        "name": index_name,
                        "unique": bool(unique),
                        "origin": str(origin),
                        "partial": bool(partial),
                        "terms": terms,
                        "create_sql_sha256": _schema_sql_digest(
                            sql_by_object.get(("index", index_name))
                        ),
                    }
                )
            indexes.sort(key=lambda index: index["name"])
            foreign_keys: list[dict[str, Any]] = []
            for (
                foreign_id,
                sequence,
                referenced_table,
                from_column,
                to_column,
                on_update,
                on_delete,
                match,
            ) in connection.execute(
                'SELECT id,seq,"table","from","to",on_update,on_delete,match '
                "FROM pragma_foreign_key_list(?)",
                (name,),
            ):
                _require_before_deadline(
                    deadline,
                    "verifier SQLite foreign-key scan",
                )
                _charge_schema_budget(budget, limits, "foreign_keys")
                supported = bool(
                    isinstance(referenced_table, str)
                    and _IDENTIFIER.fullmatch(referenced_table)
                    and isinstance(from_column, str)
                    and _IDENTIFIER.fullmatch(from_column)
                    and isinstance(to_column, str)
                    and _IDENTIFIER.fullmatch(to_column)
                    and all(
                        isinstance(action, str) and action
                        for action in (on_update, on_delete, match)
                    )
                )
                if not supported:
                    blockers.append(
                        {
                            "code": "sqlite-unsupported-foreign-key",
                            "table": name,
                            "id": int(foreign_id),
                        }
                    )
                foreign_keys.append(
                    {
                        "id": int(foreign_id),
                        "sequence": int(sequence),
                        "referenced_table": (
                            referenced_table
                            if isinstance(referenced_table, str)
                            else ""
                        ),
                        "from_column": (
                            from_column if isinstance(from_column, str) else ""
                        ),
                        "to_column": (to_column if isinstance(to_column, str) else ""),
                        "on_update": (on_update if isinstance(on_update, str) else ""),
                        "on_delete": (on_delete if isinstance(on_delete, str) else ""),
                        "match": match if isinstance(match, str) else "",
                        "supported": supported,
                    }
                )
            foreign_keys.sort(
                key=lambda foreign_key: (
                    foreign_key["id"],
                    foreign_key["sequence"],
                )
            )
            tables.append(
                {
                    "name": name,
                    "type": "table",
                    "ncol": int(column_count),
                    "without_rowid": bool(without_rowid),
                    "strict": bool(strict),
                    "create_sql_sha256": _schema_sql_digest(create_sql),
                    "columns": columns,
                    "indexes": indexes,
                    "foreign_keys": foreign_keys,
                }
            )
            table_names.append(name)
        tables.sort(key=lambda table: table["name"])
        omitted.sort(key=lambda record: (record["name"], record["type"]))
        triggers: list[dict[str, Any]] = []
        views: list[dict[str, Any]] = []
        for record in schema_records:
            if record["type"] == "trigger":
                triggers.append(
                    {
                        "name": record["name"],
                        "table": record["table"],
                        "sql_sha256": record["sql_sha256"],
                    }
                )
            elif record["type"] == "view":
                views.append(
                    {
                        "name": record["name"],
                        "sql_sha256": record["sql_sha256"],
                    }
                )
        migrations: list[list[Any]] = []
        migration_unsuccessful = False
        if "_sqlx_migrations" in table_names:
            for (
                version,
                version_type,
                description,
                description_type,
                checksum_hex,
                checksum_type,
                success,
                success_type,
            ) in connection.execute(
                "SELECT version,typeof(version),description,"
                "typeof(description),hex(checksum),typeof(checksum),"
                "success,typeof(success) FROM _sqlx_migrations"
            ):
                _require_before_deadline(deadline, "verifier SQLite migration scan")
                _charge_schema_budget(budget, limits, "migrations")
                for value in (description, checksum_hex):
                    if isinstance(value, str):
                        _charge_schema_text(
                            value,
                            budget=budget,
                            limits=limits,
                            key="migration_bytes",
                            label="verifier SQLite migration value",
                        )
                    elif isinstance(value, bytes):
                        _charge_schema_budget(
                            budget,
                            limits,
                            "migration_bytes",
                            len(value),
                        )
                if (
                    version_type != "integer"
                    or type(version) is not int
                    or description_type != "text"
                    or not isinstance(description, str)
                    or checksum_type != "blob"
                    or not isinstance(checksum_hex, str)
                    or success_type != "integer"
                    or type(success) is not int
                    or success not in {0, 1}
                ):
                    blockers.append({"code": "sqlite-migration-record-type-invalid"})
                    continue
                migration_unsuccessful |= success != 1
                migrations.append([version, description, checksum_hex, success == 1])
            migrations.sort(key=lambda migration: migration[0])
            migration_order_invalid = any(
                current[0] <= previous[0]
                for previous, current in zip(
                    migrations,
                    migrations[1:],
                    strict=False,
                )
            )
            if migration_order_invalid:
                blockers.append({"code": "sqlite-migrations-not-strictly-ordered"})
            if migration_unsuccessful:
                blockers.append({"code": "sqlite-migration-not-successful"})
        blockers = _dedupe_records(blockers)
        contract: dict[str, Any] = {
            "application_id": int(
                connection.execute("PRAGMA application_id").fetchone()[0]
            ),
            "user_version": int(
                connection.execute("PRAGMA user_version").fetchone()[0]
            ),
            "raw_schema_sha256": raw_schema_sha256,
            "schema_records": schema_records,
            "omitted_table_objects": omitted,
            "classification_blockers": blockers,
            "tables": tables,
            "triggers": triggers,
            "views": views,
            "collations": sorted(collations),
        }
        contract["schema_contract_sha256"] = sha256_bytes(canonical_bytes(contract))
        return contract, blockers, migrations
    except sqlite3.Error as error:
        if time.monotonic() > deadline or "interrupted" in str(error).casefold():
            raise BulkloadError(
                "verifier SQLite schema scan exceeded its deadline"
            ) from error
        raise BulkloadError("verifier SQLite schema scan failed") from error
    finally:
        if progress_installed:
            connection.set_progress_handler(None, 0)
        _restore_sqlite_schema_limits(connection, prior_schema_limits)


def _typed_value(value_type: str, value: Any) -> bytes:
    """Independent implementation of the protocol's typed-value encoding."""
    header, payload = _typed_value_parts(value_type, value)
    return header + payload


def _typed_value_parts(value_type: str, value: Any) -> tuple[bytes, bytes]:
    """Return the bounded typed-value header and payload without joining them."""
    if value_type == "null" and value is None:
        tag, payload = b"N", b""
    elif value_type == "integer" and type(value) is int:
        try:
            payload = struct.pack(">q", value)
        except struct.error as error:
            raise BulkloadError(
                "verifier SQLite INTEGER is outside signed 64-bit range"
            ) from error
        tag = b"I"
    elif value_type == "real" and type(value) is float:
        payload = struct.pack(">d", value)
        tag = b"R"
    elif value_type == "text" and isinstance(value, str):
        payload = value.encode("utf-8")
        tag = b"T"
    elif value_type == "blob" and isinstance(value, bytes):
        payload = value
        tag = b"B"
    else:
        raise BulkloadError("verifier SQLite value/type contract is invalid")
    if len(payload) > MAX_SQLITE_VALUE_BYTES:
        raise BulkloadError("verifier SQLite value exceeds its byte budget")
    return tag + len(payload).to_bytes(8, "big"), payload


def _sqlite_identity_sort_payload(
    value_type: str,
    value: Any,
    typed_payload: bytes,
    *,
    text_encoding: str,
) -> bytes:
    """Match hex(CAST(value AS BLOB)) ordering without asking SQLite to sort."""
    if value_type == "null" and value is None:
        return b""
    if value_type == "integer" and type(value) is int:
        return str(value).encode("ascii")
    if value_type == "text" and isinstance(value, str):
        try:
            return value.encode(text_encoding)
        except UnicodeError as error:
            raise BulkloadError(
                "verifier SQLite identity text encoding is invalid"
            ) from error
    if value_type == "blob":
        return typed_payload
    raise BulkloadError("verifier SQLite identity sort type is invalid")


def _charge_semantic_budget(budget: dict[str, int], row_bytes: int) -> None:
    if (
        set(budget) != {"rows", "bytes"}
        or type(budget["rows"]) is not int
        or type(budget["bytes"]) is not int
        or budget["rows"] < 0
        or budget["bytes"] < 0
        or type(row_bytes) is not int
        or row_bytes < 0
    ):
        raise BulkloadError("verifier semantic budget state is invalid")
    budget["rows"] += 1
    budget["bytes"] += row_bytes
    if (
        budget["rows"] > MAX_SQLITE_PLAN_ROWS
        or budget["bytes"] > MAX_SQLITE_PLAN_ROW_BYTES
    ):
        raise BulkloadError("verifier semantic budget exceeded")


def _scan_table(
    connection: sqlite3.Connection,
    *,
    basename: str,
    table_contract: dict[str, Any],
    action_table: dict[str, Any],
    deadline: float,
    budget: dict[str, int],
) -> dict[str, Any]:
    table_name = action_table["name"]
    columns = [column["name"] for column in table_contract["columns"]]
    identities = action_table["identity_columns"]
    if (
        not columns
        or not identities
        or not set(identities) <= set(columns)
        or any(_IDENTIFIER.fullmatch(item) is None for item in columns)
        or _IDENTIFIER.fullmatch(table_name) is None
    ):
        raise BulkloadError("verifier table contract is invalid")
    select_parts: list[str] = []
    for column in columns:
        quoted = _quote_identifier(column)
        select_parts.extend((f"typeof({quoted})", quoted))
    query = f"SELECT {', '.join(select_parts)} FROM {_quote_identifier(table_name)}"
    row_prefix = canonical_bytes(
        {"basename": basename, "table": table_name, "columns": columns}
    )
    count = 0
    classified_bytes = 0
    retained_identity_bytes = 0
    retained_rows: list[
        tuple[
            tuple[tuple[str, bytes], ...],
            tuple[tuple[bytes, bytes], ...],
            bytes,
            int,
        ]
    ] = []
    # sqlite3 materializes a complete result record before Python can inspect
    # it. Bound that record independently of table width. The 1 MiB allowance
    # admits SQLite record metadata around one exact-bound protocol value.
    fetch_limit = MAX_SQLITE_VALUE_BYTES + _SQLITE_FETCH_OVERHEAD_BYTES
    prior_limit = connection.setlimit(
        sqlite3.SQLITE_LIMIT_LENGTH,
        fetch_limit,
    )

    def _interrupt() -> int:
        return int(time.monotonic() > deadline)

    connection.set_progress_handler(_interrupt, 1000)
    try:
        encoding_row = connection.execute("PRAGMA encoding").fetchone()
        if (
            encoding_row is None
            or len(encoding_row) != 1
            or encoding_row[0] not in {"UTF-8", "UTF-16le", "UTF-16be"}
        ):
            raise BulkloadError("verifier SQLite encoding is unsupported")
        text_encoding = {
            "UTF-8": "utf-8",
            "UTF-16le": "utf-16le",
            "UTF-16be": "utf-16be",
        }[encoding_row[0]]
        for row in connection.execute(query):
            if time.monotonic() > deadline:
                raise BulkloadError("verifier table scan exceeded its deadline")
            values = {
                column: (str(row[index * 2]), row[index * 2 + 1])
                for index, column in enumerate(columns)
            }
            if any(values[column][0] == "real" for column in identities):
                raise BulkloadError("verifier REAL identity is forbidden")
            typed_values = {
                column: _typed_value_parts(*values[column]) for column in columns
            }
            identity_parts = tuple(typed_values[column] for column in identities)
            identity_size = sum(
                len(header) + len(payload) for header, payload in identity_parts
            )
            sort_key = tuple(
                (
                    values[column][0],
                    _sqlite_identity_sort_payload(
                        values[column][0],
                        values[column][1],
                        typed_values[column][1],
                        text_encoding=text_encoding,
                    ),
                )
                for column in identities
            )
            row_hasher = hashlib.sha256()
            row_hasher.update(row_prefix)
            for column in columns:
                header, payload = typed_values[column]
                row_hasher.update(header)
                row_hasher.update(payload)
            row_digest = row_hasher.digest()
            row_bytes = (
                identity_size
                + len(row_prefix)
                + sum(
                    len(header) + len(payload)
                    for header, payload in typed_values.values()
                )
            )
            count += 1
            classified_bytes += row_bytes
            _charge_semantic_budget(budget, row_bytes)
            retained_identity_bytes += identity_size + sum(
                len(payload) for _, payload in sort_key
            )
            if (
                count > MAX_VERIFIER_RETAINED_ROWS
                or retained_identity_bytes > MAX_VERIFIER_RETAINED_IDENTITY_BYTES
            ):
                raise BulkloadError(
                    "verifier retained identity ordering budget exceeded"
                )
            retained_rows.append((sort_key, identity_parts, row_digest, row_bytes))
    except sqlite3.Error as error:
        if time.monotonic() > deadline or "interrupted" in str(error).casefold():
            raise BulkloadError("verifier table scan exceeded its deadline") from error
        raise BulkloadError("verifier table scan failed") from error
    finally:
        connection.set_progress_handler(None, 0)
        connection.setlimit(sqlite3.SQLITE_LIMIT_LENGTH, prior_limit)
    _require_before_deadline(deadline, "verifier table ordering")
    retained_rows.sort(key=lambda row: row[0])
    _require_before_deadline(deadline, "verifier table ordering")
    digest = hashlib.sha256()
    digest.update(canonical_bytes({"table": table_name, "columns": columns}))
    previous_identity: tuple[tuple[bytes, bytes], ...] | None = None
    previous_sort_key: tuple[tuple[str, bytes], ...] | None = None
    for sort_key, identity_parts, row_digest, _ in retained_rows:
        if identity_parts == previous_identity:
            raise BulkloadError("verifier identity is duplicated")
        if sort_key == previous_sort_key and identity_parts != previous_identity:
            raise BulkloadError("verifier identity sort key is ambiguous")
        identity_size = sum(
            len(header) + len(payload) for header, payload in identity_parts
        )
        digest.update(identity_size.to_bytes(8, "big"))
        for header, payload in identity_parts:
            digest.update(header)
            digest.update(payload)
        digest.update(row_digest)
        previous_identity = identity_parts
        previous_sort_key = sort_key
    return {
        "name": table_name,
        "row_count": count,
        "classified_bytes": classified_bytes,
        "semantic_rows_sha256": digest.hexdigest(),
    }


def _observed_edges(schema: dict[str, Any]) -> list[dict[str, Any]]:
    rules: list[dict[str, Any]] = []
    for table in schema["tables"]:
        groups: dict[int, list[dict[str, Any]]] = {}
        for foreign_key in table["foreign_keys"]:
            groups.setdefault(foreign_key["id"], []).append(foreign_key)
        for foreign_id, records in sorted(groups.items()):
            ordered = sorted(records, key=lambda record: record["sequence"])
            if (
                [record["sequence"] for record in ordered] != list(range(len(ordered)))
                or not all(record["supported"] for record in ordered)
                or len(
                    {
                        (
                            record["referenced_table"],
                            record["on_update"],
                            record["on_delete"],
                            record["match"],
                        )
                        for record in ordered
                    }
                )
                != 1
            ):
                raise BulkloadError("verifier foreign-key shape is unsupported")
            first = ordered[0]
            rules.append(
                {
                    "table": table["name"],
                    "from_columns": [record["from_column"] for record in ordered],
                    "referenced_table": first["referenced_table"],
                    "referenced_columns": [record["to_column"] for record in ordered],
                    "on_update": first["on_update"],
                    "on_delete": first["on_delete"],
                    "match": first["match"],
                    "required": True,
                }
            )
    return sorted(rules, key=canonical_bytes)


def _sqlite_engine_authority(connection: sqlite3.Connection) -> dict[str, Any]:
    compile_options = sorted(
        str(row[0]) for row in connection.execute("PRAGMA compile_options")
    )
    source_id = connection.execute("SELECT sqlite_source_id()").fetchone()
    if (
        source_id is None
        or len(source_id) != 1
        or not isinstance(source_id[0], str)
        or not source_id[0]
    ):
        raise BulkloadError("SQLite source ID is unavailable")
    threadsafe_options = [
        option for option in compile_options if option.startswith("THREADSAFE=")
    ]
    if len(threadsafe_options) != 1:
        raise BulkloadError("SQLite threadsafe compile authority is unavailable")
    try:
        threadsafe = int(threadsafe_options[0].split("=", 1)[1])
    except ValueError as error:
        raise BulkloadError("SQLite threadsafe compile authority is invalid") from error
    if threadsafe not in {0, 1, 2}:
        raise BulkloadError("SQLite threadsafe compile authority is invalid")
    return {
        "sqlite_version": sqlite3.sqlite_version,
        "sqlite_source_id": source_id[0],
        "compile_options": compile_options,
        "compile_options_sha256": sha256_bytes(canonical_bytes(compile_options)),
        "threadsafe": threadsafe,
    }


def _artifact_binding(
    value: dict[str, Any],
    *,
    schema_field: str,
    digest_field: str,
) -> dict[str, Any]:
    payload = canonical_bytes(value) + b"\n"
    return {
        "schema": value[schema_field],
        digest_field: value[digest_field],
        "canonical_file_sha256": sha256_bytes(payload),
        "canonical_file_bytes": len(payload),
    }


def _failure(
    code: str,
    *,
    scope: str,
    family: str | None = None,
    field: str | None = None,
) -> dict[str, str | None]:
    if (
        not isinstance(code, str)
        or not code
        or len(code) > 96
        or re.fullmatch(r"[a-z0-9][a-z0-9-]*", code) is None
    ):
        raise BulkloadError("verifier failure code is invalid")
    return {
        "code": code,
        "scope": scope,
        "family": family,
        "field": field,
    }


def _dedupe_failures(
    values: list[dict[str, str | None]],
) -> list[dict[str, str | None]]:
    by_body = {canonical_bytes(value): value for value in values}
    result = [by_body[key] for key in sorted(by_body)]
    if len(result) > MAX_FAILURES:
        raise BulkloadError("verifier failure budget exceeded")
    return result


def _verify_input_bindings(
    action_plan: dict[str, Any],
    opening_plan: dict[str, Any],
    compose_request: dict[str, Any],
    capacity_observation: dict[str, Any],
) -> None:
    validate_codex_private_sqlite_compose_plan(opening_plan)
    validate_codex_private_sqlite_action_plan(action_plan)
    validate_codex_private_sqlite_compose_request(compose_request)
    validate_codex_private_sqlite_capacity_observation(capacity_observation)
    if (
        action_plan["schema"] != PRIVATE_SQLITE_ACTION_PLAN_SCHEMA
        or action_plan["runtime_authority"]
        != private_runtime.LEGACY_PRIVATE_RUNTIME_AUTHORITY_V5_REPAIRED
    ):
        raise BulkloadError("verifier requires the exact repaired v5 action")
    expected_v6 = private_runtime.LEGACY_PRIVATE_RUNTIME_AUTHORITY_V6
    if (
        compose_request["schema"] != PRIVATE_SQLITE_COMPOSE_REQUEST_SCHEMA
        or compose_request["request_runtime_authority"] != expected_v6
        or compose_request["action_plan_producer_runtime_authority"]
        != private_runtime.LEGACY_PRIVATE_RUNTIME_AUTHORITY_V5_REPAIRED
        or capacity_observation["schema"] != PRIVATE_SQLITE_CAPACITY_OBSERVATION_SCHEMA
        or capacity_observation["observation_runtime_authority"] != expected_v6
    ):
        raise BulkloadError("verifier requires the exact v6 request authority")
    workspace = compose_request["output_intent"]["workspace"]
    validate_codex_private_sqlite_compose_request_against_action(
        compose_request,
        action_plan,
        workspace,
        expected_v6,
    )
    validate_codex_private_sqlite_capacity_observation_against_request(
        capacity_observation,
        compose_request,
        workspace,
        expected_v6,
    )
    action_binding = compose_request["action_plan"]
    if action_binding["action_plan_sha256"] != action_plan[
        "action_plan_sha256"
    ] or action_binding["body_sha256"] != sha256_bytes(canonical_bytes(action_plan)):
        raise BulkloadError("verifier request action binding differs")
    opening_binding = action_plan["accepted_inputs"]["opening_plan"]
    if (
        opening_binding["plan_sha256"] != opening_plan["plan_sha256"]
        or opening_binding["body_sha256"] != sha256_bytes(canonical_bytes(opening_plan))
        or opening_binding["accepted_inputs_sha256"]
        != sha256_bytes(canonical_bytes(opening_plan["accepted_inputs"]))
        or capacity_observation["claims"]["capacity_sufficient_observed"] is not True
    ):
        raise BulkloadError("verifier opening or capacity authority differs")


def _expected_manifest_inputs(
    action_plan: dict[str, Any],
    compose_request: dict[str, Any],
    capacity_observation: dict[str, Any],
) -> dict[str, Any]:
    return {
        "action_plan": _artifact_binding(
            action_plan,
            schema_field="schema",
            digest_field="action_plan_sha256",
        ),
        "compose_request": _artifact_binding(
            compose_request,
            schema_field="schema",
            digest_field="request_sha256",
        ),
        "capacity_observation": _artifact_binding(
            capacity_observation,
            schema_field="schema",
            digest_field="observation_sha256",
        ),
    }


def _snapshot_input_documents(
    values: tuple[tuple[str, dict[str, Any]], ...],
    *,
    deadline: float,
) -> tuple[tuple[dict[str, Any], ...], tuple[bytes, ...]]:
    snapshots: list[dict[str, Any]] = []
    bodies: list[bytes] = []
    cumulative_bytes = 0
    for label, value in values:
        _require_before_deadline(deadline, "verifier input snapshot")
        try:
            before = canonical_bytes(value)
        except (RuntimeError, TypeError, ValueError) as error:
            raise BulkloadError(f"{label} cannot be snapshotted") from error
        cumulative_bytes += len(before)
        if cumulative_bytes > MAX_PROTOCOL_BYTES:
            raise BulkloadError(
                "verifier input documents exceed their cumulative byte budget"
            )
        try:
            snapshot = deepcopy(value)
            after = canonical_bytes(value)
            snapshot_body = canonical_bytes(snapshot)
        except (RuntimeError, TypeError, ValueError) as error:
            raise BulkloadError(f"{label} cannot be snapshotted") from error
        if before != after or after != snapshot_body:
            raise BulkloadError(f"{label} changed while snapshotting")
        _require_before_deadline(deadline, "verifier input snapshot")
        snapshots.append(snapshot)
        bodies.append(snapshot_body)
    return tuple(snapshots), tuple(bodies)


def _revalidate_input_documents(
    values: tuple[tuple[str, dict[str, Any]], ...],
    snapshots: tuple[dict[str, Any], ...],
    bodies: tuple[bytes, ...],
    *,
    deadline: float,
) -> None:
    if len(values) != len(snapshots) or len(values) != len(bodies):
        raise BulkloadError("verifier input snapshot inventory differs")
    for (label, value), snapshot, body in zip(
        values,
        snapshots,
        bodies,
        strict=True,
    ):
        _require_before_deadline(deadline, "verifier input revalidation")
        try:
            snapshot_body = canonical_bytes(snapshot)
            current_body = canonical_bytes(value)
        except (RuntimeError, TypeError, ValueError) as error:
            raise BulkloadError(f"{label} changed during observation") from error
        if snapshot_body != body:
            raise BulkloadError(f"{label} snapshot changed during observation")
        if current_body != body:
            raise BulkloadError(f"{label} changed during observation")
    _require_before_deadline(deadline, "verifier input revalidation")


def observe_codex_private_sqlite_bundle(
    bundle_path: Path,
    action_plan: dict[str, Any],
    opening_plan: dict[str, Any],
    compose_request: dict[str, Any],
    capacity_observation: dict[str, Any],
    *,
    verifier_runtime_authority: dict[str, Any],
) -> dict[str, Any]:
    """Observe an offline bundle without issuing verification authority."""
    deadline = time.monotonic() + MAX_SQLITE_PLAN_SECONDS
    input_values = (
        ("action plan", action_plan),
        ("opening plan", opening_plan),
        ("compose request", compose_request),
        ("capacity observation", capacity_observation),
        ("verifier runtime authority", verifier_runtime_authority),
    )
    snapshots, snapshot_bodies = _snapshot_input_documents(
        input_values,
        deadline=deadline,
    )
    (
        action_snapshot,
        opening_snapshot,
        request_snapshot,
        capacity_snapshot,
        runtime_snapshot,
    ) = snapshots
    _verify_input_bindings(
        action_snapshot,
        opening_snapshot,
        request_snapshot,
        capacity_snapshot,
    )
    _require_before_deadline(deadline, "verifier input validation")
    private_runtime.validate_private_runtime_authority(runtime_snapshot)
    _require_before_deadline(deadline, "verifier runtime validation")
    identifier = str(uuid.uuid4())
    _require_uuid(identifier, "oracle observation ID")
    timestamp = utc_now()
    _parse_utc(timestamp, "oracle observation timestamp")
    with private_runtime.open_pinned_private_runtime_authority(
        runtime_snapshot
    ) as pinned_runtime:
        source_sha256 = runtime_snapshot["source_digests"].get(
            PRIVATE_SQLITE_VERIFIER_SOURCE_PATH
        )
        if source_sha256 is None:
            raise BulkloadError(
                "active runtime does not bind the verifier oracle source"
            )
        report = _observe_codex_private_sqlite_bundle(
            bundle_path,
            action_snapshot,
            opening_snapshot,
            request_snapshot,
            capacity_snapshot,
            verifier_runtime_binding={
                "runtime_authority": deepcopy(runtime_snapshot),
                "source_path": PRIVATE_SQLITE_VERIFIER_SOURCE_PATH,
                "source_sha256": source_sha256,
            },
            observation_id=identifier,
            observed_at=timestamp,
            deadline=deadline,
        )
        pinned_runtime.revalidate()
        _revalidate_input_documents(
            input_values,
            snapshots,
            snapshot_bodies,
            deadline=deadline,
        )
        return report


def _observe_codex_private_sqlite_bundle(
    bundle_path: Path,
    action_plan: dict[str, Any],
    opening_plan: dict[str, Any],
    compose_request: dict[str, Any],
    capacity_observation: dict[str, Any],
    *,
    verifier_runtime_binding: dict[str, Any],
    observation_id: str,
    observed_at: str,
    deadline: float | None = None,
) -> dict[str, Any]:
    deadline = (
        time.monotonic() + MAX_SQLITE_PLAN_SECONDS if deadline is None else deadline
    )
    _require_before_deadline(deadline, "verifier bundle observation")
    failures: list[dict[str, str | None]] = []
    normalized, parent_descriptor, parent_info = _open_absolute_parent(bundle_path)
    bundle_descriptor = sqlite_descriptor = -1
    manifest_descriptor = receipt_descriptor = -1
    database_descriptors: list[tuple[int, os.stat_result, str]] = []
    family_results: list[dict[str, Any]] = []
    try:
        workspace = compose_request["output_intent"]["workspace"]
        parent_identity = _identity(parent_info)
        parent_mount = _mount(parent_descriptor)
        parent_lineage = _lineage(parent_descriptor)
        if (
            os.fspath(normalized.parent) != workspace["resolved_parent"]
            or normalized.name != workspace["final_leaf"]
            or parent_identity != workspace["parent_identity"]
            or parent_mount != workspace["mount"]
            or parent_lineage != workspace["lineage"]
            or sha256_bytes(canonical_bytes(parent_lineage))
            != workspace["lineage_sha256"]
        ):
            raise BulkloadError("offline bundle candidate workspace binding differs")
        bundle_descriptor, bundle_info = _open_private_directory_at(
            parent_descriptor,
            normalized.name,
            label="offline bundle candidate",
        )
        bundle_mount = _mount(bundle_descriptor)
        if bundle_mount != parent_mount:
            raise BulkloadError(
                "offline bundle candidate crosses the pinned workspace mount"
            )
        entries = _bounded_directory_entries(
            bundle_descriptor,
            label="offline bundle candidate",
            deadline=deadline,
        )
        if entries != ["composition-receipt.json", "manifest.json", "sqlite"]:
            raise BulkloadError("offline bundle candidate inventory differs")
        manifest, manifest_payload, manifest_info, manifest_descriptor = (
            _read_private_json_at(
                bundle_descriptor,
                "manifest.json",
                label="offline bundle candidate manifest",
            )
        )
        receipt, receipt_payload, receipt_info, receipt_descriptor = (
            _read_private_json_at(
                bundle_descriptor,
                "composition-receipt.json",
                label="offline bundle candidate composition receipt",
            )
        )
        if (
            _mount(manifest_descriptor) != parent_mount
            or _mount(receipt_descriptor) != parent_mount
        ):
            raise BulkloadError(
                "offline bundle candidate metadata crosses the pinned mount"
            )
        validate_composed_bundle_manifest(manifest)
        validate_composition_receipt(receipt)
        expected_inputs = _expected_manifest_inputs(
            action_plan,
            compose_request,
            capacity_observation,
        )
        if (
            manifest["accepted_inputs"] != expected_inputs
            or receipt["accepted_inputs"] != expected_inputs
        ):
            failures.append(
                _failure(
                    "accepted-artifact-binding-differs",
                    scope="input-chain",
                )
            )
        if manifest["producer_lineage"] != receipt["producer_lineage"]:
            failures.append(_failure("producer-lineage-differs", scope="input-chain"))
        if (
            manifest["composition_id"] != receipt["composition_id"]
            or manifest["sqlite_engine_authority"] != receipt["sqlite_engine_authority"]
        ):
            failures.append(
                _failure(
                    "manifest-receipt-identity-differs",
                    scope="composition-receipt",
                )
            )
        if receipt["manifest"] != {
            "schema": COMPOSED_BUNDLE_MANIFEST_SCHEMA,
            "manifest_sha256": manifest["manifest_sha256"],
            "canonical_file_sha256": sha256_bytes(manifest_payload),
            "canonical_file_bytes": len(manifest_payload),
        }:
            failures.append(
                _failure(
                    "composition-manifest-binding-differs",
                    scope="composition-receipt",
                )
            )
        graph_digest = sha256_bytes(canonical_bytes(action_plan["operation_graph"]))
        expected_completed = sorted(
            node["id"]
            for node in action_plan["operation_graph"]["nodes"]
            if node["id"] not in PRESEAL_RECEIPT_EXCLUDED_NODE_IDS
        )
        if (
            receipt["operation_graph"]["action_plan_operation_graph_sha256"]
            != graph_digest
            or receipt["operation_graph"]["completed_node_ids"] != expected_completed
        ):
            failures.append(
                _failure(
                    "operation-graph-binding-differs",
                    scope="composition-receipt",
                )
            )
        expected_basenames = sorted(
            family["basename"] for family in action_plan["sqlite_families"]
        )
        manifest_workspace = manifest["workspace"]
        if (
            manifest_workspace["resolved_parent"] != workspace["resolved_parent"]
            or manifest_workspace["parent_identity"] != parent_identity
            or manifest_workspace["mount"] != parent_mount
            or manifest_workspace["lineage"] != parent_lineage
            or manifest_workspace["lineage_sha256"] != workspace["lineage_sha256"]
            or manifest_workspace["final_leaf"] != normalized.name
            or manifest_workspace["staging_identity"] != _identity(bundle_info)
            or not manifest_workspace["staging_leaf"].startswith(
                workspace["staging_prefix"]
            )
        ):
            failures.append(
                _failure(
                    "manifest-workspace-binding-differs",
                    scope="workspace",
                )
            )
        claim = receipt["workspace_claim"]
        if (
            claim["workspace"] != manifest_workspace
            or claim["cooperating_lock_acquired"] is not True
            or claim["exclusive_staging_claimed"] is not True
            or claim["final_leaf_observed_absent"] is not True
            or claim["same_parent_staging"] is not True
            or claim["ticket_consumed"] is not True
            or claim["failure_policy"] != "preserve-fail-held-no-cleanup-v1"
        ):
            failures.append(
                _failure(
                    "receipt-workspace-claim-differs",
                    scope="workspace",
                )
            )
        capacity = receipt["capacity_admission"]
        expires = _parse_utc(
            capacity["expires_at"],
            "composition capacity observation expiry",
        )
        observations = capacity["observations"]
        expected_capacity_sequence = [
            ("pre-staging", None),
            ("post-staging-pre-ticket", None),
            *[
                phase
                for basename in expected_basenames
                for phase in (
                    ("pre-family-baseline", basename),
                    ("pre-family-transaction", basename),
                )
            ],
            ("pre-metadata", None),
            ("pre-seal", None),
        ]
        observed_capacity_sequence = [
            (record["phase"], record["family"]) for record in observations
        ]
        requirement = compose_request["capacity_requirement"]
        if (
            expires
            != _parse_utc(
                capacity_observation["expires_at"],
                "capacity observation expiry",
            )
            or capacity["capacity_observation_sha256"]
            != capacity_observation["observation_sha256"]
            or capacity["host_authority_id"]
            != capacity_observation["host_authority_id"]
            or any(
                record["remaining_required_bytes"] != requirement["required_bytes"]
                or record["remaining_required_inodes"] != requirement["required_inodes"]
                for record in observations
            )
            or observed_capacity_sequence != expected_capacity_sequence
            or capacity["space_reserved"] is not False
            or capacity["quota_proof"] is not False
            or capacity["future_write_guaranteed"] is not False
        ):
            failures.append(
                _failure(
                    "capacity-admission-differs",
                    scope="composition-receipt",
                )
            )
        manifest_created_at = _parse_utc(
            manifest["created_at"],
            "bundle candidate manifest creation timestamp",
        )
        receipt_completed_at = _parse_utc(
            receipt["completed_at"],
            "bundle candidate receipt completion timestamp",
        )
        report_observed_at = _parse_utc(
            observed_at,
            "oracle observation timestamp",
        )
        capacity_observed_at = [
            _parse_utc(
                record["observed_at"],
                "bundle candidate capacity observation timestamp",
            )
            for record in observations
        ]
        if not (
            capacity_observed_at[-1]
            <= manifest_created_at
            <= receipt_completed_at
            <= report_observed_at
        ):
            failures.append(
                _failure(
                    "composition-chronology-differs",
                    scope="composition-receipt",
                )
            )
        sqlite_descriptor, sqlite_info = _open_private_directory_at(
            bundle_descriptor,
            "sqlite",
            label="offline bundle candidate SQLite directory",
        )
        if _mount(sqlite_descriptor) != parent_mount:
            raise BulkloadError(
                "offline SQLite candidate directory crosses the pinned mount"
            )
        sqlite_lineage = _lineage(sqlite_descriptor)
        sqlite_entries = _bounded_directory_entries(
            sqlite_descriptor,
            label="offline SQLite candidate directory",
            deadline=deadline,
        )
        action_by_basename = {
            family["basename"]: family for family in action_plan["sqlite_families"]
        }
        opening_by_basename = {
            family["basename"]: family for family in opening_plan["sqlite_families"]
        }
        manifest_by_basename = {
            family["basename"]: family for family in manifest["families"]
        }
        if (
            sqlite_entries != expected_basenames
            or sorted(opening_by_basename) != expected_basenames
            or sorted(manifest_by_basename) != expected_basenames
        ):
            raise BulkloadError("offline bundle candidate family inventory differs")
        semantic_budget = {"rows": 0, "bytes": 0}
        observed_engine: dict[str, Any] | None = None
        engine_consistent = (
            manifest["sqlite_engine_authority"] == receipt["sqlite_engine_authority"]
        )
        payload_inventory: list[dict[str, Any]] = []
        for basename in expected_basenames:
            action_family = action_by_basename[basename]
            opening_family = opening_by_basename[basename]
            manifest_family = manifest_by_basename[basename]
            with _open_immutable_database(
                sqlite_descriptor,
                basename,
                maximum_bytes=compose_request["capacity_requirement"]["required_bytes"],
            ) as (connection, database_descriptor, database_info):
                database_descriptors.append(
                    (os.dup(database_descriptor), database_info, basename)
                )
                if _mount(database_descriptor) != parent_mount:
                    raise BulkloadError(
                        "offline SQLite candidate family crosses the pinned mount"
                    )
                physical_sha256, physical_size = _hash_descriptor(
                    database_descriptor,
                    compose_request["capacity_requirement"]["required_bytes"],
                    deadline=deadline,
                )
                engine = _sqlite_engine_authority(connection)
                if observed_engine is None:
                    observed_engine = engine
                elif observed_engine != engine:
                    engine_consistent = False
                    failures.append(
                        _failure(
                            "verifier-engine-differs-by-family",
                            scope="sqlite-engine",
                            family=basename,
                        )
                    )
                if engine != manifest["sqlite_engine_authority"]:
                    engine_consistent = False
                    failures.append(
                        _failure(
                            "claimed-sqlite-engine-differs",
                            scope="sqlite-engine",
                            family=basename,
                        )
                    )
                schema_limits = _schema_scan_limits(
                    action_family,
                    opening_family,
                )
                prior_observation_schema_limits = _install_sqlite_schema_limits(
                    connection
                )
                try:
                    integrity_ok, foreign_key_violations = _sqlite_health_checks(
                        connection,
                        deadline=deadline,
                    )
                    schema, schema_blockers, migrations = _scan_schema_independently(
                        connection,
                        deadline=deadline,
                        limits=schema_limits,
                    )
                finally:
                    _restore_sqlite_schema_limits(
                        connection,
                        prior_observation_schema_limits,
                    )
                edge_sha256 = sha256_bytes(canonical_bytes(_observed_edges(schema)))
                migration_sha256 = sha256_bytes(canonical_bytes(migrations))
                contract_by_name = {
                    table["name"]: table
                    for table in opening_family["source_schema"]["tables"]
                }
                observed_contract_by_name = {
                    table["name"]: table for table in schema["tables"]
                }
                table_results: list[dict[str, Any]] = []
                semantic_match = True
                for action_table in action_family["tables"]:
                    expected_table_contract = contract_by_name.get(action_table["name"])
                    observed_table_contract = observed_contract_by_name.get(
                        action_table["name"]
                    )
                    if expected_table_contract is None:
                        raise BulkloadError(
                            "verifier output table is absent from the schema contract"
                        )
                    if observed_table_contract is None:
                        raise BulkloadError(
                            "verifier output table is absent from the observed schema"
                        )
                    if observed_table_contract != expected_table_contract:
                        failures.append(
                            _failure(
                                "table-schema-differs",
                                scope="family",
                                family=basename,
                                field=f"tables[{action_table['name']}]",
                            )
                        )
                    scanned_table = _scan_table(
                        connection,
                        basename=basename,
                        table_contract=observed_table_contract,
                        action_table=action_table,
                        deadline=deadline,
                        budget=semantic_budget,
                    )
                    table_result = {
                        "name": scanned_table["name"],
                        "identity_columns": deepcopy(action_table["identity_columns"]),
                        "row_count": scanned_table["row_count"],
                        "semantic_rows_sha256": scanned_table["semantic_rows_sha256"],
                        "schema_sha256": sha256_bytes(
                            canonical_bytes(observed_table_contract)
                        ),
                        "foreign_keys_sha256": sha256_bytes(
                            canonical_bytes(observed_table_contract["foreign_keys"])
                        ),
                    }
                    table_results.append(table_result)
                    expected_output = action_table["expected_output"]
                    if (
                        not isinstance(expected_output, dict)
                        or scanned_table["row_count"] != expected_output["row_count"]
                        or scanned_table["classified_bytes"]
                        != expected_output["classified_bytes"]
                        or scanned_table["semantic_rows_sha256"]
                        != expected_output["semantic_rows_sha256"]
                    ):
                        semantic_match = False
                        failures.append(
                            _failure(
                                "semantic-union-differs",
                                scope="family",
                                family=basename,
                                field=f"tables[{action_table['name']}]",
                            )
                        )
                if not integrity_ok:
                    failures.append(
                        _failure(
                            "integrity-check-failed",
                            scope="family",
                            family=basename,
                        )
                    )
                if foreign_key_violations:
                    failures.append(
                        _failure(
                            "foreign-key-check-failed",
                            scope="family",
                            family=basename,
                        )
                    )
                if schema_blockers:
                    failures.append(
                        _failure(
                            "schema-blockers-observed",
                            scope="family",
                            family=basename,
                            field="schema",
                        )
                    )
                expected_schema = opening_family["source_schema"]
                schema_match = bool(
                    not schema_blockers
                    and schema["schema_contract_sha256"]
                    == expected_schema["schema_contract_sha256"]
                    and schema["raw_schema_sha256"]
                    == expected_schema["raw_schema_sha256"]
                    and schema["application_id"]
                    == action_family["schema"]["source_application_id"]
                    and schema["user_version"]
                    == action_family["schema"]["source_user_version"]
                )
                if not schema_match:
                    failures.append(
                        _failure(
                            "schema-contract-differs",
                            scope="family",
                            family=basename,
                            field="schema",
                        )
                    )
                migration_match = bool(
                    migration_sha256
                    == action_family["migration"]["source_migrations_sha256"]
                    and action_family["migration"]["exact"] is True
                )
                if not migration_match:
                    failures.append(
                        _failure(
                            "migration-contract-differs",
                            scope="family",
                            family=basename,
                            field="migrations",
                        )
                    )
                edge_match = bool(
                    edge_sha256 == action_family["edge_contract"]["registry_sha256"]
                    and action_family["edge_contract"]["registry_matches_observed"]
                    is True
                )
                if not edge_match:
                    failures.append(
                        _failure(
                            "edge-contract-differs",
                            scope="family",
                            family=basename,
                            field="edges",
                        )
                    )
                journal_mode_row = connection.execute("PRAGMA journal_mode").fetchone()
                journal_mode = (
                    str(journal_mode_row[0]).lower()
                    if journal_mode_row is not None and len(journal_mode_row) == 1
                    else ""
                )
                observed_manifest_family = {
                    "basename": basename,
                    "relative_path": f"sqlite/{basename}",
                    "mode": 0o600,
                    "size": physical_size,
                    "sha256": physical_sha256,
                    "journal_mode": journal_mode,
                    "application_id": schema["application_id"],
                    "user_version": schema["user_version"],
                    "schema": {
                        "raw_schema_sha256": schema["raw_schema_sha256"],
                        "schema_contract_sha256": schema["schema_contract_sha256"],
                        "structured_schema_sha256": sha256_bytes(
                            canonical_bytes(schema)
                        ),
                    },
                    "migrations": {
                        "migrations_sha256": migration_sha256,
                        "exact": True,
                    },
                    "edges": {
                        "registry_sha256": action_family["edge_contract"][
                            "registry_sha256"
                        ],
                        "observed_sha256": edge_sha256,
                        "closed": True,
                    },
                    "tables": table_results,
                    "absent_sidecars": sorted(
                        f"sqlite/{basename}{suffix}"
                        for suffix in ("-journal", "-shm", "-wal")
                    ),
                }
                manifest_match = manifest_family == observed_manifest_family
                if not manifest_match:
                    failures.append(
                        _failure(
                            "manifest-family-differs",
                            scope="manifest",
                            family=basename,
                        )
                    )
                family_results.append(
                    {
                        "observed": observed_manifest_family,
                        "manifest_matches_observed": manifest_match,
                        "action_plan_matches_observed": bool(
                            schema_match
                            and migration_match
                            and edge_match
                            and semantic_match
                        ),
                        "integrity_check_observed": integrity_ok,
                        "foreign_key_check_observed": (foreign_key_violations == 0),
                        "schema_matches_action_observed": schema_match,
                        "migration_matches_action_observed": migration_match,
                        "edge_matches_action_observed": edge_match,
                        "semantic_output_matches_action_observed": semantic_match,
                    }
                )
                payload_inventory.append(
                    {
                        "relative_path": f"sqlite/{basename}",
                        "type": "regular-file",
                        "mode": 0o600,
                        "size": physical_size,
                        "sha256": physical_sha256,
                    }
                )
        if observed_engine is None:
            raise BulkloadError(
                "offline bundle candidate has no SQLite engine authority"
            )
        if not engine_consistent:
            failures.append(
                _failure(
                    "sqlite-engine-authority-differs",
                    scope="sqlite-engine",
                )
            )
        payload_inventory = sorted(
            payload_inventory,
            key=lambda item: item["relative_path"],
        )
        if manifest["payload_inventory"] != {
            "entries": payload_inventory,
            "inventory_sha256": sha256_bytes(canonical_bytes(payload_inventory)),
        }:
            failures.append(
                _failure(
                    "payload-inventory-differs",
                    scope="manifest",
                    field="payload_inventory",
                )
            )
        bundle_inventory = [
            {
                "relative_path": "composition-receipt.json",
                "type": "regular-file",
                "mode": 0o600,
                "size": receipt_info.st_size,
                "sha256": sha256_bytes(receipt_payload),
            },
            {
                "relative_path": "manifest.json",
                "type": "regular-file",
                "mode": 0o600,
                "size": manifest_info.st_size,
                "sha256": sha256_bytes(manifest_payload),
            },
            {
                "relative_path": "sqlite",
                "type": "directory",
                "mode": 0o700,
                "size": None,
                "sha256": None,
            },
            *payload_inventory,
        ]
        bundle_inventory = sorted(
            bundle_inventory,
            key=lambda item: item["relative_path"],
        )
        failures = _dedupe_failures(failures)
        claims = {
            "final_leaf_commit_observed": False,
            "bundle_sealed": False,
            "full_against_inputs_recomputed": False,
            "independent_verification_complete": False,
            "offline_bundle_verified": False,
            "composer_implemented": False,
            "ready_for_internal_offline_compose": False,
            "ready_for_offline_compose": False,
            "sqlite_compose": False,
            "sqlite_publish": False,
            "published": False,
            "publication_authorized": False,
            "installed": False,
            "install_authorized": False,
            "session_union_executed": False,
            "sqlite_union_ready": False,
            "provider_runtime_acceptance": False,
            "provider_runtime_acceptance_verified": False,
            "provider_writer_proof": False,
            "combined": False,
            "combined_authorized": False,
            "ready_for_apply": False,
            "apply_authorized": False,
            "engine_diversity_verified": False,
        }
        bundle_lineage = _lineage(bundle_descriptor)
        observed_checks = {
            "artifact_bindings_observed": not any(
                failure["code"] == "accepted-artifact-binding-differs"
                for failure in failures
            ),
            "descriptor_custody_observed": True,
            "tree_inventory_observed": True,
            "manifest_structure_observed": True,
            "composition_receipt_structure_observed": not any(
                failure["code"]
                in {
                    "capacity-admission-differs",
                    "composition-chronology-differs",
                }
                for failure in failures
            ),
            "sqlite_engine_consistency_observed": engine_consistent,
            "integrity_checks_observed": all(
                family["integrity_check_observed"] for family in family_results
            ),
            "foreign_key_checks_observed": all(
                family["foreign_key_check_observed"] for family in family_results
            ),
            "schema_comparisons_observed": all(
                family["schema_matches_action_observed"] for family in family_results
            ),
            "migration_comparisons_observed": all(
                family["migration_matches_action_observed"] for family in family_results
            ),
            "edge_comparisons_observed": all(
                family["edge_matches_action_observed"] for family in family_results
            ),
            "semantic_comparisons_observed": all(
                family["semantic_output_matches_action_observed"]
                for family in family_results
            ),
        }
        report: dict[str, Any] = {
            "schema": VERIFIER_ORACLE_REPORT_SCHEMA,
            "observed_at": observed_at,
            "observation_id": observation_id,
            "verifier_runtime_authority": deepcopy(verifier_runtime_binding),
            "accepted_artifacts": expected_inputs,
            "bundle_observation": {
                "resolved_final_path": os.fspath(normalized),
                "final_leaf": normalized.name,
                "identity": _identity(bundle_info),
                "mount": bundle_mount,
                "lineage": bundle_lineage,
                "lineage_sha256": sha256_bytes(canonical_bytes(bundle_lineage)),
                "tree_inventory": bundle_inventory,
                "tree_inventory_sha256": sha256_bytes(
                    canonical_bytes(bundle_inventory)
                ),
            },
            "manifest_observation": {
                "schema": COMPOSED_BUNDLE_MANIFEST_SCHEMA,
                "manifest_sha256": manifest["manifest_sha256"],
                "canonical_file_sha256": sha256_bytes(manifest_payload),
                "canonical_file_bytes": len(manifest_payload),
                "strictly_valid": True,
            },
            "composition_receipt_observation": {
                "schema": COMPOSITION_RECEIPT_SCHEMA,
                "receipt_sha256": receipt["receipt_sha256"],
                "canonical_file_sha256": sha256_bytes(receipt_payload),
                "canonical_file_bytes": len(receipt_payload),
                "strictly_valid": True,
            },
            "families": family_results,
            "failures": failures,
            "observed_checks": observed_checks,
            "claims": claims,
            "implementation": PRIVATE_SQLITE_VERIFIER_IMPLEMENTATION,
        }
        report["oracle_report_sha256"] = object_digest(
            report,
            "oracle_report_sha256",
        )
        validate_verifier_oracle_report(report)
        if (
            _bounded_directory_entries(
                bundle_descriptor,
                label="offline bundle candidate",
                deadline=deadline,
            )
            != ["composition-receipt.json", "manifest.json", "sqlite"]
            or _bounded_directory_entries(
                sqlite_descriptor,
                label="offline SQLite candidate directory",
                deadline=deadline,
            )
            != expected_basenames
            or _stable(os.fstat(sqlite_descriptor)) != _stable(sqlite_info)
            or _stable(
                os.stat(
                    "sqlite",
                    dir_fd=bundle_descriptor,
                    follow_symlinks=False,
                )
            )
            != _stable(sqlite_info)
            or _mount(sqlite_descriptor) != parent_mount
            or _lineage(sqlite_descriptor) != sqlite_lineage
        ):
            raise BulkloadError(
                "offline SQLite candidate directory changed during observation"
            )
        for descriptor, expected, basename in database_descriptors:
            if (
                _stable(os.fstat(descriptor)) != _stable(expected)
                or _stable(
                    os.stat(
                        basename,
                        dir_fd=sqlite_descriptor,
                        follow_symlinks=False,
                    )
                )
                != _stable(expected)
                or _mount(descriptor) != parent_mount
            ):
                raise BulkloadError(
                    "offline SQLite candidate family changed during observation: "
                    f"{basename}"
                )
        if (
            _stable(os.fstat(manifest_descriptor)) != _stable(manifest_info)
            or _stable(os.fstat(receipt_descriptor)) != _stable(receipt_info)
            or _stable(
                os.stat(
                    "manifest.json",
                    dir_fd=bundle_descriptor,
                    follow_symlinks=False,
                )
            )
            != _stable(manifest_info)
            or _stable(
                os.stat(
                    "composition-receipt.json",
                    dir_fd=bundle_descriptor,
                    follow_symlinks=False,
                )
            )
            != _stable(receipt_info)
            or _stable(os.fstat(bundle_descriptor)) != _stable(bundle_info)
            or _stable(
                os.stat(
                    normalized.name,
                    dir_fd=parent_descriptor,
                    follow_symlinks=False,
                )
            )
            != _stable(bundle_info)
            or _mount(bundle_descriptor) != bundle_mount
            or _lineage(bundle_descriptor) != bundle_lineage
            or _identity(os.fstat(parent_descriptor)) != parent_identity
            or _mount(parent_descriptor) != parent_mount
            or _lineage(parent_descriptor) != parent_lineage
        ):
            raise BulkloadError("offline bundle candidate changed during observation")
        reopened_normalized, reopened_parent, reopened_parent_info = (
            _open_absolute_parent(normalized)
        )
        try:
            if (
                reopened_normalized != normalized
                or _identity(reopened_parent_info) != parent_identity
                or _mount(reopened_parent) != parent_mount
                or _lineage(reopened_parent) != parent_lineage
                or _stable(
                    os.stat(
                        normalized.name,
                        dir_fd=reopened_parent,
                        follow_symlinks=False,
                    )
                )
                != _stable(bundle_info)
            ):
                raise BulkloadError(
                    "offline bundle candidate parent changed during observation"
                )
        finally:
            os.close(reopened_parent)
        for descriptor, _, _ in database_descriptors:
            os.close(descriptor)
        database_descriptors.clear()
        return report
    finally:
        for descriptor, _, _ in database_descriptors:
            os.close(descriptor)
        for descriptor in (
            receipt_descriptor,
            manifest_descriptor,
            sqlite_descriptor,
            bundle_descriptor,
            parent_descriptor,
        ):
            if descriptor >= 0:
                os.close(descriptor)
