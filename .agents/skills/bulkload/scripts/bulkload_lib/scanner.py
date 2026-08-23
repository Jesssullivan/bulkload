"""AgentCaptureV4 and GitWorkspaceV2 read-only capture."""

from __future__ import annotations

from collections import defaultdict
import hashlib
import json
import math
import os
from pathlib import Path, PurePosixPath
import re
import socket
import sqlite3
import stat
import subprocess
import tempfile
from typing import Any, Iterable, Sequence
from urllib.parse import urlsplit, urlunsplit

from .model import (
    AGENT_CAPTURE_SCHEMA,
    GIT_WORKSPACE_SCHEMA,
    BulkloadError,
    canonical_bytes,
    new_id,
    normalize_relative,
    require_digest,
    require_exact_keys,
    resolve_real,
    runtime_source_digest,
    seal,
    sha256_bytes,
    sha256_file,
    sha256_symlink,
    translate_path,
    utc_now,
)


DEFAULT_MAX_FILES = 2_000_000
DEFAULT_MAX_BYTES = 4 * 1024**4
DEFAULT_MAX_SQLITE_ROWS = 5_000_000
ZERO_OIDS = {"0" * 40, "0" * 64}
HEX_OID = re.compile(r"^[0-9a-f]{40}(?:[0-9a-f]{24})?$")
SQLITE_SUFFIXES = (".sqlite", ".sqlite3", ".db")
SQLITE_SIDECARS = ("-wal", "-shm", "-journal")
GIT_OPERATION_MARKERS = {
    "BISECT_LOG": "bisect",
    "CHERRY_PICK_HEAD": "cherry-pick",
    "MERGE_HEAD": "merge",
    "REVERT_HEAD": "revert",
    "rebase-apply": "rebase-or-am",
    "rebase-merge": "rebase",
    "sequencer": "sequencer",
}
PSEUDO_REFS = {
    "AUTO_MERGE",
    "BISECT_HEAD",
    "CHERRY_PICK_HEAD",
    "FETCH_HEAD",
    "MERGE_HEAD",
    "ORIG_HEAD",
    "REBASE_HEAD",
    "REVERT_HEAD",
}
MANAGED_EXCLUSION_NAMESPACES = {
    "codex": {
        "AGENTS.md",
        "config.toml",
        "instructions.md",
        "prompts",
        "rules",
        "skills",
    },
    "claude": {"agents", "commands", "skills"},
    "pi": {
        "AGENTS.md",
        "APPEND_SYSTEM.md",
        "agents",
        "commands",
        "prompts",
        "skills",
        "tinyland",
    },
}
SAFE_EXECUTABLE_PATH = re.compile(r"/(?:[A-Za-z0-9._+-]+/)*[A-Za-z0-9._+-]+")


class _OpaqueGitFallback(BulkloadError):
    """A readable workspace that must travel as exact opaque bytes."""


class _MalformedAppendState(BulkloadError):
    """Stable append state whose records cannot be typed safely."""


def shell_safe_executable(raw_path: str, label: str) -> str:
    if not os.path.isabs(raw_path):
        raise BulkloadError(f"{label} path must be explicit and absolute")
    path = os.path.realpath(os.path.abspath(raw_path))
    if not SAFE_EXECUTABLE_PATH.fullmatch(path):
        raise BulkloadError(f"{label} path is not canonical and shell-safe")
    try:
        info = Path(path).stat(follow_symlinks=False)
    except OSError as error:
        raise BulkloadError(f"{label} executable is unavailable") from error
    if not stat.S_ISREG(info.st_mode) or not os.access(path, os.X_OK):
        raise BulkloadError(f"{label} is not a regular executable")
    return path


def inspect_rsync(raw_path: str) -> dict[str, Any]:
    """Bind one explicit GNU rsync executable and its required features."""
    path = shell_safe_executable(raw_path, "rsync")
    try:
        version = subprocess.run(
            [path, "--version"],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        help_result = subprocess.run(
            [path, "--help"],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
    except OSError as error:
        raise BulkloadError("pinned GNU rsync executable is unavailable") from error
    match = re.search(rb"protocol version (\d+)", version.stdout)
    if (
        version.returncode != 0
        or help_result.returncode != 0
        or match is None
        or int(match.group(1)) < 30
        or b"--from0" not in help_result.stdout
        or b"--files-from" not in help_result.stdout
        or b"--ignore-missing-args" not in help_result.stdout
    ):
        raise BulkloadError("pinned rsync lacks the required GNU transport features")
    return {
        "features": [
            "checksum",
            "delay-updates",
            "files-from",
            "from0",
            "ignore-missing-args",
        ],
        "path": path,
        "protocol": int(match.group(1)),
        "sha256": sha256_file(Path(path)),
    }


def canonical_provider_policy(
    entries: Sequence[tuple[str, str]],
) -> dict[str, Any]:
    exclusions: list[dict[str, str]] = []
    for provider, raw_relative in entries:
        if provider not in MANAGED_EXCLUSION_NAMESPACES:
            raise BulkloadError(f"unknown managed-exclusion provider: {provider!r}")
        relative = normalize_relative(raw_relative)
        parts = PurePosixPath(relative).parts
        if parts[0] not in MANAGED_EXCLUSION_NAMESPACES[provider]:
            raise BulkloadError(
                "managed exclusion is outside the provider's source-managed namespaces"
            )
        exclusions.append({"provider": provider, "relative_path": relative})
    exclusions.sort(key=lambda item: (item["provider"], item["relative_path"]))
    if len({(item["provider"], item["relative_path"]) for item in exclusions}) != len(
        exclusions
    ):
        raise BulkloadError("duplicate managed exclusion")
    for index, first in enumerate(exclusions):
        first_parts = PurePosixPath(first["relative_path"]).parts
        for second in exclusions[index + 1 :]:
            if first["provider"] != second["provider"]:
                continue
            second_parts = PurePosixPath(second["relative_path"]).parts
            if (
                first_parts == second_parts[: len(first_parts)]
                or second_parts == first_parts[: len(second_parts)]
            ):
                raise BulkloadError("managed exclusions overlap ambiguously")
    return {
        "default": "declared-root-portable-private",
        "managed_exclusions": exclusions,
        "portable_symlinks": "relative-within-provider-root",
    }


def _is_excluded(relative: str, exclusions: Sequence[str]) -> bool:
    parts = PurePosixPath(relative).parts
    return any(
        PurePosixPath(exclusion).parts == parts[: len(PurePosixPath(exclusion).parts)]
        for exclusion in exclusions
    )


def _is_regenerate_namespace(provider: str, relative: str) -> bool:
    """Prune exact rebuildable trees before inspecting their symlinks."""
    parts = PurePosixPath(relative).parts
    if parts[:1] == (".tmp",):
        return True
    if provider == "codex":
        return parts[:1] in {("logs",), ("tmp",), ("shell_snapshots",)} or parts[
            :2
        ] == ("plugins", "cache")
    if provider == "claude":
        return (
            parts[:1] in {("cache",), ("debug",), ("logs",), ("telemetry",)}
            or parts[:2] == ("security", "agent-sdk-venv")
            or (
                parts[:1] == ("agent-notes-rescue",)
                and any(part in {".tmp", "tmp"} for part in parts[1:])
            )
        )
    return provider == "pi" and parts[:1] in {("cache",), ("logs",), ("tmp",)}


def canonical_path_map(entries: Iterable[tuple[str, str]]) -> list[dict[str, str]]:
    result: list[dict[str, str]] = []
    seen_sources: set[str] = set()
    for raw_source, raw_destination in entries:
        # Source-map authority is the declared logical install path. Resolving
        # a provider/HM symlink here would erase the operator-reviewed binding.
        source = os.path.abspath(os.fspath(Path(raw_source).expanduser()))
        # The destination is interpreted on another host. Resolving it through
        # source-host symlinks (for example macOS /home) would corrupt the
        # wire contract before Sting ever sees it.
        destination = os.path.abspath(os.fspath(Path(raw_destination).expanduser()))
        if source in seen_sources:
            raise BulkloadError(f"duplicate path-map source: {source}")
        seen_sources.add(source)
        result.append({"source": source, "destination": destination})
    result.sort(key=lambda item: item["source"])
    # Overlap is intentional: the longest exact source prefix wins. This is
    # what allows /Users/jess to map to a destination home while the more
    # specific /Users/jess/git maps to Sting's XFS-backed fast-local root.
    return result


def _git_environment() -> dict[str, str]:
    environment = {
        key: value
        for key, value in os.environ.items()
        if not key.startswith("GIT_") and key not in {"SSH_ASKPASS", "GIT_ASKPASS"}
    }
    environment.update(
        {
            "GIT_CONFIG_GLOBAL": os.devnull,
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_TERMINAL_PROMPT": "0",
            "GIT_NO_REPLACE_OBJECTS": "1",
            "LC_ALL": "C",
        }
    )
    return environment


def _git(
    repository: Path,
    arguments: Sequence[str],
    *,
    input_bytes: bytes | None = None,
    check: bool = True,
) -> bytes:
    try:
        result = subprocess.run(
            ["git", "-C", os.fspath(repository), *arguments],
            check=False,
            input=input_bytes,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=_git_environment(),
        )
    except OSError as error:
        raise BulkloadError("Git is unavailable") from error
    if check and result.returncode != 0:
        # Git errors can echo configured remote values. Do not surface stderr.
        raise BulkloadError(
            f"Git inspection command failed ({arguments[0] if arguments else 'unknown'})"
        )
    return result.stdout


def _decode_path(payload: bytes, label: str) -> str:
    try:
        value = payload.decode("utf-8", errors="strict")
    except UnicodeDecodeError as error:
        raise BulkloadError(f"{label} is not portable UTF-8") from error
    normalize_relative(value)
    return value


def _stable_stat(path: Path) -> tuple[int, ...]:
    info = path.stat(follow_symlinks=False)
    return (
        info.st_dev,
        info.st_ino,
        stat.S_IFMT(info.st_mode),
        stat.S_IMODE(info.st_mode),
        info.st_nlink,
        info.st_size,
        info.st_mtime_ns,
        info.st_ctime_ns,
    )


def _declared_root(
    requested: Path, *, allow_absent: bool
) -> tuple[Path, Path, dict[str, Any] | None, bool]:
    """Keep logical install authority separate from a stable backing root."""
    logical = Path(os.path.abspath(os.fspath(requested.expanduser())))
    try:
        before = _stable_stat(logical)
    except FileNotFoundError:
        if allow_absent:
            return logical, logical, None, False
        raise BulkloadError(f"declared root does not exist: {logical}") from None
    kind = before[2]
    proof = None
    if kind == stat.S_IFLNK:
        if before[4] != 1:
            raise BulkloadError("declared root symlink has multiple directory entries")
        try:
            target = Path(os.readlink(logical))
            immediate = target if target.is_absolute() else logical.parent / target
            immediate_info = immediate.lstat()
        except OSError as error:
            raise BulkloadError(
                "declared root symlink is broken or unreadable"
            ) from error
        if stat.S_ISLNK(immediate_info.st_mode):
            raise BulkloadError("declared root contains a multi-link symlink chain")
        if not stat.S_ISDIR(immediate_info.st_mode):
            raise BulkloadError("declared root symlink does not name a directory")
        backing = resolve_real(immediate)
        proof = {
            "kind": "symlink",
            "mode": f"{before[3]:04o}",
            "sha256": sha256_symlink(logical),
            "size": before[5],
        }
    elif kind == stat.S_IFDIR:
        backing = resolve_real(logical)
    else:
        raise BulkloadError("declared root is neither a directory nor a directory link")
    if _stable_stat(logical) != before:
        raise BulkloadError("declared root changed during capture")
    return logical, backing, proof, True


def _portable_symlink_destination(root: Path, path: Path) -> str | None:
    try:
        target = os.readlink(path)
    except OSError:
        return None
    if not target or "\x00" in target or Path(target).is_absolute():
        return None
    relative_parent = path.relative_to(root).parent.as_posix()
    candidate = os.path.normpath(os.path.join(relative_parent, target))
    if candidate in {"", ".", ".."} or candidate.startswith("../"):
        return None
    return candidate


def _portable_symlink(root: Path, path: Path) -> bool:
    return _portable_symlink_destination(root, path) is not None


def _file_record(path: Path, relative: str, *, classification: str) -> dict[str, Any]:
    normalized = normalize_relative(relative)
    before = _stable_stat(path)
    file_type = before[2]
    mode = f"{before[3]:04o}"
    if file_type == stat.S_IFREG:
        digest = sha256_file(path)
        size = before[5]
        kind = "regular"
    elif file_type == stat.S_IFLNK:
        digest = sha256_symlink(path)
        size = len(os.fsencode(os.readlink(path)))
        kind = "symlink"
    elif file_type == stat.S_IFDIR:
        digest = None
        size = 0
        kind = "directory"
    else:
        raise BulkloadError(f"special filesystem entry is unsupported: {path}")
    if _stable_stat(path) != before:
        raise BulkloadError(f"filesystem entry changed during capture: {path}")
    return {
        "classification": classification,
        "kind": kind,
        "mode": mode,
        "relative_path": normalized,
        "sha256": digest,
        "size": size,
    }


def _walk_entries(
    root: Path,
    *,
    classification: str,
    excluded_roots: Iterable[Path] = (),
    skip_git_admin: bool = False,
    skip_sockets: bool = False,
    max_files: int,
    max_bytes: int,
) -> tuple[list[dict[str, Any]], list[dict[str, str]]]:
    entries: list[dict[str, Any]] = []
    blockers: list[dict[str, str]] = []
    excluded = {os.fspath(path.resolve()) for path in excluded_roots}
    charged_bytes = 0

    def unreadable(error: OSError) -> None:
        blockers.append(
            {
                "code": "unreadable-filesystem-entry",
                "path": os.fspath(error.filename or root),
            }
        )

    for current_text, directories, files in os.walk(
        root, topdown=True, followlinks=False, onerror=unreadable
    ):
        current = Path(current_text)
        retained: list[str] = []
        for directory in sorted(directories):
            child = current / directory
            if skip_git_admin and directory == ".git":
                continue
            try:
                resolved_text = os.fspath(child.resolve())
            except (OSError, RuntimeError):
                blockers.append({"code": "unresolvable-path", "path": os.fspath(child)})
                continue
            if resolved_text in excluded:
                continue
            try:
                relative = child.relative_to(root).as_posix()
                record = _file_record(child, relative, classification=classification)
                if record["kind"] == "symlink":
                    entries.append(record)
                    continue
                entries.append(record)
                retained.append(directory)
            except BulkloadError as error:
                blockers.append(
                    {
                        "code": "unsupported-filesystem-entry",
                        "path": os.fspath(child),
                        "detail": str(error),
                    }
                )
        directories[:] = retained
        for filename in sorted(files):
            child = current / filename
            if skip_git_admin and current == root and filename == ".git":
                continue
            try:
                if skip_sockets:
                    try:
                        if stat.S_ISSOCK(child.stat(follow_symlinks=False).st_mode):
                            continue
                    except OSError:
                        blockers.append(
                            {
                                "code": "unreadable-filesystem-entry",
                                "path": os.fspath(child),
                            }
                        )
                        continue
                relative = child.relative_to(root).as_posix()
                record = _file_record(child, relative, classification=classification)
                entries.append(record)
                charged_bytes += record["size"]
            except BulkloadError as error:
                blockers.append(
                    {
                        "code": "unsupported-filesystem-entry",
                        "path": os.fspath(child),
                        "detail": str(error),
                    }
                )
            if len(entries) > max_files or charged_bytes > max_bytes:
                raise BulkloadError("filesystem capture budget exceeded")
    entries.sort(key=lambda item: (item["relative_path"], item["kind"]))
    return entries, blockers


def _discover_git_roots(root: Path) -> tuple[list[Path], list[dict[str, str]]]:
    repositories: list[Path] = []
    blockers: list[dict[str, str]] = []
    for current_text, directories, _ in os.walk(root, topdown=True, followlinks=False):
        current = Path(current_text)
        git_entry = current / ".git"
        try:
            git_info = git_entry.lstat()
        except FileNotFoundError:
            git_info = None
        except OSError:
            blockers.append(
                {"code": "unreadable-git-authority", "path": os.fspath(git_entry)}
            )
            git_info = None
        if git_info is not None:
            if stat.S_ISLNK(git_info.st_mode) or not (
                stat.S_ISDIR(git_info.st_mode) or stat.S_ISREG(git_info.st_mode)
            ):
                blockers.append(
                    {"code": "unsafe-git-authority", "path": os.fspath(git_entry)}
                )
            else:
                repositories.append(current.resolve())
            directories[:] = [name for name in directories if name != ".git"]
            continue
        if (
            (current / "HEAD").is_file()
            and (current / "objects").is_dir()
            and (current / "refs").is_dir()
        ):
            repositories.append(current.resolve())
            directories[:] = []
            continue
        directories[:] = sorted(name for name in directories if name != ".git")
    return sorted(set(repositories), key=os.fspath), blockers


def _gitfile_declares_authority(repository: Path) -> bool:
    git_entry = repository / ".git"
    try:
        if not stat.S_ISREG(git_entry.stat(follow_symlinks=False).st_mode):
            return False
        with git_entry.open("rb", buffering=0) as stream:
            return stream.read(8).startswith(b"gitdir:")
    except FileNotFoundError:
        return False
    except OSError:
        return True


def _parse_worktree_list(repository: Path) -> list[dict[str, Any]]:
    payload = _git(repository, ["worktree", "list", "--porcelain", "-z"])
    records: list[dict[str, Any]] = []
    current: dict[str, Any] | None = None
    for raw_field in payload.split(b"\0"):
        if not raw_field:
            if current is not None:
                records.append(current)
                current = None
            continue
        try:
            field = raw_field.decode("utf-8")
        except UnicodeDecodeError as error:
            raise BulkloadError(
                "Git worktree metadata is not portable UTF-8"
            ) from error
        if field.startswith("worktree "):
            if current is not None:
                records.append(current)
            current = {"path": field[9:]}
            continue
        if current is None:
            raise BulkloadError("malformed Git worktree inventory")
        if field.startswith("HEAD "):
            current["head"] = field[5:]
        elif field.startswith("branch "):
            current["branch"] = field[7:]
        elif field == "detached":
            current["detached"] = True
        elif field.startswith("locked"):
            current["locked"] = True
        elif field.startswith("prunable"):
            current["prunable"] = True
        elif field == "bare":
            current["bare"] = True
    if current is not None:
        records.append(current)
    return records


def _parse_refs(repository: Path) -> list[dict[str, Any]]:
    payload = _git(
        repository,
        ["for-each-ref", "--format=%(refname)%09%(objectname)%09%(symref)"],
    )
    refs: list[dict[str, Any]] = []
    for raw_line in payload.splitlines():
        fields = raw_line.decode("utf-8", errors="strict").split("\t")
        if len(fields) != 3 or not HEX_OID.fullmatch(fields[1]):
            raise BulkloadError("malformed Git ref inventory")
        refs.append(
            {
                "name": fields[0],
                "oid": fields[1],
                "symbolic_target": fields[2] or None,
            }
        )
    return sorted(refs, key=lambda item: item["name"])


def _read_oid_lines(path: Path) -> set[str]:
    result: set[str] = set()
    try:
        payload = path.read_bytes()
    except OSError:
        return result
    for line in payload.splitlines():
        fields = line.split()
        for value in fields[:2]:
            try:
                decoded = value.decode("ascii")
            except UnicodeDecodeError:
                continue
            if HEX_OID.fullmatch(decoded) and decoded not in ZERO_OIDS:
                result.add(decoded)
    return result


def _recovery_anchors(
    repository: Path, common_dir: Path, git_dirs: Sequence[Path]
) -> list[dict[str, Any]]:
    sources: dict[str, set[str]] = defaultdict(set)
    roots = [common_dir, *git_dirs]
    for authority in roots:
        logs = authority / "logs"
        if logs.is_dir():
            for current, directories, files in os.walk(logs, followlinks=False):
                directories[:] = sorted(directories)
                for filename in sorted(files):
                    path = Path(current) / filename
                    relative = path.relative_to(authority).as_posix()
                    for oid in _read_oid_lines(path):
                        sources[oid].add(relative)
        for name in PSEUDO_REFS:
            path = authority / name
            if path.is_file():
                for oid in _read_oid_lines(path):
                    sources[oid].add(name)
    result: list[dict[str, Any]] = []
    for oid in sorted(sources):
        # The type query distinguishes a missing object without exposing its
        # data in diagnostics or in the capture.
        object_type = _git(repository, ["cat-file", "-t", oid], check=False).strip()
        if not object_type:
            if sources[oid] == {"FETCH_HEAD"}:
                continue
            raise BulkloadError("a Git recovery anchor object is missing")
        result.append({"oid": oid, "sources": sorted(sources[oid])})
    return result


def _object_files(
    common_dir: Path, *, max_files: int, max_bytes: int
) -> list[dict[str, Any]]:
    objects = common_dir / "objects"
    if not objects.is_dir():
        raise BulkloadError("Git object directory is missing")
    alternates = objects / "info" / "alternates"
    try:
        alternate_info = alternates.stat(follow_symlinks=False)
    except FileNotFoundError:
        alternate_info = None
    except OSError as error:
        raise BulkloadError("Git alternates authority is unreadable") from error
    if alternate_info is not None:
        if not stat.S_ISREG(alternate_info.st_mode):
            raise BulkloadError("Git alternates authority is unsafe")
        if alternate_info.st_size:
            raise _OpaqueGitFallback("Git alternates require opaque byte custody")
    records: list[dict[str, Any]] = []
    charged = 0
    for current, directories, files in os.walk(objects, followlinks=False):
        directories[:] = sorted(directories)
        for filename in sorted(files):
            path = Path(current) / filename
            relative = path.relative_to(objects).as_posix()
            info = path.stat(follow_symlinks=False)
            if not stat.S_ISREG(info.st_mode):
                raise BulkloadError("Git object storage contains a special entry")
            # commit-graph and multi-pack-index are regenerated; all object and
            # pack payloads remain additive transport authority.
            if relative in {"info/commit-graph", "pack/multi-pack-index"}:
                continue
            record = _file_record(path, relative, classification="git-object")
            records.append(record)
            charged += record["size"]
            if len(records) > max_files or charged > max_bytes:
                raise BulkloadError("Git object capture budget exceeded")
    return sorted(records, key=lambda item: item["relative_path"])


def _index_entries(worktree: Path) -> list[dict[str, Any]]:
    payload = _git(worktree, ["ls-files", "--stage", "-z"])
    flag_payload = _git(worktree, ["ls-files", "-v", "-z"])
    debug_payload = _git(worktree, ["ls-files", "--debug", "-z"])
    flags: dict[str, str] = {}
    for raw in flag_payload.split(b"\0"):
        if not raw:
            continue
        if len(raw) < 3 or raw[1:2] != b" ":
            raise BulkloadError("malformed Git index flag inventory")
        path = _decode_path(raw[2:], "Git index path")
        flags[path] = chr(raw[0])
    debug_flags: dict[str, int] = {}
    position = 0
    while position < len(debug_payload):
        terminator = debug_payload.find(b"\0", position)
        if terminator < 0:
            raise BulkloadError("malformed Git index debug inventory")
        path = _decode_path(debug_payload[position:terminator], "Git index path")
        position = terminator + 1
        metadata: list[bytes] = []
        for _ in range(5):
            newline = debug_payload.find(b"\n", position)
            if newline < 0:
                raise BulkloadError("malformed Git index debug inventory")
            metadata.append(debug_payload[position:newline])
            position = newline + 1
        try:
            raw_flags = metadata[-1].rsplit(b"flags:", 1)[1].strip()
            debug_flags[path] = int(raw_flags, 16)
        except (IndexError, ValueError) as error:
            raise BulkloadError("malformed Git index debug flags") from error
    entries: list[dict[str, Any]] = []
    for raw in payload.split(b"\0"):
        if not raw:
            continue
        try:
            header, raw_path = raw.split(b"\t", 1)
            mode, oid, stage_text = header.decode("ascii").split(" ")
        except (ValueError, UnicodeDecodeError) as error:
            raise BulkloadError("malformed Git index inventory") from error
        path = _decode_path(raw_path, "Git index path")
        stage = int(stage_text)
        entries.append(
            {
                "assume_unchanged": flags.get(path, "H").islower(),
                "intent_to_add": bool(debug_flags.get(path, 0) & 0x20000000),
                "mode": mode,
                "oid": oid,
                "path": path,
                "skip_worktree": flags.get(path) == "S",
                "stage": stage,
            }
        )
    return sorted(entries, key=lambda item: (item["path"], item["stage"]))


def _head_entries(worktree: Path) -> dict[str, tuple[str, str]]:
    payload = _git(
        worktree, ["ls-tree", "-r", "-z", "--full-tree", "HEAD"], check=False
    )
    result: dict[str, tuple[str, str]] = {}
    for raw in payload.split(b"\0"):
        if not raw:
            continue
        try:
            header, path_bytes = raw.split(b"\t", 1)
            mode, _kind, oid = header.decode("ascii").split(" ")
        except (ValueError, UnicodeDecodeError) as error:
            raise BulkloadError("malformed Git HEAD tree inventory") from error
        result[_decode_path(path_bytes, "Git tree path")] = (mode, oid)
    return result


def _git_blob_sha(repository: Path, oid: str, cache: dict[str, str]) -> str | None:
    if oid in ZERO_OIDS:
        return None
    if oid in cache:
        return cache[oid]
    process = subprocess.Popen(
        ["git", "-C", os.fspath(repository), "cat-file", "blob", oid],
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        env=_git_environment(),
    )
    digest = hashlib.sha256()
    assert process.stdout is not None
    with process.stdout:
        while chunk := process.stdout.read(1024 * 1024):
            digest.update(chunk)
    if process.wait() != 0:
        raise BulkloadError("cannot inspect indexed Git blob")
    cache[oid] = digest.hexdigest()
    return cache[oid]


def _capture_worktree(
    worktree_record: dict[str, Any],
    *,
    path_map: list[dict[str, str]],
    role: str,
    nested_roots: set[Path],
    max_files: int,
    max_bytes: int,
) -> tuple[dict[str, Any], list[dict[str, str]]]:
    path = resolve_real(Path(worktree_record["path"]))
    git_dir = resolve_real(
        Path(
            _git(path, ["rev-parse", "--path-format=absolute", "--git-dir"])
            .decode()
            .strip()
        )
    )
    destination_path = (
        os.fspath(path) if role == "destination" else translate_path(path, path_map)
    )
    index_path_text = (
        _git(path, ["rev-parse", "--path-format=absolute", "--git-path", "index"])
        .decode()
        .strip()
    )
    index_path = Path(index_path_text)
    if index_path.exists():
        index_record: dict[str, Any] = {
            "exists": True,
            "mode": f"{stat.S_IMODE(index_path.stat().st_mode):04o}",
            "path": os.fspath(index_path.resolve()),
            "sha256": sha256_file(index_path),
            "size": index_path.stat().st_size,
        }
    else:
        index_record = {
            "exists": False,
            "mode": None,
            "path": index_path_text,
            "sha256": None,
            "size": 0,
        }
    entries, blockers = _walk_entries(
        path,
        classification="git-worktree",
        excluded_roots={root for root in nested_roots if root != path},
        skip_git_admin=True,
        max_files=max_files,
        max_bytes=max_bytes,
    )
    index_entries = _index_entries(path)
    if any(entry["mode"] == "040000" for entry in index_entries):
        blockers.append(
            {
                "code": "unsupported-sparse-index",
                "path": os.fspath(path),
            }
        )
    head_entries = _head_entries(path)
    by_path: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for entry in index_entries:
        by_path[entry["path"]].append(entry)
    blob_cache: dict[str, str] = {}
    file_map = {
        entry["relative_path"]: entry
        for entry in entries
        if entry["kind"] != "directory"
    }
    dirt: list[dict[str, Any]] = []
    for relative in sorted(set(head_entries) | set(by_path) | set(file_map)):
        index_for_path = by_path.get(relative, [])
        stage_zero = next(
            (entry for entry in index_for_path if entry["stage"] == 0), None
        )
        head_entry = head_entries.get(relative)
        file_entry = file_map.get(relative)
        conflicted = any(entry["stage"] != 0 for entry in index_for_path)
        staged = False
        if stage_zero is None:
            staged = head_entry is not None or conflicted
        elif head_entry != (stage_zero["mode"], stage_zero["oid"]):
            staged = True
        tracked = bool(index_for_path)
        missing = (
            tracked
            and file_entry is None
            and not (
                stage_zero is not None
                and (stage_zero["skip_worktree"] or stage_zero["mode"] == "160000")
            )
        )
        modified = False
        if stage_zero is not None and file_entry is not None:
            expected_kind = "symlink" if stage_zero["mode"] == "120000" else "regular"
            expected_mode = "0755" if stage_zero["mode"] == "100755" else "0644"
            expected_sha = _git_blob_sha(path, stage_zero["oid"], blob_cache)
            modified = (
                file_entry["kind"] != expected_kind
                or file_entry["sha256"] != expected_sha
                or (expected_kind == "regular" and file_entry["mode"] != expected_mode)
            )
        untracked = not tracked and file_entry is not None
        status = {
            "conflicted": conflicted,
            "missing": missing,
            "modified": modified,
            "path": relative,
            "staged": staged,
            "untracked": untracked,
        }
        if any(value for key, value in status.items() if key != "path"):
            dirt.append(status)
        if file_entry is not None:
            file_entry["status"] = status
    operation_state: list[str] = []
    for marker, operation in GIT_OPERATION_MARKERS.items():
        candidate = git_dir / marker
        if candidate.exists() or candidate.is_symlink():
            operation_state.append(operation)
    if operation_state:
        blockers.append(
            {
                "code": "active-git-operation",
                "path": os.fspath(path),
                "detail": ",".join(sorted(operation_state)),
            }
        )
    return (
        {
            "branch": worktree_record.get("branch"),
            "destination_path": destination_path,
            "detached": bool(worktree_record.get("detached")),
            "dirt": dirt,
            "files": entries,
            "git_dir": os.fspath(git_dir),
            "head": worktree_record.get("head"),
            "index": {**index_record, "entries": index_entries},
            "locked": bool(worktree_record.get("locked")),
            "operation_state": sorted(operation_state),
            "path": os.fspath(path),
            "prunable": bool(worktree_record.get("prunable")),
        },
        blockers,
    )


def _sanitize_remote(raw: str) -> str:
    if not raw or any(character in raw for character in "\r\n\x00"):
        raise BulkloadError("unsupported Git remote locator")
    if re.match(r"^[A-Za-z][A-Za-z0-9+.-]*::", raw):
        raise BulkloadError("remote-helper URLs are unportable")
    if "://" in raw:
        parsed = urlsplit(raw)
        if parsed.scheme.lower() not in {"file", "git", "http", "https", "ssh"}:
            raise BulkloadError("unsupported Git remote scheme")
        if parsed.scheme.lower() == "file":
            return f"local-path:sha256:{sha256_bytes(raw.encode())}"
        host = parsed.hostname
        if not host:
            raise BulkloadError("malformed Git remote URL")
        if ":" in host:
            host = f"[{host}]"
        authority = host + (f":{parsed.port}" if parsed.port else "")
        return urlunsplit((parsed.scheme.lower(), authority, parsed.path, "", ""))
    locator = raw.split("?", 1)[0].split("#", 1)[0]
    if ":" in locator and "/" not in locator.split(":", 1)[0]:
        host, path = locator.rsplit("@", 1)[-1].split(":", 1)
        if host and path:
            return f"{host}:{path}"
    return f"local-path:sha256:{sha256_bytes(raw.encode())}"


def _capture_remotes(repository: Path) -> list[dict[str, str]]:
    result: list[dict[str, str]] = []
    for raw_name in _git(repository, ["remote"]).splitlines():
        name = raw_name.decode("utf-8", errors="strict")
        raw_url = (
            _git(repository, ["remote", "get-url", name])
            .decode("utf-8", errors="strict")
            .strip()
        )
        result.append({"name": name, "url": _sanitize_remote(raw_url)})
    return sorted(result, key=lambda item: item["name"])


def _capture_workspace(
    representative: Path,
    *,
    git_root: Path,
    path_map: list[dict[str, str]],
    role: str,
    nested_roots: set[Path],
    max_files: int,
    max_bytes: int,
) -> tuple[dict[str, Any], list[dict[str, str]]]:
    common_dir = resolve_real(
        Path(
            _git(
                representative,
                ["rev-parse", "--path-format=absolute", "--git-common-dir"],
            )
            .decode()
            .strip()
        )
    )
    worktree_inventory = _parse_worktree_list(representative)
    worktrees: list[dict[str, Any]] = []
    blockers: list[dict[str, str]] = []
    git_dirs: list[Path] = []
    for record in worktree_inventory:
        if record.get("bare"):
            continue
        try:
            captured, observed_blockers = _capture_worktree(
                record,
                path_map=path_map,
                role=role,
                nested_roots=nested_roots,
                max_files=max_files,
                max_bytes=max_bytes,
            )
            worktrees.append(captured)
            git_dirs.append(Path(captured["git_dir"]))
            blockers.extend(observed_blockers)
        except BulkloadError as error:
            blockers.append(
                {
                    "code": "worktree-capture-failed",
                    "path": record.get("path", "redacted"),
                    "detail": str(error),
                }
            )
    object_format = (
        _git(representative, ["rev-parse", "--show-object-format"]).decode().strip()
    )
    refs = _parse_refs(representative)
    recovery = _recovery_anchors(representative, common_dir, git_dirs)
    try:
        object_files = _object_files(
            common_dir, max_files=max_files, max_bytes=max_bytes
        )
    except _OpaqueGitFallback:
        if blockers:
            raise BulkloadError(
                "Git workspace has blockers in addition to opaque-only state"
            ) from None
        raise
    try:
        _git(representative, ["fsck", "--full", "--no-dangling"])
    except BulkloadError as error:
        if blockers or str(error) != "Git inspection command failed (fsck)":
            raise
        raise _OpaqueGitFallback("Git fsck requires opaque byte custody") from error
    try:
        logical = representative.relative_to(git_root).as_posix()
    except ValueError:
        logical = f"external/{sha256_bytes(os.fsencode(representative))[:24]}"
    destination_path = (
        os.fspath(representative)
        if role == "destination"
        else translate_path(representative, path_map)
    )
    primary_worktree = next(
        (item for item in worktrees if item["path"] == os.fspath(representative)),
        None,
    )
    workspace = {
        "branch": primary_worktree["branch"] if primary_worktree else None,
        "common_git_dir": os.fspath(common_dir),
        "destination_path": destination_path,
        "head": primary_worktree["head"] if primary_worktree else None,
        "logical_path": logical,
        "object_files": object_files,
        "object_format": object_format,
        "path": os.fspath(representative),
        "recovery_anchors": recovery,
        "refs": refs,
        "remotes": _capture_remotes(representative),
        "schema": GIT_WORKSPACE_SCHEMA,
        "worktrees": sorted(worktrees, key=lambda item: item["path"]),
    }
    workspace["workspace_id"] = sha256_bytes(
        canonical_bytes(
            {
                "destination_path": destination_path,
                "object_format": object_format,
                "remote_names": [remote["name"] for remote in workspace["remotes"]],
            }
        )
    )
    return workspace, blockers


def _typed_sql_value(value: Any) -> list[Any]:
    if value is None:
        return ["null"]
    if isinstance(value, bool):
        return ["integer", int(value)]
    if isinstance(value, int):
        return ["integer", value]
    if isinstance(value, float):
        if not math.isfinite(value):
            raise BulkloadError("SQLite contains a non-finite REAL value")
        return ["real", value.hex()]
    if isinstance(value, str):
        return ["text", value]
    if isinstance(value, bytes):
        return ["blob-sha256", sha256_bytes(value), len(value)]
    raise BulkloadError("SQLite returned an unsupported value type")


def _quote_identifier(value: str) -> str:
    return '"' + value.replace('"', '""') + '"'


def _sqlite_catalog_from_snapshot(path: Path, *, max_rows: int) -> dict[str, Any]:
    try:
        connection = sqlite3.connect(f"file:{path.as_posix()}?mode=ro", uri=True)
    except sqlite3.Error as error:
        raise BulkloadError("cannot open a consistent SQLite snapshot") from error
    try:
        check = connection.execute("PRAGMA quick_check").fetchone()
        if check != ("ok",):
            raise BulkloadError("SQLite quick_check failed")
        schema_rows = connection.execute(
            "SELECT type,name,tbl_name,sql FROM sqlite_schema "
            "WHERE name NOT LIKE 'sqlite_autoindex_%' ORDER BY type,name"
        ).fetchall()
        schema: list[dict[str, Any]] = []
        unsupported: list[str] = []
        for row_type, name, table_name, sql in schema_rows:
            schema.append(
                {
                    "name": name,
                    "sql_sha256": sha256_bytes((sql or "").encode("utf-8")),
                    "table": table_name,
                    "type": row_type,
                }
            )
            if row_type in {"trigger", "view"} or (
                row_type == "table" and sql and "CREATE VIRTUAL TABLE" in sql.upper()
            ):
                unsupported.append(f"{row_type}:{name}")
        table_names = [
            row[0]
            for row in connection.execute(
                "SELECT name FROM sqlite_schema WHERE type='table' "
                "AND name NOT LIKE 'sqlite_%' ORDER BY name"
            )
        ]
        tables: list[dict[str, Any]] = []
        charged_rows = 0
        for table_name in table_names:
            columns_raw = connection.execute(
                f"PRAGMA table_xinfo({_quote_identifier(table_name)})"
            ).fetchall()
            columns = [row[1] for row in columns_raw]
            if any(row[6] != 0 for row in columns_raw):
                unsupported.append(f"generated-column:{table_name}")
            pk_columns = [
                row[1]
                for row in sorted(columns_raw, key=lambda item: item[5])
                if row[5]
            ]
            rows: list[dict[str, str]] = []
            query = f"SELECT * FROM {_quote_identifier(table_name)}"
            for values in connection.execute(query):
                charged_rows += 1
                if charged_rows > max_rows:
                    raise BulkloadError("SQLite row capture budget exceeded")
                typed = [_typed_sql_value(value) for value in values]
                row_sha = sha256_bytes(canonical_bytes(typed))
                if pk_columns:
                    positions = [columns.index(column) for column in pk_columns]
                    key = [typed[position] for position in positions]
                    key_sha = sha256_bytes(canonical_bytes(key))
                else:
                    key_sha = row_sha
                rows.append({"key_sha256": key_sha, "row_sha256": row_sha})
            rows.sort(key=lambda item: (item["key_sha256"], item["row_sha256"]))
            tables.append(
                {
                    "columns": columns,
                    "name": table_name,
                    "primary_key": pk_columns,
                    "row_count": len(rows),
                    "rows": rows,
                    "rows_sha256": sha256_bytes(canonical_bytes(rows)),
                }
            )
        foreign_keys: list[dict[str, Any]] = []
        for table_name in table_names:
            for row in connection.execute(
                f"PRAGMA foreign_key_list({_quote_identifier(table_name)})"
            ):
                foreign_keys.append(
                    {
                        "from": row[3],
                        "on_delete": row[6],
                        "on_update": row[5],
                        "table": table_name,
                        "to": row[4],
                        "to_table": row[2],
                    }
                )
        catalog = {
            "application_id": connection.execute("PRAGMA application_id").fetchone()[0],
            "foreign_keys": sorted(
                foreign_keys,
                key=lambda item: (
                    item["table"],
                    item["from"],
                    item["to_table"],
                    item["to"] or "",
                ),
            ),
            "page_size": connection.execute("PRAGMA page_size").fetchone()[0],
            "schema": schema,
            "schema_sha256": sha256_bytes(canonical_bytes(schema)),
            "tables": tables,
            "unsupported_schema": sorted(unsupported),
            "user_version": connection.execute("PRAGMA user_version").fetchone()[0],
        }
        catalog["logical_sha256"] = sha256_bytes(canonical_bytes(catalog))
        return catalog
    except sqlite3.Error as error:
        raise BulkloadError("cannot inspect SQLite state") from error
    finally:
        connection.close()


def snapshot_sqlite(
    source: Path, destination: Path, *, max_rows: int
) -> dict[str, Any]:
    """Read one consistent image, including committed WAL state, through backup."""
    source = resolve_real(source)
    destination.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    try:
        source_connection = sqlite3.connect(
            f"file:{source.as_posix()}?mode=ro", uri=True, timeout=5.0
        )
        destination_connection = sqlite3.connect(destination)
        try:
            source_connection.execute("PRAGMA query_only=ON")
            source_connection.backup(destination_connection, pages=1024, sleep=0.01)
            destination_connection.execute("PRAGMA wal_checkpoint(TRUNCATE)")
            destination_connection.commit()
        finally:
            destination_connection.close()
            source_connection.close()
        os.chmod(destination, 0o600)
        return _sqlite_catalog_from_snapshot(destination, max_rows=max_rows)
    except sqlite3.Error as error:
        destination.unlink(missing_ok=True)
        raise BulkloadError("SQLite backup capture failed") from error


def sqlite_catalog(
    source: Path, *, max_rows: int = DEFAULT_MAX_SQLITE_ROWS
) -> dict[str, Any]:
    with tempfile.TemporaryDirectory(prefix="bulkload-sqlite-") as temporary:
        return snapshot_sqlite(
            source, Path(temporary) / "snapshot.sqlite", max_rows=max_rows
        )


def _jsonl_records(
    path: Path, *, replacements: Sequence[tuple[bytes, bytes]] = ()
) -> dict[str, Any]:
    hashes: list[str] = []
    transformed_hashes: list[str] = []
    hasher = hashlib.sha256()
    transformed_hasher = hashlib.sha256()
    try:
        with path.open("rb", buffering=0) as stream:
            for line in stream:
                if not line.endswith(b"\n"):
                    raise _MalformedAppendState(
                        "append-only JSONL has an incomplete final record"
                    )
                try:
                    json.loads(line)
                except (UnicodeDecodeError, json.JSONDecodeError) as error:
                    raise _MalformedAppendState(
                        "append-only state contains invalid JSONL"
                    ) from error
                transformed = line
                for source, destination in replacements:
                    transformed = transformed.replace(source, destination)
                try:
                    json.loads(transformed)
                except (UnicodeDecodeError, json.JSONDecodeError) as error:
                    raise BulkloadError(
                        "path rewriting produced invalid JSONL"
                    ) from error
                hashes.append(sha256_bytes(line))
                transformed_hashes.append(sha256_bytes(transformed))
                hasher.update(line)
                transformed_hasher.update(transformed)
    except OSError as error:
        raise BulkloadError(f"cannot read append-only state {path}") from error
    return {
        "records": hashes,
        "records_sha256": sha256_bytes(canonical_bytes(hashes)),
        "sha256": hasher.hexdigest(),
        "translated_records": transformed_hashes,
        "translated_sha256": transformed_hasher.hexdigest(),
    }


def _session_identity(relative: str, path: Path) -> str:
    stem = path.stem.lower()
    match = re.search(
        r"[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}",
        stem,
    )
    return match.group(0) if match else relative


def _sqlite_primary(path: str) -> str | None:
    for sidecar in SQLITE_SIDECARS:
        if path.endswith(sidecar):
            return path[: -len(sidecar)]
    if path.lower().endswith(SQLITE_SUFFIXES):
        return path
    return None


def _provider_classification(provider: str, relative: str) -> str:
    lowered = relative.lower()
    parts = PurePosixPath(lowered).parts
    leaf = parts[-1]
    if provider == "claude" and (
        parts[:1] == ("agent-notes-rescue",)
        or (
            parts[:1] == ("projects",)
            and any(part in {"tool-results", "reports"} for part in parts[1:])
        )
    ):
        return "portable-private"
    if _sqlite_primary(lowered) is not None:
        return "sqlite"
    if provider == "codex":
        if relative == "auth.json":
            return "portable-auth"
        if lowered == "history.jsonl" or lowered.startswith(
            ("sessions/", "archived_sessions/")
        ):
            return "append-jsonl"
        if lowered.startswith(("goals/", "memory/", "queue/")):
            return "union-state"
        if lowered.startswith(("rules/", "skills/", "prompts/")) or leaf in {
            "config.toml",
            "instructions.md",
        }:
            return "portable-state"
        if lowered.startswith(("logs/", "tmp/", "shell_snapshots/")) or leaf in {
            "models_cache.json",
            "version.json",
        }:
            return "regenerate"
    elif provider == "claude":
        if leaf in {".credentials.json", "auth.json", "credentials.json"}:
            return "nonportable-auth"
        if lowered == "history.jsonl" or (
            lowered.startswith("projects/") and lowered.endswith(".jsonl")
        ):
            return "append-jsonl-rewrite"
        if lowered.startswith(
            (
                "projects/",
                "todos/",
                "plans/",
                "memory/",
                "queue/",
                "commands/",
                "agents/",
                "skills/",
            )
        ) or leaf.startswith("settings"):
            return "portable-state-rewrite"
        if (
            lowered.startswith(("cache/", "debug/", "logs/", "telemetry/"))
            or leaf == "stats-cache.json"
        ):
            return "regenerate"
    elif provider == "pi":
        if leaf in {"auth.json", "credentials.json"}:
            return "portable-auth"
        if lowered == "history.jsonl" or lowered.startswith(
            ("sessions/", "history/", "archive/")
        ):
            return "append-jsonl"
        if lowered.startswith(
            ("state/", "memory/", "queue/", "skills/", "prompts/")
        ) or leaf in {
            "settings.json",
            "models.json",
        }:
            return "portable-state"
        if lowered.startswith(("cache/", "logs/", "tmp/")):
            return "regenerate"
    if provider in {"codex", "claude", "pi"}:
        return "portable-private"
    return "unknown"


def provider_item_identity(item: dict[str, Any]) -> str:
    """Return the stable identity, using the common path identity implicitly."""
    return item.get("identity", item["relative_path"])


def provider_item_destination(item: dict[str, Any]) -> str:
    """Return the destination-relative path without serializing common copies."""
    return item.get("destination_relative_path", item["relative_path"])


def expand_provider_item(item: dict[str, Any]) -> dict[str, Any]:
    """Materialize compact provider defaults only at the execution boundary."""
    return {
        **item,
        "destination_relative_path": provider_item_destination(item),
        "identity": provider_item_identity(item),
    }


def _capture_provider(
    provider: str,
    root: Path,
    *,
    role: str,
    path_map: list[dict[str, str]],
    exclusions: Sequence[str],
    max_files: int,
    max_bytes: int,
    max_sqlite_rows: int,
) -> tuple[dict[str, Any], list[dict[str, str]]]:
    logical_root, backing_root, root_link, exists = _declared_root(
        root, allow_absent=True
    )
    if not exists:
        return {
            "destination_path": os.fspath(logical_root)
            if role == "destination"
            else translate_path(logical_root, path_map),
            "exists": False,
            "items": [],
            "logical_path": os.fspath(logical_root),
            "name": provider,
            "path": os.fspath(backing_root),
            "root_link": root_link,
        }, []
    root = backing_root
    destination_root = (
        os.fspath(root)
        if role == "destination"
        else translate_path(logical_root, path_map)
    )
    files: list[tuple[str, Path]] = []
    blockers: list[dict[str, str]] = []
    charged = 0

    def unreadable(error: OSError) -> None:
        blockers.append(
            {
                "code": "unreadable-agent-state",
                "path": os.fspath(error.filename or root),
            }
        )

    for current, directories, names in os.walk(
        root, topdown=True, followlinks=False, onerror=unreadable
    ):
        retained_directories: list[str] = []
        for name in sorted(directories):
            path = Path(current) / name
            relative = path.relative_to(root).as_posix()
            if _is_excluded(relative, exclusions) or _is_regenerate_namespace(
                provider, relative
            ):
                continue
            try:
                info = path.stat(follow_symlinks=False)
            except OSError:
                blockers.append(
                    {"code": "unreadable-agent-state", "path": os.fspath(path)}
                )
                continue
            if stat.S_ISLNK(info.st_mode) and _portable_symlink(root, path):
                files.append((relative, path))
                charged += info.st_size
                continue
            if not stat.S_ISDIR(info.st_mode):
                blockers.append(
                    {"code": "special-agent-state", "path": f"{provider}:{relative}"}
                )
                continue
            files.append((relative, path))
            retained_directories.append(name)
            if len(files) > max_files:
                raise BulkloadError(f"{provider} state capture budget exceeded")
        directories[:] = retained_directories
        for name in sorted(names):
            path = Path(current) / name
            relative = path.relative_to(root).as_posix()
            if _is_excluded(relative, exclusions) or _is_regenerate_namespace(
                provider, relative
            ):
                continue
            try:
                info = path.stat(follow_symlinks=False)
            except OSError:
                blockers.append(
                    {"code": "unreadable-agent-state", "path": os.fspath(path)}
                )
                continue
            if stat.S_ISLNK(info.st_mode) and _portable_symlink(root, path):
                files.append((relative, path))
                charged += info.st_size
                continue
            if not stat.S_ISREG(info.st_mode):
                blockers.append(
                    {"code": "special-agent-state", "path": f"{provider}:{relative}"}
                )
                continue
            files.append((relative, path))
            charged += info.st_size
            if len(files) > max_files or charged > max_bytes:
                raise BulkloadError(f"{provider} state capture budget exceeded")
    sqlite_paths: dict[str, Path] = {}
    for relative, path in files:
        primary = (
            _sqlite_primary(relative.lower())
            if _provider_classification(provider, relative) == "sqlite"
            else None
        )
        if primary is not None and not relative.lower().endswith(SQLITE_SIDECARS):
            sqlite_paths[primary] = path
    items: list[dict[str, Any]] = []
    replacements = [
        (os.fsencode(entry["source"]), os.fsencode(entry["destination"]))
        for entry in sorted(
            path_map, key=lambda item: len(item["source"]), reverse=True
        )
    ]
    for relative, path in files:
        classification = _provider_classification(provider, relative)
        file_type = stat.S_IFMT(path.stat(follow_symlinks=False).st_mode)
        is_symlink = file_type == stat.S_IFLNK
        if file_type == stat.S_IFDIR:
            classification = (
                "portable-directory-rewrite"
                if classification.endswith("rewrite")
                else "portable-directory"
            )
        if is_symlink:
            link_destination = _portable_symlink_destination(root, path)
            archived_session_link = (
                provider == "codex"
                and classification == "append-jsonl"
                and PurePosixPath(relative).parts[:1] == ("sessions",)
                and link_destination is not None
                and PurePosixPath(link_destination).parts[:1] == ("archived_sessions",)
            )
            if (
                classification
                in {
                    "append-jsonl",
                    "append-jsonl-rewrite",
                    "nonportable-auth",
                    "portable-auth",
                    "sqlite",
                }
                and not archived_session_link
            ):
                blockers.append(
                    {"code": "typed-agent-symlink", "path": f"{provider}:{relative}"}
                )
                continue
            classification = "portable-symlink"
        primary = _sqlite_primary(relative.lower())
        if classification == "sqlite":
            if relative.lower().endswith(SQLITE_SIDECARS):
                continue
            try:
                logical = sqlite_catalog(path, max_rows=max_sqlite_rows)
            except BulkloadError as error:
                blockers.append(
                    {
                        "code": "sqlite-capture-failed",
                        "path": f"{provider}:{relative}",
                        "detail": str(error),
                    }
                )
                continue
            sidecars = []
            for suffix in SQLITE_SIDECARS:
                sidecar = Path(os.fspath(path) + suffix)
                if sidecar.exists():
                    sidecars.append(
                        {
                            "kind": suffix[1:],
                            "sha256": sha256_file(sidecar),
                            "size": sidecar.stat().st_size,
                        }
                    )
            if logical["unsupported_schema"]:
                blockers.append(
                    {
                        "code": "unsupported-sqlite-schema",
                        "path": f"{provider}:{relative}",
                    }
                )
            items.append(
                {
                    "classification": "sqlite",
                    "logical": logical,
                    "mode": f"{stat.S_IMODE(path.stat().st_mode):04o}",
                    "relative_path": relative,
                    "sidecars": sidecars,
                    "size": path.stat().st_size
                    + sum(item["size"] for item in sidecars),
                }
            )
            continue
        if classification == "sqlite" and primary is not None:
            # An orphan sidecar is unknown rather than silently omitted.
            if primary not in sqlite_paths:
                blockers.append(
                    {"code": "orphan-sqlite-sidecar", "path": f"{provider}:{relative}"}
                )
            continue
        if classification == "unknown":
            blockers.append(
                {"code": "unknown-agent-state", "path": f"{provider}:{relative}"}
            )
        if classification in {"portable-auth", "nonportable-auth"} and (
            stat.S_IMODE(path.stat().st_mode) & 0o077
        ):
            blockers.append(
                {"code": "insecure-auth-mode", "path": f"{provider}:{relative}"}
            )
        record = _file_record(path, relative, classification=classification)
        identity = (
            _session_identity(relative, path)
            if classification.startswith("append-jsonl")
            else relative
        )
        destination_relative = relative
        if classification.endswith("rewrite"):
            destination_relative = relative
            for source, destination in replacements:
                encoded_source = source.replace(b"/", b"-")
                encoded_destination = destination.replace(b"/", b"-")
                encoded_relative = os.fsencode(destination_relative)
                encoded_relative = encoded_relative.replace(
                    encoded_source, encoded_destination
                ).replace(encoded_source.lstrip(b"-"), encoded_destination.lstrip(b"-"))
                destination_relative = os.fsdecode(encoded_relative)
        if destination_relative != relative:
            record["destination_relative_path"] = destination_relative
        if identity != relative:
            record["identity"] = identity
        if classification.startswith("append-jsonl"):
            try:
                append_records = _jsonl_records(
                    path,
                    replacements=replacements
                    if classification.endswith("rewrite")
                    else (),
                )
            except _MalformedAppendState:
                record = _file_record(path, relative, classification="portable-private")
                record.pop("destination_relative_path", None)
                record.pop("identity", None)
            else:
                record.update(append_records)
        elif classification.endswith("rewrite") and record["kind"] == "regular":
            payload = path.read_bytes()
            transformed = payload
            for source, destination in replacements:
                transformed = transformed.replace(source, destination)
            try:
                if path.suffix.lower() == ".json":
                    json.loads(transformed)
                else:
                    transformed.decode("utf-8", errors="strict")
            except (UnicodeDecodeError, json.JSONDecodeError):
                blockers.append(
                    {
                        "code": "unportable-path-rewrite-state",
                        "path": f"{provider}:{relative}",
                    }
                )
            record["translated_sha256"] = sha256_bytes(transformed)
            record["translated_size"] = len(transformed)
        items.append(record)
    return (
        {
            "destination_path": destination_root,
            "exists": True,
            "items": sorted(
                items,
                key=lambda item: (
                    item["classification"],
                    provider_item_identity(item),
                    item["relative_path"],
                ),
            ),
            "logical_path": os.fspath(logical_root),
            "name": provider,
            "path": os.fspath(root),
            "root_link": root_link,
        },
        blockers,
    )


def _capture_seat(
    name: str,
    root: Path,
    *,
    root_kind: str,
    role: str,
    path_map: list[dict[str, str]],
    home: Path,
    max_files: int,
    max_bytes: int,
) -> tuple[dict[str, Any], list[dict[str, str]]]:
    if not re.fullmatch(r"[a-z][a-z0-9_-]{0,63}", name):
        raise BulkloadError(f"invalid mutable-seat name: {name!r}")
    if root_kind == "file":
        logical = Path(os.path.abspath(os.fspath(root.expanduser())))
        destination = (
            os.fspath(logical)
            if role == "destination"
            else translate_path(logical, path_map)
        )
        try:
            before = _stable_stat(logical)
        except FileNotFoundError:
            return {
                "destination_path": destination,
                "exists": False,
                "items": [],
                "logical_path": os.fspath(logical),
                "name": name,
                "path": os.fspath(logical.parent),
                "root_kind": "file",
                "root_link": None,
            }, []
        if before[2] != stat.S_IFREG:
            raise BulkloadError("file seat is not an exact regular file")
        backing = resolve_real(logical)
        record = _file_record(backing, backing.name, classification="mutable-seat")
        record["destination_relative_path"] = record["relative_path"]
        record["identity"] = record["relative_path"]
        if _stable_stat(logical) != before:
            raise BulkloadError("file seat changed during capture")
        return {
            "destination_path": destination,
            "exists": True,
            "items": [record],
            "logical_path": os.fspath(logical),
            "name": name,
            "path": os.fspath(backing.parent),
            "root_kind": "file",
            "root_link": None,
        }, []
    if root_kind != "directory":
        raise BulkloadError("mutable-seat kind must be directory or file")
    logical_root, backing_root, root_link, exists = _declared_root(
        root, allow_absent=True
    )
    if logical_root == home or backing_root == home:
        raise BulkloadError("mutable-seat directory may not be the declared home root")
    if not exists:
        return {
            "destination_path": os.fspath(logical_root)
            if role == "destination"
            else translate_path(logical_root, path_map),
            "exists": False,
            "items": [],
            "logical_path": os.fspath(logical_root),
            "name": name,
            "path": os.fspath(backing_root),
            "root_kind": "directory",
            "root_link": root_link,
        }, []
    root = backing_root
    destination = (
        os.fspath(root)
        if role == "destination"
        else translate_path(logical_root, path_map)
    )
    entries, blockers = _walk_entries(
        root,
        classification="mutable-seat",
        skip_sockets=True,
        max_files=max_files,
        max_bytes=max_bytes,
    )
    for entry in entries:
        entry["destination_relative_path"] = entry["relative_path"]
        entry["identity"] = entry["relative_path"]
    return {
        "destination_path": destination,
        "exists": True,
        "items": entries,
        "logical_path": os.fspath(logical_root),
        "name": name,
        "path": os.fspath(root),
        "root_kind": "directory",
        "root_link": root_link,
    }, blockers


def capture_agent_state(
    *,
    role: str,
    home: Path,
    git_root: Path,
    codex_root: Path | None,
    claude_root: Path | None,
    pi_root: Path | None,
    seats: Sequence[tuple[str, Path] | tuple[str, Path, str]],
    path_map: list[dict[str, str]],
    writers_quiesced: bool,
    managed_exclusions: Sequence[tuple[str, str]] = (),
    rsync_path: Path | None = None,
    max_files: int = DEFAULT_MAX_FILES,
    max_bytes: int = DEFAULT_MAX_BYTES,
    max_sqlite_rows: int = DEFAULT_MAX_SQLITE_ROWS,
) -> dict[str, Any]:
    if role not in {"source", "destination"}:
        raise BulkloadError("capture role must be source or destination")
    if not writers_quiesced:
        raise BulkloadError(
            "agent-capture requires an explicit writer-quiescence acknowledgement"
        )
    home = resolve_real(home)
    git_logical_root, git_root, git_root_link, _ = _declared_root(
        git_root, allow_absent=False
    )
    if not path_map:
        raise BulkloadError("AgentCaptureV4 requires an explicit path map")
    if rsync_path is None:
        raise BulkloadError("AgentCaptureV4 requires an explicit pinned rsync path")
    transport = {
        "hostname": socket.gethostname(),
        "rsync": inspect_rsync(os.fspath(rsync_path)),
    }
    provider_policy = canonical_provider_policy(managed_exclusions)
    exclusions = defaultdict(list)
    for item in provider_policy["managed_exclusions"]:
        exclusions[item["provider"]].append(item["relative_path"])
    blockers: list[dict[str, str]] = []
    discovered, discovery_blockers = _discover_git_roots(git_root)
    blockers.extend(discovery_blockers)
    by_common: dict[Path, Path] = {}
    roots_by_common: dict[Path, set[Path]] = defaultdict(set)
    opaque_git_roots: set[Path] = set()
    for repository in discovered:
        try:
            common = resolve_real(
                Path(
                    _git(
                        repository,
                        ["rev-parse", "--path-format=absolute", "--git-common-dir"],
                    )
                    .decode()
                    .strip()
                )
            )
            git_dir = resolve_real(
                Path(
                    _git(
                        repository,
                        ["rev-parse", "--path-format=absolute", "--git-dir"],
                    )
                    .decode()
                    .strip()
                )
            )
            roots_by_common[common].add(repository)
            if common not in by_common or git_dir == common:
                by_common[common] = repository
        except BulkloadError as error:
            if (
                str(error) == "Git inspection command failed (rev-parse)"
                and not _gitfile_declares_authority(repository)
            ):
                opaque_git_roots.add(repository)
            else:
                blockers.append(
                    {
                        "code": "git-discovery-failed",
                        "path": os.fspath(repository),
                        "detail": str(error),
                    }
                )
    workspaces: list[dict[str, Any]] = []
    nested_roots = set(discovered)
    for common, representative in sorted(
        by_common.items(), key=lambda item: os.fspath(item[1])
    ):
        try:
            workspace, workspace_blockers = _capture_workspace(
                representative,
                git_root=git_root,
                path_map=path_map,
                role=role,
                nested_roots=nested_roots,
                max_files=max_files,
                max_bytes=max_bytes,
            )
            workspaces.append(workspace)
            blockers.extend(workspace_blockers)
        except _OpaqueGitFallback:
            roots = roots_by_common[common]
            if any(common == root or root in common.parents for root in roots):
                opaque_git_roots.update(roots)
            else:
                blockers.append(
                    {
                        "code": "git-workspace-capture-failed",
                        "path": os.fspath(representative),
                        "detail": "opaque Git authority is outside the captured fleet",
                    }
                )
        except BulkloadError as error:
            blockers.append(
                {
                    "code": "git-workspace-capture-failed",
                    "path": os.fspath(representative),
                    "detail": str(error),
                }
            )
    non_git, non_git_blockers = _walk_entries(
        git_root,
        classification="non-git",
        excluded_roots=discovered,
        max_files=max_files,
        max_bytes=max_bytes,
    )
    blockers.extend(non_git_blockers)
    outer_opaque_roots: list[Path] = []
    for root in sorted(
        opaque_git_roots, key=lambda item: (len(item.parts), os.fspath(item))
    ):
        if any(
            parent == root or parent in root.parents for parent in outer_opaque_roots
        ):
            continue
        outer_opaque_roots.append(root)
    for opaque_root in outer_opaque_roots:
        prefix = opaque_root.relative_to(git_root)
        if prefix.parts:
            try:
                non_git.append(
                    _file_record(
                        opaque_root, prefix.as_posix(), classification="non-git"
                    )
                )
            except BulkloadError as error:
                blockers.append(
                    {
                        "code": "unreadable-git-authority",
                        "path": os.fspath(opaque_root),
                        "detail": str(error),
                    }
                )
                continue
        opaque_entries, opaque_blockers = _walk_entries(
            opaque_root,
            classification="non-git",
            excluded_roots={
                candidate
                for candidate in discovered
                if candidate != opaque_root
                and candidate not in opaque_git_roots
                and opaque_root in candidate.parents
            },
            max_files=max_files,
            max_bytes=max_bytes,
        )
        blockers.extend(opaque_blockers)
        for entry in opaque_entries:
            if prefix.parts:
                entry["relative_path"] = (
                    PurePosixPath(prefix.as_posix()) / entry["relative_path"]
                ).as_posix()
            non_git.append(entry)
    non_git.sort(key=lambda item: (item["relative_path"], item["kind"]))
    if len(non_git) > max_files or sum(item["size"] for item in non_git) > max_bytes:
        raise BulkloadError("filesystem capture budget exceeded")
    for entry in non_git:
        entry["destination_relative_path"] = entry["relative_path"]
        entry["identity"] = entry["relative_path"]
    provider_roots = {
        "codex": codex_root or home / ".codex",
        "claude": claude_root or home / ".claude",
        "pi": pi_root or home / ".pi" / "agent",
    }
    providers: list[dict[str, Any]] = []
    for provider, root in provider_roots.items():
        try:
            captured, provider_blockers = _capture_provider(
                provider,
                root,
                role=role,
                path_map=path_map,
                exclusions=exclusions[provider],
                max_files=max_files,
                max_bytes=max_bytes,
                max_sqlite_rows=max_sqlite_rows,
            )
            providers.append(captured)
            blockers.extend(provider_blockers)
        except BulkloadError as error:
            blockers.append(
                {
                    "code": "provider-capture-failed",
                    "path": provider,
                    "detail": str(error),
                }
            )
    seat_records: list[dict[str, Any]] = []
    for declaration in seats:
        name, root = declaration[:2]
        root_kind = declaration[2] if len(declaration) == 3 else "directory"
        captured, seat_blockers = _capture_seat(
            name,
            root,
            root_kind=root_kind,
            role=role,
            path_map=path_map,
            home=home,
            max_files=max_files,
            max_bytes=max_bytes,
        )
        seat_records.append(captured)
        blockers.extend(seat_blockers)
    try:
        destination_git_root = (
            os.fspath(git_root)
            if role == "destination"
            else translate_path(git_logical_root, path_map)
        )
        destination_home = (
            os.fspath(home) if role == "destination" else translate_path(home, path_map)
        )
    except BulkloadError as error:
        blockers.append(
            {"code": "unmapped-root", "path": "root-bindings", "detail": str(error)}
        )
        destination_git_root = "unmapped"
        destination_home = "unmapped"
    catalog = {
        "blockers": sorted(
            blockers,
            key=lambda item: (
                item["code"],
                item.get("path", ""),
                item.get("detail", ""),
            ),
        ),
        "git_workspaces": sorted(workspaces, key=lambda item: item["destination_path"]),
        "non_git": non_git,
        "path_map": path_map,
        "provider_policy": provider_policy,
        "providers": sorted(providers, key=lambda item: item["name"]),
        "root_bindings": {
            "destination_git_root": destination_git_root,
            "destination_home": destination_home,
            "git_root": os.fspath(git_root),
            "git_logical_root": os.fspath(git_logical_root),
            "git_root_link": git_root_link,
            "home": os.fspath(home),
        },
        "runtime_source_sha256": runtime_source_digest(),
        "seats": sorted(seat_records, key=lambda item: item["name"]),
        "transport": transport,
    }
    capture = {
        "capture_id": new_id(),
        "catalog": catalog,
        "catalog_sha256": sha256_bytes(canonical_bytes(catalog)),
        "complete": not blockers,
        "hostname": socket.gethostname(),
        "observed_at": utc_now(),
        "role": role,
        "schema": AGENT_CAPTURE_SCHEMA,
        "writers_quiesced": True,
    }
    return seal(capture, "capture_sha256")


def validate_agent_capture(
    value: dict[str, Any], *, expected_role: str | None = None
) -> None:
    require_exact_keys(
        value,
        {
            "capture_id",
            "capture_sha256",
            "catalog",
            "catalog_sha256",
            "complete",
            "hostname",
            "observed_at",
            "role",
            "schema",
            "writers_quiesced",
        },
        "AgentCaptureV4",
    )
    if value.get("schema") != AGENT_CAPTURE_SCHEMA:
        raise BulkloadError("input is not AgentCaptureV4")
    require_digest(value, "capture_sha256")
    if value.get("role") not in {"source", "destination"}:
        raise BulkloadError("AgentCaptureV4 role is invalid")
    if expected_role is not None and value["role"] != expected_role:
        raise BulkloadError(f"AgentCaptureV4 role must be {expected_role}")
    if value.get("writers_quiesced") is not True:
        raise BulkloadError("AgentCaptureV4 lacks writer quiescence")
    catalog = value.get("catalog")
    if not isinstance(catalog, dict):
        raise BulkloadError("AgentCaptureV4 catalog is missing")
    require_exact_keys(
        catalog,
        {
            "blockers",
            "git_workspaces",
            "non_git",
            "path_map",
            "provider_policy",
            "providers",
            "root_bindings",
            "runtime_source_sha256",
            "seats",
            "transport",
        },
        "AgentCaptureV4 catalog",
    )
    if value.get("catalog_sha256") != sha256_bytes(canonical_bytes(catalog)):
        raise BulkloadError("AgentCaptureV4 catalog digest mismatch")
    require_exact_keys(
        catalog["provider_policy"],
        {"default", "managed_exclusions", "portable_symlinks"},
        "AgentCaptureV4 provider policy",
    )
    if catalog["provider_policy"] != canonical_provider_policy(
        [
            (item["provider"], item["relative_path"])
            for item in catalog["provider_policy"]["managed_exclusions"]
        ]
    ):
        raise BulkloadError("AgentCaptureV4 provider policy is invalid")
    require_exact_keys(
        catalog["transport"], {"hostname", "rsync"}, "AgentCaptureV4 transport"
    )
    require_exact_keys(
        catalog["transport"]["rsync"],
        {"features", "path", "protocol", "sha256"},
        "AgentCaptureV4 rsync binding",
    )
    if catalog["transport"]["rsync"]["features"] != [
        "checksum",
        "delay-updates",
        "files-from",
        "from0",
        "ignore-missing-args",
    ]:
        raise BulkloadError("AgentCaptureV4 rsync feature binding is invalid")
    require_exact_keys(
        catalog["root_bindings"],
        {
            "destination_git_root",
            "destination_home",
            "git_logical_root",
            "git_root",
            "git_root_link",
            "home",
        },
        "AgentCaptureV4 root bindings",
    )
    for provider in catalog["providers"]:
        require_exact_keys(
            provider,
            {
                "destination_path",
                "exists",
                "items",
                "logical_path",
                "name",
                "path",
                "root_link",
            },
            "AgentCaptureV4 provider",
        )
    for seat in catalog["seats"]:
        require_exact_keys(
            seat,
            {
                "destination_path",
                "exists",
                "items",
                "logical_path",
                "name",
                "path",
                "root_kind",
                "root_link",
            },
            "AgentCaptureV4 mutable seat",
        )
        if seat["root_kind"] not in {"directory", "file"}:
            raise BulkloadError("AgentCaptureV4 mutable-seat kind is invalid")
    links = [catalog["root_bindings"]["git_root_link"]]
    links.extend(provider["root_link"] for provider in catalog["providers"])
    links.extend(seat["root_link"] for seat in catalog["seats"])
    for link in links:
        if link is not None:
            require_exact_keys(
                link, {"kind", "mode", "sha256", "size"}, "declared-root link"
            )
    for workspace in catalog.get("git_workspaces", []):
        if not isinstance(workspace, dict):
            raise BulkloadError("AgentCaptureV4 Git workspace is not an object")
        require_exact_keys(
            workspace,
            {
                "branch",
                "common_git_dir",
                "destination_path",
                "head",
                "logical_path",
                "object_files",
                "object_format",
                "path",
                "recovery_anchors",
                "refs",
                "remotes",
                "schema",
                "worktrees",
                "workspace_id",
            },
            "GitWorkspaceV2",
        )
        if workspace.get("schema") != GIT_WORKSPACE_SCHEMA:
            raise BulkloadError("AgentCaptureV4 contains an invalid GitWorkspaceV2")
        for worktree in workspace.get("worktrees", []):
            require_exact_keys(
                worktree,
                {
                    "branch",
                    "destination_path",
                    "detached",
                    "dirt",
                    "files",
                    "git_dir",
                    "head",
                    "index",
                    "locked",
                    "operation_state",
                    "path",
                    "prunable",
                },
                "GitWorkspaceV2 worktree",
            )
            require_exact_keys(
                worktree["index"],
                {"entries", "exists", "mode", "path", "sha256", "size"},
                "GitWorkspaceV2 index",
            )
    if value.get("complete") != (not catalog.get("blockers")):
        raise BulkloadError("AgentCaptureV4 completeness disagrees with blockers")


def stable_capture_pair(
    first: dict[str, Any], second: dict[str, Any], *, role: str
) -> None:
    validate_agent_capture(first, expected_role=role)
    validate_agent_capture(second, expected_role=role)
    if first["capture_id"] == second["capture_id"]:
        raise BulkloadError(f"{role} A/B captures reuse one capture ID")
    if (
        first["catalog_sha256"] != second["catalog_sha256"]
        or first["catalog"] != second["catalog"]
    ):
        raise BulkloadError(f"{role} A/B captures are not byte-stable")
    if not first["complete"] or not second["complete"]:
        raise BulkloadError(f"{role} captures contain blockers")
