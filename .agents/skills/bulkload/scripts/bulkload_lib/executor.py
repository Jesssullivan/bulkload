"""Digest-bound local application, journaling, and independent verification."""

from __future__ import annotations

from contextlib import contextmanager
import fcntl
import hashlib
import os
from pathlib import Path
import socket
import stat
import tempfile
from typing import Any, BinaryIO

from .model import (
    BulkloadError,
    RECEIPT_SCHEMA,
    VERIFY_SCHEMA,
    atomic_write_json,
    canonical_bytes,
    durable_makedirs,
    normalize_relative,
    object_digest,
    safe_join,
    utc_now,
)
from .planner import validate_plan, validate_snapshot
from .scanner import capture_git_runtime, catalog_map, find_file, inspect_path

UNSUPPORTED_GIT_AUTHORITY_FLAGS = (
    "has_alternates",
    "has_content_filters",
    "has_grafts",
    "has_gitmodules",
    "has_lfs_attributes",
    "has_unportable_attributes",
)


def _identity(item: dict[str, Any] | None) -> dict[str, Any] | None:
    if item is None:
        return None
    keys = ("kind", "mode", "sha256", "size")
    return {key: item.get(key) for key in keys if key in item}


def _repo_root(fleet_root: Path, logical: str) -> Path:
    fleet_root = fleet_root.expanduser().resolve()
    if logical == ".":
        return fleet_root
    return safe_join(fleet_root, normalize_relative(logical), allow_leaf_symlink=False)


def _current_identity(root: Path, path: str, git_class: str) -> dict[str, Any] | None:
    target = safe_join(root, path)
    try:
        target.lstat()
    except FileNotFoundError:
        return None
    return _identity(inspect_path(root, path, git_class, None))


def _append_journal(stream: BinaryIO, event: dict[str, Any]) -> None:
    stream.write(canonical_bytes(event) + b"\n")
    stream.flush()
    os.fsync(stream.fileno())


def _copy_backup(source: Path, destination: Path, expected: dict[str, Any]) -> None:
    durable_makedirs(destination.parent)
    if destination.exists() or destination.is_symlink():
        actual = _current_identity(destination.parent, destination.name, "tracked")
        if actual != expected:
            raise BulkloadError(
                f"existing backup has unexpected identity: {destination}"
            )
        return
    source_info = source.lstat()
    if stat.S_ISREG(source_info.st_mode):
        _atomic_copy_regular(source, destination, expected)
    else:
        raise BulkloadError(f"backup source has unsupported type: {source}")
    actual = _current_identity(destination.parent, destination.name, "tracked")
    if actual != expected:
        raise BulkloadError(f"backup verification failed: {destination}")


def _atomic_copy_regular(
    source: Path, destination: Path, expected: dict[str, Any]
) -> None:
    source_before = source.lstat()
    if not stat.S_ISREG(source_before.st_mode):
        raise BulkloadError(f"source ceased to be a regular file: {source}")
    source_flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    source_fd = os.open(source, source_flags)
    temp_fd, temp_name = tempfile.mkstemp(
        prefix=f".{destination.name}.bulkload-", dir=destination.parent
    )
    temp_path = Path(temp_name)
    digest = hashlib.sha256()
    size = 0
    try:
        opened = os.fstat(source_fd)
        if _stat_identity(opened) != _stat_identity(source_before):
            raise BulkloadError(f"source changed while opening: {source}")
        while True:
            chunk = os.read(source_fd, 1024 * 1024)
            if not chunk:
                break
            digest.update(chunk)
            size += len(chunk)
            view = memoryview(chunk)
            while view:
                written = os.write(temp_fd, view)
                view = view[written:]
        source_after = os.fstat(source_fd)
        if _stat_identity(source_after) != _stat_identity(source_before):
            raise BulkloadError(f"source changed while copying: {source}")
        if digest.hexdigest() != expected.get("sha256") or size != expected.get("size"):
            raise BulkloadError(
                f"source content no longer matches the accepted plan: {source}"
            )
        os.fchmod(temp_fd, int(expected["mode"], 8))
        os.fsync(temp_fd)
        os.close(temp_fd)
        temp_fd = -1
        os.replace(temp_path, destination)
        _fsync_directory(destination.parent)
    finally:
        os.close(source_fd)
        if temp_fd >= 0:
            os.close(temp_fd)
        temp_path.unlink(missing_ok=True)


def _stat_identity(value: os.stat_result) -> tuple[int, int, int, int, int]:
    return (
        value.st_dev,
        value.st_ino,
        value.st_size,
        value.st_mtime_ns,
        value.st_ctime_ns,
    )


def _fsync_directory(path: Path) -> None:
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def _paths_overlap(left: Path, right: Path) -> bool:
    for candidate, parent in ((left, right), (right, left)):
        try:
            candidate.relative_to(parent)
        except ValueError:
            continue
        return True
    return False


def _runtime_map(values: list[dict[str, Any]]) -> dict[str, dict[str, Any]]:
    return {item["logical_path"]: item for item in values}


def _unsafe_status(status: dict[str, Any]) -> bool:
    index = status.get("index")
    worktree = status.get("worktree")
    return (
        index not in {" ", "?"}
        or index == "U"
        or worktree == "U"
        or f"{index}{worktree}" in {"AA", "DD"}
    )


def _preflight_plan(
    intent: dict[str, Any], source_root: Path, destination_root: Path
) -> list[dict[str, Any]]:
    operations = intent.get("operations", [])
    expected_file_items = intent.get("expected_files", [])
    expected_files = {
        (item["repo"], item["path"]): item for item in expected_file_items
    }
    operation_keys = {(item["repo"], item["path"]) for item in operations}
    expected_source_items = intent.get("expected_repositories", [])
    expected_destination_items = intent.get("destination_repositories_before", [])
    expected_sources = _runtime_map(expected_source_items)
    expected_destinations = _runtime_map(expected_destination_items)
    if (
        len(expected_sources) != len(expected_source_items)
        or len(expected_destinations) != len(expected_destination_items)
        or set(expected_sources) != set(expected_destinations)
    ):
        raise BulkloadError("plan repository preconditions are inconsistent")

    for logical, source_expected in expected_sources.items():
        source_repo = _repo_root(source_root, logical)
        destination_repo = _repo_root(destination_root, logical)
        source_runtime = capture_git_runtime(source_repo)
        destination_runtime = capture_git_runtime(destination_repo)
        for side, runtime in (
            ("source", source_runtime),
            ("destination", destination_runtime),
        ):
            active = sorted(
                flag for flag in UNSUPPORTED_GIT_AUTHORITY_FLAGS if runtime.get(flag)
            )
            if active:
                raise BulkloadError(
                    f"{side} acquired unsupported Git authority: {logical}: {active}"
                )
        for field in (
            "branch",
            "git_operation_state",
            "head",
            "local_refs",
            "local_refs_sha256",
            "recovery_roots",
            "recovery_roots_sha256",
            "status",
            "status_sha256",
        ):
            if source_runtime.get(field) != source_expected.get(field):
                raise BulkloadError(f"source Git {field} changed: {logical}")
        destination_expected = expected_destinations[logical]
        for field in (
            "branch",
            "git_operation_state",
            "head",
            "local_refs",
            "local_refs_sha256",
            "recovery_roots",
            "recovery_roots_sha256",
        ):
            if destination_runtime.get(field) != destination_expected.get(field):
                raise BulkloadError(f"destination Git {field} changed: {logical}")
        for field in ("branch", "head"):
            if source_runtime[field] != destination_runtime[field]:
                raise BulkloadError(
                    f"source and destination Git {field} differ: {logical}"
                )
        source_refs = {item["name"]: item for item in source_runtime["local_refs"]}
        destination_refs = {
            item["name"]: item for item in destination_runtime["local_refs"]
        }
        for name, source_ref in source_refs.items():
            if destination_refs.get(name) != source_ref:
                raise BulkloadError(
                    f"source ref differs at destination: {logical}: {name}"
                )
        extra_replace_refs = sorted(
            name
            for name in destination_refs.keys() - source_refs.keys()
            if name.startswith("refs/replace/")
        )
        if extra_replace_refs:
            raise BulkloadError(
                f"destination has extra replacement refs: {logical}: {extra_replace_refs}"
            )
        if any(_unsafe_status(item) for item in source_runtime["status"]):
            raise BulkloadError(f"source index or conflict state is unsafe: {logical}")
        if any(_unsafe_status(item) for item in destination_runtime["status"]):
            raise BulkloadError(
                f"destination index or conflict state is unsafe: {logical}"
            )
        allowed_dirty = {item["path"] for item in source_runtime["status"]}
        live_dirty = {item["path"] for item in destination_runtime["status"]}
        unexpected_dirty = sorted(live_dirty - allowed_dirty)
        if unexpected_dirty:
            raise BulkloadError(
                f"destination acquired unplanned dirt: {logical}: {unexpected_dirty}"
            )

    for key, expected in expected_files.items():
        logical, relative = key
        source_identity = _current_identity(
            _repo_root(source_root, logical), relative, expected["git_class"]
        )
        if source_identity != expected["identity"]:
            raise BulkloadError(f"source expected file changed: {logical}/{relative}")
        if key not in operation_keys:
            destination_identity = _current_identity(
                _repo_root(destination_root, logical), relative, expected["git_class"]
            )
            if destination_identity != expected["identity"]:
                raise BulkloadError(
                    f"destination unchanged-file precondition changed: {logical}/{relative}"
                )

    preflight: list[dict[str, Any]] = []
    for operation in operations:
        if operation.get("op") != "copy":
            raise BulkloadError(f"unsupported operation: {operation.get('op')}")
        logical = operation["repo"]
        relative = normalize_relative(operation["path"])
        source_repo = _repo_root(source_root, logical)
        destination_repo = _repo_root(destination_root, logical)
        source_path = safe_join(source_repo, relative, allow_leaf_symlink=True)
        destination_path = safe_join(
            destination_repo, relative, allow_leaf_symlink=True
        )
        source_identity = _current_identity(
            source_repo, relative, operation["git_class"]
        )
        destination_identity = _current_identity(
            destination_repo, relative, operation["git_class"]
        )
        if source_identity != operation.get("after"):
            raise BulkloadError(f"source precondition changed: {logical}/{relative}")
        if destination_identity == operation.get("after"):
            state = "already-applied"
        elif destination_identity == operation.get("before"):
            state = "pending"
        else:
            raise BulkloadError(
                f"destination precondition changed: {logical}/{relative}"
            )
        preflight.append(
            {
                "destination": destination_path,
                "logical": logical,
                "operation": operation,
                "relative": relative,
                "source": source_path,
                "state": state,
            }
        )
    return preflight


def _open_private_regular(path: Path, flags: int) -> int:
    descriptor = os.open(
        path,
        flags | getattr(os, "O_NOFOLLOW", 0),
        0o600,
    )
    try:
        if not stat.S_ISREG(os.fstat(descriptor).st_mode):
            raise BulkloadError(f"state path is not a regular file: {path}")
        os.fchmod(descriptor, 0o600)
        return descriptor
    except BaseException:
        os.close(descriptor)
        raise


@contextmanager
def _lock_destination_repositories(
    intent: dict[str, Any], destination_root: Path
) -> Any:
    paths = {destination_root}
    for item in intent.get("expected_repositories", []):
        paths.add(_repo_root(destination_root, item["logical_path"]))
    opened: dict[tuple[int, int], int] = {}
    try:
        flags = (
            os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0)
        )
        for path in paths:
            descriptor = os.open(path, flags)
            identity = os.fstat(descriptor)
            key = (identity.st_dev, identity.st_ino)
            if key in opened:
                os.close(descriptor)
            else:
                opened[key] = descriptor
        for key in sorted(opened):
            fcntl.flock(opened[key], fcntl.LOCK_EX)
        yield
    finally:
        for key in sorted(opened, reverse=True):
            descriptor = opened[key]
            try:
                fcntl.flock(descriptor, fcntl.LOCK_UN)
            finally:
                os.close(descriptor)


def apply_plan(
    plan: dict[str, Any],
    *,
    source_root: Path,
    destination_root: Path,
    accepted_digest: str,
    state_root: Path,
    receipt_path: Path,
) -> dict[str, Any]:
    validate_plan(plan)
    if accepted_digest != plan["plan_sha256"]:
        raise BulkloadError("operator-supplied plan digest does not match")
    intent = plan["intent"]
    if not intent.get("ready") or intent.get("blockers"):
        raise BulkloadError("blocked plans cannot be applied")

    source_root = source_root.expanduser().resolve()
    destination_root = destination_root.expanduser().resolve()
    state_root = state_root.expanduser().resolve()
    receipt_path = receipt_path.expanduser().resolve()
    if hasattr(os, "geteuid") and os.geteuid() == 0:
        raise BulkloadError("apply refuses to run as root")
    if _paths_overlap(source_root, destination_root):
        raise BulkloadError("source and destination roots must be disjoint")
    if _paths_overlap(state_root, source_root) or _paths_overlap(
        state_root, destination_root
    ):
        raise BulkloadError("state root must be disjoint from source and destination")
    if (
        _paths_overlap(receipt_path, source_root)
        or _paths_overlap(receipt_path, destination_root)
        or _paths_overlap(receipt_path, state_root)
    ):
        raise BulkloadError(
            "receipt must be outside source, destination, and state roots"
        )
    destination_target = intent["destination_target"]
    if socket.gethostname() != destination_target["host"]:
        raise BulkloadError("destination host does not match the accepted plan")
    if str(destination_root) != destination_target["root"]:
        raise BulkloadError("destination root does not match the accepted plan")

    durable_makedirs(state_root)
    if not stat.S_ISDIR(state_root.lstat().st_mode):
        raise BulkloadError("state root must be a real directory")
    if stat.S_IMODE(state_root.stat().st_mode) & 0o077:
        raise BulkloadError("state root must not be group- or world-accessible")

    run_root = safe_join(
        state_root, f"runs/{plan['plan_sha256']}", allow_leaf_symlink=False
    )
    backup_root = safe_join(
        state_root, f"backups/{plan['plan_sha256']}", allow_leaf_symlink=False
    )
    durable_makedirs(run_root)
    journal_path = run_root / "journal.jsonl"

    applied: list[dict[str, Any]] = []
    with _lock_destination_repositories(intent, destination_root):
        preflight = _preflight_plan(intent, source_root, destination_root)
        journal_descriptor = _open_private_regular(
            journal_path, os.O_WRONLY | os.O_APPEND | os.O_CREAT
        )
        try:
            _fsync_directory(journal_path.parent)
        except BaseException:
            os.close(journal_descriptor)
            raise
        journal = os.fdopen(journal_descriptor, "ab", buffering=0)
        try:
            _apply_locked(
                journal,
                preflight,
                applied,
                plan,
                backup_root,
                destination_root,
            )
        finally:
            journal.close()

    receipt: dict[str, Any] = {
        "applied_at": utc_now(),
        "operations": applied,
        "plan_sha256": plan["plan_sha256"],
        "schema": RECEIPT_SCHEMA,
        "state_root": str(state_root),
    }
    receipt["receipt_sha256"] = object_digest(receipt, "receipt_sha256")
    atomic_write_json(receipt_path, receipt)
    return receipt


def _apply_locked(
    journal: BinaryIO,
    preflight: list[dict[str, Any]],
    applied: list[dict[str, Any]],
    plan: dict[str, Any],
    backup_root: Path,
    destination_root: Path,
) -> None:
    _append_journal(
        journal,
        {
            "event": "apply-start",
            "plan_sha256": plan["plan_sha256"],
            "time": utc_now(),
        },
    )
    for item in preflight:
        operation = item["operation"]
        relative = item["relative"]
        logical = item["logical"]
        destination = item["destination"]
        source = item["source"]
        backup_relative = f"{'__root__' if logical == '.' else logical}/{relative}"
        backup = safe_join(backup_root, backup_relative, allow_leaf_symlink=True)
        if item["state"] == "already-applied":
            current = _current_identity(
                _repo_root(destination_root, logical), relative, operation["git_class"]
            )
            if current != operation["after"]:
                raise BulkloadError(
                    f"destination changed after preflight: {logical}/{relative}"
                )
            if operation.get("before") is not None:
                backup_identity = _current_identity(
                    backup.parent, backup.name, operation["git_class"]
                )
                if backup_identity != operation["before"]:
                    raise BulkloadError(
                        f"already-applied replacement has no exact backup: {logical}/{relative}"
                    )
            result = {"path": relative, "repo": logical, "result": "verified-existing"}
            applied.append(result)
            _append_journal(
                journal, {"event": "copy-skip", **result, "time": utc_now()}
            )
            continue

        durable_makedirs(destination.parent)
        current = _current_identity(
            _repo_root(destination_root, logical), relative, operation["git_class"]
        )
        if current != operation.get("before"):
            raise BulkloadError(
                f"destination changed after preflight: {logical}/{relative}"
            )
        if operation.get("before") is not None:
            _copy_backup(destination, backup, operation["before"])
            current = _current_identity(
                _repo_root(destination_root, logical), relative, operation["git_class"]
            )
            if current != operation["before"]:
                raise BulkloadError(
                    f"destination changed during backup: {logical}/{relative}"
                )
            _append_journal(
                journal,
                {
                    "event": "backup-complete",
                    "path": relative,
                    "repo": logical,
                    "time": utc_now(),
                },
            )

        if operation["after"]["kind"] != "file":
            raise BulkloadError(
                f"unsupported source kind: {operation['after']['kind']}"
            )
        _atomic_copy_regular(source, destination, operation["after"])

        actual = _current_identity(
            _repo_root(destination_root, logical), relative, operation["git_class"]
        )
        if actual != operation["after"]:
            raise BulkloadError(f"post-copy verification failed: {logical}/{relative}")
        result = {"path": relative, "repo": logical, "result": "copied"}
        applied.append(result)
        _append_journal(
            journal, {"event": "copy-complete", **result, "time": utc_now()}
        )

    _append_journal(
        journal,
        {
            "event": "apply-complete",
            "plan_sha256": plan["plan_sha256"],
            "time": utc_now(),
        },
    )


def verify_plan(
    plan: dict[str, Any], destination: dict[str, Any], accepted_digest: str
) -> dict[str, Any]:
    validate_plan(plan)
    if accepted_digest != plan["plan_sha256"]:
        raise BulkloadError("operator-supplied plan digest does not match")
    validate_snapshot(destination)
    intent = plan["intent"]
    repositories = catalog_map(destination)
    failures: list[dict[str, Any]] = []
    if destination["snapshot_sha256"] == intent["destination_snapshot_sha256"]:
        failures.append(
            {
                "code": "destination-snapshot-not-fresh",
                "snapshot_sha256": destination["snapshot_sha256"],
            }
        )
    destination_target = intent["destination_target"]
    if destination.get("mode") != intent["mode"]:
        failures.append(
            {
                "actual": destination.get("mode"),
                "code": "destination-mode-mismatch",
                "expected": intent["mode"],
            }
        )
    for field in ("host", "root"):
        if destination.get(field) != destination_target[field]:
            failures.append(
                {
                    "actual": destination.get(field),
                    "code": f"destination-{field}-mismatch",
                    "expected": destination_target[field],
                }
            )

    destination_before = _runtime_map(intent.get("destination_repositories_before", []))
    for expected in intent.get("expected_repositories", []):
        logical = expected["logical_path"]
        actual = repositories.get(logical)
        if actual is None:
            failures.append({"code": "repository-missing", "repo": logical})
            continue
        for flag in UNSUPPORTED_GIT_AUTHORITY_FLAGS:
            if actual.get(flag):
                failures.append(
                    {
                        "authority": flag,
                        "code": "repository-unsupported-git-authority",
                        "repo": logical,
                    }
                )
        for field in ("branch", "head", "status_sha256"):
            if actual.get(field) != expected.get(field):
                failures.append(
                    {
                        "actual": actual.get(field),
                        "code": f"repository-{field}-mismatch",
                        "expected": expected.get(field),
                        "repo": logical,
                    }
                )
        before = destination_before.get(logical)
        if before is None or actual.get("local_refs_sha256") != before.get(
            "local_refs_sha256"
        ):
            failures.append(
                {
                    "actual": actual.get("local_refs_sha256"),
                    "code": "repository-local_refs_sha256-mismatch",
                    "expected": (before.get("local_refs_sha256") if before else None),
                    "repo": logical,
                }
            )
        if before is None or actual.get("recovery_roots_sha256") != before.get(
            "recovery_roots_sha256"
        ):
            failures.append(
                {
                    "actual": actual.get("recovery_roots_sha256"),
                    "code": "repository-recovery_roots_sha256-mismatch",
                    "expected": (
                        before.get("recovery_roots_sha256") if before else None
                    ),
                    "repo": logical,
                }
            )
    for expected in intent.get("expected_files", []):
        logical = expected["repo"]
        repo = repositories.get(logical)
        actual = find_file(repo, expected["path"]) if repo is not None else None
        if _identity(actual) != expected.get("identity"):
            failures.append(
                {
                    "actual": _identity(actual),
                    "code": "file-identity-mismatch",
                    "expected": expected.get("identity"),
                    "path": expected["path"],
                    "repo": logical,
                }
            )

    result: dict[str, Any] = {
        "destination_snapshot_sha256": destination["snapshot_sha256"],
        "failures": failures,
        "plan_sha256": plan["plan_sha256"],
        "schema": VERIFY_SCHEMA,
        "verified": not failures and not intent.get("blockers"),
        "verified_at": utc_now(),
    }
    result["verification_sha256"] = object_digest(result, "verification_sha256")
    return result


def export_copy_paths(plan: dict[str, Any], accepted_digest: str) -> list[str]:
    validate_plan(plan)
    if accepted_digest != plan["plan_sha256"]:
        raise BulkloadError("operator-supplied plan digest does not match")
    intent = plan["intent"]
    if not intent.get("ready") or intent.get("blockers"):
        raise BulkloadError("blocked plans cannot export a transfer allowlist")
    values: list[str] = []
    for expected in intent.get("expected_files", []):
        logical = expected["repo"]
        relative = normalize_relative(expected["path"])
        values.append(
            relative if logical == "." else f"{normalize_relative(logical)}/{relative}"
        )
    return sorted(values)
