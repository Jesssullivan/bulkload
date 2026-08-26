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
import threading
from typing import Any, Iterable, Iterator, Sequence
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


def utc_now() -> str:
    return datetime.now(UTC).isoformat(timespec="seconds").replace("+00:00", "Z")


def new_id() -> str:
    return str(uuid.uuid4())


class CanonicalNode:
    """A value that emits its own canonical JSON bytes without materializing them.

    A node is deliberately *not* a ``list`` or ``dict`` subclass. The C encoder
    behind :func:`json.dumps` reads a list subclass through the concrete list
    storage and would silently emit ``[]`` for a lazily backed sequence; by
    staying outside those types a node makes ``json.dumps`` raise
    :class:`TypeError` instead, so the fast path fails closed and the streaming
    emitter -- the single definition of the canonical form -- takes over.
    """

    def canonical_chunks(self) -> Iterator[bytes]:
        raise NotImplementedError


# The canonical form is exactly ``json.dumps(value, allow_nan=False,
# ensure_ascii=False, separators=(",", ":"), sort_keys=True).encode("utf-8")``.
# Every recorded digest in Bulkload is taken over those bytes, so the streaming
# emitter below reproduces them byte for byte -- same key order, same escapes,
# same number formatting -- and is differentially tested against json.dumps.
_CANONICAL_STRING = json.encoder.encode_basestring
_CANONICAL_INFINITY = float("inf")


def _canonical_number(value: float) -> str:
    if value != value or value == _CANONICAL_INFINITY or value == -_CANONICAL_INFINITY:
        raise ValueError(
            "Out of range float values are not JSON compliant: " + repr(value)
        )
    return float.__repr__(value)


def _canonical_scope(
    container: Any, markers: dict[int, Any], chunks: Iterator[bytes]
) -> Iterator[bytes]:
    marker = id(container)
    if marker in markers:
        raise ValueError("Circular reference detected")
    markers[marker] = container
    yield from chunks
    del markers[marker]


def _canonical_array(value: Any, markers: dict[int, Any]) -> Iterator[bytes]:
    first = True
    for item in value:
        yield b"[" if first else b","
        first = False
        yield from _canonical_walk(item, markers)
    yield b"[]" if first else b"]"


def _canonical_object(
    value: dict[Any, Any], markers: dict[int, Any]
) -> Iterator[bytes]:
    first = True
    # ``sort_keys=True`` sorts the raw keys before they are coerced to strings,
    # and raises the same TypeError json.dumps raises on mixed key types.
    for key, item in sorted(value.items()):
        if isinstance(key, str):
            name = key
        elif isinstance(key, float):
            name = _canonical_number(key)
        elif key is True:
            name = "true"
        elif key is False:
            name = "false"
        elif key is None:
            name = "null"
        elif isinstance(key, int):
            name = int.__repr__(key)
        else:
            raise TypeError(
                f"keys must be str, int, float, bool or None, "
                f"not {key.__class__.__name__}"
            )
        yield b"{" if first else b","
        first = False
        yield _CANONICAL_STRING(name).encode("utf-8")
        yield b":"
        yield from _canonical_walk(item, markers)
    yield b"{}" if first else b"}"


def _canonical_walk(value: Any, markers: dict[int, Any]) -> Iterator[bytes]:
    if isinstance(value, str):
        yield _CANONICAL_STRING(value).encode("utf-8")
    elif value is None:
        yield b"null"
    elif value is True:
        yield b"true"
    elif value is False:
        yield b"false"
    elif isinstance(value, int):
        yield int.__repr__(value).encode("ascii")
    elif isinstance(value, float):
        yield _canonical_number(value).encode("ascii")
    elif isinstance(value, CanonicalNode):
        yield from value.canonical_chunks()
    elif isinstance(value, (list, tuple)):
        yield from _canonical_scope(value, markers, _canonical_array(value, markers))
    elif isinstance(value, dict):
        yield from _canonical_scope(value, markers, _canonical_object(value, markers))
    else:
        raise TypeError(
            f"Object of type {value.__class__.__name__} is not JSON serializable"
        )


def canonical_chunks(value: Any) -> Iterator[bytes]:
    """Yield exactly the bytes :func:`canonical_bytes` returns, incrementally.

    Peak memory is the largest single chunk rather than two full copies of the
    payload, so a multi-GiB catalog can be sealed, written and re-proved without
    ever existing as one ``str`` and one ``bytes`` at the same time.
    """
    return _canonical_walk(value, {})


def canonical_bytes(value: Any) -> bytes:
    try:
        return json.dumps(
            value,
            allow_nan=False,
            ensure_ascii=False,
            separators=(",", ":"),
            sort_keys=True,
        ).encode("utf-8")
    except ValueError as error:
        raise BulkloadError(f"value is not canonical JSON: {error}") from error
    except TypeError:
        # A spilled node is opaque to the C encoder by construction, so this is
        # the fail-closed handoff to the streaming emitter rather than a bug.
        pass
    payload = bytearray()
    try:
        for chunk in canonical_chunks(value):
            payload += chunk
    except (TypeError, ValueError) as error:
        raise BulkloadError(f"value is not canonical JSON: {error}") from error
    return bytes(payload)


def canonical_sha256(value: Any) -> str:
    """Digest the canonical form without ever holding it whole."""
    digest = hashlib.sha256()
    try:
        for chunk in canonical_chunks(value):
            digest.update(chunk)
    except (TypeError, ValueError) as error:
        raise BulkloadError(f"value is not canonical JSON: {error}") from error
    return digest.hexdigest()


def canonical_length(value: Any) -> int:
    """Count the canonical bytes without allocating them."""
    total = 0
    try:
        for chunk in canonical_chunks(value):
            total += len(chunk)
    except (TypeError, ValueError) as error:
        raise BulkloadError(f"value is not canonical JSON: {error}") from error
    return total


def canonical_line_chunks(value: Any) -> Iterator[bytes]:
    """Stream the canonical form followed by the trailing newline evidence uses."""
    try:
        yield from canonical_chunks(value)
    except (TypeError, ValueError) as error:
        raise BulkloadError(f"value is not canonical JSON: {error}") from error
    yield b"\n"


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


SHA256_DIGEST_BYTES = 32
DEFAULT_SPILL_RECORDS = 65536


class _RecordArena:
    """One unlinked scratch file shared by every spilled record list.

    A file descriptor per spilled session file would exhaust ``RLIMIT_NOFILE``
    long before it exhausted memory, so all spills share a single arena and each
    list keeps the extents it owns. Appends are serialized under a lock because
    ``_capture_provider`` classifies files on a bounded thread pool; reads go
    through ``os.pread`` so they never disturb another reader's position.
    """

    __slots__ = ("_descriptor", "_end", "_lock")

    def __init__(self, directory: str | None) -> None:
        try:
            descriptor, path = tempfile.mkstemp(
                prefix="bulkload-records-", dir=directory
            )
            os.unlink(path)
        except OSError as error:
            raise BulkloadError("cannot open the Bulkload record spill") from error
        self._descriptor = descriptor
        self._end = 0
        self._lock = threading.Lock()

    def write(self, payload: bytes) -> int:
        with self._lock:
            offset = self._end
            self._end += len(payload)
        written = 0
        while written < len(payload):
            try:
                written += os.pwrite(
                    self._descriptor, payload[written:], offset + written
                )
            except OSError as error:
                raise BulkloadError(
                    "cannot persist the Bulkload record spill"
                ) from error
        return offset

    def read(self, offset: int, size: int) -> bytes:
        try:
            return os.pread(self._descriptor, size, offset)
        except OSError as error:
            raise BulkloadError("cannot read the Bulkload record spill") from error


_ARENA: _RecordArena | None = None
_ARENA_LOCK = threading.Lock()


def record_arena() -> _RecordArena:
    global _ARENA
    with _ARENA_LOCK:
        if _ARENA is None:
            _ARENA = _RecordArena(os.environ.get("BULKLOAD_SPILL_DIR") or None)
        return _ARENA


def spill_record_threshold() -> int:
    """How many per-line digests may stay resident before a list spills.

    ``0`` keeps every list in memory, which is the pre-spill behaviour and what
    the determinism tests pin when they want the two paths compared directly.
    """
    raw = os.environ.get("BULKLOAD_SPILL_RECORDS")
    if raw is None:
        return DEFAULT_SPILL_RECORDS
    try:
        value = int(raw)
    except ValueError:
        raise BulkloadError(
            "BULKLOAD_SPILL_RECORDS must be a non-negative integer"
        ) from None
    if value < 0:
        raise BulkloadError("BULKLOAD_SPILL_RECORDS must be a non-negative integer")
    return value


class HexRecords(CanonicalNode):
    """An ordered, prefix-comparable list of sha-256 digests held on disk.

    The catalog carries one digest per line of every append-only session file.
    Resident, that is a 64-character ``str`` object plus a list slot per line --
    the term that actually exhausts memory on a multi-GiB corpus, well before
    the serialized catalog approaches its own bound. Spilled, each digest is its
    32 raw bytes in the shared arena, and the catalog still serializes to the
    identical canonical bytes because this node emits the same JSON array the
    resident list would.

    The consumer of these lists is the planner's append-only relation, which
    needs ordered prefix comparison and nothing else, so iteration and length
    are the whole contract.
    """

    __slots__ = ("_arena", "_extents", "_pending", "_length", "_sealed")

    _FLUSH_BYTES = 1 << 20
    _EMIT_BYTES = 1 << 16

    def __init__(self, arena: _RecordArena | None = None) -> None:
        self._arena = arena if arena is not None else record_arena()
        self._extents: list[tuple[int, int]] = []
        self._pending = bytearray()
        self._length = 0
        self._sealed = False

    @classmethod
    def from_hexdigests(
        cls, values: Iterable[str], arena: _RecordArena | None = None
    ) -> "HexRecords":
        records = cls(arena)
        for value in values:
            records.append(value)
        return records

    def append(self, value: str) -> None:
        if self._sealed:
            raise BulkloadError("a sealed Bulkload record spill cannot grow")
        try:
            raw = bytes.fromhex(value)
        except (TypeError, ValueError) as error:
            raise BulkloadError(
                "spilled records must be sha-256 hexadecimal digests"
            ) from error
        if len(raw) != SHA256_DIGEST_BYTES or raw.hex() != value:
            raise BulkloadError(
                "spilled records must be lowercase sha-256 hexadecimal digests"
            )
        self._pending += raw
        self._length += 1
        if len(self._pending) >= self._FLUSH_BYTES:
            self._flush()

    def _flush(self) -> None:
        if not self._pending:
            return
        payload = bytes(self._pending)
        self._pending.clear()
        self._extents.append((self._arena.write(payload), len(payload)))

    def finish(self) -> "HexRecords":
        self._flush()
        self._sealed = True
        return self

    def __len__(self) -> int:
        return self._length

    def __bool__(self) -> bool:
        return bool(self._length)

    def __iter__(self) -> Iterator[str]:
        self._flush()
        for offset, size in self._extents:
            read = 0
            carry = b""
            while read < size:
                block = self._arena.read(
                    offset + read, min(self._EMIT_BYTES, size - read)
                )
                if not block:
                    raise BulkloadError("the Bulkload record spill is truncated")
                read += len(block)
                if carry:
                    block = carry + block
                usable = len(block) - len(block) % SHA256_DIGEST_BYTES
                for start in range(0, usable, SHA256_DIGEST_BYTES):
                    yield block[start : start + SHA256_DIGEST_BYTES].hex()
                carry = block[usable:]
            if carry:
                raise BulkloadError("the Bulkload record spill is truncated")

    def __getitem__(self, index: int) -> str:
        if not isinstance(index, int) or isinstance(index, bool):
            raise TypeError("spilled records accept integer indices only")
        wanted = index + self._length if index < 0 else index
        if wanted < 0 or wanted >= self._length:
            raise IndexError("spilled record index out of range")
        for position, value in enumerate(self):
            if position == wanted:
                return value
        raise IndexError("spilled record index out of range")

    def __eq__(self, other: Any) -> bool:
        if isinstance(other, (HexRecords, list, tuple)):
            if len(self) != len(other):
                return False
            return all(left == right for left, right in zip(self, other))
        return NotImplemented

    __hash__ = None  # type: ignore[assignment]

    def __repr__(self) -> str:
        return f"HexRecords(length={self._length})"

    def to_list(self) -> list[str]:
        return list(self)

    def canonical_chunks(self) -> Iterator[bytes]:
        block = bytearray(b"[")
        first = True
        for value in self:
            if first:
                first = False
            else:
                block += b","
            block += b'"'
            block += value.encode("ascii")
            block += b'"'
            if len(block) >= self._EMIT_BYTES:
                yield bytes(block)
                block.clear()
        block += b"]"
        yield bytes(block)


def ordered_prefix_relation(source: Sequence[str], destination: Sequence[str]) -> str:
    """Classify two ordered record lists without slicing or materializing either.

    This is the append-only semantics the whole design rests on: ``equal``,
    ``source-superset`` when the destination is a strict prefix of the source,
    ``destination-superset`` when the source is a strict prefix of the
    destination, and ``divergent`` the moment the common prefix disagrees. It
    reads both operands as ordered iterables, so a resident list and a spilled
    :class:`HexRecords` compare identically.
    """
    for left, right in zip(source, destination):
        if left != right:
            return "divergent"
    source_length = len(source)
    destination_length = len(destination)
    if source_length == destination_length:
        return "equal"
    return (
        "source-superset"
        if source_length > destination_length
        else ("destination-superset")
    )


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
    return canonical_sha256(
        {key: item for key, item in value.items() if key != digest_field}
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


# The old 4 GiB bound was a memory wall, not a custody one: sealing and writing
# evidence cost two full copies of the payload on top of the payload itself, so
# the durable bound had to sit far below what the format could carry. Sealing,
# writing and re-proving now stream, and reads allocate the file's real size
# rather than the bound, so the durable bound is what a destination can hold --
# with `_require_large_evidence_memory` still gating whatever must be parsed.
MAX_JSON_BYTES = 16 * 1024**3


def read_json(path: Path, *, max_bytes: int = MAX_JSON_BYTES) -> dict[str, Any]:
    try:
        info = path.stat(follow_symlinks=False)
        if not stat.S_ISREG(info.st_mode) or info.st_size > max_bytes:
            raise BulkloadError(f"JSON input is not a bounded regular file: {path}")
        with path.open("rb") as stream:
            # Reading ``max_bytes + 1`` asked the buffered reader to allocate the
            # whole bound up front on every call, however small the file. The
            # file's own size plus one is the same growth check for a fraction of
            # the allocation: a file that grew past it reads back truncated and
            # fails to parse, exactly as before.
            payload = stream.read(info.st_size + 1)
        if len(payload) > info.st_size:
            raise BulkloadError(f"JSON input grew while it was read: {path}")
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


def atomic_write_chunks(
    path: Path,
    chunks: Iterable[bytes],
    mode: int = 0o600,
    *,
    max_bytes: int | None = None,
    bound_error: str = "JSON output exceeds the bounded evidence contract",
) -> int:
    """Publish a stream of chunks atomically, returning the bytes written.

    ``max_bytes`` is enforced while the temporary is still private, so an
    oversized payload never becomes a visible file: the bound fails closed with
    nothing durable left behind, without the payload ever existing whole in
    memory. ``bound_error`` lets the caller keep naming its own bound.
    """
    requested = Path(os.path.abspath(os.fspath(path.expanduser())))
    path = requested
    durable_makedirs(path.parent)
    descriptor, temporary = tempfile.mkstemp(
        prefix=f".{path.name}.bulkload-", dir=path.parent
    )
    temporary_path = Path(temporary)
    written = 0
    try:
        os.fchmod(descriptor, mode)
        with os.fdopen(descriptor, "wb", closefd=True) as output:
            for chunk in chunks:
                written += len(chunk)
                if max_bytes is not None and written > max_bytes:
                    raise BulkloadError(bound_error)
                output.write(chunk)
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
    return written


def atomic_write(path: Path, payload: bytes, mode: int = 0o600) -> None:
    atomic_write_chunks(path, (payload,), mode)


def atomic_write_json(path: Path, value: dict[str, Any]) -> None:
    atomic_write_chunks(path, canonical_line_chunks(value))


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
