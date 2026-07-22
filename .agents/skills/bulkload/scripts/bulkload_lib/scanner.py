"""Read-only Git, worktree, and working-byte catalog acquisition."""

from __future__ import annotations

from contextlib import contextmanager
import hashlib
import os
from pathlib import Path
import socket
import stat
import subprocess
from typing import Any, Iterable
import unicodedata
import uuid

from .model import (
    BulkloadError,
    SNAPSHOT_SCHEMA,
    canonical_bytes,
    normalize_relative,
    object_digest,
    portability_reason,
    sanitize_remote_url,
    sensitive_reason,
    sha256_bytes,
    utc_now,
)

DEFAULT_MAX_FILES = 250_000
DEFAULT_MAX_BYTES = 50 * 1024 * 1024 * 1024


def _git_environment() -> dict[str, str]:
    environment = {
        key: value for key, value in os.environ.items() if not key.startswith("GIT_")
    }
    environment.update(
        {
            "GIT_ATTR_NOSYSTEM": "1",
            "GIT_NO_LAZY_FETCH": "1",
            "GIT_NO_REPLACE_OBJECTS": "1",
            "GIT_OPTIONAL_LOCKS": "0",
            "GIT_PAGER": "cat",
            "GIT_TERMINAL_PROMPT": "0",
        }
    )
    return environment


def _git(
    repo: Path,
    *arguments: str,
    check: bool = True,
    input_data: bytes | None = None,
) -> bytes:
    io_arguments: dict[str, Any]
    if input_data is None:
        io_arguments = {"stdin": subprocess.DEVNULL}
    else:
        io_arguments = {"input": input_data}
    process = subprocess.run(
        ["git", "-c", "core.fsmonitor=false", "-C", str(repo), *arguments],
        env=_git_environment(),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
        **io_arguments,
    )
    if check and process.returncode != 0:
        detail = process.stderr.decode("utf-8", "replace").strip()
        raise BulkloadError(f"git {' '.join(arguments)} failed in {repo}: {detail}")
    return process.stdout


def _decode_path(value: bytes) -> str:
    try:
        decoded = value.decode("utf-8")
    except UnicodeDecodeError as error:
        raise BulkloadError("non-UTF-8 filename is unsupported in v1") from error
    return normalize_relative(decoded)


def _nul_paths(payload: bytes) -> list[str]:
    return [_decode_path(part) for part in payload.split(b"\0") if part]


def _parse_index(payload: bytes) -> dict[str, list[dict[str, Any]]]:
    entries: dict[str, list[dict[str, Any]]] = {}
    for raw in payload.split(b"\0"):
        if not raw:
            continue
        metadata, separator, raw_path = raw.partition(b"\t")
        fields = metadata.split(b" ")
        if not separator or len(fields) != 3:
            raise BulkloadError("unexpected git ls-files --stage record")
        mode, object_id, stage = fields
        path = _decode_path(raw_path)
        entry = {
            "mode": mode.decode("ascii"),
            "object": object_id.decode("ascii"),
            "stage": int(stage.decode("ascii")),
        }
        entries.setdefault(path, []).append(entry)
    for values in entries.values():
        values.sort(key=lambda item: (item["stage"], item["mode"], item["object"]))
    return entries


def _parse_tree(payload: bytes) -> dict[str, dict[str, str]]:
    entries: dict[str, dict[str, str]] = {}
    for raw in payload.split(b"\0"):
        if not raw:
            continue
        metadata, separator, raw_path = raw.partition(b"\t")
        fields = metadata.split(b" ")
        if not separator or len(fields) != 3:
            raise BulkloadError("unexpected git ls-tree record")
        mode, object_type, object_id = fields
        path = _decode_path(raw_path)
        if path in entries:
            raise BulkloadError(f"duplicate git ls-tree path: {path}")
        entries[path] = {
            "mode": mode.decode("ascii", "strict"),
            "object": object_id.decode("ascii", "strict"),
            "type": object_type.decode("ascii", "strict"),
        }
    return entries


def discover_repositories(root: Path, mode: str) -> tuple[list[Path], list[str]]:
    root = root.expanduser().resolve()
    if mode == "repo":
        top = _git(root, "rev-parse", "--show-toplevel").decode("utf-8").strip()
        if Path(top).resolve() != root:
            raise BulkloadError(f"repo root must be the Git top-level: {top}")
        return [root], []
    if mode != "fleet":
        raise BulkloadError(f"unknown capture mode: {mode}")

    repositories: list[Path] = []
    errors: list[str] = []

    def record_walk_error(error: OSError) -> None:
        errors.append(
            f"walk error at {error.filename or root}: {type(error).__name__}: {error}"
        )

    for directory, dirnames, filenames in os.walk(
        root, followlinks=False, onerror=record_walk_error
    ):
        current = Path(directory)
        marker_present = ".git" in dirnames or ".git" in filenames
        if marker_present:
            repositories.append(current.resolve())
            dirnames[:] = []
            continue
        retained: list[str] = []
        for name in sorted(dirnames):
            child = current / name
            if child.is_symlink():
                errors.append(
                    f"symlink directory is unsupported during fleet discovery: {child}"
                )
                continue
            retained.append(name)
        dirnames[:] = retained
    if not repositories:
        errors.append(f"no Git repositories found beneath {root}")
    return sorted(set(repositories), key=lambda item: item.as_posix()), errors


def _parse_worktrees(payload: bytes) -> list[dict[str, Any]]:
    records: list[dict[str, Any]] = []
    current: dict[str, Any] = {}
    for raw in payload.split(b"\0"):
        if not raw:
            if current:
                records.append(current)
                current = {}
            continue
        key, separator, value = raw.partition(b" ")
        decoded_key = key.decode("ascii", "strict")
        decoded_value = value.decode("utf-8", "strict") if separator else True
        if decoded_key == "worktree":
            current["path"] = decoded_value
        elif decoded_key == "HEAD":
            current["head"] = decoded_value
        elif decoded_key == "branch":
            prefix = "refs/heads/"
            current["branch"] = (
                decoded_value[len(prefix) :]
                if isinstance(decoded_value, str) and decoded_value.startswith(prefix)
                else decoded_value
            )
        elif decoded_key in {"bare", "detached", "prunable"}:
            current[decoded_key] = decoded_value
        elif decoded_key == "locked":
            current["locked"] = decoded_value
        else:
            current[decoded_key] = decoded_value
    if current:
        records.append(current)
    return sorted(records, key=lambda item: str(item.get("path", "")))


@contextmanager
def _open_parent(root: Path, relative: str) -> Iterable[tuple[int, str]]:
    parts = normalize_relative(relative).split("/")
    flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(root, flags)
    try:
        for component in parts[:-1]:
            child = os.open(component, flags, dir_fd=descriptor)
            os.close(descriptor)
            descriptor = child
        yield descriptor, parts[-1]
    finally:
        os.close(descriptor)


def _stable_file_digest(
    parent_descriptor: int,
    leaf: str,
    display_path: Path,
    before: os.stat_result,
    maximum_bytes: int | None,
    git_object_format: str | None,
) -> tuple[str, int, str | None]:
    if not stat.S_ISREG(before.st_mode):
        raise BulkloadError(f"not a regular file: {display_path}")
    if maximum_bytes is not None and before.st_size > maximum_bytes:
        raise BulkloadError(
            f"file exceeds remaining byte budget: {before.st_size} > {maximum_bytes}"
        )
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(leaf, flags, dir_fd=parent_descriptor)
    digest = hashlib.sha256()
    git_digest = (
        hashlib.new(git_object_format) if git_object_format is not None else None
    )
    if git_digest is not None:
        git_digest.update(f"blob {before.st_size}\0".encode("ascii"))
    try:
        opened = os.fstat(descriptor)
        identity = _stat_identity(opened)
        if identity != _stat_identity(before) or not stat.S_ISREG(opened.st_mode):
            raise BulkloadError(f"file changed while opening: {display_path}")
        while True:
            chunk = os.read(descriptor, 1024 * 1024)
            if not chunk:
                break
            digest.update(chunk)
            if git_digest is not None:
                git_digest.update(chunk)
        after_open = os.fstat(descriptor)
    finally:
        os.close(descriptor)
    after_path = os.stat(leaf, dir_fd=parent_descriptor, follow_symlinks=False)
    if _stat_identity(after_open) != identity or _stat_identity(after_path) != identity:
        raise BulkloadError(f"file changed while hashing: {display_path}")
    return (
        digest.hexdigest(),
        before.st_size,
        git_digest.hexdigest() if git_digest is not None else None,
    )


def _stat_identity(value: os.stat_result) -> tuple[int, int, int, int, int]:
    return (
        value.st_dev,
        value.st_ino,
        value.st_size,
        value.st_mtime_ns,
        value.st_ctime_ns,
    )


def inspect_path(
    root: Path,
    relative: str,
    git_class: str,
    status: dict[str, Any] | None,
    maximum_bytes: int | None = None,
    git_object_format: str | None = None,
) -> dict[str, Any]:
    path = root / relative
    record: dict[str, Any] = {
        "git_class": git_class,
        "path": relative,
        "status": status,
    }
    reason = sensitive_reason(relative) or portability_reason(relative)
    if reason is not None:
        record.update(
            {
                "blocked_reason": reason,
                "eligible": False,
                "kind": "redacted",
            }
        )
        return record
    try:
        with _open_parent(root, relative) as (parent_descriptor, leaf):
            info = os.stat(leaf, dir_fd=parent_descriptor, follow_symlinks=False)
            mode = stat.S_IMODE(info.st_mode)
            record["mode"] = format(mode, "04o")
            if mode & 0o7000:
                record.update(
                    {
                        "blocked_reason": "setuid, setgid, and sticky modes are unsupported",
                        "eligible": False,
                        "kind": "unsafe-mode",
                    }
                )
            elif stat.S_ISREG(info.st_mode):
                digest, size, git_blob = _stable_file_digest(
                    parent_descriptor,
                    leaf,
                    path,
                    info,
                    maximum_bytes,
                    git_object_format,
                )
                record.update(
                    {
                        "eligible": git_class != "ignored",
                        "kind": "file",
                        "sha256": digest,
                        "size": size,
                    }
                )
                if git_blob is not None:
                    record["git_blob_oid"] = git_blob
            elif stat.S_ISLNK(info.st_mode):
                target = os.readlink(leaf, dir_fd=parent_descriptor)
                after = os.stat(leaf, dir_fd=parent_descriptor, follow_symlinks=False)
                if _stat_identity(after) != _stat_identity(info):
                    raise BulkloadError(f"symlink changed while reading: {path}")
                target_bytes = os.fsencode(target)
                target_size = len(target_bytes)
                if maximum_bytes is not None and target_size > maximum_bytes:
                    raise BulkloadError(
                        f"symlink exceeds remaining byte budget: {target_size} > {maximum_bytes}"
                    )
                record.update(
                    {
                        "blocked_reason": "symlink mutations are unsupported in v1",
                        "eligible": False,
                        "kind": "symlink",
                        "sha256": hashlib.sha256(target_bytes).hexdigest(),
                        "size": target_size,
                    }
                )
                if git_object_format is not None:
                    git_digest = hashlib.new(git_object_format)
                    git_digest.update(f"blob {target_size}\0".encode("ascii"))
                    git_digest.update(target_bytes)
                    record["git_blob_oid"] = git_digest.hexdigest()
            else:
                record.update(
                    {
                        "blocked_reason": "special files are unsupported",
                        "eligible": False,
                        "kind": "special",
                    }
                )
    except FileNotFoundError:
        record.update(
            {
                "blocked_reason": "tracked path is absent",
                "eligible": False,
                "kind": "missing",
            }
        )
    return record


def _mode_matches_index(item: dict[str, Any], index_mode: str) -> bool:
    if index_mode == "120000":
        return item.get("kind") == "symlink"
    if index_mode not in {"100644", "100755"} or item.get("kind") != "file":
        return False
    mode = item.get("mode")
    if not isinstance(mode, str):
        return False
    return bool(int(mode, 8) & 0o111) == (index_mode == "100755")


def _derive_status(
    index_entries: dict[str, list[dict[str, Any]]],
    head_entries: dict[str, dict[str, str]],
    files: list[dict[str, Any]],
) -> list[dict[str, Any]]:
    tracked_files = {
        item["path"]: item for item in files if item["git_class"] == "tracked"
    }
    records: list[dict[str, Any]] = []
    authority_paths = set(index_entries) | set(head_entries)
    for path in sorted(authority_paths):
        entries = index_entries.get(path, [])
        if not entries:
            records.append({"index": "D", "path": path, "worktree": " "})
            continue
        stage_zero = [entry for entry in entries if entry["stage"] == 0]
        if len(stage_zero) != 1 or any(entry["stage"] != 0 for entry in entries):
            records.append({"index": "U", "path": path, "worktree": "U"})
            continue

        index_entry = stage_zero[0]
        head_entry = head_entries.get(path)
        if head_entry is None:
            index_code = "A"
        elif index_entry["mode"] != head_entry["mode"]:
            index_code = "T"
        elif index_entry["object"] != head_entry["object"]:
            index_code = "M"
        else:
            index_code = " "

        item = tracked_files.get(path)
        if item is None or item.get("kind") == "missing":
            worktree_code = "D"
        elif not _mode_matches_index(item, index_entry["mode"]):
            worktree_code = "T"
        elif item.get("git_blob_oid") != index_entry["object"]:
            worktree_code = "M"
        else:
            worktree_code = " "
        if index_code != " " or worktree_code != " ":
            records.append(
                {"index": index_code, "path": path, "worktree": worktree_code}
            )

    for item in files:
        if item["git_class"] == "untracked" and item["path"] not in authority_paths:
            records.append({"index": "?", "path": item["path"], "worktree": "?"})
    return sorted(records, key=lambda item: item["path"])


def _capture_remotes(repo: Path) -> list[dict[str, Any]]:
    remotes: list[dict[str, Any]] = []
    for name in _git(repo, "remote").decode("utf-8").splitlines():
        urls = (
            _git(repo, "remote", "get-url", "--all", name).decode("utf-8").splitlines()
        )
        remotes.append(
            {
                "name": name,
                "urls": sorted({sanitize_remote_url(url) for url in urls}),
            }
        )
    return sorted(remotes, key=lambda item: item["name"])


def _capture_refs(repo: Path) -> list[dict[str, str | None]]:
    payload = _git(
        repo,
        "for-each-ref",
        "--format=%(refname)%00%(objectname)%00%(symref)",
    )
    refs: list[dict[str, str | None]] = []
    for raw in payload.splitlines():
        if not raw:
            continue
        fields = raw.split(b"\0")
        if len(fields) != 3:
            raise BulkloadError("git for-each-ref emitted an incomplete record")
        refname = fields[0].decode("utf-8", "strict")
        object_name = fields[1].decode("ascii", "strict")
        symref_text = fields[2].decode("utf-8", "strict")
        refs.append(
            {
                "name": refname,
                "object": object_name,
                "symref": symref_text or None,
            }
        )
    return sorted(refs, key=lambda item: item["name"])


def _verify_local_ref_object_closure(
    repo: Path,
    head: str,
    refs: list[dict[str, str | None]],
) -> None:
    roots = sorted(
        {
            head,
            *(
                str(item["object"])
                for item in refs
                if not str(item["name"]).startswith("refs/remotes/")
            ),
        }
    )
    _git(
        repo,
        "rev-list",
        "--objects",
        "--stdin",
        "--quiet",
        "--missing=error",
        input_data=("\n".join(roots) + "\n").encode("ascii"),
    )
    _git(
        repo,
        "fsck",
        "--full",
        "--no-dangling",
        "--no-reflogs",
        "--no-progress",
    )


def _optional_git_text(repo: Path, *arguments: str) -> str | None:
    process = subprocess.run(
        ["git", "-c", "core.fsmonitor=false", "-C", str(repo), *arguments],
        env=_git_environment(),
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    if process.returncode != 0:
        return None
    return process.stdout.decode("utf-8").strip() or None


def _git_config_value(repo: Path, *arguments: str) -> tuple[bool, str]:
    process = subprocess.run(
        [
            "git",
            "-c",
            "core.fsmonitor=false",
            "-C",
            str(repo),
            "config",
            *arguments,
        ],
        env=_git_environment(),
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    if process.returncode == 1:
        return False, ""
    if process.returncode != 0:
        raise BulkloadError("effective Git configuration query failed")
    return True, process.stdout.decode("utf-8", "strict").strip()


def _has_promisor_configuration(repo: Path) -> bool:
    present, _ = _git_config_value(repo, "--get", "extensions.partialClone")
    if present:
        return True
    for name in _git(repo, "remote").decode("utf-8", "strict").splitlines():
        present, _ = _git_config_value(
            repo, "--get", f"remote.{name}.partialclonefilter"
        )
        if present:
            return True
        present, _ = _git_config_value(repo, "--get", f"remote.{name}.promisor")
        if not present:
            continue
        typed_present, normalized = _git_config_value(
            repo,
            "--type=bool",
            "--get",
            f"remote.{name}.promisor",
        )
        if not typed_present or normalized != "false":
            return True
    return False


def _effective_filters(repo: Path, paths: list[str]) -> set[str]:
    if not paths:
        return set()
    request = b"\0".join(os.fsencode(path) for path in paths) + b"\0"
    expected = set(paths)
    values: set[str] = set()
    for cached in (False, True):
        arguments = ["check-attr"]
        if cached:
            arguments.append("--cached")
        arguments.extend(("-z", "--stdin", "filter"))
        payload = _git(repo, *arguments, input_data=request)
        if not payload.endswith(b"\0"):
            raise BulkloadError("git check-attr omitted its trailing NUL")
        fields = payload[:-1].split(b"\0")
        if len(fields) != len(paths) * 3:
            raise BulkloadError("git check-attr emitted an incomplete filter catalog")
        seen: set[str] = set()
        for index in range(0, len(fields), 3):
            path = _decode_path(fields[index])
            attribute = fields[index + 1].decode("ascii", "strict")
            value = fields[index + 2].decode("utf-8", "strict")
            if attribute != "filter" or path not in expected or path in seen:
                raise BulkloadError(
                    "git check-attr emitted an unexpected filter record"
                )
            seen.add(path)
            if value not in {"unspecified", "unset"}:
                values.add(value)
        if seen != expected:
            raise BulkloadError("git check-attr omitted a filter path")
    return values


def _has_unportable_attribute_authority(repo: Path) -> bool:
    info_raw = _git(repo, "rev-parse", "--git-path", "info/attributes")
    info_path = Path(info_raw.decode("utf-8", "strict").strip())
    if not info_path.is_absolute():
        info_path = repo / info_path
    try:
        info = info_path.lstat()
    except FileNotFoundError:
        pass
    else:
        if not stat.S_ISREG(info.st_mode) or info.st_size > 0:
            return True

    if _optional_git_text(repo, "config", "--path", "--get", "core.attributesFile"):
        return True

    home = Path.home()
    xdg = Path(os.environ.get("XDG_CONFIG_HOME", home / ".config"))
    for candidate in (xdg / "git" / "attributes", home / ".gitattributes"):
        try:
            info = candidate.lstat()
        except FileNotFoundError:
            continue
        if not stat.S_ISREG(info.st_mode) or info.st_size > 0:
            return True
    return False


def capture_git_runtime(repo: Path) -> dict[str, Any]:
    """Capture the mutable Git identity used by apply-time preconditions."""
    record = capture_repository(
        repo.expanduser().resolve(),
        ".",
        include_ignored=False,
        max_files=2**63 - 1,
        max_bytes=2**127 - 1,
    )
    if not record["complete"]:
        raise BulkloadError(f"runtime capture is incomplete: {record['errors']}")
    return {
        "branch": record["branch"],
        "has_alternates": record["has_alternates"],
        "has_content_filters": record["has_content_filters"],
        "has_grafts": record["has_grafts"],
        "has_gitmodules": record["has_gitmodules"],
        "has_lfs_attributes": record["has_lfs_attributes"],
        "has_unportable_attributes": record["has_unportable_attributes"],
        "head": record["head"],
        "local_refs": [
            item
            for item in record["refs"]
            if not item["name"].startswith("refs/remotes/")
        ],
        "local_refs_sha256": record["local_refs_sha256"],
        "refs_sha256": record["refs_sha256"],
        "status": record["status"],
        "status_sha256": record["status_sha256"],
    }


def _path_collisions(paths: Iterable[str]) -> list[list[str]]:
    collision_groups: dict[str, list[str]] = {}
    for path in paths:
        key = unicodedata.normalize("NFC", path).casefold()
        collision_groups.setdefault(key, []).append(path)
    return [
        sorted(set(group)) for group in collision_groups.values() if len(set(group)) > 1
    ]


def capture_repository(
    repo: Path,
    logical_path: str,
    *,
    include_ignored: bool,
    max_files: int,
    max_bytes: int,
) -> dict[str, Any]:
    if _has_promisor_configuration(repo):
        raise BulkloadError("partial/promisor Git repositories are unsupported in v1")
    errors: list[str] = []
    branch = _optional_git_text(repo, "symbolic-ref", "--quiet", "--short", "HEAD")
    head = _git(repo, "rev-parse", "HEAD").decode("ascii", "strict").strip()
    object_format = (
        _git(repo, "rev-parse", "--show-object-format").decode("ascii").strip()
    )
    if object_format not in {"sha1", "sha256"}:
        raise BulkloadError(f"unsupported Git object format: {object_format}")
    shallow = (
        _git(repo, "rev-parse", "--is-shallow-repository")
        .decode("ascii", "strict")
        .strip()
    )
    if shallow not in {"true", "false"}:
        raise BulkloadError("git rev-parse emitted an invalid shallow-state value")
    if shallow == "true":
        errors.append("shallow Git history is unsupported in v1")
    index_entries = _parse_index(_git(repo, "ls-files", "--stage", "-z"))
    head_entries = _parse_tree(_git(repo, "ls-tree", "-r", "-z", "--full-tree", head))
    tracked_authority = set(index_entries) | set(head_entries)
    tracked = sorted(tracked_authority)
    untracked = _nul_paths(
        _git(repo, "ls-files", "-z", "--others", "--exclude-standard")
    )
    untracked = [path for path in untracked if path not in tracked_authority]
    ignored: list[str] = []
    if include_ignored:
        ignored = _nul_paths(
            _git(repo, "ls-files", "-z", "--others", "--ignored", "--exclude-standard")
        )

    classified: list[tuple[str, str]] = []
    classified.extend((path, "tracked") for path in tracked)
    classified.extend((path, "untracked") for path in untracked)
    classified.extend((path, "ignored") for path in ignored)
    classified = sorted(set(classified), key=lambda item: (item[0], item[1]))
    if len(classified) > max_files:
        errors.append(f"file budget exceeded: {len(classified)} > {max_files}")
        classified = classified[:max_files]

    files: list[dict[str, Any]] = []
    observed_bytes = 0
    for relative, git_class in classified:
        try:
            item = inspect_path(
                repo,
                relative,
                git_class,
                None,
                max_bytes - observed_bytes,
                object_format if git_class == "tracked" else None,
            )
        except (BulkloadError, OSError) as error:
            errors.append(f"{relative}: {type(error).__name__}: {error}")
            continue
        size = item.get("size")
        if isinstance(size, int):
            observed_bytes += size
        if observed_bytes > max_bytes:
            errors.append(f"byte budget exceeded: {observed_bytes} > {max_bytes}")
            break
        files.append(item)

    statuses = _derive_status(index_entries, head_entries, files)
    status_by_path = {record["path"]: record for record in statuses}
    for item in files:
        item["status"] = status_by_path.get(item["path"])

    for item in files:
        if item["git_class"] != "tracked":
            continue
        entries = index_entries.get(item["path"], [])
        item["index_entries"] = entries
        stage_zero = [entry for entry in entries if entry["stage"] == 0]
        if len(stage_zero) != 1:
            continue
        entry = stage_zero[0]
        if entry["mode"] not in {"100644", "100755", "120000"}:
            if item.get("status") is None:
                errors.append(
                    f"{item['path']}: unsupported clean tracked index mode {entry['mode']}"
                )
            continue
        expected_kind = "symlink" if entry["mode"] == "120000" else "file"
        mode_matches = item.get("kind") == expected_kind
        if expected_kind == "file" and isinstance(item.get("mode"), str):
            executable = bool(int(item["mode"], 8) & 0o111)
            mode_matches = mode_matches and executable == (entry["mode"] == "100755")
        content_matches = item.get("git_blob_oid") == entry["object"]
        if item.get("status") is None and (not mode_matches or not content_matches):
            errors.append(
                f"{item['path']}: clean status hides tracked bytes or mode differing from index"
            )

    collisions = _path_collisions(item["path"] for item in files)
    if collisions:
        errors.append(
            f"casefold or Unicode-normalization path collisions: {collisions}"
        )

    common_dir_raw = _git(repo, "rev-parse", "--git-common-dir").decode("utf-8").strip()
    common_dir = (
        (repo / common_dir_raw).resolve()
        if not Path(common_dir_raw).is_absolute()
        else Path(common_dir_raw).resolve()
    )
    git_dir_raw = _git(repo, "rev-parse", "--git-dir").decode("utf-8").strip()
    git_dir = (
        (repo / git_dir_raw).resolve()
        if not Path(git_dir_raw).is_absolute()
        else Path(git_dir_raw).resolve()
    )
    alternates = Path(
        _git(repo, "rev-parse", "--git-path", "objects/info/alternates")
        .decode("utf-8")
        .strip()
    )
    if not alternates.is_absolute():
        alternates = repo / alternates
    grafts = Path(
        _git(repo, "rev-parse", "--git-path", "info/grafts").decode("utf-8").strip()
    )
    if not grafts.is_absolute():
        grafts = repo / grafts

    has_lfs = False
    has_content_filters = False
    has_unportable_attributes = False
    try:
        has_unportable_attributes = _has_unportable_attribute_authority(repo)
        if not has_unportable_attributes:
            effective_filters = _effective_filters(
                repo,
                sorted(
                    {path for path, git_class in classified if git_class != "ignored"}
                ),
            )
            has_lfs = "lfs" in effective_filters
            has_content_filters = bool(effective_filters - {"lfs"})
    except (BulkloadError, OSError) as error:
        errors.append(f"Git attributes: {type(error).__name__}: {error}")

    refs = _capture_refs(repo)
    _verify_local_ref_object_closure(repo, head, refs)
    local_refs = [item for item in refs if not item["name"].startswith("refs/remotes/")]

    return {
        "branch": branch,
        "complete": not errors,
        "errors": errors,
        "files": sorted(files, key=lambda item: (item["path"], item["git_class"])),
        "git_dir": str(git_dir),
        "has_alternates": alternates.exists(),
        "has_content_filters": has_content_filters,
        "has_grafts": grafts.exists() and grafts.stat().st_size > 0,
        "has_gitmodules": (repo / ".gitmodules").exists()
        or any(
            entry["mode"] == "160000"
            for entries in index_entries.values()
            for entry in entries
        )
        or any(entry["mode"] == "160000" for entry in head_entries.values()),
        "has_lfs_attributes": has_lfs,
        "has_unportable_attributes": has_unportable_attributes,
        "head": head,
        "logical_path": logical_path,
        "local_refs_sha256": sha256_bytes(canonical_bytes(local_refs)),
        "observed_bytes": observed_bytes,
        "refs": refs,
        "refs_sha256": sha256_bytes(canonical_bytes(refs)),
        "remotes": _capture_remotes(repo),
        "root": str(repo),
        "status": statuses,
        "status_sha256": sha256_bytes(canonical_bytes(statuses)),
        "upstream": _optional_git_text(
            repo, "rev-parse", "--abbrev-ref", "@{upstream}"
        ),
        "worktree_common_dir": str(common_dir),
        "worktrees": _parse_worktrees(
            _git(repo, "worktree", "list", "--porcelain", "-z")
        ),
    }


def capture_snapshot(
    root: Path,
    mode: str,
    *,
    include_ignored: bool = False,
    max_files: int = DEFAULT_MAX_FILES,
    max_bytes: int = DEFAULT_MAX_BYTES,
) -> dict[str, Any]:
    root = root.expanduser().resolve()
    repositories, discovery_errors = discover_repositories(root, mode)
    catalog: list[dict[str, Any]] = []
    errors: list[str] = list(discovery_errors)
    for repo in repositories:
        relative = repo.relative_to(root).as_posix()
        logical = (
            "." if mode == "repo" or relative == "." else normalize_relative(relative)
        )
        try:
            record = capture_repository(
                repo,
                logical,
                include_ignored=include_ignored,
                max_files=max_files,
                max_bytes=max_bytes,
            )
        except (BulkloadError, OSError) as error:
            errors.append(f"{logical}: {type(error).__name__}: {error}")
            continue
        catalog.append(record)
        errors.extend(f"{logical}: {error}" for error in record["errors"])

    snapshot: dict[str, Any] = {
        "captured_at": utc_now(),
        "capture_id": uuid.uuid4().hex,
        "catalog": sorted(catalog, key=lambda item: item["logical_path"]),
        "catalog_sha256": sha256_bytes(
            canonical_bytes(sorted(catalog, key=lambda item: item["logical_path"]))
        ),
        "complete": not errors and all(item["complete"] for item in catalog),
        "errors": errors,
        "host": socket.gethostname(),
        "include_ignored": include_ignored,
        "mode": mode,
        "root": str(root),
        "schema": SNAPSHOT_SCHEMA,
    }
    snapshot["snapshot_sha256"] = object_digest(snapshot, "snapshot_sha256")
    return snapshot


def find_file(repo_record: dict[str, Any], path: str) -> dict[str, Any] | None:
    for item in repo_record.get("files", []):
        if item.get("path") == path and item.get("git_class") != "ignored":
            return item
    return None


def dirty_paths(repo_record: dict[str, Any]) -> set[str]:
    return {item["path"] for item in repo_record.get("status", [])}


def catalog_map(snapshot: dict[str, Any]) -> dict[str, dict[str, Any]]:
    return {item["logical_path"]: item for item in snapshot.get("catalog", [])}
