"""Single public CLI for AgentCaptureV4 cutovers."""

from __future__ import annotations

import argparse
import gc
import os
from pathlib import Path
import sqlite3
import sys
from typing import Any, Sequence

from . import __version__
from .executor import (
    DEFAULT_CAPACITY_RESERVE_BYTES,
    apply_agent_plan,
    push_agent_transport,
    recover_agent_apply,
    rollback_agent_apply,
    stage_agent_plan,
    validate_stage_receipt,
    verify_agent_plan,
)
from .model import (
    BulkloadError,
    MAX_JSON_BYTES,
    assert_no_overlap,
    atomic_write,
    canonical_bytes,
    read_json,
)
from .planner import compile_agent_plan_authorities
from .scanner import (
    DEFAULT_MAX_BYTES,
    DEFAULT_MAX_FILES,
    DEFAULT_MAX_SQLITE_ROWS,
    _catalog_path_identities,
    canonical_path_map,
    capture_agent_state,
    validate_agent_capture,
)


MAX_PUBLIC_JSON_BYTES = MAX_JSON_BYTES
LARGE_PLAN_THRESHOLD_BYTES = 512 * 1024**2


def _write(path: str, value: dict[str, Any]) -> None:
    payload = canonical_bytes(value) + b"\n"
    if len(payload) > MAX_PUBLIC_JSON_BYTES:
        raise BulkloadError("public JSON output exceeds the bounded read contract")
    if path == "-":
        sys.stdout.buffer.write(payload)
    else:
        atomic_write(Path(path), payload, mode=0o600)


def _load(path: str) -> dict[str, Any]:
    candidate = Path(path).expanduser()
    size = candidate.stat(follow_symlinks=False).st_size
    _require_large_evidence_memory(size)
    return read_json(candidate)


def _available_linux_memory() -> int:
    try:
        fields = {
            line.split(":", 1)[0]: int(line.split()[1]) * 1024
            for line in Path("/proc/meminfo").read_text(encoding="ascii").splitlines()
            if ":" in line and len(line.split()) >= 2
        }
        return fields["MemAvailable"]
    except (FileNotFoundError, KeyError, OSError, UnicodeDecodeError, ValueError):
        raise BulkloadError(
            "large Bulkload evidence requires the memory-qualified destination"
        ) from None


def _require_large_evidence_memory(size: int, *, multiplier: int = 4) -> None:
    if size <= LARGE_PLAN_THRESHOLD_BYTES:
        return
    required = size * multiplier + 2 * 1024**3
    if _available_linux_memory() < required:
        raise BulkloadError(
            "destination memory is below the bounded Bulkload evidence gate"
        )


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
    snapshot = catalog.get("snapshot")
    if isinstance(snapshot, dict):
        roots.extend(
            (
                Path(snapshot["seal_path"]).parent,
                Path(snapshot["seal_path"]),
                Path(snapshot["index_path"]),
            )
        )
        roots.extend(Path(item["snapshot"]) for item in snapshot["roots"])
        if isinstance(snapshot.get("base"), dict):
            roots.append(Path(snapshot["base"]["seal_path"]).parent)
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
        if arguments.snapshot_base_seal:
            roots.append(Path(arguments.snapshot_base_seal).expanduser().parent)
    elif arguments.command == "agent-plan":
        for name in ("source_a", "source_b", "destination_a", "destination_b"):
            roots.extend(_catalog_roots(_load(getattr(arguments, name))["catalog"]))
    elif arguments.command == "agent-stage" and arguments.transport_mode == "push":
        if not arguments.prepare_receipt or not arguments.transport_allowlist:
            raise BulkloadError(
                "transport push requires a prepare receipt and sealed allowlist"
            )
        prepare = _load(arguments.prepare_receipt)
        validate_stage_receipt(prepare)
        roots.extend(Path(value) for value in prepare["transport"]["source_roots"])
        snapshot = prepare["transport"].get("source_snapshot")
        if isinstance(snapshot, dict):
            roots.extend((Path(snapshot["seal_path"]), Path(snapshot["index_path"])))
        roots.extend(
            (Path(arguments.prepare_receipt), Path(arguments.transport_allowlist))
        )
    elif hasattr(arguments, "plan") and arguments.plan:
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
    if not arguments.acknowledge_writers_quiesced and arguments.output == "-":
        raise BulkloadError("live capture requires an owner-private evidence path")
    snapshot_root = None
    if not arguments.acknowledge_writers_quiesced:
        output = Path(arguments.output).expanduser()
        snapshot_root = output.parent / f".{output.name}.snapshot"
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
        snapshot_root=snapshot_root,
        snapshot_base_seal=Path(arguments.snapshot_base_seal).expanduser()
        if arguments.snapshot_base_seal
        else None,
        managed_exclusions=arguments.managed_exclusion,
        rsync_path=Path(arguments.rsync_path),
        max_files=arguments.max_files,
        max_bytes=arguments.max_bytes,
        max_sqlite_rows=arguments.max_sqlite_rows,
        jobs=arguments.jobs,
    )


def _agent_plan(arguments: argparse.Namespace) -> dict[str, Any]:
    paths = [
        Path(getattr(arguments, name)).expanduser()
        for name in ("source_a", "source_b", "destination_a", "destination_b")
    ]
    sizes = [path.stat(follow_symlinks=False).st_size for path in paths]
    if any(size > MAX_JSON_BYTES for size in sizes):
        raise BulkloadError("AgentCaptureV4 exceeds the bounded JSON contract")
    total = sum(sizes)
    if total > LARGE_PLAN_THRESHOLD_BYTES:
        required = total * 4 + 2 * 1024**3
        if _available_linux_memory() < required:
            raise BulkloadError(
                "destination memory is below the bounded AgentPlanV4 planning gate"
            )

    def stable_authority(
        first_path: Path, second_path: Path, role: str
    ) -> tuple[dict[str, Any], tuple[str, str]]:
        first = _load(os.fspath(first_path))
        validate_agent_capture(first, expected_role=role)
        first_id = first["capture_id"]
        catalog_sha256 = first["catalog_sha256"]
        first_complete = first["complete"]
        first_quiesced = first["writers_quiesced"]
        first_contract = (
            None if first_quiesced else first["catalog"]["snapshot"]["contract_sha256"]
        )
        first_seal = (
            None if first_quiesced else first["catalog"]["snapshot"]["seal_sha256"]
        )
        first_snapshot = None if first_quiesced else first["catalog"]["snapshot"]
        first_identities = (
            None if first_quiesced else _catalog_path_identities(first["catalog"])
        )
        del first
        gc.collect()
        second = _load(os.fspath(second_path))
        validate_agent_capture(second, expected_role=role)
        if (
            first_id == second["capture_id"]
            or not first_complete
            or not second["complete"]
            or first_quiesced != second["writers_quiesced"]
            or (first_quiesced and catalog_sha256 != second["catalog_sha256"])
            or (
                not first_quiesced
                and (
                    first_contract != second["catalog"]["snapshot"]["contract_sha256"]
                    or first_seal == second["catalog"]["snapshot"]["seal_sha256"]
                    or second["catalog"]["snapshot"]["base"]
                    != {
                        "seal_path": first_snapshot["seal_path"],
                        "seal_sha256": first_snapshot["seal_sha256"],
                        "snapshot_id": first_snapshot["snapshot_id"],
                    }
                    or not first_identities.issubset(
                        _catalog_path_identities(second["catalog"])
                    )
                )
            )
        ):
            raise BulkloadError(f"{role} A/B captures are not stable and complete")
        return second, (first_id, second["capture_id"])

    source, source_ids = stable_authority(paths[0], paths[1], "source")
    destination, destination_ids = stable_authority(paths[2], paths[3], "destination")
    return compile_agent_plan_authorities(
        source,
        source_ids,
        destination,
        destination_ids,
    )


def _agent_stage(arguments: argparse.Namespace) -> dict[str, Any]:
    if arguments.transport_mode == "push":
        if (
            arguments.plan is not None
            or arguments.prepare_receipt is None
            or arguments.transport_allowlist is None
            or arguments.destination_ssh_host is None
            or arguments.transport_receipt is not None
        ):
            raise BulkloadError(
                "transport push requires only prepare receipt, allowlist, and destination host"
            )
        return push_agent_transport(
            _load(arguments.prepare_receipt),
            Path(arguments.transport_allowlist),
            accepted_plan_sha256=arguments.accept_plan_sha256,
            phase=arguments.phase,
            stage_root=Path(arguments.stage_root),
            destination_ssh_host=arguments.destination_ssh_host,
            transport_checksum=arguments.transport_checksum,
        )
    if arguments.transport_checksum:
        raise BulkloadError("transport checksum applies only to the push transport")
    if arguments.plan is None or arguments.transport_allowlist is not None:
        raise BulkloadError(
            "plan staging requires a plan and forbids a transport allowlist"
        )
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
        destination_verify_receipt=_load(arguments.destination_verify_receipt)
        if arguments.destination_verify_receipt
        else None,
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
    capture.add_argument("--snapshot-base-seal")
    capture.add_argument("--max-files", type=int, default=DEFAULT_MAX_FILES)
    capture.add_argument("--max-bytes", type=int, default=DEFAULT_MAX_BYTES)
    capture.add_argument("--max-sqlite-rows", type=int, default=DEFAULT_MAX_SQLITE_ROWS)
    capture.add_argument(
        "--jobs",
        type=int,
        help=(
            "Git-workspace capture workers. Omit to keep the shipped default "
            "of 3; raise it only on a host with the memory headroom for that "
            "many concurrent `git fsck --full` runs."
        ),
    )
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
    stage.add_argument("--plan")
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
    stage.add_argument("--transport-allowlist")
    stage.add_argument("--transport-receipt")
    stage.add_argument(
        "--transport-checksum",
        action="store_true",
        help=(
            "push with rsync --checksum. The destination re-derives every "
            "transported digest regardless; this only makes the transport "
            "itself fail earlier, at the cost of a second full read."
        ),
    )
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
    verify.add_argument("--destination-verify-receipt")
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
