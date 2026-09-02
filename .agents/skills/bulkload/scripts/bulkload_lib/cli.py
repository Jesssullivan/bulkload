"""Single public CLI for AgentCaptureV4 cutovers."""

from __future__ import annotations

import argparse
import contextlib
import gc
import os
from pathlib import Path
import socket
import sqlite3
import sys
import time
from typing import Any, Iterable, Iterator, Sequence

from . import __version__
from .executor import (
    DEFAULT_CAPACITY_RESERVE_BYTES,
    TRANSPORT_MOVERS,
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
    UNCLASSIFIED_REFUSAL_CODE,
    assert_no_overlap,
    atomic_write,
    canonical_bytes,
    path_identity,
    read_json,
    refusal_record,
)
from .mover import DEFAULT_STREAMS
from .planner import compile_agent_plan_authorities
from .scanner import (
    DEFAULT_HEARTBEAT_SECONDS,
    DEFAULT_MAX_BYTES,
    DEFAULT_MAX_FILES,
    DEFAULT_MAX_SQLITE_ROWS,
    _catalog_path_identities,
    canonical_path_map,
    capture_agent_state,
    configure_progress,
    emit_progress,
    unaccounted_seconds,
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


def _proc_meminfo_available() -> int | None:
    try:
        fields = {
            line.split(":", 1)[0]: int(line.split()[1]) * 1024
            for line in Path("/proc/meminfo").read_text(encoding="ascii").splitlines()
            if ":" in line and len(line.split()) >= 2
        }
        return fields["MemAvailable"]
    except (FileNotFoundError, KeyError, OSError, UnicodeDecodeError, ValueError):
        return None


def _sysconf_available() -> int | None:
    try:
        page = os.sysconf("SC_PAGE_SIZE")
        available = os.sysconf("SC_AVPHYS_PAGES")
    except (AttributeError, OSError, ValueError):
        return None
    if not isinstance(page, int) or not isinstance(available, int):
        return None
    if page <= 0 or available < 0:
        return None
    return page * available


def _available_memory() -> int | None:
    """Available physical memory, or None where it cannot be measured.

    Only a genuine *availability* reading may close this gate. Darwin has no
    /proc/meminfo and no SC_AVPHYS_PAGES, so the previous Linux-only probe
    raised unconditionally there and turned any evidence over 512 MiB into a
    hard abort on the release host. Total physical memory is deliberately not
    substituted: it is not an availability measure, and guessing with it would
    either abort a host that had the headroom or pass one that did not.
    """
    for probe in (_proc_meminfo_available, _sysconf_available):
        observed = probe()
        if observed is not None:
            return observed
    return None


def _require_large_evidence_memory(
    size: int,
    *,
    multiplier: int = 4,
    message: str = "destination memory is below the bounded Bulkload evidence gate",
) -> None:
    if size <= LARGE_PLAN_THRESHOLD_BYTES:
        return
    available = _available_memory()
    if available is None:
        return
    if available < size * multiplier + 2 * 1024**3:
        raise BulkloadError(message)


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


def _evidence_protected_roots(arguments: argparse.Namespace) -> list[Path]:
    """Every live root this verb reads, which no output of ours may enter.

    Reads the verb's evidence to get there, which for `agent-plan` means four
    multi-gigabyte captures, so callers compute it once and share the result.
    """
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
    return roots


@contextlib.contextmanager
def _disarmed_on_refusal(arguments: argparse.Namespace) -> Iterator[None]:
    """Drop the failure path before re-raising a refusal that is about it.

    `main` writes the refusal record inside the handler for the very refusal
    these guards raise. Without this, a guard that refuses a failure path
    performs, one frame later, exactly the write it just refused.
    """
    try:
        yield
    except BaseException:
        arguments.failure_output = None
        raise


def _protect_output(
    arguments: argparse.Namespace, protected: Sequence[Path] | None = None
) -> None:
    """Keep both output channels out of every live root the verb reads.

    `--failure-output` is a write channel like `--output`, and `atomic_write`
    creates the whole parent tree, so an unguarded refusal path plants
    directories and a file inside a root a capture is walking — the exact
    shape that refuses the capture with "live snapshot path set changed".

    `protected` is the shared root set when the caller already had to derive
    it for `--progress-log`; for `agent-plan` that derivation costs four
    multi-gigabyte reads and must not happen twice. Left at None it is
    derived here, and only once both output channels are known to need it,
    which keeps the cost off a verb that writes evidence to stdout alone.
    """
    failure_output = getattr(arguments, "failure_output", None)
    if arguments.output == "-" and not failure_output:
        return
    roots = _evidence_protected_roots(arguments) if protected is None else protected
    if arguments.output != "-":
        assert_no_overlap(Path(arguments.output), roots, "evidence output")
    if failure_output:
        with _disarmed_on_refusal(arguments):
            assert_no_overlap(
                Path(failure_output), roots, "failure output", fold_case=True
            )


# Every argument that names a file the verb reads or seals. A refusal record
# is small and always writable, so pointing it at one of these silently
# replaces a sealed input with a 400-byte JSON object.
ARTIFACT_ARGUMENTS = (
    "source_a",
    "source_b",
    "destination_a",
    "destination_b",
    "plan",
    "stage_receipt",
    "prepare_receipt",
    "transport_allowlist",
    "transport_receipt",
    "apply_receipt",
    "destination_verify_receipt",
    "journal",
    "snapshot_base_seal",
)


def _protect_failure_output(arguments: argparse.Namespace) -> None:
    """Refuse a failure path that could damage the evidence it explains."""
    path = getattr(arguments, "failure_output", None)
    if not path:
        return
    with _disarmed_on_refusal(arguments):
        if path == "-":
            raise BulkloadError(
                "failure output must be a path; stdout carries evidence"
            )
        identity = path_identity(path, fold_case=True)
        if arguments.output != "-" and identity == path_identity(
            arguments.output, fold_case=True
        ):
            raise BulkloadError("failure output must differ from the evidence output")
        for name in ARTIFACT_ARGUMENTS:
            value = getattr(arguments, name, None)
            if value and identity == path_identity(value, fold_case=True):
                flag = name.replace("_", "-")
                raise BulkloadError(
                    f"failure output must differ from the --{flag} artifact: {value}"
                )


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
        base_custody=arguments.base_custody,
    )


def _agent_plan(arguments: argparse.Namespace) -> dict[str, Any]:
    paths = [
        Path(getattr(arguments, name)).expanduser()
        for name in ("source_a", "source_b", "destination_a", "destination_b")
    ]
    sizes = [path.stat(follow_symlinks=False).st_size for path in paths]
    if any(size > MAX_JSON_BYTES for size in sizes):
        raise BulkloadError("AgentCaptureV4 exceeds the bounded JSON contract")
    _require_large_evidence_memory(
        sum(sizes),
        message="destination memory is below the bounded AgentPlanV4 planning gate",
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
            transport_mover=arguments.mover,
            transport_streams=arguments.transport_streams,
        )
    if arguments.transport_checksum:
        raise BulkloadError("transport checksum applies only to the push transport")
    if arguments.mover != "rsync":
        raise BulkloadError("transport mover applies only to the push transport")
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


def _telemetry_parser() -> argparse.ArgumentParser:
    """The progress flags every verb carries.

    Telemetry is on by default: a verb that runs for hours must never leave a
    zero-byte log behind. `--quiet` is the opt-out, and `--progress-log`
    duplicates the same lines into a file an unattended agent can tail.
    """
    parent = argparse.ArgumentParser(add_help=False)
    parent.add_argument(
        "--quiet",
        action="store_true",
        help="suppress phase and heartbeat telemetry on stderr",
    )
    parent.add_argument(
        "--progress-log",
        help=(
            "append phase and heartbeat telemetry to this file as well as "
            "stderr. It must not live under any live, stage, or snapshot root."
        ),
    )
    parent.add_argument(
        "--heartbeat-seconds",
        type=float,
        default=DEFAULT_HEARTBEAT_SECONDS,
        help=(
            "seconds between in-phase heartbeat lines "
            f"(default {DEFAULT_HEARTBEAT_SECONDS:g})"
        ),
    )
    return parent


def _cheap_protected_roots(arguments: argparse.Namespace) -> list[Path]:
    """Roots a progress log must not land in, read straight off the flags.

    Deliberately does not open any evidence: this runs before the first
    telemetry line, and `agent-plan`'s four captures are gigabytes each.
    """
    roots: list[Path] = []
    for name in ("home", "git_root", "codex_root", "claude_root", "pi_root"):
        value = getattr(arguments, name, None)
        if value:
            roots.append(Path(value))
    for declaration in [
        *getattr(arguments, "seat", []),
        *getattr(arguments, "file_seat", []),
    ]:
        roots.append(declaration[1])
    for name in ("stage_root", "rollback_root"):
        value = getattr(arguments, name, None)
        if value:
            roots.append(Path(value))
    seal = getattr(arguments, "snapshot_base_seal", None)
    if seal:
        roots.append(Path(seal).expanduser().parent)
    return roots


# One telemetry line is ~120 bytes and only the run banner is written before
# activation, so this bound is three orders of magnitude of headroom against a
# sink that never opens.
MAX_BUFFERED_PROGRESS_LINES = 1024


class _DeferredProgressLog:
    """A `--progress-log` that does not exist until it is proven safe.

    The advertised refusal — "it must not live under any live, stage, or
    snapshot root" — held for `agent-capture` alone, because that is the one
    verb whose roots are all readable straight off the flags. `agent-apply`'s
    destination roots live inside the plan, and the file was created `O_CREAT`
    before the plan was read, so `--progress-log <path under the destination
    home>` planted a file in the destination live tree that the apply journal
    does not record and `agent-rollback` therefore does not remove.

    Opening the file earlier cannot fix that; opening it later can. Lines are
    buffered from the first banner until `activate` has cleared the candidate
    against the complete per-verb root set, and only then is the file created.
    A verb that refuses before activation leaves nothing behind at all — the
    refusal is still printed on stderr, which is the sink that is always
    there.
    """

    def __init__(self, path: Path) -> None:
        self.path = path
        self._buffer: list[str] = []
        self._stream: Any = None

    def activate(self, protected: Iterable[Path]) -> None:
        assert_no_overlap(self.path, protected, "progress log")
        descriptor = os.open(
            self.path,
            os.O_WRONLY | os.O_CREAT | os.O_APPEND | getattr(os, "O_CLOEXEC", 0),
            0o600,
        )
        stream = os.fdopen(descriptor, "a", buffering=1, encoding="utf-8", closefd=True)
        for line in self._buffer:
            stream.write(line)
        self._buffer.clear()
        stream.flush()
        self._stream = stream

    def write(self, text: str) -> None:
        if self._stream is not None:
            self._stream.write(text)
        elif len(self._buffer) < MAX_BUFFERED_PROGRESS_LINES:
            self._buffer.append(text)

    def flush(self) -> None:
        if self._stream is not None:
            self._stream.flush()

    def close(self) -> None:
        stream, self._stream = self._stream, None
        if stream is not None:
            stream.close()


def _open_progress_log(arguments: argparse.Namespace) -> _DeferredProgressLog | None:
    """Bind the progress log and refuse everything the flags alone can refuse.

    This is the cheap half of the guard: it runs before the first telemetry
    line, so it may not open evidence. The complete half is
    `_DeferredProgressLog.activate`.
    """
    path = getattr(arguments, "progress_log", None)
    if not path:
        return None
    candidate = Path(path).expanduser()
    output = getattr(arguments, "output", None)
    if output and output != "-" and Path(output).expanduser() == candidate:
        raise BulkloadError("progress log must not be the evidence output path")
    assert_no_overlap(candidate, _cheap_protected_roots(arguments), "progress log")
    return _DeferredProgressLog(candidate)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="bulkload",
        description="Typed, reviewed Git and agent-state cutover",
    )
    parser.add_argument(
        "--version", action="version", version=f"%(prog)s {__version__}"
    )
    # Every verb accepts --failure-output. It is declared on a parent parser
    # so the flag reads naturally after the verb and still lands on one
    # namespace attribute that main() can consult without knowing the verb.
    refusal = argparse.ArgumentParser(add_help=False)
    refusal.add_argument(
        "--failure-output",
        metavar="PATH",
        help=(
            "write a structured refusal record (JSON) here when the verb "
            "refuses; the stderr line comes first either way. Must not be "
            "--output, an input artifact, or inside any live root"
        ),
    )
    commands = parser.add_subparsers(dest="command", required=True)
    telemetry = _telemetry_parser()

    capture = commands.add_parser(
        "agent-capture",
        parents=[refusal, telemetry],
        help="write AgentCaptureV4 evidence",
    )
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
    capture.add_argument(
        "--base-custody",
        choices=("full", "sealed"),
        default="full",
        help=(
            "how the named base seal's payloads are admitted. full re-hashes "
            "every byte of the base (today's behaviour). sealed trusts the "
            "base seal's verified index digest and namespace digest instead, "
            "except the git root and every SQLite payload, which are always "
            "re-derived."
        ),
    )
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

    plan = commands.add_parser(
        "agent-plan",
        parents=[refusal, telemetry],
        help="compile an exact four-capture union",
    )
    plan.add_argument("--source-a", required=True)
    plan.add_argument("--source-b", required=True)
    plan.add_argument("--destination-a", required=True)
    plan.add_argument("--destination-b", required=True)
    plan.add_argument("--output", required=True)
    plan.set_defaults(handler=_agent_plan)

    stage = commands.add_parser(
        "agent-stage",
        parents=[refusal, telemetry],
        help="materialize preseed or final stage",
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
        "--mover",
        choices=TRANSPORT_MOVERS,
        default="rsync",
        help=(
            "Payload mover for --transport-mode push. 'rsync' is the shipped "
            "single-stream path. 'native' is the content-addressed parallel "
            "mover: one transfer per sha256 rather than per path, resumable "
            "by re-offering every object, and unproven against a live "
            "destination. See docs/design/native-mover.md."
        ),
    )
    stage.add_argument(
        "--transport-streams",
        type=int,
        default=DEFAULT_STREAMS,
        help=(
            "Parallel streams for --mover native (default: %(default)s). "
            "Ignored by the rsync mover, which has exactly one."
        ),
    )
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

    apply = commands.add_parser(
        "agent-apply",
        parents=[refusal, telemetry],
        help="apply a sealed final stage",
    )
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
        "agent-verify",
        parents=[refusal, telemetry],
        help="independently verify final state",
    )
    verify.add_argument("--plan", required=True)
    verify.add_argument("--stage-receipt", required=True)
    verify.add_argument("--apply-receipt", required=True)
    verify.add_argument("--destination-verify-receipt")
    verify.add_argument("--output", required=True)
    verify.set_defaults(handler=_agent_verify)

    rollback = commands.add_parser(
        "agent-rollback",
        parents=[refusal, telemetry],
        help="restore exact overwritten state",
    )
    rollback.add_argument("--apply-receipt", required=True)
    rollback.add_argument("--accept-receipt-sha256", required=True)
    rollback.add_argument("--output", required=True)
    rollback.set_defaults(handler=_agent_rollback)

    recover = commands.add_parser(
        "agent-recover",
        parents=[refusal, telemetry],
        help="resume or roll back an interrupted apply",
    )
    recover.add_argument("--plan", required=True)
    recover.add_argument("--stage-receipt", required=True)
    recover.add_argument("--journal", required=True)
    recover.add_argument("--strategy", choices=("forward", "rollback"), required=True)
    recover.add_argument("--output", required=True)
    recover.set_defaults(handler=_agent_recover)
    return parser


def _failure_record(
    arguments: argparse.Namespace, error: BaseException
) -> dict[str, Any]:
    """The structured record for one refusal, converted or not.

    A refusal site that has not been converted yet still produces a record —
    with code UNCLASSIFIED and the raised sentence as its message — so a
    caller reading `--failure-output` can tell "this refusal has no diagnosis
    yet" apart from "no refusal happened".

    The channel covers what `main` catches: `BulkloadError`, `OSError` and
    `sqlite3.Error`. A defect in the engine itself still escapes as a
    traceback with no record and no `bulkload: FAIL:` line — widening that
    catch is T3's `BaseException` classify, not this change. Until then a
    caller must still treat "no record" as possible and read the exit status.
    """
    record = getattr(error, "refusal", None)
    if not isinstance(record, dict):
        record = refusal_record(
            code=UNCLASSIFIED_REFUSAL_CODE,
            phase=getattr(arguments, "command", None) or "unknown",
            remedy=(
                "This refusal site does not carry a structured diagnosis yet; "
                "read the message and the verb's inputs."
            ),
            message=str(error),
            observed=type(error).__name__,
        )
    return {
        **record,
        "command": getattr(arguments, "command", None),
        "version": __version__,
    }


def _emit_failure(arguments: argparse.Namespace, error: BaseException) -> None:
    """Write the refusal record, and never let that write mask the refusal.

    Building the record and writing it both happen inside the guard: a bad
    value in the record is as capable of raising as a bad path is, and either
    one must cost a WARN line, not the diagnosis. The catch is `Exception`,
    not `(BulkloadError, OSError)`, because `Path.expanduser()` alone raises
    `RuntimeError` for an unresolvable `~user`.
    """
    path = getattr(arguments, "failure_output", None)
    if not path:
        return
    try:
        record = _failure_record(arguments, error)
        atomic_write(Path(path), canonical_bytes(record) + b"\n", mode=0o600)
    except Exception as failure:  # noqa: BLE001 - the refusal outranks its record
        print(
            f"bulkload: WARN: cannot write failure record: {failure}", file=sys.stderr
        )


def main(argv: Sequence[str] | None = None) -> int:
    parser = build_parser()
    arguments = parser.parse_args(argv)
    started = time.monotonic()
    log = None
    status = "fail"
    code: int | None = None
    try:
        try:
            # The flag-only half of the refusal-path guard runs before
            # anything else, because every later handler writes the refusal
            # record and none of them may write it to a path this rejects.
            _protect_failure_output(arguments)
            log = _open_progress_log(arguments)
            configure_progress(
                stderr=not arguments.quiet,
                interval=arguments.heartbeat_seconds,
                log_stream=log,
            )
        except (BulkloadError, OSError) as error:
            print(f"bulkload: FAIL: {error}", file=sys.stderr)
            _emit_failure(arguments, error)
            code = 1
            return 1
        # Every verb announces itself. This line, and the closing one below,
        # are why no invocation can leave a zero-byte log behind again.
        emit_progress(
            "bulkload-run"
            " event=start"
            f" verb={arguments.command}"
            f" version={__version__}"
            f" pid={os.getpid()}"
            f" host={socket.gethostname()}"
        )
        try:
            # One derivation, two guards. The progress log and the evidence
            # output are both writes this verb makes outside its own
            # contract, and both must clear the same roots. Deriving it here
            # also means the progress log clears the roots that only the
            # evidence names — the destination home in an apply plan, the
            # source roots in a stage prepare receipt — which the flag-only
            # check cannot see. With no log there is nothing to share, so
            # `_protect_output` derives it itself, and only if it needs it.
            protected: list[Path] | None = None
            if log is not None:
                protected = _evidence_protected_roots(arguments)
                log.activate([*protected, *_cheap_protected_roots(arguments)])
            _protect_output(arguments, protected)
            result = arguments.handler(arguments)
            _write(arguments.output, result)
        except (BulkloadError, OSError, sqlite3.Error) as error:
            # The refusal is the deliverable; the record is a convenience.
            # Print first so no failure of the record write can cost the
            # operator the one line that names what refused.
            print(f"bulkload: FAIL: {error}", file=sys.stderr)
            _emit_failure(arguments, error)
            code = 1
            return 1
        status = "ok"
        code = 0
        return 0
    finally:
        elapsed = time.monotonic() - started
        # Lane F's ask: name the hole rather than let the next one hide. If
        # the timed phases do not cover 90% of the run, say how much they
        # missed instead of leaving it to be inferred from a wall clock.
        unaccounted = unaccounted_seconds(elapsed)
        if unaccounted is not None:
            emit_progress(
                "bulkload-phase"
                " phase=UNACCOUNTED"
                " root=-"
                f" seconds={unaccounted:.3f}"
                " files=-"
                " bytes=-"
            )
        emit_progress(
            "bulkload-run"
            " event=end"
            f" verb={arguments.command}"
            f" seconds={elapsed:.3f}"
            f" status={status}"
            f" exit={'-' if code is None else code}"
        )
        # `interval=None` means "keep", so the process-global heartbeat has to
        # be restored by name or a caller's tuning outlives its own run.
        configure_progress(
            stderr=not arguments.quiet,
            interval=DEFAULT_HEARTBEAT_SECONDS,
            log_stream=None,
        )
        if log is not None:
            try:
                log.close()
            except OSError:
                pass
