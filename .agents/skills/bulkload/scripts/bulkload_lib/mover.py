"""Native content-addressed mover for the AgentCaptureV4 payload transport.

The shipped transport is a single-stream `rsync -a --from0 --files-from=-`
push (`executor.py:642-661`, mode `ssh-rsync-push` at `executor.py:668-670`).
It moves one copy of every *path*, so content that repeats across the corpus is
paid for once per path, and it has no resume unit smaller than the whole run.
On the measured ceremony that was 112.04 GB charged against 84.11 GB unique, so
24.9% of a 152-minute push was redundant against the source's own content
(VERDICTS.md, LANE F).

This module is the alternative mover. Its unit of work is the *object* — one
sha256 identity — not the path:

* **Deduplicated.** Paths that share a digest are transferred once and
  materialised at the destination by `os.link`, so the wire carries the unique
  byte count, not the charged byte count.
* **Resumable.** The receiver answers each offer with `have` or `send` after
  proving the destination copy by size *and* digest, so a re-run after any
  failure transfers only what is missing. There is no run-scoped state to
  reconcile: the destination stage itself is the checkpoint.
* **Parallel.** N independent streams, each its own transport channel (one ssh
  process per stream), pulling from one shared work iterator so a slow object
  cannot idle the other streams.
* **Fail-closed.** Every received object is re-hashed before it is published;
  a digest, size, mode, or containment violation aborts the stream with a
  named reason rather than publishing an unverified byte.

The module deliberately has **no intra-package imports**: the receiver runs on
the destination as `python3 -I -S <path>/mover.py --receive --root <dir>`, and
`-I` neither prepends the script directory to `sys.path` nor honours
`PYTHONPATH`, so a receiver that needed `bulkload_lib` could not start. It
raises `MoverError`; `executor.py` converts that to `BulkloadError` at the
call site.

What this module does NOT do, stated plainly because the transport contract
depends on it: it does not preserve mtimes, hardlink topology, xattrs, ACLs,
or device/special files, and it creates implicit parent directories at 0o700
rather than at the source mode. The quarantine is re-derived by
`validate_snapshot_custody` on kind, mode, size, and sha256 only, so none of
those are custody-load-bearing there — but they are `rsync -a` behaviours the
native mover does not reproduce, and a caller outside the quarantine path must
not assume them.
"""

from __future__ import annotations

import argparse
from collections.abc import Callable, Iterable, Iterator, Sequence
from dataclasses import dataclass
import hashlib
import heapq
import json
import os
from pathlib import Path
import re
import stat
import struct
import subprocess
import sys
import tempfile
import threading
import time
from typing import IO


MOVER_PROTOCOL = 1
DEFAULT_STREAMS = 8
MAX_STREAMS = 64
COPY_BLOCK = 1024 * 1024
MAX_FRAME_BYTES = 4 * 1024 * 1024
MAX_OBJECT_BYTES = 1 << 42
SPILL_BLOCK_RECORDS = 200_000
DEFAULT_STALL_SECONDS = 900.0
HEARTBEAT_SECONDS = 15.0
MODE = re.compile(r"[0-7]{4}")
DIGEST = re.compile(r"[0-9a-f]{64}")


class MoverError(RuntimeError):
    """A fail-closed native transport error."""


@dataclass(frozen=True)
class MoveObject:
    """One transfer unit.

    ``paths`` are '/'-relative POSIX paths in the same canonical form the
    transport allowlist uses (`executor.py:264`). The first path is the
    representative: it receives the bytes, and every other path is linked to
    it at the destination.
    """

    kind: str
    mode: str
    paths: tuple[str, ...]
    digest: str | None = None
    size: int = 0
    target: str | None = None

    def header(self) -> dict[str, object]:
        value: dict[str, object] = {
            "op": "object",
            "kind": self.kind,
            "mode": self.mode,
            "paths": list(self.paths),
        }
        if self.kind == "regular":
            value["digest"] = self.digest
            value["size"] = self.size
        elif self.kind == "symlink":
            value["digest"] = self.digest
            value["target"] = self.target
        return value


@dataclass(frozen=True)
class Heartbeat:
    """Per-stream liveness sample; emitted at least every object."""

    stream: int
    phase: str
    objects_done: int
    objects_skipped: int
    bytes_sent: int
    monotonic: float


@dataclass(frozen=True)
class MoveSummary:
    objects: int
    paths: int
    objects_sent: int
    objects_skipped: int
    bytes_sent: int
    bytes_deduplicated: int
    streams: int
    seconds: float


# ---------------------------------------------------------------------------
# Framing
# ---------------------------------------------------------------------------


def _encode(value: dict[str, object]) -> bytes:
    return json.dumps(
        value, sort_keys=True, separators=(",", ":"), ensure_ascii=True
    ).encode("ascii")


def _write_frame(stream: IO[bytes], value: dict[str, object]) -> None:
    payload = _encode(value)
    if len(payload) > MAX_FRAME_BYTES:
        raise MoverError("native transport frame exceeds the bounded contract")
    stream.write(struct.pack(">I", len(payload)))
    stream.write(payload)
    stream.flush()


def _read_exact(stream: IO[bytes], count: int) -> bytes:
    chunks: list[bytes] = []
    remaining = count
    while remaining > 0:
        chunk = stream.read(remaining)
        if not chunk:
            raise MoverError("native transport stream closed mid-frame")
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


def _read_frame(stream: IO[bytes]) -> dict[str, object] | None:
    header = stream.read(4)
    if not header:
        return None
    if len(header) < 4:
        header += _read_exact(stream, 4 - len(header))
    (size,) = struct.unpack(">I", header)
    if size > MAX_FRAME_BYTES:
        raise MoverError("native transport frame exceeds the bounded contract")
    try:
        value = json.loads(_read_exact(stream, size).decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise MoverError("native transport frame is not canonical JSON") from error
    if not isinstance(value, dict):
        raise MoverError("native transport frame is not an object")
    return value


# ---------------------------------------------------------------------------
# Shared validation
# ---------------------------------------------------------------------------


def _relative(value: object) -> str:
    if not isinstance(value, str) or not value or value.startswith("/"):
        raise MoverError("native transport path is not relative")
    if any(part in {"", ".", ".."} for part in value.split("/")):
        raise MoverError("native transport path is not canonical")
    return value


def _within(root: Path, relative: str) -> Path:
    return root.joinpath(*_relative(relative).split("/"))


def _mode(value: object) -> int:
    if not isinstance(value, str) or not MODE.fullmatch(value):
        raise MoverError("native transport mode is invalid")
    return int(value, 8)


def _digest(value: object) -> str:
    if not isinstance(value, str) or not DIGEST.fullmatch(value):
        raise MoverError("native transport digest is invalid")
    return value


def _size(value: object) -> int:
    if not isinstance(value, int) or isinstance(value, bool):
        raise MoverError("native transport size is invalid")
    if not 0 <= value <= MAX_OBJECT_BYTES:
        raise MoverError("native transport size is outside the bounded contract")
    return value


def _paths(value: object) -> list[str]:
    if not isinstance(value, list) or not value:
        raise MoverError("native transport object carries no path")
    paths = [_relative(item) for item in value]
    if len(set(paths)) != len(paths):
        raise MoverError("native transport object repeats a path")
    return paths


def _hash_file(path: Path) -> tuple[str, int]:
    digest = hashlib.sha256()
    total = 0
    with path.open("rb", buffering=0) as stream:
        while chunk := stream.read(COPY_BLOCK):
            digest.update(chunk)
            total += len(chunk)
    return digest.hexdigest(), total


def _fsync_directory(path: Path) -> None:
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


# ---------------------------------------------------------------------------
# Receiver
# ---------------------------------------------------------------------------


def _ensure_parent(root: Path, path: Path) -> None:
    if path.parent != root:
        path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)


def _matches(path: Path, digest: str, size: int, mode: int) -> bool:
    try:
        info = path.lstat()
    except FileNotFoundError:
        return False
    if not stat.S_ISREG(info.st_mode) or info.st_size != size:
        return False
    if stat.S_IMODE(info.st_mode) != mode:
        return False
    return _hash_file(path)[0] == digest


def _link_aliases(root: Path, paths: Sequence[str], digest: str, size: int, mode: int):
    primary = _within(root, paths[0])
    for alias in paths[1:]:
        target = _within(root, alias)
        _ensure_parent(root, target)
        try:
            os.link(primary, target)
        except FileExistsError:
            if not _matches(target, digest, size, mode):
                raise MoverError(
                    f"destination path holds different content: {alias}"
                ) from None
        except OSError as error:
            raise MoverError(f"cannot link destination path: {alias}") from error
        else:
            _fsync_directory(target.parent)


def _receive_regular(
    root: Path, header: dict[str, object], reader: IO[bytes]
) -> tuple[int, Path]:
    paths = _paths(header.get("paths"))
    digest = _digest(header.get("digest"))
    size = _size(header.get("size"))
    mode = _mode(header.get("mode"))
    primary = _within(root, paths[0])
    _ensure_parent(root, primary)
    # Deterministic, not mkstemp. Each object has exactly one primary path and
    # exactly one stream owns it, so this name cannot collide; and because it
    # is derived from the path rather than randomised, a resume run reopens
    # and truncates the partial that a killed run left behind instead of
    # stranding it. That matters here specifically: the destination custody
    # gate compares the quarantine namespace against the sealed index and
    # fails closed on any extra entry (`scanner.py:3161-3178`), so a mover
    # that accumulated one stray temp file per interrupted object would
    # convert every resume into a custody failure.
    temporary = primary.parent / f".{primary.name}.mover-part"
    descriptor = os.open(
        temporary,
        os.O_WRONLY
        | os.O_CREAT
        | os.O_TRUNC
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_CLOEXEC", 0),
        0o600,
    )
    try:
        observed = hashlib.sha256()
        remaining = size
        with os.fdopen(descriptor, "wb", buffering=0, closefd=True) as output:
            while remaining > 0:
                chunk = reader.read(min(COPY_BLOCK, remaining))
                if not chunk:
                    raise MoverError("native transport payload ended early")
                observed.update(chunk)
                output.write(chunk)
                remaining -= len(chunk)
            output.flush()
            os.fsync(output.fileno())
        if observed.hexdigest() != digest:
            raise MoverError("native transport payload digest mismatch")
        os.chmod(temporary, mode)
        os.replace(temporary, primary)
        _fsync_directory(primary.parent)
    except BaseException:
        temporary.unlink(missing_ok=True)
        raise
    _link_aliases(root, paths, digest, size, mode)
    return size, primary


def _receive_symlink(root: Path, header: dict[str, object]) -> None:
    paths = _paths(header.get("paths"))
    digest = _digest(header.get("digest"))
    target = header.get("target")
    if not isinstance(target, str) or not target:
        raise MoverError("native transport symlink target is invalid")
    if hashlib.sha256(os.fsencode(target)).hexdigest() != digest:
        raise MoverError("native transport symlink digest mismatch")
    for relative in paths:
        path = _within(root, relative)
        _ensure_parent(root, path)
        try:
            os.symlink(target, path)
        except FileExistsError:
            if not path.is_symlink() or os.readlink(path) != target:
                raise MoverError(
                    f"destination path holds a different link: {relative}"
                ) from None
        except OSError as error:
            raise MoverError(f"cannot create destination link: {relative}") from error


def _receive_directory(root: Path, header: dict[str, object]) -> None:
    mode = _mode(header.get("mode"))
    for relative in _paths(header.get("paths")):
        path = _within(root, relative)
        _ensure_parent(root, path)
        path.mkdir(mode=mode, exist_ok=True)
        if path.is_symlink() or not path.is_dir():
            raise MoverError(f"destination path is not a directory: {relative}")
        os.chmod(path, mode)


def receiver_source_sha256() -> str:
    """The digest of the receiver's own source, as it is running.

    The sender compares this to the digest of the file it bootstrapped. It is
    the mover's local answer to the same question `runtime_source_digest()`
    (`model.py:88-101`) answers for the rest of the engine: a receiver whose
    source nobody pinned is a hole in the transport, and there is no remote
    hashing tool this can lean on.
    """
    try:
        return hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
    except OSError as error:
        raise MoverError("native transport receiver cannot pin its own source") from (
            error
        )


def receive(root: Path, reader: IO[bytes], writer: IO[bytes]) -> dict[str, int]:
    """Serve one sender stream against ``root``. Never replaces live state."""
    root = Path(os.path.abspath(os.fspath(root)))
    if root.is_symlink() or not root.is_dir():
        raise MoverError("native mover receive root is not a directory")
    _write_frame(
        writer,
        {
            "status": "hello",
            "protocol": MOVER_PROTOCOL,
            "source_sha256": receiver_source_sha256(),
        },
    )
    written = 0
    skipped = 0
    received = 0
    while True:
        header = _read_frame(reader)
        if header is None:
            raise MoverError("native transport ended without a done frame")
        operation = header.get("op")
        try:
            if operation == "done":
                summary = {
                    "bytes": received,
                    "skipped": skipped,
                    "written": written,
                }
                _write_frame(writer, {"status": "bye", **summary})
                return summary
            if operation != "object":
                raise MoverError("native transport operation is unsupported")
            kind = header.get("kind")
            if kind == "directory":
                _receive_directory(root, header)
                skipped += 1
                _write_frame(writer, {"status": "have"})
                continue
            if kind == "symlink":
                _receive_symlink(root, header)
                skipped += 1
                _write_frame(writer, {"status": "have"})
                continue
            if kind != "regular":
                raise MoverError("native transport object kind is unsupported")
            paths = _paths(header.get("paths"))
            digest = _digest(header.get("digest"))
            size = _size(header.get("size"))
            mode = _mode(header.get("mode"))
            if _matches(_within(root, paths[0]), digest, size, mode):
                _link_aliases(root, paths, digest, size, mode)
                skipped += 1
                _write_frame(writer, {"status": "have"})
                continue
            _write_frame(writer, {"status": "send"})
            transferred, _ = _receive_regular(root, header, reader)
            received += transferred
            written += 1
            _write_frame(writer, {"status": "ok", "digest": digest})
        except MoverError as error:
            _write_frame(writer, {"status": "error", "reason": str(error)})
            raise
        except OSError as error:
            _write_frame(writer, {"status": "error", "reason": "destination IO failed"})
            raise MoverError("native transport destination IO failed") from error


# ---------------------------------------------------------------------------
# Planning
# ---------------------------------------------------------------------------


class _SpillSorter:
    """Bounded-memory sort: fixed-size sorted blocks, merged on read.

    The corpus this mover exists for is 1.78M transport paths / 403 MB of
    path bytes (LANE F, VERDICTS.md), so the object plan cannot be a dict in
    RAM. Blocks spill to ``directory`` and merge with ``heapq.merge``.
    """

    def __init__(self, directory: Path | None, block: int = SPILL_BLOCK_RECORDS):
        self._directory = directory
        self._block = max(1, block)
        self._buffer: list[str] = []
        self._runs: list[Path] = []

    def add(self, line: str) -> None:
        self._buffer.append(line)
        if len(self._buffer) >= self._block and self._directory is not None:
            self._flush()

    def _flush(self) -> None:
        if not self._buffer or self._directory is None:
            return
        self._buffer.sort()
        descriptor, name = tempfile.mkstemp(
            prefix=f"mover-run-{len(self._runs):05d}-", dir=self._directory
        )
        with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
            for line in self._buffer:
                stream.write(line + "\n")
        self._runs.append(Path(name))
        self._buffer.clear()

    def __iter__(self) -> Iterator[str]:
        self._buffer.sort()
        if not self._runs:
            yield from self._buffer
            return
        self._flush()
        handles = [path.open("r", encoding="utf-8") for path in self._runs]
        try:
            for line in heapq.merge(
                *((line.rstrip("\n") for line in h) for h in handles)
            ):
                yield line
        finally:
            for handle in handles:
                handle.close()
            for path in self._runs:
                path.unlink(missing_ok=True)
            self._runs.clear()

    def close(self) -> None:
        for path in self._runs:
            path.unlink(missing_ok=True)
        self._runs.clear()
        self._buffer.clear()


def plan_objects(
    source_root: Path,
    relatives: Iterable[str],
    *,
    spill_dir: Path | None = None,
    block: int = SPILL_BLOCK_RECORDS,
    digests: Callable[[str], tuple[str, int] | None] | None = None,
) -> Iterator[MoveObject]:
    """Yield the transfer plan: directories first, then objects, then links.

    Directories are emitted before any payload because the receiver only
    creates implicit parents at 0o700; an explicit directory object is what
    carries the source mode. They are emitted in sorted order, so a parent
    always precedes its children.

    ``digests`` is the seam that makes this pass free in the real pipeline.
    Every payload path in the transport allowlist already has a sealed
    ``sha256`` and ``size`` in the capture's ``snapshot-index.jsonl``
    (`scanner.py:2437-2446`), so a caller that can look a path up there hands
    the digest over and this function never opens the file. When it returns
    ``None`` — or is not supplied at all — the file is hashed here, which is
    the single-threaded 4,523 files/s path the review measured. The wiring is
    NOT built in this change; see `docs/design/native-mover.md` §6.
    """
    source_root = Path(os.path.abspath(os.fspath(source_root)))
    directories = _SpillSorter(spill_dir, block)
    regulars = _SpillSorter(spill_dir, block)
    links = _SpillSorter(spill_dir, block)
    try:
        for relative in relatives:
            relative = _relative(relative)
            path = source_root.joinpath(*relative.split("/"))
            try:
                info = path.lstat()
            except OSError as error:
                raise MoverError(f"cannot inspect source path: {relative}") from error
            mode = f"{stat.S_IMODE(info.st_mode):04o}"
            if stat.S_ISDIR(info.st_mode):
                # Fail closed at plan time, before a byte moves, rather than
                # mid-stream: this mover writes children into a directory it
                # has already set to the source mode, so a source directory
                # without owner write+execute would strand its own subtree.
                # `rsync -a` defers directory permissions to the end of the
                # run and does not have this constraint. Measured on two real
                # corpora (~/.claude/projects, 1,969 directories; a bulkload
                # checkout, 373) the offending count is 0, which is why the
                # gap is a documented refusal and not a protocol phase.
                if stat.S_IMODE(info.st_mode) & 0o300 != 0o300:
                    raise MoverError(
                        f"source directory is not owner-writable: {relative}"
                    )
                directories.add(f"{json.dumps(relative)}\t{mode}")
            elif stat.S_ISLNK(info.st_mode):
                target = os.readlink(path)
                digest = hashlib.sha256(os.fsencode(target)).hexdigest()
                links.add(
                    f"{json.dumps(relative)}\t{digest}\t{json.dumps(target)}\t{mode}"
                )
            elif stat.S_ISREG(info.st_mode):
                sealed = None if digests is None else digests(relative)
                if sealed is None:
                    digest, size = _hash_file(path)
                else:
                    digest, size = _digest(sealed[0]), _size(sealed[1])
                    if size != info.st_size:
                        raise MoverError(
                            f"sealed size differs from the source file: {relative}"
                        )
                regulars.add(f"{digest} {size:020d} {mode} {json.dumps(relative)}")
            else:
                raise MoverError(f"source path is a special entry: {relative}")
        for line in directories:
            relative, mode = line.split("\t")
            yield MoveObject(kind="directory", mode=mode, paths=(json.loads(relative),))
        group: list[str] = []
        current: tuple[str, int, str] | None = None
        for line in regulars:
            digest, size, mode, relative = line.split(" ", 3)
            key = (digest, int(size), mode)
            if current is not None and key != current:
                yield MoveObject(
                    kind="regular",
                    mode=current[2],
                    paths=tuple(group),
                    digest=current[0],
                    size=current[1],
                )
                group = []
            current = key
            group.append(json.loads(relative))
        if current is not None:
            yield MoveObject(
                kind="regular",
                mode=current[2],
                paths=tuple(group),
                digest=current[0],
                size=current[1],
            )
        for line in links:
            relative, digest, target, mode = line.split("\t")
            yield MoveObject(
                kind="symlink",
                mode=mode,
                paths=(json.loads(relative),),
                digest=digest,
                target=json.loads(target),
            )
    finally:
        directories.close()
        regulars.close()
        links.close()


# ---------------------------------------------------------------------------
# Channels
# ---------------------------------------------------------------------------


class SubprocessChannel:
    """One receiver process; its stdin/stdout are the stream.

    Two deliberate choices, both learned from the shipped transport:

    * ``bufsize`` is left at the default so ``stdin`` is a ``BufferedWriter``.
      A ``bufsize=0`` pipe hands back a raw ``FileIO`` whose ``write`` is
      permitted to be short, and a silently short frame write is the one
      failure this protocol cannot detect.
    * ``stderr`` goes to a temporary file, not a pipe. Nothing in the sender
      loop drains a stderr pipe, so a receiver — or an ``ssh`` in front of it —
      that emits more than one pipe buffer of diagnostics would deadlock the
      stream it is trying to report on. The file is read back on close so the
      reason survives into the raised error, which is the AX gap the review
      names for the rsync push (`logs/preseed-push.log` is 0 bytes for a
      2h32m / 84 GiB push).
    """

    def __init__(self, argv: Sequence[str], *, env: dict[str, str] | None = None):
        self._argv = list(argv)
        self._env = env
        self._process: subprocess.Popen[bytes] | None = None
        self._errors: IO[bytes] | None = None

    def open(self) -> tuple[IO[bytes], IO[bytes]]:
        self._errors = tempfile.TemporaryFile(prefix="bulkload-mover-stderr-")
        self._process = subprocess.Popen(
            self._argv,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=self._errors,
            env=self._env,
        )
        assert self._process.stdin is not None and self._process.stdout is not None
        return self._process.stdin, self._process.stdout

    def diagnostics(self) -> str:
        errors = self._errors
        if errors is None or errors.closed:
            return ""
        try:
            errors.seek(0)
            return errors.read(4096).decode("utf-8", errors="replace").strip()
        except (OSError, ValueError):
            return ""

    def kill(self) -> None:
        """Break a blocked stream. Safe to call from the watchdog thread."""
        process = self._process
        if process is None:
            return
        try:
            process.kill()
        except OSError:
            pass

    def close(self) -> None:
        process = self._process
        if process is None:
            return
        self._process = None
        reason = ""
        try:
            try:
                if process.stdin is not None and not process.stdin.closed:
                    process.stdin.close()
                code = process.wait(timeout=120)
            except (OSError, ValueError, subprocess.TimeoutExpired):
                process.kill()
                process.wait()
                reason = self.diagnostics()
                raise MoverError(
                    f"native transport receiver did not exit cleanly: {reason}"
                    if reason
                    else "native transport receiver did not exit cleanly"
                ) from None
            if code != 0:
                reason = self.diagnostics()
                raise MoverError(
                    f"native transport receiver exited {code}: {reason}"
                    if reason
                    else f"native transport receiver exited {code}"
                )
        finally:
            for handle in (process.stdout, self._errors):
                if handle is not None and not handle.closed:
                    handle.close()
            self._errors = None


def ssh_channel_factory(
    ssh_path: str,
    ssh_options: Sequence[str],
    host: str,
    receiver_argv: Sequence[str],
    *,
    env: dict[str, str] | None = None,
) -> Callable[[int], SubprocessChannel]:
    argv = [ssh_path, *ssh_options, "--", host, *receiver_argv]

    def factory(_stream: int) -> SubprocessChannel:
        return SubprocessChannel(argv, env=env)

    return factory


def receiver_argv(python_path: str, module_path: str, root: str) -> list[str]:
    """The remote command line. `-I -S` matches the engine's launcher contract.

    `-I` also means the receiver's own directory is not on `sys.path`, which is
    why this module has no intra-package imports (see the module docstring).
    """
    return [python_path, "-I", "-S", module_path, "--receive", "--root", root]


def local_channel_factory(
    python_path: str, module_path: str, root: str
) -> Callable[[int], SubprocessChannel]:
    """A receiver on this host. Used by the tests and the local benchmark.

    Local-to-local is the only shape the benchmark measures: it removes the
    wire, which LANE F measures at 14.8-18.5 MiB/s neo->sting and calls 83% of
    the cold floor (VERDICTS.md, LANE F). What is left is the part this module
    can actually change — per-object protocol cost, dedup, and parallelism.
    """
    argv = receiver_argv(python_path, module_path, root)

    def factory(_stream: int) -> SubprocessChannel:
        return SubprocessChannel(argv)

    return factory


def bootstrap_receiver(
    ssh_path: str,
    ssh_options: Sequence[str],
    host: str,
    remote_path: str,
    *,
    env: dict[str, str] | None = None,
) -> str:
    """Install this module on ``host`` at ``remote_path``; return its digest.

    The engine pins its own application source into every capture and plan
    (`AGENTS.md`, "Hard rules"), but the receiver is a *remote* process, and
    there is no remote hashing tool this can depend on. So the sender ships
    the bytes it is running and then makes the receiver prove, in its hello
    frame, that those are the bytes it loaded. Nothing else is trusted about
    the destination's copy.
    """
    payload = Path(__file__).read_bytes()
    quoted = "'" + remote_path.replace("'", "'\\''") + "'"
    result = subprocess.run(
        [
            ssh_path,
            *ssh_options,
            "--",
            host,
            f"umask 077 && cat > {quoted}.part && mv -f {quoted}.part {quoted}",
        ],
        input=payload,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
        env=env,
    )
    if result.returncode != 0:
        raise MoverError("native transport receiver bootstrap failed")
    return hashlib.sha256(payload).hexdigest()


# ---------------------------------------------------------------------------
# Sender
# ---------------------------------------------------------------------------


class _Work:
    """Shared pull-iterator: one slow object cannot idle the other streams."""

    def __init__(self, objects: Iterator[MoveObject]):
        self._objects = objects
        self._lock = threading.Lock()

    def next(self) -> MoveObject | None:
        with self._lock:
            return next(self._objects, None)


def _send_object(
    writer: IO[bytes],
    reader: IO[bytes],
    source_root: Path,
    item: MoveObject,
    *,
    abort: threading.Event,
    progress: Callable[[int], None],
) -> tuple[int, bool]:
    _write_frame(writer, item.header())
    reply = _read_frame(reader)
    if reply is None:
        raise MoverError("native transport receiver closed before replying")
    status = reply.get("status")
    if status == "error":
        raise MoverError(f"destination refused the object: {reply.get('reason')}")
    if status == "have":
        return 0, False
    if status != "send":
        raise MoverError("native transport reply is unsupported")
    path = source_root.joinpath(*item.paths[0].split("/"))
    observed = hashlib.sha256()
    sent = 0
    with path.open("rb", buffering=0) as stream:
        while sent < item.size:
            if abort.is_set():
                raise MoverError("native transport aborted")
            chunk = stream.read(min(COPY_BLOCK, item.size - sent))
            if not chunk:
                raise MoverError(f"source shrank during transfer: {item.paths[0]}")
            observed.update(chunk)
            writer.write(chunk)
            sent += len(chunk)
            progress(len(chunk))
    writer.flush()
    if observed.hexdigest() != item.digest:
        raise MoverError(f"source bytes changed during transfer: {item.paths[0]}")
    reply = _read_frame(reader)
    if reply is None or reply.get("status") != "ok":
        reason = None if reply is None else reply.get("reason")
        raise MoverError(f"destination rejected the object: {reason}")
    return sent, True


def _open_stream(
    channel: SubprocessChannel, expected_source_sha256: str | None
) -> tuple[IO[bytes], IO[bytes]]:
    """Open a channel and refuse a receiver that is not the pinned source."""
    writer, reader = channel.open()
    hello = _read_frame(reader)
    if hello is None or hello.get("status") != "hello":
        reason = channel.diagnostics()
        raise MoverError(
            f"native transport receiver did not announce itself: {reason}"
            if reason
            else "native transport receiver did not announce itself"
        )
    if hello.get("protocol") != MOVER_PROTOCOL:
        raise MoverError("native transport receiver speaks a different protocol")
    observed = hello.get("source_sha256")
    if not isinstance(observed, str) or not DIGEST.fullmatch(observed):
        raise MoverError("native transport receiver source digest is invalid")
    if expected_source_sha256 is not None and observed != expected_source_sha256:
        raise MoverError("native transport receiver source differs from the sender's")
    return writer, reader


def push(
    *,
    source_root: Path,
    objects: Iterable[MoveObject],
    channel_factory: Callable[[int], SubprocessChannel],
    streams: int = DEFAULT_STREAMS,
    heartbeat: Callable[[Heartbeat], None] | None = None,
    stall_seconds: float = DEFAULT_STALL_SECONDS,
    expected_receiver_sha256: str | None = None,
) -> MoveSummary:
    """Move ``objects`` from ``source_root`` over ``streams`` channels."""
    if not isinstance(streams, int) or isinstance(streams, bool):
        raise MoverError("native transport stream count is invalid")
    if not 1 <= streams <= MAX_STREAMS:
        raise MoverError("native transport stream count is outside the contract")
    source_root = Path(os.path.abspath(os.fspath(source_root)))
    started = time.monotonic()
    iterator = iter(objects)
    prelude: list[MoveObject] = []
    first: MoveObject | None = None
    for item in iterator:
        if item.kind == "directory":
            prelude.append(item)
            continue
        first = item
        break

    def replay() -> Iterator[MoveObject]:
        if first is not None:
            yield first
        yield from iterator

    work = _Work(replay())
    abort = threading.Event()
    lock = threading.Lock()
    totals = {
        "objects": 0,
        "paths": 0,
        "sent": 0,
        "skipped": 0,
        "bytes": 0,
        "deduplicated": 0,
    }
    failures: list[BaseException] = []
    # `inf` means "this stream is finished and can never stall again". Without
    # it the watchdog reads `min(touched)` off a stream that exited an hour ago
    # and aborts a run whose surviving streams are making steady progress.
    touched = [time.monotonic()] * streams
    channels: list[SubprocessChannel | None] = [None] * streams

    def account(item: MoveObject, transferred: int, written: bool) -> None:
        with lock:
            totals["objects"] += 1
            totals["paths"] += len(item.paths)
            totals["bytes"] += transferred
            if item.kind == "regular":
                totals["deduplicated"] += item.size * (len(item.paths) - 1)
            if written:
                totals["sent"] += 1
            else:
                totals["skipped"] += 1

    def worker(stream: int) -> None:
        channel = channel_factory(stream)
        channels[stream] = channel
        done = 0
        skipped = 0
        sent_bytes = 0
        last_beat = 0.0
        try:
            writer, reader = _open_stream(channel, expected_receiver_sha256)
            touched[stream] = time.monotonic()
            while not abort.is_set():
                item = work.next()
                if item is None:
                    break

                def progress(count: int, stream=stream) -> None:
                    touched[stream] = time.monotonic()

                transferred, written = _send_object(
                    writer,
                    reader,
                    source_root,
                    item,
                    abort=abort,
                    progress=progress,
                )
                done += 1
                sent_bytes += transferred
                if not written:
                    skipped += 1
                touched[stream] = time.monotonic()
                account(item, transferred, written)
                now = time.monotonic()
                if heartbeat is not None and now - last_beat >= HEARTBEAT_SECONDS:
                    last_beat = now
                    heartbeat(
                        Heartbeat(
                            stream=stream,
                            phase="send",
                            objects_done=done,
                            objects_skipped=skipped,
                            bytes_sent=sent_bytes,
                            monotonic=now,
                        )
                    )
            _write_frame(writer, {"op": "done"})
            reply = _read_frame(reader)
            if reply is None or reply.get("status") != "bye":
                raise MoverError("native transport receiver did not acknowledge done")
            channel.close()
        except BaseException as error:  # noqa: BLE001 - recorded, then re-raised
            abort.set()
            with lock:
                failures.append(error)
            try:
                channel.close()
            except BaseException:  # noqa: BLE001 - the first failure is the truth
                pass
        finally:
            touched[stream] = float("inf")
            channels[stream] = None
            if heartbeat is not None:
                heartbeat(
                    Heartbeat(
                        stream=stream,
                        phase="done",
                        objects_done=done,
                        objects_skipped=skipped,
                        bytes_sent=sent_bytes,
                        monotonic=time.monotonic(),
                    )
                )

    if prelude:
        channel = channel_factory(0)
        try:
            writer, reader = _open_stream(channel, expected_receiver_sha256)
            for item in prelude:
                transferred, written = _send_object(
                    writer,
                    reader,
                    source_root,
                    item,
                    abort=abort,
                    progress=lambda _count: None,
                )
                account(item, transferred, written)
            _write_frame(writer, {"op": "done"})
            reply = _read_frame(reader)
            if reply is None or reply.get("status") != "bye":
                raise MoverError("native transport receiver did not acknowledge done")
        finally:
            channel.close()

    threads = [
        threading.Thread(
            target=worker,
            args=(index,),
            name=f"bulkload-mover-{index}",
            # A stream blocked in `read()` on a wedged receiver does not
            # observe `abort`. The watchdog kills its channel, which unblocks
            # it; `daemon` is the backstop so a receiver that survives even
            # that cannot hold the process open forever.
            daemon=True,
        )
        for index in range(streams)
    ]
    for thread in threads:
        thread.start()
    while any(thread.is_alive() for thread in threads):
        for thread in threads:
            thread.join(timeout=0.25)
        now = time.monotonic()
        live = [value for value in touched if value != float("inf")]
        if not live:
            continue
        idle = now - min(live)
        if stall_seconds > 0 and idle > stall_seconds:
            abort.set()
            with lock:
                failures.append(
                    MoverError(
                        f"native transport stalled: no stream progressed for "
                        f"{idle:.0f}s"
                    )
                )
            for channel in channels:
                if channel is not None:
                    channel.kill()
            break
    for thread in threads:
        thread.join(timeout=60)
    if failures:
        first_failure = failures[0]
        if isinstance(first_failure, MoverError):
            raise first_failure
        raise MoverError(f"native transport failed: {first_failure}") from first_failure
    return MoveSummary(
        objects=totals["objects"],
        paths=totals["paths"],
        objects_sent=totals["sent"],
        objects_skipped=totals["skipped"],
        bytes_sent=totals["bytes"],
        bytes_deduplicated=totals["deduplicated"],
        streams=streams,
        seconds=time.monotonic() - started,
    )


# ---------------------------------------------------------------------------
# Receiver entry point
# ---------------------------------------------------------------------------


def receive_main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="bulkload-mover",
        description="Serve one native bulkload transport stream on stdin/stdout.",
    )
    parser.add_argument(
        "--receive",
        action="store_true",
        required=True,
        help="Required: this executable has no mode other than receiving.",
    )
    parser.add_argument(
        "--root",
        required=True,
        help="Absolute destination quarantine root this stream may write.",
    )
    arguments = parser.parse_args(argv)
    try:
        receive(Path(arguments.root), sys.stdin.buffer, sys.stdout.buffer)
    except MoverError as error:
        print(f"bulkload-mover: {error}", file=sys.stderr)
        return 3
    except OSError as error:
        print(f"bulkload-mover: destination IO failed: {error}", file=sys.stderr)
        return 4
    return 0


if __name__ == "__main__":
    raise SystemExit(receive_main())
