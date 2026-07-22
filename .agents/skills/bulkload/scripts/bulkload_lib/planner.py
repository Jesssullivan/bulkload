"""Two-pass barrier and deterministic migration-plan compiler."""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any

from .model import (
    BulkloadError,
    PLAN_SCHEMA,
    SNAPSHOT_SCHEMA,
    canonical_bytes,
    object_digest,
    normalize_relative,
    portability_reason,
    require_digest,
    sanitize_remote_url,
    sensitive_reason,
    sha256_bytes,
    utc_now,
)
from .scanner import catalog_map, dirty_paths, find_file


def validate_snapshot(snapshot: dict[str, Any]) -> None:
    if not isinstance(snapshot, dict):
        raise BulkloadError("snapshot must be an object")
    _require_exact_keys(
        snapshot,
        {
            "captured_at",
            "capture_id",
            "catalog",
            "catalog_sha256",
            "complete",
            "errors",
            "host",
            "include_ignored",
            "mode",
            "root",
            "schema",
            "snapshot_sha256",
        },
        "snapshot",
    )
    if snapshot.get("schema") != SNAPSHOT_SCHEMA:
        raise BulkloadError("unsupported snapshot schema")
    if snapshot["mode"] not in {"repo", "fleet"}:
        raise BulkloadError("snapshot mode must be repo or fleet")
    for field in ("captured_at", "host", "root"):
        if not isinstance(snapshot[field], str) or not snapshot[field]:
            raise BulkloadError(f"snapshot {field} must be a non-empty string")
    if not Path(snapshot["root"]).is_absolute():
        raise BulkloadError("snapshot root must be absolute")
    if not isinstance(snapshot["include_ignored"], bool):
        raise BulkloadError("snapshot include_ignored must be Boolean")
    if not isinstance(snapshot["complete"], bool):
        raise BulkloadError("snapshot complete must be Boolean")
    if not isinstance(snapshot["errors"], list) or not all(
        isinstance(item, str) for item in snapshot["errors"]
    ):
        raise BulkloadError("snapshot errors must be a list of strings")
    if snapshot["complete"] and snapshot["errors"]:
        raise BulkloadError("complete snapshot cannot contain errors")
    if not isinstance(snapshot["capture_id"], str) or not re.fullmatch(
        r"[0-9a-f]{32}", snapshot["capture_id"]
    ):
        raise BulkloadError("snapshot capture_id must be 128-bit lowercase hex")
    if not isinstance(snapshot["catalog"], list) or not snapshot["catalog"]:
        raise BulkloadError("snapshot catalog must be a non-empty list")
    logical_paths: set[str] = set()
    for index, repository in enumerate(snapshot["catalog"]):
        label = f"snapshot.catalog[{index}]"
        if not isinstance(repository, dict):
            raise BulkloadError(f"{label} must be an object")
        required = {
            "branch",
            "complete",
            "errors",
            "files",
            "git_dir",
            "has_alternates",
            "has_content_filters",
            "has_grafts",
            "has_gitmodules",
            "has_lfs_attributes",
            "has_unportable_attributes",
            "head",
            "logical_path",
            "local_refs_sha256",
            "observed_bytes",
            "refs",
            "refs_sha256",
            "remotes",
            "root",
            "status",
            "status_sha256",
            "upstream",
            "worktree_common_dir",
            "worktrees",
        }
        _require_exact_keys(repository, required, label)
        for flag in (
            "complete",
            "has_alternates",
            "has_content_filters",
            "has_grafts",
            "has_gitmodules",
            "has_lfs_attributes",
            "has_unportable_attributes",
        ):
            if not isinstance(repository[flag], bool):
                raise BulkloadError(f"{label}.{flag} must be Boolean")
        logical = repository["logical_path"]
        if logical != ".":
            normalize_relative(logical)
        if logical in logical_paths:
            raise BulkloadError(f"duplicate snapshot repository: {logical}")
        logical_paths.add(logical)
        if not repository["complete"] or repository["errors"]:
            raise BulkloadError(f"{label} is incomplete")
        head = repository["head"]
        if not isinstance(head, str) or len(head) not in {40, 64}:
            raise BulkloadError(f"{label}.head must be a Git object ID")
        try:
            int(head, 16)
        except ValueError as error:
            raise BulkloadError(f"{label}.head must be a Git object ID") from error
        _require_sha256(repository["status_sha256"], f"{label}.status_sha256")
        _require_sha256(repository["local_refs_sha256"], f"{label}.local_refs_sha256")
        _require_sha256(repository["refs_sha256"], f"{label}.refs_sha256")
        if not isinstance(repository["files"], list) or not isinstance(
            repository["status"], list
        ):
            raise BulkloadError(f"{label} file and status catalogs must be lists")
        file_paths: set[str] = set()
        for file_index, item in enumerate(repository["files"]):
            if not isinstance(item, dict):
                raise BulkloadError(f"{label}.files[{file_index}] must be an object")
            if not {"eligible", "git_class", "kind", "path", "status"} <= set(item):
                raise BulkloadError(
                    f"{label}.files[{file_index}] lacks required fields"
                )
            path = _content_path(item["path"], allow_ineligible=True)
            git_class = item["git_class"]
            if git_class not in {"tracked", "untracked", "ignored"}:
                raise BulkloadError(
                    f"{label}.files[{file_index}] has invalid Git class"
                )
            if path in file_paths:
                raise BulkloadError(f"duplicate snapshot file record: {logical}/{path}")
            file_paths.add(path)
        status = _validate_status_catalog(repository["status"], f"{label}.status")
        status_by_path = {item["path"]: item for item in status}
        expected_status = sha256_bytes(canonical_bytes(status))
        if repository["status_sha256"] != expected_status:
            raise BulkloadError(f"{label}.status_sha256 mismatch")
        nonignored_paths: set[str] = set()
        for item in repository["files"]:
            expected_file_status = (
                None
                if item["git_class"] == "ignored"
                else status_by_path.get(item["path"])
            )
            if item["git_class"] != "ignored":
                nonignored_paths.add(item["path"])
            if item["status"] != expected_file_status:
                raise BulkloadError(
                    f"{label} file/status binding differs: {logical}/{item['path']}"
                )
        missing_status_files = sorted(status_by_path.keys() - nonignored_paths)
        if missing_status_files:
            raise BulkloadError(
                f"{label} status paths lack file records: {missing_status_files}"
            )
        for field in ("refs", "remotes", "worktrees"):
            if not isinstance(repository[field], list):
                raise BulkloadError(f"{label}.{field} must be a list")
        _validate_remote_catalog(repository["remotes"], f"{label}.remotes")
        refs = _validate_ref_catalog(
            repository["refs"], f"{label}.refs", len(head), allow_remote=True
        )
        expected_refs = sha256_bytes(canonical_bytes(refs))
        if repository["refs_sha256"] != expected_refs:
            raise BulkloadError(f"{label}.refs_sha256 mismatch")
        local_refs = [
            ref for ref in refs if not ref["name"].startswith("refs/remotes/")
        ]
        expected_local_refs = sha256_bytes(canonical_bytes(local_refs))
        if repository["local_refs_sha256"] != expected_local_refs:
            raise BulkloadError(f"{label}.local_refs_sha256 mismatch")
    require_digest(snapshot, "snapshot_sha256")
    expected_catalog = sha256_bytes(canonical_bytes(snapshot.get("catalog")))
    if snapshot.get("catalog_sha256") != expected_catalog:
        raise BulkloadError("snapshot catalog digest mismatch")
    if not snapshot.get("complete"):
        raise BulkloadError("incomplete snapshots cannot authorize planning")


def _require_exact_keys(value: dict[str, Any], expected: set[str], label: str) -> None:
    if set(value) != expected:
        raise BulkloadError(f"{label} keys differ: {sorted(set(value) ^ expected)}")


def _require_sha256(value: Any, label: str) -> None:
    if not isinstance(value, str) or len(value) != 64:
        raise BulkloadError(f"{label} must be a SHA-256 hex digest")
    try:
        int(value, 16)
    except ValueError as error:
        raise BulkloadError(f"{label} must be a SHA-256 hex digest") from error


def _validate_ref_catalog(
    values: Any,
    label: str,
    object_length: int,
    *,
    allow_remote: bool,
) -> list[dict[str, Any]]:
    if not isinstance(values, list):
        raise BulkloadError(f"{label} must be a list")
    previous_ref: str | None = None
    for index, ref in enumerate(values):
        ref_label = f"{label}[{index}]"
        if not isinstance(ref, dict):
            raise BulkloadError(f"{ref_label} must be an object")
        _require_exact_keys(ref, {"name", "object", "symref"}, ref_label)
        name = ref["name"]
        object_id = ref["object"]
        symref = ref["symref"]
        if (
            not isinstance(name, str)
            or not name.startswith("refs/")
            or any(
                ord(character) < 0x20 or ord(character) == 0x7F for character in name
            )
        ):
            raise BulkloadError(f"{ref_label}.name must be a canonical refs/ name")
        if not allow_remote and name.startswith("refs/remotes/"):
            raise BulkloadError(f"{ref_label}.name must be non-remote")
        if previous_ref is not None and name <= previous_ref:
            raise BulkloadError(f"{label} must be sorted and unique")
        previous_ref = name
        if (
            not isinstance(object_id, str)
            or len(object_id) != object_length
            or not re.fullmatch(r"[0-9a-f]+", object_id)
        ):
            raise BulkloadError(
                f"{ref_label}.object must match the repository object format"
            )
        if symref is not None and (
            not isinstance(symref, str)
            or not symref.startswith("refs/")
            or any(
                ord(character) < 0x20 or ord(character) == 0x7F for character in symref
            )
        ):
            raise BulkloadError(
                f"{ref_label}.symref must be null or a canonical refs/ name"
            )
    return values


def _validate_remote_catalog(values: Any, label: str) -> None:
    if not isinstance(values, list):
        raise BulkloadError(f"{label} must be a list")
    previous_name: str | None = None
    for index, remote in enumerate(values):
        remote_label = f"{label}[{index}]"
        if not isinstance(remote, dict):
            raise BulkloadError(f"{remote_label} must be an object")
        _require_exact_keys(remote, {"name", "urls"}, remote_label)
        name = remote["name"]
        if (
            not isinstance(name, str)
            or not name
            or any(
                ord(character) < 0x20 or ord(character) == 0x7F for character in name
            )
        ):
            raise BulkloadError(f"{remote_label}.name must be a printable string")
        if previous_name is not None and name <= previous_name:
            raise BulkloadError(f"{label} must be sorted and unique")
        previous_name = name
        urls = remote["urls"]
        if not isinstance(urls, list) or not urls:
            raise BulkloadError(f"{remote_label}.urls must be a non-empty list")
        if urls != sorted(set(urls)):
            raise BulkloadError(f"{remote_label}.urls must be sorted and unique")
        for url_index, url in enumerate(urls):
            url_label = f"{remote_label}.urls[{url_index}]"
            if not isinstance(url, str):
                raise BulkloadError(f"{url_label} must be a string")
            if url.startswith("local-path:sha256:"):
                _require_sha256(url.removeprefix("local-path:sha256:"), url_label)
            elif sanitize_remote_url(url) != url:
                raise BulkloadError(f"{url_label} is not credential-safe")


def _content_path(value: Any, *, allow_ineligible: bool = False) -> str:
    path = normalize_relative(value)
    if any(component.casefold() == ".git" for component in path.split("/")):
        raise BulkloadError(f"Git administration path is forbidden: {path}")
    if not allow_ineligible:
        reason = sensitive_reason(path) or portability_reason(path)
        if reason is not None:
            raise BulkloadError(
                f"ineligible content path is forbidden: {path}: {reason}"
            )
    return path


def _validate_status_catalog(values: Any, label: str) -> list[dict[str, Any]]:
    if not isinstance(values, list):
        raise BulkloadError(f"{label} must be a list")
    previous_path: str | None = None
    for index, item in enumerate(values):
        item_label = f"{label}[{index}]"
        if not isinstance(item, dict):
            raise BulkloadError(f"{item_label} must be an object")
        _require_exact_keys(item, {"index", "path", "worktree"}, item_label)
        path = _content_path(item["path"], allow_ineligible=True)
        if previous_path is not None and path <= previous_path:
            raise BulkloadError(f"{label} must be sorted and unique")
        previous_path = path
        if item["index"] not in {" ", "?", "A", "D", "M", "T", "U"} or item[
            "worktree"
        ] not in {" ", "?", "D", "M", "T", "U"}:
            raise BulkloadError(f"{item_label} has invalid codes")
    return values


def _validate_identity(value: Any, label: str) -> None:
    if not isinstance(value, dict):
        raise BulkloadError(f"{label} must be an identity object")
    kind = value.get("kind")
    if kind == "file":
        _require_exact_keys(value, {"kind", "mode", "sha256", "size"}, label)
        _require_sha256(value["sha256"], f"{label}.sha256")
    else:
        raise BulkloadError(
            f"{label}.kind must be file; symlink mutations are unsupported"
        )
    mode = value["mode"]
    if not isinstance(mode, str) or not re.fullmatch(r"[0-7]{4}", mode):
        raise BulkloadError(f"{label}.mode must be four octal digits")
    if int(mode, 8) & 0o7000:
        raise BulkloadError(f"{label}.mode contains privileged mode bits")
    if (
        isinstance(value["size"], bool)
        or not isinstance(value["size"], int)
        or value["size"] < 0
    ):
        raise BulkloadError(f"{label}.size must be a non-negative integer")


def _validate_repository_preconditions(
    values: Any, label: str
) -> dict[str, dict[str, Any]]:
    if not isinstance(values, list):
        raise BulkloadError(f"{label} must be a list")
    repositories: dict[str, dict[str, Any]] = {}
    for index, item in enumerate(values):
        item_label = f"{label}[{index}]"
        if not isinstance(item, dict):
            raise BulkloadError(f"{item_label} must be an object")
        _require_exact_keys(
            item,
            {
                "branch",
                "dirty_paths",
                "head",
                "logical_path",
                "local_refs",
                "local_refs_sha256",
                "status",
                "status_sha256",
            },
            item_label,
        )
        logical = item["logical_path"]
        if logical != ".":
            normalize_relative(logical)
        if logical in repositories:
            raise BulkloadError(f"duplicate repository precondition: {logical}")
        repositories[logical] = item
        if item["branch"] is not None and not isinstance(item["branch"], str):
            raise BulkloadError(f"{item_label}.branch must be a string or null")
        head = item["head"]
        if not isinstance(head, str) or len(head) not in {40, 64}:
            raise BulkloadError(f"{item_label}.head must be a Git object ID")
        try:
            int(head, 16)
        except ValueError as error:
            raise BulkloadError(f"{item_label}.head must be a Git object ID") from error
        _require_sha256(item["status_sha256"], f"{item_label}.status_sha256")
        _require_sha256(item["local_refs_sha256"], f"{item_label}.local_refs_sha256")
        local_refs = _validate_ref_catalog(
            item["local_refs"],
            f"{item_label}.local_refs",
            len(head),
            allow_remote=False,
        )
        expected_local_refs = sha256_bytes(canonical_bytes(local_refs))
        if item["local_refs_sha256"] != expected_local_refs:
            raise BulkloadError(f"{item_label}.local_refs_sha256 mismatch")
        status = _validate_status_catalog(item["status"], f"{item_label}.status")
        expected_status = sha256_bytes(canonical_bytes(status))
        if item["status_sha256"] != expected_status:
            raise BulkloadError(f"{item_label}.status_sha256 mismatch")
        if not isinstance(item["dirty_paths"], list):
            raise BulkloadError(f"{item_label}.dirty_paths must be a list")
        paths = [
            _content_path(path, allow_ineligible=True) for path in item["dirty_paths"]
        ]
        if paths != sorted(set(paths)):
            raise BulkloadError(f"{item_label}.dirty_paths must be sorted and unique")
        status_paths = [record["path"] for record in status]
        if paths != status_paths:
            raise BulkloadError(f"{item_label}.dirty_paths differ from status paths")
    return repositories


def validate_plan(plan: dict[str, Any]) -> None:
    if not isinstance(plan, dict):
        raise BulkloadError("plan must be an object")
    _require_exact_keys(
        plan,
        {"created_at", "envelope_sha256", "intent", "plan_sha256", "schema"},
        "plan",
    )
    if plan.get("schema") != PLAN_SCHEMA:
        raise BulkloadError("unsupported plan schema")
    intent = plan.get("intent")
    if not isinstance(intent, dict):
        raise BulkloadError("plan intent must be an object")
    _require_exact_keys(
        intent,
        {
            "blockers",
            "destination_catalog_sha256",
            "destination_repositories_before",
            "destination_snapshot_sha256",
            "destination_target",
            "expected_files",
            "expected_repositories",
            "findings",
            "mode",
            "operations",
            "ready",
            "source_catalog_sha256",
            "source_snapshot_passes",
        },
        "plan.intent",
    )
    if intent["mode"] not in {"repo", "fleet"}:
        raise BulkloadError("plan.intent.mode must be repo or fleet")
    for field in (
        "destination_catalog_sha256",
        "destination_snapshot_sha256",
        "source_catalog_sha256",
    ):
        _require_sha256(intent[field], f"plan.intent.{field}")
    destination_target = intent["destination_target"]
    if not isinstance(destination_target, dict):
        raise BulkloadError("plan.intent.destination_target must be an object")
    _require_exact_keys(
        destination_target, {"host", "root"}, "plan.intent.destination_target"
    )
    if not all(
        isinstance(destination_target[field], str) for field in ("host", "root")
    ):
        raise BulkloadError("plan destination host and root must be strings")
    if (
        not destination_target["host"]
        or not Path(destination_target["root"]).is_absolute()
    ):
        raise BulkloadError(
            "plan destination target must have a host and absolute root"
        )
    if (
        not isinstance(intent["source_snapshot_passes"], list)
        or len(intent["source_snapshot_passes"]) != 2
    ):
        raise BulkloadError(
            "plan.intent.source_snapshot_passes must contain two digests"
        )
    for index, digest in enumerate(intent["source_snapshot_passes"]):
        _require_sha256(digest, f"plan.intent.source_snapshot_passes[{index}]")
    if len(set(intent["source_snapshot_passes"])) != 2:
        raise BulkloadError("plan source snapshot passes must be distinct")
    if not isinstance(intent["blockers"], list) or not all(
        isinstance(item, dict) for item in intent["blockers"]
    ):
        raise BulkloadError("plan.intent.blockers must be a list of objects")
    if not isinstance(intent["findings"], list) or not all(
        isinstance(item, dict) for item in intent["findings"]
    ):
        raise BulkloadError("plan.intent.findings must be a list of objects")
    if not isinstance(intent["ready"], bool) or intent["ready"] != (
        not intent["blockers"]
    ):
        raise BulkloadError("plan.intent.ready disagrees with blockers")

    expected_repositories = _validate_repository_preconditions(
        intent["expected_repositories"], "plan.intent.expected_repositories"
    )
    destination_repositories = _validate_repository_preconditions(
        intent["destination_repositories_before"],
        "plan.intent.destination_repositories_before",
    )
    if not destination_repositories.keys() <= expected_repositories.keys():
        raise BulkloadError(
            "destination repository preconditions are not a source subset"
        )
    if intent["ready"]:
        if expected_repositories.keys() != destination_repositories.keys():
            raise BulkloadError("ready plan source and destination repositories differ")
        for logical, source in expected_repositories.items():
            destination = destination_repositories[logical]
            for field in ("branch", "head"):
                if source[field] != destination[field]:
                    raise BulkloadError(
                        f"ready plan source and destination {field} differ: {logical}"
                    )
            source_dirty = set(source["dirty_paths"])
            destination_dirty = set(destination["dirty_paths"])
            destination_only_dirt = sorted(destination_dirty - source_dirty)
            if destination_only_dirt:
                raise BulkloadError(
                    "ready plan destination has dirt absent from source: "
                    f"{logical}: {destination_only_dirt}"
                )
            source_refs = {item["name"]: item for item in source["local_refs"]}
            destination_refs = {
                item["name"]: item for item in destination["local_refs"]
            }
            for name, source_ref in source_refs.items():
                if destination_refs.get(name) != source_ref:
                    raise BulkloadError(
                        f"ready plan source ref differs at destination: {logical}: {name}"
                    )
            extra_replace_refs = sorted(
                name
                for name in destination_refs.keys() - source_refs.keys()
                if name.startswith("refs/replace/")
            )
            if extra_replace_refs:
                raise BulkloadError(
                    "ready plan destination has extra replacement refs: "
                    f"{logical}: {extra_replace_refs}"
                )

    if not isinstance(intent["expected_files"], list):
        raise BulkloadError("plan.intent.expected_files must be a list")
    expected_files: dict[tuple[str, str], dict[str, Any]] = {}
    for index, item in enumerate(intent["expected_files"]):
        label = f"plan.intent.expected_files[{index}]"
        if not isinstance(item, dict):
            raise BulkloadError(f"{label} must be an object")
        _require_exact_keys(item, {"git_class", "identity", "path", "repo"}, label)
        if item["repo"] not in expected_repositories:
            raise BulkloadError(f"{label}.repo is not declared")
        path = _content_path(item["path"])
        if item["git_class"] not in {"tracked", "untracked"}:
            raise BulkloadError(f"{label}.git_class is unsupported")
        _validate_identity(item["identity"], f"{label}.identity")
        key = (item["repo"], path)
        if key in expected_files:
            raise BulkloadError(f"duplicate expected file: {key}")
        expected_files[key] = item
    if not expected_repositories:
        raise BulkloadError("plan must bind at least one repository")
    if intent["ready"]:
        declared_dirty = {
            item["logical_path"]: set(item["dirty_paths"])
            for item in intent["expected_repositories"]
        }
        bound_files: dict[str, set[str]] = {
            logical: set() for logical in expected_repositories
        }
        for logical, path in expected_files:
            bound_files[logical].add(path)
        if bound_files != declared_dirty:
            raise BulkloadError(
                "ready plan expected files do not bind every dirty path"
            )

    if not isinstance(intent["operations"], list):
        raise BulkloadError("plan.intent.operations must be a list")
    operation_keys: set[tuple[str, str]] = set()
    for index, operation in enumerate(intent["operations"]):
        label = f"plan.intent.operations[{index}]"
        if not isinstance(operation, dict):
            raise BulkloadError(f"{label} must be an object")
        _require_exact_keys(
            operation, {"after", "before", "git_class", "op", "path", "repo"}, label
        )
        if operation["op"] != "copy":
            raise BulkloadError(f"{label}.op is unsupported")
        path = _content_path(operation["path"])
        key = (operation["repo"], path)
        if key in operation_keys:
            raise BulkloadError(f"duplicate operation: {key}")
        operation_keys.add(key)
        expected_file = expected_files.get(key)
        if expected_file is None:
            raise BulkloadError(f"operation has no expected-file binding: {key}")
        if operation["git_class"] != expected_file["git_class"]:
            raise BulkloadError(
                f"operation Git class differs from expected file: {key}"
            )
        _validate_identity(operation["after"], f"{label}.after")
        if operation["after"] != expected_file["identity"]:
            raise BulkloadError(
                f"operation after identity differs from expected file: {key}"
            )
        if operation["before"] is not None:
            _validate_identity(operation["before"], f"{label}.before")

    expected = sha256_bytes(canonical_bytes(intent))
    if plan.get("plan_sha256") != expected:
        raise BulkloadError("plan digest mismatch")
    require_digest(plan, "envelope_sha256")


def _file_identity(item: dict[str, Any] | None) -> dict[str, Any] | None:
    if item is None:
        return None
    keys = ("kind", "mode", "sha256", "size")
    return {key: item.get(key) for key in keys if key in item}


def _same_file(left: dict[str, Any] | None, right: dict[str, Any] | None) -> bool:
    return _file_identity(left) == _file_identity(right)


def _status_is_conflict(status: dict[str, Any] | None) -> bool:
    if status is None:
        return False
    index = status.get("index")
    worktree = status.get("worktree")
    return index == "U" or worktree == "U" or f"{index}{worktree}" in {"AA", "DD"}


def _status_is_staged(status: dict[str, Any] | None) -> bool:
    if status is None:
        return False
    return status.get("index") not in {" ", "?"}


def _status_is_rename_or_copy(status: dict[str, Any] | None) -> bool:
    return status is not None and (
        "original_path" in status
        or status.get("index") in {"R", "C"}
        or status.get("worktree") in {"R", "C"}
    )


def _block(blockers: list[dict[str, Any]], code: str, **details: Any) -> None:
    blockers.append({"code": code, **details})


def compile_plan(
    source_a: dict[str, Any],
    source_b: dict[str, Any],
    destination: dict[str, Any],
) -> dict[str, Any]:
    validate_snapshot(source_a)
    validate_snapshot(source_b)
    validate_snapshot(destination)
    if source_a.get("mode") != source_b.get("mode") or source_a.get(
        "mode"
    ) != destination.get("mode"):
        raise BulkloadError("source and destination capture modes differ")
    if source_a.get("catalog_sha256") != source_b.get("catalog_sha256"):
        raise BulkloadError(
            "source pass A and pass B differ; no immutable catalog barrier exists"
        )
    if source_a.get("capture_id") == source_b.get("capture_id"):
        raise BulkloadError("source pass A and pass B must be distinct captures")

    source_repositories = catalog_map(source_b)
    destination_repositories = catalog_map(destination)
    blockers: list[dict[str, Any]] = []
    findings: list[dict[str, Any]] = []
    operations: list[dict[str, Any]] = []
    expected_files: list[dict[str, Any]] = []
    expected_repositories: list[dict[str, Any]] = []
    destination_repositories_before: list[dict[str, Any]] = []

    for logical_path in sorted(source_repositories):
        source_repo = source_repositories[logical_path]
        expected_repositories.append(
            {
                "branch": source_repo.get("branch"),
                "dirty_paths": sorted(dirty_paths(source_repo)),
                "head": source_repo.get("head"),
                "logical_path": logical_path,
                "local_refs": [
                    item
                    for item in source_repo.get("refs", [])
                    if not item["name"].startswith("refs/remotes/")
                ],
                "local_refs_sha256": source_repo.get("local_refs_sha256"),
                "status": source_repo.get("status"),
                "status_sha256": source_repo.get("status_sha256"),
            }
        )
        destination_repo = destination_repositories.get(logical_path)
        if destination_repo is None:
            _block(blockers, "destination-repository-missing", repo=logical_path)
            continue

        destination_repositories_before.append(
            {
                "branch": destination_repo.get("branch"),
                "dirty_paths": sorted(dirty_paths(destination_repo)),
                "head": destination_repo.get("head"),
                "logical_path": logical_path,
                "local_refs": [
                    item
                    for item in destination_repo.get("refs", [])
                    if not item["name"].startswith("refs/remotes/")
                ],
                "local_refs_sha256": destination_repo.get("local_refs_sha256"),
                "status": destination_repo.get("status"),
                "status_sha256": destination_repo.get("status_sha256"),
            }
        )
        if source_repo.get("head") != destination_repo.get("head"):
            _block(
                blockers,
                "git-head-mismatch",
                destination=destination_repo.get("head"),
                repo=logical_path,
                source=source_repo.get("head"),
            )
        if source_repo.get("branch") != destination_repo.get("branch"):
            _block(
                blockers,
                "git-branch-mismatch",
                destination=destination_repo.get("branch"),
                repo=logical_path,
                source=source_repo.get("branch"),
            )
        source_local_refs = {
            item["name"]: {
                "object": item["object"],
                "symref": item["symref"],
            }
            for item in source_repo.get("refs", [])
            if not item["name"].startswith("refs/remotes/")
        }
        destination_local_refs = {
            item["name"]: {
                "object": item["object"],
                "symref": item["symref"],
            }
            for item in destination_repo.get("refs", [])
            if not item["name"].startswith("refs/remotes/")
        }
        for name, source_ref in sorted(source_local_refs.items()):
            destination_ref = destination_local_refs.get(name)
            if destination_ref != source_ref:
                _block(
                    blockers,
                    "git-source-ref-missing-or-different",
                    destination=destination_ref,
                    name=name,
                    repo=logical_path,
                    source=source_ref,
                )
        for name in sorted(destination_local_refs.keys() - source_local_refs.keys()):
            details = {
                "name": name,
                "repo": logical_path,
                **destination_local_refs[name],
            }
            if name.startswith("refs/replace/"):
                _block(
                    blockers,
                    "git-destination-replace-ref-extra",
                    **details,
                )
            else:
                findings.append(
                    {
                        "code": "destination-only-local-ref",
                        **details,
                    }
                )
        for side, repository in (
            ("source", source_repo),
            ("destination", destination_repo),
        ):
            for flag, code in (
                ("has_alternates", "git-alternates-unsupported"),
                (
                    "has_content_filters",
                    "git-content-filters-require-separate-plan",
                ),
                ("has_grafts", "git-grafts-unsupported"),
                ("has_gitmodules", "git-submodules-require-separate-plan"),
                ("has_lfs_attributes", "git-lfs-requires-separate-plan"),
                (
                    "has_unportable_attributes",
                    "git-unportable-attributes-require-separate-plan",
                ),
            ):
                if repository.get(flag):
                    _block(blockers, code, repo=logical_path, side=side)

        if source_repo.get("worktrees") != destination_repo.get("worktrees"):
            findings.append(
                {
                    "code": "host-local-worktree-topology-differs",
                    "repo": logical_path,
                    "source_count": len(source_repo.get("worktrees", [])),
                    "destination_count": len(destination_repo.get("worktrees", [])),
                }
            )

        source_dirty = dirty_paths(source_repo)
        destination_dirty = dirty_paths(destination_repo)
        for path in sorted(destination_dirty - source_dirty):
            _block(blockers, "destination-only-dirt", path=path, repo=logical_path)

        for path in sorted(source_dirty):
            source_file = find_file(source_repo, path)
            destination_file = find_file(destination_repo, path)
            if source_file is None:
                _block(
                    blockers, "dirty-path-not-in-catalog", path=path, repo=logical_path
                )
                continue
            status = source_file.get("status")
            if _status_is_conflict(status):
                _block(
                    blockers, "index-conflict-unsupported", path=path, repo=logical_path
                )
                continue
            if _status_is_staged(status):
                _block(
                    blockers,
                    "staged-index-state-unsupported",
                    path=path,
                    repo=logical_path,
                )
                continue
            if _status_is_rename_or_copy(status):
                _block(
                    blockers,
                    "rename-copy-state-unsupported",
                    path=path,
                    repo=logical_path,
                )
                continue
            if source_file.get("kind") == "missing":
                _block(blockers, "deletion-unsupported", path=path, repo=logical_path)
                continue
            if source_file.get("blocked_reason") or not source_file.get("eligible"):
                _block(
                    blockers,
                    "source-path-ineligible",
                    path=path,
                    reason=source_file.get("blocked_reason") or "not eligible",
                    repo=logical_path,
                )
                continue
            expected_files.append(
                {
                    "git_class": source_file.get("git_class"),
                    "identity": _file_identity(source_file),
                    "path": path,
                    "repo": logical_path,
                }
            )
            destination_status = (
                destination_file.get("status") if destination_file is not None else None
            )
            if _status_is_conflict(destination_status):
                _block(
                    blockers, "destination-index-conflict", path=path, repo=logical_path
                )
                continue
            if _status_is_staged(destination_status):
                _block(
                    blockers,
                    "destination-staged-index-state",
                    path=path,
                    repo=logical_path,
                )
                continue
            if _status_is_rename_or_copy(destination_status):
                _block(
                    blockers,
                    "destination-rename-copy-state",
                    path=path,
                    repo=logical_path,
                )
                continue
            if destination_file is not None and (
                destination_file.get("blocked_reason")
                or not destination_file.get("eligible")
                or destination_file.get("kind") == "missing"
            ):
                _block(
                    blockers,
                    "destination-path-ineligible",
                    path=path,
                    reason=destination_file.get("blocked_reason") or "not eligible",
                    repo=logical_path,
                )
                continue
            if _same_file(source_file, destination_file):
                continue
            operations.append(
                {
                    "after": _file_identity(source_file),
                    "before": _file_identity(destination_file),
                    "git_class": source_file.get("git_class"),
                    "op": "copy",
                    "path": path,
                    "repo": logical_path,
                }
            )

    for logical_path in sorted(
        set(destination_repositories) - set(source_repositories)
    ):
        findings.append({"code": "destination-only-repository", "repo": logical_path})

    intent: dict[str, Any] = {
        "blockers": sorted(blockers, key=lambda item: canonical_bytes(item)),
        "destination_catalog_sha256": destination["catalog_sha256"],
        "destination_repositories_before": sorted(
            destination_repositories_before, key=lambda item: item["logical_path"]
        ),
        "destination_snapshot_sha256": destination["snapshot_sha256"],
        "destination_target": {
            "host": destination["host"],
            "root": destination["root"],
        },
        "expected_files": sorted(
            expected_files, key=lambda item: (item["repo"], item["path"])
        ),
        "expected_repositories": sorted(
            expected_repositories, key=lambda item: item["logical_path"]
        ),
        "findings": sorted(findings, key=lambda item: canonical_bytes(item)),
        "mode": source_b["mode"],
        "operations": sorted(
            operations, key=lambda item: (item["repo"], item["path"], item["op"])
        ),
        "ready": not blockers,
        "source_catalog_sha256": source_b["catalog_sha256"],
        "source_snapshot_passes": [
            source_a["snapshot_sha256"],
            source_b["snapshot_sha256"],
        ],
    }
    plan: dict[str, Any] = {
        "created_at": utc_now(),
        "intent": intent,
        "plan_sha256": sha256_bytes(canonical_bytes(intent)),
        "schema": PLAN_SCHEMA,
    }
    plan["envelope_sha256"] = object_digest(plan, "envelope_sha256")
    validate_plan(plan)
    return plan
