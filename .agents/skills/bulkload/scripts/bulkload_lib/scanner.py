"""AgentCaptureV4 and GitWorkspaceV2 read-only capture."""

from __future__ import annotations

from array import array
from collections import defaultdict
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager, suppress
import hashlib
import heapq
import json
import math
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import socket
import sqlite3
import stat
import subprocess
import sys
import tempfile
import time
from typing import Any, Iterable, Sequence
import unicodedata
from urllib.parse import urlsplit, urlunsplit

from .model import (
    AGENT_CAPTURE_SCHEMA,
    GIT_WORKSPACE_SCHEMA,
    BulkloadError,
    MAX_JSON_BYTES,
    assert_no_overlap,
    atomic_write_json,
    canonical_bytes,
    durable_makedirs,
    ensure_safe_target,
    fsync_directory,
    git_environment,
    new_id,
    normalize_relative,
    reflink_clone,
    read_json,
    require_capacity,
    require_digest,
    require_exact_keys,
    resolve_real,
    RUNTIME_SOURCE_NAMES,
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
MAX_CAPTURE_WORKSPACE_WORKERS = 3
MAX_CAPTURE_JOBS = 64
BASE_CUSTODY_MODES = ("full", "sealed")
SNAPSHOT_INDEX_BUFFER_BYTES = 1024 * 1024
# Chosen to minimise resident bytes. One chunk of N strings costs ~130 B each;
# the merge then costs one read block per run, and a block costs twice its size
# because the reader splits it before yielding. So the total is smallest near
# sqrt(entries * 2 * block / 130). At 65_536 and a 64 KiB block a 1.8 M-entry
# root holds ~8.5 MB of chunk and ~7 MB of merge blocks instead of ~1 GB of
# list. Undersizing the chunk is the expensive mistake, not oversizing it: it
# trades one linear term for a quadratic one in the run count.
NAMESPACE_SORT_CHUNK = 65_536
NAMESPACE_RUN_BLOCK_BYTES = 64 * 1024
LIVE_SNAPSHOT_MODE = "immutable-live"
SNAPSHOT_RESERVE_BYTES = 10 * 1024**3
ZERO_OIDS = {"0" * 40, "0" * 64}
HEX_OID = re.compile(r"^[0-9a-f]{40}(?:[0-9a-f]{24})?$")
SQLITE_SUFFIXES = (".sqlite", ".sqlite3", ".db")
SQLITE_SIDECARS = ("-wal", "-shm", "-journal")
DOCTOR_REPORT_SCHEMA = "dev.tinyland.bulkload.doctor-report.v1"
DOCTOR_MAX_FINDINGS = 10_000
# A check whose remedy is "no action" names a sample, not a population. The
# ceremony holds 16,900 symlinks; naming every one of them produces a
# multi-megabyte artifact the operator is told to review and cannot.
DOCTOR_SAMPLE_FINDINGS = 20
# `partial` and `skipped` exist so that a check the run could not evaluate is
# never spelled `pass`. A truncated walk or an unreachable peer degrades the
# checks that depend on it to one of these, and `ok` still turns only on
# `fail`, so a preflight that could not answer never reads as green.
DOCTOR_STATUSES = ("pass", "partial", "skipped", "warn", "fail")
# Resolved through `--peer-env-path` rather than assumed at an absolute path:
# a NixOS peer ships /usr/bin/env and nothing else under /usr/bin, so
# `/usr/bin/uname` exits 127 on a reachable, correctly configured host.
DEFAULT_PEER_UNAME = "uname"
DEFAULT_PEER_ENV = "/usr/bin/env"
DEFAULT_PEER_TIMEOUT_SECONDS = 20.0
# PATH_MAX as each kernel enforces it. A path map may lengthen every path in
# a root at once (/Users/jess -> /srv/fast-local/jess/bulkload/...), so the
# ceiling that matters is the destination kernel's, not this host's.
PATH_LENGTH_CEILINGS = {"darwin": 1024, "linux": 4096}
# Everything the engine refuses to record. `_file_record` raises on any of
# these (scanner.py:514) and `_capture_provider` records `special-agent-state`
# for them; a socket under ~/.claude, ~/.codex or ~/.pi/agent is the single
# most likely instance on a live agent host.
_SPECIAL_ENTRY_KINDS = {
    stat.S_IFSOCK: "socket",
    stat.S_IFIFO: "fifo",
    stat.S_IFBLK: "block-device",
    stat.S_IFCHR: "character-device",
    getattr(stat, "S_IFDOOR", 0o150000): "door",
    getattr(stat, "S_IFWHT", 0o160000): "whiteout",
}
# Linux fixes every symlink at 0777 and has no lchmod; darwin reports the
# link's own bits. Any other mode is therefore a darwin spelling.
LINUX_SYMLINK_MODE = "0777"
POSIX_PROBE_TOKEN = "bulkload-posix-ok"
# Login shells that cannot parse POSIX parameter expansion. `ssh HOST cmd`
# hands the command to the peer's login shell, so a fish seat breaks every
# remote step that is not a bare simple command.
NON_POSIX_LOGIN_SHELLS = frozenset(
    {"csh", "elvish", "fish", "ion", "nu", "rc", "tcsh", "xonsh"}
)
# The transport's own SSH option vector, bound here so `doctor`'s probes and
# `push_agent_transport`'s rsync cannot drift apart.
SSH_OPTIONS = (
    "-oBatchMode=yes",
    "-oStrictHostKeyChecking=yes",
    "-oClearAllForwardings=yes",
)
SSH_HOST_AUTHORITY = re.compile(
    r"(?:(?:[a-z_][a-z0-9_-]{0,31})@)?([A-Za-z0-9](?:[A-Za-z0-9.-]{0,251}[A-Za-z0-9])?)"
)
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


def _is_pruned(provider: str | None, relative: str, exclusions: Sequence[str]) -> bool:
    """The one prune rule: a managed exclusion or a regenerate namespace.

    This predicate decides which paths a census, a copy and a charge all agree
    not to see, so it fixes `generation_sha256` and `index_sha256`. It used to
    be spelled out at eight call sites, and *not* identically: the six sites in
    the census, the copy and the charge guard it with `provider is not None`,
    while the two inside `_capture_provider` do not. That asymmetry is
    preserved at the call sites rather than folded in here, because folding it
    in would change `_capture_provider`'s behaviour for a None provider.
    """
    return _is_excluded(relative, exclusions) or _is_regenerate_namespace(
        provider, relative
    )


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


class _PhaseSample:
    """Counters a timed phase fills in while it runs."""

    __slots__ = ("bytes", "files")

    def __init__(self) -> None:
        self.bytes: int | None = None
        self.files: int | None = None


@contextmanager
def phase_timing(phase: str, root: str = "-") -> Iterable[_PhaseSample]:
    """Emit one stderr timing line per phase when BULKLOAD_PHASE_TIMING=1.

    Timing is diagnostic only: it never reaches an artifact, never changes a
    digest, and is off unless the operator asks for it. The counters are read
    at exit, so a phase sets them inside the block.
    """
    sample = _PhaseSample()
    if os.environ.get("BULKLOAD_PHASE_TIMING") != "1":
        yield sample
        return
    started = time.monotonic()
    try:
        yield sample
    finally:
        sys.stderr.write(
            "bulkload-phase"
            f" phase={phase}"
            f" root={root}"
            f" seconds={time.monotonic() - started:.3f}"
            f" files={'-' if sample.files is None else sample.files}"
            f" bytes={'-' if sample.bytes is None else sample.bytes}"
            "\n"
        )
        sys.stderr.flush()


def workspace_worker_count(jobs: int | None, pending: int) -> int:
    """Resolve the Git-workspace pool size; `None` keeps the shipped default.

    `git fsck --full` is memory-hungry per repository, so raising this above
    the shipped 3 is an explicit operator decision on a host with the headroom
    to pay for it, never an automatic function of CPU count.
    """
    resolved = MAX_CAPTURE_WORKSPACE_WORKERS if jobs is None else jobs
    if not 1 <= resolved <= MAX_CAPTURE_JOBS:
        raise BulkloadError("capture job count is out of range")
    return min(resolved, pending)


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
            env=git_environment(),
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


def _file_record(
    path: Path,
    relative: str,
    *,
    classification: str,
    deferred_digest: bool = False,
) -> dict[str, Any]:
    """Describe one captured entry.

    `deferred_digest` is only ever set by a caller that overwrites `sha256`
    from a second, authoritative read of the same bytes. It suppresses the
    whole-file hash here so those bytes are read once instead of twice; every
    other field, including the `_stable_stat` fence, is unaffected.
    """
    normalized = normalize_relative(relative)
    before = _stable_stat(path)
    file_type = before[2]
    mode = f"{before[3]:04o}"
    if file_type == stat.S_IFREG:
        digest = None if deferred_digest else sha256_file(path)
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


class _GitBlobBatch:
    """Hash indexed blobs through one persistent Git process per workspace."""

    def __init__(self, repository: Path) -> None:
        self._repository = repository
        self._process: subprocess.Popen[bytes] | None = None
        self._cache: dict[str, str] = {}

    def __enter__(self) -> _GitBlobBatch:
        return self

    def __exit__(self, _kind: object, _value: object, _traceback: object) -> None:
        self.close()

    def _start(self) -> subprocess.Popen[bytes]:
        if self._process is None:
            try:
                self._process = subprocess.Popen(
                    [
                        "git",
                        "-C",
                        os.fspath(self._repository),
                        "cat-file",
                        "--batch",
                    ],
                    stdin=subprocess.PIPE,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.DEVNULL,
                    env=git_environment(),
                )
            except OSError as error:
                raise BulkloadError("Git is unavailable") from error
        return self._process

    def sha256(self, oid: str) -> str | None:
        if oid in ZERO_OIDS:
            return None
        if not HEX_OID.fullmatch(oid):
            raise BulkloadError("indexed Git blob has a malformed object ID")
        if oid in self._cache:
            return self._cache[oid]
        process = self._start()
        assert process.stdin is not None
        assert process.stdout is not None
        try:
            process.stdin.write(oid.encode("ascii") + b"\n")
            process.stdin.flush()
            header = process.stdout.readline()
            fields = header.rstrip(b"\n").split(b" ")
            if len(fields) != 3 or fields[1] != b"blob":
                raise BulkloadError("cannot inspect indexed Git blob")
            resolved_oid = fields[0].decode("ascii", errors="strict")
            size = int(fields[2])
            if not HEX_OID.fullmatch(resolved_oid) or size < 0:
                raise BulkloadError("cannot inspect indexed Git blob")
            digest = hashlib.sha256()
            remaining = size
            while remaining:
                chunk = process.stdout.read(min(1024 * 1024, remaining))
                if not chunk:
                    raise BulkloadError("cannot inspect indexed Git blob")
                digest.update(chunk)
                remaining -= len(chunk)
            if process.stdout.read(1) != b"\n":
                raise BulkloadError("cannot inspect indexed Git blob")
        except (BrokenPipeError, OSError, UnicodeDecodeError, ValueError) as error:
            raise BulkloadError("cannot inspect indexed Git blob") from error
        self._cache[oid] = digest.hexdigest()
        return self._cache[oid]

    def close(self) -> None:
        process = self._process
        self._process = None
        if process is None:
            return
        if process.stdin is not None:
            process.stdin.close()
        if process.stdout is not None:
            process.stdout.close()
        try:
            returncode = process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
            raise BulkloadError("cannot close indexed Git blob inspection") from None
        if returncode != 0:
            raise BulkloadError("cannot close indexed Git blob inspection")


def _capture_worktree(
    worktree_record: dict[str, Any],
    *,
    path_map: list[dict[str, str]],
    role: str,
    nested_roots: set[Path],
    max_files: int,
    max_bytes: int,
    blob_batch: _GitBlobBatch,
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
            expected_sha = blob_batch.sha256(stage_zero["oid"])
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
    with _GitBlobBatch(representative) as blob_batch:
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
                    blob_batch=blob_batch,
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
    hasher = hashlib.sha256()
    # While every transformed line is byte-identical to its original, the
    # translated stream *is* the original stream: no second parse, no second
    # per-line hash and no second list of 64-character digests. Both lazily
    # fork on the first line a replacement actually rewrites, so a partially
    # rewritten file still produces exactly the values it produced before.
    transformed_hashes: list[str] | None = None
    transformed_hasher: Any = None
    try:
        with path.open("rb") as stream:
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
                if transformed == line:
                    # The rewrite is a no-op on this line, so the second
                    # json.loads would re-parse bytes that just parsed and the
                    # second sha256 would re-hash bytes that just hashed.
                    digest = sha256_bytes(line)
                    hashes.append(digest)
                    hasher.update(line)
                    if transformed_hashes is not None:
                        transformed_hashes.append(digest)
                        transformed_hasher.update(line)
                    continue
                try:
                    json.loads(transformed)
                except (UnicodeDecodeError, json.JSONDecodeError) as error:
                    raise BulkloadError(
                        "path rewriting produced invalid JSONL"
                    ) from error
                if transformed_hashes is None:
                    transformed_hashes = list(hashes)
                    transformed_hasher = hasher.copy()
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
        "translated_records": hashes
        if transformed_hashes is None
        else (transformed_hashes),
        "translated_sha256": (
            hasher.hexdigest()
            if transformed_hasher is None
            else transformed_hasher.hexdigest()
        ),
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
            if _is_pruned(provider, relative, exclusions):
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
            if _is_pruned(provider, relative, exclusions):
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
        append_jsonl = classification.startswith("append-jsonl")
        # Both arms of the append-jsonl branch below replace `sha256`: the
        # normal one from _jsonl_records' own streaming hash of the same
        # bytes, the _MalformedAppendState one by rebuilding the record from
        # scratch. Hashing the whole file here as well was a second full read
        # of the entire session corpus whose result was always discarded.
        record = _file_record(
            path,
            relative,
            classification=classification,
            deferred_digest=append_jsonl,
        )
        identity = _session_identity(relative, path) if append_jsonl else relative
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
        if append_jsonl:
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
            if record["kind"] == "regular" and record["sha256"] is None:
                raise BulkloadError(f"append-only state lost its digest: {path}")
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


def _snapshot_contract(catalog: dict[str, Any]) -> str:
    """Bind the policy and live roots while excluding mutable catalog bytes."""
    providers = [
        {
            key: provider[key]
            for key in (
                "destination_path",
                "exists",
                "logical_path",
                "name",
                "path",
                "root_link",
            )
        }
        for provider in catalog["providers"]
    ]
    seats = [
        {
            key: seat[key]
            for key in (
                "destination_path",
                "exists",
                "logical_path",
                "name",
                "path",
                "root_kind",
                "root_link",
            )
        }
        for seat in catalog["seats"]
    ]
    return sha256_bytes(
        canonical_bytes(
            {
                "path_map": catalog["path_map"],
                "provider_policy": catalog["provider_policy"],
                "providers": providers,
                "root_bindings": catalog["root_bindings"],
                "runtime_source_sha256": catalog["runtime_source_sha256"],
                "seats": seats,
                "transport": catalog["transport"],
            }
        )
    )


def _tree_census(
    root: Path,
    *,
    provider: str | None,
    exclusions: Sequence[str],
    content: bool = False,
    portable: bool = False,
) -> tuple[str, int]:
    digest = hashlib.sha256()
    charged_bytes = 0

    def observe(path: Path, relative: str) -> tuple[str, int]:
        before = _stable_stat(path)
        live_sqlite = (
            provider is not None
            and relative != "."
            and _provider_classification(provider, relative) == "sqlite"
            and not relative.lower().endswith(SQLITE_SIDECARS)
        )
        if before[2] == stat.S_IFREG:
            kind = "regular"
            content_digest = sha256_file(path) if content and not live_sqlite else None
            size = before[5]
        elif before[2] == stat.S_IFLNK:
            kind = "symlink"
            content_digest = sha256_symlink(path)
            size = 0
        elif before[2] == stat.S_IFDIR:
            kind = "directory"
            content_digest = None
            size = 0
        elif before[2] == stat.S_IFSOCK:
            kind = "socket"
            content_digest = None
            size = 0
        else:
            kind = "special"
            content_digest = None
            size = 0
        after = _stable_stat(path)
        stable_identity = (
            before[:5] == after[:5] if live_sqlite or not content else before == after
        )
        if not stable_identity:
            raise BulkloadError(f"live snapshot census entry changed: {path}")
        if portable:
            authority = [before[2], before[3]]
            if not live_sqlite:
                authority.append(before[5])
        else:
            authority = (
                list(before) if content and not live_sqlite else list(before[:5])
            )
        digest.update(
            canonical_bytes([relative, kind, authority, content_digest]) + b"\0"
        )
        return kind, size

    root_info = root.stat(follow_symlinks=False)
    if stat.S_ISREG(root_info.st_mode):
        _, charged_bytes = observe(root, ".")
        return digest.hexdigest(), charged_bytes
    if not stat.S_ISDIR(root_info.st_mode):
        raise BulkloadError(f"snapshot root is not a regular file or directory: {root}")
    observe(root, ".")
    for current, directories, files in os.walk(root, topdown=True, followlinks=False):
        current_path = Path(current)
        retained = []
        for name in sorted(directories):
            child = current_path / name
            relative = child.relative_to(root).as_posix()
            if provider is not None and _is_pruned(provider, relative, exclusions):
                continue
            kind, _ = observe(child, relative)
            if kind == "directory":
                retained.append(name)
        directories[:] = retained
        sqlite_primaries = {
            name.lower()
            for name in files
            if provider is not None and name.lower().endswith(SQLITE_SUFFIXES)
        }
        for name in sorted(files):
            child = current_path / name
            relative = child.relative_to(root).as_posix()
            if provider is not None and _is_pruned(provider, relative, exclusions):
                continue
            if (
                provider is not None
                and name.lower().endswith(SQLITE_SIDECARS)
                and _sqlite_primary(name.lower()) in sqlite_primaries
            ):
                continue
            _, size = observe(child, relative)
            charged_bytes += size
    return digest.hexdigest(), charged_bytes


def _tree_generation(
    root: Path, *, provider: str | None, exclusions: Sequence[str]
) -> str:
    return _tree_census(
        root,
        provider=provider,
        exclusions=exclusions,
        content=True,
        portable=True,
    )[0]


def _base_regular_reusable(
    source: Path,
    base: Path | None,
    mode: int,
    base_record: dict[str, Any] | None = None,
) -> bool:
    if base is None:
        return False
    if base_record is None:
        try:
            base_info = base.stat(follow_symlinks=False)
        except FileNotFoundError:
            return False
        if not stat.S_ISREG(base_info.st_mode):
            return False
        expected_mode = f"{stat.S_IMODE(base_info.st_mode):04o}"
        expected_size = base_info.st_size
        expected_digest = sha256_file(base)
    else:
        if base_record["kind"] != "regular":
            return False
        expected_mode = base_record["mode"]
        expected_size = base_record["size"]
        expected_digest = base_record["sha256"]
    if (
        expected_mode != f"{mode:04o}"
        or expected_size != source.stat(follow_symlinks=False).st_size
    ):
        return False
    before = _stable_stat(source)
    reusable = sha256_file(source) == expected_digest
    if _stable_stat(source) != before:
        raise BulkloadError(f"live file changed during base comparison: {source}")
    return reusable


def _copy_live_regular(
    source: Path,
    destination: Path,
    mode: int,
    *,
    base: Path | None = None,
    base_record: dict[str, Any] | None = None,
) -> str:
    durable_makedirs(destination.parent)
    if _base_regular_reusable(source, base, mode, base_record):
        before = _stable_stat(source)
        expected = (
            base_record["sha256"] if base_record is not None else sha256_file(base)
        )
        result = reflink_clone(base, destination, expected_sha256=expected, mode=mode)
        if _stable_stat(source) != before:
            destination.unlink(missing_ok=True)
            raise BulkloadError(f"live file changed during base clone: {source}")
        return f"base-{result['method']}"
    source_info = source.stat(follow_symlinks=False)
    if source_info.st_dev == destination.parent.stat().st_dev:
        for _ in range(3):
            before = _stable_stat(source)
            try:
                result = reflink_clone(source, destination, mode=mode)
            except BulkloadError:
                if destination.exists() or destination.is_symlink():
                    raise
                break
            if _stable_stat(source) == before:
                return result["method"]
            destination.unlink(missing_ok=True)
        else:
            raise BulkloadError(f"live file did not converge for snapshot: {source}")
    for _ in range(3):
        descriptor, temporary_name = tempfile.mkstemp(
            prefix=f".{destination.name}.snapshot-", dir=destination.parent
        )
        temporary = Path(temporary_name)
        try:
            source_fd = os.open(
                source,
                os.O_RDONLY
                | getattr(os, "O_NOFOLLOW", 0)
                | getattr(os, "O_CLOEXEC", 0),
            )
            before = os.fstat(source_fd)
            with (
                os.fdopen(source_fd, "rb", buffering=0, closefd=True) as input_stream,
                os.fdopen(descriptor, "wb", buffering=0, closefd=True) as output,
            ):
                shutil.copyfileobj(input_stream, output, 1024 * 1024)
                output.flush()
                os.fsync(output.fileno())
                after = os.fstat(input_stream.fileno())
            if (
                before.st_dev,
                before.st_ino,
                before.st_size,
                before.st_mtime_ns,
                before.st_ctime_ns,
            ) != (
                after.st_dev,
                after.st_ino,
                after.st_size,
                after.st_mtime_ns,
                after.st_ctime_ns,
            ):
                temporary.unlink(missing_ok=True)
                continue
            os.chmod(temporary, mode)
            os.replace(temporary, destination)
            fsync_directory(destination.parent)
            return "capacity-accounted-copy"
        except BaseException:
            try:
                os.close(descriptor)
            except OSError:
                pass
            temporary.unlink(missing_ok=True)
            raise
    raise BulkloadError(f"live file did not converge for snapshot: {source}")


def _copy_live_tree(
    source: Path,
    destination: Path,
    *,
    provider: str | None,
    exclusions: Sequence[str],
    max_sqlite_rows: int,
    base: Path | None = None,
    base_records: _RootRecords | dict[str, dict[str, Any]] | None = None,
) -> tuple[dict[str, int], dict[str, dict[str, int | str]]]:
    methods: dict[str, int] = defaultdict(int)
    ledger: dict[str, dict[str, int | str]] = {}

    def record(relative: str, method: str, live: Path, target: Path) -> None:
        source_info = live.stat(follow_symlinks=False)
        destination_info = target.stat(follow_symlinks=False)
        ledger[relative] = {
            "destination_device": destination_info.st_dev,
            "method": method,
            "source_device": source_info.st_dev,
        }

    source_info = source.stat(follow_symlinks=False)
    if stat.S_ISREG(source_info.st_mode):
        method = _copy_live_regular(
            source,
            destination,
            stat.S_IMODE(source_info.st_mode),
            base=base,
            base_record=(base_records or {}).get("."),
        )
        methods[method] += 1
        record(
            ".",
            method,
            base if method.startswith("base-") and base is not None else source,
            destination,
        )
        return dict(methods), ledger
    if not stat.S_ISDIR(source_info.st_mode):
        raise BulkloadError(
            f"snapshot root is not a regular file or directory: {source}"
        )
    durable_makedirs(destination, mode=stat.S_IMODE(source.stat().st_mode))
    record(".", "directory", source, destination)
    for current, directories, files in os.walk(source, topdown=True, followlinks=False):
        current_path = Path(current)
        relative_parent = current_path.relative_to(source)
        target_parent = destination / relative_parent
        retained = []
        for name in sorted(directories):
            child = current_path / name
            relative = child.relative_to(source).as_posix()
            if provider is not None and _is_pruned(provider, relative, exclusions):
                continue
            info = child.stat(follow_symlinks=False)
            target = target_parent / name
            base_target = base / relative if base is not None else None
            if stat.S_ISLNK(info.st_mode):
                before = _stable_stat(child)
                link_target = os.readlink(child)
                if _stable_stat(child) != before:
                    raise BulkloadError(
                        f"live symlink changed during snapshot: {child}"
                    )
                os.symlink(link_target, target)
                methods["symlink"] += 1
                record(relative, "symlink", child, target)
            elif stat.S_ISDIR(info.st_mode):
                durable_makedirs(target, mode=stat.S_IMODE(info.st_mode))
                retained.append(name)
                methods["directory"] += 1
                record(relative, "directory", child, target)
            else:
                raise BulkloadError(f"snapshot tree contains special entry: {child}")
        directories[:] = retained
        sqlite_primaries = {
            name.lower()
            for name in files
            if provider is not None and name.lower().endswith(SQLITE_SUFFIXES)
        }
        for name in sorted(files):
            child = current_path / name
            relative = child.relative_to(source).as_posix()
            if provider is not None and _is_pruned(provider, relative, exclusions):
                continue
            if any(name.lower().endswith(suffix) for suffix in SQLITE_SIDECARS):
                primary_name = _sqlite_primary(name.lower())
                if provider is not None and primary_name in sqlite_primaries:
                    continue
                raise BulkloadError(f"orphan SQLite sidecar in live snapshot: {child}")
            info = child.stat(follow_symlinks=False)
            target = target_parent / name
            base_target = base / relative if base is not None else None
            if stat.S_ISLNK(info.st_mode):
                before = _stable_stat(child)
                link_target = os.readlink(child)
                if _stable_stat(child) != before:
                    raise BulkloadError(
                        f"live symlink changed during snapshot: {child}"
                    )
                os.symlink(link_target, target)
                methods["symlink"] += 1
                record(relative, "symlink", child, target)
            elif not stat.S_ISREG(info.st_mode):
                raise BulkloadError(f"snapshot tree contains special entry: {child}")
            elif (
                provider is not None
                and _provider_classification(provider, relative) == "sqlite"
            ):
                snapshot_sqlite(child, target, max_rows=max_sqlite_rows)
                os.chmod(target, stat.S_IMODE(info.st_mode))
                methods["sqlite-online-backup"] += 1
                record(relative, "sqlite-online-backup", child, target)
            else:
                method = _copy_live_regular(
                    child,
                    target,
                    stat.S_IMODE(info.st_mode),
                    base=base_target,
                    base_record=(base_records or {}).get(relative),
                )
                methods[method] += 1
                record(
                    relative,
                    method,
                    base_target
                    if method.startswith("base-") and base_target is not None
                    else child,
                    target,
                )
    return dict(methods), ledger


def _snapshot_index_record(
    path: Path,
    *,
    root_index: int,
    relative: str,
    transfer: dict[str, int | str],
) -> dict[str, Any]:
    info = path.stat(follow_symlinks=False)
    mode = f"{stat.S_IMODE(info.st_mode):04o}"
    if stat.S_ISREG(info.st_mode):
        kind = "regular"
        size = info.st_size
        digest = sha256_file(path)
    elif stat.S_ISLNK(info.st_mode):
        kind = "symlink"
        size = len(os.fsencode(os.readlink(path)))
        digest = sha256_symlink(path)
    elif stat.S_ISDIR(info.st_mode):
        kind = "directory"
        size = 0
        digest = None
    else:
        raise BulkloadError(f"snapshot payload contains special entry: {path}")
    return {
        "destination_device": transfer["destination_device"],
        "kind": kind,
        "method": transfer["method"],
        "mode": mode,
        "relative_path": relative,
        "root_index": root_index,
        "sha256": digest,
        "size": size,
        "source_device": transfer["source_device"],
    }


def _snapshot_delta_charge(
    source: Path,
    base: Path | None,
    *,
    provider: str | None,
    exclusions: Sequence[str],
    base_records: _RootRecords | dict[str, dict[str, Any]] | None = None,
) -> int:
    info = source.stat(follow_symlinks=False)
    if stat.S_ISREG(info.st_mode):
        return (
            0
            if _base_regular_reusable(
                source,
                base,
                stat.S_IMODE(info.st_mode),
                (base_records or {}).get("."),
            )
            else info.st_size
        )
    charged = 0
    for current, directories, files in os.walk(source, topdown=True, followlinks=False):
        current_path = Path(current)
        retained = []
        for name in sorted(directories):
            child = current_path / name
            relative = child.relative_to(source).as_posix()
            if provider is not None and _is_pruned(provider, relative, exclusions):
                continue
            if stat.S_ISDIR(child.stat(follow_symlinks=False).st_mode):
                retained.append(name)
        directories[:] = retained
        sqlite_primaries = {
            name.lower()
            for name in files
            if provider is not None and name.lower().endswith(SQLITE_SUFFIXES)
        }
        for name in sorted(files):
            child = current_path / name
            relative = child.relative_to(source).as_posix()
            if provider is not None and _is_pruned(provider, relative, exclusions):
                continue
            if (
                provider is not None
                and name.lower().endswith(SQLITE_SIDECARS)
                and _sqlite_primary(name.lower()) in sqlite_primaries
            ):
                continue
            child_info = child.stat(follow_symlinks=False)
            if not stat.S_ISREG(child_info.st_mode):
                continue
            base_path = base / relative if base is not None else None
            if (
                provider is not None
                and _provider_classification(provider, relative) == "sqlite"
            ):
                charged += child_info.st_size
            elif not _base_regular_reusable(
                child,
                base_path,
                stat.S_IMODE(child_info.st_mode),
                (base_records or {}).get(relative),
            ):
                charged += child_info.st_size
    return charged


def _namespace_entries(root: Path) -> Iterable[str]:
    """Every relative in the root's namespace, in walk order, one at a time."""
    yield "."
    if root.is_dir():
        for current, directories, files in os.walk(
            root, topdown=True, followlinks=False
        ):
            # The global sort below subsumes this, but a deterministic descent
            # keeps two walks of the same tree racing the same way.
            directories[:] = sorted(directories)
            current_path = Path(current)
            for name in directories:
                yield (current_path / name).relative_to(root).as_posix()
            for name in sorted(files):
                yield (current_path / name).relative_to(root).as_posix()


def _namespace_run(values: list[str], spill_dir: str | None) -> Any:
    """One sorted run, NUL-delimited, in a file that has no name."""
    handle = tempfile.TemporaryFile(  # noqa: SIM115 — closed by the merge's finally
        dir=spill_dir, buffering=NAMESPACE_RUN_BLOCK_BYTES
    )
    handle.write(b"\0".join(item.encode("utf-8", "surrogatepass") for item in values))
    handle.seek(0)
    return handle


def _read_namespace_run(handle: Any) -> Iterable[str]:
    """Decode one run without ever holding more than a block of it."""
    pending = b""
    while True:
        block = handle.read(NAMESPACE_RUN_BLOCK_BYTES)
        if not block:
            break
        pending += block
        parts = pending.split(b"\0")
        pending = parts.pop()
        for part in parts:
            yield part.decode("utf-8", "surrogatepass")
    if pending:
        yield pending.decode("utf-8", "surrogatepass")


def _bounded_sorted(entries: Iterable[str]) -> Iterable[str]:
    """`sorted()` over a stream without a list of the stream.

    W2-2. `_snapshot_namespace` materialised the whole namespace as a list of
    `(str, Path)` — 206 B per entry measured, and roughly 0.9 GB on the 109 G
    corpus. It was built twice per leg: once to write the snapshot index and
    once to re-derive the namespace digest during custody validation.

    Nothing needs the list. Both callers consume it in order, so this sorts
    bounded runs and merges them. The comparison stays a Python `str`
    comparison end to end — runs are only *stored* as UTF-8 under
    `surrogatepass`, and every value is decoded back to `str` before it
    reaches `heapq.merge` — so the order is exactly the `sorted(observed)`
    order the index and `namespace_digest` were sealed in. That order is
    digest-load-bearing, which is why the landing gate is `index_sha256`
    equality across the lever rather than a spot check.

    A stream that fits in one chunk never touches the disk and is the
    identical in-memory `sorted()` it always was.
    """
    if not _env_lever("BULKLOAD_BOUND_NAMESPACE"):
        yield from sorted(entries)
        return
    try:
        chunk_size = max(1, int(os.environ.get("BULKLOAD_NAMESPACE_CHUNK", "")))
    except ValueError:
        chunk_size = NAMESPACE_SORT_CHUNK
    spill_dir = os.environ.get("BULKLOAD_SPILL_DIR") or None
    # Same env var, same hazard: runs on tmpfs relieve nothing. Warned once
    # per process per directory, so this costs one dict lookup per call.
    _warn_memory_backed_spill(Path(spill_dir or tempfile.gettempdir()))
    chunk: list[str] = []
    runs: list[Any] = []
    try:
        for relative in entries:
            chunk.append(relative)
            if len(chunk) >= chunk_size:
                chunk.sort()
                runs.append(_namespace_run(chunk, spill_dir))
                chunk = []
        chunk.sort()
        if not runs:
            yield from chunk
            return
        if chunk:
            runs.append(_namespace_run(chunk, spill_dir))
        chunk = []
        yield from heapq.merge(*(_read_namespace_run(run) for run in runs))
    finally:
        for run in runs:
            run.close()


def _snapshot_namespace(root: Path) -> Iterable[tuple[str, Path]]:
    """The sealed namespace, in sealed order, one entry at a time.

    `root / relative` reconstructs exactly the `current_path / name` the old
    list carried: `relative` is that path's `relative_to(root).as_posix()`, so
    the join round-trips.
    """
    for relative in _bounded_sorted(_namespace_entries(root)):
        yield relative, (root if relative == "." else root / relative)


def _write_snapshot_index(
    path: Path,
    roots: Sequence[dict[str, str]],
    ledgers: Sequence[dict[str, dict[str, int | str]]],
) -> tuple[str, int]:
    descriptor = os.open(
        path,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_CLOEXEC", 0),
        0o600,
    )
    digest = hashlib.sha256()
    count = 0
    try:
        # Buffered, not unbuffered: the index is ~1-2 M records of a few
        # hundred bytes each per leg, and buffering=0 turned every record into
        # its own write(2). Durability is unchanged — the explicit flush() plus
        # os.fsync() below, and the directory fsync after, are what make the
        # index durable, not the absence of a userspace buffer.
        stream = os.fdopen(descriptor, "wb", SNAPSHOT_INDEX_BUFFER_BYTES, closefd=True)
        with stream:
            for root_index, (binding, ledger) in enumerate(zip(roots, ledgers)):
                root = Path(binding["snapshot"])
                for relative, payload_path in _snapshot_namespace(root):
                    try:
                        transfer = ledger[relative]
                    except KeyError as error:
                        raise BulkloadError(
                            "snapshot payload lacks per-entry transfer custody"
                        ) from error
                    record = _snapshot_index_record(
                        payload_path,
                        root_index=root_index,
                        relative=relative,
                        transfer=transfer,
                    )
                    payload = canonical_bytes(record) + b"\n"
                    stream.write(payload)
                    digest.update(payload)
                    count += 1
            stream.flush()
            os.fsync(stream.fileno())
    except BaseException:
        try:
            os.close(descriptor)
        except OSError:
            pass
        path.unlink(missing_ok=True)
        raise
    fsync_directory(path.parent)
    return digest.hexdigest(), count


MEMORY_BACKED_FILESYSTEMS = frozenset({"ramfs", "tmpfs"})
_WARNED_SPILL_DIRS: set[str] = set()


def _memory_backed_mount(candidate: Path, mountinfo: str) -> bool:
    """Whether `candidate`'s longest containing mount is RAM-backed.

    `/proc/self/mountinfo` lines are `<fields> - <fstype> <source> <opts>`, and
    field 5 is the mount point. The longest matching mount point wins, so a
    tmpfs mounted *under* a disk-backed parent is still reported.
    """
    kind = ""
    best = -1
    for entry in mountinfo.splitlines():
        head, separator, tail = entry.partition(" - ")
        fields = head.split()
        rest = tail.split()
        if not separator or len(fields) < 5 or not rest:
            continue
        point = fields[4].replace("\\040", " ").replace("\\011", "\t")
        if len(point) > best and _within(candidate, Path(point)):
            best, kind = len(point), rest[0]
    return kind in MEMORY_BACKED_FILESYSTEMS


def _warn_memory_backed_spill(candidate: Path) -> None:
    """Say so, once, when a spill target cannot actually relieve memory.

    A spill dir on tmpfs/ramfs — the default `/tmp` on most Linux hosts — makes
    both W2-1 and W2-2 write their "spilled" bytes straight back into RAM. The
    slices then do nothing while still reporting success, which is the one
    outcome a memory wave must never produce silently.

    This warns rather than refuses on purpose. Refusing would fail the
    *default* configuration of every Linux host, which is a worse regression
    than an inert slice; and the ceremony host is darwin, which has no tmpfs
    and whose `TMPDIR` (`/var/folders/...`) is APFS. Detection is Linux-only —
    `statvfs` carries no filesystem type — so on darwin this is a no-op and the
    warning never fires.
    """
    key = os.fspath(candidate)
    if key in _WARNED_SPILL_DIRS:
        return
    # Marked before the read, not after, so a host without `/proc` (darwin)
    # attempts it once per directory rather than once per call.
    _WARNED_SPILL_DIRS.add(key)
    try:
        backed = _memory_backed_mount(
            candidate, Path("/proc/self/mountinfo").read_text(encoding="utf-8")
        )
    except OSError:
        return
    if backed:
        sys.stderr.write(
            f"bulkload-spill warning=memory-backed dir={key} effect=no-memory-relief\n"
        )
        sys.stderr.flush()


def _base_record_spill_dir(snapshot_root: Path, partial: Path) -> Path | None:
    """Where the base-record arena lives, or `None` when the lever is off.

    The arena is unlinked the instant it is created, so it can never enter a
    census; the refusal below is defence in depth against a `BULKLOAD_SPILL_DIR`
    that names the capture itself. Only the "spill dir inside the capture"
    direction is checked — the containing direction is the normal case, since
    `TMPDIR` legitimately contains the capture in every test and on every host
    whose stage root is a temporary directory.

    The lever is read *first*, ahead of every other env read, because X1 wants
    the off position to restore shipped behaviour exactly and the shipped
    engine never looks at `BULKLOAD_SPILL_DIR` at all. Consulting the lever
    downstream of the refusal — which is where it used to sit, at the
    `_BaseRecordMap` construction — meant the off position could still kill a
    capture with an error the shipped engine has no equivalent of. The
    counterexample is `test_lever_off_ignores_a_spill_dir_inside_the_capture`.
    """
    if not _env_lever("BULKLOAD_SPILL_BASE_RECORDS"):
        return None
    spill_dir = Path(os.environ.get("BULKLOAD_SPILL_DIR") or tempfile.gettempdir())
    candidate = Path(os.path.abspath(os.fspath(spill_dir.expanduser())))
    for root in (snapshot_root, partial):
        if _within(candidate, Path(os.path.abspath(os.fspath(root)))):
            raise BulkloadError("base record spill dir is inside the live snapshot")
    _warn_memory_backed_spill(candidate)
    return spill_dir


def _env_lever(name: str) -> bool:
    """One env-var rollback lever, default on. The only class X1 respects."""
    return os.environ.get(name, "1").strip().lower() not in {"0", "false", "no", "off"}


class _RootRecords:
    """One root's `relative -> sealed base index record` view."""

    __slots__ = ("_owner", "_low", "_high")

    def __init__(self, owner: _BaseRecordMap, low: int, high: int) -> None:
        self._owner = owner
        self._low = low
        self._high = high

    def __len__(self) -> int:
        return self._high - self._low

    def get(self, relative: str) -> dict[str, Any] | None:
        return self._owner.probe(self._low, self._high, relative)


class _BaseRecordMap:
    """`(label, relative) -> base index record`, keys resident, records spilled.

    W2-1. `validate_snapshot_custody(collect_records=True)` used to hand back a
    live dict of every base index record, which the caller then re-indexed into
    a second `records_by_label` dict; both stayed pinned across the whole
    `_copy_live_tree` phase. On the 109 G corpus that is 1.28 GB of record
    dicts plus 0.18 GB of extra slots, and it is the residency that puts the
    chained leg over an 8 GiB host.

    Here the record bytes go to an arena that is `mkstemp`ed and immediately
    unlinked, and only the sorted key blob (~111 MB) and two offset arrays
    (16 B/entry) stay resident. Lookup is a binary search over the offset
    array followed by one `pread` and one `json.loads` of the same line the
    validator already checked, so `get()` returns a dict equal to the one the
    resident map returned.

    Append order is the index file's own strictly increasing
    `(root_index, relative)` order, which `validate_snapshot_custody` enforces
    before calling us, so nothing here ever sorts. Keys are compared as UTF-8
    (`surrogatepass`); UTF-8 is order-preserving over the whole code-point
    range, so byte order is exactly the `str` order the index was sealed in.

    The arena is complete and read-only *before* `require_capacity` runs, and
    never grows afterwards, so its blocks are already missing from the
    `statvfs` the capacity gate reads. That discharges the prior round's
    spill-arena MUSTFIX by construction rather than by an estimate — but only
    when the spill dir shares the capture's filesystem. When it does not (an
    operator points `BULKLOAD_SPILL_DIR` at another volume, or `TMPDIR` is the
    boot volume while the capture is on an external disk) nothing charges the
    arena against the volume that actually holds it. What that leaves is a
    fail-*closed* refusal rather than an unnoticed overrun: every path that
    touches the arena converts `OSError` to `BulkloadError`, so a spill volume
    that fills refuses the capture and tears down the partial, exactly as a
    full capture volume already does.
    """

    def __init__(self, labels: Sequence[str], *, spill_dir: Path | None) -> None:
        self._labels = list(labels)
        self._keys = bytearray()
        self._key_offsets = array("Q", [0])
        self._record_offsets = array("Q", [0])
        self._ranges: list[list[int]] = [[0, 0] for _ in self._labels]
        self._resident: list[dict[str, Any]] = []
        self._arena: Any = None
        self._fd = -1
        self._offset = 0
        if spill_dir is not None:
            # `validate_snapshot_custody` promises a `BulkloadError` on every
            # refusal, and it is constructed outside that promise's `try`. An
            # unwritable or full spill volume used to escape as a raw `OSError`
            # — past the fail-closed fence, and past `_capture_live_snapshot`'s
            # `finally: base_index.close()`, which never saw an instance to
            # close. Counterexample: `test_an_unusable_spill_dir_fails_closed`.
            descriptor = -1
            try:
                descriptor, path = tempfile.mkstemp(
                    prefix="bulkload-base-records-", dir=os.fspath(spill_dir)
                )
                os.unlink(path)
                self._arena = os.fdopen(descriptor, "wb", SNAPSHOT_INDEX_BUFFER_BYTES)
            except OSError as error:
                if descriptor >= 0 and self._arena is None:
                    os.close(descriptor)
                raise BulkloadError(
                    f"base record spill arena cannot be opened: {error}"
                ) from error
            self._fd = descriptor

    @property
    def spilled(self) -> bool:
        return self._arena is not None

    def append(self, root_index: int, relative: str, line: bytes) -> None:
        index = len(self._key_offsets) - 1
        span = self._ranges[root_index]
        if span[1] == span[0]:
            span[0] = index
        span[1] = index + 1
        self._keys += relative.encode("utf-8", "surrogatepass")
        self._key_offsets.append(len(self._keys))
        if self._arena is None:
            self._resident.append(json.loads(line))
        else:
            # Buffered, so a full spill volume surfaces here or in `seal`.
            try:
                self._arena.write(line)
            except OSError as error:
                raise BulkloadError(
                    f"base record spill arena cannot be written: {error}"
                ) from error
        self._offset += len(line)
        self._record_offsets.append(self._offset)

    def seal(self) -> None:
        if self._arena is not None:
            try:
                self._arena.flush()
            except OSError as error:
                self.close()
                raise BulkloadError(
                    f"base record spill arena cannot be written: {error}"
                ) from error

    def probe(self, low: int, high: int, relative: str) -> dict[str, Any] | None:
        wanted = relative.encode("utf-8", "surrogatepass")
        offsets = self._key_offsets
        keys = self._keys
        while low < high:
            middle = (low + high) // 2
            found = bytes(keys[offsets[middle] : offsets[middle + 1]])
            if found == wanted:
                return self._record(middle)
            if found < wanted:
                low = middle + 1
            else:
                high = middle
        return None

    def _record(self, index: int) -> dict[str, Any]:
        if self._arena is None:
            return self._resident[index]
        start = self._record_offsets[index]
        try:
            line = os.pread(self._fd, self._record_offsets[index + 1] - start, start)
        except OSError as error:
            raise BulkloadError(
                f"base record spill arena cannot be read: {error}"
            ) from error
        return json.loads(line)

    def by_label(self, label: str) -> _RootRecords:
        # A duplicate label is refused a few lines downstream of our only
        # caller; until then the last root wins, exactly as the old
        # `records_by_label[label][relative] = record` overwrite did.
        for index in range(len(self._labels) - 1, -1, -1):
            if self._labels[index] == label:
                return _RootRecords(self, *self._ranges[index])
        return _RootRecords(self, 0, 0)

    def close(self) -> None:
        # Called from `finally` on the failure path, and by `seal` after a
        # write error, so it must never raise: `close()` flushes, and flushing
        # a dirty buffer to the volume that just refused the write would raise
        # a second time and mask whatever the capture actually died of. The
        # arena is unlinked and its bytes are worthless once the capture is
        # failing, so dropping them is the correct loss.
        arena, self._arena = self._arena, None
        if arena is not None:
            with suppress(OSError):
                arena.close()
        self._fd = -1

    def __len__(self) -> int:
        return len(self._key_offsets) - 1

    def __iter__(self) -> Iterable[tuple[str, str]]:
        for root_index, (low, high) in enumerate(self._ranges):
            for index in range(low, high):
                relative = bytes(
                    self._keys[self._key_offsets[index] : self._key_offsets[index + 1]]
                ).decode("utf-8", "surrogatepass")
                yield self._labels[root_index], relative

    def __eq__(self, other: object) -> bool:
        if not isinstance(other, _BaseRecordMap):
            return NotImplemented
        return self.materialize() == other.materialize()

    __hash__ = None  # type: ignore[assignment]

    def materialize(self) -> dict[tuple[str, str], dict[str, Any]]:
        """The old return shape. Test and comparison surface only."""
        result: dict[tuple[str, str], dict[str, Any]] = {}
        for root_index, (low, high) in enumerate(self._ranges):
            for index in range(low, high):
                relative = bytes(
                    self._keys[self._key_offsets[index] : self._key_offsets[index + 1]]
                ).decode("utf-8", "surrogatepass")
                result[self._labels[root_index], relative] = self._record(index)
        return result


def validate_snapshot_custody(
    snapshot: dict[str, Any],
    *,
    mirror: Path | None = None,
    required_paths: set[Path] | None = None,
    collect_records: bool = False,
    payload_custody: str = "full",
    spill_dir: Path | None = None,
) -> _BaseRecordMap:
    """Reopen a seal/index and either all payloads or an explicit stage subset.

    `payload_custody` narrows only the per-payload re-derivation, and only for
    an already-sealed base. It is a separate knob from `required_paths`, which
    also disables the snapshot root's 0o700 check, the top-level namespace
    check, the declared-roots equality and the namespace digest — the
    anti-planting perimeter. Under "sealed" that whole perimeter stays on, and
    so does the read-back of the index file itself.
    """
    if payload_custody not in BASE_CUSTODY_MODES:
        raise BulkloadError("snapshot payload custody mode is unsupported")
    if payload_custody != "full" and required_paths is not None:
        raise BulkloadError("sealed payload custody cannot subset required paths")

    def actual(raw: str) -> Path:
        path = Path(raw)
        if not path.is_absolute():
            raise BulkloadError("snapshot custody path is not absolute")
        return path if mirror is None else mirror.joinpath(*path.parts[1:])

    require_digest(snapshot, "seal_sha256")
    seal_path = actual(snapshot["seal_path"])
    index_path = actual(snapshot["index_path"])
    try:
        seal_info = seal_path.stat(follow_symlinks=False)
        seal_bytes = seal_path.read_bytes()
        snapshot_root_info = seal_path.parent.stat(follow_symlinks=False)
        index_info = index_path.stat(follow_symlinks=False)
    except OSError as error:
        raise BulkloadError("live snapshot custody is unavailable") from error
    if (
        not stat.S_ISREG(seal_info.st_mode)
        or seal_bytes != canonical_bytes(snapshot) + b"\n"
        or not stat.S_ISDIR(snapshot_root_info.st_mode)
        or required_paths is None
        and stat.S_IMODE(snapshot_root_info.st_mode) != 0o700
        or not stat.S_ISREG(index_info.st_mode)
        or index_info.st_size > MAX_JSON_BYTES
        or sha256_file(index_path) != snapshot["index_sha256"]
    ):
        raise BulkloadError("live snapshot custody seal or index differs")
    actual_roots = [actual(item["snapshot"]) for item in snapshot["roots"]]
    original_roots = [Path(item["snapshot"]) for item in snapshot["roots"]]
    if len({os.fspath(path) for path in actual_roots}) != len(actual_roots):
        raise BulkloadError("live snapshot custody roots are not unique")
    for root in actual_roots:
        if not _within(root, seal_path.parent):
            raise BulkloadError("live snapshot payload escapes custody root")
    required = (
        None
        if required_paths is None
        else {Path(os.path.abspath(os.fspath(path))) for path in required_paths}
    )
    # X3: the git root's sealed generation_sha256 is derived from the LIVE tree,
    # not from the snapshot copy, so nothing anywhere ever compares the git
    # snapshot copy's bytes to the live git tree. This re-derivation is the only
    # thing that holds the git payload to its seal, and it runs under every
    # custody mode. SQLite payloads are always re-derived too: a seal cannot
    # carry WAL-blind freshness.
    always_roots: set[int] = set()
    always_relatives: dict[int, set[str]] = {}
    for root_index, root in enumerate(snapshot["roots"]):
        if root["label"] == "git":
            always_roots.add(root_index)
            continue
        relatives: set[str] = set()
        for entry in root["sqlite"]:
            relative_path = entry["relative_path"]
            relatives.add(relative_path)
            relatives.update(relative_path + suffix for suffix in SQLITE_SIDECARS)
        always_relatives[root_index] = relatives
    seen: set[Path] = set()
    collected = _BaseRecordMap(
        [item["label"] for item in snapshot["roots"]],
        spill_dir=spill_dir if collect_records else None,
    )
    digest = hashlib.sha256()
    namespace_digest = hashlib.sha256()
    count = 0
    previous: tuple[int, str] | None = None
    try:
        with index_path.open("rb") as stream:
            for line in stream:
                if not line.endswith(b"\n") or len(line) > 64 * 1024:
                    raise BulkloadError("snapshot payload index line is malformed")
                digest.update(line)
                try:
                    record = json.loads(line)
                except (UnicodeDecodeError, json.JSONDecodeError) as error:
                    raise BulkloadError(
                        "snapshot payload index is malformed"
                    ) from error
                require_exact_keys(
                    record,
                    {
                        "destination_device",
                        "kind",
                        "method",
                        "mode",
                        "relative_path",
                        "root_index",
                        "sha256",
                        "size",
                        "source_device",
                    },
                    "snapshot payload index entry",
                )
                if canonical_bytes(record) + b"\n" != line:
                    raise BulkloadError("snapshot payload index is not canonical")
                root_index = record["root_index"]
                relative = record["relative_path"]
                if (
                    not isinstance(root_index, int)
                    or isinstance(root_index, bool)
                    or not 0 <= root_index < len(actual_roots)
                    or not isinstance(relative, str)
                ):
                    raise BulkloadError("snapshot payload index identity is invalid")
                if relative == ".":
                    path = actual_roots[root_index]
                    original_path = original_roots[root_index]
                else:
                    normalize_relative(relative)
                    path = ensure_safe_target(actual_roots[root_index], relative)
                    original_path = ensure_safe_target(
                        original_roots[root_index], relative
                    )
                key = (root_index, relative)
                if previous is not None and key <= previous:
                    raise BulkloadError("snapshot payload index order is invalid")
                previous = key
                namespace_digest.update(canonical_bytes(list(key)) + b"\0")
                if collect_records:
                    collected.append(root_index, relative, line)
                if required is None or original_path in required:
                    if (
                        payload_custody == "full"
                        or root_index in always_roots
                        or relative in always_relatives.get(root_index, frozenset())
                    ):
                        observed = _snapshot_index_record(
                            path,
                            root_index=root_index,
                            relative=relative,
                            transfer={
                                "destination_device": record["destination_device"],
                                "method": record["method"],
                                "source_device": record["source_device"],
                            },
                        )
                        if observed != record:
                            # Symlink permission bits are not portable across
                            # kernels: darwin lstat reports the link's real
                            # bits (default 0755) while linux fixes every
                            # symlink at 0777 and offers no lchmod. The link
                            # stays bound by kind, size, and the target-path
                            # digest; only the mode field is exempt, and only
                            # when both sides agree the entry is a symlink.
                            if not (
                                observed.get("kind") == "symlink"
                                and record.get("kind") == "symlink"
                                and {**observed, "mode": None}
                                == {**record, "mode": None}
                            ):
                                raise BulkloadError(
                                    "snapshot payload differs from sealed index"
                                )
                    seen.add(original_path)
                count += 1
    except OSError as error:
        collected.close()
        raise BulkloadError("snapshot payload index cannot be read") from error
    except BaseException:
        collected.close()
        raise
    observed_namespace = hashlib.sha256()
    observed_count = 0
    if required is None:
        top_level = {item.name for item in seal_path.parent.iterdir()}
        if top_level != {"roots", "snapshot-index.jsonl", "snapshot-seal.json"}:
            raise BulkloadError("live snapshot top-level namespace differs")
        roots_parent = seal_path.parent / "roots"
        roots_info = roots_parent.stat(follow_symlinks=False)
        if not stat.S_ISDIR(roots_info.st_mode) or {
            item.name for item in roots_parent.iterdir()
        } != {root["label"] for root in snapshot["roots"]}:
            raise BulkloadError("live snapshot declared roots differ")
        for root_index, root in enumerate(actual_roots):
            for relative, _ in _snapshot_namespace(root):
                observed_namespace.update(
                    canonical_bytes([root_index, relative]) + b"\0"
                )
                observed_count += 1
    if (
        count != snapshot["index_entries"]
        or digest.hexdigest() != snapshot["index_sha256"]
        or required is not None
        and seen != required
        or required is None
        and (
            observed_count != count
            or observed_namespace.hexdigest() != namespace_digest.hexdigest()
        )
    ):
        collected.close()
        raise BulkloadError("snapshot payload index count or digest differs")
    collected.seal()
    return collected


def validate_live_snapshot_generation(
    snapshot: dict[str, Any], *, passes: int = 2
) -> None:
    """Fence final transport against any source mutation after snapshot B.

    `passes` is the number of full-corpus re-derivations. It defaults to 2
    because a single pass is not atomic: `epoch()` walks the roots in order,
    so a write that lands after its own path has been digested but before the
    pass ends is invisible to that pass and only visible to the next. The
    second pass is therefore a wider straggler window, not a redundant read.
    Only a fence that is strictly dominated by a later full-strength fence
    over the same snapshot may ask for `passes=1`.
    """
    if not isinstance(passes, int) or isinstance(passes, bool) or passes < 1:
        raise BulkloadError("live generation fence requires at least one pass")
    expected_rows = []
    for root in snapshot["roots"]:
        expected_rows.append(
            {
                "generation_sha256": root["generation_sha256"],
                "label": root["label"],
                "sqlite": root["sqlite"],
            }
        )
    expected = sha256_bytes(
        canonical_bytes(
            {
                "declarations": snapshot["declarations"],
                "git_generation_sha256": snapshot["git_generation_sha256"],
                "roots": expected_rows,
            }
        )
    )

    def epoch() -> str:
        declarations = []
        for declaration in snapshot["declarations"]:
            logical = Path(declaration["logical"])
            if declaration["kind"] == "seat-file":
                try:
                    info = logical.stat(follow_symlinks=False)
                except FileNotFoundError:
                    observed = (logical, logical, None, False)
                else:
                    if not stat.S_ISREG(info.st_mode):
                        raise BulkloadError("declared file seat changed type after B")
                    observed = (logical, resolve_real(logical), None, True)
            else:
                observed = _declared_root(logical, allow_absent=True)
            declarations.append(
                {
                    "backing": os.fspath(observed[1]),
                    "exists": observed[3],
                    "kind": declaration["kind"],
                    "link": observed[2],
                    "logical": os.fspath(observed[0]),
                    "name": declaration["name"],
                }
            )
        rows = []
        git_generation = None
        for root in snapshot["roots"]:
            live = Path(root["live"])
            sqlite_rows = []
            for sqlite in root["sqlite"]:
                logical = sqlite_catalog(
                    live / sqlite["relative_path"],
                    max_rows=snapshot["max_sqlite_rows"],
                )
                sqlite_rows.append(
                    {
                        "logical_sha256": logical["logical_sha256"],
                        "relative_path": sqlite["relative_path"],
                    }
                )
            rows.append(
                {
                    "generation_sha256": _tree_generation(
                        live,
                        provider=root["provider"],
                        exclusions=root["exclusions"],
                    ),
                    "label": root["label"],
                    "sqlite": sqlite_rows,
                }
            )
            if root["label"] == "git":
                git_generation = _git_live_generation(live)
        return sha256_bytes(
            canonical_bytes(
                {
                    "declarations": declarations,
                    "git_generation_sha256": git_generation,
                    "roots": rows,
                }
            )
        )

    # Each epoch re-derives the live generation and compares it to the sealed
    # expectation. Two passes are the default because the walk is sequential
    # and therefore not atomic: `first == expected` with `second != expected`
    # is reachable whenever a straggler write lands mid-walk, behind the
    # cursor. The only term that was genuinely dead in the original fence is
    # `first != second`, which the other two comparisons already imply.
    with phase_timing("validate"):
        for _ in range(passes):
            if epoch() != expected:
                raise BulkloadError("live source changed after immutable snapshot B")


def _reverse_snapshot_path(path: str | Path, roots: Sequence[dict[str, str]]) -> str:
    candidate = Path(path)
    for binding in sorted(
        roots, key=lambda item: len(Path(item["snapshot"]).parts), reverse=True
    ):
        try:
            relative = candidate.relative_to(binding["snapshot"])
        except ValueError:
            continue
        return os.fspath(Path(binding["live"]) / relative)
    raise BulkloadError(f"snapshot path has no live root binding: {candidate}")


def _remove_snapshot_partial(path: Path) -> None:
    """Remove only an exact capture-owned partial path."""
    if ".partial-" not in path.name or path.is_symlink():
        raise BulkloadError("refusing non-partial snapshot cleanup")
    if path.is_dir():
        shutil.rmtree(path)
    else:
        path.unlink(missing_ok=True)


def _remove_snapshot_published(path: Path, snapshot_id: str) -> None:
    """Remove only a final tree whose seal proves this failed capture owns it."""
    if path.is_symlink() or not path.is_dir():
        raise BulkloadError("refusing unsafe published snapshot cleanup")
    seal_path = path / "snapshot-seal.json"
    try:
        seal_value = json.loads(seal_path.read_bytes())
    except (OSError, json.JSONDecodeError) as error:
        raise BulkloadError("published snapshot ownership cannot be proved") from error
    if seal_value.get("snapshot_id") != snapshot_id:
        raise BulkloadError("published snapshot belongs to a different capture")
    shutil.rmtree(path)
    fsync_directory(path.parent)


def _live_git_authorities(git_root: Path) -> list[tuple[Path, Path]]:
    repositories, blockers = _discover_git_roots(git_root)
    if blockers:
        raise BulkloadError("Git namespace is not convergent for live snapshot")
    authorities: list[tuple[Path, Path]] = []
    for repository in repositories:
        if not _within(repository, git_root):
            raise BulkloadError("Git worktree authority is outside live snapshot root")
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
        except BulkloadError as error:
            if (
                str(error) == "Git inspection command failed (rev-parse)"
                and not _gitfile_declares_authority(repository)
            ):
                # Canonical capture preserves this false discovery as opaque
                # bytes. The enclosing Git root is copied in full below, so it
                # remains inside immutable snapshot custody without becoming
                # Git control-file authority.
                continue
            raise
        if not _within(common, git_root):
            raise BulkloadError("Git common authority is outside live snapshot root")
        authorities.append((repository, common))
    return authorities


def _git_live_generation(git_root: Path) -> str:
    authorities: dict[str, Path] = {}
    for repository, common in _live_git_authorities(git_root):
        authorities.setdefault(os.fspath(common), repository)
    rows = []
    for common, repository in sorted(authorities.items()):
        worktrees = _parse_worktree_list(repository)
        observed_worktrees = []
        for worktree in worktrees:
            if worktree.get("bare"):
                continue
            path = Path(worktree["path"])
            index_text = (
                _git(
                    path, ["rev-parse", "--path-format=absolute", "--git-path", "index"]
                )
                .decode()
                .strip()
            )
            index = Path(index_text)
            observed_worktrees.append(
                {
                    "branch": worktree.get("branch"),
                    "detached": bool(worktree.get("detached")),
                    "head": worktree.get("head"),
                    "index_sha256": sha256_file(index) if index.exists() else None,
                    "locked": bool(worktree.get("locked")),
                    "path": os.fspath(path),
                    "prunable": bool(worktree.get("prunable")),
                }
            )
        rows.append(
            {
                "common": common,
                "refs": _parse_refs(repository),
                "worktrees": observed_worktrees,
            }
        )
    return sha256_bytes(canonical_bytes(rows))


def _within(path: Path, root: Path) -> bool:
    try:
        path.relative_to(root)
    except ValueError:
        return False
    return True


def _git_snapshot_authorities(git_root: Path) -> tuple[list[Path], list[Path]]:
    """Bind every Git admin/worktree authority and the exact link files to rewrite."""
    authorities = _live_git_authorities(git_root)
    repositories = [repository for repository, _ in authorities]
    controls: set[Path] = set()
    for repository, _common in authorities:
        git_entry = repository / ".git"
        if git_entry.is_file() and _gitfile_declares_authority(repository):
            controls.add(git_entry)
        for worktree in _parse_worktree_list(repository):
            if worktree.get("bare"):
                continue
            worktree_path = resolve_real(Path(worktree["path"]))
            if not _within(worktree_path, git_root):
                raise BulkloadError("Git linked worktree is outside live snapshot root")
            git_dir = resolve_real(
                Path(
                    _git(
                        worktree_path,
                        ["rev-parse", "--path-format=absolute", "--absolute-git-dir"],
                    )
                    .decode()
                    .strip()
                )
            )
            index = resolve_real(
                Path(
                    _git(
                        worktree_path,
                        ["rev-parse", "--path-format=absolute", "--git-path", "index"],
                    )
                    .decode()
                    .strip()
                ),
                must_exist=False,
            )
            if not _within(git_dir, git_root) or not _within(index, git_root):
                raise BulkloadError("Git admin authority is outside live snapshot root")
            gitdir_control = git_dir / "gitdir"
            if gitdir_control.is_file():
                controls.add(gitdir_control)
    return repositories, sorted(controls, key=os.fspath)


def _rewrite_git_snapshot_links(
    controls: Sequence[Path], roots: Sequence[dict[str, str]]
) -> None:
    """Rewrite only parsed Git control files, never same-named user files."""
    for live_path in controls:
        path = _snapshot_path(live_path, roots, label="snapshot")
        try:
            payload = path.read_bytes()
        except OSError as error:
            raise BulkloadError("cannot inspect snapshotted Git link") from error
        if not payload or len(payload) > 64 * 1024 or b"\0" in payload:
            raise BulkloadError("snapshotted Git link is malformed")
        newline = b"\n" if payload.endswith(b"\n") else b""
        raw_target = payload[:-1] if newline else payload
        prefix = b"gitdir: " if live_path.name == ".git" else b""
        if prefix and not raw_target.startswith(prefix):
            raise BulkloadError("snapshotted Git file authority is malformed")
        raw_value = raw_target[len(prefix) :]
        try:
            value = os.fsdecode(raw_value)
        except UnicodeDecodeError as error:
            raise BulkloadError("snapshotted Git link is not portable") from error
        declared = Path(value)
        live_target = resolve_real(
            declared if declared.is_absolute() else live_path.parent / declared
        )
        snapshot_target = _snapshot_path(live_target, roots, label="snapshot")
        rewritten = prefix + os.fsencode(snapshot_target) + newline
        if rewritten == payload:
            continue
        with path.open("wb", buffering=0) as stream:
            stream.write(rewritten)
            os.fsync(stream.fileno())
        os.chmod(path, 0o600)
        fsync_directory(path.parent)


def _rewrite_catalog_to_live(
    catalog: dict[str, Any],
    *,
    roots: Sequence[dict[str, str]],
    original_path_map: list[dict[str, str]],
    home: Path,
    git_logical: Path,
    git_backing: Path,
    git_root_link: dict[str, Any] | None,
    provider_bindings: dict[str, tuple[Path, Path, dict[str, Any] | None, bool]],
    seat_bindings: dict[str, tuple[Path, Path, dict[str, Any] | None, bool]],
    role: str,
) -> None:
    catalog["path_map"] = original_path_map
    catalog["root_bindings"] = {
        "destination_git_root": os.fspath(git_backing)
        if role == "destination"
        else translate_path(git_logical, original_path_map),
        "destination_home": os.fspath(home)
        if role == "destination"
        else translate_path(home, original_path_map),
        "git_logical_root": os.fspath(git_logical),
        "git_root": os.fspath(git_backing),
        "git_root_link": git_root_link,
        "home": os.fspath(home),
    }
    for provider in catalog["providers"]:
        logical, backing, link, exists = provider_bindings[provider["name"]]
        provider.update(
            {
                "destination_path": os.fspath(backing)
                if role == "destination"
                else translate_path(logical, original_path_map),
                "exists": exists,
                "logical_path": os.fspath(logical),
                "path": os.fspath(backing),
                "root_link": link,
            }
        )
    for seat in catalog["seats"]:
        logical, backing, link, exists = seat_bindings[seat["name"]]
        seat.update(
            {
                "destination_path": os.fspath(logical)
                if role == "destination"
                else translate_path(logical, original_path_map),
                "exists": exists,
                "logical_path": os.fspath(logical),
                "path": os.fspath(
                    backing.parent if seat["root_kind"] == "file" else backing
                ),
                "root_link": link,
            }
        )
    for workspace in catalog["git_workspaces"]:
        live_workspace_path = _reverse_snapshot_path(workspace["path"], roots)
        workspace["common_git_dir"] = _reverse_snapshot_path(
            workspace["common_git_dir"], roots
        )
        workspace["path"] = live_workspace_path
        workspace["destination_path"] = (
            live_workspace_path
            if role == "destination"
            else translate_path(live_workspace_path, original_path_map)
        )
        for worktree in workspace["worktrees"]:
            worktree["git_dir"] = _reverse_snapshot_path(worktree["git_dir"], roots)
            live_worktree_path = _reverse_snapshot_path(worktree["path"], roots)
            worktree["path"] = live_worktree_path
            worktree["destination_path"] = (
                live_worktree_path
                if role == "destination"
                else translate_path(live_worktree_path, original_path_map)
            )
            worktree["index"]["path"] = _reverse_snapshot_path(
                worktree["index"]["path"], roots
            )
        workspace["workspace_id"] = sha256_bytes(
            canonical_bytes(
                {
                    "destination_path": workspace["destination_path"],
                    "object_format": workspace["object_format"],
                    "remote_names": [remote["name"] for remote in workspace["remotes"]],
                }
            )
        )


def _capture_live_snapshot(
    *,
    role: str,
    home: Path,
    git_root: Path,
    codex_root: Path | None,
    claude_root: Path | None,
    pi_root: Path | None,
    seats: Sequence[tuple[str, Path] | tuple[str, Path, str]],
    path_map: list[dict[str, str]],
    managed_exclusions: Sequence[tuple[str, str]],
    rsync_path: Path | None,
    max_files: int,
    max_bytes: int,
    max_sqlite_rows: int,
    snapshot_root: Path,
    snapshot_reserve_bytes: int,
    snapshot_base_seal: Path | None,
    jobs: int | None = None,
    base_custody: str = "full",
) -> dict[str, Any]:
    snapshot_id = new_id()
    snapshot_root = Path(os.path.abspath(os.fspath(snapshot_root)))
    partial = snapshot_root.parent / f".{snapshot_root.name}.partial-{snapshot_id}"
    if snapshot_root.exists() or snapshot_root.is_symlink() or partial.exists():
        raise BulkloadError("live snapshot custody path already exists")
    git_logical, git_backing, git_link, _ = _declared_root(git_root, allow_absent=False)
    provider_arguments = {
        "codex": codex_root or home / ".codex",
        "claude": claude_root or home / ".claude",
        "pi": pi_root or home / ".pi" / "agent",
    }
    provider_bindings = {
        name: _declared_root(path, allow_absent=True)
        for name, path in provider_arguments.items()
    }
    seat_bindings: dict[str, tuple[Path, Path, dict[str, Any] | None, bool]] = {}
    seat_kinds: dict[str, str] = {}
    for declaration in seats:
        name, path = declaration[:2]
        kind = declaration[2] if len(declaration) == 3 else "directory"
        if (
            not re.fullmatch(r"[a-z][a-z0-9_-]{0,63}", name)
            or name in seat_bindings
            or kind not in {"directory", "file"}
        ):
            raise BulkloadError("live snapshot mutable-seat declaration is invalid")
        seat_kinds[name] = kind
        if kind == "file":
            logical = Path(os.path.abspath(os.fspath(path.expanduser())))
            try:
                info = logical.stat(follow_symlinks=False)
            except FileNotFoundError:
                seat_bindings[name] = (logical, logical, None, False)
                continue
            if not stat.S_ISREG(info.st_mode):
                raise BulkloadError("file seat is not an exact regular file")
            seat_bindings[name] = (logical, resolve_real(logical), None, True)
        else:
            seat_bindings[name] = _declared_root(path, allow_absent=True)
    provider_policy = canonical_provider_policy(managed_exclusions)
    exclusions = defaultdict(list)
    for item in provider_policy["managed_exclusions"]:
        exclusions[item["provider"]].append(item["relative_path"])
    declarations = [
        {
            "backing": os.fspath(git_backing),
            "exists": True,
            "kind": "git",
            "link": git_link,
            "logical": os.fspath(git_logical),
            "name": "git",
        }
    ]
    declarations.extend(
        {
            "backing": os.fspath(binding[1]),
            "exists": binding[3],
            "kind": "provider",
            "link": binding[2],
            "logical": os.fspath(binding[0]),
            "name": name,
        }
        for name, binding in provider_bindings.items()
    )
    declarations.extend(
        {
            "backing": os.fspath(binding[1]),
            "exists": binding[3],
            "kind": f"seat-{seat_kinds[name]}",
            "link": binding[2],
            "logical": os.fspath(binding[0]),
            "name": name,
        }
        for name, binding in seat_bindings.items()
    )
    descriptors: list[tuple[str, Path, str | None, Sequence[str]]] = [
        ("git", git_backing, None, ())
    ]
    descriptors.extend(
        (f"provider-{name}", binding[1], name, exclusions[name])
        for name, binding in provider_bindings.items()
        if binding[3]
    )
    descriptors.extend(
        (f"seat-{name}", binding[1], None, ())
        for name, binding in seat_bindings.items()
        if binding[3]
    )
    live_roots = [item[1] for item in descriptors]
    for index, root in enumerate(live_roots):
        root_info = root.stat(follow_symlinks=False)
        if not (stat.S_ISREG(root_info.st_mode) or stat.S_ISDIR(root_info.st_mode)):
            raise BulkloadError(f"live snapshot root has unsupported type: {root}")
        for other in live_roots[index + 1 :]:
            if _within(root, other) or _within(other, root):
                raise BulkloadError("live snapshot roots overlap or alias")
    assert_no_overlap(snapshot_root, live_roots, "live snapshot root")
    assert_no_overlap(partial, live_roots, "live snapshot partial root")
    _, git_controls = _git_snapshot_authorities(git_backing)
    roots: list[dict[str, str]] = []
    for label, live, provider, excluded in descriptors:
        roots.append(
            {
                "exclusions": list(excluded),
                "generation_sha256": None,
                "label": label,
                "live": os.fspath(live),
                "provider": provider,
                "snapshot": os.fspath(snapshot_root / "roots" / label),
                "sqlite": [],
            }
        )
    work_roots = [
        {
            "live": binding["live"],
            "snapshot": os.fspath(
                partial / Path(binding["snapshot"]).relative_to(snapshot_root)
            ),
        }
        for binding in roots
    ]
    base_snapshot: dict[str, Any] | None = None
    base_paths: list[Path | None] = [None] * len(roots)
    base_records: list[Any] = [{} for _ in roots]
    base_index: _BaseRecordMap | None = None
    if snapshot_base_seal is not None:
        base_snapshot = read_json(snapshot_base_seal)
        if base_snapshot.get("mode") != LIVE_SNAPSHOT_MODE:
            raise BulkloadError("snapshot base seal is not immutable-live custody")
        spill_dir = _base_record_spill_dir(snapshot_root, partial)
        with phase_timing("base-custody"):
            base_index = validate_snapshot_custody(
                base_snapshot,
                collect_records=True,
                payload_custody=base_custody,
                spill_dir=spill_dir,
            )
        try:
            base_root = Path(base_snapshot["seal_path"]).parent
            assert_no_overlap(snapshot_root, [base_root], "live snapshot root")
            assert_no_overlap(partial, [base_root], "live snapshot partial root")
            base_by_label = {item["label"]: item for item in base_snapshot["roots"]}
            if len(base_by_label) != len(base_snapshot["roots"]):
                raise BulkloadError("snapshot base root labels are not unique")
            for index, root in enumerate(roots):
                try:
                    base_root = base_by_label[root["label"]]
                except KeyError as error:
                    raise BulkloadError(
                        "snapshot base lacks a required root"
                    ) from error
                if any(
                    base_root[key] != root[key]
                    for key in ("live", "provider", "exclusions")
                ):
                    raise BulkloadError("snapshot base root contract differs")
                base_paths[index] = Path(base_root["snapshot"])
                base_records[index] = base_index.by_label(root["label"])
        except BaseException:
            base_index.close()
            raise
    methods: dict[str, int] = defaultdict(int)
    transfer_ledgers: list[dict[str, dict[str, int | str]]] = []
    censuses: list[tuple[str, int]] = []
    durable_makedirs(snapshot_root.parent)
    for label, live, provider, excluded in descriptors:
        with phase_timing("census", label) as sample:
            observed = _tree_census(live, provider=provider, exclusions=excluded)
            sample.files = observed[1]
        censuses.append(observed)
    charged_bytes = 0
    for index, (label, live, provider, excluded) in enumerate(descriptors):
        with phase_timing("charge", label) as sample:
            charge = _snapshot_delta_charge(
                live,
                base_paths[index],
                provider=provider,
                exclusions=excluded,
                base_records=base_records[index],
            )
            sample.bytes = charge
        charged_bytes += charge
    capacity = require_capacity(
        snapshot_root.parent,
        charged_bytes=charged_bytes,
        reserve_bytes=snapshot_reserve_bytes,
    )
    published = False
    try:
        durable_makedirs(partial)
        git_generation = _git_live_generation(git_backing)
        git_tree_generation = _tree_generation(
            git_backing, provider=None, exclusions=()
        )
        for index, (label, live, provider, excluded) in enumerate(descriptors):
            work_target = Path(work_roots[index]["snapshot"])
            with phase_timing("copy", label) as sample:
                observed_methods, observed_ledger = _copy_live_tree(
                    live,
                    work_target,
                    provider=provider,
                    exclusions=excluded,
                    max_sqlite_rows=max_sqlite_rows,
                    base=base_paths[index],
                    base_records=base_records[index],
                )
                sample.files = len(observed_ledger)
            transfer_ledgers.append(observed_ledger)
            for method, count in observed_methods.items():
                methods[method] += count
            after = _tree_census(live, provider=provider, exclusions=excluded)
            if censuses[index][0] != after[0]:
                raise BulkloadError(f"live snapshot path set changed: {live}")
        _rewrite_git_snapshot_links(git_controls, work_roots)
        if _git_live_generation(git_backing) != git_generation:
            raise BulkloadError("Git authority changed during live snapshot")
        if (
            _tree_generation(git_backing, provider=None, exclusions=())
            != git_tree_generation
        ):
            raise BulkloadError("Git bytes changed during live snapshot")
        for index, (label, _, provider, excluded) in enumerate(descriptors):
            with phase_timing("digest", label):
                roots[index]["generation_sha256"] = (
                    git_tree_generation
                    if roots[index]["label"] == "git"
                    else _tree_generation(
                        Path(work_roots[index]["snapshot"]),
                        provider=provider,
                        exclusions=excluded,
                    )
                )
        augmented_map = canonical_path_map(
            [
                *[(item["source"], item["destination"]) for item in path_map],
                *[
                    (
                        binding["snapshot"],
                        translate_path(binding["live"], path_map),
                    )
                    for binding in work_roots
                    if any(
                        Path(binding["live"]) == Path(item["source"])
                        or Path(item["source"]) in Path(binding["live"]).parents
                        for item in path_map
                    )
                ],
            ]
        )
        snapshot_seats = []
        for declaration in seats:
            name = declaration[0]
            binding = seat_bindings[name]
            snapshot_path = _snapshot_path(binding[1], work_roots, label="snapshot")
            snapshot_seats.append((name, snapshot_path, seat_kinds[name]))
        with phase_timing("catalog"):
            captured = capture_agent_state(
                role=role,
                home=home,
                git_root=_snapshot_path(git_backing, work_roots, label="snapshot"),
                codex_root=_snapshot_path(
                    provider_bindings["codex"][1], work_roots, label="snapshot"
                )
                if provider_bindings["codex"][3]
                else provider_bindings["codex"][0],
                claude_root=_snapshot_path(
                    provider_bindings["claude"][1], work_roots, label="snapshot"
                )
                if provider_bindings["claude"][3]
                else provider_bindings["claude"][0],
                pi_root=_snapshot_path(
                    provider_bindings["pi"][1], work_roots, label="snapshot"
                )
                if provider_bindings["pi"][3]
                else provider_bindings["pi"][0],
                seats=snapshot_seats,
                path_map=augmented_map,
                writers_quiesced=True,
                managed_exclusions=managed_exclusions,
                rsync_path=rsync_path,
                max_files=max_files,
                max_bytes=max_bytes,
                max_sqlite_rows=max_sqlite_rows,
                jobs=jobs,
            )
        catalog = captured["catalog"]
        _rewrite_catalog_to_live(
            catalog,
            roots=work_roots,
            original_path_map=path_map,
            home=home,
            git_logical=git_logical,
            git_backing=git_backing,
            git_root_link=git_link,
            provider_bindings=provider_bindings,
            seat_bindings=seat_bindings,
            role=role,
        )
        providers_by_name = {
            provider["name"]: provider for provider in catalog["providers"]
        }
        for root in roots:
            provider_name = root["provider"]
            if provider_name is None:
                continue
            root["sqlite"] = [
                {
                    "logical_sha256": item["logical"]["logical_sha256"],
                    "relative_path": item["relative_path"],
                }
                for item in providers_by_name[provider_name]["items"]
                if item["classification"] == "sqlite"
            ]
        partial_index = partial / "snapshot-index.jsonl"
        with phase_timing("seal") as sample:
            index_sha256, index_entries = _write_snapshot_index(
                partial_index, work_roots, transfer_ledgers
            )
            sample.files = index_entries
        index_path = snapshot_root / "snapshot-index.jsonl"
        snapshot = {
            "base": None
            if base_snapshot is None
            else {
                "seal_path": base_snapshot["seal_path"],
                "seal_sha256": base_snapshot["seal_sha256"],
                "snapshot_id": base_snapshot["snapshot_id"],
            },
            "capacity": capacity,
            "contract_sha256": _snapshot_contract(catalog),
            "declarations": declarations,
            "git_generation_sha256": git_generation,
            "index_entries": index_entries,
            "index_path": os.fspath(index_path),
            "index_sha256": index_sha256,
            "inventory_sha256": sha256_bytes(canonical_bytes(catalog)),
            "max_sqlite_rows": max_sqlite_rows,
            "methods": dict(sorted(methods.items())),
            "mode": LIVE_SNAPSHOT_MODE,
            "roots": roots,
            "snapshot_id": snapshot_id,
        }
        seal_path = snapshot_root / "snapshot-seal.json"
        snapshot = seal({**snapshot, "seal_path": os.fspath(seal_path)}, "seal_sha256")
        _witness_epoch(snapshot)
        partial_seal = partial / "snapshot-seal.json"
        atomic_write_json(partial_seal, snapshot)
        require_digest(snapshot, "seal_sha256")
        if partial_seal.read_bytes() != canonical_bytes(snapshot) + b"\n":
            raise BulkloadError("live snapshot seal bytes did not persist")
        os.replace(partial, snapshot_root)
        published = True
        fsync_directory(snapshot_root.parent)
        catalog["snapshot"] = snapshot
        captured.update(
            {
                "capture_id": snapshot_id,
                "catalog_sha256": sha256_bytes(canonical_bytes(catalog)),
                "observed_at": utc_now(),
                "writers_quiesced": False,
            }
        )
        return seal(captured, "capture_sha256")
    except BaseException:
        if partial.exists() and not partial.is_symlink():
            _remove_snapshot_partial(partial)
        elif published and snapshot_root.exists() and not snapshot_root.is_symlink():
            _remove_snapshot_published(snapshot_root, snapshot_id)
        raise
    finally:
        if base_index is not None:
            base_index.close()


def _witness_epoch(snapshot: dict[str, Any]) -> None:
    """W2-5. Re-derive the live epoch at capture time and only report on it.

    Wave 3 wants to delete the chained B leg and lean on a single capture's
    own during-copy proof instead. That trade rests entirely on what a live
    witness costs on the real corpus, and nobody has ever measured it. This
    runs one, off by default, and no outcome of it can fail a capture.

    Two things the design that asked for this got wrong, and why this is not
    what it specified:

    * It proposed upgrading the post-copy `_tree_census` to `content=True` and
      comparing that to the sealed generation. Those two digests are taken over
      structurally different tuples — the census emits the full stat tuple
      including `st_dev`, `st_ino` and the timestamps, while `_tree_generation`
      emits `[IFMT, IMODE, size]` plus a content digest — so they can never be
      equal, for any tree, ever. The only correct witness is a third walk:
      `_tree_generation` over the live tree. "The walk is already paid" is
      therefore false; this costs a whole extra content pass.
    * `_tree_generation` sets `content_digest=None` and drops `size` for a live
      sqlite primary, so a witness built from it alone is blind to every byte
      of provider sqlite. `validate_live_snapshot_generation` covers that hole
      by folding `sqlite_catalog(...)["logical_sha256"]` per declared sqlite.

    Both corrections land on the same conclusion: the correct witness is the
    fence the engine already owns. So this calls it rather than building a
    second, weaker one — which also means the cost it measures is the real
    cost of the thing Wave 3 would have to run.

    `passes=1` is the documented exception, not a shortcut: an observation is
    strictly dominated by all six real fence sites, which still run at full
    strength on every path.

    The `except` below is deliberately `Exception` and not `BulkloadError`.
    The fence walks the *live* tree, and a live tree can move underneath it in
    ways it never converts: `_tree_census` reaches `root.stat()` and
    `sha256_file()` unwrapped, so a root that an external tool removes between
    the seal and this call raises a bare `FileNotFoundError`. That escaped into
    `_capture_live_snapshot`'s `except BaseException`, which tears down the
    partial and re-raises — i.e. an observation that is off by default and
    changes no digest could destroy a capture that had already sealed. The
    counterexample is `test_witness_survives_a_live_root_that_vanishes`.

    `BaseException` still propagates: `KeyboardInterrupt` and `SystemExit` are
    the operator's, not a divergence, and the capture must die on them.
    """
    if os.environ.get("BULKLOAD_WITNESS_EPOCH", "").strip().lower() not in {
        "1",
        "true",
        "yes",
        "on",
    }:
        return
    with phase_timing("witness"):
        try:
            validate_live_snapshot_generation(snapshot, passes=1)
            divergence = "none"
        except Exception as error:  # noqa: BLE001 — an observation may not fail a capture
            divergence = f"{type(error).__name__}: {error}"
        with suppress(Exception):
            sys.stderr.write(f"bulkload-witness divergence={divergence}\n")
            sys.stderr.flush()


def _snapshot_path(path: Path, roots: Sequence[dict[str, str]], *, label: str) -> Path:
    for binding in roots:
        live = Path(binding["live"])
        try:
            relative = path.relative_to(live)
        except ValueError:
            continue
        return Path(binding[label]) / relative
    raise BulkloadError(f"live snapshot has no root binding for {path}")


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
    snapshot_root: Path | None = None,
    snapshot_base_seal: Path | None = None,
    managed_exclusions: Sequence[tuple[str, str]] = (),
    rsync_path: Path | None = None,
    max_files: int = DEFAULT_MAX_FILES,
    max_bytes: int = DEFAULT_MAX_BYTES,
    max_sqlite_rows: int = DEFAULT_MAX_SQLITE_ROWS,
    snapshot_reserve_bytes: int = SNAPSHOT_RESERVE_BYTES,
    jobs: int | None = None,
    base_custody: str = "full",
) -> dict[str, Any]:
    if role not in {"source", "destination"}:
        raise BulkloadError("capture role must be source or destination")
    if jobs is not None and not 1 <= jobs <= MAX_CAPTURE_JOBS:
        raise BulkloadError("capture job count is out of range")
    if base_custody not in BASE_CUSTODY_MODES:
        raise BulkloadError("snapshot base custody mode is unsupported")
    if base_custody != "full" and snapshot_base_seal is None:
        raise BulkloadError("snapshot base custody mode requires a base seal")
    if not writers_quiesced and snapshot_root is not None:
        return _capture_live_snapshot(
            role=role,
            home=home,
            git_root=git_root,
            codex_root=codex_root,
            claude_root=claude_root,
            pi_root=pi_root,
            seats=seats,
            path_map=path_map,
            managed_exclusions=managed_exclusions,
            rsync_path=rsync_path,
            max_files=max_files,
            max_bytes=max_bytes,
            max_sqlite_rows=max_sqlite_rows,
            snapshot_root=snapshot_root,
            snapshot_reserve_bytes=snapshot_reserve_bytes,
            snapshot_base_seal=snapshot_base_seal,
            jobs=jobs,
            base_custody=base_custody,
        )
    if not writers_quiesced and snapshot_root is None:
        raise BulkloadError(
            "live agent-capture requires an explicit immutable snapshot root"
        )
    if writers_quiesced and snapshot_root is not None:
        raise BulkloadError("quiesced capture does not accept a live snapshot root")
    if snapshot_base_seal is not None:
        raise BulkloadError("snapshot base seal requires a live snapshot root")
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
    workspace_inputs = sorted(by_common.items(), key=lambda item: os.fspath(item[1]))
    nested_roots = set(discovered)

    def capture_workspace(
        item: tuple[Path, Path],
    ) -> tuple[
        Path,
        Path,
        tuple[dict[str, Any], list[dict[str, str]]]
        | _OpaqueGitFallback
        | BulkloadError,
    ]:
        common, representative = item
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
            return common, representative, (workspace, workspace_blockers)
        except (_OpaqueGitFallback, BulkloadError) as error:
            return common, representative, error

    workspace_workers = workspace_worker_count(jobs, len(workspace_inputs))
    if workspace_workers:
        with ThreadPoolExecutor(max_workers=workspace_workers) as pool:
            workspace_results = list(pool.map(capture_workspace, workspace_inputs))
    else:
        workspace_results = []
    workspaces: list[dict[str, Any]] = []
    for common, representative, result in workspace_results:
        if isinstance(result, tuple):
            workspace, workspace_blockers = result
            workspaces.append(workspace)
            blockers.extend(workspace_blockers)
        elif isinstance(result, _OpaqueGitFallback):
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
        else:
            blockers.append(
                {
                    "code": "git-workspace-capture-failed",
                    "path": os.fspath(representative),
                    "detail": str(result),
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
    catalog: dict[str, Any] = {
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
    capture_id = new_id()
    catalog["snapshot"] = None
    capture = {
        "capture_id": capture_id,
        "catalog": catalog,
        "catalog_sha256": sha256_bytes(canonical_bytes(catalog)),
        "complete": not blockers,
        "hostname": socket.gethostname(),
        "observed_at": utc_now(),
        "role": role,
        "schema": AGENT_CAPTURE_SCHEMA,
        "writers_quiesced": writers_quiesced,
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
            "snapshot",
            "transport",
        },
        "AgentCaptureV4 catalog",
    )
    if value.get("catalog_sha256") != sha256_bytes(canonical_bytes(catalog)):
        raise BulkloadError("AgentCaptureV4 catalog digest mismatch")
    snapshot = catalog["snapshot"]
    if value.get("writers_quiesced") is False:
        if not isinstance(snapshot, dict):
            raise BulkloadError("live AgentCaptureV4 lacks immutable snapshot custody")
        require_exact_keys(
            snapshot,
            {
                "base",
                "capacity",
                "contract_sha256",
                "declarations",
                "git_generation_sha256",
                "index_entries",
                "index_path",
                "index_sha256",
                "inventory_sha256",
                "max_sqlite_rows",
                "methods",
                "mode",
                "roots",
                "seal_path",
                "seal_sha256",
                "snapshot_id",
            },
            "AgentCaptureV4 live snapshot",
        )
        require_digest(snapshot, "seal_sha256")
        require_exact_keys(
            snapshot["capacity"],
            {
                "available_bytes",
                "charged_bytes",
                "free_bytes",
                "required_bytes",
                "reserve_bytes",
                "total_bytes",
            },
            "AgentCaptureV4 live snapshot capacity",
        )
        capacity = snapshot["capacity"]
        if (
            any(not isinstance(item, int) or item < 0 for item in capacity.values())
            or capacity["required_bytes"]
            != capacity["charged_bytes"] + capacity["reserve_bytes"]
            or capacity["available_bytes"] < capacity["required_bytes"]
        ):
            raise BulkloadError("AgentCaptureV4 live snapshot capacity is invalid")
        if (
            snapshot["mode"] != LIVE_SNAPSHOT_MODE
            or snapshot["snapshot_id"] != value["capture_id"]
            or snapshot["contract_sha256"] != _snapshot_contract(catalog)
            or snapshot["inventory_sha256"]
            != sha256_bytes(canonical_bytes({**catalog, "snapshot": None}))
            or not re.fullmatch(r"[0-9a-f]{64}", snapshot["git_generation_sha256"])
            or not isinstance(snapshot["index_entries"], int)
            or isinstance(snapshot["index_entries"], bool)
            or snapshot["index_entries"] < 1
            or not Path(snapshot["index_path"]).is_absolute()
            or not re.fullmatch(r"[0-9a-f]{64}", snapshot["index_sha256"])
            or not isinstance(snapshot["max_sqlite_rows"], int)
            or isinstance(snapshot["max_sqlite_rows"], bool)
            or snapshot["max_sqlite_rows"] < 1
        ):
            raise BulkloadError("AgentCaptureV4 live snapshot seal is invalid")
        if snapshot["base"] is not None:
            require_exact_keys(
                snapshot["base"],
                {"seal_path", "seal_sha256", "snapshot_id"},
                "AgentCaptureV4 live snapshot base",
            )
            if (
                not Path(snapshot["base"]["seal_path"]).is_absolute()
                or not re.fullmatch(r"[0-9a-f]{64}", snapshot["base"]["seal_sha256"])
                or not isinstance(snapshot["base"]["snapshot_id"], str)
            ):
                raise BulkloadError("AgentCaptureV4 live snapshot base is invalid")
        if not isinstance(snapshot["declarations"], list):
            raise BulkloadError("AgentCaptureV4 live declarations are invalid")
        declaration_names: set[tuple[str, str]] = set()
        for declaration in snapshot["declarations"]:
            require_exact_keys(
                declaration,
                {"backing", "exists", "kind", "link", "logical", "name"},
                "AgentCaptureV4 live declaration",
            )
            identity = (declaration["kind"], declaration["name"])
            if (
                identity in declaration_names
                or declaration["kind"]
                not in {"git", "provider", "seat-directory", "seat-file"}
                or not Path(declaration["logical"]).is_absolute()
                or not Path(declaration["backing"]).is_absolute()
                or not isinstance(declaration["exists"], bool)
            ):
                raise BulkloadError("AgentCaptureV4 live declaration is invalid")
            declaration_names.add(identity)
        for root in snapshot["roots"]:
            require_exact_keys(
                root,
                {
                    "exclusions",
                    "generation_sha256",
                    "label",
                    "live",
                    "provider",
                    "snapshot",
                    "sqlite",
                },
                "live snapshot root",
            )
            if (
                not Path(root["live"]).is_absolute()
                or not Path(root["snapshot"]).is_absolute()
            ):
                raise BulkloadError("live snapshot root binding is not absolute")
            if (
                not isinstance(root["label"], str)
                or root["provider"] not in {None, "codex", "claude", "pi"}
                or not isinstance(root["exclusions"], list)
                or root["exclusions"] != sorted(set(root["exclusions"]))
                or not re.fullmatch(r"[0-9a-f]{64}", root["generation_sha256"])
                or not isinstance(root["sqlite"], list)
            ):
                raise BulkloadError("live snapshot root generation is invalid")
            for sqlite in root["sqlite"]:
                require_exact_keys(
                    sqlite,
                    {"logical_sha256", "relative_path"},
                    "live snapshot SQLite generation",
                )
                normalize_relative(sqlite["relative_path"])
                if not re.fullmatch(r"[0-9a-f]{64}", sqlite["logical_sha256"]):
                    raise BulkloadError("live snapshot SQLite generation is invalid")
        if catalog["transport"]["hostname"] == socket.gethostname():
            validate_snapshot_custody(snapshot)
    elif value.get("writers_quiesced") is True:
        if snapshot is not None:
            raise BulkloadError("quiesced AgentCaptureV4 has live snapshot custody")
    else:
        raise BulkloadError("AgentCaptureV4 writer boundary is invalid")
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


def _catalog_path_identities(catalog: dict[str, Any]) -> set[str]:
    identities: set[str] = set()
    for item in catalog.get("non_git", []):
        identities.add(f"non-git:{item['relative_path']}")
    for provider in catalog.get("providers", []):
        for item in provider.get("items", []):
            identities.add(
                f"provider:{provider['name']}:{provider_item_identity(item)}"
            )
    for seat in catalog.get("seats", []):
        for item in seat.get("items", []):
            identities.add(f"seat:{seat['name']}:{item['identity']}")
    for workspace in catalog.get("git_workspaces", []):
        workspace_id = workspace["workspace_id"]
        identities.add(f"git:{workspace_id}")
        for item in workspace.get("object_files", []):
            identities.add(f"git-object:{workspace_id}:{item['relative_path']}")
        for worktree in workspace.get("worktrees", []):
            worktree_path = worktree["path"]
            identities.add(f"git-worktree:{workspace_id}:{worktree_path}")
            for item in worktree.get("files", []):
                identities.add(
                    f"git-file:{workspace_id}:{worktree_path}:{item['relative_path']}"
                )
            if worktree.get("index", {}).get("exists"):
                identities.add(f"git-index:{workspace_id}:{worktree_path}")
    return identities


def stable_capture_pair(
    first: dict[str, Any], second: dict[str, Any], *, role: str
) -> None:
    validate_agent_capture(first, expected_role=role)
    validate_agent_capture(second, expected_role=role)
    if first["capture_id"] == second["capture_id"]:
        raise BulkloadError(f"{role} A/B captures reuse one capture ID")
    if first["writers_quiesced"] != second["writers_quiesced"]:
        raise BulkloadError(f"{role} A/B captures use different writer boundaries")
    if first["writers_quiesced"]:
        if (
            first["catalog_sha256"] != second["catalog_sha256"]
            or first["catalog"] != second["catalog"]
        ):
            raise BulkloadError(f"{role} A/B captures are not byte-stable")
    else:
        first_snapshot = first["catalog"]["snapshot"]
        second_snapshot = second["catalog"]["snapshot"]
        if (
            first_snapshot["contract_sha256"] != second_snapshot["contract_sha256"]
            or first_snapshot["seal_sha256"] == second_snapshot["seal_sha256"]
            or second_snapshot["base"]
            != {
                "seal_path": first_snapshot["seal_path"],
                "seal_sha256": first_snapshot["seal_sha256"],
                "snapshot_id": first_snapshot["snapshot_id"],
            }
        ):
            raise BulkloadError(f"{role} A/B live snapshot contract is unstable")
        missing = _catalog_path_identities(first["catalog"]) - _catalog_path_identities(
            second["catalog"]
        )
        if missing:
            raise BulkloadError(f"{role} A/B live snapshot loses prior custody")
    if not first["complete"] or not second["complete"]:
        raise BulkloadError(f"{role} captures contain blockers")


# ---------------------------------------------------------------------------
# doctor: the read-only cross-kernel preflight
#
# Every check here answers a defect that was found only after bytes moved
# during the 2026-08-25..28 ceremony and that was discoverable before any of
# them did. The verb reads: it never writes inside a declared root, never
# signals a process, and never mutates the peer.
# ---------------------------------------------------------------------------


def path_identity(value: str | Path) -> str:
    """The cross-kernel comparison key for one path.

    APFS folds case; XFS compares bytes. Two spellings that differ only by
    case or by Unicode composition are one file on the source and two files on
    the destination. That is the class that killed materialize twice on
    2026-08-27 (an on-disk `gloriousflywheel.worktrees` against a git-pointer
    `GloriousFlywheel.worktrees`), and it is why this key only ever *reports*
    which spellings collapse and never decides custody.
    """
    return unicodedata.normalize("NFC", os.fspath(value)).casefold()


def _doctor_check(
    code: str,
    status: str,
    *,
    summary: str,
    remedy: str,
    expected: Any = None,
    observed: Any = None,
    findings: Sequence[dict[str, Any]] | None = None,
    finding_total: int | None = None,
    max_findings: int = DOCTOR_MAX_FINDINGS,
) -> dict[str, Any]:
    """One typed check: what was expected, what was observed, and every
    offending path the operator has to settle before capture.

    `finding_total` is how many offending paths the run actually saw, which
    is not `len(findings)` when the collector itself was bounded. It is
    carried separately so `finding_count` never under-reports a path that
    was dropped before it reached this list.
    """
    if status not in DOCTOR_STATUSES:
        raise BulkloadError(f"doctor check status is unsupported: {status}")
    items = list(findings or [])
    named = items[:max_findings]
    total = len(items) if finding_total is None else max(finding_total, len(items))
    return {
        "code": code,
        "expected": expected,
        "finding_count": total,
        "findings": named,
        "observed": observed,
        "remedy": remedy,
        "status": status,
        "summary": summary,
        "truncated": total > len(named),
    }


def _is_sha256(value: str) -> bool:
    """The exact acceptance `model.runtime_source_digest` applies."""
    return len(value) == 64 and all(
        character in "0123456789abcdef" for character in value
    )


class _DoctorFindings:
    """A findings list that stops growing but never stops counting.

    `--max-findings` used to bound serialization only: every collector held
    the whole population in RAM and the cap was applied at the end. At the
    ceremony's 16,900 symlinks that is a multi-megabyte report of no-action
    findings, which is the same 'physically unreviewable artifact' defect the
    review names elsewhere. Bounding the collector keeps the count exact and
    the report readable.
    """

    __slots__ = ("items", "limit", "total")

    def __init__(self, limit: int) -> None:
        self.items: list[dict[str, Any]] = []
        self.limit = limit
        self.total = 0

    def add(self, item: dict[str, Any]) -> None:
        self.total += 1
        if len(self.items) < self.limit:
            self.items.append(item)


def _doctor_true_component(parent: Path, name: str) -> str | None:
    """The spelling the directory actually holds for `name`, or None."""
    wanted = path_identity(name)
    candidate: str | None = None
    try:
        with os.scandir(parent) as entries:
            for entry in entries:
                if entry.name == name:
                    return name
                if candidate is None and path_identity(entry.name) == wanted:
                    candidate = entry.name
    except OSError:
        return None
    return candidate


def _doctor_true_spelling(path: Path) -> Path | None:
    """Resolve `path` component by component against real directory entries.

    On a folding filesystem `lstat` accepts any spelling, so a recorded path
    can name a real file and still be spelled differently from the bytes the
    index will carry. This returns the spelling the directory holds.
    """
    parts = list(path.parts)
    if not parts:
        return None
    current = Path(parts[0])
    for component in parts[1:]:
        observed = _doctor_true_component(current, component)
        if observed is None:
            return None
        current = current / observed
    return current


def _doctor_case_sensitivity(root: Path) -> str:
    """Probe, without writing, whether `root` distinguishes case.

    One directory entry whose name has a cased character is looked up under
    the flipped spelling: the same inode means the filesystem folds case.
    """
    try:
        with os.scandir(root) as entries:
            for entry in entries:
                flipped = entry.name.swapcase()
                if flipped == entry.name:
                    continue
                try:
                    original = entry.stat(follow_symlinks=False)
                    probe = os.lstat(os.path.join(os.fspath(root), flipped))
                except OSError:
                    return "case-sensitive"
                return (
                    "case-insensitive"
                    if (probe.st_dev, probe.st_ino)
                    == (original.st_dev, original.st_ino)
                    else "case-sensitive"
                )
    except OSError:
        return "unknown"
    return "unknown"


def _doctor_root_destination(
    root: Path, *, role: str, path_map: Sequence[dict[str, str]]
) -> tuple[str | None, str | None]:
    """One declared root's destination spelling, or the refusal that awaits it.

    Capture translates a provider or seat root whether or not it exists
    (`_capture_provider`, absent-root branch), so coverage is checked the
    same way.
    """
    if role == "destination":
        return os.fspath(root), None
    try:
        return translate_path(root, path_map), None
    except BulkloadError as error:
        return None, str(error)


class _DoctorScan:
    """State one walk fills in for every path-shaped check at once."""

    __slots__ = (
        "complete",
        "entries",
        "long_paths",
        "sqlite_orphans",
        "sqlite_shadowed",
        "sqlite_untyped",
        "special_entries",
        "symlinks",
        "unportable_symlinks",
        "unreadable",
    )

    def __init__(self, *, max_findings: int) -> None:
        self.complete = True
        self.entries = 0
        self.long_paths = _DoctorFindings(max_findings)
        self.sqlite_orphans = _DoctorFindings(max_findings)
        self.sqlite_shadowed = _DoctorFindings(max_findings)
        self.sqlite_untyped = _DoctorFindings(max_findings)
        self.special_entries = _DoctorFindings(max_findings)
        self.symlinks = 0
        self.unportable_symlinks = _DoctorFindings(
            min(max_findings, DOCTOR_SAMPLE_FINDINGS)
        )
        self.unreadable = _DoctorFindings(max_findings)


def _doctor_sidecar_tier(
    root: dict[str, Any], *, name: str, relative: str, path: str
) -> dict[str, Any] | None:
    """Which of the engine's three sidecar behaviours this entry will meet.

    Measured against the engine, not guessed:

    * a provider root classifies every path (`_provider_classification`) and,
      when the verdict is `sqlite` and the name ends in a sidecar suffix,
      **drops the file silently** (`_capture_provider`, scanner.py:1837) while
      `_tree_census` still charges it (scanner.py:2213-2217);
    * a seat and the git fleet pass `provider=None`, so no sidecar rule fires
      at all and the file is carried as ordinary content.

    The blocking tier is therefore exactly one thing: a sidecar whose primary
    is spelled like a database (`.db`/`.sqlite`/`.sqlite3`) and is absent from
    a provider root. A file that merely ends in `-journal` is the nixpkgs
    source class and is reported under its own code.
    """
    lowered = name.lower()
    suffix = next((item for item in SQLITE_SIDECARS if lowered.endswith(item)), None)
    if suffix is None:
        return None
    primary = path[: -len(suffix)]
    primary_exists = os.path.lexists(primary)
    database_primary = lowered[: -len(suffix)].endswith(SQLITE_SUFFIXES)
    provider = root.get("provider")
    if provider is not None:
        # The fail tier is gated on the engine's own verdict, never on the
        # root being typed: `_provider_classification` is what decides that a
        # path is SQLite at all.
        if _provider_classification(provider, relative) != "sqlite":
            return None
        if not database_primary:
            tier = "shadowed"
        elif not primary_exists:
            tier = "orphan"
        else:
            return None
    elif not primary_exists:
        tier = "untyped"
    else:
        return None
    return {
        "kind": suffix[1:],
        "missing_primary": None if tier == "shadowed" else primary,
        "path": path,
        "root": root["label"],
        "tier": tier,
        "typed": provider is not None,
    }


def _doctor_record_entry(
    scan: _DoctorScan,
    root: dict[str, Any],
    *,
    name: str,
    relative: str,
    path: str,
    destination: str,
    mode: int | None,
    collect_symlink_modes: bool,
    path_ceiling: int | None,
) -> None:
    """Fold one entry into every path-shaped collector at once."""
    if mode is not None and stat.S_ISLNK(mode):
        scan.symlinks += 1
        if collect_symlink_modes:
            spelled = f"{stat.S_IMODE(mode):04o}"
            if spelled != LINUX_SYMLINK_MODE:
                scan.unportable_symlinks.add(
                    {"mode": spelled, "path": path, "root": root["label"]}
                )
    if mode is not None and not (
        stat.S_ISREG(mode) or stat.S_ISDIR(mode) or stat.S_ISLNK(mode)
    ):
        kind = _SPECIAL_ENTRY_KINDS.get(stat.S_IFMT(mode), "special")
        # A seat is the one root that survives a socket: `_capture_seat`
        # passes skip_sockets=True (scanner.py:2059), so the socket is
        # dropped rather than refused. Everything else reaches `_file_record`
        # (scanner.py:514) or `_capture_provider` (scanner.py:1751, 1777) and
        # becomes a blocker, which `stable_capture_pair` turns into
        # "captures contain blockers" (scanner.py:4862).
        dropped = kind == "socket" and root.get("special_policy") == "skip-sockets"
        scan.special_entries.add(
            {
                "engine": (
                    "dropped from the capture without a blocker"
                    if dropped
                    else "refused as a capture blocker"
                ),
                "kind": kind,
                "path": path,
                "root": root["label"],
                "blocking": not dropped,
            }
        )
    if path_ceiling is not None:
        length = len(os.fsencode(destination))
        if length > path_ceiling:
            scan.long_paths.add(
                {
                    "ceiling": path_ceiling,
                    "destination": destination,
                    "length": length,
                    "path": path,
                    "root": root["label"],
                }
            )
    sidecar = _doctor_sidecar_tier(root, name=name, relative=relative, path=path)
    if sidecar is None:
        return
    {
        "orphan": scan.sqlite_orphans,
        "shadowed": scan.sqlite_shadowed,
        "untyped": scan.sqlite_untyped,
    }[sidecar["tier"]].add(sidecar)


def _doctor_walk(
    scan: _DoctorScan,
    root: dict[str, Any],
    *,
    boundaries: frozenset[str],
    exclusions: Sequence[str],
    max_entries: int,
    collect_symlink_modes: bool,
    path_ceiling: int | None,
) -> Iterable[str]:
    """Yield one `identity NUL source-path` line per namespace entry.

    The walk prunes every *other* declared root, so a nested root is keyed
    under its own path map and never counted twice — which is how capture
    treats it, since each root is walked by its own `_walk_entries` call. It
    also applies capture's own prune rule inside a provider root
    (`_is_pruned`), so the doctor never names a path capture will not read.
    """
    base = Path(root["walk_path"])
    destination = root.get("destination")
    provider = root.get("provider")
    yield f"{path_identity(destination or base)}\0{os.fspath(base)}"
    if root["kind"] == "file":
        return
    stack = [base]
    while stack:
        current = stack.pop()
        try:
            with os.scandir(current) as handle:
                children = sorted(handle, key=lambda entry: entry.name)
        except OSError as error:
            scan.unreadable.add(
                {
                    "path": os.fspath(current),
                    "reason": str(error),
                    "root": root["label"],
                }
            )
            continue
        for entry in children:
            if entry.path in boundaries:
                continue
            relative = Path(entry.path).relative_to(base).as_posix()
            if provider is not None and _is_pruned(provider, relative, exclusions):
                continue
            scan.entries += 1
            if scan.entries > max_entries:
                scan.complete = False
                return
            spelling = (
                os.path.join(destination, relative)
                if destination is not None
                else entry.path
            )
            yield f"{path_identity(spelling)}\0{entry.path}"
            try:
                mode = entry.stat(follow_symlinks=False).st_mode
            except OSError as error:
                scan.unreadable.add(
                    {"path": entry.path, "reason": str(error), "root": root["label"]}
                )
                continue
            _doctor_record_entry(
                scan,
                root,
                name=entry.name,
                relative=relative,
                path=entry.path,
                destination=spelling,
                mode=mode,
                collect_symlink_modes=collect_symlink_modes,
                path_ceiling=path_ceiling,
            )
            if stat.S_ISDIR(mode):
                stack.append(Path(entry.path))


def _doctor_collisions(
    lines: Iterable[str], *, max_findings: int
) -> list[dict[str, Any]]:
    """Adjacent-run grouping over the sorted identity stream.

    Sorting first keeps this O(1) in memory beyond the sort itself, and the
    sort is `_bounded_sorted`, so the `BULKLOAD_BOUND_NAMESPACE` lever that
    bounds a 1.8 M-entry namespace bounds this too.
    """
    groups: list[dict[str, Any]] = []
    key: str | None = None
    spellings: set[str] = set()
    members = 0

    def flush() -> None:
        if key is None or members < 2:
            return
        if len(spellings) < 2 and len(spellings) < max_findings:
            # One identity reached by one spelling more than once is not a
            # collision. Below the cap that judgement is exact; at the cap the
            # group is reported precisely because members were dropped.
            return
        # `--max-findings` caps how many members of a group are *named*; the
        # member count is exact, so a dropped path is marked at the site it
        # was dropped rather than silently vanishing from a group whose whole
        # purpose is to name every offending path.
        groups.append(
            {
                "identity": key,
                "member_count": members,
                "paths": sorted(spellings),
                "truncated": members > len(spellings),
            }
        )

    for line in lines:
        identity, _, source = line.partition("\0")
        if identity != key:
            flush()
            key = identity
            spellings = set()
            members = 0
        members += 1
        if len(spellings) < max_findings:
            spellings.add(source)
    flush()
    return groups


def _doctor_pointer_records(repository: Path) -> list[tuple[str, Path, str]]:
    """Every recorded path in one repository's `.git` plumbing.

    Read as files rather than through `git worktree list`, so the preflight
    costs no subprocess per repository and accepts everything capture does.
    """
    git_entry = repository / ".git"
    pointers: list[tuple[str, Path, str]] = []
    try:
        if git_entry.is_file():
            text = git_entry.read_text(encoding="utf-8")
            if text.startswith("gitdir:"):
                pointers.append(("gitfile", git_entry, text[7:].strip()))
            return pointers
        common = git_entry / "commondir"
        if common.is_file():
            pointers.append(
                ("commondir", common, common.read_text(encoding="utf-8").strip())
            )
        alternates = git_entry / "objects" / "info" / "alternates"
        if alternates.is_file():
            for line in alternates.read_text(encoding="utf-8").splitlines():
                if line.strip():
                    pointers.append(("alternates", alternates, line.strip()))
        worktrees = git_entry / "worktrees"
        if worktrees.is_dir():
            with os.scandir(worktrees) as handle:
                for entry in sorted(handle, key=lambda item: item.name):
                    gitdir = Path(entry.path) / "gitdir"
                    if gitdir.is_file():
                        pointers.append(
                            (
                                "worktree-gitdir",
                                gitdir,
                                gitdir.read_text(encoding="utf-8").strip(),
                            )
                        )
    except (OSError, UnicodeDecodeError):
        return pointers
    return pointers


def _doctor_git_pointers(
    git_root: Path, *, max_findings: int
) -> tuple[_DoctorFindings, _DoctorFindings]:
    """Pointer spellings against the directory entries, and dangling targets."""
    spelling = _DoctorFindings(max_findings)
    dangling = _DoctorFindings(max_findings)
    repositories, _ = _discover_git_roots(git_root)
    for repository in repositories:
        for kind, source, recorded in _doctor_pointer_records(repository):
            if not recorded:
                continue
            candidate = Path(recorded)
            if not candidate.is_absolute():
                candidate = Path(os.path.normpath(source.parent / candidate))
            observed = _doctor_true_spelling(candidate)
            if observed is None:
                dangling.add(
                    {
                        "kind": kind,
                        "pointer": os.fspath(source),
                        "recorded": os.fspath(candidate),
                        "repository": os.fspath(repository),
                    }
                )
                continue
            if os.fspath(observed) != os.fspath(candidate):
                spelling.add(
                    {
                        "kind": kind,
                        "observed": os.fspath(observed),
                        "pointer": os.fspath(source),
                        "recorded": os.fspath(candidate),
                        "repository": os.fspath(repository),
                    }
                )
    return spelling, dangling


def _doctor_path_map_checks(
    roots: Sequence[dict[str, Any]],
    *,
    path_map: Sequence[dict[str, str]],
    role: str,
    max_findings: int,
) -> list[dict[str, Any]]:
    """Coverage — every declared root translates — and aliasing: no two maps
    land two disjoint source subtrees on one destination."""
    mapped = [root for root in roots if root["map"]]
    unmapped = [
        {
            "label": root["label"],
            "path": root["path"],
            "reason": root["destination_error"],
        }
        for root in mapped
        if root["destination_error"]
    ]
    aliases: list[dict[str, Any]] = []
    for index, first in enumerate(path_map):
        for second in path_map[index + 1 :]:
            first_source = PurePosixPath(first["source"])
            second_source = PurePosixPath(second["source"])
            nested = (
                first_source in second_source.parents
                or second_source in first_source.parents
            )
            first_destination = PurePosixPath(first["destination"])
            second_destination = PurePosixPath(second["destination"])
            if first_destination == second_destination:
                aliases.append(
                    {
                        "destinations": [first["destination"], second["destination"]],
                        "reason": "two path-map sources share one destination",
                        "sources": [first["source"], second["source"]],
                    }
                )
                continue
            overlapping = (
                first_destination in second_destination.parents
                or second_destination in first_destination.parents
            )
            if overlapping and not nested:
                aliases.append(
                    {
                        "destinations": [first["destination"], second["destination"]],
                        "reason": "disjoint sources nest on the destination",
                        "sources": [first["source"], second["source"]],
                    }
                )
    return [
        _doctor_check(
            "path-map-coverage",
            "pass" if not unmapped else "fail",
            summary="every declared root, seat, and snapshot root translates",
            expected=f"{len(mapped)} translated roots inside the path map",
            observed=f"{len(mapped) - len(unmapped)} of {len(mapped)} translate",
            findings=unmapped,
            max_findings=max_findings,
            remedy=(
                "add a --path-map SOURCE=DESTINATION covering each named root. "
                "Capture records this as an unmapped-root blocker "
                "(capture_agent_state root_bindings) only after the roots are "
                "read, and the "
                "planner as a root-binding divergence after that."
            ),
        ),
        _doctor_check(
            "path-map-alias",
            ("pass" if not aliases else ("fail" if role == "source" else "warn")),
            summary="no two path maps alias one destination subtree",
            expected="each declared source subtree lands on its own destination",
            observed=f"{len(aliases)} aliasing pairs",
            findings=aliases,
            max_findings=max_findings,
            remedy=(
                "give each source its own destination. Nested sources may "
                "reparent — the longest source prefix wins "
                "(canonical_path_map) — but disjoint sources may not collide. "
                "On the destination role the map translates to itself, so fix "
                "it on the source host, where it binds."
            ),
        ),
    ]


def _doctor_seat_shape(
    seats: Sequence[tuple[str, Path, str]],
    *,
    roots: Sequence[dict[str, Any]],
    max_findings: int,
) -> list[dict[str, Any]]:
    """The seat contract, checked in milliseconds instead of at stage time.

    v4.3 died at stage on a file seat whose plan path was `<parent>/<label>`
    while custody held the file itself (STATUS 2026-08-27T11:11:40Z). These
    are the declaration shapes that reach that far.
    """
    findings: list[dict[str, Any]] = []
    nested: list[dict[str, Any]] = []
    seen: dict[str, Path] = {}
    for name, path, kind in seats:
        if not re.fullmatch(r"[a-z][a-z0-9_-]{0,63}", name):
            findings.append(
                {
                    "expected": "[a-z][a-z0-9_-]{0,63}",
                    "name": name,
                    "path": os.fspath(path),
                    "reason": "seat name is not a legal identifier",
                }
            )
        if name in seen:
            findings.append(
                {
                    "expected": "one declaration per seat name",
                    "name": name,
                    "path": os.fspath(path),
                    "reason": f"seat name repeats {os.fspath(seen[name])}",
                }
            )
        seen[name] = path
        try:
            info = path.lstat()
        except OSError:
            continue
        if kind == "file":
            if stat.S_ISDIR(info.st_mode):
                findings.append(
                    {
                        "expected": "--file-seat names a regular file",
                        "name": name,
                        "path": os.fspath(path),
                        "reason": "declared file seat is a directory; use --seat",
                    }
                )
            elif stat.S_ISLNK(info.st_mode):
                findings.append(
                    {
                        "expected": "--file-seat names a regular file",
                        "name": name,
                        "path": os.fspath(path),
                        "reason": (
                            "declared file seat is a symlink: capture carries "
                            "the backing file under its own parent "
                            "(_capture_seat, file branch) while the plan writes "
                            "the "
                            "logical path"
                        ),
                    }
                )
            elif not stat.S_ISREG(info.st_mode):
                findings.append(
                    {
                        "expected": "--file-seat names a regular file",
                        "name": name,
                        "path": os.fspath(path),
                        "reason": "declared file seat is not a regular file",
                    }
                )
        elif stat.S_ISREG(info.st_mode):
            findings.append(
                {
                    "expected": "--seat names a directory",
                    "name": name,
                    "path": os.fspath(path),
                    "reason": (
                        "declared directory seat is a regular file; use --file-seat"
                    ),
                }
            )
        for root in roots:
            if root["label"] == f"seat:{name}" or not root["walk"]:
                continue
            if Path(root["path"]) in path.parents:
                nested.append(
                    {
                        "expected": "one owner per byte",
                        "name": name,
                        "path": os.fspath(path),
                        "reason": f"seat is inside declared root {root['label']}",
                    }
                )
    return [
        _doctor_check(
            "seat-path-shape",
            "pass" if not findings else "fail",
            summary="every declared seat has the shape its verb requires",
            expected="--seat = directory, --file-seat = regular file, names unique",
            observed=f"{len(findings)} shape defects across {len(seats)} seats",
            findings=findings,
            max_findings=max_findings,
            remedy=(
                "swap --seat and --file-seat to match what is on disk. Capture "
                "refuses a non-file file seat in _capture_seat and a "
                "non-directory declared root in _declared_root, both after the "
                "roots have been read."
            ),
        ),
        _doctor_check(
            "seat-double-carry",
            "pass" if not nested else "warn",
            summary="no seat is also carried by the root that contains it",
            expected="each declared seat outside every other declared root",
            observed=f"{len(nested)} nested seats",
            findings=nested,
            max_findings=max_findings,
            remedy=(
                "declare the seat outside the containing root, or accept the "
                "double carry deliberately: a provider walk does not prune a "
                "declared seat, so those bytes are captured under two owners "
                "and planned twice."
            ),
        ),
    ]


def _peer_environment() -> dict[str, str]:
    environment = {
        key: value
        for key, value in os.environ.items()
        if key not in {"SSH_ASKPASS", "GIT_ASKPASS"}
    }
    environment["LC_ALL"] = "C"
    return environment


def _peer_probe(
    ssh_path: str, host: str, argv: Sequence[str], *, timeout: float
) -> dict[str, Any]:
    """One authenticated, read-only command on the peer.

    The option vector is `SSH_OPTIONS`, the same tuple the rsync transport
    binds (`executor.push_agent_transport`), so a probe that authenticates
    here is evidence the transport will authenticate too.
    """
    try:
        completed = subprocess.run(
            [ssh_path, *SSH_OPTIONS, "--", host, *argv],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            env=_peer_environment(),
            timeout=timeout,
        )
    except subprocess.TimeoutExpired:
        return {"error": "peer probe timed out", "status": None, "stdout": ""}
    except OSError as error:
        return {"error": str(error), "status": None, "stdout": ""}
    return {
        "error": completed.stderr.decode("utf-8", "replace").strip()[:512] or None,
        "status": completed.returncode,
        "stdout": completed.stdout.decode("utf-8", "replace").strip()[:4096],
    }


def _doctor_peer(
    *,
    ssh_path: str,
    host: str,
    uname_path: str,
    env_path: str,
    bulkload_path: str | None,
    timeout: float,
) -> dict[str, Any]:
    """Read the peer's kernel, login shell, POSIX acceptance, and engine.

    Every probe is a simple command: nothing is written, started, or signaled
    on the peer.
    """
    # The kernel is read *through* env, not from an assumed absolute path:
    # NixOS ships /usr/bin/env and nothing else under /usr/bin, so probing
    # /usr/bin/uname directly reports a reachable, correctly configured peer
    # as unreachable and silently downgrades every check that needs its
    # kernel.
    platform_probe = _peer_probe(
        ssh_path, host, [env_path, uname_path, "-s"], timeout=timeout
    )
    environment_probe = _peer_probe(ssh_path, host, [env_path], timeout=timeout)
    # A POSIX login shell expands this to the token; fish and the csh family
    # cannot parse `${name-default}` at all and fail the command outright.
    posix_probe = _peer_probe(
        ssh_path,
        host,
        ["printf", "%s", f'"${{BULKLOAD_DOCTOR_PROBE-{POSIX_PROBE_TOKEN}}}"'],
        timeout=timeout,
    )
    login_shell = None
    for line in environment_probe["stdout"].splitlines():
        if line.startswith("SHELL="):
            login_shell = line[len("SHELL=") :].strip()
    # `SHELL=` is the only line this probe exists to read. The rest of the
    # peer's environment is dropped here rather than sealed into a report
    # that is written to --output, reviewed, and copied between hosts.
    environment_probe = {
        "error": environment_probe["error"],
        "status": environment_probe["status"],
    }
    peer: dict[str, Any] = {
        "engine_version": None,
        "login_shell": login_shell,
        "path_identity": None,
        "platform": None,
        "posix_login_shell": (
            posix_probe["status"] == 0 and posix_probe["stdout"] == POSIX_PROBE_TOKEN
        ),
        "probes": {
            "environment": environment_probe,
            "platform": platform_probe,
            "posix": posix_probe,
        },
        "reachable": platform_probe["status"] == 0,
        "ssh_host": host,
    }
    if platform_probe["status"] == 0 and platform_probe["stdout"]:
        peer["platform"] = platform_probe["stdout"].splitlines()[0].strip().lower()
        # Declared, not measured: the doctor never writes on the peer, so its
        # path identity is read off the kernel's default filesystem rather
        # than probed. Linux (ext4/XFS) compares bytes; APFS ships folding.
        peer["path_identity"] = {
            "darwin": "case-insensitive",
            "linux": "byte-exact",
        }.get(peer["platform"])
    if bulkload_path is not None:
        version_probe = _peer_probe(
            ssh_path, host, [bulkload_path, "--version"], timeout=timeout
        )
        peer["probes"]["engine"] = version_probe
        if version_probe["status"] == 0:
            peer["engine_version"] = version_probe["stdout"].strip()
    return peer


def _doctor_runtime(
    *, peer_digest: str | None, max_findings: int
) -> tuple[dict[str, Any], dict[str, Any]]:
    """Closure identity, and the pin that can silently stand in for it."""
    root = Path(__file__).resolve().parent
    try:
        measured = sha256_bytes(
            canonical_bytes(
                {
                    f"scripts/bulkload_lib/{name}": sha256_file((root / name).resolve())
                    for name in RUNTIME_SOURCE_NAMES
                }
            )
        )
    except (BulkloadError, OSError):
        measured = None
    pin = os.environ.get("BULKLOAD_RUNTIME_SOURCE_PIN")
    channel = os.environ.get("BULKLOAD_RUNTIME_SOURCE_SHA256")
    malformed = pin is not None and not _is_sha256(pin)
    # BULKLOAD_RUNTIME_SOURCE_SHA256 is the load-bearing variable, not the
    # pin: bulkload.py:103-104 writes it from PIN-or-closure and
    # model.runtime_source_digest (model.py:88-95) reads it, raising on the
    # first digest-bearing call when it is malformed. Validating only the pin
    # greenlights a host where every real verb dies instantly.
    channel_malformed = channel is not None and not _is_sha256(channel)
    presented = (
        channel if (channel and not malformed and not channel_malformed) else measured
    )
    runtime = {
        "channel": (
            "malformed"
            if channel_malformed
            else ("present" if channel else "absent")
        ),
        "measured_sha256": measured,
        "peer_sha256": peer_digest,
        "pin": "malformed" if malformed else ("active" if pin else "absent"),
        "presented_sha256": presented,
    }
    findings: list[dict[str, Any]] = []
    status = "pass"
    if channel_malformed:
        status = "fail"
        findings.append(
            {
                "expected": "64 lowercase hex characters",
                "observed": len(channel or ""),
                "reason": (
                    "BULKLOAD_RUNTIME_SOURCE_SHA256 is malformed; "
                    "model.py:88-95 raises 'pinned runtime source digest is "
                    "malformed' at the first digest-bearing call"
                ),
                "variable": "BULKLOAD_RUNTIME_SOURCE_SHA256",
            }
        )
    elif (
        channel
        and measured
        and channel != measured
        and not (pin and not malformed and pin == channel)
    ):
        # The launcher hashes exactly RUNTIME_SOURCE_NAMES (bulkload.py:17-24
        # against model.py:30-37), so under the launcher the channel equals
        # the measurement unless a pin explains the difference. Anything else
        # is a digest the operator would hand the peer as
        # --peer-runtime-source-sha256, propagating the drift to both roles.
        status = "fail"
        findings.append(
            {
                "expected": measured,
                "observed": channel,
                "reason": (
                    "BULKLOAD_RUNTIME_SOURCE_SHA256 presents a closure these "
                    "files do not hash to, and no pin accounts for it; every "
                    "receipt and the peer digest would carry it"
                ),
                "variable": "BULKLOAD_RUNTIME_SOURCE_SHA256",
            }
        )
    if malformed:
        status = "fail"
        findings.append(
            {
                "expected": "64 lowercase hex characters",
                "observed": len(pin or ""),
                "reason": (
                    "BULKLOAD_RUNTIME_SOURCE_PIN is malformed; model.py:88-95 "
                    "refuses it at the first digest-bearing call"
                ),
                "variable": "BULKLOAD_RUNTIME_SOURCE_PIN",
            }
        )
    elif pin and measured and pin != measured:
        status = "fail" if status == "fail" else "warn"
        findings.append(
            {
                "expected": measured,
                "observed": pin,
                "reason": (
                    "an operator pin presents a closure these files do not hash "
                    "to; every receipt will carry the pin, not the engine "
                    "(bulkload.py:97-105)"
                ),
                "variable": "BULKLOAD_RUNTIME_SOURCE_PIN",
            }
        )
    if peer_digest is not None and presented is not None and peer_digest != presented:
        status = "fail"
        findings.append(
            {
                "expected": presented,
                "observed": peer_digest,
                "reason": "the two roles are running different engine closures",
                "variable": "--peer-runtime-source-sha256",
            }
        )
    return runtime, _doctor_check(
        "runtime-source-parity",
        status,
        summary="both roles run one engine closure, and no pin hides drift",
        expected=presented,
        observed=peer_digest if peer_digest is not None else presented,
        findings=findings,
        max_findings=max_findings,
        remedy=(
            "run `bulkload doctor` on the peer and pass its "
            "runtime.presented_sha256 as --peer-runtime-source-sha256. Clear "
            "BULKLOAD_RUNTIME_SOURCE_PIN unless a mid-ceremony break-glass is "
            "in force."
        ),
    )


def _doctor_declared_roots(
    *,
    role: str,
    home: Path,
    git_root: Path,
    codex_root: Path | None,
    claude_root: Path | None,
    pi_root: Path | None,
    seats: Sequence[tuple[str, Path, str]],
    snapshot_root: Path,
    snapshot_base_seal: Path | None,
    path_map: Sequence[dict[str, str]],
) -> list[dict[str, Any]]:
    """Exactly the roots capture binds, in capture's own defaulting order.

    `home` is a binding, not a walked tree (capture_agent_state's
    provider_roots and root_bindings):
    it names the provider defaults and the destination home, so it is checked
    for presence and coverage but never walked.
    """
    declared: list[dict[str, Any]] = [
        {
            "kind": "directory",
            "label": "home",
            "map": True,
            "path": os.fspath(home),
            "walk": False,
        },
        {
            "kind": "directory",
            "label": "git",
            "map": True,
            "path": os.fspath(git_root),
            # `_walk_entries` is called for the fleet without skip_sockets
            # (scanner.py:4316), so every special entry becomes a blocker.
            "special_policy": "refuse",
            "walk": True,
        },
    ]
    for name, provider_root in (
        ("codex", codex_root or home / ".codex"),
        ("claude", claude_root or home / ".claude"),
        ("pi", pi_root or home / ".pi" / "agent"),
    ):
        declared.append(
            {
                "kind": "directory",
                "label": f"provider:{name}",
                "map": True,
                "path": os.fspath(provider_root),
                # The provider name capture classifies this root under; it is
                # what makes `_provider_classification` answerable here.
                "provider": name,
                "special_policy": "refuse",
                "walk": True,
            }
        )
    for name, seat_path, seat_kind in seats:
        declared.append(
            {
                "kind": seat_kind,
                "label": f"seat:{name}",
                "map": True,
                "path": os.fspath(seat_path),
                # `_capture_seat` walks with skip_sockets=True and no provider,
                # so a socket is dropped silently and no sidecar rule fires.
                "special_policy": "skip-sockets",
                "walk": True,
            }
        )
    declared.append(
        {
            "kind": "directory",
            "label": "snapshot",
            # Capture writes the immutable snapshot under the evidence path
            # and rewrites every snapshot path back to its live spelling
            # (_rewrite_catalog_to_live), so the
            # snapshot root is never translated and needs no map entry. It is
            # still checked for overlap against every declared root.
            "map": False,
            "path": os.fspath(snapshot_root),
            "walk": False,
        }
    )
    if snapshot_base_seal is not None:
        declared.append(
            {
                "kind": "file",
                "label": "snapshot-base-seal",
                "map": False,
                "path": os.fspath(snapshot_base_seal),
                "walk": False,
            }
        )
    for root in declared:
        candidate = Path(root["path"])
        destination, error = (
            _doctor_root_destination(candidate, role=role, path_map=path_map)
            if root["map"]
            else (None, None)
        )
        root["destination"] = destination
        root["destination_error"] = error
        root["walk_path"] = root["path"]
        try:
            info = candidate.lstat()
        except OSError:
            root["exists"] = False
            root["link"] = False
            continue
        root["exists"] = True
        root["link"] = stat.S_ISLNK(info.st_mode)
        if root["link"]:
            # Capture walks the backing root and translates the logical one
            # (_declared_root).
            try:
                root["walk_path"] = os.fspath(resolve_real(candidate))
            except BulkloadError:
                root["exists"] = False
        root["directory"] = Path(root["walk_path"]).is_dir()
    return declared


def run_doctor(
    *,
    role: str,
    home: Path,
    git_root: Path,
    codex_root: Path | None,
    claude_root: Path | None,
    pi_root: Path | None,
    seats: Sequence[tuple[str, Path, str]],
    path_map: Sequence[dict[str, str]],
    capture_output: Path,
    snapshot_base_seal: Path | None,
    engine_version: str,
    managed_exclusions: Sequence[tuple[str, str]] = (),
    peer_ssh_host: str | None = None,
    peer_bulkload: str | None = None,
    peer_runtime_source_sha256: str | None = None,
    ssh_path: str | None = None,
    peer_uname_path: str = DEFAULT_PEER_UNAME,
    peer_env_path: str = DEFAULT_PEER_ENV,
    peer_timeout_seconds: float = DEFAULT_PEER_TIMEOUT_SECONDS,
    max_entries: int = DEFAULT_MAX_FILES,
    max_findings: int = DOCTOR_MAX_FINDINGS,
) -> dict[str, Any]:
    """Name every path that will refuse this contract, before a byte moves."""
    if role not in {"source", "destination"}:
        raise BulkloadError("doctor role must be source or destination")
    if max_entries < 1 or max_findings < 1:
        raise BulkloadError("doctor budgets must be positive")
    snapshot_root = capture_output.parent / f".{capture_output.name}.snapshot"
    declared = _doctor_declared_roots(
        role=role,
        home=home,
        git_root=git_root,
        codex_root=codex_root,
        claude_root=claude_root,
        pi_root=pi_root,
        seats=seats,
        snapshot_root=snapshot_root,
        snapshot_base_seal=snapshot_base_seal,
        path_map=path_map,
    )
    checks: list[dict[str, Any]] = []

    presence: list[dict[str, Any]] = []
    for root in declared:
        if not root["exists"]:
            if root["label"] in {"home", "git"}:
                presence.append(
                    {
                        "expected": "an existing directory",
                        "label": root["label"],
                        "path": root["path"],
                        "reason": "declared root does not exist",
                    }
                )
            continue
        if root["kind"] == "directory" and not root["directory"]:
            presence.append(
                {
                    "expected": "a directory, or a link naming one",
                    "label": root["label"],
                    "path": root["path"],
                    "reason": "declared root is not a directory",
                }
            )
        if root["label"] == "snapshot":
            continue
        try:
            assert_no_overlap(
                capture_output, [Path(root["path"])], "capture evidence output"
            )
        except BulkloadError as error:
            presence.append(
                {
                    "expected": "evidence written outside every declared root",
                    "label": root["label"],
                    "path": root["path"],
                    "reason": str(error),
                }
            )
    checks.append(
        _doctor_check(
            "declared-root-presence",
            "pass" if not presence else "fail",
            summary="every declared root exists and the evidence path is outside it",
            expected=f"{len(declared)} declared roots",
            observed=f"{len(presence)} defects",
            findings=presence,
            max_findings=max_findings,
            remedy=(
                "create or re-declare the named root, and place the capture "
                "--output outside every live root: cli.py:_protect_output "
                "refuses the overlap after the arguments are parsed."
            ),
        )
    )

    peer: dict[str, Any] | None = None
    if peer_ssh_host is not None:
        if SSH_HOST_AUTHORITY.fullmatch(peer_ssh_host) is None:
            raise BulkloadError("peer SSH authority is not a canonical [user@]host")
        binary = ssh_path or shutil.which("ssh")
        if binary is None:
            raise BulkloadError("an explicit safe SSH executable is unavailable")
        peer = _doctor_peer(
            ssh_path=shell_safe_executable(binary, "SSH"),
            host=peer_ssh_host,
            uname_path=peer_uname_path,
            env_path=peer_env_path,
            bulkload_path=peer_bulkload,
            timeout=peer_timeout_seconds,
        )
        checks.append(
            _doctor_check(
                "peer-reachable",
                "pass" if peer["reachable"] else "fail",
                summary="the peer answers the transport's own SSH option vector",
                expected=(
                    f"{peer_env_path} {peer_uname_path} -s over BatchMode SSH"
                ),
                observed=peer["probes"]["platform"]["error"] or peer["platform"],
                findings=[]
                if peer["reachable"]
                else [
                    {
                        "host": peer_ssh_host,
                        "reason": peer["probes"]["platform"]["error"]
                        or "peer probe failed",
                        "status": peer["probes"]["platform"]["status"],
                    }
                ],
                max_findings=max_findings,
                remedy=(
                    "authenticate non-interactively (BatchMode=yes, "
                    "StrictHostKeyChecking=yes) before the transport needs "
                    "it. The kernel probe runs through --peer-env-path so a "
                    "NixOS peer, which ships no /usr/bin/uname, answers it; "
                    "--peer-uname overrides the command if that is wrong."
                ),
            )
        )
        shell = PurePosixPath(peer["login_shell"]).name if peer["login_shell"] else None
        non_posix = shell in NON_POSIX_LOGIN_SHELLS
        shell_defect = peer["reachable"] and (
            non_posix or not peer["posix_login_shell"]
        )
        checks.append(
            _doctor_check(
                "peer-login-shell",
                # An unreachable peer answered nothing, so this is unevaluated
                # rather than clean.
                "skipped"
                if not peer["reachable"]
                else ("warn" if shell_defect else "pass"),
                summary="the peer's login shell accepts the transport's command shapes",
                expected="a POSIX login shell, or every remote step piped to bash -s",
                observed=peer["login_shell"] or "unknown",
                findings=[
                    {
                        "host": peer_ssh_host,
                        "login_shell": peer["login_shell"],
                        "posix_expansion": peer["posix_login_shell"],
                        "reason": (
                            f"{shell} does not parse POSIX parameter expansion"
                            if non_posix
                            else "the peer rejected a POSIX parameter expansion"
                        ),
                    }
                ]
                if shell_defect
                else [],
                max_findings=max_findings,
                remedy=(
                    "keep remote steps to absolute binaries with no shell "
                    "syntax, or pipe scripts as `ssh HOST bash -s < script`. "
                    "rsync's own --rsync-path invocation is a simple command "
                    "and is unaffected."
                ),
            )
        )
        if peer_bulkload is not None:
            expected_version = f"bulkload {engine_version}"
            matched = peer["engine_version"] == expected_version
            checks.append(
                _doctor_check(
                    "peer-engine-version",
                    "skipped"
                    if not peer["reachable"]
                    else ("pass" if matched else "fail"),
                    summary="both roles present one engine version",
                    expected=expected_version,
                    observed=peer["engine_version"],
                    findings=[]
                    if matched
                    else [
                        {
                            "expected": expected_version,
                            "host": peer_ssh_host,
                            "observed": peer["engine_version"],
                            "path": peer_bulkload,
                            "reason": "the peer presents a different engine",
                        }
                    ],
                    max_findings=max_findings,
                    remedy=(
                        "deploy one closure to both roles: the v4 artifact "
                        "contract is a build artifact of a single engine "
                        "checkout, not a stable format."
                    ),
                )
            )

    local_platform = sys.platform
    local_kind = (
        "darwin"
        if local_platform == "darwin"
        else ("linux" if local_platform.startswith("linux") else local_platform)
    )
    peer_kind = peer["platform"] if peer else None
    cross_kernel = bool(peer_kind) and peer_kind != local_kind
    case_identity = _doctor_case_sensitivity(home)
    # What the *destination* does with two spellings is the only thing that
    # decides whether a collision group is a defect. On the destination role
    # the map is identity, so the question does not arise here at all.
    destination_identity = (
        case_identity
        if role == "destination"
        else (peer["path_identity"] if peer else None)
    )
    destination_kind = local_kind if role == "destination" else peer_kind
    path_ceiling = PATH_LENGTH_CEILINGS.get(destination_kind or local_kind)
    exclusions: dict[str, list[str]] = defaultdict(list)
    for item in canonical_provider_policy(managed_exclusions)["managed_exclusions"]:
        exclusions[item["provider"]].append(item["relative_path"])

    scan = _DoctorScan(max_findings=max_findings)
    walkable = [
        root
        for root in declared
        if root["walk"]
        and root["exists"]
        and (root["directory"] or root["kind"] == "file")
    ]
    walked_labels = {root["label"] for root in walkable}
    boundaries = frozenset(
        root["walk_path"] for root in declared if root["exists"] and root["directory"]
    )

    def stream() -> Iterable[str]:
        for root in walkable:
            if not scan.complete:
                return
            yield from _doctor_walk(
                scan,
                root,
                boundaries=boundaries - {root["walk_path"]},
                exclusions=exclusions[root.get("provider") or ""],
                max_entries=max_entries,
                collect_symlink_modes=cross_kernel,
                path_ceiling=path_ceiling,
            )

    collisions = _doctor_collisions(
        _bounded_sorted(stream()), max_findings=max_findings
    )

    def path_shaped(status: str) -> str:
        """A path-shaped verdict a truncated walk did not earn.

        `pass` here would mean 'the walk saw no defect'; after truncation it
        only means 'the walk stopped'. `DEFAULT_MAX_FILES` is 2,000,000 and
        the ceremony's own custody loop ran over 1,781,044 index entries, so
        this is one lap's growth away, not a synthetic case.
        """
        return "partial" if (status == "pass" and not scan.complete) else status

    checks.append(
        _doctor_check(
            "scan-complete",
            "pass" if scan.complete else "fail",
            summary="the namespace walk finished inside its budget",
            expected=f"at most {max_entries} entries",
            observed=(
                f"{scan.entries} entries"
                if scan.complete
                else f"{scan.entries} entries and still walking"
            ),
            findings=[]
            if scan.complete
            else [
                {
                    "expected": max_entries,
                    "observed": scan.entries,
                    "reason": (
                        "the walk stopped at --max-entries, so every "
                        "path-shaped check below is partial and none of them "
                        "may be read as green"
                    ),
                }
            ],
            max_findings=max_findings,
            remedy=(
                "raise --max-entries and re-run. A truncated walk refuses "
                "rather than reporting a partial namespace as clean."
            ),
        )
    )
    if not collisions:
        collision_status = "pass"
    elif destination_identity == "case-insensitive":
        collision_status = "fail"
    else:
        # Byte-exact or unknown: two spellings that fold together still land
        # on two destination paths, so naming them is all this check may do.
        collision_status = "warn"
    checks.append(
        _doctor_check(
            "case-fold-collision",
            path_shaped(collision_status),
            summary="no two declared paths collapse onto one destination identity",
            expected="one destination identity per source path",
            observed=(
                f"{len(collisions)} collision groups over {scan.entries} entries"
                f", destination path identity "
                f"{destination_identity or 'unknown'}"
                + ("" if scan.complete else " (walk stopped at --max-entries)")
            ),
            findings=collisions,
            max_findings=max_findings,
            remedy=(
                "rename one spelling in each group, or exclude it. A group is "
                "a set of source paths that are distinct here and one path on "
                "a folding destination, where the second silently overwrites "
                "the first; it blocks only once the destination is known to "
                "fold. It is not the git-pointer class: a recorded pointer "
                "spelling never enters this stream, and git-pointer-spelling "
                "is the check that names the 2026-08-27 "
                "GloriousFlywheel.worktrees defect."
            ),
        )
    )
    # `unreadable-agent-state` is a capture blocker, not an advisory: it is
    # what `stable_capture_pair` turns into "captures contain blockers"
    # (scanner.py:4862). Outside a provider root the fleet walk records
    # `unreadable-filesystem-entry` and refuses the same way, so the only
    # question is whether the path is inside a declared root at all.
    checks.append(
        _doctor_check(
            "root-readable",
            path_shaped("fail" if scan.unreadable.total else "pass"),
            summary="every declared path is readable by this user",
            expected="no unreadable entry inside a declared root",
            observed=f"{scan.unreadable.total} unreadable paths",
            findings=scan.unreadable.items,
            finding_total=scan.unreadable.total,
            max_findings=max_findings,
            remedy=(
                "settle the mode or exclude the path. Capture records each of "
                "these as a blocker and the capture pair then refuses, hours "
                "into the read."
            ),
        )
    )
    blocking_specials = [
        item for item in scan.special_entries.items if item["blocking"]
    ]
    checks.append(
        _doctor_check(
            "special-entry",
            path_shaped(
                "fail"
                if blocking_specials
                else ("warn" if scan.special_entries.total else "pass")
            ),
            summary="no socket, FIFO, or device node sits inside a declared root",
            expected="only regular files, directories, and symlinks",
            observed=f"{scan.special_entries.total} special entries",
            findings=scan.special_entries.items,
            finding_total=scan.special_entries.total,
            max_findings=max_findings,
            remedy=(
                "move or exclude the entry. `_file_record` raises 'special "
                "filesystem entry is unsupported' (scanner.py:514) and "
                "`_capture_provider` records `special-agent-state`, either of "
                "which refuses the capture pair; a socket under a --seat is "
                "instead dropped from the capture without a blocker, which is "
                "silent loss rather than a refusal."
            ),
        )
    )
    checks.append(
        _doctor_check(
            "path-length-ceiling",
            path_shaped(
                "pass"
                if not scan.long_paths.total
                else ("fail" if destination_kind else "warn")
            ),
            summary="every destination path fits the destination kernel's PATH_MAX",
            expected=(
                f"at most {path_ceiling} bytes per destination path"
                f" ({destination_kind or local_kind} PATH_MAX)"
                if path_ceiling
                else "a known destination kernel"
            ),
            observed=f"{scan.long_paths.total} destination paths over the ceiling",
            findings=scan.long_paths.items,
            finding_total=scan.long_paths.total,
            max_findings=max_findings,
            remedy=(
                "shorten the destination prefix in --path-map. A map may "
                "lengthen every path in a root at once, and the ceiling that "
                "binds is the destination kernel's; without --peer-ssh-host "
                "this host's ceiling stands in and the check can only warn."
            ),
        )
    )

    spelling, dangling = _doctor_git_pointers(
        Path(next(root["walk_path"] for root in declared if root["label"] == "git")),
        max_findings=max_findings,
    )
    checks.append(
        _doctor_check(
            "git-pointer-spelling",
            "pass" if not spelling.total else "fail",
            summary="every git pointer is spelled the way its directory is",
            expected="pointer spelling == on-disk spelling",
            observed=f"{spelling.total} pointers differ",
            findings=spelling.items,
            finding_total=spelling.total,
            max_findings=max_findings,
            remedy=(
                "re-point the worktree (git worktree repair) so the recorded "
                "path matches the directory entry. A folding filesystem "
                "accepts both spellings; the index carries one, and the "
                "destination cannot match the other."
            ),
        )
    )
    checks.append(
        _doctor_check(
            "git-pointer-target",
            "pass" if not dangling.total else "warn",
            summary="every git pointer names a path that exists",
            expected="no dangling worktree, commondir, or alternates pointer",
            observed=f"{dangling.total} dangling pointers",
            findings=dangling.items,
            finding_total=dangling.total,
            max_findings=max_findings,
            remedy="git worktree prune, or restore the named path before capture.",
        )
    )

    checks.extend(
        _doctor_path_map_checks(
            declared, path_map=path_map, role=role, max_findings=max_findings
        )
    )
    checks.extend(_doctor_seat_shape(seats, roots=declared, max_findings=max_findings))

    orphan_total = scan.sqlite_orphans.total + scan.sqlite_untyped.total
    checks.append(
        _doctor_check(
            "sqlite-sidecar-orphan",
            path_shaped(
                "fail"
                if scan.sqlite_orphans.total
                else ("warn" if scan.sqlite_untyped.total else "pass")
            ),
            summary="no -wal/-shm/-journal sidecar outlives its database",
            expected="every sqlite sidecar has its primary beside it",
            observed=(
                f"{scan.sqlite_orphans.total} orphans a provider root "
                f"classifies as sqlite, {scan.sqlite_untyped.total} elsewhere"
            ),
            findings=scan.sqlite_orphans.items + scan.sqlite_untyped.items,
            finding_total=orphan_total,
            max_findings=max_findings,
            remedy=(
                "checkpoint and clear the debris. The fail tier is exactly "
                "what `_provider_classification` calls sqlite inside a "
                "provider root with a `.db`/`.sqlite`/`.sqlite3` primary "
                "missing; 13 of the ceremony's 20 laps were sidecar sweeps. "
                "A seat and the git fleet pass provider=None, so a sidecar "
                "there is ordinary content and only warns."
            ),
        )
    )
    checks.append(
        _doctor_check(
            "sqlite-sidecar-shadowed",
            path_shaped("warn" if scan.sqlite_shadowed.total else "pass"),
            summary=(
                "no ordinary provider file is shadowed by the sqlite sidecar rule"
            ),
            expected="no -wal/-shm/-journal name whose primary is not a database",
            observed=f"{scan.sqlite_shadowed.total} shadowed provider files",
            findings=scan.sqlite_shadowed.items,
            finding_total=scan.sqlite_shadowed.total,
            max_findings=max_findings,
            remedy=(
                "rename or --managed-exclusion the file, or accept the loss "
                "deliberately. `_capture_provider` drops any provider path "
                "classified sqlite whose name ends in a sidecar suffix "
                "(scanner.py:1837) while `_tree_census` still charges it "
                "(scanner.py:2213-2217), so an ordinary source file such as "
                "`plugins/vendor/systemd-journal` is carried by the census "
                "and absent from the items. This is silent loss, not a "
                "refusal, which is why it warns rather than blocking."
            ),
        )
    )

    checks.append(
        _doctor_check(
            "symlink-mode-portability",
            # No peer kernel is not agreement: a probe that failed leaves this
            # unevaluated, and `pass` would report the peer-unreachable run as
            # having compared two kernels.
            "skipped"
            if peer_kind is None
            else path_shaped(
                "warn" if (cross_kernel and scan.unportable_symlinks.total) else "pass"
            ),
            summary="symlink permission bits do not bind across kernels",
            expected=(
                f"{local_kind} and {peer_kind} agree on symlink mode"
                if cross_kernel
                else (
                    "one kernel on both roles"
                    if peer_kind
                    else "a peer kernel to compare against"
                )
            ),
            observed=(
                f"{scan.unportable_symlinks.total} of {scan.symlinks} symlinks "
                f"carry a mode other than {LINUX_SYMLINK_MODE}; "
                f"at most {DOCTOR_SAMPLE_FINDINGS} are named"
                if cross_kernel
                else (
                    f"{scan.symlinks} symlinks, one kernel on both roles"
                    if peer_kind
                    else f"{scan.symlinks} symlinks, no peer kernel to compare"
                )
            ),
            findings=scan.unportable_symlinks.items if cross_kernel else [],
            finding_total=scan.unportable_symlinks.total if cross_kernel else 0,
            max_findings=max_findings,
            remedy=(
                "no action while the exemption stands: mode is exempt from "
                "symlink payload verification in validate_snapshot_custody, "
                "and "
                "kind, size and target digest still bind. Without it 16,865 of "
                "16,900 sealed links refuse on the first cross-OS run."
            ),
        )
    )

    peer_identity = peer["path_identity"] if peer else None
    # The probe and the local measurement spell the same property two ways
    # ("case-sensitive" from a live directory, "byte-exact" from a declared
    # kernel default), so they are compared in one vocabulary.
    local_identity = "byte-exact" if case_identity == "case-sensitive" else case_identity
    identity_split = bool(
        peer_identity is not None
        and local_identity in {"byte-exact", "case-insensitive"}
        and peer_identity != local_identity
    )
    checks.append(
        _doctor_check(
            "path-identity-pair",
            # Unknown is not agreement. This check is a fact about the pair,
            # not a defect a rename settles, so the blocking consequence stays
            # where the offending paths are: case-fold-collision when the
            # destination folds, git-pointer-spelling when this host does.
            "skipped"
            if peer_identity is None and role == "source"
            else ("warn" if identity_split else "pass"),
            summary="the two roles agree on what makes two paths one path",
            expected="one declared path identity across the pair",
            observed={
                "peer": peer_identity,
                "role": case_identity,
            },
            findings=[
                {
                    "consequence": (
                        "the destination folds what this host distinguishes: "
                        "every case-fold-collision group is an overwrite"
                        if peer_identity == "case-insensitive"
                        else "this host folds what the destination "
                        "distinguishes: a recorded spelling may name a real "
                        "file here and no file there (git-pointer-spelling)"
                    ),
                    "peer": peer["ssh_host"],
                    "peer_identity": peer_identity,
                    "reason": "the two roles do not agree on path identity",
                    "role_identity": case_identity,
                }
            ]
            if identity_split and peer
            else [],
            max_findings=max_findings,
            remedy=(
                "declare the identity in the contract instead of discovering "
                "it after a full custody read, and settle every path named by "
                "case-fold-collision and git-pointer-spelling first."
            ),
        )
    )

    runtime, runtime_check = _doctor_runtime(
        peer_digest=peer_runtime_source_sha256, max_findings=max_findings
    )
    checks.append(runtime_check)

    checks.sort(key=lambda item: item["code"])
    summary = {
        status: sum(1 for check in checks if check["status"] == status)
        for status in DOCTOR_STATUSES
    }
    report = {
        "checks": checks,
        "complete": scan.complete,
        "declared_roots": [
            {
                "destination": root["destination"],
                "exists": root["exists"],
                "kind": root["kind"],
                "label": root["label"],
                "path": root["path"],
                "walked": root["label"] in walked_labels,
            }
            for root in declared
        ],
        "destination": {
            # What the fail tiers were gated on, recorded so a green report
            # says which questions it was able to answer.
            "path_identity": destination_identity,
            "path_length_ceiling": path_ceiling,
            "platform": destination_kind,
        },
        "doctor_id": new_id(),
        "engine_version": engine_version,
        "generated_at": utc_now(),
        "host": {
            "hostname": socket.gethostname(),
            "path_identity": case_identity,
            "platform": local_kind,
        },
        "ok": summary["fail"] == 0,
        "peer": peer,
        "role": role,
        "runtime": runtime,
        "schema": DOCTOR_REPORT_SCHEMA,
        "scanned_entries": scan.entries,
        "summary": summary,
    }
    return seal(report, "report_sha256")
