#!/usr/bin/env python3
"""S1 gate (b) harness: a neo->sting pull, native bulkload vs rclone over sftp.

Rulings: OI-1002-Q30 (B passes only if every B rep passes), OI-1003-Q3
(gate (b) is a neo->sting pull that beats rclone over sftp; there is no
wall-clock SLA), OI-1003-Q19 (the estate-shaped corpus, #159), OI-1003-Q66
(the harness may be built; no neo run until gate (a) passes), R-N81, R-N91,
R-N13. The protocol note is docs/plans/2026-10-06-s1-gate-b-protocol.md.

The harness runs on the destination (sting). The source (neo) is reached
only over ssh, through one fixed transport: `<ssh> [-F CONFIG] -T
-oBatchMode=yes -oConnectTimeout=15 -- HOST <command>`. Everything it runs
on the source is one of:
  - the helper: the REMOTE_HELPER program below, sent on stdin to
    `<source-python> -I -` (its arguments are embedded in the program text, so
    no argv carries them). It reads conditions, hashes trees, creates the
    harness's own working copy and applies or reverts the 1 % delta in it;
  - `dd if=/dev/zero` for the link calibration (bytes discarded);
  - the repository's `estate_corpus.py verify` on the sealed corpus;
  - `bulkload-agent serve`, which `bulkload-agent pull` starts itself, and the
    sftp subsystem, which rclone starts itself.
It never writes the sealed corpus, never deletes anything on the source, and
never signals a process on either host.

Workload: the estate corpus (estate_corpus.py, #159), sealed, at scale estate
in gated mode. The helper copies it once into a new private working copy
(--source-work), which both arms read. The comparable set is that copy minus
the seats the agent's pull refuses by design: SQLite databases (by magic) and
`-wal`/`-shm`/`-journal` companions (by name), SQLITE_STATE_CHANGED. rclone
gets the same seats as an anchored `--exclude-from` list; the native arm must
refuse exactly that set and nothing else, or the sample aborts. SQLite is
carried by `snapshot`, never by a byte mover; it is not part of gate (b).

One rep (3 B reps in gated mode; one revision, so no A arm):
  1. Both hosts gated (R-N81): the source on AC power with load1 < 2.5, the
     destination with load1 < 2.5 and not on battery. --dest-load-limit may
     only tighten the destination bound: gated mode refuses a value over 2.5
     (LOAD_LIMIT), and every sample's `gated` flag is computed against the
     fixed 2.5 on both hosts.
  2. Link calibration: `dd` of --calibrate-mib MiB from /dev/zero over one
     stream and over --calibrate-streams streams (#47: each rep records link
     throughput just before it). Every arm reports its share of the ceiling.
  3. Initial copy: arms alternate N/R/N/R/N, each into a new destination (and,
     for native, new source and destination states). Before every arm both
     hosts are checked again. After every arm the destination is hashed and
     must hold every comparable regular file and directory.
  4. Warm resume: the first native arm is pulled again unchanged; R25 wants
     0 bytes received and 0 content bytes read, and the rep fails otherwise
     (gate (a)'s r25_warm_zero). Content bytes are the raw source_bytes_read:
     since #186 the agent counts the 16-byte SQLite header sniff of a refused
     seat apart (source_sniff_bytes, on the serve counters line).
  5. 1 % delta: the helper XORs 0xa5 over 1 % of the comparable regular-file
     bytes (whole files in a seeded path-hash order, the last one a prefix),
     the harness waits out the racy window (2 s, R-N76), removes those files
     from every arm's destination outside the timing (no-clobber; as the
     bench does), and the arms run again N/R/N/R/N into the same
     destinations. The helper then XORs again, which restores the bytes, and
     the copy must hash to the initial manifest.
  6. After the rep: AC power on the source, and load1 on both hosts must fall
     under the limits within --post-settle-seconds. The rep's destinations
     are then released (removed from the work root) unless
     --keep-destinations is given.
A rep passes iff the native median beats the rclone median for the initial
copy and for the delta, the warm resume received 0 bytes and read 0 content
bytes, every native arm's RSS (the pull, and the serve as the source-side
wrapper reports it) stays under 2 GiB, every arm verified and every sample
was gated. Gate (b) passes iff all 3 B reps pass. Gate (a) also gates an
interrupted resume; this harness has no such phase (it would have to stop a
pull part-way, and it never signals a process), and says so in every verdict
(DEVIATIONS, r25_interrupted_resume = "not-run").

Destination disk budget: before anything is copied on the source, the helper
measures the sealed corpus (lstat and a 16-byte read per file) and the
harness refuses with DEST_SPACE (exit 2) unless the work root's filesystem
keeps the agent's default free floor (25 %, space.rs) after every destination
it will hold at once: 5 copies of the comparable set when reps are released,
reps x 5 with --keep-destinations. The native arm enforces that floor on
every pull (DESTINATION_SPACE_INSUFFICIENT); rclone does not. The report
records the free ratio and the budget.

Native arm: `bulkload-agent pull HOST SOURCE DEST SOURCE_STATE DEST_STATE
REMOTE_EXECUTABLE [SSH_CONFIG]`, one `ssh -T` stream (the pre-W5 engine).
REMOTE_EXECUTABLE is a small wrapper the helper writes into the working
copy's bin/: it runs the source agent (with --priority=normal only when
--source-priority normal asks for it; the default is the WP0(f) background
class, OI-1003-Q25), waits for it and prints its max RSS. If the local agent
has no `pull`/`serve` pair, or --native-streams asks for W5's N-stream pull,
the native arm is refused with NATIVE_REMOTE_ARM_MISSING, "native remote arm
missing (#47)", exit 2.

rclone arm: `rclone copy` with the gate (a) flags (--create-empty-src-dirs
--links --metadata --transfers 4 --checkers 4) from an sftp remote in a
private rclone.conf (mode 0600) built from the ssh config:
  - external (default): the remote's `ssh` is the same OpenSSH command line as
    the native arm's transport, so both arms use one client, one config and
    one cipher; `shell_type = none` and `disable_hashcheck = true` keep rclone
    from opening a new ssh connection per file hash;
  - internal: rclone's own ssh, with host, user, port, known_hosts_file and
    key_file taken from `ssh -G`, and ssh-agent when SSH_AUTH_SOCK is set. A
    host with no known_hosts file is refused (HOST_KEY_UNVERIFIED).
No password, passphrase, PEM or token is ever written to the config, argv,
logs or JSON: the config is refused if it would hold one, every inherited
RCLONE_* variable is dropped, and every log and the JSON pass through
`redact`.

--under-load is informational, never a gate verdict (by analogy with
OI-1003-Q39 and OI-1003-Q50 for gate (a)): the load gates on both hosts are
lifted and load is recorded, but source AC power is still required before
every rep and arm. Evidence goes to docs/evidence/s1-gate-b-underload-*.md.

--dry-run is a local smoke on one host: a loopback ssh shim runs the "remote"
commands locally (the sftp subsystem is the local sftp-server), a synthetic
corpus with one SQLite file stands in for the estate corpus, and nothing is
gated. Its output says NOT A GATE SAMPLE and never goes under docs/evidence.

Evidence: WORK/gate-b.json holds everything (format bulkload-s1-gate-b-v1);
the Markdown draft goes to docs/evidence/s1-gate-b-<date>-<HHMM>Z.md.
Exit: 0 complete (the verdict is in the evidence), 2 refused before the
sample, 3 aborted during the sample.
"""

from __future__ import annotations

import argparse
import dataclasses
import datetime as dt
import hashlib
import json
import os
import platform
import re
import shlex
import shutil
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

FORMAT = "bulkload-s1-gate-b-v1"
RULINGS = "OI-1002-Q30, OI-1003-Q3, OI-1003-Q19, OI-1003-Q66, R-N81, R-N91, R-N13"
# An under-load sample is not R-N81 load-gated and not R-N91 quiet-gated.
RULINGS_UNDER_LOAD = (
    "OI-1003-Q39 and OI-1003-Q50 (by analogy with gate (a)), OI-1003-Q3,"
    " OI-1003-Q19, OI-1003-Q66, R-N13"
)
LOAD_LIMIT = ab.LOAD_LIMIT
GATE_B_REPS = 3
NATIVE_SAMPLES = 3  # N/R/N/R/N, as the bench's --reps 3
RSS_CAP_BYTES = 2 << 30
# The agent's default destination floor (space.rs DEFAULT_MIN_FREE_PERCENT):
# pull refuses DESTINATION_SPACE_INSUFFICIENT when a write would leave less.
# The harness runs pull with that default, so its budget uses the same floor.
AGENT_MIN_FREE_PERCENT = 25
# Budget estimate per destination entry (block rounding) and for the whole
# sample (states, logs, rclone.conf): an estimate, stated in the report.
BUDGET_BLOCK = 4096
BUDGET_SLACK_BYTES = 64 * (1 << 20)
INTERRUPTED_NOT_RUN = (
    "not-run: gate (b) has no interrupted-resume phase; stopping a pull"
    " part-way would need the harness to signal a process, which it never does"
)
# Where the gate (b) rep rule differs from gate (a)'s enforce_verdict
# (crates/bulkload-bench/src/main.rs). Unratified; carried in every verdict.
DEVIATIONS = (
    "no interrupted-resume phase: gate (a) also requires r25_interrupted_zero;"
    " gate (b) records r25_interrupted_resume=not-run and does not gate it",
)
MIB = 1 << 20
SETTLE_S = ec.RACY_SETTLE_NS / 1e9
DELTA_SEED = "bulkload-s1-gate-b-delta-v1"
REMOTE = "gateb-src"
NATIVE_ARM_MISSING = "NATIVE_REMOTE_ARM_MISSING"
EXPECTED_REFUSAL = "SQLITE_STATE_CHANGED"
FINAL_WITH_REFUSALS = "CONTRACT_SELF_INCONSISTENT"
PULL_USAGE = (
    "pull HOST SOURCE DEST SOURCE_STATE DEST_STATE [REMOTE_EXECUTABLE [SSH_CONFIG]]"
)
REMOTE_PATH_OK = re.compile(r"^/[A-Za-z0-9/_.-]+$")
NOT_GATE = "DRY RUN - NOT A GATE SAMPLE"
UNDER_LOAD = "INFORMATIONAL UNDER LOAD - NOT A GATE SAMPLE"
RCLONE_FLAGS = (
    "--create-empty-src-dirs",
    "--links",
    "--metadata",
    "--transfers",
    "4",
    "--checkers",
    "4",
    "--stats",
    "0",
    "--log-level",
    "ERROR",
)
# What the agent's pull lacks against W5 (#47) as of this harness. The
# native arm runs without them (the pre-W5 engine); --native-streams > 1 is
# refused because the first one does not exist.
W5_MISSING = (
    "N parallel streams: pull opens one `ssh -T` session to one `serve`; there is"
    " no stream-count flag and no SCM_RIGHTS rendezvous of N sessions",
    "adaptive zstd-1 on the wire: wire v5 has no compressed data frame",
    "`Ref` dedup frames: a chunk already sent in the session is sent again"
    " unless the destination fills it through WantManifest/NeedChunks",
    "`NeedRanges`: NeedChunks names whole chunks by index, not byte ranges",
    "a source-side RSS line: serve prints counters and timing but no max RSS, so"
    " the harness wraps serve to measure it",
    "a `--priority` hand-off from pull to serve: the source half always takes the"
    " WP0(f) default unless REMOTE_EXECUTABLE adds the flag",
    "an exclude or SQLite hand-off: pull refuses SQLite seats as"
    " SQLITE_STATE_CHANGED, so the estate corpus's SQLite half is out of gate (b)",
)
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


def op_prepare(args):
    sealed = os.path.join(args["corpus"], "corpus")
    work = args["work"]
    if os.path.lexists(work) or not os.path.isdir(os.path.dirname(work)):
        raise RuntimeError("source work root must be new under an existing parent")
    if not os.path.isdir(sealed):
        raise RuntimeError("sealed corpus/ is missing")
    os.mkdir(work, 0o700)
    corpus = os.path.join(work, "corpus")
    shutil.copytree(sealed, corpus, symlinks=True)
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
    return {"corpus": corpus, "wrapper": wrapper, "states": os.path.join(work, "states")}


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

HELPER: dict[str, object] = {"__name__": "gate_b_helper"}
exec(compile(REMOTE_HELPER, "gate_b_helper", "exec"), HELPER)  # noqa: S102

LOOPBACK_SHIM = """#!/bin/sh
# gate-b loopback ssh shim (--dry-run only): runs the "remote" command here.
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


def say(message: str) -> None:
    print(f"gate-b {redact(message)}", flush=True)


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


def native_capability(help_text: str, streams: int) -> Refusal | None:
    """The native remote arm: the agent's `pull`/`serve` pair, one stream."""
    has_pull = PULL_USAGE in help_text
    has_serve = re.search(r"^\s+serve\s", help_text, re.M) is not None
    if not (has_pull and has_serve):
        return Refusal(
            NATIVE_ARM_MISSING,
            "native remote arm missing (#47): the agent has no"
            f" `{PULL_USAGE}` / `serve` verb pair",
        )
    if streams != 1:
        return Refusal(
            NATIVE_ARM_MISSING,
            f"native remote arm missing (#47): --native-streams {streams} needs"
            " W5's N-stream pull; the agent's pull opens one `ssh -T` stream",
        )
    return None


def probe_agent(agent: Path, streams: int) -> tuple[str, Refusal | None]:
    if not (agent.is_file() and os.access(agent, os.X_OK)):
        return "", Refusal(
            NATIVE_ARM_MISSING,
            f"native remote arm missing (#47): no executable agent at {agent}",
        )
    result = subprocess.run(
        [str(agent), "help"], capture_output=True, text=True, check=False
    )
    text = result.stdout + result.stderr
    return text, native_capability(text, streams)


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


def rclone_argv(
    rclone: str, conf: Path, source: str, dest: Path, excludes: Path
) -> list[str]:
    return [
        rclone,
        "copy",
        f"{REMOTE}:{source}",
        str(dest),
        "--config",
        str(conf),
        "--exclude-from",
        str(excludes),
        *RCLONE_FLAGS,
    ]


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


def delta_plan(
    rows: list[list[object]], exclusions: dict[str, str], seed: str = DELTA_SEED
) -> list[list[object]]:
    """1 % of comparable regular-file bytes: whole files in seeded path-hash order."""
    files = [r for r in rows if r[1] == "f" and r[0] not in exclusions and r[3] > 0]
    target = -(-comparable_bytes(rows, exclusions) // 100)
    files.sort(
        key=lambda r: hashlib.sha256(seed.encode() + b"\0" + os.fsencode(r[0])).digest()
    )
    plan, left = [], target
    for row in files:
        if left <= 0:
            break
        take = min(left, int(row[3]))
        plan.append([row[0], take])
        left -= take
    return plan


def content_bytes_read(source_bytes_read: object) -> int | None:
    """The R25 content reads: the raw source_bytes_read, as gate (a) takes it.

    Until #186 the agent counted the 16-byte header sniff of every SQLite seat
    it refuses by magic in source_bytes_read, on every pass, and this netted
    it out. The agent now counts the sniff apart (source_sniff_bytes) and
    sniffs an unchanged refused seat once, so nothing is netted out: doing so
    would report negative content bytes.
    """
    if not isinstance(source_bytes_read, int):
        return None
    return source_bytes_read


def changed_paths(before: list[list[object]], after: list[list[object]]) -> set[str]:
    old = {r[0]: r for r in before}
    new = {r[0]: r for r in after}
    return {p for p in set(old) | set(new) if old.get(p) != new.get(p)}


def arm_order(native_samples: int) -> list[str]:
    return ["native" if i % 2 == 0 else "rclone" for i in range(native_samples * 2 - 1)]


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


def destination_copies(mode: str, reps: int, native_samples: int, keep: bool) -> int:
    """How many copies of the comparable set the work root holds at once."""
    arms = len(arm_order(native_samples))
    copies = arms * (reps if keep else 1)
    # A dry run also keeps the source working copy under the work root.
    return copies + (1 if mode == "dry-run" else 0)


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


def budget_refusal(budget: dict[str, object], work: Path) -> Refusal | None:
    if budget["ok"]:
        return None
    return Refusal(
        "DEST_SPACE",
        f"work root {work} cannot hold {budget['copies']} destination copies"
        f" ({budget['need_bytes']} bytes) and keep the agent's"
        f" {budget['floor_percent']} % free floor: {budget['available_bytes']} of"
        f" {budget['total_bytes']} bytes available (free ratio"
        f" {budget['free_ratio']}, {budget['free_ratio_after']} after)",
    )


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
    """R-N81 on both hosts, against the fixed LOAD_LIMIT.

    A raised --dest-load-limit can never make a sample gated.
    """
    return not host_problems(source, dest, "gated", LOAD_LIMIT)


def dest_conditions() -> dict[str, object]:
    out = HELPER["conditions"]()  # type: ignore[operator]
    return dict(out)


# --------------------------------------------------------------- verdicts


def rep_verdict(samples: list[dict[str, object]], mode: str) -> dict[str, object]:
    def ms(arm: str, phase: str) -> list[float]:
        return [
            float(s["elapsed_ms"])
            for s in samples
            if s["arm"] == arm and s["phase"] == phase
        ]

    medians = {
        phase: {
            "native_ms": median(ms("native", phase)),
            "rclone_ms": median(ms("rclone", phase)),
        }
        for phase in ("initial", "delta")
    }

    def wins(phase: str) -> bool:
        m = medians[phase]
        return (
            m["native_ms"] is not None
            and m["rclone_ms"] is not None
            and m["native_ms"] < m["rclone_ms"]
        )

    natives = [s for s in samples if s["arm"] == "native"]
    rss_ok = bool(natives) and all(bool(s.get("rss_ok")) for s in natives)
    verified = all(bool(s.get("verified", {}).get("ok")) for s in samples)
    all_gated = all(bool(s.get("gated")) for s in samples)
    warm = [s for s in samples if s["phase"] == "warm-resume"]
    warm_zero = bool(warm) and all(
        s.get("bytes_received") == 0 and s.get("content_bytes_read") == 0 for s in warm
    )
    timed = [s for s in samples if s["phase"] in ("initial", "delta")]
    complete = (
        len([s for s in timed if s["arm"] == "native"]) > 0
        and len([s for s in timed if s["arm"] == "rclone"]) > 0
    )
    # Gate (a)'s rule (enforce_verdict) less its interrupted-resume term, which
    # gate (b) cannot run; `verified` is gate (b)'s own addition.
    passed = (
        complete
        and wins("initial")
        and wins("delta")
        and warm_zero
        and rss_ok
        and verified
    )
    if mode != "gated" or not all_gated:
        status = "informational"
    else:
        status = "pass" if passed else "fail"
    return {
        "status": status,
        "initial_win": wins("initial"),
        "delta_win": wins("delta"),
        "native_rss_below_2gib": rss_ok,
        "all_verified": verified,
        "all_gated": all_gated,
        "r25_warm_zero": warm_zero,
        "r25_interrupted_zero": None,
        "r25_interrupted_resume": INTERRUPTED_NOT_RUN,
        "medians": medians,
    }


def gate_rollup(report: dict[str, object]) -> dict[str, object]:
    """OI-1002-Q30 shape: B passes gate (b) iff every B rep passes (3 reps)."""
    reps = report["reps"]
    statuses = [r["verdict"]["status"] for r in reps if "verdict" in r]
    passed = sum(1 for s in statuses if s == "pass")
    counts = ab.status_counts(statuses)
    medians = {
        phase: {
            arm: median(
                [
                    float(r["verdict"]["medians"][phase][arm])
                    for r in reps
                    if "verdict" in r
                    and r["verdict"]["medians"][phase][arm] is not None
                ]
            )
            for arm in ("native_ms", "rclone_ms")
        }
        for phase in ("initial", "delta")
    }
    mode = report["mode"]
    if mode == "dry-run":
        verdict = "NOT A GATE SAMPLE"
    elif report["status"] not in (
        "complete-draft",
        "complete-under-load-informational",
    ):
        verdict = "NONE (sample aborted or refused)"
    elif mode == "under-load":
        verdict = f"{UNDER_LOAD}: B statuses {counts}"
    elif len(statuses) != GATE_B_REPS:
        verdict = f"NONE ({len(statuses)} B reps; the gate needs {GATE_B_REPS})"
    elif passed == len(statuses):
        verdict = "PASS"
    else:
        verdict = "FAIL"
    return {
        "rule": (
            "under load there is no gate (b) verdict; B statuses and medians are"
            " reported"
            if mode == "under-load"
            else "B passes gate (b) iff every B rep passes: native beats rclone on"
            " the initial copy and the 1 % delta (medians), the warm resume"
            " receives 0 bytes and reads 0 content bytes (R25), native RSS < 2 GiB,"
            " every arm verified, every sample gated at load1 < 2.5 on both hosts"
            " (OI-1002-Q30, OI-1003-Q3, R-N81); no wall-clock SLA. Unratified"
            " deviation from gate (a): no interrupted-resume phase is run or"
            " gated"
        ),
        "deviations_from_gate_a": list(DEVIATIONS),
        "b_reps": len(statuses),
        "b_reps_pass": passed,
        "b_statuses": statuses,
        "b_status_counts": counts,
        "b_medians": medians,
        "verdict": verdict,
    }


REPORT_SCHEMA: dict[str, type | tuple[type, ...]] = {
    "format": str,
    "date": str,
    "stamp": str,
    "mode": str,
    "status": str,
    "rulings": str,
    "source": dict,
    "destination": dict,
    "workload": dict,
    "arms": dict,
    "reps": list,
    "gate": dict,
    "w5_missing": list,
}
SAMPLE_SCHEMA: dict[str, type | tuple[type, ...]] = {
    "sequence": int,
    "arm": str,
    "phase": str,
    "elapsed_ms": (int, float),
    "workload_bytes": int,
    "max_rss_bytes": int,
    "gated": bool,
    "verified": dict,
    "conditions_before": dict,
}


def validate_report(report: dict[str, object]) -> list[str]:
    """Shape problems in a gate-b.json report; empty means it conforms."""
    problems = []
    for key, kind in REPORT_SCHEMA.items():
        if not isinstance(report.get(key), kind):
            problems.append(f"report.{key} is not {kind}")
    if report.get("format") != FORMAT:
        problems.append(f"report.format != {FORMAT}")
    if report.get("mode") not in ("gated", "under-load", "dry-run"):
        problems.append("report.mode is not gated|under-load|dry-run")
    for i, rep in enumerate(report.get("reps") or []):
        for key in ("index", "samples", "conditions_before", "link"):
            if key not in rep:
                problems.append(f"reps[{i}].{key} missing")
        for j, sample in enumerate(rep.get("samples") or []):
            for key, kind in SAMPLE_SCHEMA.items():
                if not isinstance(sample.get(key), kind) or (
                    kind is int and isinstance(sample.get(key), bool)
                ):
                    problems.append(f"reps[{i}].samples[{j}].{key} is not {kind}")
            if sample.get("arm") not in ("native", "rclone"):
                problems.append(f"reps[{i}].samples[{j}].arm invalid")
    gate = report.get("gate") or {}
    for key in ("rule", "verdict", "b_reps", "b_statuses"):
        if key not in gate:
            problems.append(f"gate.{key} missing")
    text = json.dumps(report, default=str)
    if redact(text) != text:
        problems.append("report holds an unredacted secret")
    return problems


# ----------------------------------------------------------------- the run


@dataclasses.dataclass
class Context:
    args: argparse.Namespace
    mode: str
    work: Path
    transport: Transport
    agent: str
    source_corpus: str
    wrapper: str
    states: str
    rows: list[list[object]]
    exclusions: dict[str, str]
    digest: str
    plan: list[list[object]]
    rclone: str
    rclone_conf: Path
    excludes: Path
    env: dict[str, str]
    native_env: dict[str, str]


def await_ready(ctx: Context, seconds: int, what: str) -> tuple[dict, dict]:
    deadline = time.monotonic() + seconds
    while True:
        source = ctx.transport.helper("conditions", {})
        dest = dest_conditions()
        problems = host_problems(source, dest, ctx.mode, ctx.args.dest_load_limit)
        if not problems:
            return source, dest
        if time.monotonic() > deadline:
            raise Abort(f"{what}: hosts not ready: {'; '.join(problems)}")
        time.sleep(15)


def run_arm(
    ctx: Context,
    rep_dir: Path,
    rep_index: int,
    sequence: int,
    arm: str,
    phase: str,
    expected: list[list[object]],
    workload_bytes: int,
    link: dict[str, object],
) -> dict[str, object]:
    source, dest = await_ready(
        ctx, ctx.args.arm_settle_seconds, f"rep{rep_index} {arm}/{phase} #{sequence}"
    )
    arm_dir = rep_dir / f"{sequence}-{arm}"
    destination = arm_dir / "destination"
    if phase == "initial":
        arm_dir.mkdir()
        destination.mkdir()
    logs = ctx.work / "logs"
    stem = f"rep{rep_index}-{sequence}-{arm}-{phase}"
    if arm == "native":
        argv = native_argv(
            ctx.agent,
            ctx.transport.host,
            ctx.source_corpus,
            destination,
            f"{ctx.states}/rep{rep_index}-{sequence}",
            arm_dir / "destination-state",
            ctx.wrapper,
            ctx.transport.config,
        )
        env = ctx.native_env
    else:
        argv = rclone_argv(
            ctx.rclone, ctx.rclone_conf, ctx.source_corpus, destination, ctx.excludes
        )
        env = ctx.env
    say(
        f"rep={rep_index} seq={sequence} arm={arm} phase={phase}"
        f" source_load1={source['load1']} source_power={source['power']}"
        f" dest_load1={dest['load1']}"
    )
    run = run_measured(argv, env, logs / f"{stem}.stdout", logs / f"{stem}.stderr")
    sample: dict[str, object] = {
        "rep": rep_index,
        "sequence": sequence,
        "arm": arm,
        "phase": phase,
        "argv": redact_argv(argv),
        "exit": run["exit"],
        "elapsed_ms": run["elapsed_ms"],
        "workload_bytes": workload_bytes,
        "max_rss_bytes": run["max_rss_bytes"],
        "conditions_before": {"source": source, "destination": dest},
        "gated": sample_gated(source, dest),
        "link_fraction": link_fraction(workload_bytes, run["elapsed_ms"], link),
    }
    problems: list[str] = []
    if arm == "native":
        parsed = parse_native(run["stdout"], run["stderr"])
        problems += native_problems(run, parsed, ctx.exclusions)
        transfer = parsed["transfer"]
        serve_rss = parsed["serve_max_rss_bytes"]
        sample.update(
            {
                "bytes_received": transfer.get("bytes_received"),
                "source_bytes_read": transfer.get("source_bytes_read"),
                "completed": transfer.get("completed"),
                "reused": transfer.get("reused"),
                "refusals_expected": len(parsed["refusals"]),
                "content_bytes_read": content_bytes_read(
                    transfer.get("source_bytes_read")
                ),
                "serve_max_rss_bytes": serve_rss,
                "source_priority": parsed["serve"].get("priority"),
                "source_priority_from": parsed["serve"].get("priority_from"),
                "timing": parsed["timing"],
                "serve_timing": parsed["serve_timing"],
                "counters": parsed["counters"],
                "rss_ok": run["max_rss_bytes"] < RSS_CAP_BYTES
                and isinstance(serve_rss, int)
                and serve_rss < RSS_CAP_BYTES,
            }
        )
    elif run["exit"] != 0:
        problems.append(
            f"rclone exit {run['exit']}: {redact(run['stderr'].strip())[-300:]}"
        )
    got, _ = HELPER["manifest"](str(destination))  # type: ignore[operator]
    sample["verified"] = verify_tree(expected, ctx.exclusions, got)
    if not sample["verified"]["ok"]:
        problems.append(f"destination does not verify: {sample['verified']}")
    if problems:
        sample["problems"] = problems
        raise Abort(
            f"rep{rep_index} {arm}/{phase} #{sequence}: {'; '.join(problems)}",
            sample=sample,
        )
    return sample


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


def run_rep(ctx: Context, index: int) -> dict[str, object]:
    rep_dir = ctx.work / "reps" / f"rep{index}"
    rep_dir.mkdir(parents=True)
    source, dest = await_ready(ctx, ctx.args.arm_settle_seconds, f"rep{index} start")
    rep: dict[str, object] = {
        "index": index,
        "label": "B",
        "conditions_before": {"source": source, "destination": dest},
        "samples": [],
    }
    link = {
        "single": ctx.transport.calibrate(ctx.args.calibrate_mib, 1),
        "parallel": ctx.transport.calibrate(
            ctx.args.calibrate_mib, ctx.args.calibrate_streams
        ),
    }
    rep["link"] = link
    samples: list[dict[str, object]] = rep["samples"]  # type: ignore[assignment]
    initial_bytes = comparable_bytes(ctx.rows, ctx.exclusions)
    order = arm_order(ctx.args.native_samples)
    try:
        for seq, arm in enumerate(order):
            samples.append(
                run_arm(
                    ctx,
                    rep_dir,
                    index,
                    seq,
                    arm,
                    "initial",
                    ctx.rows,
                    initial_bytes,
                    link,
                )
            )
        samples.append(
            run_arm(ctx, rep_dir, index, 0, "native", "warm-resume", ctx.rows, 0, link)
        )
        plan_bytes = sum(int(p[1]) for p in ctx.plan)
        rep["delta_applied"] = True
        ctx.transport.helper("xor", {"root": ctx.source_corpus, "plan": ctx.plan})
        after = ctx.transport.helper("manifest", {"root": ctx.source_corpus})
        rows_after = after["rows"]
        changed = changed_paths(ctx.rows, rows_after)
        want = {str(p[0]) for p in ctx.plan}
        if changed != want:
            raise Abort(
                f"rep{index} delta changed {len(changed)} paths, planned {len(want)}"
            )
        rep["delta"] = {
            "files": len(ctx.plan),
            "bytes": plan_bytes,
            "digest_after": after["digest"],
        }
        time.sleep(SETTLE_S)
        for seq, arm in enumerate(order):
            remove_targets(rep_dir / f"{seq}-{arm}" / "destination", ctx.plan)
            samples.append(
                run_arm(
                    ctx, rep_dir, index, seq, arm, "delta", rows_after, plan_bytes, link
                )
            )
    except Abort as abort:
        if abort.sample is not None:
            samples.append(abort.sample)
        rep["aborted"] = True
        restore_source(ctx, rep)
        raise Abort(str(abort), rep) from abort
    restore_source(ctx, rep)
    if not rep.get("restored"):
        raise Abort(
            f"rep{index}: the working copy did not restore after the delta", rep
        )
    # The restoring XOR rewrote the delta files. Now that the warm resume
    # gates, the next rep's first native arm must not capture them inside the
    # racy window (R-N76), or it records no reuse key and re-reads them.
    time.sleep(SETTLE_S)
    after_source, after_dest = post_settle(ctx)
    rep["conditions_after"] = {"source": after_source, "destination": after_dest}
    rep["verdict"] = rep_verdict(samples, ctx.mode)
    problems = host_problems(
        after_source, after_dest, ctx.mode, ctx.args.dest_load_limit
    )
    if problems:
        rep["aborted"] = True
        raise Abort(f"rep{index} post-check failed: {'; '.join(problems)}", rep)
    if not ctx.args.keep_destinations:
        release_destinations(rep_dir, order, rep)
    return rep


def release_destinations(
    rep_dir: Path, order: list[str], rep: dict[str, object]
) -> None:
    """A verified rep's own destinations leave the work root (disk budget).

    Only directories this run created under its new work root, by their exact
    names; an aborted rep keeps its destinations for diagnosis.
    """
    try:
        for seq, arm in enumerate(order):
            shutil.rmtree(rep_dir / f"{seq}-{arm}")
    except OSError as error:
        rep["destinations_released"] = False
        rep["aborted"] = True
        raise Abort(
            f"rep{rep['index']}: could not release its destinations:"
            f" {type(error).__name__}: {error}",
            rep,
        ) from error
    rep["destinations_released"] = True


def restore_source(ctx: Context, rep: dict[str, object]) -> None:
    """XOR the delta again (an involution) and prove the copy is back."""
    if not rep.get("delta_applied") or "restored" in rep:
        return
    try:
        ctx.transport.helper("xor", {"root": ctx.source_corpus, "plan": ctx.plan})
        restored = ctx.transport.helper("manifest", {"root": ctx.source_corpus})
        rep["restored"] = restored["digest"] == ctx.digest
    except Abort as abort:
        rep["restored"] = False
        rep["restore_error"] = str(abort)


def post_settle(ctx: Context) -> tuple[dict, dict]:
    deadline = time.monotonic() + ctx.args.post_settle_seconds
    while True:
        source = ctx.transport.helper("conditions", {})
        dest = dest_conditions()
        if ctx.mode != "gated" or not host_problems(
            source, dest, ctx.mode, ctx.args.dest_load_limit
        ):
            return source, dest
        if source.get("power") != "ac" or time.monotonic() > deadline:
            return source, dest
        time.sleep(10)


# --------------------------------------------------------------- evidence


def fmt(value: object, digits: int = 3) -> str:
    return ab.fmt(value, digits)


def disk_line(disk: object) -> str:
    if not isinstance(disk, dict):
        return "not measured (refused or aborted before the budget check)."
    return (
        f"free ratio {disk.get('free_ratio')} before the sample,"
        f" {disk.get('free_ratio_after')} after {disk.get('copies')} destination"
        f" copies ({fmt(disk.get('need_bytes'))} bytes, estimate); agent floor"
        f" {disk.get('floor_percent')} %; destinations"
        f" {'kept' if disk.get('keep_destinations') else 'released after each rep'}."
    )


def evidence(report: dict[str, object]) -> str:
    mode = report["mode"]
    aborted = report["status"] in ("aborted", "refused")
    gate = report["gate"]
    title = f"# S1 gate (b) neo->sting pull sample - {report['stamp']}"
    if aborted:
        title += " (ABORTED)" if report["status"] == "aborted" else " (REFUSED)"
    elif mode == "dry-run":
        title += f" ({NOT_GATE})"
    elif mode == "under-load":
        title += f" ({UNDER_LOAD})"
    else:
        title += " (DRAFT)"
    lines = [title, ""]
    if mode == "dry-run":
        lines += [
            f"> **{NOT_GATE}.** Loopback transport, synthetic corpus, no gating.",
            "",
        ]
    if mode == "under-load":
        lines += [
            f"> **{UNDER_LOAD}.** Load gates lifted on both hosts and recorded;"
            " source AC power still required before every rep and arm.",
            "",
        ]
    lines += [
        f"Status: **{report['status']}**"
        + (f" - {report['reason']}" if report.get("reason") else ""),
        "",
        f"**Gate (b) verdict for B: {gate['verdict']}** ({gate['b_reps_pass']}/"
        f"{gate['b_reps']} B reps pass). Rule: {gate['rule']}.",
        "",
        f"Rulings: {report['rulings']}. Harness: `crates/bulkload-bench/scripts/gate_b.py`;"
        " protocol: `docs/plans/2026-10-06-s1-gate-b-protocol.md`.",
        "",
        "## Identity",
        "",
    ]
    src, dst, work, arms = (
        report["source"],
        report["destination"],
        report["workload"],
        report["arms"],
    )
    lines += [
        f"- Source: `{src.get('host')}` ({src.get('system')}, {src.get('node')});"
        f" working copy `{src.get('corpus')}`; agent `{src.get('agent')}`"
        f" sha256 `{str(src.get('agent_sha256'))[:16]}`; serve priority"
        f" `{src.get('priority')}`.",
        f"- Destination: `{dst.get('node')}` ({dst.get('system')}); work root"
        f" `{dst.get('work_root')}`; agent sha256 `{str(dst.get('agent_sha256'))[:16]}`;"
        f" load limit {dst.get('load_limit')} (samples are gated at {LOAD_LIMIT}).",
        f"- Destination disk: {disk_line(dst.get('disk'))}",
        f"- Corpus: `{work.get('corpus_format')}` scale `{work.get('scale')}`, identity"
        f" `{work.get('identity')}` (recorded: {work.get('identity_recorded')}).",
        f"- Comparable set: {fmt(work.get('comparable_files'))} regular files,"
        f" {fmt(work.get('comparable_bytes'))} bytes; excluded SQLite seats:"
        f" {fmt(work.get('excluded'))} ({work.get('excluded_by_reason')}).",
        f"- Delta: {fmt(work.get('delta_files'))} files, {fmt(work.get('delta_bytes'))}"
        " bytes (1 %), XOR 0xa5, reverted after each rep.",
        f"- Native: `{arms.get('native')}`. rclone: `{arms.get('rclone_version')}`,"
        f" transport `{arms.get('rclone_transport')}`, flags `{' '.join(RCLONE_FLAGS)}`.",
        "",
        "## Repetitions",
        "",
        "| rep | link 1/N MB/s | status | initial native/rclone ms | delta native/rclone ms |"
        " rss ok | verified | gated | warm zero |",
        "|---:|---|---|---|---|---|---|---|---|",
    ]
    for rep in report["reps"]:
        link = rep.get("link", {})
        v = rep.get("verdict", {})
        m = v.get("medians", {})

        def mbps(c: dict | None) -> str:
            rate = (c or {}).get("bytes_per_s")
            return fmt(rate / 1e6, 1) if rate else "n/a"

        lines.append(
            f"| {rep['index']} | {mbps(link.get('single'))}/{mbps(link.get('parallel'))} |"
            f" {v.get('status', 'aborted')} |"
            f" {fmt(m.get('initial', {}).get('native_ms'))}/{fmt(m.get('initial', {}).get('rclone_ms'))} |"
            f" {fmt(m.get('delta', {}).get('native_ms'))}/{fmt(m.get('delta', {}).get('rclone_ms'))} |"
            f" {v.get('native_rss_below_2gib', 'n/a')} | {v.get('all_verified', 'n/a')} |"
            f" {v.get('all_gated', 'n/a')} | {v.get('r25_warm_zero', 'n/a')} |"
        )
    lines += [
        "",
        "## Samples",
        "",
        "| rep | # | arm | phase | ms | link share | bytes recv | src read | rss MiB |"
        " serve rss MiB | source load1/power | dest load1 |",
        "|---:|---:|---|---|---:|---:|---:|---:|---:|---:|---|---:|",
    ]
    for rep in report["reps"]:
        for s in rep.get("samples", []):
            cb = s["conditions_before"]
            lines.append(
                f"| {rep['index']} | {s['sequence']} | {s['arm']} | {s['phase']} |"
                f" {fmt(s['elapsed_ms'])} | {fmt(s.get('link_fraction'), 3)} |"
                f" {fmt(s.get('bytes_received'))} | {fmt(s.get('source_bytes_read'))} |"
                f" {fmt(s['max_rss_bytes'] / MIB, 1)} |"
                f" {fmt((s.get('serve_max_rss_bytes') or 0) / MIB, 1) if s['arm'] == 'native' else 'n/a'} |"
                f" {cb['source'].get('load1')}/{cb['source'].get('power')} |"
                f" {cb['destination'].get('load1')} |"
            )
    lines += [
        "",
        "## What the native arm lacks against W5 (#47)",
        "",
        *[f"- {item}" for item in report["w5_missing"]],
        "",
        "## Deviations from gate (a)'s rule (unratified)",
        "",
        *[f"- {item}" for item in gate.get("deviations_from_gate_a", DEVIATIONS)],
        f"- Interrupted resume: {INTERRUPTED_NOT_RUN}.",
        "",
        "## Notes",
        "",
        "- No wall-clock SLA (OI-1003-Q3): the gate is the comparison only.",
        "- Page cache is never dropped; the helper hashes the working copy outside"
        " the timing before the first arm and after each delta, so every timed arm"
        " starts source-hot, as in gate (a).",
        "- Raw stdout/stderr (redacted): `logs/` under the work root; everything in"
        " `gate-b.json`.",
        "",
    ]
    return "\n".join(lines)


def finish(report: dict[str, object], work: Path, evidence_path: Path | None) -> None:
    report["gate"] = gate_rollup(report)
    report = scrub(report)  # type: ignore[assignment]
    report["schema_problems"] = validate_report(report)
    (work / "gate-b.json").write_text(json.dumps(report, indent=1, default=str))
    if evidence_path is not None:
        evidence_path.parent.mkdir(parents=True, exist_ok=True)
        evidence_path.write_text(evidence(report))
    say(
        f"status={report['status']} gate={report['gate']['verdict']}"
        f" json={work / 'gate-b.json'} evidence={evidence_path}"
    )


def synthetic_source(dest: Path) -> None:
    """--dry-run: the r23 synthetic corpus plus SQLite seats to exclude."""
    corpus = dest / "corpus"
    ab.synthetic_corpus(corpus)
    (corpus / "state").mkdir()
    (corpus / "state" / "store.db").write_bytes(b"SQLite format 3\0" + b"\0" * 4080)
    (corpus / "dangling").symlink_to("nowhere")
    (corpus / "small-dir-link").symlink_to("small")
    (corpus / "state" / "odd [x]{y}*?.db").write_bytes(
        b"SQLite format 3\0" + b"\1" * 64
    )
    (corpus / "state" / "store.db-wal").write_bytes(b"\x37\x7f\x06\x82" + b"\0" * 28)
    (corpus / "odd name [x]{y}*?.txt").write_bytes(b"rclone filter escaping\n")
    (dest / "SEAL.json").write_text(json.dumps({"format": "synthetic-dry-run"}))


def base_report(args: argparse.Namespace, mode: str, work: Path) -> dict[str, object]:
    now = dt.datetime.now(dt.UTC)
    return {
        "format": FORMAT,
        "date": now.strftime("%Y-%m-%d"),
        "stamp": now.strftime("%Y-%m-%d-%H%MZ"),
        "mode": mode,
        "status": "running",
        "rulings": RULINGS_UNDER_LOAD if mode == "under-load" else RULINGS,
        "coordinator_quiet": bool(args.coordinator_quiet),
        "source": {"host": args.source_host},
        "destination": {"work_root": str(work), "load_limit": args.dest_load_limit},
        "workload": {},
        "arms": {},
        "reps": [],
        "w5_missing": list(W5_MISSING),
    }


def preflight(args: argparse.Namespace, mode: str) -> Refusal | None:
    work = Path(args.work_root)
    if not work.is_absolute() or work.exists() or not work.parent.is_dir():
        return Refusal(
            "WORK_ROOT", f"work root must be new under an existing parent: {work}"
        )
    if args.dry_run and args.under_load:
        return Refusal("MODE", "--dry-run and --under-load are exclusive")
    if mode == "dry-run":
        return None
    for name in ("source_corpus", "source_work", "remote_agent", "source_repo"):
        value = getattr(args, name)
        if not value or not value.startswith("/"):
            return Refusal(
                "SOURCE_ARGS", f"--{name.replace('_', '-')} must be an absolute path"
            )
    if not REMOTE_PATH_OK.match(args.source_work):
        return Refusal(
            "SOURCE_ARGS",
            "--source-work may hold only [A-Za-z0-9/_.-] (pull's REMOTE_EXECUTABLE rule)",
        )
    if args.ssh_config and not args.ssh_config.startswith("/"):
        return Refusal(
            "SOURCE_ARGS", "--ssh-config must be absolute (pull's SSH_CONFIG rule)"
        )
    if mode == "gated":
        if not args.coordinator_quiet:
            return Refusal("QUIET", "--coordinator-quiet is required (R-N91)")
        if (args.reps, args.native_samples) != (GATE_B_REPS, NATIVE_SAMPLES):
            return Refusal("SHAPE", f"gated mode runs {GATE_B_REPS} reps of N/R/N/R/N")
        if args.scale != "estate":
            return Refusal("SCALE", "gated mode runs the estate corpus at scale estate")
        if not 0 < args.dest_load_limit <= LOAD_LIMIT:
            return Refusal(
                "LOAD_LIMIT",
                f"gated mode holds the destination to load1 < {LOAD_LIMIT} (R-N81):"
                f" --dest-load-limit {args.dest_load_limit} may only tighten it",
            )
    return None


def evidence_target(
    args: argparse.Namespace, mode: str, repo: Path, work: Path, stamp: str
) -> tuple[Path, Refusal | None]:
    evidence_dir = (repo / "docs" / "evidence").resolve()
    if args.evidence:
        path = Path(args.evidence).resolve()
    elif mode == "dry-run":
        path = work / f"s1-gate-b-dryrun-{stamp}.md"
    elif mode == "under-load":
        path = evidence_dir / f"s1-gate-b-underload-{stamp}.md"
    else:
        path = evidence_dir / f"s1-gate-b-{stamp}.md"
    if mode == "dry-run" and path.is_relative_to(evidence_dir):
        return path, Refusal(
            "EVIDENCE", "dry-run evidence never goes under docs/evidence"
        )
    if mode == "under-load" and "underload" not in path.name:
        return path, Refusal(
            "EVIDENCE", "under-load evidence must be named *underload*"
        )
    if path.exists():
        return path, Refusal("EVIDENCE", f"evidence file exists: {path}")
    return path, None


def refuse(
    refusal: Refusal, report: dict[str, object] | None, work: Path | None
) -> int:
    say(f"refused code={refusal.code} reason={refusal.reason}")
    if report is not None and work is not None and work.is_dir():
        report["status"], report["reason"] = "refused", refusal.reason
        report["refusal"] = {"code": refusal.code, "reason": refusal.reason}
        finish(report, work, None)
    return 2


def parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--repo", default=str(Path(__file__).resolve().parents[3]))
    p.add_argument("--work-root", required=True, help="new local directory on sting")
    p.add_argument("--agent", required=True, help="local bulkload-agent (destination)")
    p.add_argument("--source-host", default="neo")
    p.add_argument("--ssh", default="ssh", help="OpenSSH client for every arm")
    p.add_argument("--ssh-config", help="absolute ssh config (passed to pull too)")
    p.add_argument("--source-python", default="/usr/bin/python3")
    p.add_argument("--source-corpus", help="sealed estate corpus DEST on the source")
    p.add_argument("--source-work", help="new working-copy root on the source")
    p.add_argument("--source-repo", help="bulkload checkout on the source (verify)")
    p.add_argument("--remote-agent", help="bulkload-agent on the source")
    p.add_argument(
        "--source-priority", choices=("background", "normal"), default="background"
    )
    p.add_argument("--native-streams", type=int, default=1)
    p.add_argument(
        "--rclone", help="rclone binary (default: nixpkgs from the repo flake)"
    )
    p.add_argument(
        "--rclone-transport", choices=("external", "internal"), default="external"
    )
    p.add_argument("--scale", choices=("estate", "small"), default="estate")
    p.add_argument("--seed", default=ec.DEFAULT_SEED)
    p.add_argument("--reps", type=int, default=GATE_B_REPS)
    p.add_argument("--native-samples", type=int, default=NATIVE_SAMPLES)
    p.add_argument(
        "--dest-load-limit",
        type=float,
        default=LOAD_LIMIT,
        help=f"destination load1 bound; gated mode refuses a value over {LOAD_LIMIT}",
    )
    p.add_argument(
        "--keep-destinations",
        action="store_true",
        help="keep every rep's destinations (budget: reps x arms copies)",
    )
    p.add_argument("--calibrate-mib", type=int, default=64)
    p.add_argument("--calibrate-streams", type=int, default=4)
    p.add_argument("--settle-seconds", type=int, default=900)
    p.add_argument("--arm-settle-seconds", type=int, default=300)
    p.add_argument("--post-settle-seconds", type=int, default=180)
    p.add_argument("--evidence")
    p.add_argument("--coordinator-quiet", action="store_true")
    p.add_argument("--dry-run", action="store_true", help=NOT_GATE)
    p.add_argument("--under-load", action="store_true", help=UNDER_LOAD)
    return p


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    mode = "dry-run" if args.dry_run else ("under-load" if args.under_load else "gated")
    repo = Path(args.repo).resolve()
    work = Path(args.work_root).absolute()
    refusal = preflight(args, mode)
    if refusal:
        return refuse(refusal, None, None)
    report = base_report(args, mode, work)
    evidence_path, refusal = evidence_target(args, mode, repo, work, report["stamp"])
    if refusal:
        return refuse(refusal, None, None)
    agent = Path(args.agent).resolve()
    help_text, refusal = probe_agent(agent, args.native_streams)
    work.mkdir(mode=0o700)
    for sub in ("logs", "reps", "bin"):
        (work / sub).mkdir()
    if refusal:
        return refuse(refusal, report, work)

    ssh_bin = shutil.which(args.ssh) or args.ssh
    if mode == "dry-run":
        sftp = next(
            (
                p
                for p in (
                    shutil.which("sftp-server"),
                    "/usr/libexec/openssh/sftp-server",
                    "/usr/lib/openssh/sftp-server",
                    "/usr/libexec/sftp-server",
                )
                if p and os.path.isfile(p)
            ),
            None,
        )
        if sftp is None:
            return refuse(
                Refusal("DRY_RUN", "no local sftp-server for the loopback"),
                report,
                work,
            )
        shim = work / "bin" / "ssh"
        shim.write_text(LOOPBACK_SHIM.format(sftp_server=shlex.quote(sftp)))
        shim.chmod(0o700)
        transport = Transport(str(shim), "gateb-loopback", None, sys.executable)
        sealed = work / "source-sealed"
        synthetic_source(sealed)
        args.source_corpus = str(sealed)
        args.source_work = str(work / "source")
        args.remote_agent = str(agent)
        args.calibrate_mib = min(args.calibrate_mib, 8)
    else:
        (work / "bin" / "ssh").symlink_to(ssh_bin)
        transport = Transport(
            str(work / "bin" / "ssh"),
            args.source_host,
            args.ssh_config,
            args.source_python,
        )
    report["destination"].update(
        {
            "agent": str(agent),
            "agent_sha256": ab.sha256(agent),
            "node": platform.node(),
            "system": platform.system(),
            "ssh": ssh_bin,
        }
    )
    try:
        probe = transport.helper(
            "probe",
            {
                "agent": args.remote_agent,
                "corpus": args.source_corpus,
                "work": args.source_work,
            },
        )
        report["source"].update(
            {
                "system": probe.get("system"),
                "node": probe.get("node"),
                "python": probe.get("python"),
                "agent": args.remote_agent,
                "agent_sha256": (probe.get("agent") or {}).get("sha256"),
                "priority": args.source_priority,
                "conditions_at_probe": {
                    k: probe.get(k) for k in ("load1", "power", "cpus")
                },
            }
        )
        if not (probe.get("agent") or {}).get("executable"):
            return refuse(
                Refusal(
                    NATIVE_ARM_MISSING,
                    "native remote arm missing (#47): no executable agent on the source",
                ),
                report,
                work,
            )
        if probe.get("work_exists") or not probe.get("work_parent_is_dir"):
            return refuse(
                Refusal(
                    "SOURCE_WORK", "--source-work must be new under an existing parent"
                ),
                report,
                work,
            )
        seal = probe.get("seal") or {}
        recorded = ec.RECORDED.get((args.scale, args.seed), (None,))[0]
        report["workload"].update(
            {
                "corpus_format": seal.get("format"),
                "scale": seal.get("scale"),
                "seed": seal.get("seed"),
                "identity": seal.get("identity"),
                "identity_recorded": recorded is not None
                and seal.get("identity") == recorded,
                "sealed": seal.get("sealed"),
            }
        )
        if mode != "dry-run":
            if (
                seal.get("format") != ec.FORMAT
                or seal.get("scale") != args.scale
                or seal.get("seed") != args.seed
            ):
                return refuse(
                    Refusal(
                        "CORPUS",
                        f"source corpus is not estate corpus {args.scale}/{args.seed}: {seal}",
                    ),
                    report,
                    work,
                )
            if mode == "gated" and not (
                report["workload"]["identity_recorded"]
                and seal.get("sealed")
                and seal.get("mutations") == 0
            ):
                return refuse(
                    Refusal(
                        "CORPUS",
                        "gated mode needs the sealed, unmutated, recorded-identity corpus",
                    ),
                    report,
                    work,
                )
            script = (
                f"{args.source_repo}/crates/bulkload-bench/scripts/estate_corpus.py"
            )
            verified = transport.run(
                [args.source_python, script, "verify", args.source_corpus]
            )
            report["workload"]["verify_exit"] = verified.returncode
            if verified.returncode != 0:
                return refuse(
                    Refusal(
                        "CORPUS", "the sealed corpus does not verify on the source"
                    ),
                    report,
                    work,
                )
        # Before anything is copied on the source: will the destinations fit?
        measured = transport.helper("measure", {"corpus": args.source_corpus})
        copies = destination_copies(
            mode, args.reps, args.native_samples, args.keep_destinations
        )
        budget = disk_budget(
            *volume_space(work),
            copies,
            int(measured["comparable_bytes"]),
            int(measured["comparable_files"]) + int(measured["directories"]),
        )
        budget["measured"] = measured
        budget["keep_destinations"] = bool(args.keep_destinations)
        report["destination"]["disk"] = budget
        refusal = budget_refusal(budget, work)
        if refusal:
            return refuse(refusal, report, work)
        flags = ["--priority=normal"] if args.source_priority == "normal" else []
        prepared = transport.helper(
            "prepare",
            {
                "corpus": args.source_corpus,
                "work": args.source_work,
                "python": args.source_python if mode != "dry-run" else sys.executable,
                "agent": args.remote_agent,
                "flags": flags,
            },
        )
        report["source"]["corpus"] = prepared["corpus"]
        manifest = transport.helper("manifest", {"root": prepared["corpus"]})
    except Abort as abort:
        return refuse(Refusal("SOURCE", str(abort)), report, work)
    rows, exclusions = manifest["rows"], manifest["exclusions"]
    if comparable_bytes(rows, exclusions) != int(measured["comparable_bytes"]):
        return refuse(
            Refusal(
                "SOURCE",
                "the working copy's comparable bytes differ from the measured"
                " corpus, so the disk budget does not describe this sample",
            ),
            report,
            work,
        )
    plan = delta_plan(rows, exclusions)
    by_reason: dict[str, int] = {}
    for why in exclusions.values():
        by_reason[why] = by_reason.get(why, 0) + 1
    report["workload"].update(
        {
            "entries": len(rows),
            "comparable_files": sum(
                1 for r in rows if r[1] == "f" and r[0] not in exclusions
            ),
            "comparable_bytes": comparable_bytes(rows, exclusions),
            "excluded": len(exclusions),
            "excluded_by_reason": by_reason,
            "manifest_digest": manifest["digest"],
            "delta_files": len(plan),
            "delta_bytes": sum(int(p[1]) for p in plan),
            "delta_seed": DELTA_SEED,
        }
    )
    excludes = work / "rclone-excludes.txt"
    write_private(excludes, "".join(rclone_glob(p) + "\n" for p in sorted(exclusions)))
    env = rclone_env()
    resolved = ssh_g(transport) if args.rclone_transport == "internal" else None
    remote, refusal = rclone_remote(transport, args.rclone_transport, resolved, env)
    if refusal:
        return refuse(refusal, report, work)
    conf = work / "rclone.conf"
    write_private(conf, rclone_config_text(remote))
    rclone = str(ab.resolve_rclone(repo, args.rclone))
    version = subprocess.run(
        [rclone, "version"], capture_output=True, text=True, env=env, check=False
    )
    report["arms"] = {
        "native": "bulkload-agent pull, one ssh -T stream (pre-W5)",
        "native_streams": args.native_streams,
        "rclone": rclone,
        "rclone_version": (version.stdout.splitlines() or ["unknown"])[0],
        "rclone_transport": args.rclone_transport,
        "rclone_remote": {k: v for k, v in remote.items()},
        "order": "".join(
            "N" if a == "native" else "R" for a in arm_order(args.native_samples)
        ),
    }
    native_env = dict(os.environ)
    native_env["PATH"] = f"{work / 'bin'}{os.pathsep}{native_env.get('PATH', '')}"
    ctx = Context(
        args=args,
        mode=mode,
        work=work,
        transport=transport,
        agent=str(agent),
        source_corpus=prepared["corpus"],
        wrapper=prepared["wrapper"],
        states=prepared["states"],
        rows=rows,
        exclusions=exclusions,
        digest=manifest["digest"],
        plan=plan,
        rclone=rclone,
        rclone_conf=conf,
        excludes=excludes,
        env=env,
        native_env=native_env,
    )
    try:
        await_ready(ctx, args.settle_seconds, "initial settle")
        time.sleep(SETTLE_S)
        for index in range(args.reps):
            report["reps"].append(run_rep(ctx, index))
        final = transport.helper("manifest", {"root": ctx.source_corpus})
        report["workload"]["restored_after"] = final["digest"] == ctx.digest
        if final["digest"] != ctx.digest:
            raise Abort("the working copy no longer matches its initial manifest")
    except Abort as abort:
        if abort.rep is not None:
            report["reps"].append(abort.rep)
        report["status"], report["reason"] = "aborted", str(abort)
        finish(report, work, evidence_path)
        return 3
    report["status"] = {
        "dry-run": "dry-run-complete-not-a-gate-sample",
        "under-load": "complete-under-load-informational",
        "gated": "complete-draft",
    }[mode]
    finish(report, work, evidence_path)
    return 0


if __name__ == "__main__":
    sys.exit(main())
