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
    "mover.py",
    "scanner.py",
    "planner.py",
    "executor.py",
    "cli.py",
)


class BulkloadError(RuntimeError):
    """A fail-closed protocol, custody, or safety error.

    `refusal` carries the structured refusal record for the sites that have
    one. It is keyword-only and defaults to None so that every existing
    `BulkloadError("...")` construction keeps its exact behaviour, and so an
    unconverted site still refuses in precisely the same way — it just has no
    record to write. Nothing reads `refusal` on the success path.
    """

    def __init__(self, *args: Any, refusal: dict[str, Any] | None = None) -> None:
        super().__init__(*args)
        self.refusal = refusal


REFUSAL_SCHEMA = "dev.tinyland.bulkload.refusal.v1"
REFUSAL_SAMPLE_LIMIT = 20
REFUSAL_STRING_LIMIT = 512
REFUSAL_SEQUENCE_LIMIT = 20
REFUSAL_MAPPING_LIMIT = 40
REFUSAL_DEPTH_LIMIT = 6
UNCLASSIFIED_REFUSAL_CODE = "UNCLASSIFIED"


def refusal_json(value: Any, *, depth: int = 0) -> Any:
    """Coerce an arbitrary observation into bounded, canonical-JSON-safe data.

    A refusal record is written on the failure path of a ceremony that may be
    holding 1.8M index records, so every field is bounded here rather than at
    each call site: strings are truncated, sequences and mappings are capped,
    and recursion stops at a fixed depth. Anything that is not representable
    becomes its `repr`, because a record that cannot be serialized is a record
    the operator never sees.
    """
    if depth > REFUSAL_DEPTH_LIMIT:
        return "<depth-limited>"
    if value is None or isinstance(value, bool):
        return value
    if isinstance(value, int):
        return value
    if isinstance(value, float):
        return value if math.isfinite(value) else repr(value)
    if isinstance(value, str):
        return _refusal_text(value)
    if isinstance(value, (bytes, bytearray)):
        return _refusal_text(bytes(value).hex())
    if isinstance(value, os.PathLike):
        return _refusal_text(os.fspath(value))
    if isinstance(value, dict):
        items = list(value.items())[:REFUSAL_MAPPING_LIMIT]
        return {
            _refusal_text(str(key)): refusal_json(item, depth=depth + 1)
            for key, item in items
        }
    if isinstance(value, (set, frozenset)):
        try:
            ordered = sorted(value, key=lambda item: str(item))
        except TypeError:
            ordered = list(value)
        return [
            refusal_json(item, depth=depth + 1)
            for item in ordered[:REFUSAL_SEQUENCE_LIMIT]
        ]
    if isinstance(value, (list, tuple)):
        return [
            refusal_json(item, depth=depth + 1)
            for item in list(value)[:REFUSAL_SEQUENCE_LIMIT]
        ]
    return _refusal_text(repr(value))


def _refusal_text(value: str) -> str:
    flattened = value.replace("\n", "\\n").replace("\r", "\\r")
    if len(flattened) > REFUSAL_STRING_LIMIT:
        return flattened[:REFUSAL_STRING_LIMIT] + "..."
    return flattened


def refusal_record(
    *,
    code: str,
    phase: str,
    remedy: str,
    message: str,
    root: Any = None,
    label: Any = None,
    field: Any = None,
    expected: Any = None,
    observed: Any = None,
    count: Any = None,
    sample: Iterable[Any] = (),
) -> dict[str, Any]:
    """Build one structured refusal record.

    The shape is fixed so an agent can branch on it without sniffing: every
    key is always present, `sample` is always a list bounded by
    REFUSAL_SAMPLE_LIMIT, and `message` is always the exact single line that
    also went to stderr.
    """
    return {
        "schema": REFUSAL_SCHEMA,
        "code": code,
        "phase": phase,
        "root": refusal_json(root),
        "label": refusal_json(label),
        "field": refusal_json(field),
        "expected": refusal_json(expected),
        "observed": refusal_json(observed),
        "count": refusal_json(count),
        "sample": [refusal_json(item) for item in list(sample)[:REFUSAL_SAMPLE_LIMIT]],
        "remedy": _refusal_text(remedy),
        "message": _refusal_text(message),
    }


def refuse(
    message: str,
    *,
    code: str,
    phase: str,
    remedy: str,
    detail: str = "",
    root: Any = None,
    label: Any = None,
    field: Any = None,
    expected: Any = None,
    observed: Any = None,
    count: Any = None,
    sample: Iterable[Any] = (),
) -> BulkloadError:
    """Return a BulkloadError whose message is the original static string.

    `detail` is appended after a colon. Keeping the original sentence as the
    exact prefix is deliberate: every caller, runbook, and test that matches
    on the historical text keeps matching, while the operator finally gets the
    naming clause that decides what to do next.
    """
    line = f"{message}: {detail}" if detail else message
    line = _refusal_text(line)
    return BulkloadError(
        line,
        refusal=refusal_record(
            code=code,
            phase=phase,
            remedy=remedy,
            message=line,
            root=root,
            label=label,
            field=field,
            expected=expected,
            observed=observed,
            count=count,
            sample=sample,
        ),
    )


def refusal_eq(
    suffix: str, field: str, expected: Any, observed: Any
) -> tuple[Any, ...]:
    """An equality check for `first_mismatch`; both sides are lazy thunks."""
    return (suffix, field, None, expected, observed)


def refusal_check(
    suffix: str,
    field: str,
    predicate: Any,
    expected: Any = None,
    observed: Any = None,
) -> tuple[Any, ...]:
    """A general check for `first_mismatch`; `predicate` holds when True."""
    return (suffix, field, predicate, expected, observed)


def _refusal_thunk(value: Any) -> Any:
    return value() if callable(value) else value


def first_mismatch(family: str, checks: Iterable[tuple[Any, ...]]) -> dict[str, Any]:
    """Name the first failing condition of an already-decided refusal.

    This runs *after* the guarding `if` has decided to refuse, so it never
    changes control flow and never costs anything on the success path. Checks
    are evaluated lazily and in the source order of the original boolean
    chain, and any exception inside a check counts as that check failing —
    which is exactly what the original short-circuit `or` would have meant.
    Returns kwargs for `refuse`; when nothing reproduces (a racing observation
    that settled between the guard and the diagnosis) the family code is
    returned unqualified rather than a wrong name.
    """
    for suffix, field, predicate, expected, observed in checks:
        try:
            held = (
                bool(predicate())
                if predicate is not None
                else _refusal_thunk(expected) == _refusal_thunk(observed)
            )
        except Exception:  # noqa: BLE001 - a failing probe is a failing check
            held = False
        if held:
            continue
        try:
            expected_value = _refusal_thunk(expected)
        except Exception:  # noqa: BLE001
            expected_value = "<unavailable>"
        try:
            observed_value = _refusal_thunk(observed)
        except Exception:  # noqa: BLE001
            observed_value = "<unavailable>"
        return {
            "code": f"{family}_{suffix}",
            "field": field,
            "expected": expected_value,
            "observed": observed_value,
            "detail": f"field={field} expected={_refusal_brief(expected_value)} "
            f"observed={_refusal_brief(observed_value)}",
        }
    return {
        "code": family,
        "field": None,
        "expected": None,
        "observed": None,
        "detail": "no single condition reproduced under diagnosis",
    }


def _refusal_brief(value: Any) -> str:
    if isinstance(value, str):
        text = value
    else:
        try:
            text = json.dumps(refusal_json(value), separators=(",", ":"))
        except (TypeError, ValueError):
            text = repr(value)
    text = text.replace("\n", "\\n").replace("\r", "\\r")
    return text if len(text) <= 120 else text[:120] + "..."


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


def git_environment() -> dict[str, str]:
    """One git invocation environment for both the scanner and the executor.

    It was spelled out byte-identically in each; a second copy of a subprocess
    environment is a place for the two to drift apart silently.
    """
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


def path_identity(path: str | Path, *, fold_case: bool = False) -> Path:
    """One comparable spelling for two path arguments.

    `~/a`, `a/../a` and an absolute `a` are three spellings of one directory
    entry, and on the case-insensitive APFS volume this ceremony runs on so is
    `A`. A bare `Path(x) == Path(y)` at a call site sees four distinct paths;
    every overlap and collision guard needs to see one. `fold_case` is opt-in
    because folding is the safe direction only for a guard that refuses: on a
    case-sensitive destination it can refuse two genuinely distinct names,
    which costs an operator one renamed argument, while not folding on a
    case-insensitive source silently writes over the file the guard exists to
    protect.

    A path that cannot be expanded at all refuses here, the same way
    `resolve_real` refuses one that cannot be resolved. `Path.expanduser()`
    raises `RuntimeError` for an unknown `~user`, which no caller of a guard
    catches; a guard must refuse, not crash.
    """
    try:
        expanded = os.fspath(Path(path).expanduser())
    except (OSError, RuntimeError) as error:
        raise BulkloadError(f"cannot expand path {path!r}") from error
    resolved = os.path.realpath(os.path.abspath(expanded))
    return Path(resolved.casefold() if fold_case else resolved)


def assert_no_overlap(
    path: Path, protected: Iterable[Path], label: str, *, fold_case: bool = False
) -> None:
    candidate = path_identity(path, fold_case=fold_case)
    for raw in protected:
        named = path_identity(raw)
        root = path_identity(raw, fold_case=fold_case)
        try:
            candidate.relative_to(root)
        except ValueError:
            pass
        else:
            raise BulkloadError(f"{label} overlaps live root {named}")
        try:
            root.relative_to(candidate)
        except ValueError:
            continue
        raise BulkloadError(f"{label} contains live root {named}")


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
