"""Single public CLI for AgentCaptureV4 cutovers."""

from __future__ import annotations

import argparse
import contextlib
import errno
import gc
import os
from pathlib import Path
import socket
import sqlite3
import sys
import threading
import time
import traceback
from typing import Any, Callable, Iterable, Iterator, Sequence

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
    EXIT_BOOTSTRAP,
    EXIT_EPOCH,
    EXIT_INTERNAL,
    EXIT_INTERRUPTED,
    EXIT_OK,
    EXIT_REFUSED,
    EXIT_STORAGE,
    EXIT_USAGE,
    BulkloadError,
    EpochRefusal,
    MAX_JSON_BYTES,
    UNCLASSIFIED_REFUSAL_CODE,
    assert_no_overlap,
    atomic_write,
    canonical_bytes,
    capacity_observation,
    path_identity,
    read_json,
    refusal_record,
)
from .mover import DEFAULT_STREAMS
from .planner import compile_agent_plan_authorities, validate_agent_plan
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
    inspect_rsync,
    unaccounted_seconds,
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
        EXIT_STORAGE,
        "storage refusal: a volume could not hold or reflink-clone what was "
        "charged against it; the message names the path",
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
    full or over-quota filesystem is a storage refusal (5) whatever volume it
    came from -- the source host's own evidence and live-snapshot custody
    included, since `agent-capture` has no destination at all; `failure_detail`
    names the path so the operator knows which host to look at. Every other
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
            return EXIT_STORAGE
        return EXIT_REFUSED
    if isinstance(error, sqlite3.Error):
        return EXIT_REFUSED
    return EXIT_INTERNAL


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
        # One lock and one write per line: the heartbeat runs on its own
        # thread while phase lines come from the main one, and `phase_timing`
        # writes single-shot records to the same stderr. Two writes per line
        # (text, then newline) would let a heartbeat split another record.
        self._lock = threading.Lock()

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
        line = " ".join(parts) + "\n"
        try:
            with self._lock:
                self.stream.write(line)
                self.stream.flush()
        except (AttributeError, OSError, ValueError):
            # Telemetry may never decide the outcome of a cutover. A closed
            # stream raises `ValueError`, a broken pipe `OSError`, and a
            # `sys.stderr` that is None at all raises `AttributeError` — an
            # embedder is allowed to hand us any of the three.
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
    """--progress / --no-progress, defaulting to ON on every stderr.

    Deliberately not gated on `stderr.isatty()`. The measured complaint this
    answers is a *redirected* one -- `logs/preseed-push.log` is 0 bytes for a
    2h32m, 84 GiB push -- i.e. exactly the non-TTY case a TTY default would
    still leave silent. An orchestrator that needs byte-exact stderr passes
    `--no-progress`; a heartbeat is otherwise cheaper than a hung ceremony.
    """
    choice = getattr(arguments, "progress", None)
    if choice is None:
        return True
    return bool(choice)


def failure_detail(error: BaseException, code: int) -> str:
    """The one-line failure message, naming the path wherever one is known.

    Exit 5 is volume-neutral: the same capacity gate guards the destination
    stage, the rollback root, and the source host's own live-snapshot custody.
    A bare "no space left on device" would send an operator whose source disk
    filled to the wrong machine, so an `OSError` that carries a filename says
    which path it was.
    """
    detail = f"{type(error).__name__}: {error}" if code == EXIT_INTERNAL else str(error)
    if isinstance(error, OSError):
        named: list[str] = []
        for name in (error.filename, error.filename2):
            if name is None:
                continue
            try:
                named.append(os.fsdecode(name))
            except TypeError:
                # `filename` is a file descriptor on some OSError shapes.
                named.append(str(name))
        missing = [name for name in named if name not in detail]
        if missing:
            detail = f"{detail} (path: {', '.join(missing)})"
    return detail


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
        # Split exactly as `stable_capture_pair` (scanner.py:4751-4786) splits
        # it, so this streaming re-implementation carries the same process
        # status as the function it mirrors. The live host moving under an A/B
        # pair is the retryable epoch class (3); a reused capture ID, mixed
        # writer boundaries, or a capture that carries blockers is the operator
        # handing over the wrong documents, which is the general refusal (4)
        # and identical on retry.
        if first_id == second["capture_id"]:
            raise BulkloadError(f"{role} A/B captures reuse one capture ID")
        if first_quiesced != second["writers_quiesced"]:
            raise BulkloadError(f"{role} A/B captures use different writer boundaries")
        if first_quiesced:
            if catalog_sha256 != second["catalog_sha256"]:
                raise EpochRefusal(f"{role} A/B captures are not byte-stable")
        else:
            snapshot = second["catalog"]["snapshot"]
            if (
                first_contract != snapshot["contract_sha256"]
                or first_seal == snapshot["seal_sha256"]
                or snapshot["base"]
                != {
                    "seal_path": first_snapshot["seal_path"],
                    "seal_sha256": first_snapshot["seal_sha256"],
                    "snapshot_id": first_snapshot["snapshot_id"],
                }
            ):
                raise EpochRefusal(f"{role} A/B live snapshot contract is unstable")
            if not first_identities.issubset(
                _catalog_path_identities(second["catalog"])
            ):
                raise EpochRefusal(f"{role} A/B live snapshot loses prior custody")
        if not first_complete or not second["complete"]:
            raise BulkloadError(f"{role} captures contain blockers")
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
    if arguments.mover != "rsync":
        raise BulkloadError("transport mover applies only to the push transport")
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
            transport_mover=arguments.mover,
            transport_streams=arguments.transport_streams,
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


def _dry_run_plan(arguments: argparse.Namespace, report: dict[str, Any]) -> None:
    paths, sizes = _plan_capture_bounds(arguments)
    report["reads"].extend(
        {"path": os.fspath(path), "bytes": size} for path, size in zip(paths, sizes)
    )
    report["mutations"].append(
        _mutation("evidence-write", arguments.output, mode="0600")
    )
    report["live_destination_mutations"] = 0


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
    stage_receipt = stage_root / f"receipt-{phase}.json"
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
        # local and materialize: both create the stage root, write every staged
        # object under it, and seal `receipt-<phase>.json`
        # (executor.py:1244-1245, :1487-1489). materialize additionally reads
        # the two chained receipts and re-verifies the pushed quarantine
        # (executor.py:1472-1485, :594). Naming only `stage-object-write` here
        # under-reported both.
        report["mutations"].extend(
            (
                _mutation("stage-root-create", stage_root, mode="0700"),
                _mutation("stage-object-write", stage_root),
                _mutation("stage-receipt-write", stage_receipt),
            )
        )
        if mode == "materialize":
            report["reads"].extend(
                {"path": os.fspath(Path(value))}
                for value in (arguments.prepare_receipt, arguments.transport_receipt)
                if value
            )
            report["reads"].append({"path": os.fspath(quarantine)})
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


# Seeded into the report BEFORE any builder or fence runs, so a report that
# refused early still names what it did not check. Appending these at the end
# of each builder -- as the first cut did -- published
# `"not_evaluated":[],"mutations":[]` on exactly the refusal path, a
# machine-readable document asserting the verb mutates nothing and skipped no
# gate when in fact it had reached neither answer.
DRY_RUN_NOT_EVALUATED: dict[str, tuple[str, ...]] = {
    "agent-capture": (
        "the corpus walk, per-entry hashes, and the file/byte/row budgets",
        "managed-exclusion namespace policy and provider typing",
        "live-snapshot convergence and the sealed A-to-B base binding",
    ),
    "agent-plan": (
        "capture schema, role, and self-digest validation",
        "A/B stability: distinct capture IDs, the B-to-A seal binding, and A-only loss",
        "the compiled union, its blockers, and the capacity contract",
    ),
    "agent-stage": (
        "the exact charged-byte capacity gate, which the executor computes",
        "chained prepare/push receipt authority and the sealed allowlist digest",
        "per-object staging, reflink custody, and changed-late deferral",
    ),
    "agent-apply": (
        "fresh destination preconditions, which apply re-reads live",
        "the exact-overwrite capacity gate and reflink rollback custody",
        "journal replay of a partially applied transaction",
    ),
}

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

    `complete` is false whenever the rehearsal refused before it finished
    enumerating. Read it before `mutations`: on a refusal the mutation list is
    a prefix, not an inventory, and `live_destination_mutations` is a floor
    rather than a count.
    """
    report: dict[str, Any] = {
        "capacity": [],
        "command": arguments.command,
        "complete": True,
        "dry_run": True,
        "exit_code": EXIT_OK,
        "live_destination_mutations": 0,
        "mutations": [],
        "not_evaluated": list(DRY_RUN_NOT_EVALUATED[arguments.command]),
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
        report["complete"] = False
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
            "emit phase and heartbeat lines on stderr (never stdout). On by "
            "default, including into a pipe or a log file -- that is the case "
            "a silent multi-hour verb is indistinguishable from a hung one. "
            "Pass --no-progress for byte-exact stderr. Progress reports; it "
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

    plan = commands.add_parser(
        "agent-plan",
        parents=[refusal, telemetry],
        help="compile an exact four-capture union",
    )
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
        "agent-stage",
        parents=[refusal, telemetry],
        help="materialize preseed or final stage",
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

    apply = commands.add_parser(
        "agent-apply",
        parents=[refusal, telemetry],
        help="apply a sealed final stage",
    )
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
        "agent-verify",
        parents=[refusal, telemetry],
        help="independently verify final state",
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
        "agent-rollback",
        parents=[refusal, telemetry],
        help="restore exact overwritten state",
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
        "agent-recover",
        parents=[refusal, telemetry],
        help="resume or roll back an interrupted apply",
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


def _failure_record(
    arguments: argparse.Namespace, error: BaseException
) -> dict[str, Any]:
    """The structured record for one refusal, converted or not.

    A refusal site that has not been converted yet still produces a record —
    with code UNCLASSIFIED and the raised sentence as its message — so a
    caller reading `--failure-output` can tell "this refusal has no diagnosis
    yet" apart from "no refusal happened".

    The channel covers everything `main` catches, which is now every
    exception below `KeyboardInterrupt` and `SystemExit`. A defect in the
    engine itself therefore also earns a record — code UNCLASSIFIED, exit 6 —
    instead of escaping as a bare traceback with an undocumented status.
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
    # Two telemetry channels, one promise. `--no-progress` is documented as
    # the way to get byte-exact stderr and `--quiet` as the way to suppress
    # engine telemetry; after both landed, either flag has to silence both or
    # neither promise holds. `--progress-log` is unaffected: a silenced stderr
    # still fills the file an unattended agent tails.
    telemetry = progress_enabled(arguments) and not arguments.quiet
    progress = Progress(telemetry, command=arguments.command)
    started = time.monotonic()
    log = None
    status = "fail"
    code: int | None = None
    try:
        # The flag-only half of the refusal-path guard runs before anything
        # else, because every later handler writes the refusal record and
        # none of them may write it to a path this rejects.
        _protect_failure_output(arguments)
        log = _open_progress_log(arguments)
        configure_progress(
            stderr=telemetry,
            interval=arguments.heartbeat_seconds,
            log_stream=log,
        )
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
                code = int(report["exit_code"])
                status = "ok" if code == EXIT_OK else "fail"
                return code
            progress.phase("preflight")
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
            progress.phase(arguments.command)
            result = arguments.handler(arguments)
            progress.phase("write-evidence")
            _write(arguments.output, result)
    except KeyboardInterrupt:
        # The operator stopped this process. Bulkload never signals one, and
        # an interruption is not a refusal, so no record is written for it.
        print("bulkload: INTERRUPTED", file=sys.stderr)
        code = EXIT_INTERRUPTED
        return EXIT_INTERRUPTED
    except SystemExit:
        raise
    except BaseException as error:
        # Deliberately broader than (BulkloadError, OSError, sqlite3.Error):
        # a malformed evidence document used to escape as a bare KeyError
        # traceback with an undocumented status. It now exits 6 with one line.
        code = exit_code_for(error)
        # The refusal is the deliverable; the record is a convenience. Print
        # first so no failure of the record write can cost the operator the
        # one line that names what refused.
        print(f"bulkload: FAIL[{code}]: {failure_detail(error, code)}", file=sys.stderr)
        _emit_failure(arguments, error)
        # 6 is the only class with no curated message: nothing in the engine
        # meant to raise it, so the one-liner names a type and not a cause. An
        # operator whose hour-4 ceremony dies on `KeyError: 'catalog'` cannot
        # tell the protector from the planner from the executor, and will not
        # re-run a multi-hour verb just to set BULKLOAD_TRACEBACK=1. The typed
        # refusals (3/4/5) keep the one-line form, where the message is the
        # diagnosis and a traceback would only name the raise site.
        if code == EXIT_INTERNAL or os.environ.get("BULKLOAD_TRACEBACK") == "1":
            traceback.print_exc()
        return code
    else:
        status = "ok"
        code = EXIT_OK
        return EXIT_OK
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
            stderr=telemetry,
            interval=DEFAULT_HEARTBEAT_SECONDS,
            log_stream=None,
        )
        if log is not None:
            try:
                log.close()
            except OSError:
                pass
