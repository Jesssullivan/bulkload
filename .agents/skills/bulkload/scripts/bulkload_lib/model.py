"""Canonical JSON, digest, path, and persistence helpers."""

from __future__ import annotations

from datetime import UTC, datetime
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import tempfile
from typing import Any
import unicodedata
from urllib.parse import urlsplit, urlunsplit

SNAPSHOT_SCHEMA = "dev.tinyland.bulkload.snapshot.v1"
PLAN_SCHEMA = "dev.tinyland.bulkload.plan.v1"
RECEIPT_SCHEMA = "dev.tinyland.bulkload.receipt.v1"
VERIFY_SCHEMA = "dev.tinyland.bulkload.verify.v1"


class BulkloadError(RuntimeError):
    """A fail-closed protocol or safety error."""


def utc_now() -> str:
    return datetime.now(UTC).isoformat(timespec="seconds").replace("+00:00", "Z")


def canonical_bytes(value: Any) -> bytes:
    """Return the single v1 canonical JSON encoding."""
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


def object_digest(value: dict[str, Any], digest_field: str) -> str:
    return sha256_bytes(
        canonical_bytes(
            {key: item for key, item in value.items() if key != digest_field}
        )
    )


def require_digest(value: dict[str, Any], digest_field: str) -> None:
    actual = value.get(digest_field)
    expected = object_digest(value, digest_field)
    if actual != expected:
        raise BulkloadError(
            f"{digest_field} mismatch: recorded {actual!r}, computed {expected}"
        )


def read_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(
            path.read_bytes(),
            object_pairs_hook=_unique_object,
            parse_constant=_reject_nonfinite,
        )
    except (OSError, UnicodeDecodeError, json.JSONDecodeError, ValueError) as error:
        raise BulkloadError(f"cannot read JSON object {path}: {error}") from error
    if not isinstance(value, dict):
        raise BulkloadError(f"{path} must contain a JSON object")
    return value


def _reject_nonfinite(value: str) -> None:
    raise ValueError(f"non-finite JSON number {value} is forbidden")


def _unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, item in pairs:
        if key in value:
            raise ValueError(f"duplicate JSON key {key!r} is forbidden")
        value[key] = item
    return value


def atomic_write(path: Path, payload: bytes, mode: int = 0o600) -> None:
    """Write, fsync, rename, and fsync the containing directory."""
    path = path.expanduser()
    path = path.parent.resolve() / path.name
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
        _fsync_directory(path.parent)
    except BaseException:
        try:
            os.close(descriptor)
        except OSError:
            pass
        temporary_path.unlink(missing_ok=True)
        raise


def atomic_write_json(path: Path, value: dict[str, Any]) -> None:
    atomic_write(path, canonical_bytes(value) + b"\n")


def _fsync_directory(path: Path) -> None:
    flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
    descriptor = os.open(path, flags)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def durable_makedirs(path: Path, mode: int = 0o700) -> None:
    """Create a real directory chain and persist every new directory entry."""
    absolute = Path(os.path.abspath(os.fspath(path)))
    components = absolute.parts
    if not components:
        raise BulkloadError("directory path must not be empty")

    flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0)
    anchor = absolute.anchor or "."
    descriptor = os.open(anchor, flags)
    traversed = Path(anchor)
    try:
        start = 1 if absolute.anchor else 0
        for component in components[start:]:
            child_path = traversed / component
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
                        f"directory path is not a real directory: {child_path}"
                    ) from error
            except OSError as error:
                raise BulkloadError(
                    f"directory path is not a real directory: {child_path}"
                ) from error
            try:
                if created:
                    os.fsync(child)
                    os.fsync(descriptor)
            except BaseException:
                os.close(child)
                raise
            os.close(descriptor)
            descriptor = child
            traversed = child_path
    finally:
        os.close(descriptor)


def normalize_relative(value: str, *, allow_dot: bool = False) -> str:
    if not isinstance(value, str) or "\x00" in value:
        raise BulkloadError("relative path must be a NUL-free string")
    if any(unicodedata.category(character) in {"Cc", "Cf"} for character in value):
        raise BulkloadError("control and format characters are forbidden in paths")
    candidate = PurePosixPath(value)
    if candidate.is_absolute():
        raise BulkloadError(f"absolute path is forbidden: {value!r}")
    parts = candidate.parts
    if any(part in {"", ".", ".."} for part in parts):
        if allow_dot and value == ".":
            return "."
        raise BulkloadError(f"non-canonical relative path is forbidden: {value!r}")
    normalized = candidate.as_posix()
    if not normalized or normalized == ".":
        if allow_dot:
            return "."
        raise BulkloadError("empty relative path is forbidden")
    if value != normalized:
        raise BulkloadError(f"non-canonical relative path is forbidden: {value!r}")
    return normalized


def safe_join(root: Path, relative: str, *, allow_leaf_symlink: bool = True) -> Path:
    normalized = normalize_relative(relative)
    root = root.resolve()
    current = root
    parts = PurePosixPath(normalized).parts
    for index, component in enumerate(parts):
        current = current / component
        is_leaf = index == len(parts) - 1
        try:
            info = current.lstat()
        except FileNotFoundError:
            continue
        if stat.S_ISLNK(info.st_mode) and (not is_leaf or not allow_leaf_symlink):
            raise BulkloadError(f"path traverses a symlink: {current}")
    return current


def sanitize_remote_url(raw: str) -> str:
    """Return a credential-safe supported Git remote locator."""
    if (
        not raw
        or "\x00" in raw
        or any(unicodedata.category(character) in {"Cc", "Cf"} for character in raw)
    ):
        raise BulkloadError("unsupported Git remote locator")
    if re.match(r"^[A-Za-z][A-Za-z0-9+.-]*::", raw):
        raise BulkloadError("unsupported Git remote helper locator")
    if "://" in raw:
        try:
            parsed = urlsplit(raw)
            scheme = parsed.scheme.lower()
            if scheme not in {"file", "ftp", "ftps", "git", "http", "https", "ssh"}:
                raise BulkloadError("unsupported Git remote URL scheme")
            host = parsed.hostname
            port = parsed.port
        except ValueError as error:
            raise BulkloadError("malformed Git remote URL") from error
        if scheme != "file" and not host:
            raise BulkloadError("malformed Git remote URL")
        if host and ":" in host:
            host = f"[{host}]"
        authority = host or ""
        if port is not None:
            authority = f"{authority}:{port}"
        sanitized = urlunsplit((scheme, authority, parsed.path, "", ""))
        if scheme == "file":
            return f"local-path:sha256:{sha256_bytes(sanitized.encode('utf-8'))}"
        return sanitized

    locator = raw.split("?", 1)[0].split("#", 1)[0]
    at = locator.rfind("@")
    host_start = at + 1
    separator = -1
    if host_start < len(locator) and locator[host_start] == "[":
        closing = locator.find("]", host_start + 1)
        if closing >= 0 and closing + 1 < len(locator) and locator[closing + 1] == ":":
            separator = closing + 1
    else:
        separator = locator.find(":", host_start)
    if separator > host_start:
        host = locator[host_start:separator]
        path = locator[separator + 1 :]
        if (
            path
            and "@" not in path
            and "/" not in host
            and not any(character.isspace() for character in host + path)
        ):
            return f"{host}:{path}"
    if "@" in raw:
        raise BulkloadError("unsupported Git remote locator")
    return f"local-path:sha256:{sha256_bytes(raw.encode('utf-8'))}"


SENSITIVE_EXACT = {
    ".env",
    ".envrc",
    ".git-credentials",
    "auth.json",
    ".credentials.json",
    ".npmrc",
    ".pypirc",
    "credentials.json",
    "credentials",
    "credentials.toml",
    "credentials.tfrc.json",
    "cookies",
    "id_rsa",
    "id_ed25519",
    "id_ecdsa",
    "id_dsa",
    ".netrc",
    "kubeconfig",
    "hosts.yml",
    "login data",
    "web data",
}
SENSITIVE_SUFFIXES = (
    ".db",
    ".pem",
    ".key",
    ".p12",
    ".pfx",
    ".sqlite",
    ".sqlite3",
    "-wal",
    "-shm",
)
SENSITIVE_COMPONENTS = {
    ".aws",
    ".docker",
    ".gem",
    ".gnupg",
    ".kube",
    ".ssh",
    ".terraform.d",
    "browser-profile",
    "chromium-profile",
}


def sensitive_reason(path: str) -> str | None:
    parts = PurePosixPath(path).parts
    lowered = tuple(part.lower() for part in parts)
    for component in lowered:
        if component in SENSITIVE_COMPONENTS:
            return f"sensitive path component {component}"
    leaf = lowered[-1] if lowered else ""
    if leaf in SENSITIVE_EXACT:
        return f"sensitive filename {leaf}"
    if leaf.startswith(".env.") and leaf not in {".env.example", ".env.sample"}:
        return f"sensitive environment filename {leaf}"
    if leaf.endswith(SENSITIVE_SUFFIXES):
        return f"sensitive or live-state suffix on {leaf}"
    if "private" in leaf and "key" in leaf:
        return f"private-key-shaped filename {leaf}"
    return None


def portability_reason(path: str) -> str | None:
    for component in PurePosixPath(path).parts:
        if component.startswith("._"):
            return f"AppleDouble metadata is quarantined in v1: {component}"
    return None
