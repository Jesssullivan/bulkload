"""Single public CLI for AgentCaptureV4 cutovers."""

from __future__ import annotations

import argparse
from pathlib import Path
import sqlite3
import sys
from typing import Any, Sequence

from . import __version__
from .executor import (
    DEFAULT_CAPACITY_RESERVE_BYTES,
    apply_agent_plan,
    recover_agent_apply,
    rollback_agent_apply,
    stage_agent_plan,
    verify_agent_plan,
)
from .model import (
    BulkloadError,
    assert_no_overlap,
    atomic_write_json,
    canonical_bytes,
    read_json,
)
from .planner import compile_agent_plan
from .scanner import (
    DEFAULT_MAX_BYTES,
    DEFAULT_MAX_FILES,
    DEFAULT_MAX_SQLITE_ROWS,
    canonical_path_map,
    capture_agent_state,
)


def _write(path: str, value: dict[str, Any]) -> None:
    if path == "-":
        sys.stdout.buffer.write(canonical_bytes(value) + b"\n")
    else:
        atomic_write_json(Path(path), value)


def _load(path: str) -> dict[str, Any]:
    return read_json(Path(path).expanduser())


def _catalog_roots(catalog: dict[str, Any]) -> list[Path]:
    roots = [
        Path(catalog["root_bindings"]["home"]),
        Path(catalog["root_bindings"]["git_root"]),
    ]
    roots.extend(
        Path(provider["path"])
        for provider in catalog.get("providers", [])
        if provider.get("exists")
    )
    roots.extend(
        Path(seat["path"]) for seat in catalog.get("seats", []) if seat.get("exists")
    )
    return roots


def _protect_output(arguments: argparse.Namespace) -> None:
    if arguments.output == "-":
        return
    roots: list[Path] = []
    if arguments.command == "agent-capture":
        roots.extend((Path(arguments.home), Path(arguments.git_root)))
        roots.extend(
            Path(path)
            for path in (arguments.codex_root, arguments.claude_root, arguments.pi_root)
            if path
        )
        roots.extend(path for _, path, _ in [*arguments.seat, *arguments.file_seat])
    elif arguments.command == "agent-plan":
        for name in ("source_a", "source_b", "destination_a", "destination_b"):
            roots.extend(_catalog_roots(_load(getattr(arguments, name))["catalog"]))
    elif hasattr(arguments, "plan"):
        plan = _load(arguments.plan)
        roots.extend(_catalog_roots(plan["source"]["catalog"]))
        roots.extend(_catalog_roots(plan["destination"]["catalog"]))
    elif arguments.command == "agent-rollback":
        apply_receipt = _load(arguments.apply_receipt)
        journal = _load(apply_receipt["journal_path"])
        roots.extend(Path(item["target"]) for item in journal.get("mutations", []))
        roots.extend(Path(path) for path in journal.get("created_git_roots", []))
        roots.extend(
            Path(item["path"]) for item in journal.get("created_worktrees", [])
        )
    if hasattr(arguments, "stage_root"):
        roots.append(Path(arguments.stage_root))
    assert_no_overlap(Path(arguments.output), roots, "evidence output")


def _parse_mapping(value: str) -> tuple[str, str]:
    if "=" not in value:
        raise argparse.ArgumentTypeError("path map must be SOURCE=DESTINATION")
    source, destination = value.split("=", 1)
    if not source or not destination:
        raise argparse.ArgumentTypeError("path map must have two non-empty paths")
    return source, destination


def _parse_seat(value: str) -> tuple[str, Path, str]:
    if "=" not in value:
        raise argparse.ArgumentTypeError("mutable seat must be NAME=PATH")
    name, path = value.split("=", 1)
    if not name or not path:
        raise argparse.ArgumentTypeError("mutable seat must have a name and path")
    return name, Path(path).expanduser(), "directory"


def _parse_file_seat(value: str) -> tuple[str, Path, str]:
    name, path, _ = _parse_seat(value)
    return name, path, "file"


def _parse_managed_exclusion(value: str) -> tuple[str, str]:
    if ":" not in value:
        raise argparse.ArgumentTypeError(
            "managed exclusion must be PROVIDER:RELATIVE_PATH"
        )
    provider, relative = value.split(":", 1)
    if not provider or not relative:
        raise argparse.ArgumentTypeError("managed exclusion fields must be non-empty")
    return provider, relative


def _agent_capture(arguments: argparse.Namespace) -> dict[str, Any]:
    home = Path(arguments.home).expanduser()
    return capture_agent_state(
        role=arguments.role,
        home=home,
        git_root=Path(arguments.git_root).expanduser(),
        codex_root=Path(arguments.codex_root).expanduser()
        if arguments.codex_root
        else None,
        claude_root=Path(arguments.claude_root).expanduser()
        if arguments.claude_root
        else None,
        pi_root=Path(arguments.pi_root).expanduser() if arguments.pi_root else None,
        seats=[*arguments.seat, *arguments.file_seat],
        path_map=canonical_path_map(arguments.path_map),
        writers_quiesced=arguments.acknowledge_writers_quiesced,
        managed_exclusions=arguments.managed_exclusion,
        rsync_path=Path(arguments.rsync_path),
        max_files=arguments.max_files,
        max_bytes=arguments.max_bytes,
        max_sqlite_rows=arguments.max_sqlite_rows,
    )


def _agent_plan(arguments: argparse.Namespace) -> dict[str, Any]:
    return compile_agent_plan(
        _load(arguments.source_a),
        _load(arguments.source_b),
        _load(arguments.destination_a),
        _load(arguments.destination_b),
    )


def _agent_stage(arguments: argparse.Namespace) -> dict[str, Any]:
    return stage_agent_plan(
        _load(arguments.plan),
        accepted_plan_sha256=arguments.accept_plan_sha256,
        phase=arguments.phase,
        stage_root=Path(arguments.stage_root),
        allow_accounted_copy=arguments.allow_accounted_copy,
        reserve_bytes=arguments.capacity_reserve_bytes,
        transport_mode=arguments.transport_mode,
        destination_ssh_host=arguments.destination_ssh_host,
        prepare_receipt=_load(arguments.prepare_receipt)
        if arguments.prepare_receipt
        else None,
        transport_receipt=_load(arguments.transport_receipt)
        if arguments.transport_receipt
        else None,
    )


def _agent_apply(arguments: argparse.Namespace) -> dict[str, Any]:
    return apply_agent_plan(
        _load(arguments.plan),
        _load(arguments.stage_receipt),
        accepted_plan_sha256=arguments.accept_plan_sha256,
        journal_path=Path(arguments.journal),
        rollback_root=Path(arguments.rollback_root),
        reserve_bytes=arguments.capacity_reserve_bytes,
    )


def _agent_verify(arguments: argparse.Namespace) -> dict[str, Any]:
    return verify_agent_plan(
        _load(arguments.plan),
        _load(arguments.stage_receipt),
        _load(arguments.apply_receipt),
    )


def _agent_rollback(arguments: argparse.Namespace) -> dict[str, Any]:
    return rollback_agent_apply(
        _load(arguments.apply_receipt),
        accepted_receipt_sha256=arguments.accept_receipt_sha256,
    )


def _agent_recover(arguments: argparse.Namespace) -> dict[str, Any]:
    return recover_agent_apply(
        _load(arguments.plan),
        _load(arguments.stage_receipt),
        journal_path=Path(arguments.journal),
        strategy=arguments.strategy,
    )


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="bulkload",
        description="Typed, reviewed Git and agent-state cutover",
    )
    parser.add_argument(
        "--version", action="version", version=f"%(prog)s {__version__}"
    )
    commands = parser.add_subparsers(dest="command", required=True)

    capture = commands.add_parser("agent-capture", help="write AgentCaptureV4 evidence")
    capture.add_argument("--role", choices=("source", "destination"), required=True)
    capture.add_argument("--home", required=True)
    capture.add_argument("--git-root", required=True)
    capture.add_argument("--codex-root")
    capture.add_argument("--claude-root")
    capture.add_argument("--pi-root")
    capture.add_argument("--seat", action="append", type=_parse_seat, default=[])
    capture.add_argument(
        "--file-seat", action="append", type=_parse_file_seat, default=[]
    )
    capture.add_argument(
        "--managed-exclusion",
        action="append",
        type=_parse_managed_exclusion,
        default=[],
    )
    capture.add_argument("--rsync-path", required=True)
    capture.add_argument(
        "--path-map", action="append", type=_parse_mapping, required=True
    )
    capture.add_argument("--acknowledge-writers-quiesced", action="store_true")
    capture.add_argument("--max-files", type=int, default=DEFAULT_MAX_FILES)
    capture.add_argument("--max-bytes", type=int, default=DEFAULT_MAX_BYTES)
    capture.add_argument("--max-sqlite-rows", type=int, default=DEFAULT_MAX_SQLITE_ROWS)
    capture.add_argument("--output", required=True)
    capture.set_defaults(handler=_agent_capture)

    plan = commands.add_parser("agent-plan", help="compile an exact four-capture union")
    plan.add_argument("--source-a", required=True)
    plan.add_argument("--source-b", required=True)
    plan.add_argument("--destination-a", required=True)
    plan.add_argument("--destination-b", required=True)
    plan.add_argument("--output", required=True)
    plan.set_defaults(handler=_agent_plan)

    stage = commands.add_parser(
        "agent-stage", help="materialize preseed or final stage"
    )
    stage.add_argument("--phase", choices=("preseed", "final"), required=True)
    stage.add_argument("--plan", required=True)
    stage.add_argument("--accept-plan-sha256", required=True)
    stage.add_argument("--stage-root", required=True)
    stage.add_argument("--allow-accounted-copy", action="store_true")
    stage.add_argument(
        "--transport-mode",
        choices=("local", "prepare", "push", "materialize"),
        default="local",
    )
    stage.add_argument("--destination-ssh-host")
    stage.add_argument("--prepare-receipt")
    stage.add_argument("--transport-receipt")
    stage.add_argument(
        "--capacity-reserve-bytes", type=int, default=DEFAULT_CAPACITY_RESERVE_BYTES
    )
    stage.add_argument("--output", required=True)
    stage.set_defaults(handler=_agent_stage)

    apply = commands.add_parser("agent-apply", help="apply a sealed final stage")
    apply.add_argument("--plan", required=True)
    apply.add_argument("--stage-receipt", required=True)
    apply.add_argument("--accept-plan-sha256", required=True)
    apply.add_argument("--journal", required=True)
    apply.add_argument("--rollback-root", required=True)
    apply.add_argument(
        "--capacity-reserve-bytes", type=int, default=DEFAULT_CAPACITY_RESERVE_BYTES
    )
    apply.add_argument("--output", required=True)
    apply.set_defaults(handler=_agent_apply)

    verify = commands.add_parser(
        "agent-verify", help="independently verify final state"
    )
    verify.add_argument("--plan", required=True)
    verify.add_argument("--stage-receipt", required=True)
    verify.add_argument("--apply-receipt", required=True)
    verify.add_argument("--output", required=True)
    verify.set_defaults(handler=_agent_verify)

    rollback = commands.add_parser(
        "agent-rollback", help="restore exact overwritten state"
    )
    rollback.add_argument("--apply-receipt", required=True)
    rollback.add_argument("--accept-receipt-sha256", required=True)
    rollback.add_argument("--output", required=True)
    rollback.set_defaults(handler=_agent_rollback)

    recover = commands.add_parser(
        "agent-recover", help="resume or roll back an interrupted apply"
    )
    recover.add_argument("--plan", required=True)
    recover.add_argument("--stage-receipt", required=True)
    recover.add_argument("--journal", required=True)
    recover.add_argument("--strategy", choices=("forward", "rollback"), required=True)
    recover.add_argument("--output", required=True)
    recover.set_defaults(handler=_agent_recover)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    parser = build_parser()
    arguments = parser.parse_args(argv)
    try:
        _protect_output(arguments)
        result = arguments.handler(arguments)
        _write(arguments.output, result)
    except (BulkloadError, OSError, sqlite3.Error) as error:
        print(f"bulkload: FAIL: {error}", file=sys.stderr)
        return 1
    return 0
