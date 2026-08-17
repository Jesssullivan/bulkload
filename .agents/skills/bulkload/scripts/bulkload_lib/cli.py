"""Command-line interface for the bulkload v1 protocol."""

from __future__ import annotations

import argparse
import ctypes
import errno
import json
import math
import os
from pathlib import Path
import secrets
import shutil
import socket
import stat
import subprocess
import sys
from typing import Any, Sequence

from . import __version__
from . import private_runtime
from .executor import apply_plan, export_copy_paths, verify_plan
from .model import (
    BulkloadError,
    atomic_write_json,
    canonical_bytes,
    durable_makedirs,
    read_json,
)
from .planner import compile_plan
from .private_apply import (
    apply_codex_private_install,
    compile_codex_private_install_plan,
    recover_codex_private_mutation,
    rollback_codex_private_install,
    verify_codex_private_install,
)
from .private_quiescence import (
    DEFAULT_PRIVATE_QUIESCENCE_TTL_SECONDS,
    acquire_codex_private_bulkload_lock,
    create_codex_private_quiescence_attestation,
    open_codex_private_quiescence_attestation,
)
from .private_sqlite_plan import (
    compile_codex_private_sqlite_compose_plan,
    validate_codex_private_sqlite_compose_plan_against_inputs,
)
from .private_sqlite_action_plan import (
    compile_codex_private_sqlite_action_plan,
    validate_codex_private_sqlite_action_plan_against_close,
)
from .private_sqlite_close import (
    capture_codex_private_sqlite_session_reclose,
    compile_codex_private_sqlite_close_request,
    validate_codex_private_sqlite_close_request,
    validate_codex_private_sqlite_close_request_against_inputs,
    validate_codex_private_sqlite_private_reclose_capture,
    validate_codex_private_sqlite_session_reclose_against_live,
    validate_codex_private_sqlite_session_reclose_capture,
)
from .private_state import (
    DEFAULT_BACKUP_TIMEOUT_SECONDS,
    DEFAULT_MAX_METADATA_BYTES,
    DEFAULT_MAX_METADATA_ENTRIES,
    DEFAULT_MAX_SQLITE_FAMILIES,
    DEFAULT_MAX_THREAD_ENTRIES,
    DEFAULT_MAX_THREAD_INDEX_BYTES,
    DEFAULT_MAX_TOTAL_SQLITE_BYTES,
    capture_codex_private_state,
    compile_codex_private_state_plan,
    private_quiescence_capture_record,
    read_codex_private_bundle,
    read_codex_private_state_plan,
    revalidate_codex_private_bundles,
    write_private_json_noreplace,
)
from .scanner import DEFAULT_MAX_BYTES, DEFAULT_MAX_FILES, capture_snapshot
from .sessions import (
    DEFAULT_MAX_SESSION_BYTES,
    DEFAULT_MAX_SESSION_CATALOG_BYTES,
    DEFAULT_MAX_SESSION_DIRECTORIES,
    DEFAULT_MAX_SESSION_ENTRIES,
    DEFAULT_MAX_SESSION_ERRORS,
    DEFAULT_MAX_SESSION_FILE_BYTES,
    DEFAULT_MAX_SESSION_FILES,
    DEFAULT_MAX_SESSION_OUTPUT_BYTES,
    DEFAULT_MAX_SESSION_PATH_BYTES,
    DEFAULT_MAX_SESSION_PATH_COMPONENTS,
    DEFAULT_MAX_SESSION_RECORD_BYTES,
    DEFAULT_MAX_SESSION_RECORDS_PER_FILE,
    MAX_CODEX_ROOT_LINEAGE,
    MAX_CODEX_SESSION_SNAPSHOT_BYTES,
    capture_codex_session_close_capture,
    capture_codex_session_prefix_proof,
    capture_codex_sessions,
    compile_codex_session_close_request,
    compile_codex_session_prefix_request,
    compile_codex_session_union_plan,
    validate_codex_session_close_capture,
    validate_codex_session_close_request,
    validate_codex_session_prefix_proof,
    validate_codex_session_prefix_request,
    validate_codex_session_snapshot,
)


def _reject_output_overlap(output: str, roots: Sequence[Path]) -> None:
    if output == "-":
        return
    target = Path(output).expanduser().resolve()
    for root in roots:
        resolved_root = root.expanduser().resolve()
        try:
            target.relative_to(resolved_root)
        except ValueError:
            continue
        raise BulkloadError(
            f"evidence output must be outside the captured root: {resolved_root}"
        )


def _reject_output_input_alias(output: str, inputs: Sequence[str]) -> None:
    """Keep a command's durable evidence inputs immutable."""
    target = Path(output).expanduser().resolve()
    for value in inputs:
        source = Path(value).expanduser().resolve()
        same_file = target == source
        if not same_file:
            try:
                same_file = os.path.samefile(target, source)
            except FileNotFoundError:
                pass
        if same_file:
            raise BulkloadError(f"evidence output aliases an input artifact: {source}")


def _write_json(path: str, value: dict[str, Any]) -> None:
    if path == "-":
        sys.stdout.buffer.write(canonical_bytes(value) + b"\n")
        return
    atomic_write_json(Path(path).expanduser(), value)


def _stable_artifact_stat(info: os.stat_result) -> tuple[int, ...]:
    return (
        info.st_dev,
        info.st_ino,
        info.st_uid,
        stat.S_IMODE(info.st_mode),
        info.st_nlink,
        info.st_size,
        info.st_mtime_ns,
        info.st_ctime_ns,
    )


def _read_pinned_codex_json(path: str) -> tuple[dict[str, Any], int, tuple[int, ...]]:
    source = Path(path).expanduser()
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0)
    try:
        descriptor = os.open(source, flags)
    except (OSError, ValueError) as error:
        raise BulkloadError(f"cannot open pinned Codex evidence {source}") from error
    try:
        before = os.fstat(descriptor)
        if (
            not stat.S_ISREG(before.st_mode)
            or before.st_uid != os.getuid()
            or stat.S_IMODE(before.st_mode) != 0o600
            or before.st_nlink != 1
            or before.st_size < 1
            or before.st_size > MAX_CODEX_SESSION_SNAPSHOT_BYTES + 1
        ):
            raise BulkloadError(
                f"Codex evidence must be a bounded owner-private regular file: {source}"
            )
        with os.fdopen(descriptor, "rb", closefd=False) as stream:
            payload = stream.read(MAX_CODEX_SESSION_SNAPSHOT_BYTES + 2)
        if len(payload) != before.st_size:
            raise BulkloadError(f"Codex evidence changed while reading: {source}")
        after = os.fstat(descriptor)
        entry = os.stat(source, follow_symlinks=False)
        expected = _stable_artifact_stat(before)
        if (
            _stable_artifact_stat(after) != expected
            or _stable_artifact_stat(entry) != expected
        ):
            raise BulkloadError(f"Codex evidence changed while reading: {source}")

        def unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
            value: dict[str, Any] = {}
            for key, item in pairs:
                if key in value:
                    raise ValueError(f"duplicate JSON key {key!r} is forbidden")
                value[key] = item
            return value

        def reject_nonfinite(value: str) -> None:
            raise ValueError(f"non-finite JSON number {value} is forbidden")

        def parse_finite_float(value: str) -> float:
            parsed = float(value)
            if not math.isfinite(parsed):
                raise ValueError(f"non-finite JSON number {value} is forbidden")
            return parsed

        try:
            document = json.loads(
                payload,
                object_pairs_hook=unique_object,
                parse_constant=reject_nonfinite,
                parse_float=parse_finite_float,
            )
        except (
            UnicodeDecodeError,
            json.JSONDecodeError,
            OverflowError,
            RecursionError,
            ValueError,
        ) as error:
            raise BulkloadError(
                f"cannot read Codex JSON object {source}: {error}"
            ) from error
        if not isinstance(document, dict):
            raise BulkloadError(f"{source} must contain a JSON object")
        return document, descriptor, expected
    except BaseException:
        os.close(descriptor)
        raise


def _revalidate_pinned_codex_input(
    path: str,
    descriptor: int,
    expected: tuple[int, ...],
) -> None:
    source = Path(path).expanduser()
    try:
        if (
            _stable_artifact_stat(os.fstat(descriptor)) != expected
            or _stable_artifact_stat(os.stat(source, follow_symlinks=False)) != expected
        ):
            raise BulkloadError(f"Codex evidence changed after reading: {source}")
    except (OSError, ValueError) as error:
        if isinstance(error, BulkloadError):
            raise
        raise BulkloadError(f"cannot revalidate Codex evidence {source}") from error


def _directory_identity_lineage(descriptor: int) -> set[tuple[int, int]]:
    current = os.dup(descriptor)
    lineage: set[tuple[int, int]] = set()
    try:
        for _ in range(MAX_CODEX_ROOT_LINEAGE):
            current_info = os.fstat(current)
            current_identity = (current_info.st_dev, current_info.st_ino)
            lineage.add(current_identity)
            flags = (
                os.O_RDONLY
                | getattr(os, "O_DIRECTORY", 0)
                | getattr(os, "O_NOFOLLOW", 0)
                | getattr(os, "O_CLOEXEC", 0)
            )
            parent = os.open("..", flags, dir_fd=current)
            parent_info = os.fstat(parent)
            if (parent_info.st_dev, parent_info.st_ino) == current_identity:
                os.close(parent)
                return lineage
            os.close(current)
            current = parent
    finally:
        os.close(current)
    raise BulkloadError("Codex evidence output parent lineage is unbounded")


def _validate_codex_output_descriptor(
    descriptor: int,
    payload: bytes,
    *,
    expected_links: int,
) -> tuple[int, int]:
    info = os.fstat(descriptor)
    if (
        not stat.S_ISREG(info.st_mode)
        or info.st_uid != os.getuid()
        or stat.S_IMODE(info.st_mode) != 0o600
        or info.st_size != len(payload)
        or info.st_nlink != expected_links
    ):
        raise BulkloadError("Codex evidence temporary output custody changed")
    offset = 0
    while offset < len(payload):
        expected = payload[offset : offset + 1024 * 1024]
        observed = os.pread(descriptor, len(expected), offset)
        if observed != expected:
            raise BulkloadError("Codex evidence temporary output bytes changed")
        offset += len(expected)
    return (info.st_dev, info.st_ino)


def _rename_codex_output_noreplace(
    source_directory: int,
    source_name: str,
    destination_directory: int,
    destination_name: str,
) -> None:
    libc = ctypes.CDLL(None, use_errno=True)
    source = os.fsencode(source_name)
    destination = os.fsencode(destination_name)
    if sys.platform == "darwin":
        try:
            rename = libc.renameatx_np
        except AttributeError as error:
            raise BulkloadError(
                "atomic no-replace evidence publication is unavailable"
            ) from error
        rename.argtypes = (
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_uint,
        )
        rename.restype = ctypes.c_int
        result = rename(
            source_directory,
            source,
            destination_directory,
            destination,
            0x00000004,
        )
    elif sys.platform.startswith("linux"):
        try:
            rename = libc.renameat2
        except AttributeError as error:
            raise BulkloadError(
                "atomic no-replace evidence publication is unavailable"
            ) from error
        rename.argtypes = (
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_uint,
        )
        rename.restype = ctypes.c_int
        result = rename(
            source_directory,
            source,
            destination_directory,
            destination,
            1,
        )
    else:
        raise BulkloadError(
            f"atomic no-replace evidence publication is unsupported on {sys.platform}"
        )
    if result == 0:
        return
    error_number = ctypes.get_errno()
    if error_number == errno.EEXIST:
        raise BulkloadError("Codex evidence output already exists")
    raise OSError(
        error_number,
        os.strerror(error_number),
        destination_name,
    )


def _write_pinned_codex_json(
    output: str,
    value: dict[str, Any],
    snapshots: Sequence[dict[str, Any]],
    pinned_inputs: Sequence[tuple[str, int, tuple[int, ...]]],
    *,
    protected_roots: Sequence[Path] = (),
) -> None:
    if output == "-":
        raise BulkloadError(
            "Codex evidence output must be an owner-private file; "
            "stdout publication is forbidden"
        )
    requested = Path(output).expanduser()
    roots = [
        *protected_roots,
        *[
            Path(snapshot["resolved_root"])
            for snapshot in snapshots
            if isinstance(snapshot.get("resolved_root"), str)
        ],
    ]
    try:
        _reject_output_overlap(str(requested), roots)
    except (OSError, RuntimeError, ValueError) as error:
        raise BulkloadError("cannot validate Codex evidence output path") from error
    try:
        parent = requested.parent.resolve()
    except (OSError, RuntimeError, ValueError) as error:
        raise BulkloadError("cannot resolve Codex evidence output parent") from error
    durable_makedirs(parent)
    target = parent / requested.name
    try:
        _reject_output_overlap(str(target), roots)
    except (OSError, RuntimeError, ValueError) as error:
        raise BulkloadError("cannot validate Codex evidence output path") from error
    flags = (
        os.O_RDONLY
        | getattr(os, "O_DIRECTORY", 0)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_CLOEXEC", 0)
    )
    try:
        parent_descriptor = os.open(parent, flags)
    except (OSError, ValueError) as error:
        raise BulkloadError(
            f"cannot pin Codex evidence output parent: {parent}"
        ) from error
    temporary_name: str | None = None
    temporary_descriptor: int | None = None
    try:
        parent_info = os.fstat(parent_descriptor)
        parent_expected = (
            parent_info.st_dev,
            parent_info.st_ino,
            parent_info.st_uid,
            stat.S_IMODE(parent_info.st_mode),
        )
        if (
            parent_info.st_uid != os.getuid()
            or stat.S_IMODE(parent_info.st_mode) & 0o077
        ):
            raise BulkloadError(
                f"Codex evidence output parent is not owner-private: {parent}"
            )
        protected_identities = {
            (snapshot["root_identity"]["device"], snapshot["root_identity"]["inode"])
            for snapshot in snapshots
            if isinstance(snapshot.get("root_identity"), dict)
        }
        if _directory_identity_lineage(parent_descriptor) & protected_identities:
            raise BulkloadError(
                "Codex evidence output parent overlaps a captured session root"
            )
        try:
            target_info = os.stat(
                requested.name,
                dir_fd=parent_descriptor,
                follow_symlinks=False,
            )
        except FileNotFoundError:
            target_info = None
        if target_info is not None:
            raise BulkloadError("Codex evidence output already exists")
        payload = canonical_bytes(value) + b"\n"
        for _ in range(128):
            candidate = f".{requested.name}.bulkload-{secrets.token_hex(8)}"
            try:
                temporary_descriptor = os.open(
                    candidate,
                    os.O_RDWR
                    | os.O_CREAT
                    | os.O_EXCL
                    | getattr(os, "O_NOFOLLOW", 0)
                    | getattr(os, "O_CLOEXEC", 0),
                    0o600,
                    dir_fd=parent_descriptor,
                )
                temporary_name = candidate
                break
            except FileExistsError:
                continue
        else:
            raise BulkloadError("cannot allocate Codex evidence temporary file")
        os.fchmod(temporary_descriptor, 0o600)
        offset = 0
        while offset < len(payload):
            written = os.write(temporary_descriptor, payload[offset:])
            if written < 1:
                raise BulkloadError("cannot write Codex evidence output")
            offset += written
        os.fsync(temporary_descriptor)
        temporary_identity = _validate_codex_output_descriptor(
            temporary_descriptor,
            payload,
            expected_links=1,
        )
        for path, descriptor, expected in pinned_inputs:
            _revalidate_pinned_codex_input(path, descriptor, expected)
        current_parent = os.stat(parent, follow_symlinks=False)
        if (
            current_parent.st_dev,
            current_parent.st_ino,
            current_parent.st_uid,
            stat.S_IMODE(current_parent.st_mode),
        ) != parent_expected:
            raise BulkloadError("Codex evidence output parent changed before publish")
        if (
            _validate_codex_output_descriptor(
                temporary_descriptor,
                payload,
                expected_links=1,
            )
            != temporary_identity
        ):
            raise BulkloadError("Codex evidence temporary output identity changed")
        _rename_codex_output_noreplace(
            parent_descriptor,
            temporary_name,
            parent_descriptor,
            requested.name,
        )
        temporary_name = None
        for path, descriptor, expected in pinned_inputs:
            _revalidate_pinned_codex_input(path, descriptor, expected)
        if (
            _validate_codex_output_descriptor(
                temporary_descriptor,
                payload,
                expected_links=1,
            )
            != temporary_identity
        ):
            raise BulkloadError("Codex evidence output changed during publish")
        published_info = os.stat(
            requested.name,
            dir_fd=parent_descriptor,
            follow_symlinks=False,
        )
        if (
            not stat.S_ISREG(published_info.st_mode)
            or published_info.st_uid != os.getuid()
            or stat.S_IMODE(published_info.st_mode) != 0o600
            or published_info.st_size != len(payload)
            or published_info.st_nlink != 1
            or (published_info.st_dev, published_info.st_ino) != temporary_identity
        ):
            raise BulkloadError("Codex evidence target changed during publish")
        current_parent = os.stat(parent, follow_symlinks=False)
        if (
            current_parent.st_dev,
            current_parent.st_ino,
            current_parent.st_uid,
            stat.S_IMODE(current_parent.st_mode),
        ) != parent_expected:
            raise BulkloadError("Codex evidence output parent changed during publish")
        os.fsync(parent_descriptor)
        for path, descriptor, expected in pinned_inputs:
            _revalidate_pinned_codex_input(path, descriptor, expected)
        if (
            _validate_codex_output_descriptor(
                temporary_descriptor,
                payload,
                expected_links=1,
            )
            != temporary_identity
        ):
            raise BulkloadError("Codex evidence output changed after publish")
        published_info = os.stat(
            requested.name,
            dir_fd=parent_descriptor,
            follow_symlinks=False,
        )
        if (
            not stat.S_ISREG(published_info.st_mode)
            or published_info.st_uid != os.getuid()
            or stat.S_IMODE(published_info.st_mode) != 0o600
            or published_info.st_size != len(payload)
            or published_info.st_nlink != 1
            or (published_info.st_dev, published_info.st_ino) != temporary_identity
        ):
            raise BulkloadError("Codex evidence target changed after publish")
        current_parent = os.stat(parent, follow_symlinks=False)
        if (
            current_parent.st_dev,
            current_parent.st_ino,
            current_parent.st_uid,
            stat.S_IMODE(current_parent.st_mode),
        ) != parent_expected:
            raise BulkloadError("Codex evidence output parent changed after publish")
    finally:
        if temporary_descriptor is not None:
            try:
                os.close(temporary_descriptor)
            except OSError:
                pass
        os.close(parent_descriptor)


def _capture(arguments: argparse.Namespace) -> int:
    root = Path(arguments.root).expanduser().resolve()
    _reject_output_overlap(arguments.output, [root])
    snapshot = capture_snapshot(
        root,
        arguments.mode,
        include_ignored=arguments.include_ignored,
        max_files=arguments.max_files,
        max_bytes=arguments.max_bytes,
    )
    _write_json(arguments.output, snapshot)
    if arguments.output != "-":
        print(
            f"catalog={snapshot['catalog_sha256']} repos={len(snapshot['catalog'])} "
            f"complete={str(snapshot['complete']).lower()}"
        )
    return 0 if snapshot["complete"] else 3


def _plan(arguments: argparse.Namespace) -> int:
    if arguments.output != "-":
        _reject_output_input_alias(
            arguments.output,
            [arguments.source_a, arguments.source_b, arguments.destination],
        )
    source_a = read_json(Path(arguments.source_a))
    source_b = read_json(Path(arguments.source_b))
    destination = read_json(Path(arguments.destination))
    local_roots = [
        Path(snapshot["root"])
        for snapshot in (source_a, source_b, destination)
        if snapshot.get("host") == socket.gethostname()
        and isinstance(snapshot.get("root"), str)
    ]
    _reject_output_overlap(arguments.output, local_roots)
    plan = compile_plan(source_a, source_b, destination)
    _write_json(arguments.output, plan)
    if arguments.output != "-":
        intent = plan["intent"]
        print(
            f"plan={plan['plan_sha256']} ready={str(intent['ready']).lower()} "
            f"operations={len(intent['operations'])} blockers={len(intent['blockers'])}"
        )
    return 0 if plan["intent"]["ready"] else 4


def _apply(arguments: argparse.Namespace) -> int:
    _reject_output_input_alias(arguments.receipt, [arguments.plan])
    receipt = apply_plan(
        read_json(Path(arguments.plan)),
        source_root=Path(arguments.source_root),
        destination_root=Path(arguments.destination_root),
        accepted_digest=arguments.accept_plan,
        state_root=Path(arguments.state_root),
        receipt_path=Path(arguments.receipt),
    )
    print(
        f"receipt={receipt['receipt_sha256']} operations={len(receipt['operations'])}"
    )
    return 0


def _verify(arguments: argparse.Namespace) -> int:
    if arguments.output != "-":
        _reject_output_input_alias(
            arguments.output, [arguments.plan, arguments.destination]
        )
    destination = read_json(Path(arguments.destination))
    local_roots = (
        [Path(destination["root"])]
        if destination.get("host") == socket.gethostname()
        and isinstance(destination.get("root"), str)
        else []
    )
    _reject_output_overlap(arguments.output, local_roots)
    result = verify_plan(
        read_json(Path(arguments.plan)),
        destination,
        arguments.accept_plan,
    )
    _write_json(arguments.output, result)
    if arguments.output != "-":
        print(
            f"verified={str(result['verified']).lower()} "
            f"failures={len(result['failures'])}"
        )
    return 0 if result["verified"] else 5


def _files(arguments: argparse.Namespace) -> int:
    if not arguments.null:
        raise BulkloadError("files requires --null; newline allowlists are unsafe")
    values = export_copy_paths(read_json(Path(arguments.plan)), arguments.accept_plan)
    separator = b"\0"
    payload = separator.join(value.encode("utf-8") for value in values)
    if values:
        payload += separator
    sys.stdout.buffer.write(payload)
    return 0


def _doctor(_: argparse.Namespace) -> int:
    tools = {}
    version_arguments = {"git": ["--version"], "rsync": ["--version"], "ssh": ["-V"]}
    for name in ("git", "rsync", "ssh"):
        path = shutil.which(name)
        tools[name] = {"available": path is not None, "path": path}
        if path is not None:
            process = subprocess.run(
                [path, *version_arguments[name]],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                check=False,
            )
            tools[name]["version"] = (
                process.stdout.splitlines()[0] if process.stdout else None
            )
    report = {
        "bulkload_version": __version__,
        "python": sys.version.split()[0],
        "python_supported": sys.version_info >= (3, 11),
        "tools": tools,
    }
    sys.stdout.write(json.dumps(report, indent=2, sort_keys=True) + "\n")
    return 0 if tools["git"]["available"] and report["python_supported"] else 6


def _codex_capture(arguments: argparse.Namespace) -> int:
    root = Path(os.path.abspath(Path(arguments.root).expanduser()))
    snapshot = capture_codex_sessions(
        root,
        role=arguments.role,
        acknowledge_writers_quiesced=arguments.acknowledge_writers_quiesced,
        host_authority_id=arguments.host_authority_id,
        max_files=arguments.max_files,
        max_entries=arguments.max_entries,
        max_directories=arguments.max_directories,
        max_bytes=arguments.max_bytes,
        max_file_bytes=arguments.max_file_bytes,
        max_record_bytes=arguments.max_record_bytes,
        max_records_per_file=arguments.max_records_per_file,
        max_path_bytes=arguments.max_path_bytes,
        max_path_components=arguments.max_path_components,
        max_catalog_bytes=arguments.max_catalog_bytes,
        max_errors=arguments.max_errors,
        max_output_bytes=arguments.max_output_bytes,
    )
    validate_codex_session_snapshot(snapshot)
    _write_pinned_codex_json(
        arguments.output,
        snapshot,
        [snapshot],
        [],
        protected_roots=[root],
    )
    if arguments.output != "-":
        print(
            f"catalog={snapshot['catalog_sha256']} "
            f"sessions={len(snapshot['sessions'])} "
            f"complete={str(snapshot['complete']).lower()}"
        )
    return 0 if snapshot["complete"] else 3


def _codex_prefix_request(arguments: argparse.Namespace) -> int:
    inputs = [
        arguments.source_a,
        arguments.source_b,
        arguments.destination_a,
        arguments.destination_b,
    ]
    pinned: list[tuple[str, int, tuple[int, ...]]] = []
    try:
        snapshots: list[dict[str, Any]] = []
        for path in inputs:
            snapshot, descriptor, expected = _read_pinned_codex_json(path)
            pinned.append((path, descriptor, expected))
            validate_codex_session_snapshot(snapshot)
            snapshots.append(snapshot)
        request = compile_codex_session_prefix_request(*snapshots)
        for path, descriptor, expected in pinned:
            _revalidate_pinned_codex_input(path, descriptor, expected)
        _write_pinned_codex_json(
            arguments.output,
            request,
            snapshots,
            pinned,
        )
    finally:
        for _, descriptor, _ in pinned:
            try:
                os.close(descriptor)
            except OSError:
                pass
    if arguments.output != "-":
        print(
            f"request={request['request_sha256']} prefixes={len(request['requests'])}"
        )
    return 0


def _codex_prefix_proof(arguments: argparse.Namespace) -> int:
    inputs = [
        arguments.prefix_request,
        arguments.source_a,
        arguments.source_b,
        arguments.destination_a,
        arguments.destination_b,
    ]
    pinned: list[tuple[str, int, tuple[int, ...]]] = []
    try:
        documents: list[dict[str, Any]] = []
        for index, path in enumerate(inputs):
            document, descriptor, expected = _read_pinned_codex_json(path)
            pinned.append((path, descriptor, expected))
            if index == 0:
                validate_codex_session_prefix_request(document)
            else:
                validate_codex_session_snapshot(document)
            documents.append(document)
        request, source_a, source_b, destination_a, destination_b = documents
        root = Path(os.path.abspath(Path(arguments.root).expanduser()))
        proof = capture_codex_session_prefix_proof(
            root,
            role=arguments.role,
            prefix_request=request,
            source_a=source_a,
            source_b=source_b,
            destination_a=destination_a,
            destination_b=destination_b,
            acknowledge_writers_quiesced=arguments.acknowledge_writers_quiesced,
        )
        validate_codex_session_prefix_proof(proof)
        for path, descriptor, expected in pinned:
            _revalidate_pinned_codex_input(path, descriptor, expected)
        _write_pinned_codex_json(
            arguments.output,
            proof,
            [source_a, source_b, destination_a, destination_b],
            pinned,
            protected_roots=[root],
        )
    finally:
        for _, descriptor, _ in pinned:
            try:
                os.close(descriptor)
            except OSError:
                pass
    if arguments.output != "-":
        print(
            f"proof={proof['proof_sha256']} "
            f"role={proof['role']} prefixes={len(proof['proofs'])}"
        )
    return 0


def _codex_close_request(arguments: argparse.Namespace) -> int:
    required_inputs = [
        ("prefix_request", arguments.prefix_request),
        ("source_a", arguments.source_a),
        ("source_b", arguments.source_b),
        ("destination_a", arguments.destination_a),
        ("destination_b", arguments.destination_b),
    ]
    optional_inputs = [
        ("source_prefix_a", arguments.source_prefix_a),
        ("source_prefix_b", arguments.source_prefix_b),
        ("destination_prefix_a", arguments.destination_prefix_a),
        ("destination_prefix_b", arguments.destination_prefix_b),
    ]
    pinned: list[tuple[str, int, tuple[int, ...]]] = []
    try:
        documents: dict[str, dict[str, Any] | None] = {}
        for name, path in [*required_inputs, *optional_inputs]:
            if path is None:
                documents[name] = None
                continue
            document, descriptor, expected = _read_pinned_codex_json(path)
            pinned.append((path, descriptor, expected))
            if name == "prefix_request":
                validate_codex_session_prefix_request(document)
            elif "prefix" in name:
                validate_codex_session_prefix_proof(document)
            else:
                validate_codex_session_snapshot(document)
            documents[name] = document
        close_request = compile_codex_session_close_request(
            documents["prefix_request"],
            documents["source_a"],
            documents["source_b"],
            documents["destination_a"],
            documents["destination_b"],
            source_prefix_a=documents["source_prefix_a"],
            source_prefix_b=documents["source_prefix_b"],
            destination_prefix_a=documents["destination_prefix_a"],
            destination_prefix_b=documents["destination_prefix_b"],
        )
        for path, descriptor, expected in pinned:
            _revalidate_pinned_codex_input(path, descriptor, expected)
        snapshots = [
            documents["source_a"],
            documents["source_b"],
            documents["destination_a"],
            documents["destination_b"],
        ]
        _write_pinned_codex_json(
            arguments.output,
            close_request,
            snapshots,
            pinned,
        )
    finally:
        for _, descriptor, _ in pinned:
            try:
                os.close(descriptor)
            except OSError:
                pass
    if arguments.output != "-":
        print(f"close_request={close_request['close_request_sha256']}")
    return 0


def _codex_close_capture(arguments: argparse.Namespace) -> int:
    pinned: list[tuple[str, int, tuple[int, ...]]] = []
    try:
        close_request, descriptor, expected = _read_pinned_codex_json(
            arguments.close_request
        )
        pinned.append((arguments.close_request, descriptor, expected))
        validate_codex_session_close_request(close_request)
        root = Path(os.path.abspath(Path(arguments.root).expanduser()))
        close_capture = capture_codex_session_close_capture(
            root,
            role=arguments.role,
            close_request=close_request,
            acknowledge_writers_quiesced=arguments.acknowledge_writers_quiesced,
        )
        validate_codex_session_close_capture(close_capture)
        _revalidate_pinned_codex_input(
            arguments.close_request,
            descriptor,
            expected,
        )
        _write_pinned_codex_json(
            arguments.output,
            close_capture,
            [close_capture["snapshot"]],
            pinned,
            protected_roots=[root],
        )
    finally:
        for _, descriptor, _ in pinned:
            try:
                os.close(descriptor)
            except OSError:
                pass
    if arguments.output != "-":
        print(
            f"close_capture={close_capture['close_capture_sha256']} "
            f"capture={close_capture['snapshot']['capture_id']}"
        )
    return 0


def _codex_plan(arguments: argparse.Namespace) -> int:
    inputs = [
        arguments.source_a,
        arguments.source_b,
        arguments.destination_a,
        arguments.destination_b,
    ]
    pinned: list[tuple[str, int, tuple[int, ...]]] = []
    try:
        snapshots: list[dict[str, Any]] = []
        for path in inputs:
            snapshot, descriptor, expected = _read_pinned_codex_json(path)
            pinned.append((path, descriptor, expected))
            validate_codex_session_snapshot(snapshot)
            snapshots.append(snapshot)
        source_a, source_b, destination_a, destination_b = snapshots
        optional_documents: dict[str, dict[str, Any] | None] = {
            "prefix_request": None,
            "source_prefix_a": None,
            "source_prefix_b": None,
            "destination_prefix_a": None,
            "destination_prefix_b": None,
            "close_request": None,
            "source_close_a": None,
            "source_close_b": None,
            "destination_close_a": None,
            "destination_close_b": None,
        }
        for name in optional_documents:
            path = getattr(arguments, name)
            if path is None:
                continue
            document, descriptor, expected = _read_pinned_codex_json(path)
            pinned.append((path, descriptor, expected))
            if name == "prefix_request":
                validate_codex_session_prefix_request(document)
            elif name == "close_request":
                validate_codex_session_close_request(document)
            elif "_close_" in name:
                validate_codex_session_close_capture(document)
            else:
                validate_codex_session_prefix_proof(document)
            optional_documents[name] = document
        plan = compile_codex_session_union_plan(
            source_a,
            source_b,
            destination_a,
            destination_b,
            prefix_request=optional_documents["prefix_request"],
            source_prefix_a=optional_documents["source_prefix_a"],
            source_prefix_b=optional_documents["source_prefix_b"],
            destination_prefix_a=optional_documents["destination_prefix_a"],
            destination_prefix_b=optional_documents["destination_prefix_b"],
            close_request=optional_documents["close_request"],
            source_close_a=optional_documents["source_close_a"],
            source_close_b=optional_documents["source_close_b"],
            destination_close_a=optional_documents["destination_close_a"],
            destination_close_b=optional_documents["destination_close_b"],
        )
        for path, descriptor, expected in pinned:
            _revalidate_pinned_codex_input(path, descriptor, expected)
        _write_pinned_codex_json(
            arguments.output,
            plan,
            [
                *snapshots,
                *[
                    document["snapshot"]
                    for name, document in optional_documents.items()
                    if "_close_" in name and document is not None
                ],
            ],
            pinned,
        )
    finally:
        for _, descriptor, _ in pinned:
            try:
                os.close(descriptor)
            except OSError:
                pass
    intent = plan["intent"]
    if arguments.output != "-":
        print(
            f"plan={plan['plan_sha256']} "
            f"ready={str(intent['ready_for_attended_copy']).lower()} "
            f"copy_if_absent={len(intent['copy_if_absent'])} "
            f"promote_superset={len(intent['promote_source_superset'])} "
            f"blockers={len(intent['blockers'])}"
        )
    return 0 if intent["ready_for_attended_copy"] else 4


def _codex_private_capture(arguments: argparse.Namespace) -> int:
    codex_home = Path(arguments.codex_home)
    sqlite_home = (
        Path(arguments.sqlite_home) if arguments.sqlite_home is not None else None
    )
    selected_state_classes = [
        state_class
        for state_class, selected in (
            ("auth", arguments.include_auth),
            ("sqlite", arguments.include_sqlite),
        )
        if selected
    ]
    with acquire_codex_private_bulkload_lock(codex_home, sqlite_home) as lock:
        with open_codex_private_quiescence_attestation(
            Path(arguments.quiescence_attestation),
            accept_attestation=arguments.accept_quiescence_attestation,
            expected_purpose="capture",
            expected_host_authority_id=arguments.host_authority_id,
            expected_codex_version=arguments.codex_version,
            expected_selected_state_classes=selected_state_classes,
            expected_codex_home=codex_home,
            expected_sqlite_home=sqlite_home,
            expected_operation_output=Path(arguments.output_directory),
            expected_capture_role=arguments.role,
        ) as quiescence:
            quiescence.assert_bulkload_lock(lock)
            capture = capture_codex_private_state(
                codex_home,
                Path(arguments.output_directory),
                role=arguments.role,
                host_authority_id=arguments.host_authority_id,
                codex_version=arguments.codex_version,
                sqlite_home=sqlite_home,
                include_auth=arguments.include_auth,
                include_sqlite=arguments.include_sqlite,
                acknowledge_private_capture=arguments.acknowledge_private_capture,
                quiescence=private_quiescence_capture_record(quiescence.value),
                max_sqlite_families=arguments.max_sqlite_families,
                max_total_sqlite_bytes=arguments.max_total_sqlite_bytes,
                backup_timeout_seconds=arguments.backup_timeout_seconds,
                max_thread_entries=arguments.max_thread_entries,
                max_thread_index_bytes=arguments.max_thread_index_bytes,
                max_metadata_entries=arguments.max_metadata_entries,
                max_metadata_bytes=arguments.max_metadata_bytes,
            )
            lock.revalidate()
            quiescence.revalidate()
    print(
        f"capture={capture['capture_sha256']} "
        f"auth={str(capture['auth'] is not None).lower()} "
        f"sqlite_families={len(capture['sqlite_families'])} "
        "ready_for_apply=false"
    )
    return 0


def _codex_private_quiescence_attest(arguments: argparse.Namespace) -> int:
    selected_state_classes = [
        state_class
        for state_class, selected in (
            ("auth", arguments.include_auth),
            ("sqlite", arguments.include_sqlite),
        )
        if selected
    ]
    attestation = create_codex_private_quiescence_attestation(
        Path(arguments.codex_home),
        Path(arguments.output),
        purpose=arguments.purpose,
        host_authority_id=arguments.host_authority_id,
        codex_version=arguments.codex_version,
        selected_state_classes=selected_state_classes,
        sqlite_home=(
            Path(arguments.sqlite_home) if arguments.sqlite_home is not None else None
        ),
        create_only_output=Path(arguments.operation_output),
        acknowledge_writers_quiesced=arguments.acknowledge_writers_quiesced,
        capture_role=arguments.capture_role,
        accepted_plan_sha256=arguments.accept_plan,
        accepted_apply_receipt_sha256=arguments.accept_apply_receipt,
        accepted_journal_sha256=arguments.accept_journal,
        ttl_seconds=arguments.ttl_seconds,
    )
    print(
        f"attestation={attestation['attestation_sha256']} "
        f"id={attestation['attestation_id']} "
        f"expires_at={attestation['expires_at']} "
        "provider_writer_proof=false"
    )
    return 0


def _codex_private_plan(arguments: argparse.Namespace) -> int:
    source = Path(arguments.source_bundle)
    destination = Path(arguments.destination_bundle)
    with private_runtime.open_pinned_private_runtime_authority() as runtime:
        plan = compile_codex_private_state_plan(
            source,
            destination,
            runtime_authority=runtime.record,
        )
        live_roots = (
            *plan["protected_live_roots"]["source"],
            *plan["protected_live_roots"]["destination"],
        )
        revalidate_codex_private_bundles(
            source,
            destination,
            expected_source_capture_sha256=plan["source_capture_sha256"],
            expected_destination_capture_sha256=plan["destination_capture_sha256"],
        )
        runtime.revalidate()
        write_private_json_noreplace(
            Path(arguments.output),
            plan,
            protected_directories=(source, destination),
            recorded_protected_directories=tuple(Path(root) for root in live_roots),
        )
        try:
            revalidate_codex_private_bundles(
                source,
                destination,
                expected_source_capture_sha256=plan["source_capture_sha256"],
                expected_destination_capture_sha256=plan["destination_capture_sha256"],
            )
            runtime.revalidate()
        except BulkloadError as error:
            raise BulkloadError(
                "private plan inputs or runtime changed after publication; "
                f"fail-held evidence: {arguments.output}"
            ) from error
    print(
        f"plan={plan['plan_sha256']} "
        f"blockers={len(plan['blockers'])} ready_for_apply=false"
    )
    return 4


def _codex_private_sqlite_close_request(arguments: argparse.Namespace) -> int:
    if arguments.output == "-":
        raise BulkloadError(
            "private SQLite close request requires an owner-private output file"
        )
    required_inputs = (
        ("opening_plan", arguments.opening_plan),
        ("compatibility_plan", arguments.compatibility_plan),
        ("adapter_registry", arguments.adapter_registry),
        ("path_map", arguments.path_map),
        ("session_union_plan", arguments.session_union_plan),
        ("session_source_a", arguments.session_source_a),
        ("session_source_b", arguments.session_source_b),
        ("session_destination_a", arguments.session_destination_a),
        ("session_destination_b", arguments.session_destination_b),
    )
    optional_inputs = (
        ("session_prefix_request", arguments.session_prefix_request),
        ("session_source_prefix_a", arguments.session_source_prefix_a),
        ("session_source_prefix_b", arguments.session_source_prefix_b),
        (
            "session_destination_prefix_a",
            arguments.session_destination_prefix_a,
        ),
        (
            "session_destination_prefix_b",
            arguments.session_destination_prefix_b,
        ),
        ("session_close_request", arguments.session_close_request),
        ("session_source_close_a", arguments.session_source_close_a),
        ("session_source_close_b", arguments.session_source_close_b),
        (
            "session_destination_close_a",
            arguments.session_destination_close_a,
        ),
        (
            "session_destination_close_b",
            arguments.session_destination_close_b,
        ),
    )
    opening_bundles = {
        "source_a": Path(arguments.source_a_bundle),
        "source_b": Path(arguments.source_b_bundle),
        "destination_a": Path(arguments.destination_a_bundle),
        "destination_b": Path(arguments.destination_b_bundle),
    }
    pinned: list[tuple[str, int, tuple[int, ...]]] = []
    documents: dict[str, dict[str, Any] | None] = {}
    try:
        for name, path in (*required_inputs, *optional_inputs):
            if path is None:
                documents[name] = None
                continue
            document, descriptor, expected = _read_pinned_codex_json(path)
            documents[name] = document
            pinned.append((path, descriptor, expected))
        with private_runtime.open_pinned_private_runtime_authority() as runtime:
            close_request = compile_codex_private_sqlite_close_request(
                documents["opening_plan"],
                documents["session_union_plan"],
                documents["session_source_a"],
                documents["session_source_b"],
                documents["session_destination_a"],
                documents["session_destination_b"],
                runtime.record,
                accept_opening_plan=arguments.accept_opening_plan,
                accept_session_union_plan=arguments.accept_session_union_plan,
                writer_stop_epoch_id=arguments.writer_stop_epoch_id,
                writer_stop_epoch_at=arguments.writer_stop_epoch_at,
                acknowledge_provider_writers_stopped=(
                    arguments.acknowledge_provider_writers_stopped
                ),
                opening_compatibility_plan=documents["compatibility_plan"],
                opening_source_a_directory=opening_bundles["source_a"],
                opening_source_b_directory=opening_bundles["source_b"],
                opening_destination_a_directory=opening_bundles["destination_a"],
                opening_destination_b_directory=opening_bundles["destination_b"],
                opening_adapter_registry=documents["adapter_registry"],
                opening_path_map=documents["path_map"],
                opening_session_prefix_request=documents["session_prefix_request"],
                opening_session_source_prefix_a=documents["session_source_prefix_a"],
                opening_session_source_prefix_b=documents["session_source_prefix_b"],
                opening_session_destination_prefix_a=documents[
                    "session_destination_prefix_a"
                ],
                opening_session_destination_prefix_b=documents[
                    "session_destination_prefix_b"
                ],
                opening_session_close_request=documents["session_close_request"],
                opening_session_source_close_a=documents["session_source_close_a"],
                opening_session_source_close_b=documents["session_source_close_b"],
                opening_session_destination_close_a=documents[
                    "session_destination_close_a"
                ],
                opening_session_destination_close_b=documents[
                    "session_destination_close_b"
                ],
            )
            validate_codex_private_sqlite_close_request(close_request)

            def revalidate_inputs() -> None:
                for path, descriptor, expected in pinned:
                    _revalidate_pinned_codex_input(path, descriptor, expected)
                runtime.revalidate()
                validate_codex_private_sqlite_compose_plan_against_inputs(
                    documents["opening_plan"],
                    documents["compatibility_plan"],
                    opening_bundles["source_a"],
                    opening_bundles["source_b"],
                    opening_bundles["destination_a"],
                    opening_bundles["destination_b"],
                    adapter_registry=documents["adapter_registry"],
                    path_map=documents["path_map"],
                    session_union_plan=documents["session_union_plan"],
                    session_source_a=documents["session_source_a"],
                    session_source_b=documents["session_source_b"],
                    session_destination_a=documents["session_destination_a"],
                    session_destination_b=documents["session_destination_b"],
                    session_prefix_request=documents["session_prefix_request"],
                    session_source_prefix_a=documents["session_source_prefix_a"],
                    session_source_prefix_b=documents["session_source_prefix_b"],
                    session_destination_prefix_a=documents[
                        "session_destination_prefix_a"
                    ],
                    session_destination_prefix_b=documents[
                        "session_destination_prefix_b"
                    ],
                    session_close_request=documents["session_close_request"],
                    session_source_close_a=documents["session_source_close_a"],
                    session_source_close_b=documents["session_source_close_b"],
                    session_destination_close_a=documents[
                        "session_destination_close_a"
                    ],
                    session_destination_close_b=documents[
                        "session_destination_close_b"
                    ],
                )

            revalidate_inputs()
            opening = documents["opening_plan"]
            protected_roots = [
                *opening_bundles.values(),
                *(
                    Path(
                        opening["private_opening"][role]["stable_projection"][
                            authority
                        ]["resolved_path"]
                    )
                    for role in ("source", "destination")
                    for authority in ("codex_home", "sqlite_home")
                ),
            ]
            _write_pinned_codex_json(
                arguments.output,
                close_request,
                [
                    documents["session_source_a"],
                    documents["session_source_b"],
                    documents["session_destination_a"],
                    documents["session_destination_b"],
                ],
                pinned,
                protected_roots=protected_roots,
            )
            try:
                revalidate_inputs()
            except BulkloadError as error:
                raise BulkloadError(
                    "private SQLite close inputs or runtime changed after "
                    f"publication; fail-held evidence: {arguments.output}"
                ) from error
            runtime.revalidate()
    finally:
        for _, descriptor, _ in pinned:
            try:
                os.close(descriptor)
            except OSError:
                pass
    if arguments.output != "-":
        print(
            f"close_request={close_request['close_request_sha256']} "
            f"epoch={close_request['writer_stop_epoch']['epoch_id']} "
            "provider_writer_proof=false"
        )
    return 0


def _codex_private_sqlite_session_reclose(arguments: argparse.Namespace) -> int:
    if arguments.output == "-":
        raise BulkloadError(
            "private SQLite session re-close requires an owner-private output file"
        )
    pinned: list[tuple[str, int, tuple[int, ...]]] = []
    runtime: Any | None = None
    try:
        close_request, descriptor, expected = _read_pinned_codex_json(
            arguments.close_request
        )
        pinned.append((arguments.close_request, descriptor, expected))
        validate_codex_private_sqlite_close_request(close_request)
        runtime = private_runtime.open_pinned_private_runtime_authority(
            close_request["runtime_authority"]
        )
        root = Path(os.path.abspath(Path(arguments.root).expanduser()))
        capture = capture_codex_private_sqlite_session_reclose(
            root,
            role=arguments.role,
            close_request=close_request,
            accept_close_request=arguments.accept_close_request,
            writer_stop_epoch_id=arguments.writer_stop_epoch_id,
            acknowledge_writers_quiesced=arguments.acknowledge_writers_quiesced,
        )
        validate_codex_private_sqlite_session_reclose_capture(capture)
        _revalidate_pinned_codex_input(
            arguments.close_request,
            descriptor,
            expected,
        )
        runtime.revalidate()
        _write_pinned_codex_json(
            arguments.output,
            capture,
            [capture["snapshot"]],
            pinned,
            protected_roots=[root],
        )
        validate_codex_private_sqlite_session_reclose_against_live(
            capture,
            root,
            close_request,
            role=arguments.role,
            accept_close_request=arguments.accept_close_request,
            writer_stop_epoch_id=arguments.writer_stop_epoch_id,
            acknowledge_writers_quiesced=arguments.acknowledge_writers_quiesced,
        )
        _revalidate_pinned_codex_input(
            arguments.close_request,
            descriptor,
            expected,
        )
        runtime.revalidate()
    finally:
        if runtime is not None:
            runtime.close()
        for _, descriptor, _ in pinned:
            try:
                os.close(descriptor)
            except OSError:
                pass
    if arguments.output != "-":
        print(
            f"session_reclose={capture['reclose_capture_sha256']} "
            f"capture={capture['snapshot']['capture_id']} "
            "provider_writer_proof=false"
        )
    return 0


def _codex_private_sqlite_private_reclose(arguments: argparse.Namespace) -> int:
    close_request, descriptor, expected = _read_pinned_codex_json(
        arguments.close_request
    )
    runtime: Any | None = None
    try:
        validate_codex_private_sqlite_close_request(close_request)
        runtime = private_runtime.open_pinned_private_runtime_authority(
            close_request["runtime_authority"]
        )
        if arguments.accept_close_request != close_request["close_request_sha256"]:
            raise BulkloadError("accepted private SQLite close-request digest differs")
        if (
            arguments.writer_stop_epoch_id
            != close_request["writer_stop_epoch"]["epoch_id"]
        ):
            raise BulkloadError("private SQLite writer-stop epoch acceptance differs")
        opening = close_request["private_opening"][arguments.role]["binding"][
            "stable_projection"
        ]
        codex_home = Path(arguments.codex_home)
        sqlite_home = Path(arguments.sqlite_home)
        if (
            codex_home.expanduser().resolve()
            != Path(opening["codex_home"]["resolved_path"])
            or sqlite_home.expanduser().resolve()
            != Path(opening["sqlite_home"]["resolved_path"])
            or arguments.host_authority_id != opening["host_authority_id"]
            or arguments.codex_version != opening["codex_version"]
        ):
            raise BulkloadError(
                "private SQLite private re-close live authority differs "
                "from the opening"
            )
        selected_state_classes = opening["selected_state_classes"]
        if "sqlite" not in selected_state_classes:
            raise BulkloadError(
                "private SQLite private re-close opening does not select SQLite"
            )
        with acquire_codex_private_bulkload_lock(codex_home, sqlite_home) as lock:
            with open_codex_private_quiescence_attestation(
                Path(arguments.quiescence_attestation),
                accept_attestation=arguments.accept_quiescence_attestation,
                expected_purpose="close",
                expected_host_authority_id=arguments.host_authority_id,
                expected_codex_version=arguments.codex_version,
                expected_selected_state_classes=selected_state_classes,
                expected_codex_home=codex_home,
                expected_sqlite_home=sqlite_home,
                expected_operation_output=Path(arguments.output_directory),
                expected_capture_role=arguments.role,
                expected_plan_sha256=close_request["close_request_sha256"],
            ) as quiescence:
                quiescence.assert_bulkload_lock(lock)
                recorded_roots = tuple(
                    Path(
                        close_request["private_opening"][role]["binding"][
                            "stable_projection"
                        ][authority]["resolved_path"]
                    )
                    for role in ("source", "destination")
                    for authority in ("codex_home", "sqlite_home")
                )
                capture = capture_codex_private_state(
                    codex_home,
                    Path(arguments.output_directory),
                    role=arguments.role,
                    host_authority_id=arguments.host_authority_id,
                    codex_version=arguments.codex_version,
                    sqlite_home=sqlite_home,
                    include_auth="auth" in selected_state_classes,
                    include_sqlite=True,
                    acknowledge_private_capture=(arguments.acknowledge_private_capture),
                    quiescence=private_quiescence_capture_record(quiescence.value),
                    max_sqlite_families=opening["budgets"]["max_sqlite_families"],
                    max_total_sqlite_bytes=opening["budgets"]["max_total_sqlite_bytes"],
                    backup_timeout_seconds=opening["budgets"]["backup_timeout_seconds"],
                    max_thread_entries=opening["budgets"]["max_thread_entries"],
                    max_thread_index_bytes=opening["budgets"]["max_thread_index_bytes"],
                    max_metadata_entries=opening["budgets"]["max_metadata_entries"],
                    max_metadata_bytes=opening["budgets"]["max_metadata_bytes"],
                    recorded_protected_directories=recorded_roots,
                )
                validate_codex_private_sqlite_private_reclose_capture(
                    capture,
                    close_request,
                    role=arguments.role,
                )
                _revalidate_pinned_codex_input(
                    arguments.close_request,
                    descriptor,
                    expected,
                )
                lock.revalidate()
                quiescence.revalidate()
                runtime.revalidate()
    finally:
        if runtime is not None:
            runtime.close()
        os.close(descriptor)
    print(
        f"private_reclose={capture['capture_sha256']} "
        f"capture={capture['capture_id']} "
        "provider_writer_proof=false ready_for_apply=false"
    )
    return 0


def _codex_private_sqlite_compose_plan(arguments: argparse.Namespace) -> int:
    bundle_arguments = {
        "source_a": (Path(arguments.source_a_bundle), "source"),
        "source_b": (Path(arguments.source_b_bundle), "source"),
        "destination_a": (
            Path(arguments.destination_a_bundle),
            "destination",
        ),
        "destination_b": (
            Path(arguments.destination_b_bundle),
            "destination",
        ),
    }
    required_documents = (
        "compatibility_plan",
        "adapter_registry",
        "path_map",
        "session_union_plan",
        "session_source_a",
        "session_source_b",
        "session_destination_a",
        "session_destination_b",
    )
    optional_documents = (
        "session_prefix_request",
        "session_source_prefix_a",
        "session_source_prefix_b",
        "session_destination_prefix_a",
        "session_destination_prefix_b",
        "session_close_request",
        "session_source_close_a",
        "session_source_close_b",
        "session_destination_close_a",
        "session_destination_close_b",
    )
    pinned: list[tuple[str, int, tuple[int, ...]]] = []
    documents: dict[str, dict[str, Any] | None] = {}
    try:
        for name in (*required_documents, *optional_documents):
            path = getattr(arguments, name)
            if path is None:
                documents[name] = None
                continue
            document, descriptor, expected = _read_pinned_codex_json(path)
            documents[name] = document
            pinned.append((path, descriptor, expected))
        with private_runtime.open_pinned_private_runtime_authority() as runtime:
            plan = compile_codex_private_sqlite_compose_plan(
                documents["compatibility_plan"],
                bundle_arguments["source_a"][0],
                bundle_arguments["source_b"][0],
                bundle_arguments["destination_a"][0],
                bundle_arguments["destination_b"][0],
                accept_compatibility_plan=arguments.accept_compatibility_plan,
                adapter_registry=documents["adapter_registry"],
                accept_adapter_registry=arguments.accept_adapter_registry,
                path_map=documents["path_map"],
                accept_path_map=arguments.accept_path_map,
                session_union_plan=documents["session_union_plan"],
                accept_session_union_plan=arguments.accept_session_union_plan,
                session_source_a=documents["session_source_a"],
                session_source_b=documents["session_source_b"],
                session_destination_a=documents["session_destination_a"],
                session_destination_b=documents["session_destination_b"],
                session_prefix_request=documents["session_prefix_request"],
                session_source_prefix_a=documents["session_source_prefix_a"],
                session_source_prefix_b=documents["session_source_prefix_b"],
                session_destination_prefix_a=documents["session_destination_prefix_a"],
                session_destination_prefix_b=documents["session_destination_prefix_b"],
                session_close_request=documents["session_close_request"],
                session_source_close_a=documents["session_source_close_a"],
                session_source_close_b=documents["session_source_close_b"],
                session_destination_close_a=documents["session_destination_close_a"],
                session_destination_close_b=documents["session_destination_close_b"],
                runtime_authority=runtime.record,
            )
            expected_capture_digests = {
                "source_a": plan["private_opening"]["source"]["capture_sha256s"][0],
                "source_b": plan["private_opening"]["source"]["capture_sha256s"][1],
                "destination_a": plan["private_opening"]["destination"][
                    "capture_sha256s"
                ][0],
                "destination_b": plan["private_opening"]["destination"][
                    "capture_sha256s"
                ][1],
            }

            def revalidate_inputs() -> None:
                for path, descriptor, expected in pinned:
                    _revalidate_pinned_codex_input(path, descriptor, expected)
                for name, (directory, role) in bundle_arguments.items():
                    capture, _ = read_codex_private_bundle(directory, role)
                    if capture["capture_sha256"] != expected_capture_digests[name]:
                        raise BulkloadError(
                            "private SQLite opening bundle changed during planning"
                        )
                runtime.revalidate()
                validate_codex_private_sqlite_compose_plan_against_inputs(
                    plan,
                    documents["compatibility_plan"],
                    bundle_arguments["source_a"][0],
                    bundle_arguments["source_b"][0],
                    bundle_arguments["destination_a"][0],
                    bundle_arguments["destination_b"][0],
                    adapter_registry=documents["adapter_registry"],
                    path_map=documents["path_map"],
                    session_union_plan=documents["session_union_plan"],
                    session_source_a=documents["session_source_a"],
                    session_source_b=documents["session_source_b"],
                    session_destination_a=documents["session_destination_a"],
                    session_destination_b=documents["session_destination_b"],
                    session_prefix_request=documents["session_prefix_request"],
                    session_source_prefix_a=documents["session_source_prefix_a"],
                    session_source_prefix_b=documents["session_source_prefix_b"],
                    session_destination_prefix_a=documents[
                        "session_destination_prefix_a"
                    ],
                    session_destination_prefix_b=documents[
                        "session_destination_prefix_b"
                    ],
                    session_close_request=documents["session_close_request"],
                    session_source_close_a=documents["session_source_close_a"],
                    session_source_close_b=documents["session_source_close_b"],
                    session_destination_close_a=documents[
                        "session_destination_close_a"
                    ],
                    session_destination_close_b=documents[
                        "session_destination_close_b"
                    ],
                )

            revalidate_inputs()
            protected_directories = tuple(
                directory for directory, _ in bundle_arguments.values()
            )
            recorded_roots = tuple(
                Path(root)
                for role in ("source", "destination")
                for root in {
                    plan["private_opening"][role]["stable_projection"]["codex_home"][
                        "resolved_path"
                    ],
                    plan["private_opening"][role]["stable_projection"]["sqlite_home"][
                        "resolved_path"
                    ],
                }
            )
            write_private_json_noreplace(
                Path(arguments.output),
                plan,
                protected_directories=protected_directories,
                recorded_protected_directories=recorded_roots,
            )
            try:
                revalidate_inputs()
            except BulkloadError as error:
                raise BulkloadError(
                    "private SQLite plan inputs changed after publication; "
                    f"fail-held evidence: {arguments.output}"
                ) from error
    finally:
        for _, descriptor, _ in pinned:
            try:
                os.close(descriptor)
            except OSError:
                pass
    print(
        f"plan={plan['plan_sha256']} "
        f"classification_complete="
        f"{str(plan['readiness']['classification_complete']).lower()} "
        f"blockers={len(plan['blockers'])} "
        "sqlite_compose=false sqlite_publish=false ready_for_apply=false"
    )
    return 4


def _codex_private_sqlite_compose_action_plan(
    arguments: argparse.Namespace,
) -> int:
    if arguments.output == "-":
        raise BulkloadError(
            "private SQLite action plan requires an owner-private output file"
        )
    bundles = {
        "source_a": Path(arguments.source_close_a_bundle),
        "source_b": Path(arguments.source_close_b_bundle),
        "destination_a": Path(arguments.destination_close_a_bundle),
        "destination_b": Path(arguments.destination_close_b_bundle),
    }
    opening_bundles = {
        "source_a": Path(arguments.opening_source_a_bundle),
        "source_b": Path(arguments.opening_source_b_bundle),
        "destination_a": Path(arguments.opening_destination_a_bundle),
        "destination_b": Path(arguments.opening_destination_b_bundle),
    }
    input_paths = {
        "opening_plan": arguments.opening_plan,
        "close_request": arguments.close_request,
        "opening_compatibility_plan": arguments.opening_compatibility_plan,
        "opening_adapter_registry": arguments.opening_adapter_registry,
        "opening_path_map": arguments.opening_path_map,
        "opening_session_union_plan": arguments.opening_session_union_plan,
        "opening_session_source_a": arguments.opening_session_source_a,
        "opening_session_source_b": arguments.opening_session_source_b,
        "opening_session_destination_a": (arguments.opening_session_destination_a),
        "opening_session_destination_b": (arguments.opening_session_destination_b),
        "session_source_a": arguments.session_source_close_a,
        "session_source_b": arguments.session_source_close_b,
        "session_destination_a": arguments.session_destination_close_a,
        "session_destination_b": arguments.session_destination_close_b,
    }
    optional_input_paths = {
        "opening_session_prefix_request": (arguments.opening_session_prefix_request),
        "opening_session_source_prefix_a": (arguments.opening_session_source_prefix_a),
        "opening_session_source_prefix_b": (arguments.opening_session_source_prefix_b),
        "opening_session_destination_prefix_a": (
            arguments.opening_session_destination_prefix_a
        ),
        "opening_session_destination_prefix_b": (
            arguments.opening_session_destination_prefix_b
        ),
        "opening_session_close_request": (arguments.opening_session_close_request),
        "opening_session_source_close_a": (arguments.opening_session_source_close_a),
        "opening_session_source_close_b": (arguments.opening_session_source_close_b),
        "opening_session_destination_close_a": (
            arguments.opening_session_destination_close_a
        ),
        "opening_session_destination_close_b": (
            arguments.opening_session_destination_close_b
        ),
    }
    pinned: list[tuple[str, int, tuple[int, ...]]] = []
    documents: dict[str, dict[str, Any] | None] = {}
    try:
        for name, path in input_paths.items():
            document, descriptor, expected = _read_pinned_codex_json(path)
            documents[name] = document
            pinned.append((path, descriptor, expected))
        for name, path in optional_input_paths.items():
            if path is None:
                documents[name] = None
                continue
            document, descriptor, expected = _read_pinned_codex_json(path)
            documents[name] = document
            pinned.append((path, descriptor, expected))
        with private_runtime.open_pinned_private_runtime_authority() as runtime:

            def revalidate_opening_chain() -> None:
                validate_codex_private_sqlite_close_request_against_inputs(
                    documents["close_request"],
                    documents["opening_plan"],
                    documents["opening_session_union_plan"],
                    documents["opening_session_source_a"],
                    documents["opening_session_source_b"],
                    documents["opening_session_destination_a"],
                    documents["opening_session_destination_b"],
                    runtime.record,
                    opening_compatibility_plan=documents["opening_compatibility_plan"],
                    opening_source_a_directory=opening_bundles["source_a"],
                    opening_source_b_directory=opening_bundles["source_b"],
                    opening_destination_a_directory=opening_bundles["destination_a"],
                    opening_destination_b_directory=opening_bundles["destination_b"],
                    opening_adapter_registry=documents["opening_adapter_registry"],
                    opening_path_map=documents["opening_path_map"],
                    opening_session_prefix_request=documents[
                        "opening_session_prefix_request"
                    ],
                    opening_session_source_prefix_a=documents[
                        "opening_session_source_prefix_a"
                    ],
                    opening_session_source_prefix_b=documents[
                        "opening_session_source_prefix_b"
                    ],
                    opening_session_destination_prefix_a=documents[
                        "opening_session_destination_prefix_a"
                    ],
                    opening_session_destination_prefix_b=documents[
                        "opening_session_destination_prefix_b"
                    ],
                    opening_session_close_request=documents[
                        "opening_session_close_request"
                    ],
                    opening_session_source_close_a=documents[
                        "opening_session_source_close_a"
                    ],
                    opening_session_source_close_b=documents[
                        "opening_session_source_close_b"
                    ],
                    opening_session_destination_close_a=documents[
                        "opening_session_destination_close_a"
                    ],
                    opening_session_destination_close_b=documents[
                        "opening_session_destination_close_b"
                    ],
                )

            revalidate_opening_chain()
            action_plan = compile_codex_private_sqlite_action_plan(
                documents["opening_plan"],
                documents["close_request"],
                bundles["source_a"],
                bundles["source_b"],
                bundles["destination_a"],
                bundles["destination_b"],
                documents["session_source_a"],
                documents["session_source_b"],
                documents["session_destination_a"],
                documents["session_destination_b"],
                accept_opening_plan=arguments.accept_opening_plan,
                accept_close_request=arguments.accept_close_request,
                runtime_authority=runtime.record,
            )

            def revalidate_inputs() -> None:
                for path, descriptor, expected in pinned:
                    _revalidate_pinned_codex_input(path, descriptor, expected)
                runtime.revalidate()
                revalidate_opening_chain()
                validate_codex_private_sqlite_action_plan_against_close(
                    action_plan,
                    documents["opening_plan"],
                    documents["close_request"],
                    bundles["source_a"],
                    bundles["source_b"],
                    bundles["destination_a"],
                    bundles["destination_b"],
                    documents["session_source_a"],
                    documents["session_source_b"],
                    documents["session_destination_a"],
                    documents["session_destination_b"],
                )

            revalidate_inputs()
            closing_snapshots = [
                documents[name]["snapshot"]
                for name in (
                    "session_source_a",
                    "session_source_b",
                    "session_destination_a",
                    "session_destination_b",
                )
            ]
            close_request = documents["close_request"]
            protected_roots = [
                *bundles.values(),
                *opening_bundles.values(),
                *(
                    Path(
                        close_request["private_opening"][role]["binding"][
                            "stable_projection"
                        ][authority]["resolved_path"]
                    )
                    for role in ("source", "destination")
                    for authority in ("codex_home", "sqlite_home")
                ),
            ]
            _write_pinned_codex_json(
                arguments.output,
                action_plan,
                closing_snapshots,
                pinned,
                protected_roots=protected_roots,
            )
            try:
                revalidate_inputs()
            except BulkloadError as error:
                raise BulkloadError(
                    "private SQLite action-plan inputs or runtime changed after "
                    f"publication; fail-held evidence: {arguments.output}"
                ) from error
    finally:
        for _, descriptor, _ in pinned:
            try:
                os.close(descriptor)
            except OSError:
                pass
    if arguments.output != "-":
        print(
            f"action_plan={action_plan['action_plan_sha256']} "
            "post_plan_close_proven=true "
            f"descriptive_action_complete="
            f"{str(action_plan['readiness']['descriptive_action_complete']).lower()} "
            f"ready_for_offline_compose="
            f"{str(action_plan['readiness']['ready_for_offline_compose']).lower()} "
            "composer_implemented=false sqlite_compose=false "
            "sqlite_publish=false ready_for_apply=false"
        )
    return 4


def _codex_private_install_plan(arguments: argparse.Namespace) -> int:
    source = Path(arguments.source_bundle)
    destination = Path(arguments.destination_bundle)
    compatibility = read_codex_private_state_plan(Path(arguments.compatibility_plan))
    with private_runtime.open_pinned_private_runtime_authority() as runtime:
        plan = compile_codex_private_install_plan(
            compatibility,
            source,
            destination,
            accept_compatibility_plan=arguments.accept_compatibility_plan,
            runtime_authority=runtime.record,
        )
        live_roots = (
            *compatibility["protected_live_roots"]["source"],
            *compatibility["protected_live_roots"]["destination"],
        )
        revalidate_codex_private_bundles(
            source,
            destination,
            expected_source_capture_sha256=plan["source_capture_sha256"],
            expected_destination_capture_sha256=plan[
                "destination_before_capture_sha256"
            ],
        )
        runtime.revalidate()
        write_private_json_noreplace(
            Path(arguments.output),
            plan,
            protected_directories=(source, destination),
            recorded_protected_directories=tuple(Path(root) for root in live_roots),
        )
        try:
            revalidate_codex_private_bundles(
                source,
                destination,
                expected_source_capture_sha256=plan["source_capture_sha256"],
                expected_destination_capture_sha256=plan[
                    "destination_before_capture_sha256"
                ],
            )
            runtime.revalidate()
        except BulkloadError as error:
            raise BulkloadError(
                "private install-plan inputs or runtime changed after publication; "
                f"fail-held evidence: {arguments.output}"
            ) from error
    print(
        f"plan={plan['plan_sha256']} "
        f"blockers={len(plan['blockers'])} "
        f"ready_for_apply={str(plan['ready_for_apply']).lower()}"
    )
    return 0 if plan["ready_for_apply"] else 4


def _codex_private_apply(arguments: argparse.Namespace) -> int:
    receipt = apply_codex_private_install(
        Path(arguments.install_plan),
        Path(arguments.compatibility_plan),
        Path(arguments.source_bundle),
        Path(arguments.destination_before_bundle),
        destination_codex_home=Path(arguments.destination_codex_home),
        destination_sqlite_home=(
            Path(arguments.destination_sqlite_home)
            if arguments.destination_sqlite_home is not None
            else None
        ),
        rollback_directory=Path(arguments.rollback_directory),
        post_capture_directory=Path(arguments.post_capture_directory),
        recovery_capture_directory=Path(arguments.recovery_capture_directory),
        journal_path=Path(arguments.journal),
        receipt_path=Path(arguments.receipt),
        accept_plan=arguments.accept_plan,
        destination_host_authority_id=arguments.destination_host_authority_id,
        codex_version=arguments.codex_version,
        quiescence_attestation_path=Path(arguments.quiescence_attestation),
        accept_quiescence_attestation=(arguments.accept_quiescence_attestation),
        acknowledge_private_apply=arguments.acknowledge_private_apply,
    )
    print(
        f"receipt={receipt['receipt_sha256']} "
        f"offline_verified={str(receipt['offline_verified']).lower()} "
        "provider_runtime_acceptance_verified=false"
    )
    return 0


def _codex_private_verify(arguments: argparse.Namespace) -> int:
    receipt = verify_codex_private_install(
        Path(arguments.install_plan),
        Path(arguments.compatibility_plan),
        Path(arguments.source_bundle),
        Path(arguments.destination_before_bundle),
        Path(arguments.apply_receipt),
        destination_codex_home=Path(arguments.destination_codex_home),
        destination_sqlite_home=(
            Path(arguments.destination_sqlite_home)
            if arguments.destination_sqlite_home is not None
            else None
        ),
        capture_directory=Path(arguments.capture_directory),
        receipt_path=Path(arguments.receipt),
        accept_plan=arguments.accept_plan,
        accept_apply_receipt=arguments.accept_apply_receipt,
        destination_host_authority_id=arguments.destination_host_authority_id,
        codex_version=arguments.codex_version,
        quiescence_attestation_path=Path(arguments.quiescence_attestation),
        accept_quiescence_attestation=(arguments.accept_quiescence_attestation),
        acknowledge_private_verify=arguments.acknowledge_private_verify,
    )
    print(
        f"receipt={receipt['receipt_sha256']} "
        f"offline_verified={str(receipt['offline_verified']).lower()} "
        "provider_runtime_acceptance_verified=false"
    )
    return 0


def _codex_private_rollback(arguments: argparse.Namespace) -> int:
    receipt = rollback_codex_private_install(
        Path(arguments.install_plan),
        Path(arguments.compatibility_plan),
        Path(arguments.source_bundle),
        Path(arguments.destination_before_bundle),
        Path(arguments.apply_receipt),
        Path(arguments.rollback_directory),
        destination_codex_home=Path(arguments.destination_codex_home),
        destination_sqlite_home=(
            Path(arguments.destination_sqlite_home)
            if arguments.destination_sqlite_home is not None
            else None
        ),
        preflight_capture_directory=Path(arguments.preflight_capture_directory),
        post_capture_directory=Path(arguments.post_capture_directory),
        recovery_capture_directory=Path(arguments.recovery_capture_directory),
        journal_path=Path(arguments.journal),
        receipt_path=Path(arguments.receipt),
        accept_plan=arguments.accept_plan,
        accept_apply_receipt=arguments.accept_apply_receipt,
        destination_host_authority_id=arguments.destination_host_authority_id,
        codex_version=arguments.codex_version,
        quiescence_attestation_path=Path(arguments.quiescence_attestation),
        accept_quiescence_attestation=(arguments.accept_quiescence_attestation),
        acknowledge_private_rollback=arguments.acknowledge_private_rollback,
        acknowledge_no_post_apply_writes=(arguments.acknowledge_no_post_apply_writes),
    )
    print(
        f"receipt={receipt['receipt_sha256']} "
        f"offline_restored={str(receipt['offline_restored']).lower()} "
        "provider_runtime_acceptance_verified=false"
    )
    return 0


def _codex_private_recover(arguments: argparse.Namespace) -> int:
    receipt = recover_codex_private_mutation(
        Path(arguments.install_plan),
        Path(arguments.compatibility_plan),
        Path(arguments.source_bundle),
        Path(arguments.destination_before_bundle),
        Path(arguments.journal),
        apply_receipt_path=(
            Path(arguments.apply_receipt)
            if arguments.apply_receipt is not None
            else None
        ),
        destination_codex_home=Path(arguments.destination_codex_home),
        destination_sqlite_home=(
            Path(arguments.destination_sqlite_home)
            if arguments.destination_sqlite_home is not None
            else None
        ),
        preflight_capture_directory=Path(arguments.preflight_capture_directory),
        post_capture_directory=Path(arguments.post_capture_directory),
        receipt_path=Path(arguments.receipt),
        accept_plan=arguments.accept_plan,
        accept_journal=arguments.accept_journal,
        accept_apply_receipt=arguments.accept_apply_receipt,
        destination_host_authority_id=arguments.destination_host_authority_id,
        codex_version=arguments.codex_version,
        quiescence_attestation_path=Path(arguments.quiescence_attestation),
        accept_quiescence_attestation=(arguments.accept_quiescence_attestation),
        acknowledge_private_recovery=arguments.acknowledge_private_recovery,
    )
    print(
        f"receipt={receipt['receipt_sha256']} "
        f"decision={receipt['decision']} "
        "provider_runtime_acceptance_verified=false"
    )
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="bulkload",
        description="Manifest-first repository and agent-context migration controller",
    )
    parser.add_argument(
        "--version", action="version", version=f"%(prog)s {__version__}"
    )
    commands = parser.add_subparsers(dest="command", required=True)

    capture = commands.add_parser(
        "capture", help="capture a read-only repo or fleet snapshot"
    )
    capture.add_argument("--root", required=True)
    capture.add_argument("--mode", choices=("repo", "fleet"), default="repo")
    capture.add_argument("--output", required=True)
    capture.add_argument("--include-ignored", action="store_true")
    capture.add_argument("--max-files", type=int, default=DEFAULT_MAX_FILES)
    capture.add_argument("--max-bytes", type=int, default=DEFAULT_MAX_BYTES)
    capture.set_defaults(handler=_capture)

    plan = commands.add_parser(
        "plan", help="compile a digest-bound plan after two stable scans"
    )
    plan.add_argument("--source-a", required=True)
    plan.add_argument("--source-b", required=True)
    plan.add_argument("--destination", required=True)
    plan.add_argument("--output", required=True)
    plan.set_defaults(handler=_plan)

    apply = commands.add_parser(
        "apply", help="apply a ready plan to locally visible roots"
    )
    apply.add_argument("--plan", required=True)
    apply.add_argument("--accept-plan", required=True)
    apply.add_argument("--source-root", required=True)
    apply.add_argument("--destination-root", required=True)
    apply.add_argument("--state-root", required=True)
    apply.add_argument("--receipt", required=True)
    apply.set_defaults(handler=_apply)

    verify = commands.add_parser(
        "verify", help="verify a plan against a fresh destination scan"
    )
    verify.add_argument("--plan", required=True)
    verify.add_argument("--accept-plan", required=True)
    verify.add_argument("--destination", required=True)
    verify.add_argument("--output", required=True)
    verify.set_defaults(handler=_verify)

    files = commands.add_parser(
        "files", help="emit the expected-source staging allowlist from a ready plan"
    )
    files.add_argument("--plan", required=True)
    files.add_argument("--accept-plan", required=True)
    files.add_argument("--null", action="store_true")
    files.set_defaults(handler=_files)

    codex_capture = commands.add_parser(
        "codex-capture",
        help="capture one quiesced, role-bound Codex rollout catalog",
    )
    codex_capture.add_argument("--root", required=True)
    codex_capture.add_argument("--output", required=True)
    codex_capture.add_argument(
        "--role",
        choices=("source", "destination"),
        required=True,
    )
    codex_capture.add_argument(
        "--acknowledge-writers-quiesced",
        action="store_true",
        help="assert that every writer to this rollout root is stopped",
    )
    codex_capture.add_argument(
        "--host-authority-id",
        required=True,
        help="stable non-secret UUID naming this host filesystem authority",
    )
    codex_capture.add_argument(
        "--max-files", type=int, default=DEFAULT_MAX_SESSION_FILES
    )
    codex_capture.add_argument(
        "--max-entries", type=int, default=DEFAULT_MAX_SESSION_ENTRIES
    )
    codex_capture.add_argument(
        "--max-directories",
        type=int,
        default=DEFAULT_MAX_SESSION_DIRECTORIES,
    )
    codex_capture.add_argument(
        "--max-bytes", type=int, default=DEFAULT_MAX_SESSION_BYTES
    )
    codex_capture.add_argument(
        "--max-file-bytes",
        type=int,
        default=DEFAULT_MAX_SESSION_FILE_BYTES,
    )
    codex_capture.add_argument(
        "--max-record-bytes",
        type=int,
        default=DEFAULT_MAX_SESSION_RECORD_BYTES,
    )
    codex_capture.add_argument(
        "--max-records-per-file",
        type=int,
        default=DEFAULT_MAX_SESSION_RECORDS_PER_FILE,
    )
    codex_capture.add_argument(
        "--max-path-bytes",
        type=int,
        default=DEFAULT_MAX_SESSION_PATH_BYTES,
    )
    codex_capture.add_argument(
        "--max-path-components",
        type=int,
        default=DEFAULT_MAX_SESSION_PATH_COMPONENTS,
    )
    codex_capture.add_argument(
        "--max-catalog-bytes",
        type=int,
        default=DEFAULT_MAX_SESSION_CATALOG_BYTES,
    )
    codex_capture.add_argument(
        "--max-errors",
        type=int,
        default=DEFAULT_MAX_SESSION_ERRORS,
    )
    codex_capture.add_argument(
        "--max-output-bytes",
        type=int,
        default=DEFAULT_MAX_SESSION_OUTPUT_BYTES,
    )
    codex_capture.set_defaults(handler=_codex_capture)

    codex_prefix_request = commands.add_parser(
        "codex-prefix-request",
        aliases=("codex-prefix-requests",),
        help="compile immutable requests for same-UUID prefix observations",
    )
    codex_prefix_request.add_argument("--source-a", required=True)
    codex_prefix_request.add_argument("--source-b", required=True)
    codex_prefix_request.add_argument("--destination-a", required=True)
    codex_prefix_request.add_argument("--destination-b", required=True)
    codex_prefix_request.add_argument("--output", required=True)
    codex_prefix_request.set_defaults(handler=_codex_prefix_request)

    codex_prefix_proof = commands.add_parser(
        "codex-prefix-proof",
        help="capture one role-bound pass over requested byte prefixes",
    )
    codex_prefix_proof.add_argument("--prefix-request", required=True)
    codex_prefix_proof.add_argument("--source-a", required=True)
    codex_prefix_proof.add_argument("--source-b", required=True)
    codex_prefix_proof.add_argument("--destination-a", required=True)
    codex_prefix_proof.add_argument("--destination-b", required=True)
    codex_prefix_proof.add_argument("--root", required=True)
    codex_prefix_proof.add_argument(
        "--role",
        choices=("source", "destination"),
        required=True,
    )
    codex_prefix_proof.add_argument(
        "--acknowledge-writers-quiesced",
        action="store_true",
        help="assert that every writer to this rollout root is stopped",
    )
    codex_prefix_proof.add_argument("--output", required=True)
    codex_prefix_proof.set_defaults(handler=_codex_prefix_proof)

    codex_close_request = commands.add_parser(
        "codex-close-request",
        help="bind every required prefix proof before close capture",
    )
    codex_close_request.add_argument("--prefix-request", required=True)
    codex_close_request.add_argument("--source-a", required=True)
    codex_close_request.add_argument("--source-b", required=True)
    codex_close_request.add_argument("--destination-a", required=True)
    codex_close_request.add_argument("--destination-b", required=True)
    codex_close_request.add_argument("--source-prefix-a")
    codex_close_request.add_argument("--source-prefix-b")
    codex_close_request.add_argument("--destination-prefix-a")
    codex_close_request.add_argument("--destination-prefix-b")
    codex_close_request.add_argument("--output", required=True)
    codex_close_request.set_defaults(handler=_codex_close_request)

    codex_close_capture = commands.add_parser(
        "codex-close-capture",
        help="capture one fresh v2 snapshot bound to a close request",
    )
    codex_close_capture.add_argument("--close-request", required=True)
    codex_close_capture.add_argument("--root", required=True)
    codex_close_capture.add_argument(
        "--role",
        choices=("source", "destination"),
        required=True,
    )
    codex_close_capture.add_argument(
        "--acknowledge-writers-quiesced",
        action="store_true",
        help="assert that every writer to this rollout root is stopped",
    )
    codex_close_capture.add_argument("--output", required=True)
    codex_close_capture.set_defaults(handler=_codex_close_capture)

    codex_plan = commands.add_parser(
        "codex-plan",
        help="compile a dry-run Codex session union plan",
    )
    codex_plan.add_argument("--source-a", required=True)
    codex_plan.add_argument("--source-b", required=True)
    codex_plan.add_argument("--destination-a", required=True)
    codex_plan.add_argument("--destination-b", required=True)
    codex_plan.add_argument("--prefix-request")
    codex_plan.add_argument("--source-prefix-a")
    codex_plan.add_argument("--source-prefix-b")
    codex_plan.add_argument("--destination-prefix-a")
    codex_plan.add_argument("--destination-prefix-b")
    codex_plan.add_argument("--close-request")
    codex_plan.add_argument("--source-close-a")
    codex_plan.add_argument("--source-close-b")
    codex_plan.add_argument("--destination-close-a")
    codex_plan.add_argument("--destination-close-b")
    codex_plan.add_argument("--output", required=True)
    codex_plan.set_defaults(handler=_codex_plan)

    codex_private_capture = commands.add_parser(
        "codex-private-capture",
        help=(
            "capture typed private state; SQLite requires quiescence and "
            "no live WAL/SHM/journal sidecars"
        ),
    )
    codex_private_capture.add_argument("--codex-home", required=True)
    codex_private_capture.add_argument(
        "--sqlite-home",
        help="required with --include-sqlite; pass the resolved effective authority",
    )
    codex_private_capture.add_argument("--output-directory", required=True)
    codex_private_capture.add_argument(
        "--role",
        choices=("source", "destination"),
        required=True,
    )
    codex_private_capture.add_argument("--host-authority-id", required=True)
    codex_private_capture.add_argument("--codex-version", required=True)
    codex_private_capture.add_argument("--include-auth", action="store_true")
    codex_private_capture.add_argument("--include-sqlite", action="store_true")
    codex_private_capture.add_argument(
        "--acknowledge-private-capture",
        action="store_true",
        help="acknowledge that the output contains private provider state",
    )
    codex_private_capture.add_argument(
        "--quiescence-attestation",
        required=True,
        help="short-lived owner-private operator attestation",
    )
    codex_private_capture.add_argument(
        "--accept-quiescence-attestation",
        required=True,
        help="exact SHA-256 of the accepted quiescence attestation",
    )
    codex_private_capture.add_argument(
        "--max-sqlite-families",
        type=int,
        default=DEFAULT_MAX_SQLITE_FAMILIES,
    )
    codex_private_capture.add_argument(
        "--max-total-sqlite-bytes",
        type=int,
        default=DEFAULT_MAX_TOTAL_SQLITE_BYTES,
    )
    codex_private_capture.add_argument(
        "--backup-timeout-seconds",
        type=int,
        default=DEFAULT_BACKUP_TIMEOUT_SECONDS,
    )
    codex_private_capture.add_argument(
        "--max-thread-entries",
        type=int,
        default=DEFAULT_MAX_THREAD_ENTRIES,
    )
    codex_private_capture.add_argument(
        "--max-thread-index-bytes",
        type=int,
        default=DEFAULT_MAX_THREAD_INDEX_BYTES,
    )
    codex_private_capture.add_argument(
        "--max-metadata-entries",
        type=int,
        default=DEFAULT_MAX_METADATA_ENTRIES,
    )
    codex_private_capture.add_argument(
        "--max-metadata-bytes",
        type=int,
        default=DEFAULT_MAX_METADATA_BYTES,
    )
    codex_private_capture.set_defaults(handler=_codex_private_capture)

    codex_private_quiescence = commands.add_parser(
        "codex-private-quiescence-attest",
        help=(
            "record an operator-attested procedural writer fence; "
            "this is not provider-writer proof"
        ),
    )
    codex_private_quiescence.add_argument("--codex-home", required=True)
    codex_private_quiescence.add_argument("--sqlite-home")
    codex_private_quiescence.add_argument("--output", required=True)
    codex_private_quiescence.add_argument("--operation-output", required=True)
    codex_private_quiescence.add_argument(
        "--purpose",
        choices=("capture", "close", "apply", "verify", "rollback", "recover"),
        required=True,
    )
    codex_private_quiescence.add_argument(
        "--capture-role",
        choices=("source", "destination"),
    )
    codex_private_quiescence.add_argument("--host-authority-id", required=True)
    codex_private_quiescence.add_argument("--codex-version", required=True)
    codex_private_quiescence.add_argument("--include-auth", action="store_true")
    codex_private_quiescence.add_argument("--include-sqlite", action="store_true")
    codex_private_quiescence.add_argument("--accept-plan")
    codex_private_quiescence.add_argument("--accept-apply-receipt")
    codex_private_quiescence.add_argument("--accept-journal")
    codex_private_quiescence.add_argument(
        "--ttl-seconds",
        type=int,
        default=DEFAULT_PRIVATE_QUIESCENCE_TTL_SECONDS,
    )
    codex_private_quiescence.add_argument(
        "--acknowledge-writers-quiesced",
        action="store_true",
        help=(
            "attest that provider writers are procedurally quiesced; "
            "Bulkload cannot independently prove this"
        ),
    )
    codex_private_quiescence.set_defaults(handler=_codex_private_quiescence_attest)

    codex_private_plan = commands.add_parser(
        "codex-private-plan",
        help="compare private captures without installing live state",
    )
    codex_private_plan.add_argument("--source-bundle", required=True)
    codex_private_plan.add_argument("--destination-bundle", required=True)
    codex_private_plan.add_argument("--output", required=True)
    codex_private_plan.set_defaults(handler=_codex_private_plan)

    codex_private_sqlite_close_request = commands.add_parser(
        "codex-private-sqlite-close-request",
        help=(
            "bind one operator-attested writer-stop epoch to the exact private "
            "SQLite opening and session-union bodies"
        ),
    )
    codex_private_sqlite_close_request.add_argument(
        "--opening-plan",
        required=True,
    )
    codex_private_sqlite_close_request.add_argument(
        "--accept-opening-plan",
        required=True,
    )
    codex_private_sqlite_close_request.add_argument(
        "--compatibility-plan",
        required=True,
    )
    codex_private_sqlite_close_request.add_argument(
        "--source-a-bundle",
        required=True,
    )
    codex_private_sqlite_close_request.add_argument(
        "--source-b-bundle",
        required=True,
    )
    codex_private_sqlite_close_request.add_argument(
        "--destination-a-bundle",
        required=True,
    )
    codex_private_sqlite_close_request.add_argument(
        "--destination-b-bundle",
        required=True,
    )
    codex_private_sqlite_close_request.add_argument(
        "--adapter-registry",
        required=True,
    )
    codex_private_sqlite_close_request.add_argument(
        "--path-map",
        required=True,
    )
    codex_private_sqlite_close_request.add_argument(
        "--session-union-plan",
        required=True,
    )
    codex_private_sqlite_close_request.add_argument(
        "--accept-session-union-plan",
        required=True,
    )
    codex_private_sqlite_close_request.add_argument(
        "--session-source-a",
        required=True,
    )
    codex_private_sqlite_close_request.add_argument(
        "--session-source-b",
        required=True,
    )
    codex_private_sqlite_close_request.add_argument(
        "--session-destination-a",
        required=True,
    )
    codex_private_sqlite_close_request.add_argument(
        "--session-destination-b",
        required=True,
    )
    codex_private_sqlite_close_request.add_argument("--session-prefix-request")
    codex_private_sqlite_close_request.add_argument("--session-source-prefix-a")
    codex_private_sqlite_close_request.add_argument("--session-source-prefix-b")
    codex_private_sqlite_close_request.add_argument("--session-destination-prefix-a")
    codex_private_sqlite_close_request.add_argument("--session-destination-prefix-b")
    codex_private_sqlite_close_request.add_argument("--session-close-request")
    codex_private_sqlite_close_request.add_argument("--session-source-close-a")
    codex_private_sqlite_close_request.add_argument("--session-source-close-b")
    codex_private_sqlite_close_request.add_argument("--session-destination-close-a")
    codex_private_sqlite_close_request.add_argument("--session-destination-close-b")
    codex_private_sqlite_close_request.add_argument(
        "--writer-stop-epoch-id",
        required=True,
    )
    codex_private_sqlite_close_request.add_argument(
        "--writer-stop-epoch-at",
        required=True,
        help="canonical UTC-seconds timestamp for the attended writer-stop epoch",
    )
    codex_private_sqlite_close_request.add_argument(
        "--acknowledge-provider-writers-stopped",
        action="store_true",
        help=(
            "record an operator procedural assertion; this does not establish "
            "provider-writer proof"
        ),
    )
    codex_private_sqlite_close_request.add_argument("--output", required=True)
    codex_private_sqlite_close_request.set_defaults(
        handler=_codex_private_sqlite_close_request
    )

    codex_private_sqlite_session_reclose = commands.add_parser(
        "codex-private-sqlite-session-reclose",
        help=(
            "capture one fresh live session root against a private SQLite "
            "writer-stop close request"
        ),
    )
    codex_private_sqlite_session_reclose.add_argument(
        "--close-request",
        required=True,
    )
    codex_private_sqlite_session_reclose.add_argument(
        "--accept-close-request",
        required=True,
    )
    codex_private_sqlite_session_reclose.add_argument("--root", required=True)
    codex_private_sqlite_session_reclose.add_argument(
        "--role",
        choices=("source", "destination"),
        required=True,
    )
    codex_private_sqlite_session_reclose.add_argument(
        "--writer-stop-epoch-id",
        required=True,
    )
    codex_private_sqlite_session_reclose.add_argument(
        "--acknowledge-writers-quiesced",
        action="store_true",
        help="assert that every writer to this live session root is stopped",
    )
    codex_private_sqlite_session_reclose.add_argument("--output", required=True)
    codex_private_sqlite_session_reclose.set_defaults(
        handler=_codex_private_sqlite_session_reclose
    )

    codex_private_sqlite_private_reclose = commands.add_parser(
        "codex-private-sqlite-private-reclose",
        help=(
            "capture one fresh private Codex/SQLite bundle bound to the exact "
            "writer-stop close request"
        ),
    )
    codex_private_sqlite_private_reclose.add_argument(
        "--close-request",
        required=True,
    )
    codex_private_sqlite_private_reclose.add_argument(
        "--accept-close-request",
        required=True,
    )
    codex_private_sqlite_private_reclose.add_argument(
        "--writer-stop-epoch-id",
        required=True,
    )
    codex_private_sqlite_private_reclose.add_argument(
        "--codex-home",
        required=True,
    )
    codex_private_sqlite_private_reclose.add_argument(
        "--sqlite-home",
        required=True,
    )
    codex_private_sqlite_private_reclose.add_argument(
        "--output-directory",
        required=True,
    )
    codex_private_sqlite_private_reclose.add_argument(
        "--role",
        choices=("source", "destination"),
        required=True,
    )
    codex_private_sqlite_private_reclose.add_argument(
        "--host-authority-id",
        required=True,
    )
    codex_private_sqlite_private_reclose.add_argument(
        "--codex-version",
        required=True,
    )
    codex_private_sqlite_private_reclose.add_argument(
        "--acknowledge-private-capture",
        action="store_true",
        help="acknowledge that the output contains private provider state",
    )
    codex_private_sqlite_private_reclose.add_argument(
        "--quiescence-attestation",
        required=True,
    )
    codex_private_sqlite_private_reclose.add_argument(
        "--accept-quiescence-attestation",
        required=True,
    )
    codex_private_sqlite_private_reclose.set_defaults(
        handler=_codex_private_sqlite_private_reclose
    )

    codex_private_sqlite_plan = commands.add_parser(
        "codex-private-sqlite-compose-plan",
        help=(
            "compile a four-pass, session-bound SQLite classification request; "
            "no composer, publisher, installer, or apply is exposed"
        ),
    )
    codex_private_sqlite_plan.add_argument(
        "--compatibility-plan",
        required=True,
    )
    codex_private_sqlite_plan.add_argument(
        "--accept-compatibility-plan",
        required=True,
    )
    codex_private_sqlite_plan.add_argument("--source-a-bundle", required=True)
    codex_private_sqlite_plan.add_argument("--source-b-bundle", required=True)
    codex_private_sqlite_plan.add_argument(
        "--destination-a-bundle",
        required=True,
    )
    codex_private_sqlite_plan.add_argument(
        "--destination-b-bundle",
        required=True,
    )
    codex_private_sqlite_plan.add_argument("--adapter-registry", required=True)
    codex_private_sqlite_plan.add_argument(
        "--accept-adapter-registry",
        required=True,
    )
    codex_private_sqlite_plan.add_argument("--path-map", required=True)
    codex_private_sqlite_plan.add_argument("--accept-path-map", required=True)
    codex_private_sqlite_plan.add_argument(
        "--session-union-plan",
        required=True,
    )
    codex_private_sqlite_plan.add_argument(
        "--accept-session-union-plan",
        required=True,
    )
    codex_private_sqlite_plan.add_argument("--session-source-a", required=True)
    codex_private_sqlite_plan.add_argument("--session-source-b", required=True)
    codex_private_sqlite_plan.add_argument(
        "--session-destination-a",
        required=True,
    )
    codex_private_sqlite_plan.add_argument(
        "--session-destination-b",
        required=True,
    )
    codex_private_sqlite_plan.add_argument("--session-prefix-request")
    codex_private_sqlite_plan.add_argument("--session-source-prefix-a")
    codex_private_sqlite_plan.add_argument("--session-source-prefix-b")
    codex_private_sqlite_plan.add_argument("--session-destination-prefix-a")
    codex_private_sqlite_plan.add_argument("--session-destination-prefix-b")
    codex_private_sqlite_plan.add_argument("--session-close-request")
    codex_private_sqlite_plan.add_argument("--session-source-close-a")
    codex_private_sqlite_plan.add_argument("--session-source-close-b")
    codex_private_sqlite_plan.add_argument("--session-destination-close-a")
    codex_private_sqlite_plan.add_argument("--session-destination-close-b")
    codex_private_sqlite_plan.add_argument("--output", required=True)
    codex_private_sqlite_plan.set_defaults(handler=_codex_private_sqlite_compose_plan)

    codex_private_sqlite_action_plan = commands.add_parser(
        "codex-private-sqlite-compose-action-plan",
        help=(
            "compile a closed descriptive offline SQLite action plan; no "
            "composer, publisher, installer, or apply is exposed"
        ),
    )
    codex_private_sqlite_action_plan.add_argument(
        "--opening-plan",
        required=True,
    )
    codex_private_sqlite_action_plan.add_argument(
        "--accept-opening-plan",
        required=True,
    )
    codex_private_sqlite_action_plan.add_argument(
        "--close-request",
        required=True,
    )
    codex_private_sqlite_action_plan.add_argument(
        "--accept-close-request",
        required=True,
    )
    codex_private_sqlite_action_plan.add_argument(
        "--opening-compatibility-plan",
        required=True,
        help="the exact compatibility plan consumed by the v4 opening",
    )
    codex_private_sqlite_action_plan.add_argument(
        "--opening-source-a-bundle",
        required=True,
    )
    codex_private_sqlite_action_plan.add_argument(
        "--opening-source-b-bundle",
        required=True,
    )
    codex_private_sqlite_action_plan.add_argument(
        "--opening-destination-a-bundle",
        required=True,
    )
    codex_private_sqlite_action_plan.add_argument(
        "--opening-destination-b-bundle",
        required=True,
    )
    codex_private_sqlite_action_plan.add_argument(
        "--opening-adapter-registry",
        required=True,
    )
    codex_private_sqlite_action_plan.add_argument(
        "--opening-path-map",
        required=True,
    )
    codex_private_sqlite_action_plan.add_argument(
        "--opening-session-union-plan",
        required=True,
    )
    codex_private_sqlite_action_plan.add_argument(
        "--opening-session-source-a",
        required=True,
    )
    codex_private_sqlite_action_plan.add_argument(
        "--opening-session-source-b",
        required=True,
    )
    codex_private_sqlite_action_plan.add_argument(
        "--opening-session-destination-a",
        required=True,
    )
    codex_private_sqlite_action_plan.add_argument(
        "--opening-session-destination-b",
        required=True,
    )
    codex_private_sqlite_action_plan.add_argument(
        "--opening-session-prefix-request",
    )
    codex_private_sqlite_action_plan.add_argument(
        "--opening-session-source-prefix-a",
    )
    codex_private_sqlite_action_plan.add_argument(
        "--opening-session-source-prefix-b",
    )
    codex_private_sqlite_action_plan.add_argument(
        "--opening-session-destination-prefix-a",
    )
    codex_private_sqlite_action_plan.add_argument(
        "--opening-session-destination-prefix-b",
    )
    codex_private_sqlite_action_plan.add_argument(
        "--opening-session-close-request",
    )
    codex_private_sqlite_action_plan.add_argument(
        "--opening-session-source-close-a",
    )
    codex_private_sqlite_action_plan.add_argument(
        "--opening-session-source-close-b",
    )
    codex_private_sqlite_action_plan.add_argument(
        "--opening-session-destination-close-a",
    )
    codex_private_sqlite_action_plan.add_argument(
        "--opening-session-destination-close-b",
    )
    codex_private_sqlite_action_plan.add_argument(
        "--source-close-a-bundle",
        required=True,
    )
    codex_private_sqlite_action_plan.add_argument(
        "--source-close-b-bundle",
        required=True,
    )
    codex_private_sqlite_action_plan.add_argument(
        "--destination-close-a-bundle",
        required=True,
    )
    codex_private_sqlite_action_plan.add_argument(
        "--destination-close-b-bundle",
        required=True,
    )
    codex_private_sqlite_action_plan.add_argument(
        "--session-source-close-a",
        required=True,
    )
    codex_private_sqlite_action_plan.add_argument(
        "--session-source-close-b",
        required=True,
    )
    codex_private_sqlite_action_plan.add_argument(
        "--session-destination-close-a",
        required=True,
    )
    codex_private_sqlite_action_plan.add_argument(
        "--session-destination-close-b",
        required=True,
    )
    codex_private_sqlite_action_plan.add_argument("--output", required=True)
    codex_private_sqlite_action_plan.set_defaults(
        handler=_codex_private_sqlite_compose_action_plan
    )

    codex_private_install_plan = commands.add_parser(
        "codex-private-install-plan",
        help=(
            "compile auth-only source to auth+SQLite destination install; "
            "destination SQLite remains exact and is never composed"
        ),
    )
    codex_private_install_plan.add_argument(
        "--compatibility-plan",
        required=True,
    )
    codex_private_install_plan.add_argument("--source-bundle", required=True)
    codex_private_install_plan.add_argument(
        "--destination-bundle",
        required=True,
    )
    codex_private_install_plan.add_argument(
        "--accept-compatibility-plan",
        required=True,
    )
    codex_private_install_plan.add_argument("--output", required=True)
    codex_private_install_plan.set_defaults(handler=_codex_private_install_plan)

    codex_private_apply = commands.add_parser(
        "codex-private-apply",
        help=(
            "attended atomic auth install with exact SQLite preservation; "
            "offline receipt is not provider authentication proof"
        ),
    )
    codex_private_apply.add_argument("--install-plan", required=True)
    codex_private_apply.add_argument("--compatibility-plan", required=True)
    codex_private_apply.add_argument("--source-bundle", required=True)
    codex_private_apply.add_argument(
        "--destination-before-bundle",
        required=True,
    )
    codex_private_apply.add_argument("--destination-codex-home", required=True)
    codex_private_apply.add_argument("--destination-sqlite-home")
    codex_private_apply.add_argument("--rollback-directory", required=True)
    codex_private_apply.add_argument(
        "--post-capture-directory",
        required=True,
    )
    codex_private_apply.add_argument(
        "--recovery-capture-directory",
        required=True,
    )
    codex_private_apply.add_argument("--journal", required=True)
    codex_private_apply.add_argument("--receipt", required=True)
    codex_private_apply.add_argument("--accept-plan", required=True)
    codex_private_apply.add_argument(
        "--destination-host-authority-id",
        required=True,
    )
    codex_private_apply.add_argument("--codex-version", required=True)
    codex_private_apply.add_argument(
        "--quiescence-attestation",
        required=True,
    )
    codex_private_apply.add_argument(
        "--accept-quiescence-attestation",
        required=True,
    )
    codex_private_apply.add_argument(
        "--acknowledge-private-apply",
        action="store_true",
    )
    codex_private_apply.set_defaults(handler=_codex_private_apply)

    codex_private_verify = commands.add_parser(
        "codex-private-verify",
        help=(
            "independently verify installed bytes offline; fresh provider "
            "authentication remains an attended external acceptance step"
        ),
    )
    codex_private_verify.add_argument("--install-plan", required=True)
    codex_private_verify.add_argument("--compatibility-plan", required=True)
    codex_private_verify.add_argument("--source-bundle", required=True)
    codex_private_verify.add_argument(
        "--destination-before-bundle",
        required=True,
    )
    codex_private_verify.add_argument("--apply-receipt", required=True)
    codex_private_verify.add_argument("--destination-codex-home", required=True)
    codex_private_verify.add_argument("--destination-sqlite-home")
    codex_private_verify.add_argument("--capture-directory", required=True)
    codex_private_verify.add_argument("--receipt", required=True)
    codex_private_verify.add_argument("--accept-plan", required=True)
    codex_private_verify.add_argument("--accept-apply-receipt", required=True)
    codex_private_verify.add_argument(
        "--destination-host-authority-id",
        required=True,
    )
    codex_private_verify.add_argument("--codex-version", required=True)
    codex_private_verify.add_argument(
        "--quiescence-attestation",
        required=True,
    )
    codex_private_verify.add_argument(
        "--accept-quiescence-attestation",
        required=True,
    )
    codex_private_verify.add_argument(
        "--acknowledge-private-verify",
        action="store_true",
    )
    codex_private_verify.set_defaults(handler=_codex_private_verify)

    codex_private_rollback = commands.add_parser(
        "codex-private-rollback",
        help="attended exact rollback after a no-post-apply-write assertion",
    )
    codex_private_rollback.add_argument("--install-plan", required=True)
    codex_private_rollback.add_argument("--compatibility-plan", required=True)
    codex_private_rollback.add_argument("--source-bundle", required=True)
    codex_private_rollback.add_argument(
        "--destination-before-bundle",
        required=True,
    )
    codex_private_rollback.add_argument("--apply-receipt", required=True)
    codex_private_rollback.add_argument("--rollback-directory", required=True)
    codex_private_rollback.add_argument(
        "--destination-codex-home",
        required=True,
    )
    codex_private_rollback.add_argument("--destination-sqlite-home")
    codex_private_rollback.add_argument(
        "--preflight-capture-directory",
        required=True,
    )
    codex_private_rollback.add_argument(
        "--post-capture-directory",
        required=True,
    )
    codex_private_rollback.add_argument(
        "--recovery-capture-directory",
        required=True,
    )
    codex_private_rollback.add_argument("--journal", required=True)
    codex_private_rollback.add_argument("--receipt", required=True)
    codex_private_rollback.add_argument("--accept-plan", required=True)
    codex_private_rollback.add_argument(
        "--accept-apply-receipt",
        required=True,
    )
    codex_private_rollback.add_argument(
        "--destination-host-authority-id",
        required=True,
    )
    codex_private_rollback.add_argument("--codex-version", required=True)
    codex_private_rollback.add_argument(
        "--quiescence-attestation",
        required=True,
    )
    codex_private_rollback.add_argument(
        "--accept-quiescence-attestation",
        required=True,
    )
    codex_private_rollback.add_argument(
        "--acknowledge-private-rollback",
        action="store_true",
    )
    codex_private_rollback.add_argument(
        "--acknowledge-no-post-apply-writes",
        action="store_true",
    )
    codex_private_rollback.set_defaults(handler=_codex_private_rollback)

    codex_private_recover = commands.add_parser(
        "codex-private-recover",
        help="recover an interrupted typed private auth mutation",
    )
    codex_private_recover.add_argument("--install-plan", required=True)
    codex_private_recover.add_argument("--compatibility-plan", required=True)
    codex_private_recover.add_argument("--source-bundle", required=True)
    codex_private_recover.add_argument(
        "--destination-before-bundle",
        required=True,
    )
    codex_private_recover.add_argument("--journal", required=True)
    codex_private_recover.add_argument("--apply-receipt")
    codex_private_recover.add_argument(
        "--destination-codex-home",
        required=True,
    )
    codex_private_recover.add_argument("--destination-sqlite-home")
    codex_private_recover.add_argument(
        "--preflight-capture-directory",
        required=True,
    )
    codex_private_recover.add_argument(
        "--post-capture-directory",
        required=True,
    )
    codex_private_recover.add_argument("--receipt", required=True)
    codex_private_recover.add_argument("--accept-plan", required=True)
    codex_private_recover.add_argument("--accept-journal", required=True)
    codex_private_recover.add_argument("--accept-apply-receipt")
    codex_private_recover.add_argument(
        "--destination-host-authority-id",
        required=True,
    )
    codex_private_recover.add_argument("--codex-version", required=True)
    codex_private_recover.add_argument(
        "--quiescence-attestation",
        required=True,
    )
    codex_private_recover.add_argument(
        "--accept-quiescence-attestation",
        required=True,
    )
    codex_private_recover.add_argument(
        "--acknowledge-private-recovery",
        action="store_true",
    )
    codex_private_recover.set_defaults(handler=_codex_private_recover)

    doctor = commands.add_parser("doctor", help="report local runtime prerequisites")
    doctor.set_defaults(handler=_doctor)
    return parser


def main(
    argv: Sequence[str] | None = None,
    *,
    process_runtime_authority: Any | None = None,
) -> int:
    try:
        if process_runtime_authority is not None:
            private_runtime.bind_process_private_runtime_authority(
                process_runtime_authority
            )
        arguments = build_parser().parse_args(argv)
        if arguments.command.startswith("codex-private-"):
            with private_runtime.open_pinned_private_runtime_authority() as runtime:
                result = int(arguments.handler(arguments))
                runtime.revalidate()
                return result
        return int(arguments.handler(arguments))
    except (BulkloadError, OSError) as error:
        print(f"bulkload: error: {error}", file=sys.stderr)
        return 2
