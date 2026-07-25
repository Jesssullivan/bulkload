"""Command-line interface for the bulkload v1 protocol."""

from __future__ import annotations

import argparse
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
from .executor import apply_plan, export_copy_paths, verify_plan
from .model import (
    BulkloadError,
    atomic_write_json,
    canonical_bytes,
    durable_makedirs,
    read_json,
)
from .planner import compile_plan
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
    capture_codex_sessions,
    compile_codex_session_union_plan,
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


def _write_pinned_codex_json(
    output: str,
    value: dict[str, Any],
    snapshots: Sequence[dict[str, Any]],
    input_identities: Sequence[tuple[int, ...]],
    *,
    protected_roots: Sequence[Path] = (),
) -> None:
    if output == "-":
        sys.stdout.buffer.write(canonical_bytes(value) + b"\n")
        return
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
            or stat.S_IMODE(parent_info.st_mode) & 0o022
        ):
            raise BulkloadError(
                f"Codex evidence output parent is not private: {parent}"
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
        if target_info is not None and any(
            (target_info.st_dev, target_info.st_ino) == (identity[0], identity[1])
            for identity in input_identities
        ):
            raise BulkloadError("Codex evidence output aliases an input artifact")
        payload = canonical_bytes(value) + b"\n"
        for _ in range(128):
            candidate = f".{requested.name}.bulkload-{secrets.token_hex(8)}"
            try:
                temporary_descriptor = os.open(
                    candidate,
                    os.O_WRONLY
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
        try:
            os.fchmod(temporary_descriptor, 0o600)
            offset = 0
            while offset < len(payload):
                written = os.write(temporary_descriptor, payload[offset:])
                if written < 1:
                    raise BulkloadError("cannot write Codex evidence output")
                offset += written
            os.fsync(temporary_descriptor)
        finally:
            os.close(temporary_descriptor)
        current_parent = os.stat(parent, follow_symlinks=False)
        if (
            current_parent.st_dev,
            current_parent.st_ino,
            current_parent.st_uid,
            stat.S_IMODE(current_parent.st_mode),
        ) != parent_expected:
            raise BulkloadError("Codex evidence output parent changed before rename")
        os.rename(
            temporary_name,
            requested.name,
            src_dir_fd=parent_descriptor,
            dst_dir_fd=parent_descriptor,
        )
        temporary_name = None
        os.fsync(parent_descriptor)
    finally:
        if temporary_name is not None:
            try:
                os.unlink(temporary_name, dir_fd=parent_descriptor)
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
        plan = compile_codex_session_union_plan(
            source_a,
            source_b,
            destination_a,
            destination_b,
        )
        for path, descriptor, expected in pinned:
            _revalidate_pinned_codex_input(path, descriptor, expected)
        _write_pinned_codex_json(
            arguments.output,
            plan,
            snapshots,
            [expected for _, _, expected in pinned],
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
            f"blockers={len(intent['blockers'])}"
        )
    return 0 if intent["ready_for_attended_copy"] else 4


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

    codex_plan = commands.add_parser(
        "codex-plan",
        help="compile a dry-run absent-only Codex session union plan",
    )
    codex_plan.add_argument("--source-a", required=True)
    codex_plan.add_argument("--source-b", required=True)
    codex_plan.add_argument("--destination-a", required=True)
    codex_plan.add_argument("--destination-b", required=True)
    codex_plan.add_argument("--output", required=True)
    codex_plan.set_defaults(handler=_codex_plan)

    doctor = commands.add_parser("doctor", help="report local runtime prerequisites")
    doctor.set_defaults(handler=_doctor)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    try:
        arguments = build_parser().parse_args(argv)
        return int(arguments.handler(arguments))
    except (BulkloadError, OSError) as error:
        print(f"bulkload: error: {error}", file=sys.stderr)
        return 2
