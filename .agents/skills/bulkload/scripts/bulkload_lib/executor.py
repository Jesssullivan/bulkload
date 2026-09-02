"""Reflink-aware staging and journaled AgentPlanV4 execution."""

from __future__ import annotations

from collections import defaultdict
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import socket
import sqlite3
import stat
import subprocess
import sys
import tempfile
from typing import IO, Any, Iterable, Iterator, Sequence

from .model import (
    AGENT_APPLY_SCHEMA,
    AGENT_JOURNAL_SCHEMA,
    AGENT_RECOVER_SCHEMA,
    AGENT_ROLLBACK_SCHEMA,
    AGENT_STAGE_SCHEMA,
    AGENT_VERIFY_SCHEMA,
    BulkloadError,
    MAX_JSON_BYTES,
    accounted_copy,
    assert_no_overlap,
    atomic_write,
    atomic_write_json,
    canonical_bytes,
    durable_makedirs,
    fsync_directory,
    git_environment,
    new_id,
    read_json,
    reflink_clone,
    require_capacity,
    require_digest,
    require_exact_keys,
    seal,
    sha256_bytes,
    sha256_file,
    sha256_symlink,
    utc_now,
)
from . import mover
from .planner import PlanOperationResolver, validate_agent_plan
from .scanner import (
    DEFAULT_MAX_SQLITE_ROWS,
    _quote_identifier,
    _sqlite_catalog_from_snapshot,
    _typed_sql_value,
    inspect_rsync,
    shell_safe_executable,
    snapshot_sqlite,
    sqlite_catalog,
    validate_live_snapshot_generation,
    validate_snapshot_custody,
)


DEFAULT_CAPACITY_RESERVE_BYTES = 10 * 1024**3
SSH_OPTIONS = (
    "-oBatchMode=yes",
    "-oStrictHostKeyChecking=yes",
    "-oClearAllForwardings=yes",
)
REMOTE_STAGE_ROOT = re.compile(r"/(?:[A-Za-z0-9._-]+/)*[A-Za-z0-9._-]+")


class _StageSourceChanged(BulkloadError):
    """A preliminary-plan entry moved and is not eligible for authority."""


def _remote_safe_stage_root(path: Path) -> Path:
    value = os.fspath(path)
    if not REMOTE_STAGE_ROOT.fullmatch(value) or any(
        component in {".", ".."} for component in value.split("/")
    ):
        raise BulkloadError("stage root is not a canonical remote-safe absolute path")
    return Path(value)


def _write_apply_journal(path: Path, journal: dict[str, Any]) -> None:
    seal(journal, "journal_sha256")
    atomic_write_json(path, journal)


def _read_apply_journal(path: Path) -> dict[str, Any]:
    journal = read_json(path)
    if journal.get("schema") != AGENT_JOURNAL_SCHEMA:
        raise BulkloadError("apply journal schema is invalid")
    require_digest(journal, "journal_sha256")
    allowed = {
        "apply_receipt",
        "capacity",
        "created_git_objects",
        "created_git_roots",
        "created_worktrees",
        "git_prepared",
        "git_refs_after",
        "git_refs_before",
        "git_worktrees_after",
        "git_worktrees_before",
        "journal_id",
        "journal_sha256",
        "mutations",
        "plan_sha256",
        "progress",
        "recovery_receipts",
        "rollback_progress",
        "rollback_receipt",
        "rollback_root",
        "rollback_snapshots",
        "schema",
        "snapshot_progress",
        "stage_manifest_sha256",
        "stage_receipt_sha256",
        "state",
        "transaction_id",
        "updated_at",
        "verify_receipt_sha256",
    }
    unknown = set(journal) - allowed
    if unknown:
        raise BulkloadError(f"AgentJournalV4 has unapproved fields: {sorted(unknown)}")
    return journal


def _git(repository: Path, arguments: Sequence[str], *, check: bool = True) -> bytes:
    result = subprocess.run(
        ["git", "-C", os.fspath(repository), *arguments],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=git_environment(),
    )
    if check and result.returncode != 0:
        raise BulkloadError(
            f"Git mutation/verification failed ({arguments[0] if arguments else 'unknown'})"
        )
    return result.stdout


def _live_roots(plan: dict[str, Any]) -> list[Path]:
    return [Path(value) for value in _catalog_roots(plan["destination"]["catalog"])]


def _catalog_roots(catalog: dict[str, Any]) -> list[str]:
    values = {
        catalog["root_bindings"]["git_root"],
        catalog["root_bindings"]["home"],
    }
    values.update(
        provider["path"]
        for provider in catalog.get("providers", [])
        if provider.get("exists")
    )
    values.update(
        seat["path"] for seat in catalog.get("seats", []) if seat.get("exists")
    )
    roots = sorted(os.fspath(Path(value)) for value in values)
    if any(not Path(value).is_absolute() for value in roots):
        raise BulkloadError("catalog live roots must be absolute")
    return roots


def _catalog_source_roots(catalog: dict[str, Any]) -> list[str]:
    snapshot = catalog.get("snapshot")
    if not isinstance(snapshot, dict):
        return _catalog_roots(catalog)
    roots = sorted({item["snapshot"] for item in snapshot["roots"]})
    if any(not Path(value).is_absolute() for value in roots):
        raise BulkloadError("snapshot custody roots must be absolute")
    return roots


def _snapshot_source_path(catalog: dict[str, Any], path: Path) -> Path:
    snapshot = catalog.get("snapshot")
    if not isinstance(snapshot, dict):
        return path
    matches: list[tuple[int, Path, Path]] = []
    for binding in snapshot["roots"]:
        live = Path(binding["live"])
        try:
            relative = path.relative_to(live)
        except ValueError:
            continue
        matches.append((len(live.parts), Path(binding["snapshot"]), relative))
    if not matches:
        raise BulkloadError("AgentPlanV4 source path lacks snapshot custody")
    _, root, relative = max(matches, key=lambda item: item[0])
    return root / relative


def _object_path(stage_root: Path, digest: str) -> Path:
    return stage_root / "objects" / digest[:2] / digest


def _source_path(
    path: str | Path, mirror: Path | None, catalog: dict[str, Any]
) -> Path:
    source = Path(path)
    if not source.is_absolute():
        raise BulkloadError("AgentPlanV4 source path is not absolute")
    source = _snapshot_source_path(catalog, source)
    return source if mirror is None else mirror.joinpath(*source.parts[1:])


def _plan_source_paths(plan: dict[str, Any]) -> list[str]:
    paths: set[str] = set()
    resolver = PlanOperationResolver(plan)
    catalog = plan["source"]["catalog"]
    if isinstance(catalog.get("snapshot"), dict):
        paths.update(
            {
                catalog["snapshot"]["seal_path"],
                catalog["snapshot"]["index_path"],
            }
        )
    for compact_operation in plan["operations"]:
        operation = resolver.materialize(compact_operation)
        source = operation["source"]
        if operation["kind"] == "git-workspace-union":
            common = Path(source["common_git_dir"]) / "objects"
            paths.update(
                os.fspath(
                    _snapshot_source_path(catalog, common / item["relative_path"])
                )
                for item in source.get("object_files", [])
            )
            for worktree in source.get("worktrees", []):
                paths.update(
                    os.fspath(
                        _snapshot_source_path(
                            catalog, Path(worktree["path"]) / item["relative_path"]
                        )
                    )
                    for item in worktree.get("files", [])
                )
                if worktree["index"]["exists"]:
                    paths.add(
                        os.fspath(
                            _snapshot_source_path(
                                catalog, Path(worktree["index"]["path"])
                            )
                        )
                    )
            continue
        path = Path(operation["source_root"]) / source["relative_path"]
        paths.add(os.fspath(_snapshot_source_path(catalog, path)))
        if operation["kind"] == "sqlite-union" and catalog.get("snapshot") is None:
            paths.update(
                os.fspath(Path(os.fspath(path) + f"-{item['kind']}"))
                for item in source.get("sidecars", [])
            )
    result = []
    for raw in sorted(paths):
        path = Path(raw)
        if not path.is_absolute() or any(
            part in {"", ".", ".."} for part in path.parts
        ):
            raise BulkloadError("transport allowlist contains a non-canonical path")
        result.append(path.relative_to("/").as_posix())
    return result


def _transport_environment(ssh_path: str) -> dict[str, str]:
    environment = {
        key: value
        for key, value in os.environ.items()
        if not key.startswith("RSYNC_") and key not in {"SSH_ASKPASS", "GIT_ASKPASS"}
    }
    environment.update(
        {
            "LC_ALL": "C",
            "RSYNC_RSH": f"{ssh_path} {' '.join(SSH_OPTIONS)}",
        }
    )
    return environment


def _probe_remote_rsync(ssh_path: str, host: str, binding: dict[str, Any]) -> None:
    outputs = []
    for argument in ("--version", "--help"):
        result = subprocess.run(
            [ssh_path, *SSH_OPTIONS, "--", host, binding["path"], argument],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            env=_transport_environment(ssh_path),
        )
        if result.returncode != 0:
            raise BulkloadError("authenticated destination rsync feature probe failed")
        outputs.append(result.stdout)
    match = re.search(rb"protocol version (\d+)", outputs[0])
    if (
        match is None
        or int(match.group(1)) != binding["protocol"]
        or b"--from0" not in outputs[1]
        or b"--files-from" not in outputs[1]
        or b"--ignore-missing-args" not in outputs[1]
    ):
        raise BulkloadError("destination rsync differs from captured feature authority")


def _transport_host(value: str, expected: str) -> str:
    match = re.fullmatch(
        r"(?:(?:[a-z_][a-z0-9_-]{0,31})@)?([A-Za-z0-9](?:[A-Za-z0-9.-]{0,251}[A-Za-z0-9])?)",
        value,
    )
    if match is None or match.group(1) != expected:
        raise BulkloadError("SSH authority differs from captured hostname")
    return value


def _transport_ssh(ssh_binary: str | None) -> str:
    path = ssh_binary or shutil.which("ssh")
    if path is None:
        raise BulkloadError("an explicit safe SSH executable is unavailable")
    return shell_safe_executable(path, "SSH")


def _transport_body(
    plan_sha256: str,
    holds: list[dict[str, Any]],
    phase: str,
    stage_root: Path,
    transport: dict[str, Any],
    capacity: dict[str, Any],
) -> dict[str, Any]:
    manifest = {
        "created_at": utc_now(),
        "entries": [],
        "holds": holds,
        "phase": phase,
        "plan_sha256": plan_sha256,
        "stage_id": new_id(),
        "stage_root": os.fspath(stage_root),
    }
    seal(manifest, "manifest_sha256")
    receipt = {
        "capacity": capacity,
        "created_at": utc_now(),
        "manifest": manifest,
        "manifest_sha256": manifest["manifest_sha256"],
        "materialization": {},
        "phase": phase,
        "plan_sha256": plan_sha256,
        "ready_for_apply": False,
        "receipt_id": new_id(),
        "schema": AGENT_STAGE_SCHEMA,
        "stage_root": os.fspath(stage_root),
        "transport": transport,
    }
    return seal(receipt, "receipt_sha256")


def _allowlist_path(stage_root: Path, phase: str) -> Path:
    return stage_root / f".transport-allowlist-{phase}.nul"


def _snapshot_allowlist_stream(
    source: Any,
    snapshot: Any,
    *,
    expected_size: int,
    expected_sha256: str,
) -> None:
    info = os.fstat(source.fileno())
    if (
        not stat.S_ISREG(info.st_mode)
        or stat.S_IMODE(info.st_mode) != 0o600
        or info.st_size != expected_size
        or info.st_size > MAX_JSON_BYTES
    ):
        raise BulkloadError("transport allowlist custody is invalid")
    digest = hashlib.sha256()
    pending = b""
    previous: bytes | None = None
    while chunk := source.read(1024 * 1024):
        digest.update(chunk)
        snapshot.write(chunk)
        pending += chunk
        records = pending.split(b"\0")
        pending = records.pop()
        for record in records:
            if (
                not record
                or record.startswith(b"/")
                or any(part in {b"", b".", b".."} for part in record.split(b"/"))
                or previous is not None
                and record <= previous
            ):
                raise BulkloadError("transport allowlist is not canonical")
            previous = record
        if len(pending) > 1024 * 1024:
            raise BulkloadError("transport allowlist path exceeds the bounded contract")
    if pending:
        raise BulkloadError("transport allowlist is not NUL terminated")
    if digest.hexdigest() != expected_sha256:
        raise BulkloadError("transport allowlist digest mismatch")
    snapshot.flush()
    os.fsync(snapshot.fileno())
    snapshot.seek(0)


def _validate_prepare_receipt(
    plan: dict[str, Any],
    phase: str,
    stage_root: Path,
    receipt: dict[str, Any],
) -> None:
    validate_stage_receipt(receipt)
    payload = b"".join(os.fsencode(item) + b"\0" for item in _plan_source_paths(plan))
    source = plan["source"]["catalog"]
    destination = plan["destination"]["catalog"]
    if (
        receipt["plan_sha256"] != plan["plan_sha256"]
        or receipt["phase"] != phase
        or receipt["stage_root"] != os.fspath(stage_root)
        or receipt["ready_for_apply"]
        or receipt["manifest"]["entries"]
        or receipt["transport"]["mode"] != "destination-prepare"
        or receipt["transport"]["allowlist_sha256"] != sha256_bytes(payload)
        or receipt["transport"]["allowlist_size"] != len(payload)
        or receipt["transport"]["destination_host"]
        != destination["transport"]["hostname"]
        or receipt["transport"]["destination_rsync"]
        != destination["transport"]["rsync"]
        or receipt["transport"]["source_host"] != source["transport"]["hostname"]
        or receipt["transport"]["source_roots"] != _catalog_source_roots(source)
        or receipt["transport"]["source_rsync"] != source["transport"]["rsync"]
        or receipt["transport"]["source_snapshot"] != source.get("snapshot")
        or receipt["transport"]["transport_receipt_sha256"] is not None
    ):
        raise BulkloadError("prepare receipt is detached from the accepted stage plan")
    if receipt["transport"]["quarantine_root"] != os.fspath(
        stage_root / ".transport-quarantine"
    ):
        raise BulkloadError("prepare receipt quarantine binding is invalid")


TRANSPORT_MOVERS = ("rsync", "native")


def _allowlist_relatives(stream: IO[bytes]) -> Iterator[str]:
    """Stream NUL-separated allowlist entries without holding the list in RAM.

    The real allowlist is 403 MB / 1,781,044 paths (VERDICTS.md, judge §2A),
    so this is read in blocks for the same reason the plan is: the engine's
    memory ceiling is the thing that curated the codex root down to 22 G.
    """
    stream.seek(0)
    pending = b""
    while True:
        block = stream.read(1024 * 1024)
        if not block:
            break
        pending += block
        *complete, pending = pending.split(b"\0")
        for item in complete:
            yield os.fsdecode(item)
    if pending:
        raise BulkloadError("transport allowlist is not NUL-terminated")


def _push_native_transport(
    *,
    allowlist: IO[bytes],
    ssh_path: str,
    host: str,
    quarantine: Path,
    stage_root: Path,
    streams: int,
    environment: dict[str, str],
    channel_factory: Any | None,
) -> dict[str, Any]:
    """Move the payload as content-addressed objects over N ssh streams.

    This is the alternative to the single-stream rsync push below. It is not
    the default and it is not yet proven against a live destination; see
    `docs/design/native-mover.md`. It is custody-compatible by construction:
    the quarantine is re-derived by `validate_snapshot_custody` under
    `required_paths` (`executor.py:1533-1535`), which re-checks kind, mode,
    size and sha256 per allowlist path and disables the namespace perimeter
    (`scanner.py:2978-2986`) — none of the `rsync -a` metadata this mover
    drops (mtimes, hardlink topology, xattrs) is load-bearing there.
    """
    with tempfile.TemporaryDirectory(prefix="bulkload-mover-spill-") as spill:
        if channel_factory is None:
            remote_module = os.fspath(stage_root / ".transport-mover.py")
            expected = mover.bootstrap_receiver(
                ssh_path, SSH_OPTIONS, host, remote_module, env=environment
            )
            factory = mover.ssh_channel_factory(
                ssh_path,
                SSH_OPTIONS,
                host,
                mover.receiver_argv("python3", remote_module, os.fspath(quarantine)),
                env=environment,
            )
        else:
            expected, factory = channel_factory
        try:
            summary = mover.push(
                source_root=Path("/"),
                objects=mover.plan_objects(
                    Path("/"),
                    _allowlist_relatives(allowlist),
                    spill_dir=Path(spill),
                ),
                channel_factory=factory,
                streams=streams,
                expected_receiver_sha256=expected,
                heartbeat=lambda beat: print(
                    f"bulkload-mover stream={beat.stream} phase={beat.phase} "
                    f"objects={beat.objects_done} skipped={beat.objects_skipped} "
                    f"bytes={beat.bytes_sent}",
                    file=sys.stderr,
                    flush=True,
                ),
            )
        except mover.MoverError as error:
            raise BulkloadError(f"native quarantine push failed: {error}") from error
    # One closing line, unconditionally. `logs/preseed-push.log` is 0 bytes for
    # a 2h32m / 84 GiB rsync push (VERDICTS.md, judge §1); a transport that
    # says nothing about what it moved is the loudest AX complaint in the
    # review, and this is the cheapest place to stop repeating it.
    print(
        f"bulkload-mover done objects={summary.objects} "
        f"sent={summary.objects_sent} skipped={summary.objects_skipped} "
        f"paths={summary.paths} bytes={summary.bytes_sent} "
        f"deduplicated={summary.bytes_deduplicated} "
        f"streams={summary.streams} seconds={summary.seconds:.1f}",
        file=sys.stderr,
        flush=True,
    )
    return {
        "objects": summary.objects,
        "objects_sent": summary.objects_sent,
        "objects_skipped": summary.objects_skipped,
        "paths": summary.paths,
        "bytes_sent": summary.bytes_sent,
        "bytes_deduplicated": summary.bytes_deduplicated,
        "streams": summary.streams,
    }


def push_agent_transport(
    prepare_receipt: dict[str, Any],
    allowlist_path: Path,
    *,
    accepted_plan_sha256: str,
    phase: str,
    stage_root: Path,
    destination_ssh_host: str,
    transport_checksum: bool = False,
    transport_mover: str = "rsync",
    transport_streams: int = mover.DEFAULT_STREAMS,
    _ssh_binary: str | None = None,
    _channel_factory: Any | None = None,
) -> dict[str, Any]:
    if transport_mover not in TRANSPORT_MOVERS:
        raise BulkloadError("transport mover is unsupported")
    if transport_mover == "rsync" and _channel_factory is not None:
        raise BulkloadError(
            "transport channel factory applies only to the native mover"
        )
    if transport_checksum and transport_mover != "rsync":
        raise BulkloadError("transport checksum applies only to the rsync mover")
    validate_stage_receipt(prepare_receipt)
    stage_root = _remote_safe_stage_root(stage_root)
    transport_authority = prepare_receipt["transport"]
    if (
        prepare_receipt["plan_sha256"] != accepted_plan_sha256
        or prepare_receipt["phase"] != phase
        or prepare_receipt["stage_root"] != os.fspath(stage_root)
        or prepare_receipt["ready_for_apply"]
        or prepare_receipt["manifest"]["entries"]
        or transport_authority["mode"] != "destination-prepare"
        or transport_authority["transport_receipt_sha256"] is not None
        or transport_authority["quarantine_root"]
        != os.fspath(stage_root / ".transport-quarantine")
    ):
        raise BulkloadError("prepare receipt is detached from the accepted push")
    assert_no_overlap(
        stage_root,
        [Path(value) for value in transport_authority["source_roots"]],
        "stage root",
    )
    if socket.gethostname() != transport_authority["source_host"]:
        raise BulkloadError("transport push must run on the captured source host")
    source_snapshot = transport_authority["source_snapshot"]
    if isinstance(source_snapshot, dict):
        validate_snapshot_custody(source_snapshot)
        if phase == "final":
            # One pass here, and only here. This fence is strictly dominated
            # by the full-strength fence below, which re-runs over the same
            # snapshot under the same `phase == "final"` condition after the
            # push. Nothing in between confers authority: the payload lands in
            # the destination *quarantine*, and the transport receipt that
            # grants downstream authority is built after the second fence. A
            # straggler that hides behind this pass's walk cursor is still
            # caught there before anything can act on it.
            validate_live_snapshot_generation(source_snapshot, passes=1)
    host = _transport_host(
        destination_ssh_host, transport_authority["destination_host"]
    )
    ssh_path = _transport_ssh(_ssh_binary)
    source_binding = inspect_rsync(transport_authority["source_rsync"]["path"])
    if source_binding != transport_authority["source_rsync"]:
        raise BulkloadError("source rsync differs from captured authority")
    _probe_remote_rsync(ssh_path, host, transport_authority["destination_rsync"])
    quarantine = Path(prepare_receipt["transport"]["quarantine_root"])
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0)
    descriptor = os.open(allowlist_path, flags)
    with (
        os.fdopen(descriptor, "rb", buffering=0, closefd=True) as allowlist,
        tempfile.TemporaryFile(prefix="bulkload-allowlist-") as snapshot,
    ):
        os.fchmod(snapshot.fileno(), 0o600)
        _snapshot_allowlist_stream(
            allowlist,
            snapshot,
            expected_size=transport_authority["allowlist_size"],
            expected_sha256=transport_authority["allowlist_sha256"],
        )
        # The payload push does not carry the custody proof. Every transported
        # byte is re-derived on the destination before it can gain apply
        # authority: validate_snapshot_custody re-hashes the transported index
        # and every required payload out of the quarantine mirror, reflink_clone
        # and _publish_object re-verify each staged object, and agent-verify
        # re-hashes the whole sealed stage. rsync's own whole-file checksum pass
        # is therefore a second full read of the corpus that proves nothing the
        # destination does not prove independently. --transport-checksum
        # restores it for an operator who wants the transport to fail earlier.
        if transport_mover == "native":
            _push_native_transport(
                allowlist=snapshot,
                ssh_path=ssh_path,
                host=host,
                quarantine=quarantine,
                stage_root=stage_root,
                streams=transport_streams,
                environment=_transport_environment(ssh_path),
                channel_factory=_channel_factory,
            )
            result = None
        else:
            result = subprocess.run(
                [
                    source_binding["path"],
                    "-a",
                    "--from0",
                    "--files-from=-",
                    *(("--checksum",) if transport_checksum else ()),
                    "--delay-updates",
                    "--ignore-missing-args",
                    "--no-devices",
                    "--no-specials",
                    f"--rsync-path={transport_authority['destination_rsync']['path']}",
                    "/",
                    f"{host}:{quarantine}/",
                ],
                stdin=snapshot,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
                env=_transport_environment(ssh_path),
            )
    if result is not None and result.returncode != 0:
        raise BulkloadError("authenticated rsync quarantine push failed")
    if isinstance(source_snapshot, dict) and phase == "final":
        validate_live_snapshot_generation(source_snapshot)
    transport = dict(transport_authority)
    transport["mode"] = (
        "ssh-native-push" if transport_mover == "native" else "ssh-rsync-push"
    )
    transport["source_rsync"] = source_binding
    transport["transport_receipt_sha256"] = prepare_receipt["receipt_sha256"]
    receipt = _transport_body(
        accepted_plan_sha256,
        prepare_receipt["manifest"]["holds"],
        phase,
        stage_root,
        transport,
        prepare_receipt["capacity"],
    )
    with tempfile.TemporaryDirectory(prefix="bulkload-transport-") as temporary:
        local_receipt = Path(temporary) / f"transport-receipt-{phase}.json"
        descriptor = os.open(
            local_receipt,
            os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_CLOEXEC", 0),
            0o600,
        )
        with os.fdopen(descriptor, "wb", closefd=True) as stream:
            stream.write(canonical_bytes(receipt) + b"\n")
            stream.flush()
            os.fsync(stream.fileno())
        receipt_result = subprocess.run(
            [
                source_binding["path"],
                "-a",
                "--checksum",
                "--delay-updates",
                "--no-devices",
                "--no-specials",
                f"--rsync-path={transport_authority['destination_rsync']['path']}",
                os.fspath(local_receipt),
                f"{host}:{stage_root}/.transport-receipt-{phase}.json",
            ],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            env=_transport_environment(ssh_path),
        )
    if receipt_result.returncode != 0:
        raise BulkloadError("authenticated transport-receipt push failed")
    return receipt


def _materialized_transport(
    plan: dict[str, Any],
    phase: str,
    stage_root: Path,
    prepare_receipt: dict[str, Any],
    transport_receipt: dict[str, Any],
) -> tuple[Path, dict[str, Any]]:
    _validate_prepare_receipt(plan, phase, stage_root, prepare_receipt)
    validate_stage_receipt(transport_receipt)
    expected_allowlist = b"".join(
        os.fsencode(item) + b"\0" for item in _plan_source_paths(plan)
    )
    expected_transport = dict(prepare_receipt["transport"])
    expected_transport["transport_receipt_sha256"] = prepare_receipt["receipt_sha256"]
    # One receipt shape, two movers. Which one moved the bytes is recorded and
    # bound, but it is not otherwise load-bearing here: the quarantine is
    # re-derived from the sealed index either way a few lines below, at
    # `validate_snapshot_custody(..., mirror=source_mirror)`.
    accepted_transports = [
        {**expected_transport, "mode": name}
        for name in ("ssh-rsync-push", "ssh-native-push")
    ]
    if (
        transport_receipt["plan_sha256"] != plan["plan_sha256"]
        or transport_receipt["phase"] != phase
        or transport_receipt["stage_root"] != os.fspath(stage_root)
        or transport_receipt["ready_for_apply"]
        or transport_receipt["manifest"]["entries"]
        or transport_receipt["transport"] not in accepted_transports
        or transport_receipt["transport"]["allowlist_sha256"]
        != sha256_bytes(expected_allowlist)
        or transport_receipt["transport"]["allowlist_size"] != len(expected_allowlist)
    ):
        raise BulkloadError(
            "transport receipt is detached from the accepted stage plan"
        )
    quarantine = stage_root / ".transport-quarantine"
    if transport_receipt["transport"]["quarantine_root"] != os.fspath(quarantine):
        raise BulkloadError("transport receipt quarantine binding is invalid")
    for path in (stage_root, quarantine):
        info = path.stat(follow_symlinks=False)
        if not stat.S_ISDIR(info.st_mode) or stat.S_IMODE(info.st_mode) != 0o700:
            raise BulkloadError("prepared stage/quarantine custody mode is invalid")
    transport = dict(transport_receipt["transport"])
    transport["mode"] = "ssh-rsync-quarantine"
    transport["transport_receipt_sha256"] = transport_receipt["receipt_sha256"]
    return quarantine, transport


def _verify_record(
    path: Path, record: dict[str, Any], *, translated: bool = False
) -> None:
    expected_kind = record.get("kind", "regular")
    try:
        info = path.stat(follow_symlinks=False)
    except FileNotFoundError as error:
        raise _StageSourceChanged(
            f"required state disappeared before staging: {path}"
        ) from error
    if expected_kind == "regular":
        if not stat.S_ISREG(info.st_mode):
            raise _StageSourceChanged(f"state type changed before staging: {path}")
        expected_sha = record.get("sha256")
        if sha256_file(path) != expected_sha or info.st_size != record.get("size"):
            raise _StageSourceChanged(f"state bytes changed before staging: {path}")
    elif expected_kind == "symlink":
        if not stat.S_ISLNK(info.st_mode) or sha256_symlink(path) != record.get(
            "sha256"
        ):
            raise _StageSourceChanged(f"symbolic link changed before staging: {path}")
    elif expected_kind == "directory":
        if not stat.S_ISDIR(info.st_mode):
            raise _StageSourceChanged(f"directory changed before staging: {path}")
    else:
        raise _StageSourceChanged("unsupported staged entry kind")
    # Symlink permission bits are not portable across kernels: darwin lstat
    # reports the creating umask's bits while Linux fixes every symlink at 0777
    # and has no lchmod. The custody verifier already exempts symlink mode
    # (c81de89); a quarantined symlink's identity is its target bytes, checked
    # above via sha256_symlink. The record must still *declare* a mode for
    # every kind, so a mode-less record cannot slip past this gate and reach
    # _materialize_file's record["mode"] as a KeyError instead of a refusal.
    recorded_mode = record.get("mode")
    if recorded_mode is None or (
        expected_kind != "symlink"
        and f"{stat.S_IMODE(info.st_mode):04o}" != recorded_mode
    ):
        raise _StageSourceChanged(f"state mode changed before staging: {path}")


def _rewrite_payload(source: Path, path_map: list[dict[str, str]]) -> bytes:
    try:
        payload = source.read_bytes()
    except OSError as error:
        raise BulkloadError(f"cannot read path-rewritten state {source}") from error
    result = payload
    for entry in sorted(path_map, key=lambda item: len(item["source"]), reverse=True):
        result = result.replace(
            os.fsencode(entry["source"]), os.fsencode(entry["destination"])
        )
    # Validate structured JSON when the suffix promises it; other Claude
    # state (for example Markdown commands) must still be portable UTF-8.
    try:
        if source.suffix.lower() == ".jsonl":
            for line in result.splitlines():
                if line:
                    json.loads(line)
        elif source.suffix.lower() == ".json":
            json.loads(result)
        else:
            result.decode("utf-8", errors="strict")
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise BulkloadError("path rewriting produced invalid provider JSON") from error
    return result


def _publish_object(temporary: Path, target: Path, digest: str, size: int) -> bool:
    """Publish by an O_EXCL hard-link; never replace an existing object."""
    try:
        os.link(temporary, target, follow_symlinks=False)
        fsync_directory(target.parent)
        created = True
    except FileExistsError:
        created = False
    finally:
        temporary.unlink(missing_ok=True)
    try:
        info = target.stat(follow_symlinks=False)
    except FileNotFoundError as error:
        raise BulkloadError("content-addressed object publication failed") from error
    if (
        not stat.S_ISREG(info.st_mode)
        or info.st_size != size
        or sha256_file(target) != digest
    ):
        raise BulkloadError("stage content-addressed object is corrupt")
    return created


def _object_temporary(target: Path) -> Path:
    durable_makedirs(target.parent)
    return target.parent / f".{target.name}.ingest-{new_id()}"


def _write_object_bytes(
    stage_root: Path, digest: str, payload: bytes, mode: int
) -> Path:
    target = _object_path(stage_root, digest)
    if target.exists():
        if sha256_file(target) != digest:
            raise BulkloadError("stage content-addressed object is corrupt")
        return target
    temporary = _object_temporary(target)
    atomic_write(temporary, payload, mode=mode)
    _publish_object(temporary, target, digest, len(payload))
    return target


def _materialize_file(
    *,
    stage_root: Path,
    source_path: Path,
    record: dict[str, Any],
    destination_candidate: Path | None,
    path_map: list[dict[str, str]],
    transform: str | None,
    allow_accounted_copy: bool,
    stats: dict[str, int],
) -> dict[str, Any]:
    _verify_record(source_path, record)
    kind = record.get("kind", "regular")
    mode = int(record.get("mode", "0600"), 8)
    if kind == "directory":
        return {"blob_sha256": None, "kind": kind, "mode": record["mode"], "size": 0}
    if kind == "symlink":
        payload = os.fsencode(os.readlink(source_path))
        digest = sha256_bytes(payload)
        if digest != record["sha256"]:
            raise _StageSourceChanged("symbolic-link payload changed before staging")
        _write_object_bytes(stage_root, digest, payload, 0o600)
        stats["metadata_bytes"] += len(payload)
        return {
            "blob_sha256": digest,
            "kind": kind,
            "mode": record["mode"],
            "size": len(payload),
        }

    if transform == "path-rewrite":
        if not allow_accounted_copy:
            raise BulkloadError(
                "path rewriting requires explicit capacity-accounted materialization"
            )
        payload = _rewrite_payload(source_path, path_map)
        digest = sha256_bytes(payload)
        expected = record.get("translated_sha256")
        if digest != expected:
            raise _StageSourceChanged(
                "path-rewritten state digest differs from the plan"
            )
        _write_object_bytes(stage_root, digest, payload, mode)
        stats["accounted_copy_bytes"] += len(payload)
        return {
            "blob_sha256": digest,
            "kind": kind,
            "mode": record["mode"],
            "size": len(payload),
        }

    digest = record["sha256"]
    target = _object_path(stage_root, digest)
    if target.exists():
        if sha256_file(target) != digest:
            raise BulkloadError("stage content-addressed object is corrupt")
        stats["reused_object_bytes"] += record["size"]
        return {
            "blob_sha256": digest,
            "kind": kind,
            "mode": record["mode"],
            "size": record["size"],
        }
    if destination_candidate is not None and destination_candidate.exists():
        info = destination_candidate.stat(follow_symlinks=False)
        if (
            stat.S_ISREG(info.st_mode)
            and info.st_size == record["size"]
            and (sha256_file(destination_candidate) == digest)
        ):
            temporary = _object_temporary(target)
            reflink_clone(
                destination_candidate,
                temporary,
                expected_sha256=digest,
                mode=mode,
            )
            _publish_object(temporary, target, digest, record["size"])
            stats["destination_reflink_bytes"] += record["size"]
            return {
                "blob_sha256": digest,
                "kind": kind,
                "mode": record["mode"],
                "size": record["size"],
            }
    temporary = _object_temporary(target)
    try:
        reflink_clone(source_path, temporary, expected_sha256=digest, mode=mode)
        _publish_object(temporary, target, digest, record["size"])
        stats["source_reflink_bytes"] += record["size"]
    except BulkloadError as clone_error:
        temporary.unlink(missing_ok=True)
        try:
            _verify_record(source_path, record)
        except _StageSourceChanged:
            raise
        if not allow_accounted_copy:
            raise clone_error
        temporary = _object_temporary(target)
        try:
            accounted_copy(
                source_path,
                temporary,
                expected_sha256=digest,
                mode=mode,
            )
            _publish_object(temporary, target, digest, record["size"])
        except BulkloadError as copy_error:
            temporary.unlink(missing_ok=True)
            try:
                _verify_record(source_path, record)
            except _StageSourceChanged:
                raise
            raise copy_error
        stats["accounted_copy_bytes"] += record["size"]
    return {
        "blob_sha256": digest,
        "kind": kind,
        "mode": record["mode"],
        "size": record["size"],
    }


def _typed_row(values: Sequence[Any]) -> list[list[Any]]:
    return [_typed_sql_value(value) for value in values]


def _row_key(values: Sequence[Any], columns: list[str], primary_key: list[str]) -> str:
    typed = _typed_row(values)
    if primary_key:
        key = [typed[columns.index(column)] for column in primary_key]
        return sha256_bytes(canonical_bytes(key))
    return sha256_bytes(canonical_bytes(typed))


def _row_digest(values: Sequence[Any]) -> str:
    return sha256_bytes(canonical_bytes(_typed_row(values)))


def _sqlite_union_is_safe(
    source_record: dict[str, Any], destination_record: dict[str, Any]
) -> bool:
    source = source_record["logical"]
    destination = destination_record["logical"]
    if any(
        source[key] != destination[key]
        for key in ("application_id", "schema_sha256", "user_version")
    ):
        return False
    destination_tables = {table["name"]: table for table in destination["tables"]}
    for source_table in source["tables"]:
        destination_table = destination_tables.get(source_table["name"])
        if destination_table is None:
            return False
        for table in (source_table, destination_table):
            keys = [row["key_sha256"] for row in table["rows"]]
            if table["primary_key"] and len(keys) != len(set(keys)):
                return False
        destination_rows = {
            row["key_sha256"]: row["row_sha256"] for row in destination_table["rows"]
        }
        for row in source_table["rows"]:
            observed = destination_rows.get(row["key_sha256"])
            if observed is not None and observed != row["row_sha256"]:
                return False
    return True


def _compose_sqlite(
    source_snapshot: Path,
    destination_snapshot: Path | None,
    output: Path,
    source_record: dict[str, Any],
    destination_record: dict[str, Any] | None,
) -> dict[str, Any]:
    if destination_snapshot is None or destination_record is None:
        source_authoritative = True
    else:
        source_authoritative = not _sqlite_union_is_safe(
            source_record, destination_record
        )
    if source_authoritative:
        reflink_clone(
            source_snapshot,
            output,
            expected_sha256=sha256_file(source_snapshot),
            mode=0o600,
        )
        return _sqlite_catalog_from_snapshot(output, max_rows=DEFAULT_MAX_SQLITE_ROWS)
    reflink_clone(
        destination_snapshot,
        output,
        expected_sha256=sha256_file(destination_snapshot),
        mode=0o600,
    )
    source_connection = sqlite3.connect(
        f"file:{source_snapshot.as_posix()}?mode=ro", uri=True
    )
    output_connection = sqlite3.connect(output)
    try:
        output_connection.execute("PRAGMA foreign_keys=OFF")
        output_connection.execute("BEGIN IMMEDIATE")
        for table in source_record["logical"]["tables"]:
            name = table["name"]
            columns = table["columns"]
            primary_key = table["primary_key"]
            quoted = _quote_identifier(name)
            destination_rows: dict[str, str] = {}
            destination_counts: dict[str, int] = defaultdict(int)
            for values in output_connection.execute(f"SELECT * FROM {quoted}"):
                digest = _row_digest(values)
                if primary_key:
                    destination_rows[_row_key(values, columns, primary_key)] = digest
                else:
                    destination_counts[digest] += 1
            placeholders = ",".join("?" for _ in columns)
            column_list = ",".join(_quote_identifier(column) for column in columns)
            insert = f"INSERT INTO {quoted} ({column_list}) VALUES ({placeholders})"
            source_counts: dict[str, int] = defaultdict(int)
            for values in source_connection.execute(f"SELECT * FROM {quoted}"):
                key = _row_key(values, columns, primary_key)
                digest = _row_digest(values)
                if not primary_key:
                    source_counts[digest] += 1
                    if source_counts[digest] > destination_counts[digest]:
                        output_connection.execute(insert, values)
                    continue
                observed = destination_rows.get(key)
                if observed is not None and observed != digest:
                    raise BulkloadError("SQLite shared row diverged after planning")
                if observed is None:
                    output_connection.execute(insert, values)
                    destination_rows[key] = digest
        output_connection.commit()
        if output_connection.execute("PRAGMA quick_check").fetchone() != ("ok",):
            raise BulkloadError("composed SQLite quick_check failed")
        if output_connection.execute("PRAGMA foreign_key_check").fetchone() is not None:
            raise BulkloadError("composed SQLite foreign-key check failed")
    except BaseException:
        output_connection.rollback()
        raise
    finally:
        output_connection.close()
        source_connection.close()
    return _sqlite_catalog_from_snapshot(output, max_rows=DEFAULT_MAX_SQLITE_ROWS)


def _sqlite_stage_entry(
    operation: dict[str, Any],
    *,
    stage_root: Path,
    source_mirror: Path | None,
    source_catalog: dict[str, Any],
    stats: dict[str, int],
) -> dict[str, Any]:
    temporary_root = stage_root / ".sqlite-work"
    durable_makedirs(temporary_root)
    source_path = _source_path(
        Path(operation["source_root"]) / operation["source"]["relative_path"],
        source_mirror,
        source_catalog,
    )
    source_snapshot = temporary_root / f"source-{operation['operation_id']}.sqlite"
    source_snapshot.unlink(missing_ok=True)
    source_catalog = snapshot_sqlite(
        source_path, source_snapshot, max_rows=DEFAULT_MAX_SQLITE_ROWS
    )
    if (
        source_catalog["logical_sha256"]
        != operation["source"]["logical"]["logical_sha256"]
    ):
        raise BulkloadError("source SQLite state changed after planning")
    destination_before = operation.get("destination_before")
    destination_snapshot: Path | None = None
    if destination_before is not None:
        destination_path = Path(operation["destination_path"])
        destination_snapshot = (
            temporary_root / f"destination-{operation['operation_id']}.sqlite"
        )
        destination_snapshot.unlink(missing_ok=True)
        destination_catalog = snapshot_sqlite(
            destination_path,
            destination_snapshot,
            max_rows=DEFAULT_MAX_SQLITE_ROWS,
        )
        if (
            destination_catalog["logical_sha256"]
            != destination_before["logical"]["logical_sha256"]
        ):
            raise BulkloadError("destination SQLite state changed after planning")
    composed = temporary_root / f"composed-{operation['operation_id']}.sqlite"
    composed.unlink(missing_ok=True)
    logical = _compose_sqlite(
        source_snapshot,
        destination_snapshot,
        composed,
        operation["source"],
        destination_before,
    )
    digest = sha256_file(composed)
    target = _object_path(stage_root, digest)
    if not target.exists():
        temporary = _object_temporary(target)
        reflink_clone(composed, temporary, expected_sha256=digest, mode=0o600)
        _publish_object(temporary, target, digest, composed.stat().st_size)
        stats["sqlite_compose_bytes"] += composed.stat().st_size
    source_snapshot.unlink(missing_ok=True)
    if destination_snapshot is not None:
        destination_snapshot.unlink(missing_ok=True)
    composed.unlink(missing_ok=True)
    return {
        "blob_sha256": digest,
        "destination_before": destination_before,
        "destination_path": operation["destination_path"],
        "expected_logical": logical,
        "kind": "sqlite",
        "mode": operation["source"]["mode"],
        "operation_id": operation["operation_id"],
        "owner": operation["owner"],
        "size": target.stat().st_size,
    }


def _stage_file_operation(
    operation: dict[str, Any],
    *,
    stage_root: Path,
    plan: dict[str, Any],
    source_mirror: Path | None,
    allow_accounted_copy: bool,
    stats: dict[str, int],
) -> dict[str, Any]:
    source_path = _source_path(
        Path(operation["source_root"]) / operation["source"]["relative_path"],
        source_mirror,
        plan["source"]["catalog"],
    )
    material = _materialize_file(
        stage_root=stage_root,
        source_path=source_path,
        record=operation["source"],
        destination_candidate=Path(operation["destination_path"]),
        path_map=plan["path_map"],
        transform=operation.get("transform"),
        allow_accounted_copy=allow_accounted_copy,
        stats=stats,
    )
    return {
        **material,
        "destination_before": operation.get("destination_before"),
        "destination_path": operation["destination_path"],
        "operation_id": operation["operation_id"],
        "owner": operation["owner"],
    }


def _stage_git_operation(
    operation: dict[str, Any],
    *,
    stage_root: Path,
    plan: dict[str, Any],
    source_mirror: Path | None,
    allow_accounted_copy: bool,
    stats: dict[str, int],
) -> dict[str, Any]:
    source = operation["source"]
    destination_before = operation.get("destination_before")
    destination_common = (
        Path(destination_before["common_git_dir"])
        if destination_before is not None
        else (
            Path(operation["destination_path"]) / ".git"
            if source.get("worktrees")
            else Path(operation["destination_path"])
        )
    )
    objects: list[dict[str, Any]] = []
    for record in source.get("object_files", []):
        source_path = _source_path(
            Path(source["common_git_dir"]) / "objects" / record["relative_path"],
            source_mirror,
            plan["source"]["catalog"],
        )
        target = destination_common / "objects" / record["relative_path"]
        material = _materialize_file(
            stage_root=stage_root,
            source_path=source_path,
            record=record,
            destination_candidate=target,
            path_map=plan["path_map"],
            transform=None,
            allow_accounted_copy=allow_accounted_copy,
            stats=stats,
        )
        objects.append({**material, "relative_path": record["relative_path"]})
    worktrees: list[dict[str, Any]] = []
    destination_worktrees = {
        worktree["path"]: worktree
        for worktree in (destination_before or {}).get("worktrees", [])
    }
    for source_worktree in source.get("worktrees", []):
        destination_path = Path(source_worktree["destination_path"])
        destination_worktree = destination_worktrees.get(os.fspath(destination_path))
        destination_files = {
            item["relative_path"]: item
            for item in (destination_worktree or {}).get("files", [])
        }
        files: list[dict[str, Any]] = []
        source_file_names: set[str] = set()
        for record in source_worktree.get("files", []):
            source_file_names.add(record["relative_path"])
            source_path = _source_path(
                Path(source_worktree["path"]) / record["relative_path"],
                source_mirror,
                plan["source"]["catalog"],
            )
            target = destination_path / record["relative_path"]
            material = _materialize_file(
                stage_root=stage_root,
                source_path=source_path,
                record=record,
                destination_candidate=target,
                path_map=plan["path_map"],
                transform=None,
                allow_accounted_copy=allow_accounted_copy,
                stats=stats,
            )
            files.append(
                {
                    **material,
                    "destination_before": destination_files.get(
                        record["relative_path"]
                    ),
                    "destination_path": os.fspath(target),
                    "relative_path": record["relative_path"],
                }
            )
        deletions = [
            os.fspath(destination_path / relative)
            for relative in destination_files
            if relative not in source_file_names
        ]
        index = source_worktree["index"]
        destination_index = (destination_worktree or {}).get("index")
        destination_index_before = None
        if destination_index and destination_index.get("exists"):
            destination_index_before = {
                "kind": "regular",
                "mode": destination_index["mode"],
                "sha256": destination_index["sha256"],
                "size": destination_index["size"],
            }
        staged_index = None
        if index["exists"]:
            material = _materialize_file(
                stage_root=stage_root,
                source_path=_source_path(
                    index["path"], source_mirror, plan["source"]["catalog"]
                ),
                record={
                    "kind": "regular",
                    "mode": index["mode"],
                    "sha256": index["sha256"],
                    "size": index["size"],
                },
                destination_candidate=(
                    Path(destination_worktree["index"]["path"])
                    if destination_worktree and destination_worktree["index"]["exists"]
                    else None
                ),
                path_map=plan["path_map"],
                transform=None,
                allow_accounted_copy=allow_accounted_copy,
                stats=stats,
            )
            staged_index = {
                **material,
                "destination_before": destination_index_before,
                "expected": index,
            }
        worktrees.append(
            {
                "branch": source_worktree["branch"],
                "deletions": sorted(deletions),
                "detached": source_worktree["detached"],
                "files": files,
                "head": source_worktree["head"],
                "index": staged_index,
                "index_destination_before": destination_index_before,
                "index_destination_path": destination_index.get("path")
                if destination_index and destination_index.get("exists")
                else None,
                "index_expected_exists": index["exists"],
                "locked": bool(source_worktree.get("locked")),
                "path": os.fspath(destination_path),
            }
        )
    return {
        "destination_before": destination_before,
        "destination_common_git_dir": os.fspath(destination_common),
        "destination_path": operation["destination_path"],
        "kind": "git-workspace",
        "object_format": source["object_format"],
        "objects": objects,
        "operation_id": operation["operation_id"],
        "ref_actions": operation["ref_actions"],
        "worktrees": worktrees,
    }


def _stage_receipt_path(stage_root: Path, phase: str) -> Path:
    return stage_root / f"receipt-{phase}.json"


def validate_stage_receipt(
    value: dict[str, Any], *, require_final: bool = False
) -> None:
    require_exact_keys(
        value,
        {
            "capacity",
            "created_at",
            "manifest",
            "manifest_sha256",
            "materialization",
            "phase",
            "plan_sha256",
            "ready_for_apply",
            "receipt_id",
            "receipt_sha256",
            "schema",
            "stage_root",
            "transport",
        },
        "AgentStageV4 receipt",
    )
    if value.get("schema") != AGENT_STAGE_SCHEMA:
        raise BulkloadError("input is not an AgentStageV4 receipt")
    require_digest(value, "receipt_sha256")
    if value.get("phase") not in {"preseed", "final"}:
        raise BulkloadError("AgentStageV4 phase is invalid")
    manifest = value.get("manifest")
    if not isinstance(manifest, dict):
        raise BulkloadError("AgentStageV4 lacks its embedded sealed manifest")
    require_digest(manifest, "manifest_sha256")
    require_exact_keys(
        manifest,
        {
            "created_at",
            "entries",
            "holds",
            "manifest_sha256",
            "phase",
            "plan_sha256",
            "stage_id",
            "stage_root",
        },
        "AgentStageV4 embedded manifest",
    )
    if value.get("manifest_sha256") != manifest["manifest_sha256"]:
        raise BulkloadError("AgentStageV4 manifest binding is invalid")
    if (
        manifest.get("phase") != value["phase"]
        or manifest.get("plan_sha256") != value.get("plan_sha256")
        or manifest.get("stage_root") != value.get("stage_root")
        or not isinstance(manifest.get("entries"), list)
    ):
        raise BulkloadError("AgentStageV4 embedded manifest contract is invalid")
    if require_final and (
        value["phase"] != "final" or value.get("ready_for_apply") is not True
    ):
        raise BulkloadError("final sealed AgentStageV4 receipt is required")
    require_exact_keys(
        value["transport"],
        {
            "allowlist_sha256",
            "allowlist_size",
            "destination_host",
            "destination_rsync",
            "mode",
            "quarantine_root",
            "source_host",
            "source_roots",
            "source_rsync",
            "source_snapshot",
            "transport_receipt_sha256",
        },
        "AgentStageV4 transport",
    )
    if value["transport"]["mode"] not in {
        "local",
        "destination-prepare",
        "ssh-native-push",
        "ssh-rsync-push",
        "ssh-rsync-quarantine",
    }:
        raise BulkloadError("AgentStageV4 transport mode is invalid")
    transport = value["transport"]
    source_snapshot = transport["source_snapshot"]
    if source_snapshot is not None:
        if not isinstance(source_snapshot, dict):
            raise BulkloadError("AgentStageV4 source snapshot is invalid")
        require_digest(source_snapshot, "seal_sha256")
    if (
        not isinstance(transport["allowlist_size"], int)
        or isinstance(transport["allowlist_size"], bool)
        or not 0 <= transport["allowlist_size"] <= MAX_JSON_BYTES
        or not isinstance(transport["allowlist_sha256"], str)
        or not re.fullmatch(r"[0-9a-f]{64}", transport["allowlist_sha256"])
        or not isinstance(transport["source_host"], str)
        or not transport["source_host"]
        or not isinstance(transport["destination_host"], str)
        or not transport["destination_host"]
        or not isinstance(transport["source_roots"], list)
        or any(
            not isinstance(root, str) or not Path(root).is_absolute()
            for root in transport["source_roots"]
        )
        or transport["source_roots"] != sorted(set(transport["source_roots"]))
    ):
        raise BulkloadError("AgentStageV4 transport authority is invalid")


def _verify_stage_objects(manifest: dict[str, Any], stage_root: Path) -> None:
    blobs: dict[str, int] = {}

    def add(entry: dict[str, Any]) -> None:
        digest = entry.get("blob_sha256")
        if digest is not None:
            blobs[digest] = int(entry["size"])

    for entry in manifest["entries"]:
        if entry["kind"] in {"file", "sqlite"}:
            add(entry)
        elif entry["kind"] == "git-workspace":
            for item in entry["objects"]:
                add(item)
            for worktree in entry["worktrees"]:
                for item in worktree["files"]:
                    add(item)
                if worktree["index"] is not None:
                    add(worktree["index"])
        else:
            raise BulkloadError("stage manifest contains an unknown entry kind")
    for digest, size in sorted(blobs.items()):
        path = _object_path(stage_root, digest)
        try:
            info = path.stat(follow_symlinks=False)
        except FileNotFoundError as error:
            raise BulkloadError("sealed stage object is missing") from error
        if (
            not stat.S_ISREG(info.st_mode)
            or info.st_size != size
            or sha256_file(path) != digest
        ):
            raise BulkloadError("sealed stage object failed exact verification")


def stage_agent_plan(
    plan: dict[str, Any],
    *,
    accepted_plan_sha256: str,
    phase: str,
    stage_root: Path,
    allow_accounted_copy: bool,
    reserve_bytes: int = DEFAULT_CAPACITY_RESERVE_BYTES,
    transport_mode: str = "local",
    destination_ssh_host: str | None = None,
    prepare_receipt: dict[str, Any] | None = None,
    transport_receipt: dict[str, Any] | None = None,
) -> dict[str, Any]:
    validate_agent_plan(plan, require_ready=True)
    if accepted_plan_sha256 != plan["plan_sha256"]:
        raise BulkloadError("accepted plan digest does not match AgentPlanV4")
    if phase not in {"preseed", "final"}:
        raise BulkloadError("agent-stage phase must be preseed or final")
    if transport_mode not in {"local", "prepare", "materialize"}:
        raise BulkloadError("agent-stage transport mode is invalid")
    stage_root = _remote_safe_stage_root(stage_root)
    assert_no_overlap(stage_root, _live_roots(plan), "stage root")
    if transport_mode == "prepare":
        if (
            destination_ssh_host is not None
            or prepare_receipt is not None
            or transport_receipt is not None
        ):
            raise BulkloadError(
                "destination preparation does not accept transport inputs"
            )
        if (
            socket.gethostname()
            != plan["destination"]["catalog"]["transport"]["hostname"]
        ):
            raise BulkloadError(
                "transport preparation must run on captured destination"
            )
        durable_makedirs(stage_root)
        os.chmod(stage_root, 0o700)
        quarantine = stage_root / ".transport-quarantine"
        durable_makedirs(quarantine)
        os.chmod(quarantine, 0o700)
        charged = int(plan["capacity"]["incoming_unique_bytes"])
        if phase == "final":
            charged += int(plan["capacity"]["sqlite_compose_bytes"])
        capacity = require_capacity(
            stage_root, charged_bytes=charged, reserve_bytes=reserve_bytes
        )
        source = plan["source"]["catalog"]["transport"]
        destination = plan["destination"]["catalog"]["transport"]
        destination_binding = inspect_rsync(destination["rsync"]["path"])
        if destination_binding != destination["rsync"]:
            raise BulkloadError("destination rsync differs from captured authority")
        payload = b"".join(
            os.fsencode(item) + b"\0" for item in _plan_source_paths(plan)
        )
        atomic_write(_allowlist_path(stage_root, phase), payload, mode=0o600)
        transport = {
            "allowlist_sha256": sha256_bytes(payload),
            "allowlist_size": len(payload),
            "destination_host": destination["hostname"],
            "destination_rsync": destination_binding,
            "mode": "destination-prepare",
            "quarantine_root": os.fspath(quarantine),
            "source_host": source["hostname"],
            "source_roots": _catalog_source_roots(plan["source"]["catalog"]),
            "source_rsync": source["rsync"],
            "source_snapshot": plan["source"]["catalog"].get("snapshot"),
            "transport_receipt_sha256": None,
        }
        receipt = _transport_body(
            plan["plan_sha256"],
            plan["holds"],
            phase,
            stage_root,
            transport,
            capacity,
        )
        atomic_write_json(stage_root / f".prepare-receipt-{phase}.json", receipt)
        return receipt
    if destination_ssh_host is not None:
        raise BulkloadError("destination SSH host is invalid for plan staging")
    if transport_mode == "materialize":
        if transport_receipt is None or prepare_receipt is None:
            raise BulkloadError(
                "transport materialization requires exact prepare and push receipts"
            )
        if (
            socket.gethostname()
            != plan["destination"]["catalog"]["transport"]["hostname"]
        ):
            raise BulkloadError(
                "transport materialization must run on captured destination"
            )
    elif transport_receipt is not None or prepare_receipt is not None:
        raise BulkloadError("local staging does not accept a transport receipt")
    stage_root = Path(os.path.realpath(stage_root))
    durable_makedirs(stage_root)
    os.chmod(stage_root, 0o700)
    receipt_path = _stage_receipt_path(stage_root, phase)
    if receipt_path.exists():
        receipt = read_json(receipt_path)
        validate_stage_receipt(receipt, require_final=phase == "final")
        if receipt["plan_sha256"] != plan["plan_sha256"]:
            raise BulkloadError("existing stage receipt belongs to a different plan")
        _verify_stage_objects(receipt["manifest"], stage_root)
        snapshot = plan["source"]["catalog"].get("snapshot")
        if (
            phase == "final"
            and transport_mode == "local"
            and isinstance(snapshot, dict)
        ):
            validate_live_snapshot_generation(snapshot)
        return receipt
    if phase == "final":
        preseed_receipt_path = _stage_receipt_path(stage_root, "preseed")
        if not preseed_receipt_path.exists():
            raise BulkloadError("final staging requires the sealed preseed receipt")
        preseed = read_json(preseed_receipt_path)
        validate_stage_receipt(preseed)
        _verify_stage_objects(preseed["manifest"], stage_root)

    charged = int(plan["capacity"]["incoming_unique_bytes"])
    if phase == "final":
        charged += int(plan["capacity"]["sqlite_compose_bytes"])
    capacity = require_capacity(
        stage_root, charged_bytes=charged, reserve_bytes=reserve_bytes
    )
    if transport_mode == "materialize":
        assert transport_receipt is not None
        source_mirror, transport = _materialized_transport(
            plan, phase, stage_root, prepare_receipt, transport_receipt
        )
        snapshot = plan["source"]["catalog"].get("snapshot")
        if isinstance(snapshot, dict):
            metadata = {snapshot["seal_path"], snapshot["index_path"]}
            required = {
                Path("/") / item
                for item in _plan_source_paths(plan)
                if os.fspath(Path("/") / item) not in metadata
            }
            validate_snapshot_custody(
                snapshot, mirror=source_mirror, required_paths=required
            )
    else:
        source = plan["source"]["catalog"]["transport"]
        destination = plan["destination"]["catalog"]["transport"]
        if source["hostname"] != destination["hostname"]:
            raise BulkloadError("cross-host stage must use source push and materialize")
        source_binding = inspect_rsync(source["rsync"]["path"])
        destination_binding = inspect_rsync(destination["rsync"]["path"])
        if (
            source_binding != source["rsync"]
            or destination_binding != destination["rsync"]
        ):
            raise BulkloadError("local rsync differs from captured authority")
        snapshot = plan["source"]["catalog"].get("snapshot")
        if isinstance(snapshot, dict):
            validate_snapshot_custody(snapshot)
            if phase == "final":
                validate_live_snapshot_generation(snapshot)
        payload = b"".join(
            os.fsencode(item) + b"\0" for item in _plan_source_paths(plan)
        )
        source_mirror = None
        transport = {
            "allowlist_sha256": sha256_bytes(payload),
            "allowlist_size": len(payload),
            "destination_host": destination["hostname"],
            "destination_rsync": destination_binding,
            "mode": "local",
            "quarantine_root": None,
            "source_host": source["hostname"],
            "source_roots": _catalog_source_roots(plan["source"]["catalog"]),
            "source_rsync": source_binding,
            "source_snapshot": plan["source"]["catalog"].get("snapshot"),
            "transport_receipt_sha256": None,
        }
    stats = defaultdict(int)
    entries: list[dict[str, Any]] = []
    resolver = PlanOperationResolver(plan)
    for compact_operation in plan["operations"]:
        operation = resolver.materialize(compact_operation)
        try:
            if operation["kind"] == "git-workspace-union":
                entries.append(
                    _stage_git_operation(
                        operation,
                        stage_root=stage_root,
                        plan=plan,
                        source_mirror=source_mirror,
                        allow_accounted_copy=allow_accounted_copy,
                        stats=stats,
                    )
                )
            elif operation["kind"] == "sqlite-union":
                if phase == "final":
                    entries.append(
                        _sqlite_stage_entry(
                            operation,
                            stage_root=stage_root,
                            source_mirror=source_mirror,
                            source_catalog=plan["source"]["catalog"],
                            stats=stats,
                        )
                    )
            elif operation["kind"] in {"file-install", "auth-install"}:
                staged = _stage_file_operation(
                    operation,
                    stage_root=stage_root,
                    plan=plan,
                    source_mirror=source_mirror,
                    allow_accounted_copy=allow_accounted_copy,
                    stats=stats,
                )
                staged["payload_kind"] = staged["kind"]
                staged["kind"] = "file"
                staged["operation_kind"] = operation["kind"]
                entries.append(staged)
            else:
                raise BulkloadError("AgentPlanV4 contains an unknown operation")
        except _StageSourceChanged:
            if phase == "final":
                raise
            stats["deferred_operations"] += 1
    snapshot = plan["source"]["catalog"].get("snapshot")
    if phase == "final" and transport_mode == "local" and isinstance(snapshot, dict):
        validate_live_snapshot_generation(snapshot)
    manifest = {
        "created_at": utc_now(),
        "entries": entries,
        "holds": plan["holds"],
        "phase": phase,
        "plan_sha256": plan["plan_sha256"],
        "stage_id": new_id(),
        "stage_root": os.fspath(stage_root),
    }
    seal(manifest, "manifest_sha256")
    receipt = {
        "capacity": capacity,
        "created_at": utc_now(),
        "manifest": manifest,
        "manifest_sha256": manifest["manifest_sha256"],
        "materialization": dict(sorted(stats.items())),
        "phase": phase,
        "plan_sha256": plan["plan_sha256"],
        "ready_for_apply": phase == "final",
        "receipt_id": new_id(),
        "schema": AGENT_STAGE_SCHEMA,
        "stage_root": os.fspath(stage_root),
        "transport": transport,
    }
    seal(receipt, "receipt_sha256")
    atomic_write_json(receipt_path, receipt)
    return receipt


def _current_record(path: Path) -> dict[str, Any] | None:
    try:
        info = path.stat(follow_symlinks=False)
    except FileNotFoundError:
        return None
    mode = f"{stat.S_IMODE(info.st_mode):04o}"
    if stat.S_ISREG(info.st_mode):
        return {
            "kind": "regular",
            "mode": mode,
            "sha256": sha256_file(path),
            "size": info.st_size,
        }
    if stat.S_ISLNK(info.st_mode):
        return {
            "kind": "symlink",
            "mode": mode,
            "sha256": sha256_symlink(path),
            "size": len(os.fsencode(os.readlink(path))),
        }
    if stat.S_ISDIR(info.st_mode):
        return {"kind": "directory", "mode": mode, "sha256": None, "size": 0}
    raise BulkloadError(f"destination contains a special entry: {path}")


def _same_record(
    current: dict[str, Any] | None, expected: dict[str, Any] | None
) -> bool:
    if current is None or expected is None:
        return current is expected
    keys: tuple[str, ...] = ("kind", "mode", "sha256", "size")
    if current.get("kind") == "symlink" and expected.get("kind") == "symlink":
        # Symlink permission bits are not portable across kernels: darwin
        # lstat reports the creating umask's bits (commonly 0755) while Linux
        # fixes every symlink at 0777 and offers no lchmod, so the mode a
        # source host records can never be reproduced on the other kernel.
        # _atomic_install_blob installs links with a bare os.symlink and never
        # chmods one, so the live destination mode is always the destination
        # kernel's, not the manifest's. The link stays bound by kind, size,
        # and the target-path digest; only mode is exempt, and only when both
        # sides agree the entry is a symlink — the same two-sided shape the
        # custody verifier uses (scanner.validate_snapshot_custody, c81de89).
        keys = ("kind", "sha256", "size")
    return all(current.get(key) == expected.get(key) for key in keys if key in expected)


def _snapshot_target(target: Path, rollback_root: Path) -> tuple[dict[str, Any], int]:
    before = _current_record(target)
    token = sha256_bytes(os.fsencode(os.path.abspath(os.fspath(target))))
    snapshot = rollback_root / "files" / token[:2] / token
    if before is None:
        if snapshot.exists() or snapshot.is_symlink():
            raise BulkloadError("rollback root contains an unexpected stale snapshot")
        return {"before": None, "snapshot": None, "target": os.fspath(target)}, 0
    if before["kind"] == "regular":
        if snapshot.exists():
            if (
                not snapshot.is_file()
                or snapshot.stat().st_size != before["size"]
                or sha256_file(snapshot) != before["sha256"]
            ):
                raise BulkloadError("existing rollback snapshot is not exact")
        else:
            reflink_clone(
                target,
                snapshot,
                expected_sha256=before["sha256"],
                mode=int(before["mode"], 8),
            )
        return {
            "before": before,
            "snapshot": os.fspath(snapshot.relative_to(rollback_root)),
            "target": os.fspath(target),
        }, before["size"]
    if before["kind"] == "symlink":
        payload = os.fsencode(os.readlink(target))
        if snapshot.exists():
            if sha256_file(snapshot) != before["sha256"]:
                raise BulkloadError("existing rollback symlink snapshot is not exact")
        else:
            atomic_write(snapshot, payload)
        return {
            "before": before,
            "snapshot": os.fspath(snapshot.relative_to(rollback_root)),
            "target": os.fspath(target),
        }, len(payload)
    return {
        "before": before,
        "snapshot": None,
        "target": os.fspath(target),
    }, 0


def _atomic_install_blob(
    stage_root: Path, entry: dict[str, Any], target: Path
) -> dict[str, Any]:
    kind = entry.get("payload_kind", entry["kind"])
    mode = int(entry["mode"], 8)
    if kind == "directory":
        durable_makedirs(target)
        os.chmod(target, mode)
        fsync_directory(target)
        return _current_record(target) or {}
    blob = _object_path(stage_root, entry["blob_sha256"])
    if sha256_file(blob) != entry["blob_sha256"]:
        raise BulkloadError("sealed stage object changed before apply")
    durable_makedirs(target.parent)
    temporary = target.parent / f".{target.name}.bulkload-{new_id()}"
    if kind in {"regular", "sqlite"}:
        reflink_clone(blob, temporary, expected_sha256=entry["blob_sha256"], mode=mode)
    elif kind == "symlink":
        payload = blob.read_bytes()
        os.symlink(os.fsdecode(payload), temporary)
    else:
        raise BulkloadError("unsupported staged object kind")
    os.replace(temporary, target)
    fsync_directory(target.parent)
    return _current_record(target) or {}


def _resolve_git_dir(worktree: Path) -> Path:
    return Path(
        _git(worktree, ["rev-parse", "--path-format=absolute", "--git-dir"])
        .decode()
        .strip()
    )


def _ensure_git_workspace(
    entry: dict[str, Any],
    journal: dict[str, Any],
    journal_path: Path,
) -> Path:
    primary = Path(entry["destination_path"])
    bare = not entry["worktrees"]
    marker = primary / ("HEAD" if bare else ".git")
    if not marker.exists():
        if primary.exists() and any(primary.iterdir()):
            raise BulkloadError("new Git workspace destination is non-empty")
        durable_makedirs(primary.parent)
        if primary.parent.stat().st_dev != Path(journal["rollback_root"]).stat().st_dev:
            raise BulkloadError(
                "new Git workspace and rollback root must share a device"
            )
        created = journal.setdefault("created_git_roots", [])
        if os.fspath(primary) not in created:
            created.append(os.fspath(primary))
            _write_apply_journal(journal_path, journal)
        durable_makedirs(primary)
        arguments = ["init"]
        if bare:
            arguments.append("--bare")
        if entry["object_format"] != "sha1":
            arguments.append(f"--object-format={entry['object_format']}")
        arguments.append(os.fspath(primary))
        result = subprocess.run(
            ["git", *arguments],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=git_environment(),
        )
        if result.returncode != 0:
            raise BulkloadError("cannot initialize destination Git workspace")
    return primary


def _git_ref_state(
    repository: Path, names: Iterable[str]
) -> dict[str, dict[str, str | None] | None]:
    result: dict[str, dict[str, str | None] | None] = {}
    for name in sorted(set(names)):
        payload = _git(repository, ["rev-parse", "--verify", name], check=False).strip()
        if not payload:
            result[name] = None
            continue
        symbolic = _git(repository, ["symbolic-ref", "-q", name], check=False).strip()
        result[name] = {
            "oid": payload.decode("ascii"),
            "symbolic_target": symbolic.decode("utf-8") if symbolic else None,
        }
    return result


def _git_worktree_state(worktree: Path) -> dict[str, Any]:
    symbolic = _git(worktree, ["symbolic-ref", "-q", "HEAD"], check=False).strip()
    head = _git(worktree, ["rev-parse", "--verify", "HEAD"], check=False).strip()
    git_dir = _resolve_git_dir(worktree)
    locked_path = git_dir / "locked"
    locked = locked_path.exists()
    lock_reason = None
    if locked:
        try:
            payload = locked_path.read_bytes()
            if payload and not payload.endswith(b"\n"):
                raise ValueError("missing terminator")
            lock_reason = (payload[:-1] if payload else payload).decode("utf-8")
        except (OSError, UnicodeDecodeError, ValueError) as error:
            raise BulkloadError("Git worktree lock reason is not portable") from error
    return {
        "branch": symbolic.decode("utf-8") if symbolic else None,
        "detached": not bool(symbolic),
        "head": head.decode("ascii") if head else None,
        "locked": locked,
        "lock_reason": lock_reason,
    }


def _git_crash_fence(boundary: str) -> None:
    if os.environ.get("BULKLOAD_TEST_CRASH_GIT_AFTER") == boundary:
        raise BulkloadError(f"injected crash after Git {boundary} mutation")


def _preflight_git_targets(entry: dict[str, Any], journal: dict[str, Any]) -> None:
    created_roots = set(journal.get("created_git_roots", []))
    created_paths = {item["path"] for item in journal.get("created_worktrees", [])}
    primary_target = Path(entry["destination_path"])
    primary_marker = primary_target / ("HEAD" if not entry["worktrees"] else ".git")
    if (
        not (primary_marker.exists() or primary_marker.is_symlink())
        and (primary_target.exists() or primary_target.is_symlink())
        and os.fspath(primary_target) not in created_roots
    ):
        raise BulkloadError("new Git workspace destination already exists")
    for worktree in entry["worktrees"]:
        target = Path(worktree["path"])
        marker = target / ".git"
        if (
            target != primary_target
            and not (marker.exists() or marker.is_symlink())
            and (target.exists() or target.is_symlink())
            and os.fspath(target) not in created_paths
        ):
            raise BulkloadError("new Git worktree destination already exists")


def _apply_git_entry(
    entry: dict[str, Any],
    *,
    stage_root: Path,
    journal: dict[str, Any],
    journal_path: Path,
) -> None:
    primary = _ensure_git_workspace(entry, journal, journal_path)
    common = Path(
        _git(primary, ["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .decode()
        .strip()
    )
    if os.fspath(common) != entry["destination_common_git_dir"]:
        raise BulkloadError(
            "destination Git common directory differs from stage binding"
        )
    ref_names = [action["name"] for action in entry["ref_actions"]]
    ref_names.extend(
        ref["name"] for ref in (entry.get("destination_before") or {}).get("refs", [])
    )
    git_refs_before = journal.setdefault("git_refs_before", {})
    if os.fspath(primary) not in git_refs_before:
        git_refs_before[os.fspath(primary)] = _git_ref_state(primary, ref_names)
    created_worktrees = journal.setdefault("created_worktrees", [])
    created_paths = {item["path"] for item in created_worktrees}
    worktrees_before = journal.setdefault("git_worktrees_before", {}).setdefault(
        os.fspath(primary), {}
    )
    for worktree in entry["worktrees"]:
        target = Path(worktree["path"])
        if (
            (target / ".git").exists()
            and os.fspath(target) not in created_paths
            and os.fspath(target) not in worktrees_before
        ):
            worktrees_before[os.fspath(target)] = _git_worktree_state(target)

    intended_refs = dict(git_refs_before[os.fspath(primary)])
    for action in entry["ref_actions"]:
        intended_refs[action["name"]] = {
            "oid": action["oid"],
            "symbolic_target": action.get("symbolic_target"),
        }
    intended_worktrees = {}
    for worktree in entry["worktrees"]:
        before = worktrees_before.get(worktree["path"])
        locked = bool(worktree.get("locked"))
        intended_worktrees[worktree["path"]] = {
            "branch": worktree["branch"],
            "detached": worktree["detached"],
            "head": worktree["head"],
            "locked": locked,
            "lock_reason": (
                before["lock_reason"] if locked and before and before["locked"] else ""
            )
            if locked
            else None,
        }
    for field, intended in (
        ("git_refs_after", intended_refs),
        ("git_worktrees_after", intended_worktrees),
    ):
        recorded = journal.setdefault(field, {})
        if os.fspath(primary) in recorded and recorded[os.fspath(primary)] != intended:
            raise BulkloadError("Git intended after-state changed during recovery")
        recorded[os.fspath(primary)] = intended
    _write_apply_journal(journal_path, journal)

    created_objects = journal.setdefault("created_git_objects", [])
    for object_entry in entry["objects"]:
        target = common / "objects" / object_entry["relative_path"]
        if target.exists():
            if sha256_file(target) != object_entry["blob_sha256"]:
                raise BulkloadError("destination Git object file diverges")
            continue
        record = {
            "path": os.fspath(target),
            "sha256": object_entry["blob_sha256"],
        }
        if record not in created_objects:
            created_objects.append(record)
            _write_apply_journal(journal_path, journal)
        _atomic_install_blob(stage_root, {**object_entry, "kind": "regular"}, target)
    for action in entry["ref_actions"]:
        observed = _git_ref_state(primary, [action["name"]])[action["name"]]
        expected = intended_refs[action["name"]]
        if observed == expected:
            continue
        if action.get("symbolic_target"):
            _git(primary, ["symbolic-ref", action["name"], action["symbolic_target"]])
        else:
            _git(primary, ["update-ref", action["name"], action["oid"]])
        _git_crash_fence("ref")
    for worktree in entry["worktrees"]:
        target = Path(worktree["path"])
        if target == primary:
            continue
        if not (target / ".git").exists():
            durable_makedirs(target.parent)
            record = {"path": os.fspath(target), "repository": os.fspath(primary)}
            if record not in created_worktrees:
                created_worktrees.append(record)
                _write_apply_journal(journal_path, journal)
            if worktree["branch"]:
                _git(
                    primary,
                    [
                        "worktree",
                        "add",
                        "--force",
                        "--no-checkout",
                        os.fspath(target),
                        worktree["branch"],
                    ],
                )
            else:
                _git(
                    primary,
                    [
                        "worktree",
                        "add",
                        "--detach",
                        "--no-checkout",
                        os.fspath(target),
                        worktree["head"],
                    ],
                )
            _git_crash_fence("head")
    for worktree in entry["worktrees"]:
        target = Path(worktree["path"])
        observed = _git_worktree_state(target)
        if worktree["branch"] and observed["branch"] != worktree["branch"]:
            _git(target, ["symbolic-ref", "HEAD", worktree["branch"]])
            _git_crash_fence("head")
        elif (
            not worktree["branch"]
            and worktree["head"]
            and (observed["branch"] is not None or observed["head"] != worktree["head"])
        ):
            _git(target, ["update-ref", "--no-deref", "HEAD", worktree["head"]])
            _git_crash_fence("head")
        if target != primary:
            observed_locked = (_resolve_git_dir(target) / "locked").exists()
            if worktree.get("locked") and not observed_locked:
                _git(primary, ["worktree", "lock", os.fspath(target)])
                _git_crash_fence("lock")
            elif not worktree.get("locked") and observed_locked:
                _git(primary, ["worktree", "unlock", os.fspath(target)])
                _git_crash_fence("lock")
    observed_refs = _git_ref_state(primary, ref_names)
    observed_worktrees = {
        worktree["path"]: _git_worktree_state(Path(worktree["path"]))
        for worktree in entry["worktrees"]
    }
    if observed_refs != intended_refs or observed_worktrees != intended_worktrees:
        raise BulkloadError("Git preparation did not reach its intended after-state")
    _git(primary, ["fsck", "--full", "--no-dangling"])


def _collect_mutations(manifest: dict[str, Any]) -> list[dict[str, Any]]:
    mutations: list[dict[str, Any]] = []
    for entry in manifest["entries"]:
        if entry["kind"] in {"file", "sqlite"}:
            mutations.append(
                {
                    "action": "install",
                    "entry": entry,
                    "target": entry["destination_path"],
                }
            )
            if entry["kind"] == "sqlite":
                sidecars = {
                    f"-{item['kind']}": {
                        "kind": "regular",
                        "sha256": item["sha256"],
                        "size": item["size"],
                    }
                    for item in (entry.get("destination_before") or {}).get(
                        "sidecars", []
                    )
                }
                for suffix in ("-wal", "-shm", "-journal"):
                    mutations.append(
                        {
                            "action": "delete",
                            "destination_before": sidecars.get(suffix),
                            "target": entry["destination_path"] + suffix,
                        }
                    )
        elif entry["kind"] == "git-workspace":

            def preserve_reflog(path: Path, git_dir: Path) -> None:
                mutations.append(
                    {
                        "action": "preserve-git-reflog",
                        "destination_before": _current_record(path),
                        "target": os.fspath(path),
                    }
                )
                parent = path.parent
                while parent != git_dir:
                    if not (parent.exists() or parent.is_symlink()):
                        mutations.append(
                            {
                                "action": "preserve-git-directory",
                                "destination_before": None,
                                "target": os.fspath(parent),
                            }
                        )
                    parent = parent.parent

            common = Path(entry["destination_common_git_dir"])
            for action in entry["ref_actions"]:
                preserve_reflog(common / "logs" / action["name"], common)
            destination_worktrees = {
                item["path"]: item
                for item in (entry.get("destination_before") or {}).get("worktrees", [])
            }
            for worktree in entry["worktrees"]:
                destination_worktree = destination_worktrees.get(worktree["path"])
                if destination_worktree is not None and (
                    destination_worktree["branch"],
                    destination_worktree["head"],
                ) != (worktree["branch"], worktree["head"]):
                    git_dir = Path(destination_worktree["git_dir"])
                    preserve_reflog(git_dir / "logs" / "HEAD", git_dir)
                for file_entry in worktree["files"]:
                    mutations.append(
                        {
                            "action": "install",
                            "entry": file_entry,
                            "target": file_entry["destination_path"],
                        }
                    )
                for target in worktree["deletions"]:
                    relative = str(Path(target).relative_to(worktree["path"]))
                    destination_before = next(
                        (
                            item
                            for item in (
                                next(
                                    (
                                        candidate
                                        for candidate in (
                                            entry.get("destination_before") or {}
                                        ).get("worktrees", [])
                                        if candidate["path"] == worktree["path"]
                                    ),
                                    {"files": []},
                                )["files"]
                            )
                            if item["relative_path"] == relative
                        ),
                        None,
                    )
                    mutations.append(
                        {
                            "action": "rmdir"
                            if (destination_before or {}).get("kind") == "directory"
                            else "delete",
                            "destination_before": destination_before,
                            "target": target,
                        }
                    )
                # Existing worktree indexes can be snapshotted before topology mutation.
                destination_before = next(
                    (
                        item
                        for item in (entry.get("destination_before") or {}).get(
                            "worktrees", []
                        )
                        if item["path"] == worktree["path"]
                    ),
                    None,
                )
                if worktree["index"] is not None and destination_before is not None:
                    mutations.append(
                        {
                            "action": "install-index",
                            "entry": worktree["index"],
                            "target": destination_before["index"]["path"],
                            "worktree": worktree["path"],
                        }
                    )
                elif (
                    not worktree["index_expected_exists"]
                    and worktree["index_destination_path"] is not None
                ):
                    mutations.append(
                        {
                            "action": "delete",
                            "destination_before": worktree["index_destination_before"],
                            "target": worktree["index_destination_path"],
                        }
                    )
    # Git owns newly created repository/worktree roots. Journal only their
    # otherwise implicit parents so the Git rollback steps retain authority.
    skip_roots: set[str] = set()
    for entry in manifest["entries"]:
        if entry["kind"] != "git-workspace":
            continue
        destination_before = entry.get("destination_before") or {}
        if not destination_before:
            skip_roots.add(entry["destination_path"])
        existing_worktrees = {
            item["path"] for item in destination_before.get("worktrees", [])
        }
        skip_roots.update(
            item["path"]
            for item in entry["worktrees"]
            if item["path"] not in existing_worktrees
        )

    known_targets = {item["target"] for item in mutations}
    missing_parents: set[str] = set()

    def collect_missing_parents(target: Path) -> None:
        parent = target.parent
        while parent != parent.parent and not (parent.exists() or parent.is_symlink()):
            value = os.fspath(parent)
            if value not in known_targets and value not in skip_roots:
                missing_parents.add(value)
            parent = parent.parent

    for mutation in mutations:
        if mutation["action"].startswith("preserve-git-"):
            continue
        collect_missing_parents(Path(mutation["target"]))
    for root in skip_roots:
        collect_missing_parents(Path(root))
    mutations.extend(
        {"action": "mkdir", "destination_before": None, "target": target}
        for target in missing_parents
    )

    # One target must have one deterministic intended mutation.
    result: dict[str, dict[str, Any]] = {}
    for mutation in mutations:
        target = mutation["target"]
        if target in result and result[target] != mutation:
            raise BulkloadError(f"multiple stage mutations target one path: {target}")
        result[target] = mutation

    def order(mutation: dict[str, Any]) -> tuple[int, int, str]:
        depth = len(Path(mutation["target"]).parts)
        if mutation["action"] == "mkdir":
            return (0, depth, mutation["target"])
        if mutation["action"] == "rmdir":
            return (2, -depth, mutation["target"])
        return (1, 0, mutation["target"])

    return sorted(result.values(), key=order)


def _snapshot_all(
    journal: dict[str, Any],
    *,
    journal_path: Path,
    rollback_root: Path,
) -> int:
    mutations = journal["mutations"]
    snapshots = journal["rollback_snapshots"]
    if len(snapshots) > len(mutations):
        raise BulkloadError("rollback snapshot progress exceeds mutation inventory")
    for mutation in mutations[len(snapshots) :]:
        snapshot, _ = _snapshot_target(Path(mutation["target"]), rollback_root)
        snapshot["action"] = mutation["action"]
        snapshots.append(snapshot)
        journal["snapshot_progress"] = len(snapshots)
        journal["updated_at"] = utc_now()
        _write_apply_journal(journal_path, journal)
        raw = os.environ.get("BULKLOAD_TEST_CRASH_AFTER_SNAPSHOT")
        if raw is not None and raw.isdecimal() and len(snapshots) >= int(raw):
            raise BulkloadError("injected crash after durable rollback snapshot")
    return sum(int((item["before"] or {}).get("size", 0)) for item in snapshots)


def _revalidate_mutation_preconditions(mutations: list[dict[str, Any]]) -> None:
    for mutation in mutations:
        target = Path(mutation["target"])
        if mutation["action"].startswith("preserve-git-"):
            if not _same_record(
                _current_record(target), mutation.get("destination_before")
            ):
                raise BulkloadError(f"destination changed after planning: {target}")
            continue
        entry = mutation.get("entry", {})
        expected = entry.get("destination_before", mutation.get("destination_before"))
        if entry.get("kind") == "sqlite":
            if expected is None:
                if target.exists() or target.is_symlink():
                    raise BulkloadError(
                        "destination SQLite appeared after final staging"
                    )
            else:
                observed = sqlite_catalog(target)
                if observed["logical_sha256"] != expected["logical"]["logical_sha256"]:
                    raise BulkloadError(
                        "destination SQLite changed after final staging"
                    )
            continue
        current = _current_record(target)
        if not _same_record(current, expected):
            raise BulkloadError(f"destination changed after planning: {target}")


def _crash_fence(journal: dict[str, Any]) -> None:
    raw = os.environ.get("BULKLOAD_TEST_CRASH_AFTER")
    if raw is None:
        return
    try:
        threshold = int(raw)
    except ValueError:
        return
    if journal.get("progress", 0) >= threshold:
        raise BulkloadError("injected crash after durable mutation boundary")


def _apply_mutations(
    journal: dict[str, Any],
    *,
    journal_path: Path,
    manifest: dict[str, Any],
    stage_root: Path,
) -> None:
    if not journal.get("git_prepared"):
        git_entries = [
            entry for entry in manifest["entries"] if entry["kind"] == "git-workspace"
        ]
        for entry in git_entries:
            _preflight_git_targets(entry, journal)
        for entry in git_entries:
            _apply_git_entry(
                entry,
                stage_root=stage_root,
                journal=journal,
                journal_path=journal_path,
            )
        journal["git_prepared"] = True
        known_targets = {mutation["target"] for mutation in journal["mutations"]}
        for entry in manifest["entries"]:
            if entry["kind"] != "git-workspace":
                continue
            for worktree in entry["worktrees"]:
                index_path = _resolve_git_dir(Path(worktree["path"])) / "index"
                if os.fspath(index_path) in known_targets:
                    continue
                if worktree["index"] is not None:
                    mutation = {
                        "action": "install-index",
                        "entry": worktree["index"],
                        "target": os.fspath(index_path),
                        "worktree": worktree["path"],
                    }
                else:
                    mutation = {
                        "action": "delete",
                        "destination_before": None,
                        "target": os.fspath(index_path),
                    }
                journal["mutations"].append(mutation)
                journal["rollback_snapshots"].append(
                    {
                        "action": mutation["action"],
                        "before": None,
                        "snapshot": None,
                        "target": os.fspath(index_path),
                    }
                )
                journal["snapshot_progress"] = len(journal["rollback_snapshots"])
        _write_apply_journal(journal_path, journal)
    mutations = journal["mutations"]
    for index in range(journal.get("progress", 0), len(mutations)):
        mutation = mutations[index]
        target = Path(mutation["target"])
        if mutation["action"] == "mkdir":
            durable_makedirs(target)
            after = _current_record(target)
        elif mutation["action"].startswith("preserve-git-"):
            after = _current_record(target)
        elif mutation["action"] == "rmdir":
            if target.exists() or target.is_symlink():
                if not target.is_dir() or target.is_symlink():
                    raise BulkloadError("transactional rmdir target is not a directory")
                target.rmdir()
                fsync_directory(target.parent)
            after = None
        elif mutation["action"] == "delete":
            if target.is_dir() and not target.is_symlink():
                raise BulkloadError(
                    "transactional deletion of a directory is forbidden"
                )
            target.unlink(missing_ok=True)
            if target.parent.exists():
                fsync_directory(target.parent)
            after = None
        else:
            entry = mutation["entry"]
            if mutation["action"] == "install-index" and not target.parent.exists():
                target = _resolve_git_dir(Path(mutation["worktree"])) / "index"
                mutation["target"] = os.fspath(target)
            after = _atomic_install_blob(stage_root, entry, target)
        mutation["after"] = after
        journal["progress"] = index + 1
        journal["updated_at"] = utc_now()
        _write_apply_journal(journal_path, journal)
        _crash_fence(journal)
    journal["state"] = "mutations-applied"
    journal["updated_at"] = utc_now()
    _write_apply_journal(journal_path, journal)


def _load_manifest(stage_receipt: dict[str, Any]) -> dict[str, Any]:
    validate_stage_receipt(stage_receipt)
    manifest = stage_receipt["manifest"]
    _verify_stage_objects(manifest, Path(stage_receipt["stage_root"]))
    return manifest


def _receipt_from_journal(journal: dict[str, Any]) -> dict[str, Any]:
    receipt = journal.get("apply_receipt")
    if not isinstance(receipt, dict):
        raise BulkloadError("completed journal lacks its exact apply receipt")
    require_digest(receipt, "receipt_sha256")
    return receipt


def apply_agent_plan(
    plan: dict[str, Any],
    stage_receipt: dict[str, Any],
    *,
    accepted_plan_sha256: str,
    journal_path: Path,
    rollback_root: Path,
    reserve_bytes: int = DEFAULT_CAPACITY_RESERVE_BYTES,
) -> dict[str, Any]:
    validate_agent_plan(plan, require_ready=True)
    validate_stage_receipt(stage_receipt, require_final=True)
    if accepted_plan_sha256 != plan["plan_sha256"]:
        raise BulkloadError("accepted plan digest does not match AgentPlanV4")
    if stage_receipt["plan_sha256"] != plan["plan_sha256"]:
        raise BulkloadError("final stage belongs to a different plan")
    manifest = _load_manifest(stage_receipt)
    stage_root = Path(stage_receipt["stage_root"])
    journal_path = Path(os.path.abspath(os.fspath(journal_path.expanduser())))
    rollback_root = Path(os.path.abspath(os.fspath(rollback_root.expanduser())))
    protected = [*_live_roots(plan), stage_root]
    assert_no_overlap(journal_path, protected, "apply journal")
    assert_no_overlap(rollback_root, protected, "rollback root")
    assert_no_overlap(journal_path, [rollback_root], "apply journal")
    durable_makedirs(journal_path.parent)
    durable_makedirs(rollback_root)
    if journal_path.exists():
        journal = _read_apply_journal(journal_path)
        if (
            journal.get("plan_sha256") != plan["plan_sha256"]
            or journal.get("stage_manifest_sha256") != manifest["manifest_sha256"]
            or journal.get("stage_receipt_sha256") != stage_receipt["receipt_sha256"]
            or journal.get("rollback_root") != os.fspath(rollback_root)
        ):
            raise BulkloadError("existing apply journal belongs to another transaction")
        if journal.get("state") in {"applied", "verified"}:
            return _receipt_from_journal(journal)
        if journal.get("state") == "rolled-back":
            raise BulkloadError("a rolled-back journal cannot be applied again")
    else:
        if any(rollback_root.iterdir()):
            raise BulkloadError("new rollback root must be empty")
        mutations = _collect_mutations(manifest)
        _revalidate_mutation_preconditions(mutations)
        exact_overwritten = sum(
            (_current_record(Path(item["target"])) or {}).get("size", 0)
            for item in mutations
        )
        capacity = require_capacity(
            rollback_root,
            charged_bytes=exact_overwritten,
            reserve_bytes=reserve_bytes,
        )
        journal = {
            "apply_receipt": None,
            "capacity": capacity,
            "created_git_objects": [],
            "created_git_roots": [],
            "created_worktrees": [],
            "git_prepared": False,
            "git_refs_before": {},
            "journal_id": new_id(),
            "mutations": mutations,
            "plan_sha256": plan["plan_sha256"],
            "progress": 0,
            "rollback_snapshots": [],
            "rollback_root": os.fspath(rollback_root),
            "schema": AGENT_JOURNAL_SCHEMA,
            "snapshot_progress": 0,
            "stage_manifest_sha256": manifest["manifest_sha256"],
            "stage_receipt_sha256": stage_receipt["receipt_sha256"],
            "state": "preparing-rollback",
            "transaction_id": new_id(),
            "updated_at": utc_now(),
        }
        _write_apply_journal(journal_path, journal)
    if journal.get("state") == "preparing-rollback":
        _revalidate_mutation_preconditions(journal["mutations"])
        charged = _snapshot_all(
            journal,
            journal_path=journal_path,
            rollback_root=rollback_root,
        )
        exact_overwritten = int(journal["capacity"]["charged_bytes"])
        if charged != exact_overwritten:
            raise BulkloadError(
                "rollback exact-overwrite charge changed during snapshot"
            )
        journal["state"] = "rollback-sealed"
        journal["updated_at"] = utc_now()
        _write_apply_journal(journal_path, journal)
    if journal.get("state") not in {"rollback-sealed", "mutations-applied"}:
        raise BulkloadError("apply journal is in an unsupported transaction state")
    _apply_mutations(
        journal,
        journal_path=journal_path,
        manifest=manifest,
        stage_root=stage_root,
    )
    receipt = {
        "applied_at": utc_now(),
        "capacity": journal["capacity"],
        "holds": plan["holds"],
        "journal_path": os.fspath(journal_path),
        "mutation_count": len(journal["mutations"]),
        "plan_sha256": plan["plan_sha256"],
        "provider_runtime_acceptance_verified": False,
        "receipt_id": new_id(),
        "rollback_root": os.fspath(rollback_root),
        "schema": AGENT_APPLY_SCHEMA,
        "stage_manifest_sha256": manifest["manifest_sha256"],
        "transaction_id": journal["transaction_id"],
    }
    seal(receipt, "receipt_sha256")
    journal["apply_receipt"] = receipt
    journal["state"] = "applied"
    journal["updated_at"] = utc_now()
    _write_apply_journal(journal_path, journal)
    return receipt


def validate_apply_receipt(value: dict[str, Any]) -> None:
    require_exact_keys(
        value,
        {
            "applied_at",
            "capacity",
            "holds",
            "journal_path",
            "mutation_count",
            "plan_sha256",
            "provider_runtime_acceptance_verified",
            "receipt_id",
            "receipt_sha256",
            "rollback_root",
            "schema",
            "stage_manifest_sha256",
            "transaction_id",
        },
        "AgentApplyV4 receipt",
    )
    if value.get("schema") != AGENT_APPLY_SCHEMA:
        raise BulkloadError("input is not an AgentApplyV4 receipt")
    require_digest(value, "receipt_sha256")
    if value.get("provider_runtime_acceptance_verified") is not False:
        raise BulkloadError("offline receipt overclaims provider runtime acceptance")


def _verify_git_entry(entry: dict[str, Any]) -> list[dict[str, str]]:
    failures: list[dict[str, str]] = []
    repository = Path(entry["destination_path"])
    observed_format = (
        _git(repository, ["rev-parse", "--show-object-format"], check=False)
        .decode(errors="replace")
        .strip()
    )
    if observed_format != entry["object_format"]:
        failures.append(
            {"code": "git-object-format-mismatch", "path": os.fspath(repository)}
        )
    for action in entry["ref_actions"]:
        observed = _git_ref_state(repository, [action["name"]])[action["name"]]
        expected = {
            "oid": action["oid"],
            "symbolic_target": action.get("symbolic_target"),
        }
        if observed != expected:
            failures.append({"code": "git-ref-mismatch", "path": action["name"]})
    action_names = {action["name"] for action in entry["ref_actions"]}
    for ref in (entry.get("destination_before") or {}).get("refs", []):
        if ref["name"] in action_names:
            continue
        observed = _git_ref_state(repository, [ref["name"]])[ref["name"]]
        expected = {
            "oid": ref["oid"],
            "symbolic_target": ref.get("symbolic_target"),
        }
        if observed != expected:
            failures.append(
                {"code": "git-destination-ref-mismatch", "path": ref["name"]}
            )
    for worktree in entry["worktrees"]:
        path = Path(worktree["path"])
        if not (path / ".git").exists():
            failures.append({"code": "git-worktree-missing", "path": os.fspath(path)})
            continue
        index = _resolve_git_dir(path) / "index"
        if worktree["index"] is not None:
            if (
                not index.exists()
                or sha256_file(index) != worktree["index"]["blob_sha256"]
            ):
                failures.append({"code": "git-index-mismatch", "path": os.fspath(path)})
        elif index.exists() or index.is_symlink():
            failures.append({"code": "git-index-mismatch", "path": os.fspath(path)})
        observed_state = _git_worktree_state(path)
        expected_state = {
            "branch": worktree["branch"],
            "detached": worktree["detached"],
            "head": worktree["head"],
            "locked": bool(worktree.get("locked")),
        }
        if any(observed_state[key] != value for key, value in expected_state.items()):
            failures.append(
                {"code": "git-worktree-state-mismatch", "path": os.fspath(path)}
            )
        for file_entry in worktree["files"]:
            current = _current_record(Path(file_entry["destination_path"]))
            expected = {
                "kind": file_entry["kind"],
                "mode": file_entry["mode"],
                "sha256": file_entry["blob_sha256"],
                "size": file_entry["size"],
            }
            if not _same_record(current, expected):
                failures.append(
                    {
                        "code": "git-worktree-byte-mismatch",
                        "path": file_entry["destination_path"],
                    }
                )
        for target in worktree["deletions"]:
            if Path(target).exists() or Path(target).is_symlink():
                failures.append(
                    {"code": "git-worktree-deletion-mismatch", "path": target}
                )
    try:
        _git(repository, ["fsck", "--full", "--no-dangling"])
    except BulkloadError:
        failures.append({"code": "git-fsck-failed", "path": os.fspath(repository)})
    return failures


def verify_agent_plan(
    plan: dict[str, Any],
    stage_receipt: dict[str, Any],
    apply_receipt: dict[str, Any],
    *,
    destination_verify_receipt: dict[str, Any] | None = None,
) -> dict[str, Any]:
    validate_agent_plan(plan, require_ready=True)
    validate_stage_receipt(stage_receipt, require_final=True)
    validate_apply_receipt(apply_receipt)
    if (
        len(
            {
                plan["plan_sha256"],
                stage_receipt["plan_sha256"],
                apply_receipt["plan_sha256"],
            }
        )
        != 1
    ):
        raise BulkloadError("verify inputs do not share one plan digest")
    manifest = (
        stage_receipt["manifest"]
        if destination_verify_receipt is not None
        else _load_manifest(stage_receipt)
    )
    if apply_receipt.get("stage_manifest_sha256") != manifest["manifest_sha256"]:
        raise BulkloadError("apply receipt is detached from the final stage manifest")
    if destination_verify_receipt is not None:
        validate_verify_receipt(
            destination_verify_receipt, required_role="destination-state"
        )
        if (
            destination_verify_receipt["verified"] is not True
            or destination_verify_receipt["failures"]
            or destination_verify_receipt["plan_sha256"] != plan["plan_sha256"]
            or destination_verify_receipt["apply_receipt_sha256"]
            != apply_receipt["receipt_sha256"]
            or destination_verify_receipt["holds"] != plan["holds"]
            or destination_verify_receipt["observation_host"]
            != plan["destination"]["catalog"]["transport"]["hostname"]
        ):
            raise BulkloadError(
                "destination verification is detached from the cutover release"
            )
        source = plan["source"]["catalog"]
        if socket.gethostname() != source["transport"]["hostname"]:
            raise BulkloadError("cutover release must run on the captured source host")
        snapshot = source.get("snapshot")
        if not isinstance(snapshot, dict):
            raise BulkloadError("cutover release requires immutable source B custody")
        validate_snapshot_custody(snapshot)
        # `allow_break_glass=False`, and only here. This site is not gated on
        # a phase -- it runs whenever a destination receipt is presented --
        # and the receipt built immediately below seals
        # `independent_fresh_observation: True` and `verified: True`, which
        # `validate_verify_receipt` then *requires*. Under the #24 break-glass
        # that observation would not have happened, and the sealed receipt
        # would be byte-identical to an honest one. So the bypass is refused
        # at the one site that would launder it into a clean cutover.
        validate_live_snapshot_generation(snapshot, allow_break_glass=False)
        receipt = {
            "apply_receipt_sha256": apply_receipt["receipt_sha256"],
            "destination_verify_receipt_sha256": destination_verify_receipt[
                "receipt_sha256"
            ],
            "failures": [],
            "holds": plan["holds"],
            "independent_fresh_observation": True,
            "observation_host": socket.gethostname(),
            "plan_sha256": plan["plan_sha256"],
            "provider_runtime_acceptance_verified": False,
            "receipt_id": new_id(),
            "schema": AGENT_VERIFY_SCHEMA,
            "source_snapshot_seal_sha256": snapshot["seal_sha256"],
            "verification_role": "cutover-release",
            "verified": True,
            "verified_at": utc_now(),
        }
        seal(receipt, "receipt_sha256")
        return receipt
    journal_path = Path(apply_receipt["journal_path"])
    journal = _read_apply_journal(journal_path)
    if (
        journal.get("transaction_id") != apply_receipt["transaction_id"]
        or journal.get("apply_receipt") != apply_receipt
        or journal.get("stage_receipt_sha256") != stage_receipt["receipt_sha256"]
    ):
        raise BulkloadError("verify evidence is detached from its journal")
    failures: list[dict[str, str]] = []
    for entry in manifest["entries"]:
        if entry["kind"] == "git-workspace":
            failures.extend(_verify_git_entry(entry))
        elif entry["kind"] == "sqlite":
            target = Path(entry["destination_path"])
            try:
                observed = sqlite_catalog(target)
            except BulkloadError:
                failures.append(
                    {
                        "code": "sqlite-verification-failed",
                        "path": entry["destination_path"],
                    }
                )
            else:
                if (
                    observed["logical_sha256"]
                    != entry["expected_logical"]["logical_sha256"]
                ):
                    failures.append(
                        {
                            "code": "sqlite-logical-mismatch",
                            "path": entry["destination_path"],
                        }
                    )
                for suffix in ("-wal", "-shm", "-journal"):
                    if Path(entry["destination_path"] + suffix).exists():
                        failures.append(
                            {
                                "code": "sqlite-sidecar-after-apply",
                                "path": entry["destination_path"],
                            }
                        )
        elif entry["kind"] == "file":
            current = _current_record(Path(entry["destination_path"]))
            expected = {
                "kind": entry["payload_kind"],
                "mode": entry["mode"],
                "sha256": entry["blob_sha256"],
                "size": entry["size"],
            }
            if not _same_record(current, expected):
                failures.append(
                    {
                        "code": "file-verification-failed",
                        "path": entry["destination_path"],
                    }
                )
    receipt = {
        "apply_receipt_sha256": apply_receipt["receipt_sha256"],
        "destination_verify_receipt_sha256": None,
        "failures": sorted(failures, key=lambda item: (item["code"], item["path"])),
        "holds": plan["holds"],
        "independent_fresh_observation": True,
        "observation_host": socket.gethostname(),
        "plan_sha256": plan["plan_sha256"],
        "provider_runtime_acceptance_verified": False,
        "receipt_id": new_id(),
        "schema": AGENT_VERIFY_SCHEMA,
        "source_snapshot_seal_sha256": None,
        "verification_role": "destination-state",
        "verified": not failures,
        "verified_at": utc_now(),
    }
    seal(receipt, "receipt_sha256")
    if not failures:
        journal["state"] = "verified"
        journal["verify_receipt_sha256"] = receipt["receipt_sha256"]
        journal["updated_at"] = utc_now()
        _write_apply_journal(journal_path, journal)
    return receipt


def validate_verify_receipt(
    value: dict[str, Any], *, required_role: str | None = None
) -> None:
    require_exact_keys(
        value,
        {
            "apply_receipt_sha256",
            "destination_verify_receipt_sha256",
            "failures",
            "holds",
            "independent_fresh_observation",
            "observation_host",
            "plan_sha256",
            "provider_runtime_acceptance_verified",
            "receipt_id",
            "receipt_sha256",
            "schema",
            "source_snapshot_seal_sha256",
            "verification_role",
            "verified",
            "verified_at",
        },
        "AgentVerifyV4 receipt",
    )
    if value.get("schema") != AGENT_VERIFY_SCHEMA:
        raise BulkloadError("input is not an AgentVerifyV4 receipt")
    require_digest(value, "receipt_sha256")
    for key in ("apply_receipt_sha256", "plan_sha256"):
        if not re.fullmatch(r"[0-9a-f]{64}", value.get(key, "")):
            raise BulkloadError("AgentVerifyV4 digest binding is invalid")
    role = value.get("verification_role")
    if role not in {"destination-state", "cutover-release"} or (
        required_role is not None and role != required_role
    ):
        raise BulkloadError("AgentVerifyV4 verification role is invalid")
    if (
        not isinstance(value.get("failures"), list)
        or not isinstance(value.get("holds"), list)
        or value.get("independent_fresh_observation") is not True
        or value.get("provider_runtime_acceptance_verified") is not False
        or not isinstance(value.get("observation_host"), str)
        or not value["observation_host"]
        or not isinstance(value.get("verified"), bool)
        or value["verified"] != (not value["failures"])
    ):
        raise BulkloadError("AgentVerifyV4 receipt claim is invalid")
    destination_digest = value["destination_verify_receipt_sha256"]
    source_seal = value["source_snapshot_seal_sha256"]
    if role == "destination-state":
        if destination_digest is not None or source_seal is not None:
            raise BulkloadError("destination verification overclaims cutover release")
    elif (
        not isinstance(destination_digest, str)
        or not re.fullmatch(r"[0-9a-f]{64}", destination_digest)
        or not isinstance(source_seal, str)
        or not re.fullmatch(r"[0-9a-f]{64}", source_seal)
        or value["verified"] is not True
    ):
        raise BulkloadError("cutover-release receipt binding is invalid")


def _restore_snapshot(snapshot: dict[str, Any], rollback_root: Path) -> None:
    target = Path(snapshot["target"])
    before = snapshot["before"]
    if before is None:
        current = _current_record(target)
        if current is None:
            return
        if current["kind"] == "directory":
            try:
                target.rmdir()
            except OSError as error:
                raise BulkloadError(
                    "rollback refuses a non-empty newly created directory"
                ) from error
            fsync_directory(target.parent)
        else:
            target.unlink()
            fsync_directory(target.parent)
        return
    if before["kind"] == "directory":
        if not (target.exists() or target.is_symlink()):
            durable_makedirs(target)
        elif not target.is_dir() or target.is_symlink():
            raise BulkloadError("rollback directory target changed type")
        os.chmod(target, int(before["mode"], 8))
        fsync_directory(target)
        return
    relative = snapshot["snapshot"]
    if relative is None:
        raise BulkloadError("rollback snapshot is missing")
    stored = rollback_root / relative
    durable_makedirs(target.parent)
    temporary = target.parent / f".{target.name}.rollback-{new_id()}"
    if before["kind"] == "regular":
        reflink_clone(
            stored,
            temporary,
            expected_sha256=before["sha256"],
            mode=int(before["mode"], 8),
        )
    else:
        os.symlink(os.fsdecode(stored.read_bytes()), temporary)
    os.replace(temporary, target)
    fsync_directory(target.parent)


def _mutation_after_record(mutation: dict[str, Any]) -> dict[str, Any] | None:
    if "after" in mutation:
        return mutation["after"]
    if mutation["action"] in {"delete", "rmdir"}:
        return None
    if mutation["action"] == "mkdir":
        return {"kind": "directory", "mode": "0700", "sha256": None, "size": 0}
    entry = mutation["entry"]
    return {
        "kind": entry.get("payload_kind", "regular")
        if entry["kind"] != "sqlite"
        else "regular",
        "mode": entry["mode"],
        "sha256": entry["blob_sha256"],
        "size": entry["size"],
    }


def _validate_rollback_preconditions(journal: dict[str, Any]) -> None:
    snapshots = journal.get("rollback_snapshots", [])
    mutations = journal.get("mutations", [])
    if len(snapshots) > len(mutations):
        raise BulkloadError("rollback snapshot inventory exceeds mutations")
    for mutation, snapshot in zip(mutations, snapshots):
        if mutation["action"].startswith("preserve-git-"):
            current = _current_record(Path(mutation["target"]))
            if "after" in mutation and not (
                _same_record(current, snapshot["before"])
                or _same_record(current, mutation["after"])
            ):
                raise BulkloadError(
                    f"destination changed outside the transaction: {mutation['target']}"
                )
            continue
        current = _current_record(Path(mutation["target"]))
        if not (
            _same_record(current, snapshot["before"])
            or _same_record(current, _mutation_after_record(mutation))
        ):
            raise BulkloadError(
                f"destination changed outside the transaction: {mutation['target']}"
            )
    for repository_text, refs_before in journal.get("git_refs_before", {}).items():
        refs_after = journal.get("git_refs_after", {}).get(repository_text, {})
        repository = Path(repository_text)
        for name, before in refs_before.items():
            current = _git_ref_state(repository, [name])[name]
            allowed = (
                (before, refs_after.get(name, before))
                if not journal.get("git_prepared")
                else (refs_after.get(name, before),)
            )
            if current not in allowed:
                raise BulkloadError(f"Git ref changed outside the transaction: {name}")
    for repository_text, worktrees_before in journal.get(
        "git_worktrees_before", {}
    ).items():
        worktrees_after = journal.get("git_worktrees_after", {}).get(
            repository_text, {}
        )
        for path, before in worktrees_before.items():
            current = _git_worktree_state(Path(path))
            after = worktrees_after.get(path, before)
            if set(before) != set(after):
                raise BulkloadError("Git worktree journal state fields changed")
            head_applied = {
                key: after[key] if key in {"branch", "detached", "head"} else value
                for key, value in before.items()
            }
            allowed = (
                (after,)
                if journal.get("git_prepared")
                else (
                    before,
                    head_applied,
                    after,
                )
            )
            if current not in allowed:
                raise BulkloadError(
                    f"Git worktree changed outside the transaction: {path}"
                )


def _rollback_steps(journal: dict[str, Any]) -> list[tuple[str, Any]]:
    steps: list[tuple[str, Any]] = []
    created_directories = {
        item["target"]
        for item in journal.get("mutations", [])
        if item["action"] == "mkdir"
        or item["action"] == "preserve-git-directory"
        or item.get("entry", {}).get("payload_kind", item.get("entry", {}).get("kind"))
        == "directory"
    }

    def delayed_directory(snapshot: dict[str, Any]) -> bool:
        return (
            snapshot.get("before") is None and snapshot["target"] in created_directories
        )

    steps.extend(
        ("snapshot", item)
        for item in reversed(journal.get("rollback_snapshots", []))
        if not delayed_directory(item) and item.get("action") != "preserve-git-reflog"
    )
    steps.extend(
        ("created-worktree", item)
        for item in reversed(journal.get("created_worktrees", []))
    )
    for repository, refs in sorted(journal.get("git_refs_before", {}).items()):
        for name, state in sorted(refs.items()):
            steps.append(
                (
                    "git-ref",
                    {"name": name, "repository": repository, "state": state},
                )
            )
    for repository, worktrees in sorted(
        journal.get("git_worktrees_before", {}).items()
    ):
        for path, state in sorted(worktrees.items()):
            steps.append(
                (
                    "git-worktree",
                    {"path": path, "repository": repository, "state": state},
                )
            )
    steps.extend(
        ("snapshot", item)
        for item in reversed(journal.get("rollback_snapshots", []))
        if item.get("action") == "preserve-git-reflog"
    )
    steps.extend(
        ("created-object", item)
        for item in reversed(journal.get("created_git_objects", []))
    )
    created_roots = set(journal.get("created_git_roots", []))
    steps.extend(
        ("git-fsck", repository)
        for repository in sorted(journal.get("git_refs_before", {}))
        if repository not in created_roots
    )
    steps.extend(
        ("created-root", item)
        for item in reversed(journal.get("created_git_roots", []))
    )
    steps.extend(
        ("snapshot", item)
        for item in reversed(journal.get("rollback_snapshots", []))
        if delayed_directory(item)
    )
    return steps


def _execute_rollback_step(kind: str, payload: Any, rollback_root: Path) -> None:
    if kind == "snapshot":
        _restore_snapshot(payload, rollback_root)
        return
    if kind == "created-worktree":
        worktree = Path(payload["path"])
        if worktree.exists():
            repository = Path(payload["repository"])
            if (_resolve_git_dir(worktree) / "locked").exists():
                _git(repository, ["worktree", "unlock", os.fspath(worktree)])
            _git(worktree, ["read-tree", "HEAD"])
            _git(worktree, ["checkout-index", "-a", "-f"])
            _git(repository, ["worktree", "remove", os.fspath(worktree)])
        return
    if kind == "git-ref":
        repository = Path(payload["repository"])
        state = payload["state"]
        if _git_ref_state(repository, [payload["name"]])[payload["name"]] == state:
            return
        if state is None:
            _git(repository, ["update-ref", "--no-deref", "-d", payload["name"]])
        elif state.get("symbolic_target"):
            _git(
                repository,
                ["symbolic-ref", payload["name"], state["symbolic_target"]],
            )
        else:
            _git(
                repository,
                ["update-ref", "--no-deref", payload["name"], state["oid"]],
            )
        if _git_ref_state(repository, [payload["name"]])[payload["name"]] != state:
            raise BulkloadError("Git ref rollback did not reach its before-state")
        return
    if kind == "git-worktree":
        repository = Path(payload["repository"])
        path = Path(payload["path"])
        state = payload["state"]
        current = _git_worktree_state(path)
        if state["branch"] and current["branch"] != state["branch"]:
            _git(path, ["symbolic-ref", "HEAD", state["branch"]])
        elif (
            not state["branch"]
            and state["head"]
            and (current["branch"] is not None or current["head"] != state["head"])
        ):
            _git(path, ["update-ref", "--no-deref", "HEAD", state["head"]])
        if path != repository:
            desired_reason = state["lock_reason"]
            if state["locked"] and (
                not current["locked"] or current["lock_reason"] != desired_reason
            ):
                if current["locked"]:
                    _git(repository, ["worktree", "unlock", os.fspath(path)])
                arguments = ["worktree", "lock"]
                if desired_reason:
                    arguments.extend(["--reason", desired_reason])
                _git(repository, [*arguments, os.fspath(path)])
            elif not state["locked"] and current["locked"]:
                _git(repository, ["worktree", "unlock", os.fspath(path)])
        return
    if kind == "created-object":
        path = Path(payload["path"])
        if path.exists():
            if not path.is_file() or sha256_file(path) != payload["sha256"]:
                raise BulkloadError(
                    "transaction-created Git object changed before rollback"
                )
            path.unlink()
            fsync_directory(path.parent)
        return
    if kind == "git-fsck":
        _git(Path(payload), ["fsck", "--full", "--no-dangling"])
        return
    if kind == "created-root":
        root = Path(payload)
        artifact = (
            rollback_root
            / "created-git-roots"
            / sha256_bytes(os.fsencode(os.path.abspath(payload)))
        )
        if root.exists() and artifact.exists():
            raise BulkloadError("created Git root exists beside its rollback artifact")
        if root.exists():
            if root.stat().st_dev != rollback_root.stat().st_dev:
                raise BulkloadError("created Git root cannot be atomically rolled back")
            durable_makedirs(artifact.parent)
            os.replace(root, artifact)
            fsync_directory(root.parent)
            fsync_directory(artifact.parent)
        return
    raise BulkloadError("unknown rollback journal step")


def _rollback_crash_fence(progress: int) -> None:
    raw = os.environ.get("BULKLOAD_TEST_CRASH_ROLLBACK_AFTER")
    if raw is not None and raw.isdecimal() and progress >= int(raw):
        raise BulkloadError("injected crash after durable rollback boundary")


def _verify_rollback_end_state(journal: dict[str, Any]) -> None:
    for snapshot in journal.get("rollback_snapshots", []):
        if not _same_record(
            _current_record(Path(snapshot["target"])), snapshot["before"]
        ):
            raise BulkloadError("rollback end-state differs from its exact snapshot")

    created_roots = set(journal.get("created_git_roots", []))
    for root in created_roots:
        path = Path(root)
        if path.exists() or path.is_symlink():
            raise BulkloadError("transaction-created Git root survived rollback")
    for repository_text, refs in journal.get("git_refs_before", {}).items():
        if repository_text in created_roots:
            continue
        repository = Path(repository_text)
        for name, before in refs.items():
            if _git_ref_state(repository, [name])[name] != before:
                raise BulkloadError("Git ref differs from its rollback before-state")
    for repository_text, worktrees in journal.get("git_worktrees_before", {}).items():
        if repository_text in created_roots:
            continue
        for path, before in worktrees.items():
            if _git_worktree_state(Path(path)) != before:
                raise BulkloadError(
                    "Git worktree differs from its rollback before-state"
                )

    for item in journal.get("created_git_objects", []):
        path = Path(item["path"])
        if path.exists() or path.is_symlink():
            raise BulkloadError("transaction-created Git object survived rollback")
    topology: dict[str, set[str]] = {}
    for item in journal.get("created_worktrees", []):
        path = Path(item["path"])
        if path.exists() or path.is_symlink():
            raise BulkloadError("transaction-created Git worktree survived rollback")
        repository_text = item["repository"]
        if repository_text in created_roots:
            continue
        if repository_text not in topology:
            fields = _git(
                Path(repository_text), ["worktree", "list", "--porcelain", "-z"]
            ).split(b"\0")
            try:
                topology[repository_text] = {
                    field[9:].decode("utf-8")
                    for field in fields
                    if field.startswith(b"worktree ")
                }
            except UnicodeDecodeError as error:
                raise BulkloadError(
                    "Git worktree rollback topology is not portable"
                ) from error
        if item["path"] in topology[repository_text]:
            raise BulkloadError("transaction-created Git worktree remains registered")


def rollback_agent_apply(
    apply_receipt: dict[str, Any],
    *,
    accepted_receipt_sha256: str,
) -> dict[str, Any]:
    validate_apply_receipt(apply_receipt)
    if accepted_receipt_sha256 != apply_receipt["receipt_sha256"]:
        raise BulkloadError("accepted apply receipt digest does not match")
    journal_path = Path(apply_receipt["journal_path"])
    journal = _read_apply_journal(journal_path)
    if (
        journal.get("transaction_id") != apply_receipt["transaction_id"]
        or journal.get("apply_receipt") != apply_receipt
        or journal.get("stage_manifest_sha256")
        != apply_receipt["stage_manifest_sha256"]
    ):
        raise BulkloadError("apply receipt is detached from its journal")
    if journal.get("state") == "rolled-back":
        receipt = journal.get("rollback_receipt")
        if not isinstance(receipt, dict):
            raise BulkloadError("rolled-back journal lacks its exact receipt")
        require_digest(receipt, "receipt_sha256")
        return receipt
    rollback_root = Path(journal["rollback_root"])
    if journal.get("state") != "rolling-back":
        _validate_rollback_preconditions(journal)
        journal["rollback_progress"] = 0
        journal["state"] = "rolling-back"
        journal["updated_at"] = utc_now()
        _write_apply_journal(journal_path, journal)
    steps = _rollback_steps(journal)
    progress = int(journal.get("rollback_progress", 0))
    if progress > len(steps):
        raise BulkloadError("rollback progress exceeds its exact step inventory")
    for index in range(progress, len(steps)):
        kind, payload = steps[index]
        _execute_rollback_step(kind, payload, rollback_root)
        journal["rollback_progress"] = index + 1
        journal["updated_at"] = utc_now()
        _write_apply_journal(journal_path, journal)
        _rollback_crash_fence(index + 1)
    _verify_rollback_end_state(journal)
    receipt = {
        "apply_receipt_sha256": apply_receipt["receipt_sha256"],
        "plan_sha256": apply_receipt["plan_sha256"],
        "receipt_id": new_id(),
        "restored_entries": len(journal.get("rollback_snapshots", [])),
        "rolled_back_at": utc_now(),
        "schema": AGENT_ROLLBACK_SCHEMA,
        "transaction_id": journal["transaction_id"],
    }
    seal(receipt, "receipt_sha256")
    journal["rollback_receipt"] = receipt
    journal["state"] = "rolled-back"
    journal["updated_at"] = utc_now()
    _write_apply_journal(journal_path, journal)
    return receipt


def recover_agent_apply(
    plan: dict[str, Any],
    stage_receipt: dict[str, Any],
    *,
    journal_path: Path,
    strategy: str,
) -> dict[str, Any]:
    if strategy not in {"forward", "rollback"}:
        raise BulkloadError("recovery strategy must be forward or rollback")
    validate_agent_plan(plan, require_ready=True)
    validate_stage_receipt(stage_receipt, require_final=True)
    journal = _read_apply_journal(journal_path)
    if (
        journal.get("plan_sha256") != plan.get("plan_sha256")
        or journal.get("stage_receipt_sha256") != stage_receipt["receipt_sha256"]
        or journal.get("stage_manifest_sha256") != stage_receipt["manifest_sha256"]
    ):
        raise BulkloadError("recovery plan differs from journal authority")
    existing_recovery = journal.get("recovery_receipts", {}).get(strategy)
    if isinstance(existing_recovery, dict):
        if existing_recovery.get("schema") != AGENT_RECOVER_SCHEMA:
            raise BulkloadError("stored recovery receipt has the wrong schema")
        require_digest(existing_recovery, "receipt_sha256")
        return existing_recovery
    if strategy == "forward":
        apply_receipt = apply_agent_plan(
            plan,
            stage_receipt,
            accepted_plan_sha256=plan["plan_sha256"],
            journal_path=journal_path,
            rollback_root=Path(journal["rollback_root"]),
            reserve_bytes=0,
        )
        result_sha = apply_receipt["receipt_sha256"]
        state = "applied"
    else:
        existing = journal.get("apply_receipt")
        if not isinstance(existing, dict):
            # Mint a bounded recovery-only apply receipt authority from the
            # journal; rollback still uses only pre-mutation reflink snapshots.
            existing = {
                "applied_at": journal["updated_at"],
                "capacity": journal["capacity"],
                "holds": plan["holds"],
                "journal_path": os.fspath(journal_path),
                "mutation_count": len(journal["mutations"]),
                "plan_sha256": journal["plan_sha256"],
                "provider_runtime_acceptance_verified": False,
                "receipt_id": new_id(),
                "rollback_root": journal["rollback_root"],
                "schema": AGENT_APPLY_SCHEMA,
                "stage_manifest_sha256": journal["stage_manifest_sha256"],
                "transaction_id": journal["transaction_id"],
            }
            seal(existing, "receipt_sha256")
            journal["apply_receipt"] = existing
            _write_apply_journal(journal_path, journal)
        rollback = rollback_agent_apply(
            existing, accepted_receipt_sha256=existing["receipt_sha256"]
        )
        result_sha = rollback["receipt_sha256"]
        state = "rolled-back"
    receipt = {
        "journal_path": os.fspath(journal_path),
        "plan_sha256": plan["plan_sha256"],
        "receipt_id": new_id(),
        "result_receipt_sha256": result_sha,
        "schema": AGENT_RECOVER_SCHEMA,
        "strategy": strategy,
        "transaction_state": state,
    }
    seal(receipt, "receipt_sha256")
    journal = _read_apply_journal(journal_path)
    journal.setdefault("recovery_receipts", {})[strategy] = receipt
    journal["updated_at"] = utc_now()
    _write_apply_journal(journal_path, journal)
    return receipt
