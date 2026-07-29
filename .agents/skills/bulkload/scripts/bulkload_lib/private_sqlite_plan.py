"""Plan-only cross-plane classification for private Codex SQLite state.

This module deliberately has no composer, publisher, installer, or mutation
entrypoint.  It turns four immutable v3 private captures plus a recomputed
Codex session-union closure into a digest-bound opening request for a later,
separately reviewed offline composer.
"""

from __future__ import annotations

from contextlib import contextmanager
import hashlib
import os
from pathlib import Path
import re
import sqlite3
import stat
import struct
import time
from typing import Any, Iterator
from urllib.parse import quote
import uuid

from . import private_runtime
from .model import (
    BulkloadError,
    canonical_bytes,
    object_digest,
    sha256_bytes,
    utc_now,
)
from .private_state import (
    AUTH_BASENAME,
    COUNTED_TABLES,
    MAX_AUTH_BYTES,
    MAX_PRIVATE_PLAN_BYTES,
    MAX_PRIVATE_MANIFEST_BYTES,
    SQLITE_DIRECTORY,
    SQLITE_BASENAME as PRIVATE_SQLITE_BASENAME,
    read_codex_private_bundle,
    validate_codex_private_state_plan,
    validate_codex_private_state_plan_against_bundles,
)
from .sessions import (
    validate_codex_session_union_plan,
    validate_codex_session_union_plan_against_inputs,
    validate_codex_session_union_evidence_binding,
)


PRIVATE_SQLITE_PLAN_SCHEMA = (
    "dev.tinyland.bulkload.codex-private-sqlite-compose-plan.v4"
)
SQLITE_ADAPTER_REGISTRY_SCHEMA = (
    "dev.tinyland.bulkload.codex-private-sqlite-adapter-registry.v1"
)
SQLITE_PATH_MAP_SCHEMA = "dev.tinyland.bulkload.codex-private-sqlite-path-map.v1"
MAX_SQLITE_PLAN_BYTES = MAX_PRIVATE_PLAN_BYTES - 1
MAX_SQLITE_REGISTRY_BYTES = 8 * 1024 * 1024
MAX_SQLITE_PATH_MAP_BYTES = 1024 * 1024
MAX_SQLITE_PLAN_ROWS = 10_000_000
MAX_SQLITE_PLAN_ROW_BYTES = 64 * 1024 * 1024 * 1024
MAX_SQLITE_VALUE_BYTES = 64 * 1024 * 1024
MAX_SQLITE_PLAN_SECONDS = 600

_SHA256 = re.compile(r"[0-9a-f]{64}")
_SQLITE_BASENAME = re.compile(r"(?:state|logs|goals|memories)_[1-9][0-9]*\.sqlite")
_IDENTIFIER = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
_FAMILY_PREFIX_TO_ROLE = {
    "state": "state",
    "logs": "logs",
    "goals": "goals",
    "memories": "memories",
}
_MERGE_CLASSES = {
    "exact",
    "keyed-union",
    "append-multiset",
    "unsupported",
}
_STANDARD_COLLATIONS = {"BINARY", "NOCASE", "RTRIM"}
_REQUIRED_STATE_PATH_RULE = {
    "table": "threads",
    "session_id_column": "id",
    "column": "rollout_path",
    "kind": "session-rollout",
}
_REQUIRED_PATH_MAP_RULE = {
    "family_basename": "state_5.sqlite",
    **_REQUIRED_STATE_PATH_RULE,
}


def _require_exact_keys(value: Any, expected: set[str], label: str) -> dict[str, Any]:
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


def _require_identifier(value: Any, label: str) -> str:
    if not isinstance(value, str) or _IDENTIFIER.fullmatch(value) is None:
        raise BulkloadError(f"{label} is not a supported SQLite identifier")
    return value


def _require_absolute_normal_path(value: Any, label: str) -> str:
    if (
        not isinstance(value, str)
        or "\x00" in value
        or not Path(value).is_absolute()
        or os.path.normpath(value) != value
        or value == Path(value).anchor
    ):
        raise BulkloadError(f"{label} must be a normalized absolute path")
    return value


def _family_identity(
    basename: str,
    *,
    allow_unknown: bool = False,
) -> tuple[str, int | None]:
    if not isinstance(basename, str):
        raise BulkloadError("SQLite adapter family basename is invalid")
    if _SQLITE_BASENAME.fullmatch(basename) is None:
        if allow_unknown and PRIVATE_SQLITE_BASENAME.fullmatch(basename) is not None:
            return "unsupported", None
        raise BulkloadError("SQLite adapter family basename is unsupported")
    prefix, generation_text = basename.removesuffix(".sqlite").rsplit("_", 1)
    return _FAMILY_PREFIX_TO_ROLE[prefix], int(generation_text)


def _validate_table_rule(value: Any) -> dict[str, Any]:
    rule = _require_exact_keys(
        value,
        {"name", "merge_class", "identity_columns"},
        "SQLite adapter table rule",
    )
    _require_identifier(rule["name"], "SQLite adapter table name")
    if rule["merge_class"] not in _MERGE_CLASSES:
        raise BulkloadError("SQLite adapter table merge class is unsupported")
    identities = rule["identity_columns"]
    if (
        not isinstance(identities, list)
        or not identities
        or len(identities) != len(set(identities))
    ):
        raise BulkloadError("SQLite adapter table identity is invalid")
    for column in identities:
        _require_identifier(column, "SQLite adapter identity column")
    return rule


def _validate_path_rule(value: Any, *, registry: bool) -> dict[str, Any]:
    expected = (
        {"table", "session_id_column", "column", "kind"}
        if registry
        else {
            "family_basename",
            "table",
            "session_id_column",
            "column",
            "kind",
        }
    )
    label = "SQLite adapter path rule" if registry else "SQLite path-map rule"
    rule = _require_exact_keys(value, expected, label)
    if not registry:
        _family_identity(rule["family_basename"])
    for key in ("table", "session_id_column", "column"):
        _require_identifier(rule[key], f"{label} {key}")
    if rule["kind"] != "session-rollout":
        raise BulkloadError(f"{label} kind is unsupported")
    if (
        rule["table"] != "threads"
        or rule["session_id_column"] != "id"
        or rule["column"] != "rollout_path"
    ):
        raise BulkloadError(
            f"{label} is outside the initial exact rollout-path allowlist"
        )
    return rule


def _validate_edge_rule(value: Any) -> dict[str, Any]:
    rule = _require_exact_keys(
        value,
        {
            "table",
            "from_columns",
            "referenced_table",
            "referenced_columns",
            "on_update",
            "on_delete",
            "match",
            "required",
        },
        "SQLite adapter edge rule",
    )
    _require_identifier(rule["table"], "SQLite adapter edge table")
    _require_identifier(
        rule["referenced_table"],
        "SQLite adapter referenced table",
    )
    for key in ("from_columns", "referenced_columns"):
        columns = rule[key]
        if (
            not isinstance(columns, list)
            or not columns
            or len(columns) != len(set(columns))
        ):
            raise BulkloadError("SQLite adapter edge columns are invalid")
        for column in columns:
            _require_identifier(column, "SQLite adapter edge column")
    if len(rule["from_columns"]) != len(rule["referenced_columns"]):
        raise BulkloadError("SQLite adapter edge arity differs")
    for key in ("on_update", "on_delete", "match"):
        if (
            not isinstance(rule[key], str)
            or not rule[key]
            or len(rule[key].encode("utf-8")) > 64
        ):
            raise BulkloadError(f"SQLite adapter edge {key} is invalid")
    if rule["required"] is not True:
        raise BulkloadError("SQLite adapter edge requirement is invalid")
    return rule


def validate_sqlite_adapter_registry(value: dict[str, Any]) -> None:
    registry = _require_exact_keys(
        value,
        {
            "schema",
            "registry_id",
            "created_at",
            "families",
            "registry_sha256",
        },
        "SQLite adapter registry",
    )
    if registry["schema"] != SQLITE_ADAPTER_REGISTRY_SCHEMA:
        raise BulkloadError("unsupported SQLite adapter registry schema")
    _require_uuid(registry["registry_id"], "SQLite adapter registry ID")
    if not isinstance(registry["created_at"], str) or not registry["created_at"]:
        raise BulkloadError("SQLite adapter registry timestamp is invalid")
    families = registry["families"]
    if not isinstance(families, list):
        raise BulkloadError("SQLite adapter registry families must be a list")
    basenames: list[str] = []
    for item in families:
        family = _require_exact_keys(
            item,
            {
                "basename",
                "family_role",
                "generation",
                "source_codex_version",
                "destination_codex_version",
                "source_schema_contract_sha256",
                "destination_schema_contract_sha256",
                "source_raw_schema_sha256",
                "destination_raw_schema_sha256",
                "source_migrations_sha256",
                "destination_migrations_sha256",
                "source_application_id",
                "destination_application_id",
                "source_user_version",
                "destination_user_version",
                "migration_relation",
                "adapter",
                "tables",
                "path_authorities",
                "edges",
                "allowed_collations",
            },
            "SQLite adapter family",
        )
        role, generation = _family_identity(family["basename"])
        if family["family_role"] != role or family["generation"] != generation:
            raise BulkloadError("SQLite adapter family identity is inconsistent")
        for key in ("source_codex_version", "destination_codex_version"):
            if (
                not isinstance(family[key], str)
                or not family[key]
                or len(family[key].encode("utf-8")) > 128
            ):
                raise BulkloadError("SQLite adapter Codex version is invalid")
        for key in (
            "source_schema_contract_sha256",
            "destination_schema_contract_sha256",
            "source_raw_schema_sha256",
            "destination_raw_schema_sha256",
            "source_migrations_sha256",
            "destination_migrations_sha256",
        ):
            _require_sha256(family[key], f"SQLite adapter {key}")
        for key in (
            "source_application_id",
            "destination_application_id",
            "source_user_version",
            "destination_user_version",
        ):
            if type(family[key]) is not int or family[key] < 0:
                raise BulkloadError("SQLite adapter header value is invalid")
        if family["migration_relation"] not in {
            "exact",
            "registered-prefix-upgrade",
        }:
            raise BulkloadError("SQLite adapter migration relation is invalid")
        adapter = family["adapter"]
        if family["migration_relation"] == "exact":
            if adapter is not None:
                raise BulkloadError("exact SQLite migration cannot name an adapter")
        else:
            adapter = _require_exact_keys(
                adapter,
                {"adapter_id", "adapter_source_sha256"},
                "SQLite migration adapter",
            )
            if (
                not isinstance(adapter["adapter_id"], str)
                or not adapter["adapter_id"]
                or len(adapter["adapter_id"].encode("utf-8")) > 255
            ):
                raise BulkloadError("SQLite migration adapter ID is invalid")
            _require_sha256(
                adapter["adapter_source_sha256"],
                "SQLite migration adapter source digest",
            )
        tables = family["tables"]
        if not isinstance(tables, list):
            raise BulkloadError("SQLite adapter tables must be a list")
        for table in tables:
            _validate_table_rule(table)
        table_names = [table["name"] for table in tables]
        if table_names != sorted(set(table_names)):
            raise BulkloadError("SQLite adapter tables are not canonical")
        path_rules = family["path_authorities"]
        if not isinstance(path_rules, list):
            raise BulkloadError("SQLite adapter path rules must be a list")
        for rule in path_rules:
            _validate_path_rule(rule, registry=True)
            if rule["table"] not in table_names:
                raise BulkloadError("SQLite adapter path table is not registered")
        if [canonical_bytes(rule) for rule in path_rules] != sorted(
            {canonical_bytes(rule) for rule in path_rules}
        ):
            raise BulkloadError("SQLite adapter path rules are not canonical")
        expected_path_rules = (
            [_REQUIRED_STATE_PATH_RULE]
            if family["basename"] == "state_5.sqlite"
            else []
        )
        if path_rules != expected_path_rules:
            raise BulkloadError(
                "SQLite adapter path rules differ from the v4 exact authority"
            )
        edges = family["edges"]
        if not isinstance(edges, list):
            raise BulkloadError("SQLite adapter edges must be a list")
        for edge in edges:
            _validate_edge_rule(edge)
            if (
                edge["table"] not in table_names
                or edge["referenced_table"] not in table_names
            ):
                raise BulkloadError("SQLite adapter edge table is not registered")
        if [canonical_bytes(edge) for edge in edges] != sorted(
            {canonical_bytes(edge) for edge in edges}
        ):
            raise BulkloadError("SQLite adapter edges are not canonical")
        collations = family["allowed_collations"]
        if (
            not isinstance(collations, list)
            or collations != sorted(set(collations))
            or not all(
                isinstance(collation, str)
                and bool(collation)
                and len(collation.encode("utf-8")) <= 255
                for collation in collations
            )
        ):
            raise BulkloadError("SQLite adapter collations are invalid")
        basenames.append(family["basename"])
    if basenames != sorted(set(basenames)):
        raise BulkloadError("SQLite adapter families are not canonical")
    if len(canonical_bytes(registry)) > MAX_SQLITE_REGISTRY_BYTES:
        raise BulkloadError("SQLite adapter registry exceeds its byte budget")
    _require_sha256(registry["registry_sha256"], "SQLite adapter registry digest")
    if object_digest(registry, "registry_sha256") != registry["registry_sha256"]:
        raise BulkloadError("SQLite adapter registry digest mismatch")


def validate_sqlite_path_map(value: dict[str, Any]) -> None:
    path_map = _require_exact_keys(
        value,
        {
            "schema",
            "mapping_id",
            "created_at",
            "source_host_authority_id",
            "destination_host_authority_id",
            "codex_version",
            "session_union_plan_sha256",
            "source_session_catalog_sha256",
            "destination_session_catalog_sha256",
            "source_session_root",
            "destination_session_root",
            "rules",
            "path_map_sha256",
        },
        "SQLite path map",
    )
    if path_map["schema"] != SQLITE_PATH_MAP_SCHEMA:
        raise BulkloadError("unsupported SQLite path-map schema")
    _require_uuid(path_map["mapping_id"], "SQLite path-map ID")
    if not isinstance(path_map["created_at"], str) or not path_map["created_at"]:
        raise BulkloadError("SQLite path-map timestamp is invalid")
    for key in ("source_host_authority_id", "destination_host_authority_id"):
        _require_uuid(path_map[key], f"SQLite path-map {key}")
    versions = _require_exact_keys(
        path_map["codex_version"],
        {"source", "destination"},
        "SQLite path-map Codex versions",
    )
    if not all(
        isinstance(version, str)
        and bool(version)
        and len(version.encode("utf-8")) <= 128
        for version in versions.values()
    ):
        raise BulkloadError("SQLite path-map Codex version is invalid")
    for key in (
        "session_union_plan_sha256",
        "source_session_catalog_sha256",
        "destination_session_catalog_sha256",
    ):
        _require_sha256(path_map[key], f"SQLite path-map {key}")
    _require_absolute_normal_path(
        path_map["source_session_root"],
        "SQLite path-map source session root",
    )
    _require_absolute_normal_path(
        path_map["destination_session_root"],
        "SQLite path-map destination session root",
    )
    rules = path_map["rules"]
    if not isinstance(rules, list):
        raise BulkloadError("SQLite path-map rules must be a list")
    for rule in rules:
        _validate_path_rule(rule, registry=False)
    encodings = [canonical_bytes(rule) for rule in rules]
    if encodings != sorted(set(encodings)):
        raise BulkloadError("SQLite path-map rules are not canonical")
    if rules != [_REQUIRED_PATH_MAP_RULE]:
        raise BulkloadError("SQLite path-map rules differ from the v4 exact authority")
    if len(canonical_bytes(path_map)) > MAX_SQLITE_PATH_MAP_BYTES:
        raise BulkloadError("SQLite path map exceeds its byte budget")
    _require_sha256(path_map["path_map_sha256"], "SQLite path-map digest")
    if object_digest(path_map, "path_map_sha256") != path_map["path_map_sha256"]:
        raise BulkloadError("SQLite path-map digest mismatch")


def _stable_private_projection(capture: dict[str, Any]) -> dict[str, Any]:
    return {
        "role": capture["role"],
        "host": capture["host"],
        "host_authority_id": capture["host_authority_id"],
        "codex_version": capture["codex_version"],
        "codex_home": capture["codex_home"],
        "sqlite_home": capture["sqlite_home"],
        "selected_state_classes": capture["selected_state_classes"],
        "budgets": capture["budgets"],
        "auth": capture["auth"],
        "sqlite_families": capture["sqlite_families"],
        "sqlite_live_namespace_sha256": capture["sqlite_live_namespace_sha256"],
        "copy_method": capture["copy_method"],
        "complete": capture["complete"],
        "ready_for_apply": capture["ready_for_apply"],
    }


def _private_pair_binding(
    first: dict[str, Any],
    second: dict[str, Any],
    *,
    role: str,
) -> dict[str, Any]:
    if first["role"] != role or second["role"] != role:
        raise BulkloadError(f"SQLite {role} opening capture role differs")
    if (
        "sqlite" not in first["selected_state_classes"]
        or "sqlite" not in second["selected_state_classes"]
    ):
        raise BulkloadError(f"SQLite {role} opening captures must select SQLite")
    if first["capture_id"] == second["capture_id"]:
        raise BulkloadError(f"SQLite {role} opening passes must be distinct")
    first_attestation = first["quiescence"]["attestation_id"]
    second_attestation = second["quiescence"]["attestation_id"]
    if first_attestation == second_attestation:
        raise BulkloadError(
            f"SQLite {role} opening passes must use distinct attestations"
        )
    first_projection = _stable_private_projection(first)
    second_projection = _stable_private_projection(second)
    if canonical_bytes(first_projection) != canonical_bytes(second_projection):
        raise BulkloadError("sqlite-opening-stability-mismatch")
    return {
        "role": role,
        "host": first["host"],
        "host_authority_id": first["host_authority_id"],
        "codex_version": first["codex_version"],
        "capture_ids": [first["capture_id"], second["capture_id"]],
        "capture_sha256s": [
            first["capture_sha256"],
            second["capture_sha256"],
        ],
        "quiescence_attestation_ids": [
            first_attestation,
            second_attestation,
        ],
        "stable_projection": first_projection,
        "stable_projection_sha256": sha256_bytes(canonical_bytes(first_projection)),
    }


def _normalize_capture_id(value: str) -> str:
    try:
        return uuid.UUID(value).hex
    except ValueError as error:
        raise BulkloadError("cross-plane capture ID is invalid") from error


def _session_evidence_capture_ids(
    source_a: dict[str, Any],
    source_b: dict[str, Any],
    destination_a: dict[str, Any],
    destination_b: dict[str, Any],
    *,
    source_prefix_a: dict[str, Any] | None,
    source_prefix_b: dict[str, Any] | None,
    destination_prefix_a: dict[str, Any] | None,
    destination_prefix_b: dict[str, Any] | None,
    source_close_a: dict[str, Any] | None,
    source_close_b: dict[str, Any] | None,
    destination_close_a: dict[str, Any] | None,
    destination_close_b: dict[str, Any] | None,
) -> set[str]:
    capture_ids = {
        source_a["capture_id"],
        source_b["capture_id"],
        destination_a["capture_id"],
        destination_b["capture_id"],
    }
    for proof in (
        source_prefix_a,
        source_prefix_b,
        destination_prefix_a,
        destination_prefix_b,
    ):
        if proof is not None:
            capture_ids.add(proof["capture_id"])
    for close in (
        source_close_a,
        source_close_b,
        destination_close_a,
        destination_close_b,
    ):
        if close is not None:
            capture_ids.add(close["snapshot"]["capture_id"])
    return {_normalize_capture_id(capture_id) for capture_id in capture_ids}


def _path_is_within(path: str, root: str) -> bool:
    try:
        Path(path).relative_to(Path(root))
        return True
    except ValueError:
        return False


def _validate_cross_plane_authority(
    source_pair: dict[str, Any],
    destination_pair: dict[str, Any],
    session_plan: dict[str, Any],
    path_map: dict[str, Any],
) -> None:
    if source_pair["host_authority_id"] == destination_pair["host_authority_id"]:
        raise BulkloadError("source and destination host authorities must differ")
    if (
        session_plan["source"]["host_authority_id"] != source_pair["host_authority_id"]
        or session_plan["destination"]["host_authority_id"]
        != destination_pair["host_authority_id"]
    ):
        raise BulkloadError("session and private host authorities are cross-wired")
    if (
        path_map["source_host_authority_id"] != source_pair["host_authority_id"]
        or path_map["destination_host_authority_id"]
        != destination_pair["host_authority_id"]
        or path_map["codex_version"]
        != {
            "source": source_pair["codex_version"],
            "destination": destination_pair["codex_version"],
        }
        or path_map["session_union_plan_sha256"] != session_plan["plan_sha256"]
        or path_map["source_session_catalog_sha256"]
        != session_plan["source"]["catalog_sha256"]
        or path_map["destination_session_catalog_sha256"]
        != session_plan["destination"]["catalog_sha256"]
        or path_map["source_session_root"] != session_plan["source"]["resolved_root"]
        or path_map["destination_session_root"]
        != session_plan["destination"]["resolved_root"]
    ):
        raise BulkloadError("SQLite path map does not bind the session closure")
    for role, pair in (
        ("source", source_pair),
        ("destination", destination_pair),
    ):
        session_root = path_map[f"{role}_session_root"]
        codex_root = pair["stable_projection"]["codex_home"]["resolved_path"]
        if not _path_is_within(session_root, codex_root):
            raise BulkloadError(
                f"SQLite {role} session root escapes the Codex-home authority"
            )


def _schema_sql_digest(value: Any) -> str | None:
    if value is None:
        return None
    if not isinstance(value, str):
        raise BulkloadError("SQLite schema SQL is not text")
    return sha256_bytes(value.encode("utf-8"))


def _sql_has_keyword(value: Any, keyword: str) -> bool:
    if not isinstance(value, str):
        return False
    index = 0
    while index < len(value):
        character = value[index]
        if character == "-" and value[index : index + 2] == "--":
            newline = value.find("\n", index + 2)
            index = len(value) if newline < 0 else newline + 1
            continue
        if character == "/" and value[index : index + 2] == "/*":
            close = value.find("*/", index + 2)
            index = len(value) if close < 0 else close + 2
            continue
        if character in {"'", '"', "`", "["}:
            closing = "]" if character == "[" else character
            index += 1
            while index < len(value):
                if value[index] != closing:
                    index += 1
                    continue
                if (
                    closing != "]"
                    and index + 1 < len(value)
                    and value[index + 1] == closing
                ):
                    index += 2
                    continue
                index += 1
                break
            continue
        if character.isalpha() or character == "_":
            end = index + 1
            while end < len(value) and (value[end].isalnum() or value[end] == "_"):
                end += 1
            if value[index:end].casefold() == keyword.casefold():
                return True
            index = end
            continue
        index += 1
    return False


def _affinity(declared_type: str) -> str:
    value = declared_type.upper()
    if "INT" in value:
        return "INTEGER"
    if any(token in value for token in ("CHAR", "CLOB", "TEXT")):
        return "TEXT"
    if "BLOB" in value or not value:
        return "BLOB"
    if any(token in value for token in ("REAL", "FLOA", "DOUB")):
        return "REAL"
    return "NUMERIC"


def _quote_identifier(value: str) -> str:
    _require_identifier(value, "SQLite identifier")
    return f'"{value}"'


def _require_sqlite_deadline(deadline: float, label: str) -> None:
    if time.monotonic() > deadline:
        raise BulkloadError(f"{label} exceeded its deadline")


@contextmanager
def _sqlite_deadline_guard(
    connection: sqlite3.Connection,
    *,
    deadline: float,
    label: str,
) -> Iterator[None]:
    _require_sqlite_deadline(deadline, label)

    def interrupt_after_deadline() -> int:
        return int(time.monotonic() > deadline)

    connection.set_progress_handler(interrupt_after_deadline, 1000)
    try:
        yield
        _require_sqlite_deadline(deadline, label)
    except sqlite3.OperationalError as error:
        if time.monotonic() > deadline or "interrupted" in str(error).casefold():
            raise BulkloadError(f"{label} exceeded its deadline") from error
        raise
    finally:
        connection.set_progress_handler(None, 0)


def _structured_schema(
    connection: sqlite3.Connection,
    *,
    deadline: float,
) -> tuple[dict[str, Any], list[dict[str, Any]], list[list[Any]]]:
    with _sqlite_deadline_guard(
        connection,
        deadline=deadline,
        label="SQLite schema classification",
    ):
        return _structured_schema_body(connection, deadline=deadline)


def _structured_schema_body(
    connection: sqlite3.Connection,
    *,
    deadline: float,
) -> tuple[dict[str, Any], list[dict[str, Any]], list[list[Any]]]:
    blockers: list[dict[str, Any]] = []
    for (internal_table,) in connection.execute(
        """
        SELECT name
        FROM sqlite_schema
        WHERE type = 'table' AND name LIKE 'sqlite_%'
        ORDER BY name
        """
    ):
        blockers.append(
            {
                "code": "sqlite-internal-table-state-not-classified",
                "name": str(internal_table),
            }
        )
    schema_rows = list(
        connection.execute(
            """
            SELECT type, name, tbl_name, sql
            FROM sqlite_schema
            WHERE name NOT LIKE 'sqlite_%'
            ORDER BY type, name
            """
        )
    )
    schema_records: list[dict[str, Any]] = []
    sql_by_key: dict[tuple[str, str], Any] = {}
    for object_type, name, table, sql in schema_rows:
        if (
            not isinstance(object_type, str)
            or not isinstance(name, str)
            or not isinstance(table, str)
            or _IDENTIFIER.fullmatch(name) is None
            or _IDENTIFIER.fullmatch(table) is None
        ):
            blockers.append({"code": "sqlite-unsupported-schema-identifier"})
            continue
        key = (object_type, name)
        if key in sql_by_key:
            blockers.append({"code": "sqlite-duplicate-schema-object"})
            continue
        sql_by_key[key] = sql
        schema_records.append(
            {
                "type": object_type,
                "name": name,
                "table": table,
                "sql_sha256": _schema_sql_digest(sql),
            }
        )
    raw_schema_sha256 = sha256_bytes(canonical_bytes(schema_records))

    try:
        table_list = list(
            connection.execute(
                """
                SELECT schema, name, type, ncol, wr, strict
                FROM pragma_table_list
                WHERE schema = 'main' AND name NOT LIKE 'sqlite_%'
                ORDER BY name
                """
            )
        )
    except sqlite3.DatabaseError as error:
        if time.monotonic() > deadline or "interrupted" in str(error).casefold():
            raise BulkloadError(
                "SQLite schema classification exceeded its deadline"
            ) from error
        raise BulkloadError("SQLite table_list authority is unavailable") from error
    tables: list[dict[str, Any]] = []
    table_names: list[str] = []
    omitted_table_objects: list[dict[str, str]] = []
    observed_collations: set[str] = set()
    for schema_name, name, object_type, ncol, without_rowid, strict in table_list:
        if time.monotonic() > deadline:
            raise BulkloadError("SQLite schema classification exceeded its deadline")
        if (
            schema_name != "main"
            or not isinstance(name, str)
            or _IDENTIFIER.fullmatch(name) is None
            or object_type not in {"table", "view", "virtual", "shadow"}
        ):
            blockers.append({"code": "sqlite-unsupported-table-object"})
            continue
        if object_type in {"virtual", "shadow"}:
            omitted_table_objects.append(
                {
                    "name": name,
                    "type": object_type,
                }
            )
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
        create_sql = sql_by_key.get(("table", name))
        if _sql_has_keyword(create_sql, "COLLATE"):
            blockers.append(
                {
                    "code": "sqlite-column-collation-semantics-not-classified",
                    "table": name,
                }
            )
        columns: list[dict[str, Any]] = []
        for row in connection.execute(
            'SELECT cid, name, type, "notnull", dflt_value, pk, hidden '
            "FROM pragma_table_xinfo(?) ORDER BY cid",
            (name,),
        ):
            cid, column_name, declared_type, not_null, default, pk, hidden = row
            if (
                not isinstance(column_name, str)
                or _IDENTIFIER.fullmatch(column_name) is None
                or hidden not in {0, 1, 2, 3}
            ):
                blockers.append(
                    {
                        "code": "sqlite-unsupported-column",
                        "table": name,
                    }
                )
                continue
            declared = str(declared_type or "")
            columns.append(
                {
                    "cid": int(cid),
                    "name": column_name,
                    "declared_type": declared,
                    "affinity": _affinity(declared),
                    "not_null": bool(not_null),
                    "default_sql_sha256": _schema_sql_digest(default),
                    "primary_key_position": int(pk),
                    "hidden": int(hidden),
                    "generated_kind": {
                        0: "none",
                        1: "hidden",
                        2: "virtual",
                        3: "stored",
                    }[int(hidden)],
                }
            )
        indexes: list[dict[str, Any]] = []
        for index_row in connection.execute(
            'SELECT seq, name, "unique", origin, partial '
            "FROM pragma_index_list(?) ORDER BY name",
            (name,),
        ):
            _, index_name, unique, origin, partial = index_row
            if (
                not isinstance(index_name, str)
                or _IDENTIFIER.fullmatch(index_name) is None
            ):
                blockers.append({"code": "sqlite-unsupported-index", "table": name})
                continue
            terms: list[dict[str, Any]] = []
            for term in connection.execute(
                'SELECT seqno, cid, name, "desc", coll, key '
                "FROM pragma_index_xinfo(?) ORDER BY seqno",
                (index_name,),
            ):
                seqno, cid, term_name, descending, collation, key_term = term
                if term_name is not None and (
                    not isinstance(term_name, str)
                    or _IDENTIFIER.fullmatch(term_name) is None
                ):
                    blockers.append(
                        {"code": "sqlite-unsupported-index-term", "table": name}
                    )
                if collation is not None:
                    observed_collations.add(str(collation))
                terms.append(
                    {
                        "sequence": int(seqno),
                        "cid": int(cid),
                        "name": term_name,
                        "descending": bool(descending),
                        "collation": collation,
                        "key": bool(key_term),
                    }
                )
            indexes.append(
                {
                    "name": index_name,
                    "unique": bool(unique),
                    "origin": str(origin),
                    "partial": bool(partial),
                    "terms": terms,
                    "create_sql_sha256": _schema_sql_digest(
                        sql_by_key.get(("index", index_name))
                    ),
                }
            )
        foreign_keys: list[dict[str, Any]] = []
        for foreign_key in connection.execute(
            'SELECT id, seq, "table", "from", "to", on_update, '
            "on_delete, match FROM pragma_foreign_key_list(?) ORDER BY id, seq",
            (name,),
        ):
            (
                foreign_id,
                sequence,
                referenced_table,
                from_column,
                to_column,
                on_update,
                on_delete,
                match,
            ) = foreign_key
            supported = (
                isinstance(referenced_table, str)
                and _IDENTIFIER.fullmatch(referenced_table) is not None
                and isinstance(from_column, str)
                and _IDENTIFIER.fullmatch(from_column) is not None
                and isinstance(to_column, str)
                and _IDENTIFIER.fullmatch(to_column) is not None
                and all(
                    isinstance(action, str) and bool(action)
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
                        referenced_table if isinstance(referenced_table, str) else ""
                    ),
                    "from_column": (
                        from_column if isinstance(from_column, str) else ""
                    ),
                    "to_column": to_column if isinstance(to_column, str) else "",
                    "on_update": on_update if isinstance(on_update, str) else "",
                    "on_delete": on_delete if isinstance(on_delete, str) else "",
                    "match": match if isinstance(match, str) else "",
                    "supported": supported,
                }
            )
        tables.append(
            {
                "name": name,
                "type": "table",
                "ncol": int(ncol),
                "without_rowid": bool(without_rowid),
                "strict": bool(strict),
                "create_sql_sha256": _schema_sql_digest(create_sql),
                "columns": columns,
                "indexes": indexes,
                "foreign_keys": foreign_keys,
            }
        )
        table_names.append(name)

    triggers = [
        {
            "name": record["name"],
            "table": record["table"],
            "sql_sha256": record["sql_sha256"],
        }
        for record in schema_records
        if record["type"] == "trigger"
    ]
    views = [
        {
            "name": record["name"],
            "sql_sha256": record["sql_sha256"],
        }
        for record in schema_records
        if record["type"] == "view"
    ]
    migrations: list[list[Any]] = []
    if "_sqlx_migrations" in table_names:
        for row in connection.execute(
            """
            SELECT
                version,
                typeof(version),
                description,
                typeof(description),
                hex(checksum),
                typeof(checksum),
                success,
                typeof(success)
            FROM _sqlx_migrations
            ORDER BY version
            """
        ):
            (
                version,
                version_type,
                description,
                description_type,
                checksum_hex,
                checksum_type,
                success,
                success_type,
            ) = row
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
            migrations.append([version, description, checksum_hex, success == 1])
        versions = [migration[0] for migration in migrations]
        if versions != sorted(set(versions)):
            blockers.append({"code": "sqlite-migrations-not-strictly-ordered"})
        if any(migration[3] is not True for migration in migrations):
            blockers.append({"code": "sqlite-migration-not-successful"})
    blockers = _dedupe_blockers(blockers)
    contract: dict[str, Any] = {
        "application_id": int(
            connection.execute("PRAGMA application_id").fetchone()[0]
        ),
        "user_version": int(connection.execute("PRAGMA user_version").fetchone()[0]),
        "raw_schema_sha256": raw_schema_sha256,
        "schema_records": schema_records,
        "omitted_table_objects": omitted_table_objects,
        "classification_blockers": blockers,
        "tables": tables,
        "triggers": triggers,
        "views": views,
        "collations": sorted(observed_collations),
    }
    contract["schema_contract_sha256"] = sha256_bytes(canonical_bytes(contract))
    return contract, blockers, migrations


def _observed_edge_rules(
    schema: dict[str, Any],
) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    rules: list[dict[str, Any]] = []
    blockers: list[dict[str, Any]] = []
    for table in schema["tables"]:
        groups: dict[int, list[dict[str, Any]]] = {}
        for foreign_key in table["foreign_keys"]:
            groups.setdefault(foreign_key["id"], []).append(foreign_key)
        for foreign_id, records in sorted(groups.items()):
            records = sorted(records, key=lambda record: record["sequence"])
            if (
                [record["sequence"] for record in records] != list(range(len(records)))
                or not all(record["supported"] for record in records)
                or len(
                    {
                        (
                            record["referenced_table"],
                            record["on_update"],
                            record["on_delete"],
                            record["match"],
                        )
                        for record in records
                    }
                )
                != 1
            ):
                blockers.append(
                    {
                        "code": "sqlite-foreign-key-shape-unsupported",
                        "table": table["name"],
                        "id": foreign_id,
                    }
                )
                continue
            first = records[0]
            rules.append(
                {
                    "table": table["name"],
                    "from_columns": [record["from_column"] for record in records],
                    "referenced_table": first["referenced_table"],
                    "referenced_columns": [record["to_column"] for record in records],
                    "on_update": first["on_update"],
                    "on_delete": first["on_delete"],
                    "match": first["match"],
                    "required": True,
                }
            )
    return (
        sorted(rules, key=canonical_bytes),
        _dedupe_blockers(blockers),
    )


@contextmanager
def _open_pinned_snapshot(
    bundle_root: Path,
    basename: str,
) -> Iterator[sqlite3.Connection]:
    path = bundle_root / "sqlite" / basename
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise BulkloadError("cannot pin private SQLite snapshot") from error
    connection: sqlite3.Connection | None = None
    try:
        before = os.fstat(descriptor)
        path_before = os.stat(path, follow_symlinks=False)
        if (
            not stat.S_ISREG(before.st_mode)
            or before.st_uid != os.getuid()
            or before.st_nlink != 1
            or stat.S_IMODE(before.st_mode) != 0o600
            or (before.st_dev, before.st_ino)
            != (path_before.st_dev, path_before.st_ino)
        ):
            raise BulkloadError("private SQLite snapshot custody is invalid")
        descriptor_path = f"/dev/fd/{descriptor}"
        if not Path(descriptor_path).exists():
            raise BulkloadError("descriptor-backed SQLite access is unavailable")
        uri = f"file:{quote(descriptor_path, safe='/')}?mode=ro&immutable=1"
        try:
            connection = sqlite3.connect(uri, uri=True)
            connection.execute("PRAGMA query_only=ON")
            yield connection
        except sqlite3.DatabaseError as error:
            raise BulkloadError(
                "cannot inspect pinned private SQLite snapshot"
            ) from error
        finally:
            if connection is not None:
                connection.close()
        after = os.fstat(descriptor)
        path_after = os.stat(path, follow_symlinks=False)

        def stable(info: os.stat_result) -> tuple[int, ...]:
            return (
                info.st_dev,
                info.st_ino,
                info.st_mode,
                info.st_nlink,
                info.st_size,
                info.st_mtime_ns,
                info.st_ctime_ns,
            )

        if stable(before) != stable(after) or stable(before) != stable(path_after):
            raise BulkloadError("private SQLite snapshot changed during planning")
    finally:
        os.close(descriptor)


def _migration_relation(
    source: list[list[Any]],
    destination: list[list[Any]],
) -> tuple[str, dict[str, Any]]:
    source_digest = sha256_bytes(canonical_bytes(source))
    destination_digest = sha256_bytes(canonical_bytes(destination))
    common = 0
    for source_item, destination_item in zip(source, destination):
        if canonical_bytes(source_item) != canonical_bytes(destination_item):
            break
        common += 1
    details = {
        "source_count": len(source),
        "destination_count": len(destination),
        "source_latest_migration": source[-1][0] if source else None,
        "destination_latest_migration": (destination[-1][0] if destination else None),
        "common_prefix_count": common,
        "common_prefix_sha256": sha256_bytes(canonical_bytes(source[:common])),
        "source_tail_sha256": sha256_bytes(canonical_bytes(source[common:])),
        "source_migrations_sha256": source_digest,
        "destination_migrations_sha256": destination_digest,
    }
    if source == destination:
        return "exact", details
    if len(source) > len(destination) and source[: len(destination)] == destination:
        return "registered-prefix-upgrade", details
    return "blocked", details


def _typed_value(value_type: str, value: Any) -> bytes:
    if value_type == "null" and value is None:
        payload = b""
        tag = b"N"
    elif value_type == "integer" and type(value) is int:
        try:
            payload = struct.pack(">q", value)
        except struct.error as error:
            raise BulkloadError(
                "SQLite INTEGER is outside signed 64-bit range"
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
        raise BulkloadError("SQLite value/type contract is invalid")
    return tag + len(payload).to_bytes(8, "big") + payload


def _session_paths(
    session_plan: dict[str, Any],
) -> dict[str, dict[str, str]]:
    paths: dict[str, dict[str, str]] = {"source": {}, "destination": {}}

    def add(role: str, session_id: Any, relative_path: Any) -> None:
        if (
            not isinstance(session_id, str)
            or not isinstance(relative_path, str)
            or Path(relative_path).is_absolute()
            or os.path.normpath(relative_path) != relative_path
            or relative_path in {"", ".", ".."}
            or ".." in Path(relative_path).parts
        ):
            raise BulkloadError("Codex session plan path binding is invalid")
        previous = paths[role].get(session_id)
        if previous is not None and previous != relative_path:
            raise BulkloadError("Codex session plan repeats one UUID at two paths")
        paths[role][session_id] = relative_path

    for item in session_plan["intent"]["copy_if_absent"]:
        add("source", item.get("session_id"), item.get("source_relative_path"))
    for key in (
        "promote_source_superset",
        "exact_common",
        "preserve_destination_superset",
    ):
        for item in session_plan["intent"][key]:
            add("source", item.get("session_id"), item.get("source_relative_path"))
            add(
                "destination",
                item.get("session_id"),
                item.get("destination_relative_path"),
            )
    for item in session_plan["intent"]["preserve_destination"]:
        add("destination", item.get("session_id"), item.get("relative_path"))
    return paths


def _translate_rollout_path(
    value: Any,
    *,
    role: str,
    source_root: str,
    destination_root: str,
    expected_relative_path: str,
) -> str:
    if not isinstance(value, str):
        raise BulkloadError("SQLite rollout path is not text")
    _require_absolute_normal_path(value, "SQLite rollout path")
    own_root = source_root if role == "source" else destination_root
    if not _path_is_within(value, own_root):
        if value.casefold().startswith(own_root.casefold()):
            raise BulkloadError("SQLite rollout path is a casefold/prefix near miss")
        raise BulkloadError("SQLite rollout path escapes the session authority")
    relative = Path(value).relative_to(Path(own_root))
    if os.fspath(relative) != expected_relative_path:
        raise BulkloadError(
            "SQLite rollout path does not match its accepted session UUID"
        )
    return os.fspath(Path(destination_root) / relative)


def _path_rules_for_table(
    basename: str,
    table: str,
    registry_family: dict[str, Any],
    path_map: dict[str, Any],
) -> list[dict[str, Any]]:
    registered = [
        rule for rule in registry_family["path_authorities"] if rule["table"] == table
    ]
    mapped = [
        {
            "table": rule["table"],
            "session_id_column": rule["session_id_column"],
            "column": rule["column"],
            "kind": rule["kind"],
        }
        for rule in path_map["rules"]
        if rule["family_basename"] == basename and rule["table"] == table
    ]
    if registered != mapped:
        raise BulkloadError("SQLite registry and path-map authorities differ")
    return registered


def _preflight_semantic_rows(
    connection: sqlite3.Connection,
    *,
    table: str,
    columns: list[str],
    deadline: float,
) -> None:
    with _sqlite_deadline_guard(
        connection,
        deadline=deadline,
        label="SQLite semantic preflight",
    ):
        _preflight_semantic_rows_body(
            connection,
            table=table,
            columns=columns,
            deadline=deadline,
        )


def _preflight_semantic_rows_body(
    connection: sqlite3.Connection,
    *,
    table: str,
    columns: list[str],
    deadline: float,
) -> None:
    probes: list[str] = []
    for column in columns:
        quoted = _quote_identifier(column)
        probes.extend(
            (
                f"typeof({quoted})",
                "CASE "
                f"WHEN typeof({quoted}) = 'null' THEN 0 "
                f"WHEN typeof({quoted}) IN ('integer', 'real') THEN 8 "
                f"ELSE length(CAST({quoted} AS BLOB)) END",
            )
        )
    query = f"SELECT {', '.join(probes)} FROM {_quote_identifier(table)}"
    rows = 0
    charged_bytes = 0
    try:
        for raw in connection.execute(query):
            if time.monotonic() > deadline:
                raise BulkloadError("SQLite semantic preflight exceeded its deadline")
            for index in range(len(columns)):
                value_type = raw[index * 2]
                byte_length = raw[index * 2 + 1]
                if (
                    value_type not in {"null", "integer", "real", "text", "blob"}
                    or type(byte_length) is not int
                    or byte_length < 0
                ):
                    raise BulkloadError("SQLite semantic preflight value is invalid")
                if byte_length > MAX_SQLITE_VALUE_BYTES:
                    raise BulkloadError("SQLite semantic value exceeds its byte budget")
                charged_bytes += 9 + byte_length
            rows += 1
            if rows > MAX_SQLITE_PLAN_ROWS or charged_bytes > MAX_SQLITE_PLAN_ROW_BYTES:
                raise BulkloadError("SQLite semantic classification budget exceeded")
    except sqlite3.Error as error:
        if time.monotonic() > deadline or "interrupted" in str(error).casefold():
            raise BulkloadError(
                "SQLite semantic preflight exceeded its deadline"
            ) from error
        raise BulkloadError("SQLite semantic preflight failed") from error


def _semantic_row_stream(
    connection: sqlite3.Connection,
    *,
    basename: str,
    role: str,
    table_contract: dict[str, Any],
    table_rule: dict[str, Any],
    path_rules: list[dict[str, Any]],
    path_map: dict[str, Any],
    accepted_session_paths: dict[str, str],
    deadline: float,
    state: dict[str, Any],
    plan_budget: dict[str, int] | None = None,
) -> Iterator[tuple[tuple[tuple[str, str], ...], bytes, bytes]]:
    if plan_budget is None:
        plan_budget = {"rows": 0, "bytes": 0}
    columns = [column["name"] for column in table_contract["columns"]]
    _preflight_semantic_rows(
        connection,
        table=table_rule["name"],
        columns=columns,
        deadline=deadline,
    )
    with _sqlite_deadline_guard(
        connection,
        deadline=deadline,
        label="SQLite semantic classification",
    ):
        yield from _semantic_row_stream_body(
            connection,
            basename=basename,
            role=role,
            table_contract=table_contract,
            table_rule=table_rule,
            path_rules=path_rules,
            path_map=path_map,
            accepted_session_paths=accepted_session_paths,
            deadline=deadline,
            state=state,
            plan_budget=plan_budget,
        )


def _semantic_row_stream_body(
    connection: sqlite3.Connection,
    *,
    basename: str,
    role: str,
    table_contract: dict[str, Any],
    table_rule: dict[str, Any],
    path_rules: list[dict[str, Any]],
    path_map: dict[str, Any],
    accepted_session_paths: dict[str, str],
    deadline: float,
    state: dict[str, Any],
    plan_budget: dict[str, int],
) -> Iterator[tuple[tuple[tuple[str, str], ...], bytes, bytes]]:
    columns = [column["name"] for column in table_contract["columns"]]
    identities = table_rule["identity_columns"]
    if not set(identities) <= set(columns):
        raise BulkloadError("SQLite registered identity column is absent")
    select_parts: list[str] = []
    for column in columns:
        quoted = _quote_identifier(column)
        select_parts.extend((f"typeof({quoted})", quoted))
    ordering: list[str] = []
    for column in identities:
        quoted = _quote_identifier(column)
        ordering.extend((f"typeof({quoted})", f"hex(CAST({quoted} AS BLOB))"))
    select_parts.extend(ordering)
    query = (
        f"SELECT {', '.join(select_parts)} FROM {_quote_identifier(table_rule['name'])} "
        f"ORDER BY {', '.join(ordering)}"
    )
    previous_identity: bytes | None = None
    previous_sort_key: tuple[tuple[str, str], ...] | None = None
    digest = hashlib.sha256()
    digest.update(canonical_bytes({"table": table_rule["name"], "columns": columns}))
    count = 0
    charged_bytes = 0
    path_by_column = {rule["column"]: rule for rule in path_rules}
    previous_limit = connection.setlimit(
        sqlite3.SQLITE_LIMIT_LENGTH,
        MAX_SQLITE_VALUE_BYTES,
    )
    try:
        for raw in connection.execute(query):
            if time.monotonic() > deadline:
                raise BulkloadError(
                    "SQLite semantic classification exceeded its deadline"
                )
            values: dict[str, tuple[str, Any]] = {}
            for index, column in enumerate(columns):
                values[column] = (str(raw[index * 2]), raw[index * 2 + 1])
            sort_offset = len(columns) * 2
            sort_key = tuple(
                (
                    str(raw[sort_offset + index * 2]),
                    str(raw[sort_offset + index * 2 + 1]),
                )
                for index in range(len(identities))
            )
            if any(value_type == "real" for value_type, _ in sort_key):
                raise BulkloadError("SQLite REAL columns cannot be row identities")
            if previous_sort_key is not None and sort_key < previous_sort_key:
                raise BulkloadError("SQLite row identity order is not monotonic")
            for column, rule in path_by_column.items():
                session_type, session_value = values[rule["session_id_column"]]
                if (
                    session_type != "text"
                    or session_value not in accepted_session_paths
                ):
                    raise BulkloadError(
                        "SQLite path authority names a session outside "
                        "the accepted union"
                    )
                value_type, value = values[column]
                if value_type != "text":
                    raise BulkloadError("SQLite rollout-path column is not text")
                values[column] = (
                    "text",
                    _translate_rollout_path(
                        value,
                        role=role,
                        source_root=path_map["source_session_root"],
                        destination_root=path_map["destination_session_root"],
                        expected_relative_path=accepted_session_paths[session_value],
                    ),
                )
            identity = b"".join(_typed_value(*values[column]) for column in identities)
            if previous_identity == identity:
                raise BulkloadError("SQLite row identity is duplicated")
            if previous_sort_key == sort_key and previous_identity != identity:
                raise BulkloadError("SQLite row identity sort key is ambiguous")
            previous_identity = identity
            previous_sort_key = sort_key
            row_payload = canonical_bytes(
                {
                    "basename": basename,
                    "table": table_rule["name"],
                    "columns": columns,
                }
            ) + b"".join(_typed_value(*values[column]) for column in columns)
            row_digest = hashlib.sha256(row_payload).digest()
            digest.update(len(identity).to_bytes(8, "big"))
            digest.update(identity)
            digest.update(row_digest)
            count += 1
            row_bytes = len(identity) + len(row_payload)
            charged_bytes += row_bytes
            plan_budget["rows"] += 1
            plan_budget["bytes"] += row_bytes
            if (
                plan_budget["rows"] > MAX_SQLITE_PLAN_ROWS
                or plan_budget["bytes"] > MAX_SQLITE_PLAN_ROW_BYTES
            ):
                raise BulkloadError("SQLite semantic classification budget exceeded")
            yield sort_key, identity, row_digest
    except sqlite3.Error as error:
        if time.monotonic() > deadline or "interrupted" in str(error).casefold():
            raise BulkloadError(
                "SQLite semantic classification exceeded its deadline"
            ) from error
        raise BulkloadError("SQLite semantic row exceeds its fetch budget") from error
    finally:
        connection.setlimit(sqlite3.SQLITE_LIMIT_LENGTH, previous_limit)
    state.update(
        {
            "row_count": count,
            "semantic_rows_sha256": digest.hexdigest(),
            "classified_bytes": charged_bytes,
        }
    )


def _next_or_none(
    iterator: Iterator[tuple[tuple[tuple[str, str], ...], bytes, bytes]],
) -> tuple[tuple[tuple[str, str], ...], bytes, bytes] | None:
    try:
        return next(iterator)
    except StopIteration:
        return None


def _table_unique_contract(
    table_contract: dict[str, Any],
    identity_columns: list[str],
) -> tuple[bool, bool, bool]:
    columns_by_name = {column["name"]: column for column in table_contract["columns"]}
    primary_key = [
        column["name"]
        for column in sorted(
            (
                column
                for column in table_contract["columns"]
                if column["primary_key_position"] > 0
            ),
            key=lambda column: column["primary_key_position"],
        )
    ]
    rowid_identity = (
        not table_contract["without_rowid"]
        and len(primary_key) == 1
        and primary_key == identity_columns
        and columns_by_name[primary_key[0]]["affinity"] == "INTEGER"
    )
    identity_is_unique = rowid_identity
    identity_semantics_safe = rowid_identity
    has_unclassified_unique = False
    for index in table_contract["indexes"]:
        if not index["unique"]:
            continue
        key_terms = [term for term in index["terms"] if term["key"]]
        columns = [term["name"] for term in key_terms]
        if index["partial"] or any(column is None for column in columns):
            has_unclassified_unique = True
            continue
        if columns != identity_columns:
            has_unclassified_unique = True
            continue
        identity_is_unique = True
        safe_collations = all(term["collation"] == "BINARY" for term in key_terms)
        safe_columns = all(
            columns_by_name[column]["not_null"]
            and columns_by_name[column]["affinity"] in {"INTEGER", "TEXT", "BLOB"}
            for column in identity_columns
        )
        safe_storage = table_contract["strict"]
        if safe_collations and safe_columns and safe_storage:
            identity_semantics_safe = True
        else:
            has_unclassified_unique = True
    if primary_key and primary_key != identity_columns:
        has_unclassified_unique = True
    return (
        identity_is_unique,
        identity_semantics_safe,
        has_unclassified_unique,
    )


def _classify_table(
    source: sqlite3.Connection,
    destination: sqlite3.Connection,
    *,
    basename: str,
    source_contract: dict[str, Any],
    destination_contract: dict[str, Any],
    table_rule: dict[str, Any],
    registry_family: dict[str, Any],
    path_map: dict[str, Any],
    accepted_session_paths: dict[str, dict[str, str]],
    deadline: float,
    plan_budget: dict[str, int] | None = None,
) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    if plan_budget is None:
        plan_budget = {"rows": 0, "bytes": 0}
    blockers: list[dict[str, Any]] = []
    source_table = next(
        (
            item
            for item in source_contract["tables"]
            if item["name"] == table_rule["name"]
        ),
        None,
    )
    destination_table = next(
        (
            item
            for item in destination_contract["tables"]
            if item["name"] == table_rule["name"]
        ),
        None,
    )
    if source_table is None or destination_table is None:
        return (
            {
                "name": table_rule["name"],
                "merge_class": table_rule["merge_class"],
                "identity_columns": table_rule["identity_columns"],
                "source": None,
                "destination": None,
                "shared_equal": 0,
                "source_only": 0,
                "destination_only": 0,
                "conflicts": 0,
                "semantic_classification_complete": False,
            },
            [{"code": "sqlite-registered-table-missing", "table": table_rule["name"]}],
        )
    if table_rule["merge_class"] in {"append-multiset", "unsupported"}:
        return (
            {
                "name": table_rule["name"],
                "merge_class": table_rule["merge_class"],
                "identity_columns": table_rule["identity_columns"],
                "source": None,
                "destination": None,
                "shared_equal": 0,
                "source_only": 0,
                "destination_only": 0,
                "conflicts": 0,
                "semantic_classification_complete": False,
            },
            [
                {
                    "code": "sqlite-table-merge-class-not-classifiable",
                    "table": table_rule["name"],
                    "merge_class": table_rule["merge_class"],
                }
            ],
        )
    (
        identity_is_unique,
        identity_semantics_safe,
        has_unclassified_unique,
    ) = _table_unique_contract(
        source_table,
        table_rule["identity_columns"],
    )
    (
        destination_identity_is_unique,
        destination_identity_semantics_safe,
        destination_has_unclassified_unique,
    ) = _table_unique_contract(
        destination_table,
        table_rule["identity_columns"],
    )
    if not identity_is_unique or not destination_identity_is_unique:
        return (
            {
                "name": table_rule["name"],
                "merge_class": table_rule["merge_class"],
                "identity_columns": table_rule["identity_columns"],
                "source": None,
                "destination": None,
                "shared_equal": 0,
                "source_only": 0,
                "destination_only": 0,
                "conflicts": 0,
                "semantic_classification_complete": False,
            },
            [
                {
                    "code": "sqlite-identity-is-not-an-exact-unique-key",
                    "table": table_rule["name"],
                }
            ],
        )
    if not identity_semantics_safe or not destination_identity_semantics_safe:
        return (
            {
                "name": table_rule["name"],
                "merge_class": table_rule["merge_class"],
                "identity_columns": table_rule["identity_columns"],
                "source": None,
                "destination": None,
                "shared_equal": 0,
                "source_only": 0,
                "destination_only": 0,
                "conflicts": 0,
                "semantic_classification_complete": False,
            },
            [
                {
                    "code": "sqlite-identity-unique-semantics-unsupported",
                    "table": table_rule["name"],
                }
            ],
        )
    if has_unclassified_unique or destination_has_unclassified_unique:
        blockers.append(
            {
                "code": "sqlite-secondary-unique-closure-not-implemented",
                "table": table_rule["name"],
            }
        )
    path_rules = _path_rules_for_table(
        basename,
        table_rule["name"],
        registry_family,
        path_map,
    )
    source_state: dict[str, Any] = {}
    destination_state: dict[str, Any] = {}
    source_rows = _semantic_row_stream(
        source,
        basename=basename,
        role="source",
        table_contract=source_table,
        table_rule=table_rule,
        path_rules=path_rules,
        path_map=path_map,
        accepted_session_paths=accepted_session_paths["source"],
        deadline=deadline,
        state=source_state,
        plan_budget=plan_budget,
    )
    destination_rows = _semantic_row_stream(
        destination,
        basename=basename,
        role="destination",
        table_contract=destination_table,
        table_rule=table_rule,
        path_rules=path_rules,
        path_map=path_map,
        accepted_session_paths=accepted_session_paths["destination"],
        deadline=deadline,
        state=destination_state,
        plan_budget=plan_budget,
    )
    shared_equal = source_only = destination_only = conflicts = 0
    try:
        source_item = _next_or_none(source_rows)
        destination_item = _next_or_none(destination_rows)
        while source_item is not None or destination_item is not None:
            if destination_item is None or (
                source_item is not None and source_item[0] < destination_item[0]
            ):
                source_only += 1
                source_item = _next_or_none(source_rows)
            elif source_item is None or destination_item[0] < source_item[0]:
                destination_only += 1
                destination_item = _next_or_none(destination_rows)
            else:
                if source_item[1] != destination_item[1]:
                    raise BulkloadError("SQLite row identity sort key is ambiguous")
                if source_item[2] == destination_item[2]:
                    shared_equal += 1
                else:
                    conflicts += 1
                source_item = _next_or_none(source_rows)
                destination_item = _next_or_none(destination_rows)
    finally:
        source_rows.close()
        destination_rows.close()
    if conflicts:
        blockers.append(
            {
                "code": "sqlite-shared-row-divergence",
                "table": table_rule["name"],
                "count": conflicts,
            }
        )
    if table_rule["merge_class"] == "exact" and (
        source_only or destination_only or conflicts
    ):
        blockers.append(
            {
                "code": "sqlite-exact-table-divergence",
                "table": table_rule["name"],
            }
        )
    return (
        {
            "name": table_rule["name"],
            "merge_class": table_rule["merge_class"],
            "identity_columns": table_rule["identity_columns"],
            "source": source_state,
            "destination": destination_state,
            "shared_equal": shared_equal,
            "source_only": source_only,
            "destination_only": destination_only,
            "conflicts": conflicts,
            "semantic_classification_complete": not blockers,
        },
        blockers,
    )


def _artifact_binding(
    capture: dict[str, Any],
    family: dict[str, Any],
) -> dict[str, Any]:
    return {
        "capture_sha256": capture["capture_sha256"],
        "snapshot_sha256": family["sha256"],
        "snapshot_size": family["snapshot_size"],
        "source_sha256": family["source_sha256"],
        "source_size": family["source_size"],
        "schema_sha256": family["schema_sha256"],
        "migrations_sha256": family["migrations_sha256"],
        "application_id": family["application_id"],
        "user_version": family["user_version"],
    }


def _dedupe_blockers(
    blockers: list[dict[str, Any]],
) -> list[dict[str, Any]]:
    by_encoding = {canonical_bytes(blocker): blocker for blocker in blockers}
    return [by_encoding[encoding] for encoding in sorted(by_encoding)]


def _classify_family(
    basename: str,
    source_capture: dict[str, Any],
    destination_capture: dict[str, Any],
    source_root: Path,
    destination_root: Path,
    registry_family: dict[str, Any] | None,
    path_map: dict[str, Any],
    session_plan: dict[str, Any],
    *,
    deadline: float,
    plan_budget: dict[str, int],
) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    _require_sqlite_deadline(deadline, "SQLite family classification")
    source_family = next(
        item
        for item in source_capture["sqlite_families"]
        if item["basename"] == basename
    )
    destination_family = next(
        item
        for item in destination_capture["sqlite_families"]
        if item["basename"] == basename
    )
    role, generation = _family_identity(basename, allow_unknown=True)
    blockers: list[dict[str, Any]] = []
    if role == "unsupported":
        blockers.append({"code": "sqlite-family-unsupported"})
    with _open_pinned_snapshot(source_root, basename) as source_connection:
        source_schema, source_blockers, source_migrations = _structured_schema(
            source_connection,
            deadline=deadline,
        )
        blockers.extend({"role": "source", **item} for item in source_blockers)
        source_edges, source_edge_blockers = _observed_edge_rules(source_schema)
        blockers.extend({"role": "source", **item} for item in source_edge_blockers)
        with _open_pinned_snapshot(
            destination_root,
            basename,
        ) as destination_connection:
            (
                destination_schema,
                destination_blockers,
                destination_migrations,
            ) = _structured_schema(
                destination_connection,
                deadline=deadline,
            )
            blockers.extend(
                {"role": "destination", **item} for item in destination_blockers
            )
            destination_edges, destination_edge_blockers = _observed_edge_rules(
                destination_schema
            )
            blockers.extend(
                {"role": "destination", **item} for item in destination_edge_blockers
            )
            relation, migration_details = _migration_relation(
                source_migrations,
                destination_migrations,
            )
            _require_sqlite_deadline(
                deadline,
                "SQLite migration classification",
            )
            table_relations: list[dict[str, Any]] = []
            if source_schema["triggers"] or destination_schema["triggers"]:
                blockers.append({"code": "sqlite-trigger-semantics-not-classified"})
            if source_schema["views"] or destination_schema["views"]:
                blockers.append({"code": "sqlite-view-semantics-not-classified"})
            if registry_family is None:
                blockers.append({"code": "sqlite-adapter-not-registered"})
            else:
                expected = {
                    "source_codex_version": source_capture["codex_version"],
                    "destination_codex_version": destination_capture["codex_version"],
                    "source_schema_contract_sha256": source_schema[
                        "schema_contract_sha256"
                    ],
                    "destination_schema_contract_sha256": destination_schema[
                        "schema_contract_sha256"
                    ],
                    "source_raw_schema_sha256": source_schema["raw_schema_sha256"],
                    "destination_raw_schema_sha256": destination_schema[
                        "raw_schema_sha256"
                    ],
                    "source_migrations_sha256": migration_details[
                        "source_migrations_sha256"
                    ],
                    "destination_migrations_sha256": migration_details[
                        "destination_migrations_sha256"
                    ],
                    "source_application_id": source_schema["application_id"],
                    "destination_application_id": destination_schema["application_id"],
                    "source_user_version": source_schema["user_version"],
                    "destination_user_version": destination_schema["user_version"],
                    "migration_relation": relation,
                }
                if any(
                    registry_family[key] != value for key, value in expected.items()
                ):
                    blockers.append({"code": "sqlite-adapter-exact-key-mismatch"})
                unknown_collations = (
                    set(source_schema["collations"])
                    | set(destination_schema["collations"])
                ) - set(registry_family["allowed_collations"])
                if unknown_collations:
                    blockers.append({"code": "sqlite-unregistered-collation"})
                registry_edges = registry_family["edges"]
                if (
                    source_edges != registry_edges
                    or destination_edges != registry_edges
                ):
                    blockers.append({"code": "sqlite-edge-registry-mismatch"})
                source_tables = {item["name"] for item in source_schema["tables"]}
                destination_tables = {
                    item["name"] for item in destination_schema["tables"]
                }
                registered_tables = {item["name"] for item in registry_family["tables"]}
                if (
                    source_tables != registered_tables
                    or destination_tables != registered_tables
                ):
                    blockers.append({"code": "sqlite-table-set-unregistered"})
                exact_contracts = (
                    source_schema["schema_contract_sha256"]
                    == destination_schema["schema_contract_sha256"]
                )
                if not exact_contracts:
                    blockers.append(
                        {"code": "sqlite-schema-adapter-execution-not-implemented"}
                    )
                else:
                    for table_rule in registry_family["tables"]:
                        table_relation, table_blockers = _classify_table(
                            source_connection,
                            destination_connection,
                            basename=basename,
                            source_contract=source_schema,
                            destination_contract=destination_schema,
                            table_rule=table_rule,
                            registry_family=registry_family,
                            path_map=path_map,
                            accepted_session_paths=_session_paths(session_plan),
                            deadline=deadline,
                            plan_budget=plan_budget,
                        )
                        table_relations.append(table_relation)
                        blockers.extend(table_blockers)
                        _require_sqlite_deadline(
                            deadline,
                            "SQLite table classification",
                        )
            if (
                source_edges
                or destination_edges
                or (registry_family is not None and registry_family["edges"])
            ):
                blockers.append({"code": "sqlite-post-compose-edge-closure-required"})
    blockers = _dedupe_blockers(blockers)
    _require_sqlite_deadline(deadline, "SQLite family classification")
    for table_relation in table_relations:
        if any(blocker.get("table") == table_relation["name"] for blocker in blockers):
            table_relation["semantic_classification_complete"] = False
    relation_record = {
        "relation": relation,
        **migration_details,
        "adapter": registry_family["adapter"] if registry_family is not None else None,
    }
    return (
        {
            "basename": basename,
            "family_role": role,
            "generation": generation,
            "source_artifact": _artifact_binding(source_capture, source_family),
            "destination_artifact": _artifact_binding(
                destination_capture,
                destination_family,
            ),
            "source_schema": source_schema,
            "destination_schema": destination_schema,
            "migration_relation": relation_record,
            "path_authorities_sha256": sha256_bytes(
                canonical_bytes(
                    registry_family["path_authorities"]
                    if registry_family is not None
                    else []
                )
            ),
            "table_relations": table_relations,
            "edge_closure": {
                "registry_sha256": sha256_bytes(
                    canonical_bytes(
                        registry_family["edges"] if registry_family is not None else []
                    )
                ),
                "source_observed_sha256": sha256_bytes(canonical_bytes(source_edges)),
                "destination_observed_sha256": sha256_bytes(
                    canonical_bytes(destination_edges)
                ),
                "registry_matches_observed": bool(
                    registry_family is not None
                    and source_edges == registry_family["edges"]
                    and destination_edges == registry_family["edges"]
                ),
                "post_compose_required": bool(
                    source_edges
                    or destination_edges
                    or (registry_family is not None and registry_family["edges"])
                ),
                "proven": False,
            },
            "blockers": blockers,
            "classification_complete": not blockers,
            "composer_implemented": False,
        },
        blockers,
    )


def compile_codex_private_sqlite_compose_plan(
    compatibility_plan: dict[str, Any],
    source_a_directory: Path,
    source_b_directory: Path,
    destination_a_directory: Path,
    destination_b_directory: Path,
    *,
    accept_compatibility_plan: str,
    adapter_registry: dict[str, Any],
    accept_adapter_registry: str,
    path_map: dict[str, Any],
    accept_path_map: str,
    session_union_plan: dict[str, Any],
    accept_session_union_plan: str,
    session_source_a: dict[str, Any],
    session_source_b: dict[str, Any],
    session_destination_a: dict[str, Any],
    session_destination_b: dict[str, Any],
    session_prefix_request: dict[str, Any] | None = None,
    session_source_prefix_a: dict[str, Any] | None = None,
    session_source_prefix_b: dict[str, Any] | None = None,
    session_destination_prefix_a: dict[str, Any] | None = None,
    session_destination_prefix_b: dict[str, Any] | None = None,
    session_close_request: dict[str, Any] | None = None,
    session_source_close_a: dict[str, Any] | None = None,
    session_source_close_b: dict[str, Any] | None = None,
    session_destination_close_a: dict[str, Any] | None = None,
    session_destination_close_b: dict[str, Any] | None = None,
    runtime_authority: dict[str, Any],
) -> dict[str, Any]:
    """Compile a non-actionable SQLite classification/opening request."""
    deadline = time.monotonic() + MAX_SQLITE_PLAN_SECONDS
    private_runtime.validate_private_runtime_authority(runtime_authority)
    validate_codex_private_state_plan(compatibility_plan)
    if accept_compatibility_plan != compatibility_plan["plan_sha256"]:
        raise BulkloadError("accepted compatibility plan digest differs")
    validate_sqlite_adapter_registry(adapter_registry)
    if accept_adapter_registry != adapter_registry["registry_sha256"]:
        raise BulkloadError("accepted SQLite adapter registry digest differs")
    validate_sqlite_path_map(path_map)
    if accept_path_map != path_map["path_map_sha256"]:
        raise BulkloadError("accepted SQLite path-map digest differs")
    validate_codex_session_union_plan(session_union_plan)
    if accept_session_union_plan != session_union_plan["plan_sha256"]:
        raise BulkloadError("accepted Codex session union plan digest differs")
    validate_codex_session_union_plan_against_inputs(
        session_union_plan,
        session_source_a,
        session_source_b,
        session_destination_a,
        session_destination_b,
        prefix_request=session_prefix_request,
        source_prefix_a=session_source_prefix_a,
        source_prefix_b=session_source_prefix_b,
        destination_prefix_a=session_destination_prefix_a,
        destination_prefix_b=session_destination_prefix_b,
        close_request=session_close_request,
        source_close_a=session_source_close_a,
        source_close_b=session_source_close_b,
        destination_close_a=session_destination_close_a,
        destination_close_b=session_destination_close_b,
    )
    if (
        session_union_plan["intent"]["ready_for_attended_copy"] is not True
        or session_union_plan["intent"]["blockers"]
    ):
        raise BulkloadError("Codex session union plan is not a closed ready plan")

    source_a, source_a_root = read_codex_private_bundle(
        source_a_directory,
        "source",
    )
    source_b, source_b_root = read_codex_private_bundle(
        source_b_directory,
        "source",
    )
    destination_a, destination_a_root = read_codex_private_bundle(
        destination_a_directory,
        "destination",
    )
    destination_b, destination_b_root = read_codex_private_bundle(
        destination_b_directory,
        "destination",
    )
    validate_codex_private_state_plan_against_bundles(
        compatibility_plan,
        source_a_root,
        destination_a_root,
    )
    if compatibility_plan["runtime_authority"] != runtime_authority:
        raise BulkloadError("compatibility and SQLite planner runtime authority differ")
    source_pair = _private_pair_binding(source_a, source_b, role="source")
    destination_pair = _private_pair_binding(
        destination_a,
        destination_b,
        role="destination",
    )
    private_evidence_ids = {
        _normalize_capture_id(evidence_id)
        for evidence_id in (
            *source_pair["capture_ids"],
            *source_pair["quiescence_attestation_ids"],
            *destination_pair["capture_ids"],
            *destination_pair["quiescence_attestation_ids"],
        )
    }
    if len(private_evidence_ids) != 8:
        raise BulkloadError("private opening evidence IDs are not globally distinct")
    session_capture_ids = _session_evidence_capture_ids(
        session_source_a,
        session_source_b,
        session_destination_a,
        session_destination_b,
        source_prefix_a=session_source_prefix_a,
        source_prefix_b=session_source_prefix_b,
        destination_prefix_a=session_destination_prefix_a,
        destination_prefix_b=session_destination_prefix_b,
        source_close_a=session_source_close_a,
        source_close_b=session_source_close_b,
        destination_close_a=session_destination_close_a,
        destination_close_b=session_destination_close_b,
    )
    if private_evidence_ids & session_capture_ids:
        raise BulkloadError("private and session evidence reuse an evidence ID")
    _validate_cross_plane_authority(
        source_pair,
        destination_pair,
        session_union_plan,
        path_map,
    )
    if (
        compatibility_plan["source_capture_sha256"] != source_a["capture_sha256"]
        or compatibility_plan["destination_capture_sha256"]
        != destination_a["capture_sha256"]
    ):
        raise BulkloadError("compatibility plan does not bind private opening pass A")

    source_families = {item["basename"] for item in source_a["sqlite_families"]}
    destination_families = {
        item["basename"] for item in destination_a["sqlite_families"]
    }
    registry_by_basename = {
        item["basename"]: item for item in adapter_registry["families"]
    }
    classification_blockers: list[dict[str, Any]] = []
    family_relations: list[dict[str, Any]] = []
    if source_families != destination_families:
        classification_blockers.append(
            {
                "code": "sqlite-family-set-mismatch",
                "missing_from_source": sorted(destination_families - source_families),
                "missing_from_destination": sorted(
                    source_families - destination_families
                ),
            }
        )
    else:
        plan_budget = {"rows": 0, "bytes": 0}
        for basename in sorted(source_families):
            family_relation, blockers = _classify_family(
                basename,
                source_a,
                destination_a,
                source_a_root,
                destination_a_root,
                registry_by_basename.get(basename),
                path_map,
                session_union_plan,
                deadline=deadline,
                plan_budget=plan_budget,
            )
            family_relations.append(family_relation)
            classification_blockers.extend(
                {"basename": basename, **blocker} for blocker in blockers
            )
            _require_sqlite_deadline(
                deadline,
                "SQLite plan classification",
            )
    unobserved_registry = sorted(
        set(registry_by_basename) - source_families - destination_families
    )
    if unobserved_registry:
        classification_blockers.append(
            {
                "code": "sqlite-registry-family-unobserved",
                "basenames": unobserved_registry,
            }
        )
    plan_blockers = _dedupe_blockers(
        [
            *classification_blockers,
            {"code": "post-plan-private-close-required"},
            {"code": "sqlite-composer-not-implemented"},
            {"code": "session-union-execution-and-verification-not-implemented"},
        ]
    )
    _require_sqlite_deadline(deadline, "SQLite plan classification")
    plan: dict[str, Any] = {
        "schema": PRIVATE_SQLITE_PLAN_SCHEMA,
        "created_at": utc_now(),
        "runtime_authority": runtime_authority,
        "accepted_inputs": {
            "compatibility_plan_sha256": compatibility_plan["plan_sha256"],
            "adapter_registry_sha256": adapter_registry["registry_sha256"],
            "path_map_sha256": path_map["path_map_sha256"],
            "session_union_plan_sha256": session_union_plan["plan_sha256"],
        },
        "private_opening": {
            "source": source_pair,
            "destination": destination_pair,
        },
        "session_union": {
            "plan_sha256": session_union_plan["plan_sha256"],
            "source": session_union_plan["source"],
            "destination": session_union_plan["destination"],
            "prefix_evidence": session_union_plan["prefix_evidence"],
            "ready_for_attended_copy": True,
            "executed": False,
            "verified": False,
        },
        "adapter_registry": adapter_registry,
        "path_map": path_map,
        "sqlite_families": family_relations,
        "blockers": plan_blockers,
        "opening_stable": True,
        "post_plan_close_required": True,
        "post_plan_close_proven": False,
        "readiness": {
            "classification_complete": not classification_blockers,
            "composer_implemented": False,
            "sqlite_union_ready": False,
            "sqlite_compose": False,
            "sqlite_publish": False,
            "combined": False,
            "ready_for_apply": False,
        },
        "implementation": "sqlite-compose-opening-plan-only-v4",
    }
    plan["plan_sha256"] = object_digest(plan, "plan_sha256")
    validate_codex_private_sqlite_compose_plan(plan)
    _require_sqlite_deadline(deadline, "SQLite plan classification")
    return plan


_ROLE_BLOCKER_FIELDS = {
    "sqlite-internal-table-state-not-classified": {"name"},
    "sqlite-unsupported-schema-identifier": set(),
    "sqlite-duplicate-schema-object": set(),
    "sqlite-unsupported-table-object": set(),
    "sqlite-virtual-or-shadow-table-unsupported": {"name", "type"},
    "sqlite-column-collation-semantics-not-classified": {"table"},
    "sqlite-unsupported-column": {"table"},
    "sqlite-unsupported-index": {"table"},
    "sqlite-unsupported-index-term": {"table"},
    "sqlite-unsupported-foreign-key": {"table", "id"},
    "sqlite-migration-record-type-invalid": set(),
    "sqlite-migrations-not-strictly-ordered": set(),
    "sqlite-migration-not-successful": set(),
    "sqlite-foreign-key-shape-unsupported": {"table", "id"},
}
_FAMILY_BLOCKER_FIELDS = {
    "sqlite-registered-table-missing": {"table"},
    "sqlite-table-merge-class-not-classifiable": {"table", "merge_class"},
    "sqlite-identity-is-not-an-exact-unique-key": {"table"},
    "sqlite-identity-unique-semantics-unsupported": {"table"},
    "sqlite-secondary-unique-closure-not-implemented": {"table"},
    "sqlite-shared-row-divergence": {"table", "count"},
    "sqlite-exact-table-divergence": {"table"},
    "sqlite-family-unsupported": set(),
    "sqlite-trigger-semantics-not-classified": set(),
    "sqlite-view-semantics-not-classified": set(),
    "sqlite-adapter-not-registered": set(),
    "sqlite-adapter-exact-key-mismatch": set(),
    "sqlite-unregistered-collation": set(),
    "sqlite-edge-registry-mismatch": set(),
    "sqlite-table-set-unregistered": set(),
    "sqlite-schema-adapter-execution-not-implemented": set(),
    "sqlite-post-compose-edge-closure-required": set(),
}
_TERMINAL_PLAN_BLOCKERS = {
    "post-plan-private-close-required",
    "sqlite-composer-not-implemented",
    "session-union-execution-and-verification-not-implemented",
}


def _validate_private_sqlite_family_blocker(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict) or not isinstance(value.get("code"), str):
        raise BulkloadError("private SQLite family blocker is invalid")
    code = value["code"]
    if code in _ROLE_BLOCKER_FIELDS:
        expected = {"code", "role", *_ROLE_BLOCKER_FIELDS[code]}
        blocker = _require_exact_keys(
            value,
            expected,
            "private SQLite role blocker",
        )
        if blocker["role"] not in {"source", "destination"}:
            raise BulkloadError("private SQLite blocker role is invalid")
    elif code in _FAMILY_BLOCKER_FIELDS:
        expected = {"code", *_FAMILY_BLOCKER_FIELDS[code]}
        blocker = _require_exact_keys(
            value,
            expected,
            "private SQLite family blocker",
        )
    else:
        raise BulkloadError("private SQLite family blocker code is unsupported")
    for key in ("name", "table"):
        if key in blocker:
            _require_identifier(blocker[key], f"private SQLite blocker {key}")
    if "id" in blocker and (type(blocker["id"]) is not int or blocker["id"] < 0):
        raise BulkloadError("private SQLite blocker foreign-key ID is invalid")
    if "count" in blocker and (
        type(blocker["count"]) is not int or blocker["count"] < 1
    ):
        raise BulkloadError("private SQLite blocker count is invalid")
    if blocker.get("type") is not None and blocker["type"] not in {"virtual", "shadow"}:
        raise BulkloadError("private SQLite blocker table type is invalid")
    if blocker.get("merge_class") is not None and blocker["merge_class"] not in {
        "append-multiset",
        "unsupported",
    }:
        raise BulkloadError("private SQLite blocker merge class is invalid")
    return blocker


def _validate_private_sqlite_plan_blocker(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict) or not isinstance(value.get("code"), str):
        raise BulkloadError("private SQLite plan blocker is invalid")
    code = value["code"]
    if code in _TERMINAL_PLAN_BLOCKERS:
        return _require_exact_keys(
            value,
            {"code"},
            "private SQLite terminal blocker",
        )
    if code == "sqlite-family-set-mismatch":
        blocker = _require_exact_keys(
            value,
            {
                "code",
                "missing_from_source",
                "missing_from_destination",
            },
            "private SQLite family-set blocker",
        )
        if not all(
            isinstance(blocker[key], list)
            and all(isinstance(item, str) for item in blocker[key])
            for key in ("missing_from_source", "missing_from_destination")
        ):
            raise BulkloadError("private SQLite family-set blocker is invalid")
        values = [
            *blocker["missing_from_source"],
            *blocker["missing_from_destination"],
        ]
        if (
            not values
            or len(values) != len(set(values))
            or blocker["missing_from_source"] != sorted(blocker["missing_from_source"])
            or blocker["missing_from_destination"]
            != sorted(blocker["missing_from_destination"])
        ):
            raise BulkloadError("private SQLite family-set blocker is invalid")
        for basename in values:
            _family_identity(basename, allow_unknown=True)
        return blocker
    if code == "sqlite-registry-family-unobserved":
        blocker = _require_exact_keys(
            value,
            {"code", "basenames"},
            "private SQLite unobserved-registry blocker",
        )
        basenames = blocker["basenames"]
        if (
            not isinstance(basenames, list)
            or not basenames
            or not all(isinstance(item, str) for item in basenames)
            or basenames != sorted(set(basenames))
        ):
            raise BulkloadError("private SQLite unobserved-registry blocker is invalid")
        for basename in basenames:
            _family_identity(basename)
        return blocker
    if "basename" not in value:
        raise BulkloadError("private SQLite wrapped family blocker is invalid")
    blocker = value
    _family_identity(blocker["basename"], allow_unknown=True)
    _validate_private_sqlite_family_blocker(
        {key: item for key, item in blocker.items() if key != "basename"}
    )
    return blocker


def _validate_artifact_binding(value: Any, *, label: str) -> None:
    artifact = _require_exact_keys(
        value,
        {
            "capture_sha256",
            "snapshot_sha256",
            "snapshot_size",
            "source_sha256",
            "source_size",
            "schema_sha256",
            "migrations_sha256",
            "application_id",
            "user_version",
        },
        label,
    )
    for key in (
        "capture_sha256",
        "snapshot_sha256",
        "source_sha256",
        "schema_sha256",
        "migrations_sha256",
    ):
        _require_sha256(artifact[key], f"{label} {key}")
    for key in (
        "snapshot_size",
        "source_size",
        "application_id",
        "user_version",
    ):
        if type(artifact[key]) is not int or artifact[key] < 0:
            raise BulkloadError(f"{label} {key} is invalid")


def _raw_schema_records_from_contract(
    schema: dict[str, Any],
) -> list[dict[str, Any]]:
    records: list[dict[str, Any]] = []
    for table in schema["tables"]:
        records.append(
            {
                "type": "table",
                "name": table["name"],
                "table": table["name"],
                "sql_sha256": table["create_sql_sha256"],
            }
        )
        records.extend(
            {
                "type": "index",
                "name": index["name"],
                "table": table["name"],
                "sql_sha256": index["create_sql_sha256"],
            }
            for index in table["indexes"]
            if not index["name"].startswith("sqlite_")
        )
    records.extend(
        {
            "type": "trigger",
            "name": trigger["name"],
            "table": trigger["table"],
            "sql_sha256": trigger["sql_sha256"],
        }
        for trigger in schema["triggers"]
    )
    records.extend(
        {
            "type": "view",
            "name": view["name"],
            "table": view["name"],
            "sql_sha256": view["sql_sha256"],
        }
        for view in schema["views"]
    )
    return sorted(records, key=lambda record: (record["type"], record["name"]))


def _validate_schema_contract(value: Any, *, label: str) -> None:
    schema = _require_exact_keys(
        value,
        {
            "application_id",
            "user_version",
            "raw_schema_sha256",
            "schema_records",
            "omitted_table_objects",
            "classification_blockers",
            "tables",
            "triggers",
            "views",
            "collations",
            "schema_contract_sha256",
        },
        label,
    )
    for key in ("application_id", "user_version"):
        if type(schema[key]) is not int or schema[key] < 0:
            raise BulkloadError(f"{label} header is invalid")
    for key in ("raw_schema_sha256", "schema_contract_sha256"):
        _require_sha256(schema[key], f"{label} {key}")
    tables = schema["tables"]
    if not isinstance(tables, list):
        raise BulkloadError(f"{label} tables must be a list")
    table_names: list[str] = []
    for item in tables:
        table = _require_exact_keys(
            item,
            {
                "name",
                "type",
                "ncol",
                "without_rowid",
                "strict",
                "create_sql_sha256",
                "columns",
                "indexes",
                "foreign_keys",
            },
            f"{label} table",
        )
        _require_identifier(table["name"], f"{label} table name")
        if (
            table["type"] != "table"
            or type(table["ncol"]) is not int
            or table["ncol"] < 0
            or type(table["without_rowid"]) is not bool
            or type(table["strict"]) is not bool
        ):
            raise BulkloadError(f"{label} table facts are invalid")
        _require_sha256(
            table["create_sql_sha256"],
            f"{label} table SQL digest",
        )
        columns = table["columns"]
        if not isinstance(columns, list):
            raise BulkloadError(f"{label} columns must be a list")
        column_names: list[str] = []
        column_ids: list[int] = []
        primary_key_positions: list[int] = []
        for column_value in columns:
            column = _require_exact_keys(
                column_value,
                {
                    "cid",
                    "name",
                    "declared_type",
                    "affinity",
                    "not_null",
                    "default_sql_sha256",
                    "primary_key_position",
                    "hidden",
                    "generated_kind",
                },
                f"{label} column",
            )
            _require_identifier(column["name"], f"{label} column name")
            if (
                type(column["cid"]) is not int
                or type(column["primary_key_position"]) is not int
                or column["primary_key_position"] < 0
                or column["hidden"] not in {0, 1, 2, 3}
                or type(column["not_null"]) is not bool
                or column["affinity"]
                not in {"INTEGER", "TEXT", "BLOB", "REAL", "NUMERIC"}
                or column["generated_kind"]
                not in {"none", "hidden", "virtual", "stored"}
                or not isinstance(column["declared_type"], str)
            ):
                raise BulkloadError(f"{label} column facts are invalid")
            if column["default_sql_sha256"] is not None:
                _require_sha256(
                    column["default_sql_sha256"],
                    f"{label} default SQL digest",
                )
            column_names.append(column["name"])
            column_ids.append(column["cid"])
            if column["primary_key_position"] > 0:
                primary_key_positions.append(column["primary_key_position"])
        if (
            len(column_names) != len(set(column_names))
            or column_ids != sorted(set(column_ids))
            or sorted(primary_key_positions)
            != list(range(1, len(primary_key_positions) + 1))
        ):
            raise BulkloadError(f"{label} columns are not canonical")
        if table["ncol"] != len(columns):
            raise BulkloadError(f"{label} table column count differs")
        indexes = table["indexes"]
        if not isinstance(indexes, list):
            raise BulkloadError(f"{label} indexes must be a list")
        index_names: list[str] = []
        for index_value in indexes:
            index = _require_exact_keys(
                index_value,
                {
                    "name",
                    "unique",
                    "origin",
                    "partial",
                    "terms",
                    "create_sql_sha256",
                },
                f"{label} index",
            )
            _require_identifier(index["name"], f"{label} index name")
            if (
                type(index["unique"]) is not bool
                or type(index["partial"]) is not bool
                or not isinstance(index["origin"], str)
                or not isinstance(index["terms"], list)
            ):
                raise BulkloadError(f"{label} index facts are invalid")
            internal_index = index["name"].startswith("sqlite_")
            if internal_index != (index["create_sql_sha256"] is None):
                raise BulkloadError(f"{label} index SQL authority is invalid")
            if not internal_index:
                _require_sha256(
                    index["create_sql_sha256"],
                    f"{label} index SQL digest",
                )
            term_sequences: list[int] = []
            for term_value in index["terms"]:
                term = _require_exact_keys(
                    term_value,
                    {
                        "sequence",
                        "cid",
                        "name",
                        "descending",
                        "collation",
                        "key",
                    },
                    f"{label} index term",
                )
                if (
                    type(term["sequence"]) is not int
                    or type(term["cid"]) is not int
                    or type(term["descending"]) is not bool
                    or type(term["key"]) is not bool
                    or (
                        term["name"] is not None
                        and (
                            not isinstance(term["name"], str)
                            or _IDENTIFIER.fullmatch(term["name"]) is None
                        )
                    )
                    or (
                        term["collation"] is not None
                        and not isinstance(term["collation"], str)
                    )
                ):
                    raise BulkloadError(f"{label} index term is invalid")
                term_sequences.append(term["sequence"])
            if term_sequences != list(range(len(term_sequences))):
                raise BulkloadError(f"{label} index terms are not canonical")
            index_names.append(index["name"])
        if index_names != sorted(set(index_names)):
            raise BulkloadError(f"{label} indexes are not canonical")
        foreign_keys = table["foreign_keys"]
        if not isinstance(foreign_keys, list):
            raise BulkloadError(f"{label} foreign keys must be a list")
        foreign_key_order: list[tuple[int, int]] = []
        for foreign_key_value in foreign_keys:
            foreign_key = _require_exact_keys(
                foreign_key_value,
                {
                    "id",
                    "sequence",
                    "referenced_table",
                    "from_column",
                    "to_column",
                    "on_update",
                    "on_delete",
                    "match",
                    "supported",
                },
                f"{label} foreign key",
            )
            if (
                type(foreign_key["id"]) is not int
                or foreign_key["id"] < 0
                or type(foreign_key["sequence"]) is not int
                or foreign_key["sequence"] < 0
                or type(foreign_key["supported"]) is not bool
                or not all(
                    isinstance(foreign_key[key], str)
                    for key in (
                        "referenced_table",
                        "from_column",
                        "to_column",
                        "on_update",
                        "on_delete",
                        "match",
                    )
                )
            ):
                raise BulkloadError(f"{label} foreign key is invalid")
            if foreign_key["supported"]:
                for key in (
                    "referenced_table",
                    "from_column",
                    "to_column",
                ):
                    _require_identifier(
                        foreign_key[key],
                        f"{label} foreign key {key}",
                    )
                if not all(
                    foreign_key[key] for key in ("on_update", "on_delete", "match")
                ):
                    raise BulkloadError(f"{label} foreign key action is invalid")
            foreign_key_order.append((foreign_key["id"], foreign_key["sequence"]))
        if foreign_key_order != sorted(set(foreign_key_order)):
            raise BulkloadError(f"{label} foreign keys are not canonical")
        table_names.append(table["name"])
    if table_names != sorted(set(table_names)):
        raise BulkloadError(f"{label} tables are not canonical")
    triggers = schema["triggers"]
    if not isinstance(triggers, list):
        raise BulkloadError(f"{label} triggers must be a list")
    trigger_names: list[str] = []
    for trigger_value in triggers:
        trigger = _require_exact_keys(
            trigger_value,
            {"name", "table", "sql_sha256"},
            f"{label} trigger",
        )
        _require_identifier(trigger["name"], f"{label} trigger name")
        _require_identifier(trigger["table"], f"{label} trigger table")
        _require_sha256(trigger["sql_sha256"], f"{label} trigger SQL")
        trigger_names.append(trigger["name"])
    if trigger_names != sorted(set(trigger_names)):
        raise BulkloadError(f"{label} triggers are not canonical")
    views = schema["views"]
    if not isinstance(views, list):
        raise BulkloadError(f"{label} views must be a list")
    view_names: list[str] = []
    for view_value in views:
        view = _require_exact_keys(
            view_value,
            {"name", "sql_sha256"},
            f"{label} view",
        )
        _require_identifier(view["name"], f"{label} view name")
        _require_sha256(view["sql_sha256"], f"{label} view SQL")
        view_names.append(view["name"])
    if view_names != sorted(set(view_names)):
        raise BulkloadError(f"{label} views are not canonical")
    collations = schema["collations"]
    if (
        not isinstance(collations, list)
        or collations != sorted(set(collations))
        or not all(isinstance(collation, str) and collation for collation in collations)
    ):
        raise BulkloadError(f"{label} collations are invalid")
    schema_records = schema["schema_records"]
    if not isinstance(schema_records, list):
        raise BulkloadError(f"{label} schema records must be a list")
    schema_record_keys: list[tuple[str, str]] = []
    schema_records_by_key: dict[tuple[str, str], dict[str, Any]] = {}
    for record_value in schema_records:
        record = _require_exact_keys(
            record_value,
            {"type", "name", "table", "sql_sha256"},
            f"{label} schema record",
        )
        if not isinstance(record["type"], str) or record["type"] not in {
            "table",
            "index",
            "trigger",
            "view",
        }:
            raise BulkloadError(f"{label} schema record type is invalid")
        _require_identifier(record["name"], f"{label} schema record name")
        _require_identifier(record["table"], f"{label} schema record table")
        if record["sql_sha256"] is not None:
            _require_sha256(
                record["sql_sha256"],
                f"{label} schema record SQL",
            )
        record_key = (record["type"], record["name"])
        schema_record_keys.append(record_key)
        schema_records_by_key[record_key] = record
    if schema_record_keys != sorted(set(schema_record_keys)):
        raise BulkloadError(f"{label} schema records are not canonical")
    if sha256_bytes(canonical_bytes(schema_records)) != schema["raw_schema_sha256"]:
        raise BulkloadError(f"{label} raw-schema digest differs from schema records")
    for structured_record in _raw_schema_records_from_contract(schema):
        if (
            schema_records_by_key.get(
                (structured_record["type"], structured_record["name"])
            )
            != structured_record
        ):
            raise BulkloadError(
                f"{label} structured schema differs from raw-schema records"
            )
    omitted_objects = schema["omitted_table_objects"]
    if not isinstance(omitted_objects, list):
        raise BulkloadError(f"{label} omitted table objects must be a list")
    omitted_object_keys: list[tuple[str, str]] = []
    for omitted_value in omitted_objects:
        omitted = _require_exact_keys(
            omitted_value,
            {"name", "type"},
            f"{label} omitted table object",
        )
        _require_identifier(
            omitted["name"],
            f"{label} omitted table object name",
        )
        if omitted["type"] not in {"virtual", "shadow"}:
            raise BulkloadError(f"{label} omitted table object type is invalid")
        omitted_object_keys.append((omitted["name"], omitted["type"]))
    if omitted_object_keys != sorted(set(omitted_object_keys)):
        raise BulkloadError(f"{label} omitted table objects are not canonical")
    classification_blockers = schema["classification_blockers"]
    if not isinstance(classification_blockers, list) or [
        canonical_bytes(blocker) for blocker in classification_blockers
    ] != sorted({canonical_bytes(blocker) for blocker in classification_blockers}):
        raise BulkloadError(f"{label} classification blockers are not canonical")
    for blocker in classification_blockers:
        if not isinstance(blocker, dict) or "role" in blocker:
            raise BulkloadError(f"{label} classification blocker has role-local fields")
        _validate_private_sqlite_family_blocker({"role": "source", **blocker})
    represented_keys = {
        (record["type"], record["name"])
        for record in _raw_schema_records_from_contract(schema)
    }
    raw_omission_names = sorted(
        record["name"]
        for record in schema_records
        if (record["type"], record["name"]) not in represented_keys
        and record["type"] == "table"
    )
    if any(
        record["type"] != "table"
        for record in schema_records
        if (record["type"], record["name"]) not in represented_keys
    ) or raw_omission_names != sorted(omitted["name"] for omitted in omitted_objects):
        raise BulkloadError(f"{label} raw-schema omissions differ")
    body = {
        key: item for key, item in schema.items() if key != "schema_contract_sha256"
    }
    if sha256_bytes(canonical_bytes(body)) != schema["schema_contract_sha256"]:
        raise BulkloadError(f"{label} digest mismatch")


def _validate_table_relation(value: Any, *, label: str) -> None:
    relation = _require_exact_keys(
        value,
        {
            "name",
            "merge_class",
            "identity_columns",
            "source",
            "destination",
            "shared_equal",
            "source_only",
            "destination_only",
            "conflicts",
            "semantic_classification_complete",
        },
        label,
    )
    _require_identifier(relation["name"], f"{label} name")
    if relation["merge_class"] not in _MERGE_CLASSES:
        raise BulkloadError(f"{label} merge class is invalid")
    identities = relation["identity_columns"]
    if (
        not isinstance(identities, list)
        or not identities
        or len(identities) != len(set(identities))
    ):
        raise BulkloadError(f"{label} identity is invalid")
    for identity in identities:
        _require_identifier(identity, f"{label} identity")
    for key in (
        "shared_equal",
        "source_only",
        "destination_only",
        "conflicts",
    ):
        if type(relation[key]) is not int or relation[key] < 0:
            raise BulkloadError(f"{label} count is invalid")
    states: list[dict[str, Any]] = []
    for role in ("source", "destination"):
        state = relation[role]
        if state is None:
            continue
        state = _require_exact_keys(
            state,
            {"row_count", "semantic_rows_sha256", "classified_bytes"},
            f"{label} {role}",
        )
        if (
            type(state["row_count"]) is not int
            or state["row_count"] < 0
            or type(state["classified_bytes"]) is not int
            or state["classified_bytes"] < 0
        ):
            raise BulkloadError(f"{label} {role} counts are invalid")
        _require_sha256(
            state["semantic_rows_sha256"],
            f"{label} {role} digest",
        )
        states.append(state)
    if len(states) not in {0, 2}:
        raise BulkloadError(f"{label} state pair is incomplete")
    if states and (
        relation["source"]["row_count"]
        != relation["shared_equal"] + relation["source_only"] + relation["conflicts"]
        or relation["destination"]["row_count"]
        != relation["shared_equal"]
        + relation["destination_only"]
        + relation["conflicts"]
    ):
        raise BulkloadError(f"{label} count arithmetic differs")
    if (
        type(relation["semantic_classification_complete"]) is not bool
        or relation["semantic_classification_complete"] is True
        and (
            not states
            or relation["conflicts"] != 0
            or relation["merge_class"] in {"append-multiset", "unsupported"}
            or relation["source_only"] == 0
            and relation["destination_only"] == 0
            and relation["source"]["semantic_rows_sha256"]
            != relation["destination"]["semantic_rows_sha256"]
            or relation["merge_class"] == "exact"
            and (relation["source_only"] != 0 or relation["destination_only"] != 0)
        )
    ):
        raise BulkloadError(f"{label} readiness is invalid")


def _validate_family_relation(value: Any) -> None:
    family = _require_exact_keys(
        value,
        {
            "basename",
            "family_role",
            "generation",
            "source_artifact",
            "destination_artifact",
            "source_schema",
            "destination_schema",
            "migration_relation",
            "path_authorities_sha256",
            "table_relations",
            "edge_closure",
            "blockers",
            "classification_complete",
            "composer_implemented",
        },
        "private SQLite family relation",
    )
    role, generation = _family_identity(
        family["basename"],
        allow_unknown=True,
    )
    if family["family_role"] != role or family["generation"] != generation:
        raise BulkloadError("private SQLite family identity differs")
    _validate_artifact_binding(
        family["source_artifact"],
        label="private SQLite source artifact",
    )
    _validate_artifact_binding(
        family["destination_artifact"],
        label="private SQLite destination artifact",
    )
    _validate_schema_contract(
        family["source_schema"],
        label="private SQLite source schema",
    )
    _validate_schema_contract(
        family["destination_schema"],
        label="private SQLite destination schema",
    )
    migration = _require_exact_keys(
        family["migration_relation"],
        {
            "relation",
            "source_count",
            "destination_count",
            "source_latest_migration",
            "destination_latest_migration",
            "common_prefix_count",
            "common_prefix_sha256",
            "source_tail_sha256",
            "source_migrations_sha256",
            "destination_migrations_sha256",
            "adapter",
        },
        "private SQLite migration relation",
    )
    if migration["relation"] not in {
        "exact",
        "registered-prefix-upgrade",
        "blocked",
    }:
        raise BulkloadError("private SQLite migration relation is invalid")
    for key in ("source_count", "destination_count", "common_prefix_count"):
        if type(migration[key]) is not int or migration[key] < 0:
            raise BulkloadError("private SQLite migration count is invalid")
    for role_name in ("source", "destination"):
        count = migration[f"{role_name}_count"]
        latest = migration[f"{role_name}_latest_migration"]
        if (
            latest is not None
            and type(latest) is not int
            or (count == 0) != (latest is None)
        ):
            raise BulkloadError("private SQLite latest migration claim differs")
    if migration["common_prefix_count"] > min(
        migration["source_count"],
        migration["destination_count"],
    ):
        raise BulkloadError("private SQLite migration prefix is invalid")
    for key in (
        "common_prefix_sha256",
        "source_tail_sha256",
        "source_migrations_sha256",
        "destination_migrations_sha256",
    ):
        _require_sha256(migration[key], f"private SQLite migration {key}")
    if migration["adapter"] is not None:
        adapter = _require_exact_keys(
            migration["adapter"],
            {"adapter_id", "adapter_source_sha256"},
            "private SQLite migration adapter",
        )
        if not isinstance(adapter["adapter_id"], str) or not adapter["adapter_id"]:
            raise BulkloadError("private SQLite migration adapter ID is invalid")
        _require_sha256(
            adapter["adapter_source_sha256"],
            "private SQLite migration adapter digest",
        )
    empty_migrations_sha256 = sha256_bytes(canonical_bytes([]))
    if (
        (migration["source_count"] == 0)
        != (migration["source_migrations_sha256"] == empty_migrations_sha256)
        or (migration["destination_count"] == 0)
        != (migration["destination_migrations_sha256"] == empty_migrations_sha256)
        or (migration["common_prefix_count"] == 0)
        != (migration["common_prefix_sha256"] == empty_migrations_sha256)
        or (migration["source_count"] - migration["common_prefix_count"] == 0)
        != (migration["source_tail_sha256"] == empty_migrations_sha256)
    ):
        raise BulkloadError("private SQLite migration empty-set digest differs")
    if migration["relation"] == "exact":
        if (
            migration["adapter"] is not None
            or migration["source_count"] != migration["destination_count"]
            or migration["source_count"] != migration["common_prefix_count"]
            or migration["source_migrations_sha256"]
            != migration["destination_migrations_sha256"]
            or migration["common_prefix_sha256"]
            != migration["source_migrations_sha256"]
            or migration["source_tail_sha256"] != empty_migrations_sha256
        ):
            raise BulkloadError("private SQLite exact migration claim differs")
    elif migration["relation"] == "registered-prefix-upgrade":
        if (
            migration["adapter"] is None
            or migration["source_count"] <= migration["destination_count"]
            or migration["common_prefix_count"] != migration["destination_count"]
            or migration["common_prefix_sha256"]
            != migration["destination_migrations_sha256"]
            or migration["source_migrations_sha256"]
            == migration["destination_migrations_sha256"]
            or migration["source_migrations_sha256"] == empty_migrations_sha256
            or migration["source_tail_sha256"] == empty_migrations_sha256
            or migration["destination_count"] == 0
            and migration["source_tail_sha256"] != migration["source_migrations_sha256"]
        ):
            raise BulkloadError("private SQLite prefix-upgrade migration claim differs")
    for artifact_role, schema_role, migration_digest_key in (
        ("source_artifact", "source_schema", "source_migrations_sha256"),
        (
            "destination_artifact",
            "destination_schema",
            "destination_migrations_sha256",
        ),
    ):
        artifact = family[artifact_role]
        schema = family[schema_role]
        if (
            artifact["application_id"] != schema["application_id"]
            or artifact["user_version"] != schema["user_version"]
            or artifact["migrations_sha256"] != migration[migration_digest_key]
        ):
            raise BulkloadError(
                "private SQLite artifact, schema, and migration bindings differ"
            )
    _require_sha256(
        family["path_authorities_sha256"],
        "private SQLite path-authority digest",
    )
    relations = family["table_relations"]
    if not isinstance(relations, list):
        raise BulkloadError("private SQLite table relations must be a list")
    for relation in relations:
        _validate_table_relation(relation, label="private SQLite table relation")
    relation_names = [relation["name"] for relation in relations]
    if relation_names != sorted(set(relation_names)):
        raise BulkloadError("private SQLite table relations are not canonical")
    edge = _require_exact_keys(
        family["edge_closure"],
        {
            "registry_sha256",
            "source_observed_sha256",
            "destination_observed_sha256",
            "registry_matches_observed",
            "post_compose_required",
            "proven",
        },
        "private SQLite edge closure",
    )
    for key in (
        "registry_sha256",
        "source_observed_sha256",
        "destination_observed_sha256",
    ):
        _require_sha256(edge[key], f"private SQLite edge {key}")
    if (
        type(edge["registry_matches_observed"]) is not bool
        or type(edge["post_compose_required"]) is not bool
        or edge["proven"] is not False
    ):
        raise BulkloadError("private SQLite edge readiness is invalid")
    blockers = family["blockers"]
    if not isinstance(blockers, list) or [
        canonical_bytes(blocker) for blocker in blockers
    ] != sorted({canonical_bytes(blocker) for blocker in blockers}):
        raise BulkloadError("private SQLite family blockers are invalid")
    for blocker in blockers:
        _validate_private_sqlite_family_blocker(blocker)
    blocker_codes = {blocker["code"] for blocker in blockers}
    if (role == "unsupported") != ("sqlite-family-unsupported" in blocker_codes):
        raise BulkloadError("private SQLite unsupported-family blocker differs")
    migration_looks_exact = bool(
        migration["source_count"] == migration["destination_count"]
        and migration["source_count"] == migration["common_prefix_count"]
        and migration["source_migrations_sha256"]
        == migration["destination_migrations_sha256"]
        and migration["common_prefix_sha256"] == migration["source_migrations_sha256"]
        and migration["source_tail_sha256"] == empty_migrations_sha256
    )
    migration_looks_prefix_upgrade = bool(
        migration["source_count"] > migration["destination_count"]
        and migration["common_prefix_count"] == migration["destination_count"]
        and migration["common_prefix_sha256"]
        == migration["destination_migrations_sha256"]
        and migration["source_migrations_sha256"]
        != migration["destination_migrations_sha256"]
        and migration["source_tail_sha256"] != empty_migrations_sha256
    )
    if migration["relation"] == "blocked" and (
        migration_looks_exact
        or migration_looks_prefix_upgrade
        or not {
            "sqlite-adapter-exact-key-mismatch",
            "sqlite-adapter-not-registered",
        }
        & blocker_codes
    ):
        raise BulkloadError("private SQLite blocked migration claim differs")
    schemas_differ = bool(
        family["source_schema"]["schema_contract_sha256"]
        != family["destination_schema"]["schema_contract_sha256"]
    )
    if (
        schemas_differ
        != ("sqlite-schema-adapter-execution-not-implemented" in blocker_codes)
        or schemas_differ
        and relations
    ):
        raise BulkloadError("private SQLite schema-adapter blocker differs from schema")
    for role_name, schema in (
        ("source", family["source_schema"]),
        ("destination", family["destination_schema"]),
    ):
        expected_omissions = [
            (omitted["name"], omitted["type"])
            for omitted in schema["omitted_table_objects"]
        ]
        actual_omissions = sorted(
            (blocker["name"], blocker["type"])
            for blocker in blockers
            if blocker["code"] == "sqlite-virtual-or-shadow-table-unsupported"
            and blocker.get("role") == role_name
        )
        if expected_omissions != actual_omissions:
            raise BulkloadError(
                "private SQLite raw-schema omissions differ from blockers"
            )
    source_tables = {
        table["name"]: table for table in family["source_schema"]["tables"]
    }
    destination_tables = {
        table["name"]: table for table in family["destination_schema"]["tables"]
    }
    if not {
        "sqlite-adapter-not-registered",
        "sqlite-table-set-unregistered",
        "sqlite-schema-adapter-execution-not-implemented",
    } & blocker_codes and (
        relation_names != sorted(source_tables)
        or relation_names != sorted(destination_tables)
    ):
        raise BulkloadError(
            "private SQLite table relations do not cover the visible schema"
        )
    identity_blocker_codes = {
        "sqlite-identity-is-not-an-exact-unique-key",
        "sqlite-identity-unique-semantics-unsupported",
        "sqlite-secondary-unique-closure-not-implemented",
    }
    table_guard_codes = {
        "sqlite-registered-table-missing",
        "sqlite-table-merge-class-not-classifiable",
        *identity_blocker_codes,
    }
    for relation in relations:
        source_table = source_tables.get(relation["name"])
        destination_table = destination_tables.get(relation["name"])
        expected_identity_blockers: set[str] = set()
        expected_table_guard_blockers: list[dict[str, Any]] = []
        expected_states = True
        if source_table is None or destination_table is None:
            expected_states = False
            expected_table_guard_blockers.append(
                {
                    "code": "sqlite-registered-table-missing",
                    "table": relation["name"],
                }
            )
        elif relation["merge_class"] in {"append-multiset", "unsupported"}:
            expected_states = False
            expected_table_guard_blockers.append(
                {
                    "code": "sqlite-table-merge-class-not-classifiable",
                    "table": relation["name"],
                    "merge_class": relation["merge_class"],
                }
            )
        else:
            source_columns = {column["name"] for column in source_table["columns"]}
            destination_columns = {
                column["name"] for column in destination_table["columns"]
            }
            if not set(relation["identity_columns"]) <= (
                source_columns & destination_columns
            ):
                source_unique = destination_unique = False
                source_safe = destination_safe = False
                source_secondary = destination_secondary = False
            else:
                source_unique, source_safe, source_secondary = _table_unique_contract(
                    source_table,
                    relation["identity_columns"],
                )
                (
                    destination_unique,
                    destination_safe,
                    destination_secondary,
                ) = _table_unique_contract(
                    destination_table,
                    relation["identity_columns"],
                )
            if not source_unique or not destination_unique:
                expected_identity_blockers.add(
                    "sqlite-identity-is-not-an-exact-unique-key"
                )
                expected_states = False
            elif not source_safe or not destination_safe:
                expected_identity_blockers.add(
                    "sqlite-identity-unique-semantics-unsupported"
                )
                expected_states = False
            elif source_secondary or destination_secondary:
                expected_identity_blockers.add(
                    "sqlite-secondary-unique-closure-not-implemented"
                )
        expected_table_guard_blockers.extend(
            {"code": code, "table": relation["name"]}
            for code in sorted(expected_identity_blockers)
        )
        actual_table_guard_blockers = [
            blocker
            for blocker in blockers
            if blocker.get("table") == relation["name"]
            and blocker["code"] in table_guard_codes
        ]
        if [
            canonical_bytes(blocker) for blocker in actual_table_guard_blockers
        ] != sorted(
            canonical_bytes(blocker) for blocker in expected_table_guard_blockers
        ):
            raise BulkloadError("private SQLite table classification blockers differ")
        if expected_states != bool(
            relation["source"] is not None and relation["destination"] is not None
        ):
            raise BulkloadError("private SQLite table semantic-state coverage differs")
        if expected_states:
            for role_name, table_contract in (
                ("source", source_table),
                ("destination", destination_table),
            ):
                columns = [column["name"] for column in table_contract["columns"]]
                state = relation[role_name]
                empty_stream_sha256 = sha256_bytes(
                    canonical_bytes(
                        {
                            "table": relation["name"],
                            "columns": columns,
                        }
                    )
                )
                minimum_row_bytes = len(
                    canonical_bytes(
                        {
                            "basename": family["basename"],
                            "table": relation["name"],
                            "columns": columns,
                        }
                    )
                ) + 9 * (len(columns) + len(relation["identity_columns"]))
                if state["row_count"] == 0:
                    if (
                        state["classified_bytes"] != 0
                        or state["semantic_rows_sha256"] != empty_stream_sha256
                    ):
                        raise BulkloadError(
                            "private SQLite empty table semantic state differs"
                        )
                elif state["classified_bytes"] < state["row_count"] * minimum_row_bytes:
                    raise BulkloadError(
                        "private SQLite classified-byte lower bound differs"
                    )
            rowsets_equal = (
                relation["source_only"] == 0
                and relation["destination_only"] == 0
                and relation["conflicts"] == 0
            )
            if rowsets_equal:
                if relation["source"] != relation["destination"]:
                    raise BulkloadError(
                        "private SQLite equal-rowset semantic states differ"
                    )
            elif (
                relation["source"]["semantic_rows_sha256"]
                == relation["destination"]["semantic_rows_sha256"]
            ):
                raise BulkloadError("private SQLite divergent-rowset digests are equal")
        expected_divergence_blockers: list[dict[str, Any]] = []
        if relation["conflicts"]:
            expected_divergence_blockers.append(
                {
                    "code": "sqlite-shared-row-divergence",
                    "table": relation["name"],
                    "count": relation["conflicts"],
                }
            )
        if relation["merge_class"] == "exact" and (
            relation["source_only"]
            or relation["destination_only"]
            or relation["conflicts"]
        ):
            expected_divergence_blockers.append(
                {
                    "code": "sqlite-exact-table-divergence",
                    "table": relation["name"],
                }
            )
        actual_divergence_blockers = [
            blocker
            for blocker in blockers
            if blocker.get("table") == relation["name"]
            and blocker["code"]
            in {
                "sqlite-shared-row-divergence",
                "sqlite-exact-table-divergence",
            }
        ]
        if [
            canonical_bytes(blocker) for blocker in actual_divergence_blockers
        ] != sorted(
            canonical_bytes(blocker) for blocker in expected_divergence_blockers
        ):
            raise BulkloadError("private SQLite table divergence blockers differ")
        relation_blocked = any(
            blocker.get("table") == relation["name"] for blocker in blockers
        )
        expected_table_complete = bool(
            relation["source"] is not None
            and relation["destination"] is not None
            and relation["conflicts"] == 0
            and relation["merge_class"] not in {"append-multiset", "unsupported"}
            and (
                relation["merge_class"] != "exact"
                or (relation["source_only"] == 0 and relation["destination_only"] == 0)
            )
            and not relation_blocked
        )
        if relation["semantic_classification_complete"] != expected_table_complete:
            raise BulkloadError("private SQLite table classification readiness differs")
    migration_table_relation = next(
        (relation for relation in relations if relation["name"] == "_sqlx_migrations"),
        None,
    )
    if migration_table_relation is not None:
        for role_name in ("source", "destination"):
            state = migration_table_relation[role_name]
            invalid_migration_rows = any(
                blocker["code"] == "sqlite-migration-record-type-invalid"
                and blocker.get("role") == role_name
                for blocker in blockers
            )
            if (
                state is not None
                and not invalid_migration_rows
                and state["row_count"] != migration[f"{role_name}_count"]
            ):
                raise BulkloadError("private SQLite migration table count differs")
    source_observed_edges, source_edge_blockers = _observed_edge_rules(
        family["source_schema"]
    )
    destination_observed_edges, destination_edge_blockers = _observed_edge_rules(
        family["destination_schema"]
    )
    if edge["source_observed_sha256"] != sha256_bytes(
        canonical_bytes(source_observed_edges)
    ) or edge["destination_observed_sha256"] != sha256_bytes(
        canonical_bytes(destination_observed_edges)
    ):
        raise BulkloadError("private SQLite observed-edge digest differs from schema")
    for role_name, observed_blockers in (
        ("source", source_edge_blockers),
        ("destination", destination_edge_blockers),
    ):
        schema = family[f"{role_name}_schema"]
        expected_role_blockers = sorted(
            canonical_bytes({"role": role_name, **blocker})
            for blocker in [
                *schema["classification_blockers"],
                *observed_blockers,
            ]
        )
        actual_role_blockers = sorted(
            canonical_bytes(blocker)
            for blocker in blockers
            if blocker.get("role") == role_name
        )
        if actual_role_blockers != expected_role_blockers:
            raise BulkloadError("private SQLite role classification blockers differ")
    empty_edges_sha256 = sha256_bytes(canonical_bytes([]))
    if edge["registry_matches_observed"]:
        if not (
            edge["registry_sha256"]
            == edge["source_observed_sha256"]
            == edge["destination_observed_sha256"]
        ):
            raise BulkloadError("private SQLite edge equality differs")
    elif (
        not {
            "sqlite-edge-registry-mismatch",
            "sqlite-adapter-not-registered",
        }
        & blocker_codes
    ):
        raise BulkloadError("private SQLite edge mismatch blocker is absent")
    expected_edge_close = any(
        edge[key] != empty_edges_sha256
        for key in (
            "registry_sha256",
            "source_observed_sha256",
            "destination_observed_sha256",
        )
    )
    if edge["post_compose_required"] != expected_edge_close or expected_edge_close != (
        "sqlite-post-compose-edge-closure-required" in blocker_codes
    ):
        raise BulkloadError("private SQLite edge close claim differs")
    if bool(
        family["source_schema"]["triggers"] or family["destination_schema"]["triggers"]
    ) != ("sqlite-trigger-semantics-not-classified" in blocker_codes):
        raise BulkloadError("private SQLite trigger blocker is absent")
    if bool(
        family["source_schema"]["views"] or family["destination_schema"]["views"]
    ) != ("sqlite-view-semantics-not-classified" in blocker_codes):
        raise BulkloadError("private SQLite view blocker is absent")
    if (
        type(family["classification_complete"]) is not bool
        or family["classification_complete"]
        != (
            not blockers
            and all(
                relation["semantic_classification_complete"] for relation in relations
            )
        )
        or family["composer_implemented"] is not False
    ):
        raise BulkloadError("private SQLite family readiness is invalid")


def _validate_projection_identity(
    value: Any,
    *,
    label: str,
    directory: bool,
) -> None:
    expected = {"device", "inode", "uid", "mode"}
    if not directory:
        expected.add("links")
    identity = _require_exact_keys(value, expected, label)
    if not all(type(identity[key]) is int and identity[key] >= 0 for key in expected):
        raise BulkloadError(f"{label} values are invalid")


def _validate_stable_private_projection(
    value: Any,
    *,
    role: str,
) -> dict[str, dict[str, Any]]:
    projection = _require_exact_keys(
        value,
        {
            "role",
            "host",
            "host_authority_id",
            "codex_version",
            "codex_home",
            "sqlite_home",
            "selected_state_classes",
            "budgets",
            "auth",
            "sqlite_families",
            "sqlite_live_namespace_sha256",
            "copy_method",
            "complete",
            "ready_for_apply",
        },
        f"private SQLite {role} stable projection",
    )
    if (
        projection["role"] != role
        or not isinstance(projection["host"], str)
        or not projection["host"]
        or not isinstance(projection["codex_version"], str)
        or not projection["codex_version"]
    ):
        raise BulkloadError("private SQLite stable projection identity is invalid")
    _require_uuid(
        projection["host_authority_id"],
        "private SQLite stable projection host authority",
    )
    codex_home = _require_exact_keys(
        projection["codex_home"],
        {"resolved_path", "identity"},
        "private SQLite stable projection Codex home",
    )
    _require_absolute_normal_path(
        codex_home["resolved_path"],
        "private SQLite stable projection Codex home",
    )
    _validate_projection_identity(
        codex_home["identity"],
        label="private SQLite stable projection Codex-home identity",
        directory=True,
    )
    sqlite_home = _require_exact_keys(
        projection["sqlite_home"],
        {"resolved_path", "identity", "authority_source"},
        "private SQLite stable projection SQLite home",
    )
    _require_absolute_normal_path(
        sqlite_home["resolved_path"],
        "private SQLite stable projection SQLite home",
    )
    if sqlite_home["authority_source"] != "explicit":
        raise BulkloadError("private SQLite stable projection authority is invalid")
    _validate_projection_identity(
        sqlite_home["identity"],
        label="private SQLite stable projection SQLite-home identity",
        directory=True,
    )
    selected = projection["selected_state_classes"]
    if (
        not isinstance(selected, list)
        or not all(isinstance(item, str) for item in selected)
        or selected != sorted(set(selected))
        or not set(selected) <= {"auth", "sqlite"}
        or "sqlite" not in selected
    ):
        raise BulkloadError("private SQLite stable projection selection is invalid")
    budgets = _require_exact_keys(
        projection["budgets"],
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
        "private SQLite stable projection budgets",
    )
    if not all(type(item) is int and item > 0 for item in budgets.values()):
        raise BulkloadError("private SQLite stable projection budgets are invalid")
    if (
        budgets["max_auth_bytes"] != MAX_AUTH_BYTES
        or budgets["max_manifest_bytes"] != MAX_PRIVATE_MANIFEST_BYTES
    ):
        raise BulkloadError("private SQLite stable projection fixed budgets differ")
    auth = projection["auth"]
    if ("auth" in selected) != (auth is not None):
        raise BulkloadError("private SQLite stable projection auth selection differs")
    if auth is not None:
        auth = _require_exact_keys(
            auth,
            {
                "basename",
                "snapshot_path",
                "sha256",
                "size",
                "source_identity",
            },
            "private SQLite stable projection auth",
        )
        if (
            auth["basename"] != AUTH_BASENAME
            or auth["snapshot_path"] != AUTH_BASENAME
            or type(auth["size"]) is not int
            or auth["size"] <= 0
            or auth["size"] > budgets["max_auth_bytes"]
        ):
            raise BulkloadError("private SQLite stable projection auth is invalid")
        _require_sha256(auth["sha256"], "private SQLite stable projection auth")
        _validate_projection_identity(
            auth["source_identity"],
            label="private SQLite stable projection auth identity",
            directory=False,
        )
    families = projection["sqlite_families"]
    if not isinstance(families, list) or not families:
        raise BulkloadError("private SQLite stable projection families are invalid")
    family_keys = {
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
        "source_sha256",
        "source_identity",
        "source_size",
        "tables",
        "thread_count",
        "thread_index_bytes",
        "thread_ids_sha256",
        "thread_paths_sha256",
        "user_version",
    }
    projected_by_basename: dict[str, dict[str, Any]] = {}
    for family_value in families:
        family = _require_exact_keys(
            family_value,
            family_keys,
            "private SQLite stable projection family",
        )
        basename = family["basename"]
        if (
            not isinstance(basename, str)
            or PRIVATE_SQLITE_BASENAME.fullmatch(basename) is None
            or basename in projected_by_basename
            or family["snapshot_path"] != f"{SQLITE_DIRECTORY}/{basename}"
            or family["copy_method"] != "sqlite-immutable-backup-api"
            or family["quick_check"] != "ok"
            or family["snapshot_journal_mode"] != "delete"
        ):
            raise BulkloadError("private SQLite stable projection family is invalid")
        for key in (
            "sha256",
            "source_sha256",
            "schema_sha256",
            "migrations_sha256",
            "thread_ids_sha256",
            "thread_paths_sha256",
        ):
            _require_sha256(
                family[key],
                f"private SQLite stable projection {key}",
            )
        for key in (
            "snapshot_size",
            "source_size",
            "logical_source_bytes",
            "migration_count",
            "metadata_entries",
            "metadata_bytes",
            "thread_count",
            "thread_index_bytes",
            "application_id",
            "user_version",
        ):
            if type(family[key]) is not int or family[key] < 0:
                raise BulkloadError(
                    f"private SQLite stable projection {key} is invalid"
                )
        if (
            family["thread_count"] > budgets["max_thread_entries"]
            or family["thread_index_bytes"] > budgets["max_thread_index_bytes"]
            or family["metadata_entries"] > budgets["max_metadata_entries"]
            or family["metadata_bytes"] > budgets["max_metadata_bytes"]
        ):
            raise BulkloadError(
                "private SQLite stable projection exceeds its capture budget"
            )
        if (
            family["latest_migration"] is not None
            and type(family["latest_migration"]) is not int
            or (family["migration_count"] == 0) != (family["latest_migration"] is None)
        ):
            raise BulkloadError(
                "private SQLite stable projection latest migration is invalid"
            )
        _validate_projection_identity(
            family["source_identity"],
            label="private SQLite stable projection source identity",
            directory=False,
        )
        tables = family["tables"]
        if not isinstance(tables, list):
            raise BulkloadError("private SQLite stable projection tables are invalid")
        projected_tables: dict[str, dict[str, Any]] = {}
        for table_value in tables:
            if not isinstance(table_value, dict) or not isinstance(
                table_value.get("name"), str
            ):
                raise BulkloadError("private SQLite stable projection table is invalid")
            table_name = table_value["name"]
            expected_table_keys = {"name", "columns_sha256"}
            if table_name in COUNTED_TABLES:
                expected_table_keys.add("row_count")
            table = _require_exact_keys(
                table_value,
                expected_table_keys,
                "private SQLite stable projection table",
            )
            if not table_name or table_name in projected_tables:
                raise BulkloadError("private SQLite stable projection table is invalid")
            _require_sha256(
                table["columns_sha256"],
                "private SQLite stable projection table columns",
            )
            if "row_count" in table and (
                type(table["row_count"]) is not int or table["row_count"] < 0
            ):
                raise BulkloadError(
                    "private SQLite stable projection table count is invalid"
                )
            projected_tables[table_name] = table
        if list(projected_tables) != sorted(projected_tables):
            raise BulkloadError(
                "private SQLite stable projection tables are not canonical"
            )
        projected_thread_count = projected_tables.get("threads", {}).get("row_count", 0)
        if family["thread_count"] != projected_thread_count:
            raise BulkloadError("private SQLite stable projection thread count differs")
        projected_by_basename[basename] = family
    if list(projected_by_basename) != sorted(projected_by_basename):
        raise BulkloadError(
            "private SQLite stable projection families are not canonical"
        )
    if (
        len(families) > budgets["max_sqlite_families"]
        or sum(family["source_size"] for family in families)
        > budgets["max_total_sqlite_bytes"]
        or sum(family["snapshot_size"] for family in families)
        > budgets["max_total_sqlite_bytes"]
    ):
        raise BulkloadError(
            "private SQLite stable projection exceeds its SQLite budget"
        )
    _require_sha256(
        projection["sqlite_live_namespace_sha256"],
        "private SQLite stable projection live namespace",
    )
    copy_method = _require_exact_keys(
        projection["copy_method"],
        {"auth", "sqlite", "raw_wal_shm_copy"},
        "private SQLite stable projection copy method",
    )
    if (
        copy_method["auth"] != ("pinned-private-file" if "auth" in selected else None)
        or copy_method["sqlite"] != "sqlite-immutable-backup-api"
        or copy_method["raw_wal_shm_copy"] is not False
        or projection["complete"] is not True
        or projection["ready_for_apply"] is not False
    ):
        raise BulkloadError("private SQLite stable projection readiness is invalid")
    return projected_by_basename


def validate_codex_private_sqlite_compose_plan(value: dict[str, Any]) -> None:
    plan = _require_exact_keys(
        value,
        {
            "schema",
            "created_at",
            "runtime_authority",
            "accepted_inputs",
            "private_opening",
            "session_union",
            "adapter_registry",
            "path_map",
            "sqlite_families",
            "blockers",
            "opening_stable",
            "post_plan_close_required",
            "post_plan_close_proven",
            "readiness",
            "implementation",
            "plan_sha256",
        },
        "private SQLite compose plan",
    )
    if plan["schema"] != PRIVATE_SQLITE_PLAN_SCHEMA:
        raise BulkloadError("unsupported private SQLite compose-plan schema")
    if not isinstance(plan["created_at"], str) or not plan["created_at"]:
        raise BulkloadError("private SQLite compose-plan timestamp is invalid")
    private_runtime.validate_private_runtime_authority(plan["runtime_authority"])
    accepted = _require_exact_keys(
        plan["accepted_inputs"],
        {
            "compatibility_plan_sha256",
            "adapter_registry_sha256",
            "path_map_sha256",
            "session_union_plan_sha256",
        },
        "private SQLite accepted inputs",
    )
    for key, digest in accepted.items():
        _require_sha256(digest, f"private SQLite {key}")
    opening = _require_exact_keys(
        plan["private_opening"],
        {"source", "destination"},
        "private SQLite opening",
    )
    all_private_ids: list[str] = []
    all_private_digests: list[str] = []
    opening_families: dict[str, dict[str, dict[str, Any]]] = {}
    for role in ("source", "destination"):
        binding = _require_exact_keys(
            opening[role],
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
        for key in ("capture_ids", "quiescence_attestation_ids"):
            values = binding[key]
            if not isinstance(values, list) or len(values) != 2:
                raise BulkloadError(f"private SQLite {role} {key} are invalid")
            for item in values:
                _require_uuid(item, f"private SQLite {role} {key}")
            if len(set(values)) != 2:
                raise BulkloadError(f"private SQLite {role} {key} are reused")
        all_private_ids.extend(binding["capture_ids"])
        all_private_ids.extend(binding["quiescence_attestation_ids"])
        digests = binding["capture_sha256s"]
        if not isinstance(digests, list) or len(digests) != 2:
            raise BulkloadError("private SQLite capture digests are invalid")
        for digest in digests:
            _require_sha256(digest, "private SQLite capture digest")
        if len(set(digests)) != 2:
            raise BulkloadError("private SQLite opening capture digests are reused")
        all_private_digests.extend(digests)
        _require_sha256(
            binding["stable_projection_sha256"],
            "private SQLite stable projection digest",
        )
        if (
            sha256_bytes(canonical_bytes(binding["stable_projection"]))
            != binding["stable_projection_sha256"]
        ):
            raise BulkloadError("private SQLite stable projection digest mismatch")
        projection = binding["stable_projection"]
        projected_by_basename = _validate_stable_private_projection(
            projection,
            role=role,
        )
        if (
            projection["host"] != binding["host"]
            or projection["host_authority_id"] != binding["host_authority_id"]
            or projection["codex_version"] != binding["codex_version"]
        ):
            raise BulkloadError(
                "private SQLite opening differs from its stable projection"
            )
        opening_families[role] = projected_by_basename
    if (
        opening["source"]["host_authority_id"]
        == opening["destination"]["host_authority_id"]
    ):
        raise BulkloadError("private SQLite host authorities must differ")
    normalized_private_ids = {_normalize_capture_id(item) for item in all_private_ids}
    if len(normalized_private_ids) != 8:
        raise BulkloadError(
            "private SQLite opening evidence IDs are not globally distinct"
        )
    if len(set(all_private_digests)) != 4:
        raise BulkloadError(
            "private SQLite opening capture digests are not globally distinct"
        )
    session = _require_exact_keys(
        plan["session_union"],
        {
            "plan_sha256",
            "source",
            "destination",
            "prefix_evidence",
            "ready_for_attended_copy",
            "executed",
            "verified",
        },
        "private SQLite session union",
    )
    _require_sha256(session["plan_sha256"], "private SQLite session plan digest")
    if (
        session["plan_sha256"] != accepted["session_union_plan_sha256"]
        or session["ready_for_attended_copy"] is not True
        or session["executed"] is not False
        or session["verified"] is not False
    ):
        raise BulkloadError("private SQLite session claim is invalid")
    session_capture_ids = validate_codex_session_union_evidence_binding(
        session["source"],
        session["destination"],
        session["prefix_evidence"],
    )
    if normalized_private_ids & session_capture_ids:
        raise BulkloadError("private and session evidence reuse an evidence ID")
    for role in ("source", "destination"):
        if session[role]["host_authority_id"] != opening[role]["host_authority_id"]:
            raise BulkloadError(
                "private SQLite session and opening authorities are cross-wired"
            )
    registry = plan["adapter_registry"]
    validate_sqlite_adapter_registry(registry)
    if registry["registry_sha256"] != accepted["adapter_registry_sha256"]:
        raise BulkloadError("private SQLite registry binding differs")
    registry_by_basename = {
        family["basename"]: family for family in registry["families"]
    }
    registry_basenames = list(registry_by_basename)
    path_map = plan["path_map"]
    validate_sqlite_path_map(path_map)
    if path_map["path_map_sha256"] != accepted["path_map_sha256"]:
        raise BulkloadError("private SQLite path-map binding differs")
    codex_versions = path_map["codex_version"]
    for role in ("source", "destination"):
        if (
            not isinstance(codex_versions[role], str)
            or not codex_versions[role]
            or codex_versions[role] != opening[role]["codex_version"]
            or path_map[f"{role}_host_authority_id"]
            != opening[role]["host_authority_id"]
            or path_map[f"{role}_session_catalog_sha256"]
            != session[role]["catalog_sha256"]
            or path_map[f"{role}_session_root"] != session[role]["resolved_root"]
            or not _path_is_within(
                path_map[f"{role}_session_root"],
                opening[role]["stable_projection"]["codex_home"]["resolved_path"],
            )
        ):
            raise BulkloadError("private SQLite path-map cross-plane binding differs")
    if path_map["session_union_plan_sha256"] != session["plan_sha256"]:
        raise BulkloadError("private SQLite path-map session binding differs")
    families = plan["sqlite_families"]
    if not isinstance(families, list):
        raise BulkloadError("private SQLite family relations must be a list")
    basenames = [item.get("basename") for item in families if isinstance(item, dict)]
    if len(basenames) != len(families) or basenames != sorted(set(basenames)):
        raise BulkloadError("private SQLite family relations are not canonical")
    source_opening_basenames = list(opening_families["source"])
    destination_opening_basenames = list(opening_families["destination"])
    if source_opening_basenames == destination_opening_basenames:
        if basenames != source_opening_basenames:
            raise BulkloadError(
                "private SQLite family relations differ from the opening"
            )
    elif basenames:
        raise BulkloadError("private SQLite mismatched family opening has relations")
    for family in families:
        _validate_family_relation(family)
    classified_rows = 0
    classified_bytes = 0
    for family in families:
        for relation in family["table_relations"]:
            for role in ("source", "destination"):
                state = relation[role]
                if state is not None:
                    classified_rows += state["row_count"]
                    classified_bytes += state["classified_bytes"]
    if (
        classified_rows > MAX_SQLITE_PLAN_ROWS
        or classified_bytes > MAX_SQLITE_PLAN_ROW_BYTES
    ):
        raise BulkloadError(
            "private SQLite persisted semantic classification budget exceeded"
        )
    for family in families:
        registry_family = registry_by_basename.get(family["basename"])
        family_blocker_codes = {blocker["code"] for blocker in family["blockers"]}
        if registry_family is None:
            if "sqlite-adapter-not-registered" not in family_blocker_codes:
                raise BulkloadError("private SQLite missing-adapter blocker differs")
        else:
            if "sqlite-adapter-not-registered" in family_blocker_codes:
                raise BulkloadError(
                    "private SQLite registered adapter is marked absent"
                )
            expected_registry_key = {
                "source_codex_version": opening["source"]["codex_version"],
                "destination_codex_version": opening["destination"]["codex_version"],
                "source_schema_contract_sha256": family["source_schema"][
                    "schema_contract_sha256"
                ],
                "destination_schema_contract_sha256": family["destination_schema"][
                    "schema_contract_sha256"
                ],
                "source_raw_schema_sha256": family["source_schema"][
                    "raw_schema_sha256"
                ],
                "destination_raw_schema_sha256": family["destination_schema"][
                    "raw_schema_sha256"
                ],
                "source_migrations_sha256": family["migration_relation"][
                    "source_migrations_sha256"
                ],
                "destination_migrations_sha256": family["migration_relation"][
                    "destination_migrations_sha256"
                ],
                "source_application_id": family["source_schema"]["application_id"],
                "destination_application_id": family["destination_schema"][
                    "application_id"
                ],
                "source_user_version": family["source_schema"]["user_version"],
                "destination_user_version": family["destination_schema"][
                    "user_version"
                ],
                "migration_relation": family["migration_relation"]["relation"],
            }
            registry_key_mismatch = any(
                registry_family[key] != item
                for key, item in expected_registry_key.items()
            )
            if registry_key_mismatch != (
                "sqlite-adapter-exact-key-mismatch" in family_blocker_codes
            ):
                raise BulkloadError("private SQLite adapter exact-key blocker differs")
            if (
                family["migration_relation"]["adapter"] != registry_family["adapter"]
                or family["path_authorities_sha256"]
                != sha256_bytes(canonical_bytes(registry_family["path_authorities"]))
                or family["edge_closure"]["registry_sha256"]
                != sha256_bytes(canonical_bytes(registry_family["edges"]))
            ):
                raise BulkloadError(
                    "private SQLite family differs from its adapter registry"
                )
            mapped_path_rules = [
                {key: item for key, item in rule.items() if key != "family_basename"}
                for rule in path_map["rules"]
                if rule["family_basename"] == family["basename"]
            ]
            if mapped_path_rules != registry_family["path_authorities"]:
                raise BulkloadError(
                    "private SQLite registry and path-map bindings differ"
                )
            unknown_collations = (
                set(family["source_schema"]["collations"])
                | set(family["destination_schema"]["collations"])
            ) - set(registry_family["allowed_collations"])
            if bool(unknown_collations) != (
                "sqlite-unregistered-collation" in family_blocker_codes
            ):
                raise BulkloadError(
                    "private SQLite unregistered-collation blocker differs"
                )
            source_edges, _ = _observed_edge_rules(family["source_schema"])
            destination_edges, _ = _observed_edge_rules(family["destination_schema"])
            expected_edge_match = bool(
                source_edges == registry_family["edges"]
                and destination_edges == registry_family["edges"]
            )
            if family["edge_closure"][
                "registry_matches_observed"
            ] != expected_edge_match or (not expected_edge_match) != (
                "sqlite-edge-registry-mismatch" in family_blocker_codes
            ):
                raise BulkloadError(
                    "private SQLite edge-registry classification differs"
                )
            source_table_names = {
                table["name"] for table in family["source_schema"]["tables"]
            }
            destination_table_names = {
                table["name"] for table in family["destination_schema"]["tables"]
            }
            registry_table_names = {
                table["name"] for table in registry_family["tables"]
            }
            table_set_mismatch = bool(
                source_table_names != registry_table_names
                or destination_table_names != registry_table_names
            )
            if table_set_mismatch != (
                "sqlite-table-set-unregistered" in family_blocker_codes
            ):
                raise BulkloadError(
                    "private SQLite table-set blocker differs from registry"
                )
            schemas_differ = (
                family["source_schema"]["schema_contract_sha256"]
                != family["destination_schema"]["schema_contract_sha256"]
            )
            if not schemas_differ:
                relation_rules = [
                    {
                        "name": relation["name"],
                        "merge_class": relation["merge_class"],
                        "identity_columns": relation["identity_columns"],
                    }
                    for relation in family["table_relations"]
                ]
                if relation_rules != registry_family["tables"]:
                    raise BulkloadError(
                        "private SQLite table relations differ from registry"
                    )
        relation_by_name = {
            relation["name"]: relation for relation in family["table_relations"]
        }
        for role in ("source", "destination"):
            projected = opening_families[role][family["basename"]]
            expected_artifact = {
                "capture_sha256": opening[role]["capture_sha256s"][0],
                "snapshot_sha256": projected["sha256"],
                "snapshot_size": projected["snapshot_size"],
                "source_sha256": projected["source_sha256"],
                "source_size": projected["source_size"],
                "schema_sha256": projected["schema_sha256"],
                "migrations_sha256": projected["migrations_sha256"],
                "application_id": projected["application_id"],
                "user_version": projected["user_version"],
            }
            if family[f"{role}_artifact"] != expected_artifact:
                raise BulkloadError(
                    "private SQLite artifact differs from its opening capture"
                )
            projected_tables = {table["name"]: table for table in projected["tables"]}
            for table_name, projected_table in projected_tables.items():
                if "row_count" not in projected_table:
                    continue
                relation = relation_by_name.get(table_name)
                if (
                    relation is not None
                    and relation[role] is not None
                    and relation[role]["row_count"] != projected_table["row_count"]
                ):
                    raise BulkloadError("private SQLite projected table count differs")
            migration_relation = relation_by_name.get("_sqlx_migrations")
            if (
                migration_relation is not None
                and migration_relation[role] is not None
                and migration_relation[role]["row_count"]
                != projected["migration_count"]
            ):
                raise BulkloadError(
                    "private SQLite projected migration-table count differs"
                )
            invalid_migration_rows = any(
                blocker["code"] == "sqlite-migration-record-type-invalid"
                and blocker.get("role") == role
                for blocker in family["blockers"]
            )
            if not invalid_migration_rows and (
                projected["migration_count"]
                != family["migration_relation"][f"{role}_count"]
                or projected["latest_migration"]
                != family["migration_relation"][f"{role}_latest_migration"]
            ):
                raise BulkloadError(
                    "private SQLite projected migration metadata differs"
                )
    blockers = plan["blockers"]
    if not isinstance(blockers, list) or [
        canonical_bytes(blocker) for blocker in blockers
    ] != sorted({canonical_bytes(blocker) for blocker in blockers}):
        raise BulkloadError("private SQLite blockers are invalid")
    for blocker in blockers:
        _validate_private_sqlite_plan_blocker(blocker)
    expected_plan_blockers = [
        {"basename": family["basename"], **blocker}
        for family in families
        for blocker in family["blockers"]
    ]
    if source_opening_basenames != destination_opening_basenames:
        expected_plan_blockers.append(
            {
                "code": "sqlite-family-set-mismatch",
                "missing_from_source": sorted(
                    set(destination_opening_basenames) - set(source_opening_basenames)
                ),
                "missing_from_destination": sorted(
                    set(source_opening_basenames) - set(destination_opening_basenames)
                ),
            }
        )
    unobserved_registry = sorted(
        set(registry_basenames)
        - set(source_opening_basenames)
        - set(destination_opening_basenames)
    )
    if unobserved_registry:
        expected_plan_blockers.append(
            {
                "code": "sqlite-registry-family-unobserved",
                "basenames": unobserved_registry,
            }
        )
    expected_plan_blockers.extend({"code": code} for code in _TERMINAL_PLAN_BLOCKERS)
    if blockers != _dedupe_blockers(expected_plan_blockers):
        raise BulkloadError(
            "private SQLite plan blockers differ from family and opening evidence"
        )
    blocker_codes = {blocker["code"] for blocker in blockers}
    if (
        not {
            "post-plan-private-close-required",
            "sqlite-composer-not-implemented",
            "session-union-execution-and-verification-not-implemented",
        }
        <= blocker_codes
    ):
        raise BulkloadError("private SQLite terminal fail-holds are absent")
    readiness = _require_exact_keys(
        plan["readiness"],
        {
            "classification_complete",
            "composer_implemented",
            "sqlite_union_ready",
            "sqlite_compose",
            "sqlite_publish",
            "combined",
            "ready_for_apply",
        },
        "private SQLite readiness",
    )
    if (
        type(readiness["classification_complete"]) is not bool
        or any(
            readiness[key] is not False
            for key in (
                "composer_implemented",
                "sqlite_union_ready",
                "sqlite_compose",
                "sqlite_publish",
                "combined",
                "ready_for_apply",
            )
        )
        or plan["opening_stable"] is not True
        or plan["post_plan_close_required"] is not True
        or plan["post_plan_close_proven"] is not False
        or plan["implementation"] != "sqlite-compose-opening-plan-only-v4"
    ):
        raise BulkloadError("private SQLite plan readiness is invalid")
    nonterminal_blockers = [
        blocker
        for blocker in blockers
        if blocker["code"]
        not in {
            "post-plan-private-close-required",
            "sqlite-composer-not-implemented",
            "session-union-execution-and-verification-not-implemented",
        }
    ]
    if readiness["classification_complete"] != (
        not nonterminal_blockers
        and all(family["classification_complete"] for family in families)
    ):
        raise BulkloadError("private SQLite classification readiness differs")
    if len(canonical_bytes(plan)) > MAX_SQLITE_PLAN_BYTES:
        raise BulkloadError("private SQLite plan exceeds its byte budget")
    _require_sha256(plan["plan_sha256"], "private SQLite plan digest")
    if object_digest(plan, "plan_sha256") != plan["plan_sha256"]:
        raise BulkloadError("private SQLite plan digest mismatch")


def validate_codex_private_sqlite_compose_plan_against_inputs(
    plan: dict[str, Any],
    compatibility_plan: dict[str, Any],
    source_a_directory: Path,
    source_b_directory: Path,
    destination_a_directory: Path,
    destination_b_directory: Path,
    *,
    adapter_registry: dict[str, Any],
    path_map: dict[str, Any],
    session_union_plan: dict[str, Any],
    session_source_a: dict[str, Any],
    session_source_b: dict[str, Any],
    session_destination_a: dict[str, Any],
    session_destination_b: dict[str, Any],
    session_prefix_request: dict[str, Any] | None = None,
    session_source_prefix_a: dict[str, Any] | None = None,
    session_source_prefix_b: dict[str, Any] | None = None,
    session_destination_prefix_a: dict[str, Any] | None = None,
    session_destination_prefix_b: dict[str, Any] | None = None,
    session_close_request: dict[str, Any] | None = None,
    session_source_close_a: dict[str, Any] | None = None,
    session_source_close_b: dict[str, Any] | None = None,
    session_destination_close_a: dict[str, Any] | None = None,
    session_destination_close_b: dict[str, Any] | None = None,
) -> None:
    """Recompute a persisted v4 plan from every accepted opening input."""
    validate_codex_private_sqlite_compose_plan(plan)
    recomputed = compile_codex_private_sqlite_compose_plan(
        compatibility_plan,
        source_a_directory,
        source_b_directory,
        destination_a_directory,
        destination_b_directory,
        accept_compatibility_plan=plan["accepted_inputs"]["compatibility_plan_sha256"],
        adapter_registry=adapter_registry,
        accept_adapter_registry=plan["accepted_inputs"]["adapter_registry_sha256"],
        path_map=path_map,
        accept_path_map=plan["accepted_inputs"]["path_map_sha256"],
        session_union_plan=session_union_plan,
        accept_session_union_plan=plan["accepted_inputs"]["session_union_plan_sha256"],
        session_source_a=session_source_a,
        session_source_b=session_source_b,
        session_destination_a=session_destination_a,
        session_destination_b=session_destination_b,
        session_prefix_request=session_prefix_request,
        session_source_prefix_a=session_source_prefix_a,
        session_source_prefix_b=session_source_prefix_b,
        session_destination_prefix_a=session_destination_prefix_a,
        session_destination_prefix_b=session_destination_prefix_b,
        session_close_request=session_close_request,
        session_source_close_a=session_source_close_a,
        session_source_close_b=session_source_close_b,
        session_destination_close_a=session_destination_close_a,
        session_destination_close_b=session_destination_close_b,
        runtime_authority=plan["runtime_authority"],
    )
    ignored = {"created_at", "plan_sha256"}
    expected = {key: item for key, item in recomputed.items() if key not in ignored}
    observed = {key: item for key, item in plan.items() if key not in ignored}
    if observed != expected:
        raise BulkloadError(
            "private SQLite compose plan differs from its opening inputs"
        )
