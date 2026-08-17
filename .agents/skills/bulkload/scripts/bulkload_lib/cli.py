"""Command-line interface for the bulkload v1 protocol."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
from typing import Any, Sequence

from . import __version__
from .executor import apply_plan, export_copy_paths, verify_plan
from .model import BulkloadError, atomic_write_json, canonical_bytes, read_json
from .planner import compile_plan
from .scanner import DEFAULT_MAX_BYTES, DEFAULT_MAX_FILES, capture_snapshot


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
