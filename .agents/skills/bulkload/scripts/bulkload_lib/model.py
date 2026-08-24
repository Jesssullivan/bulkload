"""Shared contracts and durable filesystem primitives for Bulkload."""

from __future__ import annotations

import ctypes
from datetime import UTC, datetime
import errno
import hashlib
import json
import math
import os
from pathlib import Path, PurePosixPath
import shutil
import stat
import tempfile
from typing import Any, Iterable
import unicodedata
import uuid


AGENT_CAPTURE_SCHEMA = "dev.tinyland.bulkload.agent-capture.v4"
GIT_WORKSPACE_SCHEMA = "dev.tinyland.bulkload.git-workspace.v2"
AGENT_PLAN_SCHEMA = "dev.tinyland.bulkload.agent-plan.v4"
AGENT_STAGE_SCHEMA = "dev.tinyland.bulkload.agent-stage-receipt.v4"
AGENT_APPLY_SCHEMA = "dev.tinyland.bulkload.agent-apply-receipt.v4"
AGENT_VERIFY_SCHEMA = "dev.tinyland.bulkload.agent-verify-receipt.v4"
AGENT_ROLLBACK_SCHEMA = "dev.tinyland.bulkload.agent-rollback-receipt.v4"
AGENT_RECOVER_SCHEMA = "dev.tinyland.bulkload.agent-recover-receipt.v4"
AGENT_JOURNAL_SCHEMA = "dev.tinyland.bulkload.agent-journal.v4"
RUNTIME_SOURCE_NAMES = (
    "__init__.py",
    "model.py",
    "scanner.py",
    "planner.py",
    "executor.py",
    "cli.py",
)


class BulkloadError(RuntimeError):
    """A fail-closed protocol, custody, or safety error."""


class QuiescenceRefusal(BulkloadError):
    """A declared capture root is not quiet. Nothing was read beyond metadata."""


def utc_now() -> str:
    return datetime.now(UTC).isoformat(timespec="seconds").replace("+00:00", "Z")


def new_id() -> str:
    return str(uuid.uuid4())


def canonical_bytes(value: Any) -> bytes:
    try:
        return json.dumps(
            value,
            allow_nan=False,
            ensure_ascii=False,
            separators=(",", ":"),
            sort_keys=True,
        ).encode("utf-8")
    except (TypeError, ValueError) as error:
        raise BulkloadError(f"value is not canonical JSON: {error}") from error


def sha256_bytes(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    try:
        with path.open("rb", buffering=0) as stream:
            while chunk := stream.read(1024 * 1024):
                digest.update(chunk)
    except OSError as error:
        raise BulkloadError(f"cannot hash file {path}") from error
    return digest.hexdigest()


def sha256_symlink(path: Path) -> str:
    try:
        payload = os.fsencode(os.readlink(path))
    except OSError as error:
        raise BulkloadError(f"cannot read symbolic link {path}") from error
    return sha256_bytes(payload)


def runtime_source_digest() -> str:
    pinned = os.environ.get("BULKLOAD_RUNTIME_SOURCE_SHA256")
    if pinned is not None:
        if len(pinned) != 64 or any(
            character not in "0123456789abcdef" for character in pinned
        ):
            raise BulkloadError("pinned runtime source digest is malformed")
        return pinned
    root = Path(__file__).resolve().parent
    inventory = {
        f"scripts/bulkload_lib/{name}": sha256_file((root / name).resolve())
        for name in RUNTIME_SOURCE_NAMES
    }
    return sha256_bytes(canonical_bytes(inventory))


def object_digest(value: dict[str, Any], digest_field: str) -> str:
    return sha256_bytes(
        canonical_bytes(
            {key: item for key, item in value.items() if key != digest_field}
        )
    )


def seal(value: dict[str, Any], digest_field: str) -> dict[str, Any]:
    value[digest_field] = object_digest(value, digest_field)
    return value


def require_digest(value: dict[str, Any], digest_field: str) -> None:
    actual = value.get(digest_field)
    expected = object_digest(value, digest_field)
    if actual != expected:
        raise BulkloadError(
            f"{digest_field} mismatch: recorded {actual!r}, computed {expected}"
        )


def require_exact_keys(
    value: dict[str, Any], expected: Iterable[str], label: str
) -> None:
    if not isinstance(value, dict):
        raise BulkloadError(f"{label} must be a JSON object")
    expected_keys = set(expected)
    observed = set(value)
    if observed != expected_keys:
        raise BulkloadError(
            f"{label} fields differ from the approved contract: "
            f"{sorted(observed ^ expected_keys)}"
        )


def _unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, item in pairs:
        if key in value:
            raise ValueError(f"duplicate JSON key {key!r} is forbidden")
        value[key] = item
    return value


def _reject_nonfinite(value: str) -> None:
    raise ValueError(f"non-finite JSON number {value} is forbidden")


def _parse_finite(value: str) -> float:
    result = float(value)
    if not math.isfinite(result):
        raise ValueError(f"non-finite JSON number {value} is forbidden")
    return result


MAX_JSON_BYTES = 4 * 1024**3


def read_json(path: Path, *, max_bytes: int = MAX_JSON_BYTES) -> dict[str, Any]:
    try:
        info = path.stat(follow_symlinks=False)
        if not stat.S_ISREG(info.st_mode) or info.st_size > max_bytes:
            raise BulkloadError(f"JSON input is not a bounded regular file: {path}")
        with path.open("rb") as stream:
            payload = stream.read(max_bytes + 1)
        value = json.loads(
            payload,
            object_pairs_hook=_unique_object,
            parse_constant=_reject_nonfinite,
            parse_float=_parse_finite,
        )
    except BulkloadError:
        raise
    except (OSError, UnicodeDecodeError, json.JSONDecodeError, ValueError) as error:
        raise BulkloadError(f"cannot read JSON object {path}: {error}") from error
    if not isinstance(value, dict):
        raise BulkloadError(f"{path} must contain a JSON object")
    return value


def fsync_directory(path: Path) -> None:
    flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_CLOEXEC", 0)
    descriptor = os.open(path, flags)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def durable_makedirs(path: Path, mode: int = 0o700) -> None:
    """Create a no-symlink directory chain and persist each new entry."""
    absolute = Path(os.path.abspath(os.fspath(path.expanduser())))
    if not absolute.is_absolute():
        raise BulkloadError("durable directory path must be absolute")
    parts = absolute.parts
    flags = (
        os.O_RDONLY
        | getattr(os, "O_DIRECTORY", 0)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_CLOEXEC", 0)
    )
    descriptor = os.open(absolute.anchor, flags)
    traversed = Path(absolute.anchor)
    try:
        for component in parts[1:]:
            created = False
            try:
                child = os.open(component, flags, dir_fd=descriptor)
            except FileNotFoundError:
                try:
                    os.mkdir(component, mode, dir_fd=descriptor)
                    created = True
                except FileExistsError:
                    pass
                try:
                    child = os.open(component, flags, dir_fd=descriptor)
                except OSError as error:
                    raise BulkloadError(
                        f"directory path is not a real directory: {traversed / component}"
                    ) from error
            except OSError as error:
                raise BulkloadError(
                    f"directory path is not a real directory: {traversed / component}"
                ) from error
            if created:
                os.fchmod(child, mode)
                os.fsync(child)
                os.fsync(descriptor)
            os.close(descriptor)
            descriptor = child
            traversed /= component
    finally:
        os.close(descriptor)


def atomic_write(path: Path, payload: bytes, mode: int = 0o600) -> None:
    requested = Path(os.path.abspath(os.fspath(path.expanduser())))
    path = requested
    durable_makedirs(path.parent)
    descriptor, temporary = tempfile.mkstemp(
        prefix=f".{path.name}.bulkload-", dir=path.parent
    )
    temporary_path = Path(temporary)
    try:
        os.fchmod(descriptor, mode)
        with os.fdopen(descriptor, "wb", closefd=True) as output:
            output.write(payload)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary_path, path)
        fsync_directory(path.parent)
    except BaseException:
        try:
            os.close(descriptor)
        except OSError:
            pass
        temporary_path.unlink(missing_ok=True)
        raise


def atomic_write_json(path: Path, value: dict[str, Any]) -> None:
    atomic_write(path, canonical_bytes(value) + b"\n")


def normalize_relative(value: str, *, allow_dot: bool = False) -> str:
    if not isinstance(value, str) or "\x00" in value:
        raise BulkloadError("relative path must be a NUL-free string")
    if any(unicodedata.category(character) in {"Cc", "Cf"} for character in value):
        raise BulkloadError("control and format characters are forbidden in paths")
    candidate = PurePosixPath(value)
    if candidate.is_absolute():
        raise BulkloadError(f"absolute path is forbidden: {value!r}")
    if value == "." and allow_dot:
        return value
    if not candidate.parts or any(part in {"", ".", ".."} for part in candidate.parts):
        raise BulkloadError(f"non-canonical relative path is forbidden: {value!r}")
    if candidate.as_posix() != value:
        raise BulkloadError(f"non-canonical relative path is forbidden: {value!r}")
    return value


def resolve_real(path: Path, *, must_exist: bool = True) -> Path:
    expanded = Path(os.path.abspath(os.fspath(path.expanduser())))
    try:
        return expanded.resolve(strict=must_exist)
    except (OSError, RuntimeError) as error:
        raise BulkloadError(f"cannot resolve path {expanded}") from error


def translate_path(path: str | Path, path_map: Iterable[dict[str, str]]) -> str:
    source_path = Path(os.path.abspath(os.fspath(Path(path).expanduser())))
    matches: list[tuple[int, Path, Path]] = []
    for entry in path_map:
        source = Path(entry["source"])
        destination = Path(entry["destination"])
        try:
            relative = source_path.relative_to(source)
        except ValueError:
            continue
        matches.append((len(source.parts), destination, relative))
    if not matches:
        raise BulkloadError(
            f"source path is outside the approved path map: {source_path}"
        )
    _, destination, relative = max(matches, key=lambda item: item[0])
    return os.fspath(destination / relative)


def assert_no_overlap(path: Path, protected: Iterable[Path], label: str) -> None:
    candidate = Path(os.path.realpath(os.path.abspath(os.fspath(path.expanduser()))))
    for raw in protected:
        root = Path(os.path.realpath(os.path.abspath(os.fspath(raw.expanduser()))))
        try:
            candidate.relative_to(root)
        except ValueError:
            pass
        else:
            raise BulkloadError(f"{label} overlaps live root {root}")
        try:
            root.relative_to(candidate)
        except ValueError:
            continue
        raise BulkloadError(f"{label} contains live root {root}")


def ensure_safe_target(root: Path, relative: str) -> Path:
    normalized = normalize_relative(relative)
    root = Path(os.path.abspath(os.fspath(root)))
    current = root
    for component in PurePosixPath(normalized).parts[:-1]:
        current /= component
        try:
            info = current.lstat()
        except FileNotFoundError:
            continue
        if stat.S_ISLNK(info.st_mode) or not stat.S_ISDIR(info.st_mode):
            raise BulkloadError(
                f"destination ancestry is not a real directory: {current}"
            )
    return root / normalized


def capacity_observation(path: Path) -> dict[str, int]:
    try:
        info = os.statvfs(path)
    except OSError as error:
        raise BulkloadError(f"cannot observe capacity for {path}") from error
    return {
        "available_bytes": info.f_bavail * info.f_frsize,
        "free_bytes": info.f_bfree * info.f_frsize,
        "total_bytes": info.f_blocks * info.f_frsize,
    }


def require_capacity(
    path: Path, *, charged_bytes: int, reserve_bytes: int
) -> dict[str, int]:
    if charged_bytes < 0 or reserve_bytes < 0:
        raise BulkloadError("capacity charges must be non-negative")
    observed = capacity_observation(path)
    required = charged_bytes + reserve_bytes
    if observed["available_bytes"] < required:
        raise BulkloadError(
            "capacity gate failed: exact charged bytes plus reserve exceed available bytes"
        )
    return {
        **observed,
        "charged_bytes": charged_bytes,
        "reserve_bytes": reserve_bytes,
        "required_bytes": required,
    }


def _linux_reflink(source_fd: int, destination_fd: int) -> None:
    import fcntl

    fcntl.ioctl(destination_fd, 0x40049409, source_fd)


def _darwin_clonefile(source: Path, destination: Path) -> None:
    libc = ctypes.CDLL(None, use_errno=True)
    try:
        clonefile = libc.clonefile
    except AttributeError as error:
        raise OSError(errno.ENOTSUP, "clonefile unavailable") from error
    clonefile.argtypes = (ctypes.c_char_p, ctypes.c_char_p, ctypes.c_int)
    clonefile.restype = ctypes.c_int
    if clonefile(os.fsencode(source), os.fsencode(destination), 0) != 0:
        number = ctypes.get_errno()
        raise OSError(number, os.strerror(number), os.fspath(destination))


def reflink_clone(
    source: Path,
    destination: Path,
    *,
    expected_sha256: str | None = None,
    mode: int | None = None,
) -> dict[str, Any]:
    """Create and rehash a clone. Unsupported cloning is always an error."""
    source = resolve_real(source)
    requested = Path(os.path.abspath(os.fspath(destination)))
    destination = requested
    source_info = source.stat(follow_symlinks=False)
    if not stat.S_ISREG(source_info.st_mode):
        raise BulkloadError(f"reflink source is not a regular file: {source}")
    durable_makedirs(destination.parent)
    if destination.exists() or destination.is_symlink():
        raise BulkloadError(f"reflink destination already exists: {destination}")
    temporary = destination.parent / f".{destination.name}.reflink-{new_id()}"
    try:
        if sys_platform() == "darwin":
            _darwin_clonefile(source, temporary)
            descriptor = os.open(temporary, os.O_RDONLY | getattr(os, "O_CLOEXEC", 0))
        else:
            source_fd = os.open(
                source,
                os.O_RDONLY
                | getattr(os, "O_NOFOLLOW", 0)
                | getattr(os, "O_CLOEXEC", 0),
            )
            try:
                descriptor = os.open(
                    temporary,
                    os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_CLOEXEC", 0),
                    0o600,
                )
                try:
                    _linux_reflink(source_fd, descriptor)
                except BaseException:
                    os.close(descriptor)
                    descriptor = -1
                    raise
            finally:
                os.close(source_fd)
        try:
            os.fchmod(
                descriptor,
                mode if mode is not None else stat.S_IMODE(source_info.st_mode),
            )
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
        observed = sha256_file(temporary)
        expected = expected_sha256 or sha256_file(source)
        if observed != expected:
            raise BulkloadError("reflink verification digest mismatch")
        os.replace(temporary, destination)
        fsync_directory(destination.parent)
        return {
            "method": "reflink",
            "bytes": source_info.st_size,
            "sha256": observed,
            "source_device": source_info.st_dev,
            "destination_device": destination.stat().st_dev,
        }
    except (OSError, BulkloadError) as error:
        temporary.unlink(missing_ok=True)
        if isinstance(error, BulkloadError):
            raise
        raise BulkloadError(
            f"required reflink clone failed for {destination}; full-copy fallback is forbidden"
        ) from error


def accounted_copy(
    source: Path,
    destination: Path,
    *,
    expected_sha256: str,
    mode: int,
) -> dict[str, Any]:
    """An explicitly capacity-charged copy; callers must gate it first."""
    source = resolve_real(source)
    requested = Path(os.path.abspath(os.fspath(destination)))
    destination = requested
    durable_makedirs(destination.parent)
    if destination.exists() or destination.is_symlink():
        raise BulkloadError(f"copy destination already exists: {destination}")
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f".{destination.name}.copy-", dir=destination.parent
    )
    temporary = Path(temporary_name)
    try:
        os.fchmod(descriptor, mode)
        with (
            source.open("rb", buffering=0) as input_stream,
            os.fdopen(descriptor, "wb", closefd=True) as output_stream,
        ):
            shutil.copyfileobj(input_stream, output_stream, 1024 * 1024)
            output_stream.flush()
            os.fsync(output_stream.fileno())
        observed = sha256_file(temporary)
        if observed != expected_sha256:
            raise BulkloadError("capacity-accounted copy digest mismatch")
        os.replace(temporary, destination)
        fsync_directory(destination.parent)
        return {
            "method": "capacity-accounted-copy",
            "bytes": source.stat().st_size,
            "sha256": observed,
        }
    except BaseException:
        try:
            os.close(descriptor)
        except OSError:
            pass
        temporary.unlink(missing_ok=True)
        raise


def sys_platform() -> str:
    import sys

    return sys.platform
