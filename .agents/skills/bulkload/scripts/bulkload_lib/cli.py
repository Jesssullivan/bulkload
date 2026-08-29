"""Single public CLI for AgentCaptureV4 cutovers."""

from __future__ import annotations

import argparse
import errno
import gc
import os
from pathlib import Path
import sqlite3
import sys
import threading
import time
import traceback
from typing import Any, Callable, Sequence

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
    EXIT_BOOTSTRAP,
    EXIT_DESTINATION,
    EXIT_EPOCH,
    EXIT_INTERNAL,
    EXIT_INTERRUPTED,
    EXIT_OK,
    EXIT_REFUSED,
    EXIT_USAGE,
    BulkloadError,
    MAX_JSON_BYTES,
    assert_no_overlap,
    atomic_write,
    canonical_bytes,
    capacity_observation,
    read_json,
)
from .planner import compile_agent_plan_authorities, validate_agent_plan
from .scanner import (
    DEFAULT_MAX_BYTES,
    DEFAULT_MAX_FILES,
    DEFAULT_MAX_SQLITE_ROWS,
    _catalog_path_identities,
    canonical_path_map,
    capture_agent_state,
    inspect_rsync,
    validate_agent_capture,
)


MAX_PUBLIC_JSON_BYTES = MAX_JSON_BYTES
LARGE_PLAN_THRESHOLD_BYTES = 512 * 1024**2
PROGRESS_INTERVAL_SECONDS = 30.0
DRY_RUN_REPORT = "bulkload-dry-run"

# The documented process-status table. `EXIT_BOOTSTRAP` is raised by the
# launcher (scripts/bulkload.py) before this module is importable, so it never
# appears here; it is imported and re-exported so the table has exactly one
# home. Keep this in sync with README "Exit codes" and SKILL.md "Exit codes".
EXIT_CODES: tuple[tuple[int, str], ...] = (
    (EXIT_OK, "the verb completed and its evidence was written"),
    (
        EXIT_USAGE,
        "the command line is wrong (argparse usage, --help/--version excepted)",
    ),
    (EXIT_BOOTSTRAP, "the pinned launcher could not build its source closure"),
    (EXIT_EPOCH, "quiescence/epoch refusal: the live source moved under the capture"),
    (
        EXIT_REFUSED,
        "custody, plan, or typed-invariant refusal (the general fail-closed class)",
    ),
    (
        EXIT_DESTINATION,
        "destination refusal: the capacity gate failed or a required reflink failed",
    ),
    (EXIT_INTERNAL, "an unexpected internal error, including malformed input evidence"),
    (
        EXIT_INTERRUPTED,
        "the operator interrupted the process (SIGINT); nothing was signalled by Bulkload",
    ),
)


def exit_code_for(error: BaseException) -> int:
    """Classify one raised error into the documented exit-code table.

    Refusal classes carry their own `exit_code`. An `OSError` that reports a
    full or over-quota filesystem is a destination refusal; every other
    `OSError` and `sqlite3.Error` is the general refusal class. Anything else
    reached this process by surprise and is internal (6) -- that is the code
    that a malformed evidence document earns, instead of a bare traceback.
    """
    if isinstance(error, BulkloadError):
        return int(getattr(error, "exit_code", EXIT_REFUSED))
    if isinstance(error, OSError):
        full = {
            getattr(errno, name)
            for name in ("ENOSPC", "EDQUOT")
            if hasattr(errno, name)
        }
        if error.errno in full:
            return EXIT_DESTINATION
        return EXIT_REFUSED
    if isinstance(error, sqlite3.Error):
        return EXIT_REFUSED
    return EXIT_INTERNAL


def _stderr_is_tty() -> bool:
    try:
        return bool(sys.stderr.isatty())
    except (AttributeError, OSError, ValueError):
        return False


class Progress:
    """Phase and heartbeat reporting on stderr.

    Progress writes only to stderr, never to stdout, never to a live path, and
    never signals a process. It exists because silence is the measured
    operator complaint against this engine: a verb that has printed nothing
    for two hours cannot be told apart from a hung one, and the answer to that
    must not be a signal.
    """

    def __init__(
        self,
        enabled: bool,
        *,
        command: str = "",
        stream: Any = None,
        interval: float = PROGRESS_INTERVAL_SECONDS,
        clock: Callable[[], float] = time.monotonic,
    ) -> None:
        self.enabled = bool(enabled)
        self.command = command
        self.stream = sys.stderr if stream is None else stream
        self.interval = interval
        self.clock = clock
        self.phase_name = "start"
        self._started = clock()
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None

    def _emit(self, event: str, **fields: Any) -> None:
        if not self.enabled:
            return
        parts = [
            f"bulkload: {event}:",
            f"command={self.command or '-'}",
            f"phase={self.phase_name}",
            f"elapsed={self.clock() - self._started:.1f}s",
        ]
        parts.extend(f"{key}={value}" for key, value in sorted(fields.items()))
        try:
            print(" ".join(parts), file=self.stream, flush=True)
        except (OSError, ValueError):
            self.enabled = False

    def _heartbeat(self) -> None:
        while not self._stop.wait(self.interval):
            self._emit("alive")

    def phase(self, name: str) -> None:
        self.phase_name = name
        self._emit("phase")

    def __enter__(self) -> "Progress":
        if self.enabled and self.interval > 0:
            self._thread = threading.Thread(
                target=self._heartbeat, name="bulkload-progress", daemon=True
            )
            self._thread.start()
        return self

    def __exit__(self, kind: Any, value: Any, trace: Any) -> bool:
        self._stop.set()
        thread = self._thread
        self._thread = None
        if thread is not None:
            thread.join(timeout=1.0)
        return False


def progress_enabled(arguments: argparse.Namespace) -> bool:
    """--progress / --no-progress, defaulting to on for an interactive stderr."""
    choice = getattr(arguments, "progress", None)
    if choice is None:
        return _stderr_is_tty()
    return bool(choice)


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


def _capture_snapshot_root(arguments: argparse.Namespace) -> Path | None:
    """Refuse a live capture without owner-private evidence, and site its custody.

    Shared by `_agent_capture` and the dry run so both reach the identical
    refusal from the identical code, rather than a rehearsal that only
    resembles the real precondition.
    """
    if arguments.acknowledge_writers_quiesced:
        return None
    if arguments.output == "-":
        raise BulkloadError("live capture requires an owner-private evidence path")
    output = Path(arguments.output).expanduser()
    return output.parent / f".{output.name}.snapshot"


def _agent_capture(arguments: argparse.Namespace) -> dict[str, Any]:
    home = Path(arguments.home).expanduser()
    snapshot_root = _capture_snapshot_root(arguments)
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


PLAN_CAPTURE_ARGUMENTS = ("source_a", "source_b", "destination_a", "destination_b")


def _plan_capture_bounds(
    arguments: argparse.Namespace,
) -> tuple[list[Path], list[int]]:
    """The bounded-JSON and planning-memory gates, before any capture is read."""
    paths = [
        Path(getattr(arguments, name)).expanduser() for name in PLAN_CAPTURE_ARGUMENTS
    ]
    sizes = [path.stat(follow_symlinks=False).st_size for path in paths]
    if any(size > MAX_JSON_BYTES for size in sizes):
        raise BulkloadError("AgentCaptureV4 exceeds the bounded JSON contract")
    _require_large_evidence_memory(
        sum(sizes),
        message="destination memory is below the bounded AgentPlanV4 planning gate",
    )
    return paths, sizes


def _agent_plan(arguments: argparse.Namespace) -> dict[str, Any]:
    paths, _ = _plan_capture_bounds(arguments)

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


def _stage_argument_refusal(arguments: argparse.Namespace) -> None:
    """Every agent-stage argument-shape refusal, before a byte is staged."""
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
        return
    if arguments.transport_checksum:
        raise BulkloadError("transport checksum applies only to the push transport")
    if arguments.plan is None or arguments.transport_allowlist is not None:
        raise BulkloadError(
            "plan staging requires a plan and forbids a transport allowlist"
        )


def _agent_stage(arguments: argparse.Namespace) -> dict[str, Any]:
    _stage_argument_refusal(arguments)
    if arguments.transport_mode == "push":
        return push_agent_transport(
            _load(arguments.prepare_receipt),
            Path(arguments.transport_allowlist),
            accepted_plan_sha256=arguments.accept_plan_sha256,
            phase=arguments.phase,
            stage_root=Path(arguments.stage_root),
            destination_ssh_host=arguments.destination_ssh_host,
            transport_checksum=arguments.transport_checksum,
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


def _mutation(kind: str, target: Any, **fields: Any) -> dict[str, Any]:
    return {"kind": kind, "target": os.fspath(target), **fields}


def _capacity_note(label: str, target: Path) -> dict[str, Any]:
    """Observe a destination without letting the observation become a refusal.

    A missing stage or rollback root is normal before the verb that creates
    it, so an unobservable path is reported as unobserved rather than raised.
    """
    try:
        return {
            "label": label,
            "path": os.fspath(target),
            "observed": True,
            **capacity_observation(target),
        }
    except (BulkloadError, OSError) as error:
        return {
            "label": label,
            "path": os.fspath(target),
            "observed": False,
            "detail": str(error),
        }


def _operation_counts(plan: dict[str, Any]) -> dict[str, int]:
    counts: dict[str, int] = {}
    for operation in plan.get("operations", []):
        kind = str(operation.get("kind"))
        counts[kind] = counts.get(kind, 0) + 1
    return counts


def _plan_summary(plan: dict[str, Any]) -> dict[str, Any]:
    return {
        "blockers": sorted(
            {str(item.get("code")) for item in plan.get("blockers", [])}
        ),
        "capacity": plan.get("capacity", {}),
        "holds": sorted({str(item.get("code")) for item in plan.get("holds", [])}),
        "operation_counts": _operation_counts(plan),
        "operations": len(plan.get("operations", [])),
        "plan_sha256": plan.get("plan_sha256", ""),
        "ready": bool(plan.get("ready")),
    }


def _dry_run_capture(arguments: argparse.Namespace, report: dict[str, Any]) -> None:
    snapshot_root = _capture_snapshot_root(arguments)
    canonical_path_map(arguments.path_map)
    binding = inspect_rsync(arguments.rsync_path)
    report["reads"].extend(
        {"path": value}
        for value in sorted(
            {
                os.fspath(Path(value).expanduser())
                for value in (
                    arguments.home,
                    arguments.git_root,
                    arguments.codex_root,
                    arguments.claude_root,
                    arguments.pi_root,
                    arguments.snapshot_base_seal,
                )
                if value
            }
            | {
                os.fspath(path)
                for _, path, _ in [*arguments.seat, *arguments.file_seat]
            }
        )
    )
    report["transport"] = {
        "rsync": binding["path"],
        "rsync_sha256": binding["sha256"],
    }
    report["mutations"].append(
        _mutation("evidence-write", arguments.output, mode="0600")
    )
    if snapshot_root is not None:
        report["mutations"].append(
            _mutation("live-snapshot-custody-create", snapshot_root, mode="0700")
        )
    report["live_destination_mutations"] = 0
    report["not_evaluated"].extend(
        (
            "the corpus walk, per-entry hashes, and the file/byte/row budgets",
            "managed-exclusion namespace policy and provider typing",
            "live-snapshot convergence and the sealed A-to-B base binding",
        )
    )


def _dry_run_plan(arguments: argparse.Namespace, report: dict[str, Any]) -> None:
    paths, sizes = _plan_capture_bounds(arguments)
    report["reads"].extend(
        {"path": os.fspath(path), "bytes": size} for path, size in zip(paths, sizes)
    )
    report["mutations"].append(
        _mutation("evidence-write", arguments.output, mode="0600")
    )
    report["live_destination_mutations"] = 0
    report["not_evaluated"].extend(
        (
            "capture schema, role, and self-digest validation",
            "A/B stability: distinct capture IDs, the B-to-A seal binding, and A-only loss",
            "the compiled union, its blockers, and the capacity contract",
        )
    )


def _dry_run_stage(arguments: argparse.Namespace, report: dict[str, Any]) -> None:
    _stage_argument_refusal(arguments)
    stage_root = Path(arguments.stage_root)
    phase = arguments.phase
    mode = arguments.transport_mode
    # Sidecar names are the executor's own: executor.py:362 (allowlist),
    # executor.py:1470 (prepare receipt), executor.py:581 (transport receipt),
    # executor.py:623 (quarantine root).
    allowlist = stage_root / f".transport-allowlist-{phase}.nul"
    prepare_receipt = stage_root / f".prepare-receipt-{phase}.json"
    transport_receipt = stage_root / f".transport-receipt-{phase}.json"
    quarantine = stage_root / ".transport-quarantine"
    report["mutations"].append(
        _mutation("evidence-write", arguments.output, mode="0600")
    )
    if mode == "prepare":
        report["mutations"].extend(
            (
                _mutation("stage-root-create", stage_root, mode="0700"),
                _mutation("quarantine-create", quarantine, mode="0700"),
                _mutation("allowlist-write", allowlist),
                _mutation("prepare-receipt-write", prepare_receipt),
            )
        )
    elif mode == "push":
        report["mutations"].extend(
            (
                _mutation(
                    "remote-quarantine-push",
                    quarantine,
                    host=arguments.destination_ssh_host,
                    transport="ssh-rsync-push",
                ),
                _mutation(
                    "remote-transport-receipt-write",
                    transport_receipt,
                    host=arguments.destination_ssh_host,
                ),
            )
        )
    else:
        report["mutations"].append(_mutation("stage-object-write", stage_root))
    report["live_destination_mutations"] = 0
    if arguments.plan is not None:
        plan = _load(arguments.plan)
        validate_agent_plan(plan, require_ready=True)
        # executor.py:1405 -- the same comparison, before any stage byte.
        if arguments.accept_plan_sha256 != plan["plan_sha256"]:
            raise BulkloadError("accepted plan digest does not match AgentPlanV4")
        report["plan"] = _plan_summary(plan)
    report["capacity"] = [
        _capacity_note("stage-root", stage_root),
    ]
    report["not_evaluated"].extend(
        (
            "the exact charged-byte capacity gate, which the executor computes",
            "chained prepare/push receipt authority and the sealed allowlist digest",
            "per-object staging, reflink custody, and changed-late deferral",
        )
    )


def _dry_run_apply(arguments: argparse.Namespace, report: dict[str, Any]) -> None:
    plan = _load(arguments.plan)
    validate_agent_plan(plan, require_ready=True)
    stage_receipt = _load(arguments.stage_receipt)
    validate_stage_receipt(stage_receipt, require_final=True)
    # executor.py:2414-2417 -- the same two bindings, before the first mutation.
    if arguments.accept_plan_sha256 != plan["plan_sha256"]:
        raise BulkloadError("accepted plan digest does not match AgentPlanV4")
    if stage_receipt["plan_sha256"] != plan["plan_sha256"]:
        raise BulkloadError("final stage belongs to a different plan")
    summary = _plan_summary(plan)
    report["plan"] = summary
    report["reads"].extend(
        {"path": os.fspath(Path(value))}
        for value in (arguments.plan, arguments.stage_receipt)
    )
    report["mutations"].extend(
        (
            _mutation("journal-write", arguments.journal),
            _mutation("rollback-custody-write", arguments.rollback_root),
            _mutation(
                "live-destination-apply",
                Path(plan["destination"]["catalog"]["root_bindings"]["home"]),
                operations=summary["operations"],
            ),
            _mutation("evidence-write", arguments.output, mode="0600"),
        )
    )
    report["live_destination_mutations"] = summary["operations"]
    report["capacity"] = [
        _capacity_note("rollback-root", Path(arguments.rollback_root)),
        _capacity_note("stage-root", Path(stage_receipt["stage_root"])),
    ]
    report["not_evaluated"].extend(
        (
            "fresh destination preconditions, which apply re-reads live",
            "the exact-overwrite capacity gate and reflink rollback custody",
            "journal replay of a partially applied transaction",
        )
    )


DRY_RUN_BUILDERS: dict[str, Callable[[argparse.Namespace, dict[str, Any]], None]] = {
    "agent-capture": _dry_run_capture,
    "agent-plan": _dry_run_plan,
    "agent-stage": _dry_run_stage,
    "agent-apply": _dry_run_apply,
}


def _print_report(report: dict[str, Any]) -> None:
    """Write the dry-run report to stdout, byte-exact where stdout is binary."""
    payload = canonical_bytes(report) + b"\n"
    stream = getattr(sys.stdout, "buffer", None)
    if stream is None:
        sys.stdout.write(payload.decode("utf-8"))
    else:
        stream.write(payload)
    sys.stdout.flush()


def dry_run(arguments: argparse.Namespace) -> dict[str, Any]:
    """Report the exact actions and refusals of a verb without mutating.

    A dry run writes nothing -- not the stage, not the journal, not even the
    `--output` evidence document -- and prints its report on stdout instead.
    It runs the refusals that are reachable without reading the corpus, from
    the same functions the real verb calls, and names in `not_evaluated`
    every gate it did not reach. `exit_code` is the status the process will
    return, so a dry run is usable as a gate and not only as documentation.
    """
    report: dict[str, Any] = {
        "capacity": [],
        "command": arguments.command,
        "dry_run": True,
        "exit_code": EXIT_OK,
        "live_destination_mutations": 0,
        "mutations": [],
        "not_evaluated": [],
        "output": arguments.output,
        "reads": [],
        "refusals": [],
        "report": DRY_RUN_REPORT,
    }
    try:
        _protect_output(arguments)
        DRY_RUN_BUILDERS[arguments.command](arguments, report)
    except Exception as error:  # noqa: BLE001 - a dry run always reports
        # `Exception`, not `(BulkloadError, OSError, sqlite3.Error)`: a
        # malformed evidence document raises `KeyError` here, and the whole
        # point of a dry run is that it answers with a report rather than a
        # traceback. `KeyboardInterrupt` and `SystemExit` still escape.
        report["refusals"].append(
            {"class": type(error).__name__, "message": str(error)}
        )
        report["exit_code"] = exit_code_for(error)
    return report


class _Parser(argparse.ArgumentParser):
    """An ArgumentParser whose usage failures exit 1, not argparse's 2.

    Exit 2 belongs to the launcher's bootstrap failure
    (`scripts/bulkload.py`), which is raised before this module can be
    imported. Leaving argparse on 2 would make a typo indistinguishable from
    a broken source closure. `--help` and `--version` keep argparse's 0.
    """

    def error(self, message: str) -> Any:
        self.print_usage(sys.stderr)
        print(f"{self.prog}: usage error: {message}", file=sys.stderr)
        raise SystemExit(EXIT_USAGE)


def _add_common(parser: argparse.ArgumentParser, *, dry_run: bool) -> None:
    if dry_run:
        parser.add_argument(
            "--dry-run",
            action="store_true",
            help=(
                "print the exact mutations, reads, and refusals this verb "
                "would perform, then exit without writing anything -- not "
                "the stage, not the journal, not the --output evidence. The "
                "report names in not_evaluated every gate it did not reach."
            ),
        )
    parser.add_argument(
        "--progress",
        action=argparse.BooleanOptionalAction,
        default=None,
        help=(
            "emit phase and heartbeat lines on stderr (never stdout). "
            "Defaults to on when stderr is a terminal. Progress reports; it "
            "never signals a process."
        ),
    )


def build_parser() -> argparse.ArgumentParser:
    parser = _Parser(
        prog="bulkload",
        description="Typed, reviewed Git and agent-state cutover",
        epilog="Exit codes: "
        + "; ".join(f"{code} {meaning}" for code, meaning in EXIT_CODES),
    )
    parser.add_argument(
        "--version",
        action="version",
        version=f"%(prog)s {__version__}",
        help="print the pinned Bulkload version and exit",
    )
    commands = parser.add_subparsers(dest="command", required=True)

    capture = commands.add_parser("agent-capture", help="write AgentCaptureV4 evidence")
    capture.add_argument(
        "--role",
        choices=("source", "destination"),
        required=True,
        help="which side this capture observes: the machine being left or the machine being moved to",
    )
    capture.add_argument(
        "--home", required=True, help="absolute live home root for this role"
    )
    capture.add_argument(
        "--git-root",
        required=True,
        help="absolute live Git fleet root for this role, declared in addition to the home map",
    )
    capture.add_argument(
        "--codex-root",
        help="absolute Codex provider root; omit when this role has none",
    )
    capture.add_argument(
        "--claude-root",
        help="absolute Claude provider root; omit when this role has none",
    )
    capture.add_argument(
        "--pi-root", help="absolute Pi provider root; omit when this role has none"
    )
    capture.add_argument(
        "--seat",
        action="append",
        type=_parse_seat,
        default=[],
        help="repeatable declared directory seat, NAME=/absolute/path; a whole-home seat is invalid",
    )
    capture.add_argument(
        "--file-seat",
        action="append",
        type=_parse_file_seat,
        default=[],
        help="repeatable declared regular-file seat, NAME=/absolute/file; an absent optional file is captured as absent",
    )
    capture.add_argument(
        "--managed-exclusion",
        action="append",
        type=_parse_managed_exclusion,
        default=[],
        help="repeatable exact managed exclusion, PROVIDER:RELATIVE_PATH; must be identical on both roles",
    )
    capture.add_argument(
        "--rsync-path",
        required=True,
        help="absolute pinned GNU rsync; capture binds its path, hash, protocol, and features",
    )
    capture.add_argument(
        "--path-map",
        action="append",
        type=_parse_mapping,
        required=True,
        help="repeatable source-to-destination map SOURCE=DESTINATION; the longest source prefix wins",
    )
    capture.add_argument(
        "--acknowledge-writers-quiesced",
        action="store_true",
        help=(
            "assert every writer is already stopped and skip immutable "
            "live-snapshot custody. The shipped ceremony does not use this: "
            "sessions stay alive and capture takes the snapshot path."
        ),
    )
    capture.add_argument(
        "--snapshot-base-seal",
        help="path to capture A's snapshot-seal.json; required for the B half of a live A/B pair",
    )
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
    capture.add_argument(
        "--max-files",
        type=int,
        default=DEFAULT_MAX_FILES,
        help="refuse a capture past this many entries (default: %(default)s)",
    )
    capture.add_argument(
        "--max-bytes",
        type=int,
        default=DEFAULT_MAX_BYTES,
        help="refuse a capture past this many payload bytes (default: %(default)s)",
    )
    capture.add_argument(
        "--max-sqlite-rows",
        type=int,
        default=DEFAULT_MAX_SQLITE_ROWS,
        help="refuse a capture past this many catalogued SQLite rows (default: %(default)s)",
    )
    capture.add_argument(
        "--jobs",
        type=int,
        help=(
            "Git-workspace capture workers. Omit to keep the shipped default "
            "of 3; raise it only on a host with the memory headroom for that "
            "many concurrent `git fsck --full` runs."
        ),
    )
    capture.add_argument(
        "--output",
        required=True,
        help="owner-private path for the AgentCaptureV4 document; '-' is refused for a live capture",
    )
    _add_common(capture, dry_run=True)
    capture.set_defaults(handler=_agent_capture)

    plan = commands.add_parser("agent-plan", help="compile an exact four-capture union")
    plan.add_argument(
        "--source-a", required=True, help="source capture A: the sealed immutable base"
    )
    plan.add_argument(
        "--source-b",
        required=True,
        help="source capture B: chained to A's seal and the plan authority",
    )
    plan.add_argument(
        "--destination-a",
        required=True,
        help="destination capture A: the sealed immutable base",
    )
    plan.add_argument(
        "--destination-b",
        required=True,
        help="destination capture B: chained to A's seal and the plan authority",
    )
    plan.add_argument(
        "--output",
        required=True,
        help="owner-private path for the compiled AgentPlanV4",
    )
    _add_common(plan, dry_run=True)
    plan.set_defaults(handler=_agent_plan)

    stage = commands.add_parser(
        "agent-stage", help="materialize preseed or final stage"
    )
    stage.add_argument(
        "--phase",
        choices=("preseed", "final"),
        required=True,
        help="preseed rehearses onto the external stage; only a materialized final stage is apply authority",
    )
    stage.add_argument(
        "--plan",
        help="accepted AgentPlanV4 path; required for every transport mode except push",
    )
    stage.add_argument(
        "--accept-plan-sha256",
        required=True,
        help="the exact reviewed plan_sha256; a mismatch refuses before a stage byte is written",
    )
    stage.add_argument(
        "--stage-root",
        required=True,
        help="absolute external stage/quarantine root, outside every live root and on reflink-capable storage",
    )
    stage.add_argument(
        "--allow-accounted-copy",
        action="store_true",
        help="permit a copy for charged incoming/transformed bytes only; never a hidden full rollback duplicate",
    )
    stage.add_argument(
        "--transport-mode",
        choices=("local", "prepare", "push", "materialize"),
        default="local",
        help="local stage, destination prepare, source push, or destination materialize (default: %(default)s)",
    )
    stage.add_argument(
        "--destination-ssh-host",
        help="USER@HOST for a push; every connection originates on the source",
    )
    stage.add_argument(
        "--prepare-receipt",
        help="the destination prepare receipt; required for push and materialize",
    )
    stage.add_argument(
        "--transport-allowlist",
        help="the sealed NUL allowlist written by prepare; push only",
    )
    stage.add_argument("--transport-receipt", help="the push receipt; materialize only")
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
        "--capacity-reserve-bytes",
        type=int,
        default=DEFAULT_CAPACITY_RESERVE_BYTES,
        help="bytes held back from the destination capacity gate (default: %(default)s)",
    )
    stage.add_argument(
        "--output", required=True, help="owner-private path for this stage receipt"
    )
    _add_common(stage, dry_run=True)
    stage.set_defaults(handler=_agent_stage)

    apply = commands.add_parser("agent-apply", help="apply a sealed final stage")
    apply.add_argument("--plan", required=True, help="the accepted final AgentPlanV4")
    apply.add_argument(
        "--stage-receipt",
        required=True,
        help="the materialized final stage receipt (ready_for_apply=true)",
    )
    apply.add_argument(
        "--accept-plan-sha256",
        required=True,
        help="the exact reviewed plan_sha256; a mismatch refuses before the first live mutation",
    )
    apply.add_argument(
        "--journal",
        required=True,
        help="durable journal path that makes apply, rollback, and recovery idempotent",
    )
    apply.add_argument(
        "--rollback-root",
        required=True,
        help="absolute reflink-capable root for exact overwritten-entry custody; a failed reflink is a hard stop",
    )
    apply.add_argument(
        "--capacity-reserve-bytes",
        type=int,
        default=DEFAULT_CAPACITY_RESERVE_BYTES,
        help="bytes held back from the exact-overwrite capacity gate (default: %(default)s)",
    )
    apply.add_argument(
        "--output", required=True, help="owner-private path for the apply receipt"
    )
    _add_common(apply, dry_run=True)
    apply.set_defaults(handler=_agent_apply)

    verify = commands.add_parser(
        "agent-verify", help="independently verify final state"
    )
    verify.add_argument("--plan", required=True, help="the applied final AgentPlanV4")
    verify.add_argument(
        "--stage-receipt", required=True, help="the final stage receipt that apply used"
    )
    verify.add_argument(
        "--apply-receipt", required=True, help="the receipt written by agent-apply"
    )
    verify.add_argument(
        "--destination-verify-receipt",
        help="the destination's verify receipt; supply it on the source to emit the cutover-release receipt",
    )
    verify.add_argument(
        "--output", required=True, help="owner-private path for the verify receipt"
    )
    _add_common(verify, dry_run=False)
    verify.set_defaults(handler=_agent_verify)

    rollback = commands.add_parser(
        "agent-rollback", help="restore exact overwritten state"
    )
    rollback.add_argument(
        "--apply-receipt", required=True, help="the apply receipt being reverted"
    )
    rollback.add_argument(
        "--accept-receipt-sha256",
        required=True,
        help="the exact receipt_sha256 of that apply receipt; rollback is attended and digest-bound",
    )
    rollback.add_argument(
        "--output", required=True, help="owner-private path for the rollback receipt"
    )
    _add_common(rollback, dry_run=False)
    rollback.set_defaults(handler=_agent_rollback)

    recover = commands.add_parser(
        "agent-recover", help="resume or roll back an interrupted apply"
    )
    recover.add_argument(
        "--plan",
        required=True,
        help="the final AgentPlanV4 of the interrupted transaction",
    )
    recover.add_argument(
        "--stage-receipt",
        required=True,
        help="the final stage receipt of that transaction",
    )
    recover.add_argument(
        "--journal",
        required=True,
        help="the durable journal left by the interrupted apply",
    )
    recover.add_argument(
        "--strategy",
        choices=("forward", "rollback"),
        required=True,
        help="forward completes the transaction; rollback restores the sealed snapshot",
    )
    recover.add_argument(
        "--output", required=True, help="owner-private path for the recovery receipt"
    )
    _add_common(recover, dry_run=False)
    recover.set_defaults(handler=_agent_recover)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    parser = build_parser()
    arguments = parser.parse_args(argv)
    progress = Progress(progress_enabled(arguments), command=arguments.command)
    try:
        with progress:
            if getattr(arguments, "dry_run", False):
                progress.phase("dry-run")
                report = dry_run(arguments)
                _print_report(report)
                for refusal in report["refusals"]:
                    print(
                        f"bulkload: DRY-RUN REFUSAL: {refusal['message']}",
                        file=sys.stderr,
                    )
                return int(report["exit_code"])
            progress.phase("preflight")
            _protect_output(arguments)
            progress.phase(arguments.command)
            result = arguments.handler(arguments)
            progress.phase("write-evidence")
            _write(arguments.output, result)
    except KeyboardInterrupt:
        # The operator stopped this process. Bulkload never signals one.
        print("bulkload: INTERRUPTED", file=sys.stderr)
        return EXIT_INTERRUPTED
    except SystemExit:
        raise
    except BaseException as error:
        # Deliberately broader than (BulkloadError, OSError, sqlite3.Error):
        # a malformed evidence document used to escape as a bare KeyError
        # traceback with an undocumented status. It now exits 6 with one line.
        code = exit_code_for(error)
        detail = (
            f"{type(error).__name__}: {error}" if code == EXIT_INTERNAL else str(error)
        )
        print(f"bulkload: FAIL[{code}]: {detail}", file=sys.stderr)
        if os.environ.get("BULKLOAD_TRACEBACK") == "1":
            traceback.print_exc()
        return code
    return EXIT_OK
