#!/usr/bin/env python3
"""Shared pieces of the cross-host link harness (m3_link.py, #246).

Retired from gate_b.py, the S1 gate (b) harness, under N03 (Linear TIN-4543,
63feb3cf, 2026-10-17): gate_b.py, m0_gate_a.py and their tests and recipes
were deleted, and end-to-end seat time is the gate of record (R-C842,
ace8649c); the r23 A/B corpus race stays only as a regression guard. What
is left here is exactly what m3_link.py imports, and what that needs: the
gated-mode runner, --coordinator-quiet (R-N91), the neo-side corpus staging
and the gate-b CLI are gone.

What stays:
  - REMOTE_HELPER: the source-side helper program (Python 3.8+, standard
    library only), sent on stdin to `<python> -I -` over the one ssh
    transport and executed locally to hash destinations, so both sides hash
    one way. Its ops: conditions, probe, prepare (a private working copy by
    explicit read and write, never `shutil` or `cp`, refused when FIEMAP
    shows it sharing an extent, OI-1003-Q165 (b)), measure, manifest and xor.
    m3_link.py appends its own ops to it.
  - Transport: `<ssh> [-F CONFIG] -T -oBatchMode=yes -oConnectTimeout=15 --
    HOST <command>`, the helper call and the `dd` link calibration.
  - The rclone sftp remote (external: the same OpenSSH command line as the
    native arm; internal: rclone's own ssh from `ssh -G`), built so that no
    password, passphrase, PEM or token reaches the config, argv, logs or
    JSON (`redact`, `redact_argv`, `scrub`, `rclone_env`, a 0600 config).
  - The native arm's argv and output parsing, the refusal check (the agent's
    pull may refuse exactly the excluded SQLite seats, SQLITE_STATE_CHANGED),
    tree verification, the destination disk budget (the agent's 25 % free
    floor, space.rs) and the R-N81 gating flag of a sample.
  - LOOPBACK_SHIM: the one-host loopback ssh shim of a dry run.

The helper's output prefix (`gate-b-helper`) and the serve wrapper's
`gate_b_serve` line are wire tokens between these programs; they keep their
names.
"""

from __future__ import annotations

import dataclasses
import json
import os
import re
import shlex
import statistics
import subprocess
import sys
import threading
import time
from pathlib import Path

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
import estate_corpus as ec  # noqa: E402
import r23_ab as ab  # noqa: E402

LOAD_LIMIT = ab.LOAD_LIMIT
RSS_CAP_BYTES = 2 << 30
# The agent's default destination floor (space.rs DEFAULT_MIN_FREE_PERCENT):
# pull refuses DESTINATION_SPACE_INSUFFICIENT when a write would leave less.
# The harness runs pull with that default, so its budget uses the same floor.
AGENT_MIN_FREE_PERCENT = 25
# Budget estimate per destination entry (block rounding) and for the whole
# sample (states, logs, rclone.conf): an estimate, stated in the report.
BUDGET_BLOCK = 4096
BUDGET_SLACK_BYTES = 64 * (1 << 20)
MIB = 1 << 20
SETTLE_S = ec.RACY_SETTLE_NS / 1e9
REMOTE = "gateb-src"
EXPECTED_REFUSAL = "SQLITE_STATE_CHANGED"
FINAL_WITH_REFUSALS = "CONTRACT_SELF_INCONSISTENT"
SECRET_KEYS = (
    "pass",
    "password",
    "key_pem",
    "key_file_pass",
    "token",
    "client_secret",
    "secret_access_key",
    "access_key_id",
)
SECRET_FLAGS = re.compile(
    r"^--(?:sftp-)?(?:pass|password|key-pem|key-file-pass|token|client-secret)$"
)
REDACTIONS = (
    (
        re.compile(
            r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?-----END [A-Z0-9 ]*PRIVATE KEY-----",
            re.S,
        ),
        "[REDACTED PRIVATE KEY]",
    ),
    (
        re.compile(
            r"(?im)^(\s*(?:" + "|".join(SECRET_KEYS) + r")\s*=\s*)(?!\[REDACTED)\S.*$",
        ),
        r"\1[REDACTED]",
    ),
    (
        re.compile(
            r"(--(?:sftp-)?(?:pass|password|key-pem|key-file-pass|token|client-secret)"
            r"(?:=|\s+))(?!\[REDACTED)[^\s\"']+"
        ),
        r"\1[REDACTED]",
    ),
    (
        re.compile(
            r"\b(RCLONE_\w*(?:PASS|TOKEN|KEY_PEM|SECRET)\w*=)(?!\[REDACTED)[^\s\"']+"
        ),
        r"\1[REDACTED]",
    ),
    (
        re.compile(r"(\b[a-z][a-z0-9+.-]*://[^/\s:@]+:)(?!\[REDACTED)[^@\s/]+@"),
        r"\1[REDACTED]@",
    ),
)

# The source-side helper. Python 3.8+ and the standard library only (neo's
# /usr/bin/python3 may be the Command Line Tools' 3.9). The harness execs
# the same text locally to hash destinations, so both sides hash one way.
REMOTE_HELPER = r'''
import hashlib, json, os, platform, shutil, stat, subprocess, sys, time

MAGICS = (b"SQLite format 3\x00", b"\x37\x7f\x06\x82", b"\x37\x7f\x06\x83")
XOR = bytes(i ^ 0xA5 for i in range(256))
SUFFIXES = ("-wal", "-shm", "-journal")
WRAPPER = """#!{python} -I
import os, subprocess, sys
child = subprocess.Popen([{agent!r}] + {flags!r} + sys.argv[1:])
_pid, status, usage = os.wait4(child.pid, 0)
code = os.waitstatus_to_exitcode(status)
child.returncode = code
scale = 1 if sys.platform == "darwin" else 1024
sys.stderr.write("gate_b_serve max_rss_bytes=%d exit=%d\\n" % (usage.ru_maxrss * scale, code))
sys.stderr.flush()
sys.exit(code if code >= 0 else 128 - code)
"""


def power():
    if platform.system() == "Darwin":
        try:
            out = subprocess.run(["/usr/bin/pmset", "-g", "batt"], stdout=subprocess.PIPE,
                                 stderr=subprocess.DEVNULL, universal_newlines=True,
                                 check=False).stdout
        except OSError:
            return "unknown"
        first = out.splitlines()[0] if out else ""
        if "'AC Power'" in first:
            return "ac"
        if "'Battery Power'" in first:
            return "battery"
        return "unknown"
    base = "/sys/class/power_supply"
    mains = []
    if os.path.isdir(base):
        for name in sorted(os.listdir(base)):
            try:
                with open(os.path.join(base, name, "type")) as handle:
                    if handle.read().strip() == "Mains":
                        mains.append(os.path.join(base, name, "online"))
            except OSError:
                pass
    if not mains:
        return "unknown-no-supply-class"
    for online in mains:
        try:
            with open(online) as handle:
                if handle.read().strip() == "1":
                    return "ac"
        except OSError:
            pass
    return "battery"


def conditions():
    return {"utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "load1": round(os.getloadavg()[0], 2), "power": power(),
            "system": platform.system(), "node": platform.node(),
            "platform": platform.platform(), "cpus": os.cpu_count()}


def sha256_head(path):
    digest = hashlib.sha256()
    head = b""
    with open(path, "rb") as handle:
        while True:
            block = handle.read(1 << 20)
            if not block:
                break
            if len(head) < 16:
                head = (head + block)[:16]
            digest.update(block)
    return digest.hexdigest(), head


def exclusion(rel, head):
    if rel.endswith(SUFFIXES):
        return "sqlite-companion-name"
    if head.startswith(MAGICS):
        return "sqlite-magic"
    return None


def manifest(root):
    root = os.path.abspath(root)
    rows = []
    excluded = {}
    for dirpath, dirs, files in os.walk(root):
        dirs.sort()
        rel_dir = os.path.relpath(dirpath, root)
        for name in sorted(dirs + files):
            path = os.path.join(dirpath, name)
            rel = name if rel_dir == "." else os.path.join(rel_dir, name)
            info = os.lstat(path)
            mode = stat.S_IMODE(info.st_mode)
            if stat.S_ISLNK(info.st_mode):
                rows.append([rel, "l", mode, 0, os.readlink(path)])
            elif stat.S_ISDIR(info.st_mode):
                rows.append([rel, "d", mode, 0, ""])
            elif stat.S_ISREG(info.st_mode):
                digest, head = sha256_head(path)
                rows.append([rel, "f", mode, info.st_size, digest])
                why = exclusion(rel, head)
                if why:
                    excluded[rel] = why
            else:
                rows.append([rel, "o", mode, 0, ""])
    rows.sort(key=lambda row: os.fsencode(row[0]))
    return rows, excluded


def manifest_digest(rows):
    digest = hashlib.sha256()
    for row in rows:
        digest.update(json.dumps(row, ensure_ascii=True).encode() + b"\n")
    return digest.hexdigest()


def op_conditions(args):
    return conditions()


def op_probe(args):
    out = conditions()
    out["python"] = sys.version.split()[0]
    agent = args.get("agent")
    if agent:
        exists = os.path.isfile(agent)
        out["agent"] = {"path": agent, "exists": exists,
                        "executable": exists and os.access(agent, os.X_OK),
                        "sha256": sha256_head(agent)[0] if exists else None}
    corpus = args.get("corpus")
    if corpus:
        seal_path = os.path.join(corpus, "SEAL.json")
        try:
            with open(seal_path) as handle:
                seal = json.load(handle)
        except (OSError, ValueError) as err:
            out["seal"] = {"error": type(err).__name__}
        else:
            readonly = seal.get("readonly")
            out["seal"] = {key: seal.get(key) for key in ("format", "seed", "scale", "identity")}
            out["seal"]["sealed"] = bool(readonly)
            out["seal"]["mutations"] = len(seal.get("mutations") or [])
    work = args.get("work")
    if work:
        out["work_exists"] = os.path.lexists(work)
        out["work_parent_is_dir"] = os.path.isdir(os.path.dirname(work))
    return out


FIEMAP_IOC = 0xC020660B  # FS_IOC_FIEMAP, _IOWR('f', 11, struct fiemap)
# fstatfs f_type: FIEMAP's SHARED flag is trusted on xfs, btrfs, bcachefs and
# ocfs2; ext2/3/4 and tmpfs cannot reflink (0 by construction); anything else
# (CIFS/SMB3 clones but never flags sharing, NFS, overlayfs) is unsupported.
FIEMAP_TRUSTED = (0x58465342, 0x9123683E, 0xCA451A4E, 0x7461636F)
CANNOT_REFLINK = (0xEF53, 0x01021994)


def _raise(error):
    """os.walk's onerror: an unreadable directory fails the walk."""
    raise error


def fs_magic(fd):
    """fstatfs(fd).f_type (low 32 bits) through libc, or None."""
    import ctypes
    try:
        libc = ctypes.CDLL(None, use_errno=True)
    except OSError:
        return None
    buffer = ctypes.create_string_buffer(256)
    if libc.fstatfs(fd, buffer) != 0:
        return None
    return ctypes.c_long.from_buffer(buffer).value & 0xFFFFFFFF


def shared_extents(path):
    """Shared extents of one regular file now, or None where that cannot be known.

    OI-1003-Q165: by FIEMAP on a file system that reports sharing, 0 on one
    that cannot reflink, None elsewhere and off Linux.
    """
    if not sys.platform.startswith("linux"):
        return None
    import errno, fcntl, struct
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    try:
        magic = fs_magic(fd)
        if magic not in FIEMAP_TRUSTED and magic not in CANNOT_REFLINK:
            return None
        start, shared = 0, 0
        while True:
            request = bytearray(struct.pack("=QQIIII", start, 2 ** 64 - 1, 0, 0, 64, 0)
                                + bytes(56 * 64))
            try:
                fcntl.ioctl(fd, FIEMAP_IOC, request, True)
            except OSError as err:
                if err.errno in (errno.EOPNOTSUPP, errno.ENOTTY):
                    return 0 if magic in CANNOT_REFLINK else None
                raise
            mapped = struct.unpack_from("=I", request, 20)[0]
            last, following = mapped == 0, start
            for index in range(min(mapped, 64)):
                logical, _physical, length = struct.unpack_from("=QQQ", request, 32 + 56 * index)
                flags = struct.unpack_from("=I", request, 72 + 56 * index)[0]
                shared += 1 if flags & 0x2000 else 0
                last = last or bool(flags & 0x1)
                following = logical + length
            if last or mapped < 64 or following <= start:
                return shared
            start = following
    finally:
        os.close(fd)


def tree_shared(root):
    """Shared extents under `root`: an int, "unsupported" (Linux) or "n/a"."""
    total = 0
    for dirpath, _dirs, files in os.walk(root, onerror=_raise):
        for name in files:
            path = os.path.join(dirpath, name)
            if not stat.S_ISREG(os.lstat(path).st_mode):
                continue
            count = shared_extents(path)
            if count is None:
                return "unsupported" if sys.platform.startswith("linux") else "n/a"
            total += count
    return total


def plain_copy(source, target):
    """Copy a tree by os.read/os.write, never a clone (OI-1003-Q165).

    Symlinks are recreated, mtimes kept; any other node is refused.
    """
    os.mkdir(target, 0o700)
    for dirpath, dirs, files in os.walk(source, onerror=_raise):
        here = os.path.join(target, os.path.relpath(dirpath, source))
        for name in sorted(dirs) + sorted(files):
            path, dest = os.path.join(dirpath, name), os.path.join(here, name)
            info = os.lstat(path)
            if stat.S_ISLNK(info.st_mode):
                os.symlink(os.readlink(path), dest)
            elif stat.S_ISDIR(info.st_mode):
                os.mkdir(dest, 0o700)
            elif stat.S_ISREG(info.st_mode):
                inp = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
                try:
                    out = os.open(dest, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW
                                  | os.O_CLOEXEC, 0o600)
                    try:
                        while True:
                            block = os.read(inp, 1 << 20)
                            if not block:
                                break
                            view = memoryview(block)
                            while view:
                                view = view[os.write(out, view):]
                    finally:
                        os.close(out)
                finally:
                    os.close(inp)
                os.chmod(dest, stat.S_IMODE(info.st_mode) | 0o600)
                os.utime(dest, ns=(info.st_atime_ns, info.st_mtime_ns))
            else:
                raise RuntimeError("not a file, directory or symlink: " + path)
        dirs[:] = [name for name in dirs
                   if not stat.S_ISLNK(os.lstat(os.path.join(dirpath, name)).st_mode)]
    for dirpath, _dirs, _files in os.walk(source, topdown=False, onerror=_raise):
        info = os.lstat(dirpath)
        os.utime(os.path.join(target, os.path.relpath(dirpath, source)),
                 ns=(info.st_atime_ns, info.st_mtime_ns))


def op_prepare(args):
    sealed = os.path.join(args["corpus"], "corpus")
    work = args["work"]
    if os.path.lexists(work) or not os.path.isdir(os.path.dirname(work)):
        raise RuntimeError("source work root must be new under an existing parent")
    if not os.path.isdir(sealed):
        raise RuntimeError("sealed corpus/ is missing")
    os.mkdir(work, 0o700)
    corpus = os.path.join(work, "corpus")
    plain_copy(sealed, corpus)
    shared = tree_shared(corpus)
    if isinstance(shared, int) and shared:
        raise RuntimeError("the working copy shares %d extent(s) with another file: a clone,"
                           " refused (S2, OI-1003-Q165)" % shared)
    for dirpath, dirs, files in os.walk(corpus):
        os.chmod(dirpath, 0o755)
        for name in files:
            path = os.path.join(dirpath, name)
            info = os.lstat(path)
            if stat.S_ISREG(info.st_mode):
                os.chmod(path, 0o755 if info.st_mode & 0o111 else 0o644)
    for sub in ("states", "bin"):
        os.mkdir(os.path.join(work, sub), 0o700)
    wrapper = os.path.join(work, "bin", "gate-b-serve")
    with open(wrapper, "w") as handle:
        handle.write(WRAPPER.format(python=args["python"], agent=args["agent"],
                                    flags=list(args.get("flags") or [])))
    os.chmod(wrapper, 0o700)
    return {"corpus": corpus, "wrapper": wrapper, "states": os.path.join(work, "states"),
            "shared_extents": shared}


def op_measure(args):
    # Read-only: sizes by lstat and a 16-byte head per regular file, no hashing.
    root = os.path.join(args["corpus"], "corpus")
    if not os.path.isdir(root):
        raise RuntimeError("sealed corpus/ is missing")
    out = {"comparable_files": 0, "comparable_bytes": 0, "directories": 0,
           "others": 0, "excluded": 0, "excluded_bytes": 0}
    for dirpath, dirs, files in os.walk(root):
        for name in dirs + files:
            path = os.path.join(dirpath, name)
            info = os.lstat(path)
            if stat.S_ISDIR(info.st_mode):
                out["directories"] += 1
            elif stat.S_ISREG(info.st_mode):
                with open(path, "rb") as handle:
                    head = handle.read(16)
                if exclusion(name, head):
                    out["excluded"] += 1
                    out["excluded_bytes"] += info.st_size
                else:
                    out["comparable_files"] += 1
                    out["comparable_bytes"] += info.st_size
            else:
                out["others"] += 1
    return out


def op_manifest(args):
    if not os.path.isdir(args["root"]):
        raise RuntimeError("manifest root is not a directory")
    rows, excluded = manifest(args["root"])
    return {"rows": rows, "exclusions": excluded, "digest": manifest_digest(rows)}


def op_xor(args):
    root = os.path.abspath(args["root"])
    total = 0
    for rel, length in args["plan"]:
        path = os.path.join(root, rel)
        if os.path.islink(path) or not os.path.realpath(path).startswith(root + os.sep):
            raise RuntimeError("delta target escapes the working copy")
        with open(path, "r+b") as handle:
            left = length
            while left > 0:
                block = handle.read(min(left, 1 << 20))
                if not block:
                    raise RuntimeError("delta target shorter than its plan")
                handle.seek(-len(block), 1)
                handle.write(block.translate(XOR))
                left -= len(block)
            handle.flush()
            os.fsync(handle.fileno())
        total += length
    fd = os.open(root, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)
    return {"files": len(args["plan"]), "bytes": total}


OPS = {"conditions": op_conditions, "probe": op_probe, "prepare": op_prepare,
       "measure": op_measure, "manifest": op_manifest, "xor": op_xor}


def main(op, args):
    try:
        result = OPS[op](args)
    except Exception as err:  # reported as a value; the harness decides
        print("gate-b-helper " + json.dumps({"error": "%s: %s" % (type(err).__name__, err)}))
        sys.exit(1)
    print("gate-b-helper " + json.dumps(result, ensure_ascii=True))
'''


LOOPBACK_SHIM = """#!/bin/sh
# loopback ssh shim (--dry-run only): runs the "remote" command here.
while [ $# -gt 0 ]; do
  case "$1" in
    -F|-o|-p|-i|-l|-E|-c|-J) shift 2 ;;
    --) shift; break ;;
    -*) shift ;;
    *) break ;;
  esac
done
shift  # the host
if [ "$1" = "-s" ]; then exec {sftp_server}; fi
exec /bin/sh -c "$*"
"""


@dataclasses.dataclass(frozen=True)
class Refusal:
    """A refusal before the sample, as a value (exit 2)."""

    code: str
    reason: str


class Abort(Exception):
    """The sample ended early; the evidence is written as aborted."""

    def __init__(
        self,
        reason: str,
        rep: dict[str, object] | None = None,
        sample: dict[str, object] | None = None,
    ) -> None:
        super().__init__(reason)
        self.rep = rep
        self.sample = sample


def redact(text: str) -> str:
    """Scrub anything shaped like a credential from text bound for logs/JSON."""
    for pattern, replacement in REDACTIONS:
        text = pattern.sub(replacement, text)
    return text


def redact_argv(argv: list[str]) -> list[str]:
    out: list[str] = []
    hide = False
    for arg in argv:
        if hide:
            out.append("[REDACTED]")
            hide = False
            continue
        if SECRET_FLAGS.match(arg):
            hide = True
        out.append(redact(arg))
    return out


def scrub(value: object) -> object:
    if isinstance(value, str):
        return redact(value)
    if isinstance(value, list | tuple):
        return [scrub(v) for v in value]
    if isinstance(value, dict):
        return {k: scrub(v) for k, v in value.items()}
    return value


# ---------------------------------------------------------------- transport


@dataclasses.dataclass
class Transport:
    """The one way the harness reaches the source host."""

    ssh: str
    host: str
    config: str | None
    python: str

    def argv(self, remote: list[str], extra: tuple[str, ...] = ()) -> list[str]:
        base = [self.ssh]
        if self.config:
            base += ["-F", self.config]
        base += ["-T", "-oBatchMode=yes", "-oConnectTimeout=15", *extra]
        return [*base, "--", self.host, shlex.join(remote)]

    def helper(self, op: str, args: dict[str, object]) -> dict[str, object]:
        program = (
            REMOTE_HELPER
            + f"\nmain({op!r}, json.loads({json.dumps(json.dumps(args))}))\n"
        )
        result = subprocess.run(
            self.argv([self.python, "-I", "-"]),
            input=program,
            capture_output=True,
            text=True,
            check=False,
        )
        return helper_result(op, result.returncode, result.stdout, result.stderr)

    def run(self, remote: list[str]) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            self.argv(remote), capture_output=True, text=True, check=False
        )

    def calibrate(self, mib: int, streams: int) -> dict[str, object]:
        argv = self.argv(
            ["dd", "if=/dev/zero", f"bs={MIB}", f"count={mib}"],
            extra=("-oCompression=no",),
        )
        counts = [0] * streams
        codes = [0] * streams

        def pump(index: int) -> None:
            with subprocess.Popen(
                argv,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
            ) as proc:
                stream = proc.stdout
                for block in iter(lambda: stream.read(MIB), b""):
                    counts[index] += len(block)
            codes[index] = proc.returncode

        started = time.monotonic()
        threads = [threading.Thread(target=pump, args=(i,)) for i in range(streams)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()
        elapsed = time.monotonic() - started
        total = sum(counts)
        return {
            "streams": streams,
            "bytes": total,
            "seconds": round(elapsed, 3),
            "bytes_per_s": round(total / elapsed) if elapsed > 0 else None,
            "complete": total == mib * MIB * streams and not any(codes),
        }


def helper_result(op: str, code: int, stdout: str, stderr: str) -> dict[str, object]:
    lines = [line for line in stdout.splitlines() if line.startswith("gate-b-helper ")]
    if not lines:
        tail = redact(stderr.strip())[-400:]
        raise Abort(f"source helper {op} gave no result (exit {code}): {tail}")
    result = json.loads(lines[-1].partition(" ")[2])
    if code != 0 or "error" in result:
        raise Abort(f"source helper {op} failed: {redact(str(result.get('error')))}")
    return result


def ssh_g(transport: Transport) -> dict[str, list[str]]:
    argv = [transport.ssh]
    if transport.config:
        argv += ["-F", transport.config]
    out = subprocess.run(
        [*argv, "-G", transport.host], capture_output=True, text=True, check=True
    ).stdout
    return parse_ssh_g(out)


def parse_ssh_g(text: str) -> dict[str, list[str]]:
    resolved: dict[str, list[str]] = {}
    for line in text.splitlines():
        key, _, value = line.strip().partition(" ")
        if key:
            resolved.setdefault(key.lower(), []).append(value.strip())
    return resolved


def rclone_remote(
    transport: Transport,
    mode: str,
    resolved: dict[str, list[str]] | None,
    env: dict[str, str],
) -> tuple[dict[str, str] | None, Refusal | None]:
    """The sftp remote, from the ssh config; never a password or key body."""
    if mode == "external":
        words = [transport.ssh]
        if transport.config:
            words += ["-F", transport.config]
        words += ["-oBatchMode=yes", transport.host]
        remote = {
            "type": "sftp",
            "ssh": " ".join(f'"{w}"' if " " in w else w for w in words),
            "shell_type": "none",
            "disable_hashcheck": "true",
            "skip_links": "true",
        }
    else:
        resolved = resolved or {}

        def first(key: str) -> str | None:
            return (resolved.get(key) or [None])[0]

        known = [
            os.path.expanduser(p)
            for value in resolved.get("userknownhostsfile", [])
            for p in value.split()
        ]
        known = [p for p in known if os.path.isfile(p)]
        if not known:
            return None, Refusal(
                "HOST_KEY_UNVERIFIED",
                "internal rclone transport needs a known_hosts file from `ssh -G`",
            )
        remote = {
            "type": "sftp",
            "host": first("hostname") or transport.host,
            "user": first("user") or "",
            "port": first("port") or "22",
            "known_hosts_file": known[0],
            "skip_links": "true",
        }
        if env.get("SSH_AUTH_SOCK"):
            remote["key_use_agent"] = "true"
        else:
            keys = [
                os.path.expanduser(p)
                for p in resolved.get("identityfile", [])
                if os.path.isfile(os.path.expanduser(p))
            ]
            if not keys:
                return None, Refusal(
                    "NO_SSH_IDENTITY",
                    "internal rclone transport: no ssh-agent and no identity file",
                )
            remote["key_file"] = keys[0]
    leaked = sorted(set(remote) & set(SECRET_KEYS))
    if leaked:
        return None, Refusal("SECRET_IN_CONFIG", f"rclone remote would hold {leaked}")
    return remote, None


def rclone_config_text(remote: dict[str, str]) -> str:
    return f"[{REMOTE}]\n" + "".join(f"{k} = {v}\n" for k, v in remote.items())


def write_private(path: Path, text: str) -> None:
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "w") as handle:
        handle.write(text)


def rclone_env() -> dict[str, str]:
    return {k: v for k, v in os.environ.items() if not k.startswith("RCLONE_")}


def rclone_glob(path: str) -> str:
    """An anchored rclone filter that matches exactly this path."""
    escaped = re.sub(r"([\\*?\[\]{}])", r"\\\1", path)
    return "/" + escaped


def native_argv(
    agent: str,
    host: str,
    source: str,
    dest: Path,
    source_state: str,
    dest_state: Path,
    wrapper: str,
    config: str | None,
) -> list[str]:
    argv = [agent, "pull", host, source, str(dest), source_state, str(dest_state)]
    argv.append(wrapper)
    if config:
        argv.append(config)
    return argv


# ------------------------------------------------------------- measurement


def run_measured(
    argv: list[str], env: dict[str, str], stdout: Path, stderr: Path
) -> dict[str, object]:
    """Run one arm; wall time and the child's own max RSS (wait4)."""
    with stdout.open("wb") as out, stderr.open("wb") as err:
        started = time.monotonic()
        proc = subprocess.Popen(
            argv, stdin=subprocess.DEVNULL, stdout=out, stderr=err, env=env
        )
        _pid, status, usage = os.wait4(proc.pid, 0)
        elapsed = time.monotonic() - started
    code = os.waitstatus_to_exitcode(status)
    proc.returncode = code
    scale = 1 if sys.platform == "darwin" else 1024
    text_out = stdout.read_text(errors="replace")
    text_err = stderr.read_text(errors="replace")
    stdout.write_text(redact(text_out))
    stderr.write_text(redact(text_err))
    return {
        "exit": code,
        "elapsed_ms": round(elapsed * 1000, 3),
        "max_rss_bytes": usage.ru_maxrss * scale,
        "stdout": text_out,
        "stderr": text_err,
    }


def unescape_ascii(text: str) -> str:
    """Invert Rust's `escape_ascii` back to a path string (surrogateescape)."""
    out = bytearray()
    i = 0
    simple = {"n": 10, "r": 13, "t": 9, "\\": 92, "'": 39, '"': 34, "0": 0}
    while i < len(text):
        ch = text[i]
        if ch == "\\" and i + 1 < len(text):
            nxt = text[i + 1]
            if nxt == "x" and i + 3 < len(text):
                out.append(int(text[i + 2 : i + 4], 16))
                i += 4
                continue
            if nxt in simple:
                out.append(simple[nxt])
                i += 2
                continue
        out += ch.encode("utf-8", "surrogateescape")
        i += 1
    return os.fsdecode(bytes(out))


def parse_native(stdout: str, stderr: str) -> dict[str, object]:
    parsed: dict[str, object] = {
        "transfer": {},
        "timing": {},
        "chunk": {},
        "counters": {},
        "serve": {},
        "serve_timing": {},
        "refusals": [],
        "serve_max_rss_bytes": None,
        "final_refusal": None,
    }
    for line in stdout.splitlines():
        if line.startswith("completed="):
            parsed["transfer"] = ab.pairs(line)
        elif line.startswith("transfer_timing "):
            parsed["timing"] = ab.pairs(line)
        elif line.startswith("chunk_timing "):
            parsed["chunk"] = ab.pairs(line)
        elif line.startswith("counters "):
            parsed["counters"] = ab.pairs(line)
    for line in stderr.splitlines():
        if line.startswith("refused "):
            path, _, code = line[len("refused ") :].rpartition(": ")
            parsed["refusals"].append([unescape_ascii(path), code.strip()])
        elif line.startswith("counters ") and "verb=serve" in line:
            parsed["serve"] = ab.pairs(line)
        elif line.startswith("transfer_timing ") and "verb=serve" in line:
            parsed["serve_timing"] = ab.pairs(line)
        elif line.startswith("gate_b_serve "):
            parsed["serve_max_rss_bytes"] = ab.pairs(line).get("max_rss_bytes")
        elif line.startswith("bulkload-agent: refused: "):
            parsed["final_refusal"] = line.rpartition(": ")[2].split(" ")[0]
    return parsed


def native_problems(
    run: dict[str, object], parsed: dict[str, object], exclusions: dict[str, str]
) -> list[str]:
    """The native arm may refuse exactly the excluded SQLite seats, nothing else."""
    problems = []
    if not parsed["transfer"]:
        problems.append(f"no transfer line (exit {run['exit']})")
    refused = {path: code for path, code in parsed["refusals"]}
    unexpected = sorted(
        f"{p}: {c}"
        for p, c in refused.items()
        if p not in exclusions or c != EXPECTED_REFUSAL
    )
    if unexpected:
        problems.append(f"unexpected refusals: {unexpected[:5]}")
    unrefused = sorted(set(exclusions) - set(refused))
    if unrefused:
        problems.append(
            f"excluded seats the agent carried (the sets disagree): {unrefused[:5]}"
        )
    if refused:
        if run["exit"] != 1 or parsed["final_refusal"] != FINAL_WITH_REFUSALS:
            problems.append(
                f"exit {run['exit']} / {parsed['final_refusal']} with refusals"
            )
    elif run["exit"] != 0:
        problems.append(f"exit {run['exit']}")
    return problems


def verify_tree(
    expected: list[list[object]],
    exclusions: dict[str, str],
    got: list[list[object]],
) -> dict[str, object]:
    """Regular files (content) and directories are mandatory; the rest is fidelity."""
    want = {row[0]: row for row in expected if row[0] not in exclusions}
    have = {row[0]: row for row in got}
    missing_files, wrong_content, missing_dirs = [], [], []
    symlink_mismatch, mode_mismatch = [], []
    for path, row in want.items():
        other = have.get(path)
        kind = row[1]
        if kind == "f":
            if other is None or other[1] != "f":
                missing_files.append(path)
            elif other[3] != row[3] or other[4] != row[4]:
                wrong_content.append(path)
            elif other[2] != row[2]:
                mode_mismatch.append(path)
        elif kind == "d":
            if other is None or other[1] != "d":
                missing_dirs.append(path)
        elif kind == "l" and (other is None or other[1:] != row[1:]):
            symlink_mismatch.append(path)
    extra = sorted(set(have) - set(want))
    return {
        "ok": not (missing_files or wrong_content or missing_dirs),
        "files_expected": sum(1 for r in want.values() if r[1] == "f"),
        "missing_files": len(missing_files),
        "wrong_content": len(wrong_content),
        "missing_dirs": len(missing_dirs),
        "symlink_mismatch": len(symlink_mismatch),
        "mode_mismatch": len(mode_mismatch),
        "extra_entries": len(extra),
        "examples": {
            "missing_files": missing_files[:5],
            "wrong_content": wrong_content[:5],
            "missing_dirs": missing_dirs[:5],
            "symlink_mismatch": symlink_mismatch[:5],
            "extra_entries": extra[:5],
        },
    }


def comparable_bytes(rows: list[list[object]], exclusions: dict[str, str]) -> int:
    return sum(int(r[3]) for r in rows if r[1] == "f" and r[0] not in exclusions)


def changed_paths(before: list[list[object]], after: list[list[object]]) -> set[str]:
    old = {r[0]: r for r in before}
    new = {r[0]: r for r in after}
    return {p for p in set(old) | set(new) if old.get(p) != new.get(p)}


def median(values: list[float]) -> float | None:
    return statistics.median(values) if values else None


def link_fraction(
    workload_bytes: int, elapsed_ms: float, link: dict[str, object] | None
) -> float | None:
    if not link or not elapsed_ms:
        return None
    ceiling = max(
        (float(c.get("bytes_per_s") or 0) for c in link.values() if c), default=0.0
    )
    if ceiling <= 0:
        return None
    return round(workload_bytes / (elapsed_ms / 1000) / ceiling, 4)


# ------------------------------------------------------------ disk budget


def disk_budget(
    total: int,
    available: int,
    copies: int,
    comparable: int,
    entries: int,
    floor_percent: int = AGENT_MIN_FREE_PERCENT,
) -> dict[str, object]:
    """The agent's space.rs check, for every destination held at once.

    `after / total >= floor / 100` in integers, as space::check; exactly at the
    floor passes. `need` is an estimate: content bytes plus one block per entry
    per copy, plus a fixed slack for states and logs.
    """
    per_copy = comparable + BUDGET_BLOCK * entries
    need = copies * per_copy + BUDGET_SLACK_BYTES
    after = available - need
    ok = total > 0 and after >= 0 and after * 100 >= total * floor_percent
    return {
        "total_bytes": total,
        "available_bytes": available,
        "free_ratio": round(available / total, 4) if total > 0 else None,
        "floor_percent": floor_percent,
        "copies": copies,
        "per_copy_bytes": per_copy,
        "need_bytes": need,
        "free_ratio_after": round(after / total, 4) if total > 0 else None,
        "ok": ok,
    }


def volume_space(path: Path) -> tuple[int, int]:
    """(total, available to this user) of the filesystem holding `path`."""
    stats = os.statvfs(path)
    return stats.f_blocks * stats.f_frsize, stats.f_bavail * stats.f_frsize


# --------------------------------------------------------------- gating


def host_problems(
    source: dict[str, object],
    dest: dict[str, object],
    mode: str,
    dest_limit: float,
) -> list[str]:
    """Gated: R-N81 on the source, a load bound on the destination.

    Under load only the power checks stay; a dry run is not gated.
    """
    if mode == "dry-run":
        return []
    problems = []
    if source.get("power") != "ac":
        problems.append(f"source power={source.get('power')}")
    if dest.get("power") == "battery":
        problems.append("destination on battery power")
    if mode == "gated":
        if float(source.get("load1", 99)) >= LOAD_LIMIT:
            problems.append(f"source load1={source.get('load1')} >= {LOAD_LIMIT}")
        if float(dest.get("load1", 99)) >= dest_limit:
            problems.append(f"destination load1={dest.get('load1')} >= {dest_limit}")
    return problems


def sample_gated(source: dict[str, object], dest: dict[str, object]) -> bool:
    """R-N81 on both hosts, against the fixed LOAD_LIMIT."""
    return not host_problems(source, dest, "gated", LOAD_LIMIT)


# ------------------------------------------------------------------- delta


def remove_targets(destination: Path, plan: list[list[object]]) -> None:
    """Outside the timing: the delta's targets leave this arm's own destination."""
    root = os.path.realpath(destination)
    for rel, _length in plan:
        path = destination / str(rel)
        if not os.path.realpath(path).startswith(root + os.sep):
            raise Abort(f"delta target escapes the destination: {rel}")
        if path.is_file() and not path.is_symlink():
            path.unlink()
    fd = os.open(destination, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)
