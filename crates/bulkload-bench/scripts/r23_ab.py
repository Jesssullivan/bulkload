#!/usr/bin/env python3
"""Gate (a) / R23 B/A harness for #88 (OI-1002-Q30, OI-1002-Q28, OI-1002-Q27).

Runs `bulkload-bench` built at two revisions in the order B/A/B/A/B
(OI-1002-Q30), one bench invocation per repetition. B (--rev-b, default
origin/main) is the candidate; A (--rev-a, default 7c3ecc7) is the
informational baseline. Each invocation is the full R23 bench as in
docs/evidence/r23-2026-09-18.md: native and rclone alternate N/R/N/R/N
(`--reps 3`), then warm resume, interrupted resume and the 1 % delta, so the
rclone baseline runs the same way, with the same flags, inside every
repetition. One extra native-only repetition of the v4 engine (41bf9a4 by
default) is run last, as the reference for the dedup-loss measurement.

Gate rollup (OI-1002-Q30, AGENTS.md R23 amendment): B passes the R23 gate if
and only if every B rep's bench verdict is `pass`. A never decides the gate.
An aborted or refused sample has no gate verdict.

What it reports, per repetition and as medians per revision (#88):
  - flush_barrier_ns and flush_full_ns totals over the native initial copies;
  - receive-thread stall against wall time: a direct counter if the build
    has one, otherwise seal time (every flush_*_ns) as an upper-bound proxy;
  - files/s of the native initial copy (files_materialized / elapsed);
  - dedup loss: (bytes received by v5 - bytes received by v4) / payload;
  - walk_ns, and the #112 walk-ahead wait counter when the build prints one
    (any native_timing key naming walk/slot/ahead together with wait).
Every key=value the bench prints is kept in the JSON, so a counter added
later is captured without changing this script.

Preconditions (gated mode, the default):
  - Darwin only. The bench's own R-N81 preflight reads `pmset`; on Linux it
    reports power=unknown and refuses every gated sample.
  - --coordinator-quiet: the operator or coordinator holds other lanes quiet
    (R-N91). It is recorded, not checked. --under-load does not require it.
  - The corpus is R23 corpus v1 (r23_corpus.py, OI-1002-Q28): 23 regular
    files, 239,819,837 bytes, and `r23_corpus.py verify` matches the
    committed manifest (content identity f4a7619f...). Gated mode refuses
    non-default --expect-files/--expect-bytes.
  - The work root does not exist yet. Its parent should be on the volume
    under test (TinylandState for gate a).

Corpus integrity: the sealed corpus is read-only (0444/0555). The harness
copies it once into <work>/corpus with 0644/0755 modes, because the bench
mutates its private fixture for the 1 % delta and so cannot read a
read-only tree. That working copy is content-verified before the builds.
Every rep must report the same bench `sealed_corpus_blake3` for the working
copy. That value is a content-and-metadata hash: BLAKE3 over every row's path, kind, size,
mode, content hash and stat fields (dev, ino, mtime, ctime, nlink), so it
is stable for one untouched copy and differs between copies. After the last rep, both the working copy and the
sealed corpus are verified again. Any failure aborts the sample (exit 3).

Host checks: after the builds, the harness waits up to --settle-seconds for
AC power and load1 < 2.5, and checks again before every repetition. After
every repetition, power must still be AC, every bench row must say
gated=true, and load1 must fall below 2.5 within --post-settle-seconds (the
bench's own work raises it during the rep). The bench itself checks before
every arm. A failed check ends the sample: status=aborted, exit 3. Under
--under-load only the power checks apply (see below), and every bench row
must say power=ac instead of gated=true.

Page cache: the cache is never dropped, on either host. The bench verifies
the source with a full BLAKE3 walk before every arm, so every timed arm,
native and rclone alike, starts with the source hot. That is the one
consistent state reachable without root (`purge` needs root on Darwin, and
a drop would be undone by that verification read). The harness records
residency of the working copy before each rep, and residency of the rep's
private fixture, which the timed arms actually read, right after the rep.
The bench is a single process, so the harness cannot measure in between.
Destinations are always fresh: every repetition gets a new work root, and
the bench makes a new destination per arm.

Builds: each revision is exported with `git archive` into
<work>/build-src/src-<sha12> and built in `nix develop path:<tree>#default`.
The per-revision CARGO_TARGET_DIR and the copied binaries stay under
--build-root (default <repo>/target/r23-ab) as a cache across sessions.
The harness never deletes anything outside the work root.

Evidence: <work>/r23-ab.json holds everything, and the Markdown draft goes
to docs/evidence/r23-<date>-<HHMM>Z.md. An aborted sample is titled ABORTED
and has no medians table.

--dry-run makes a tiny synthetic corpus, passes --informational and skips
the platform, corpus-verify, quiet and load checks. Its output says
NOT A GATE SAMPLE everywhere and goes to the work root. It refuses to
write under docs/evidence.

--under-load (operator rulings OI-1003-Q39 and OI-1003-Q50) is an
informational R23 sample under the host's real pressure; it is never an R23
gate verdict. It keeps the gated mode's Darwin, corpus v1, verify and seal
checks, and passes --informational to the bench. It lifts only the load gate:
load1 is recorded before and right after every rep, but it is not required
to be below 2.5, and the post-rep load wait is skipped. AC power is
still required: once before the first rep (waiting up to --settle-seconds),
before every rep, right after every rep, and on every bench sample row,
refused informational reps included. Because --informational stops the bench
refusing an arm on battery, the harness checks the rows itself. Lanes need
not be held quiet: --coordinator-quiet is not required, and when given it is
recorded as acknowledged, not as R-N91 gating. A non-B rep (A or V4) that the
bench refuses is recorded in refused_reps and the sample continues
(OI-1003-Q50); a refused B rep aborts (exit 3). The rollup reports B's bench
statuses (for example `informational x3`) and the bench's informational
native-vs-rclone initial and delta medians, not a pass count. The evidence
goes to docs/evidence/r23-underload-<date>-<HHMM>Z.md; an --evidence name
that does not contain `underload` is refused.

Exit: 0 complete (the gate verdict is in the evidence, pass or fail),
2 refused before the sample, 3 aborted during the sample, 4 build failure.
"""

from __future__ import annotations

import argparse
import collections
import datetime as dt
import hashlib
import json
import os
import platform
import random
import re
import shutil
import statistics
import subprocess
import sys
import tarfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import r23_corpus  # noqa: E402

CONTENT_IDENTITY = r23_corpus.EXPECTED_IDENTITY
LOAD_LIMIT = 2.5
# Corpus v1 from r23_corpus.py (OI-1002-Q28); the 09-18 corpus is gone.
RECORD_FILES = 23
RECORD_BYTES = 239_819_837
DEFAULT_CORPUS = (
    "/Volumes/TinylandState/tinyland-state/bulkload-r23-corpus-f4a7619f7b88/corpus"
)
DEFAULT_A = "7c3ecc7"
DEFAULT_B = "origin/main"
DEFAULT_V4 = "41bf9a4"
RULINGS = (
    "OI-1002-Q30, OI-1002-Q28, OI-1002-Q27, R23, R-N57, R-N81, R-N91, R-N134, R-N13"
)
# An under-load sample is not R-N81 load-gated and not R-N91 quiet-gated
# (OI-1003-Q39), so it does not cite them as followed.
RULINGS_UNDER_LOAD = (
    "OI-1003-Q39, OI-1003-Q50, OI-1002-Q30, OI-1002-Q28, R23, R-N57, R-N134, R-N13"
)
DEFAULT_PATTERN = "BABAB"
GATE_B_REPS = DEFAULT_PATTERN.count("B")
SEAL_KEYS = (
    "flush_barrier_ns",
    "flush_full_ns",
    "flush_fdatasync_ns",
    "flush_dir_ns",
    "flush_dir_barrier_ns",
)
KNOWN_TIMING = {
    "sequence",
    "phase",
    "scope",
    "walk_ns",
    "reuse_census_ns",
    "cdc_hash_ns",
    "queue_wait_ns",
    "transfer_ns",
    "materialize_ns",
    "publish_groups",
    "sqlite_commits",
    "sqlite_commit_ns",
}
WALK_WAIT = re.compile(r"(walk.*(wait|slot|ahead))|((slot|ahead).*wait)")
RECV_STALL = re.compile(r"(recv|receive).*(stall|seal|block).*_ns$")
PAIR = re.compile(r'(\w+)=("(?:[^"\\]|\\.)*"|\S+)')
NOT_GATE = "DRY RUN - NOT A GATE SAMPLE"
UNDER_LOAD = "INFORMATIONAL UNDER LOAD - NOT A GATE SAMPLE"


class InformationalRefusal(Exception):
    """An informational (non-B) rep that the bench refused in --under-load mode.

    OI-1003-Q50: under host pressure the A or V4 baseline may fail on its own; the
    sample records the refusal and continues, because only B decides anything.
    """

    def __init__(self, message: str, record: dict[str, object]) -> None:
        super().__init__(message)
        self.record = record


class Abort(Exception):
    """The sample ended early; the evidence is written as aborted."""

    def __init__(self, reason: str, rep: dict[str, object] | None = None) -> None:
        super().__init__(reason)
        self.rep = rep


def say(message: str) -> None:
    print(f"r23-ab {message}", flush=True)


def pairs(line: str) -> dict[str, object]:
    out: dict[str, object] = {}
    for key, raw in PAIR.findall(line):
        value: object = raw.strip('"')
        for cast in (int, float):
            try:
                value = cast(raw)
                break
            except ValueError:
                continue
        if raw in ("true", "false"):
            value = raw == "true"
        out[key] = value
    return out


def corpus_verify(corpus: Path) -> int:
    """Content identity check of the v1 corpus (r23_corpus.py verify)."""
    return subprocess.run(
        [
            sys.executable,
            str(Path(__file__).resolve().parent / "r23_corpus.py"),
            "verify",
            str(corpus),
        ],
        check=False,
    ).returncode


def power_source() -> str:
    if platform.system() == "Darwin":
        try:
            text = subprocess.run(
                ["/usr/bin/pmset", "-g", "batt"],
                capture_output=True,
                text=True,
                check=False,
            ).stdout
        except OSError:
            return "unknown"
        first = text.splitlines()[0] if text else ""
        if "'AC Power'" in first:
            return "ac"
        if "'Battery Power'" in first:
            return "battery"
        return "unknown"
    supplies = Path("/sys/class/power_supply")
    mains = (
        [
            p
            for p in supplies.glob("*")
            if (p / "type").is_file() and (p / "type").read_text().strip() == "Mains"
        ]
        if supplies.is_dir()
        else []
    )
    if not mains:
        return "unknown-no-supply-class"
    online = any((p / "online").read_text().strip() == "1" for p in mains)
    return "ac" if online else "battery"


def conditions() -> dict[str, object]:
    load1 = os.getloadavg()[0]
    power = power_source()
    return {
        "utc": dt.datetime.now(dt.UTC).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "load1": round(load1, 2),
        "power": power,
        "ok": power == "ac" and load1 < LOAD_LIMIT,
    }


def corpus_shape(root: Path) -> tuple[int, int]:
    files = size = 0
    for dirpath, _dirs, names in os.walk(root):
        for name in names:
            path = Path(dirpath) / name
            if path.is_file() and not path.is_symlink():
                files += 1
                size += path.stat().st_size
    return files, size


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def git(repo: Path, *args: str) -> str:
    return subprocess.run(
        ["git", "-C", str(repo), *args], capture_output=True, text=True, check=True
    ).stdout.strip()


def build(
    repo: Path, rev: str, build_root: Path, scratch: Path, jobs: int
) -> dict[str, str]:
    """Build bulkload-bench at `rev` in its own tree and target dir.

    The exported source tree goes under `scratch` (inside the new work root).
    `build_root` keeps only the per-revision CARGO_TARGET_DIR and the copied
    binaries, as a cache across sessions; nothing there is ever deleted.
    """
    sha = git(repo, "rev-parse", "--verify", f"{rev}^{{commit}}")
    short = sha[:12]
    binary = build_root / "bin" / f"bulkload-bench-{short}"
    recorded = binary.with_name(binary.name + ".sha256")
    cached = (
        binary.is_file()
        and recorded.is_file()
        and recorded.read_text().strip() == sha256(binary)
    )
    if not cached:
        if binary.is_file():
            say(f"cached binary {binary} has no matching sha256 record; rebuilding")
        source = scratch / f"src-{short}"
        source.mkdir(parents=True)
        archive = scratch / f"src-{short}.tar"
        git(repo, "archive", "--format=tar", "-o", str(archive), sha)
        with tarfile.open(archive) as tree:
            tree.extractall(source, filter="data")
        archive.unlink()
        env = dict(os.environ)
        env["CARGO_TARGET_DIR"] = str(build_root / f"target-{short}")
        env["CARGO_BUILD_JOBS"] = str(jobs)
        say(f"build rev={rev} sha={sha} jobs={jobs}")
        status = subprocess.run(
            [
                "nix",
                "develop",
                f"path:{source}#default",
                "--command",
                "cargo",
                "build",
                "--release",
                "--locked",
                "-p",
                "bulkload-bench",
            ],
            cwd=source,
            env=env,
            check=False,
        ).returncode
        if status != 0:
            raise SystemExit(4)
        binary.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(
            build_root / f"target-{short}" / "release" / "bulkload-bench", binary
        )
        recorded.write_text(sha256(binary) + "\n")
    digest = sha256(binary)
    if recorded.read_text().strip() != digest:
        say(f"build refused: {binary} does not match its sha256 record")
        raise SystemExit(4)
    return {"rev": rev, "sha": sha, "binary": str(binary), "sha256": digest}


def resolve_rclone(repo: Path, given: str | None) -> Path:
    if given:
        return Path(given).resolve()
    out = subprocess.run(
        [
            "nix",
            "build",
            "--no-link",
            "--print-out-paths",
            "--inputs-from",
            str(repo),
            "nixpkgs#rclone",
        ],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.split()
    return Path(out[-1]) / "bin" / "rclone"


def synthetic_corpus(root: Path) -> None:
    """A seconds-long corpus: small files, a few MiB files, one duplicate."""
    rng = random.Random(88)
    (root / "small").mkdir(parents=True)
    (root / "big").mkdir()
    (root / "empty-dir").mkdir()
    for index in range(40):
        (root / "small" / f"f{index:02}.bin").write_bytes(
            rng.randbytes(rng.randint(1, 64) * 1024)
        )
    shared = rng.randbytes(3 << 20)
    (root / "big" / "a.bin").write_bytes(shared)
    (root / "big" / "a-copy.bin").write_bytes(shared)  # cross-file duplicate
    (root / "big" / "b.bin").write_bytes(rng.randbytes(2 << 20))
    (root / "small" / "link").symlink_to("f00.bin")


def residency(binary: Path, corpus: Path) -> object:
    result = subprocess.run(
        [str(binary), "micro", "residency", "--source", str(corpus)],
        capture_output=True,
        text=True,
        check=False,
    )
    for line in result.stdout.splitlines():
        if line.startswith("micro name=residency"):
            return pairs(line).get("source_resident_fraction")
    return None


def parse_bench(stdout: str) -> dict[str, object]:
    parsed: dict[str, object] = {"samples": [], "medians": [], "header": {}}
    timing: dict[tuple[object, object], dict[str, object]] = {}
    counters: dict[tuple[object, object], dict[str, object]] = {}
    for line in stdout.splitlines():
        kind, _, rest = line.partition(" ")
        row = pairs(rest)
        if kind == "benchmark":
            parsed["header"] = row
        elif kind == "sample":
            parsed["samples"].append(row)
        elif kind == "native_timing":
            timing[(row.get("sequence"), row.get("phase"))] = row
        elif kind == "native_counters":
            counters[(row.get("sequence"), row.get("phase"))] = row
        elif kind == "median":
            parsed["medians"].append(row)
        elif kind in ("verdict", "delta", "rclone_command"):
            parsed[kind] = row
    for sample in parsed["samples"]:
        key = (sample.get("sequence"), sample.get("phase"))
        if key in timing:
            sample["timing"] = timing[key]
        if key in counters:
            sample["counters"] = counters[key]
    return parsed


def native_initial(parsed: dict[str, object]) -> list[dict[str, object]]:
    return [
        s
        for s in parsed["samples"]
        if s.get("arm") == "Native" and s.get("phase") == "initial"
    ]


def median(values: list[float]) -> float | None:
    return statistics.median(values) if values else None


def summarize(parsed: dict[str, object]) -> dict[str, object]:
    natives = native_initial(parsed)
    rclones = [
        s
        for s in parsed["samples"]
        if s.get("arm") == "Rclone" and s.get("phase") == "initial"
    ]
    deltas = [s for s in parsed["samples"] if s.get("phase") == "delta"]

    def total(key: str) -> int:
        return sum(int(s.get("counters", {}).get(key, 0)) for s in natives)

    wall_ns = sum(float(s["elapsed_ms"]) * 1e6 for s in natives)
    seal_ns = sum(total(k) for k in SEAL_KEYS)
    direct = sorted(
        {
            k
            for s in natives
            for k in {**s.get("timing", {}), **s.get("counters", {})}
            if RECV_STALL.search(k)
        }
    )
    stall_ns = (
        sum(
            int({**s.get("timing", {}), **s.get("counters", {})}.get(k, 0))
            for s in natives
            for k in direct
        )
        if direct
        else seal_ns
    )
    timing_keys = {k for s in natives for k in s.get("timing", {})}
    walk_wait_keys = sorted(k for k in timing_keys if WALK_WAIT.search(k))
    files_per_s = [
        int(s.get("counters", {}).get("files_materialized", 0))
        / (float(s["elapsed_ms"]) / 1000)
        for s in natives
        if float(s["elapsed_ms"]) > 0
    ]
    return {
        "native_initial_ms": [s["elapsed_ms"] for s in natives],
        "rclone_initial_ms": [s["elapsed_ms"] for s in rclones],
        "native_delta_ms": [
            s["elapsed_ms"] for s in deltas if s.get("arm") == "Native"
        ],
        "rclone_delta_ms": [
            s["elapsed_ms"] for s in deltas if s.get("arm") == "Rclone"
        ],
        "native_initial_median_ms": median([s["elapsed_ms"] for s in natives]),
        "rclone_initial_median_ms": median([s["elapsed_ms"] for s in rclones]),
        "flush_barrier_ns_total": total("flush_barrier_ns"),
        "flush_full_ns_total": total("flush_full_ns"),
        "seal_ns_total": seal_ns,
        "receive_stall_source": ",".join(direct)
        if direct
        else "proxy:sum(" + "+".join(SEAL_KEYS) + ")",
        "receive_stall_ns_total": stall_ns,
        "native_initial_wall_ns_total": int(wall_ns),
        "receive_stall_share_of_wall": (stall_ns / wall_ns) if wall_ns else None,
        "files_per_s_median": median(files_per_s),
        "bytes_received_median": median(
            [
                float(s["transferred_content_bytes"])
                for s in natives
                if isinstance(s.get("transferred_content_bytes"), int)
            ]
        ),
        "payload_bytes": natives[0].get("workload_bytes") if natives else None,
        "walk_ns_median": median(
            [float(s.get("timing", {}).get("walk_ns", 0)) for s in natives]
        ),
        "walk_ahead_wait_keys": walk_wait_keys,
        "walk_ahead_wait_ns_median": {
            k: median([float(s.get("timing", {}).get(k, 0)) for s in natives])
            for k in walk_wait_keys
        },
        "new_timing_keys": sorted(timing_keys - KNOWN_TIMING),
        "all_gated": all(bool(s.get("gated")) for s in parsed["samples"]),
        "all_power_ac": all(s.get("power") == "ac" for s in parsed["samples"]),
        # The bench's own native-vs-rclone medians; under load (gated=false
        # rows) they are informational and the verdict carries no wins.
        "bench_medians": {
            str(m.get("phase")): {
                "native_ms": m.get("native_ms"),
                "rclone_ms": m.get("rclone_ms"),
            }
            for m in parsed["medians"]
        },
        "verdict": parsed.get("verdict", {}),
    }


def host_ready(now: dict[str, object], under_load: bool) -> bool:
    """Gated: AC power and load1 < 2.5 (R-N81). Under load: AC power only.

    OI-1003-Q39 lifts only the load gate; power stays required.
    """
    return now["power"] == "ac" if under_load else bool(now["ok"])


def power_problems(
    after: dict[str, object], settled: dict[str, object], parsed: dict[str, object]
) -> list[str]:
    """AC power right after a rep, after the settle wait, and on every bench row.

    With --informational the bench records an arm on battery instead of
    refusing it, so under load these checks are what keep power gated.
    """
    problems = []
    if after["power"] != "ac" or settled["power"] != "ac":
        problems.append(f"power={after['power']}/{settled['power']}")
    off_ac = [s for s in parsed.get("samples", []) if s.get("power") != "ac"]
    if off_ac:
        problems.append(
            f"{len(off_ac)} bench row(s) not on AC power: "
            + ", ".join(
                f"{s.get('arm')}/{s.get('phase')} power={s.get('power')}"
                for s in off_ac
            )
        )
    return problems


def post_settle(
    args: argparse.Namespace,
) -> tuple[dict[str, object], dict[str, object]]:
    """Conditions right after a rep, and after waiting for load1 to settle.

    Power must be AC at once. Load1 may wait up to --post-settle-seconds to fall
    below the limit, because the bench's own work raises it during the rep.
    Under --under-load the wait is skipped: load is recorded, not gated
    (OI-1003-Q39).
    """
    first = conditions()
    now = first
    deadline = time.monotonic() + args.post_settle_seconds
    while (
        not args.dry_run
        and not args.under_load
        and float(now["load1"]) >= LOAD_LIMIT
        and time.monotonic() < deadline
    ):
        time.sleep(10)
        now = conditions()
    return first, now


def run_rep(
    label: str,
    info: dict[str, str],
    args: argparse.Namespace,
    work: Path,
    corpus: Path,
    rclone: Path,
    index: int,
    native_only: bool,
    state: dict[str, object],
) -> dict[str, object]:
    root = work / "reps" / f"rep{index}-{label}-{info['sha'][:12]}"
    logs = work / "logs"
    before = conditions()
    if not args.dry_run and not host_ready(before, args.under_load):
        raise Abort(f"rep{index} {label} precondition failed: {before}")
    source_cache = residency(Path(info["binary"]), corpus)
    command = [
        info["binary"],
        "--corpus-root",
        str(corpus),
        "--work-root",
        str(root),
        "--rclone",
        str(rclone),
        "--revision",
        info["sha"],
    ]
    command += ["--only", "native", "--reps", "1"] if native_only else ["--reps", "3"]
    if args.dry_run or args.under_load:
        command.append("--informational")
    say(
        f"rep={index} label={label} sha={info['sha'][:12]} load1={before['load1']} "
        f"power={before['power']} source_residency={source_cache}"
    )
    started = time.monotonic()
    result = subprocess.run(command, capture_output=True, text=True, check=False)
    wall_s = time.monotonic() - started
    (logs / f"rep{index}-{label}.stdout").write_text(result.stdout)
    (logs / f"rep{index}-{label}.stderr").write_text(result.stderr)
    after, settled = post_settle(args)
    parsed = parse_bench(result.stdout)
    power = [] if args.dry_run else power_problems(after, settled, parsed)
    if "verdict" not in parsed:
        message = (
            f"rep{index} {label} bench refused (exit {result.returncode}): "
            f"{result.stderr.strip().splitlines()[-1:]}"
        )
        # A refused informational rep is recorded only when power held; a
        # power failure ends the sample like any other rep (OI-1003-Q39).
        if args.under_load and label != "B" and not power:
            raise InformationalRefusal(
                message,
                {
                    "index": index,
                    "label": label,
                    "sha": info["sha"],
                    "exit": result.returncode,
                    "wall_s": round(wall_s, 3),
                    "reason": message,
                    "conditions_before": before,
                    "conditions_after": after,
                    "conditions_after_settled": settled,
                },
            )
        raise Abort("; ".join([message, *power]))
    # The timed arms read the bench's private per-rep fixture, not the sealed
    # source; its residency is measured after the rep (the bench is one process).
    fixture = root / "native-sealed-fixture"
    fixture_cache = (
        residency(Path(info["binary"]), fixture) if fixture.is_dir() else None
    )
    summary = summarize(parsed)
    sealed = parsed["header"].get("sealed_corpus_blake3")
    rep = {
        "index": index,
        "label": label,
        "sha": info["sha"],
        "native_only": native_only,
        "command": command,
        "exit": result.returncode,
        "wall_s": round(wall_s, 3),
        "conditions_before": before,
        "conditions_after": after,
        "conditions_after_settled": settled,
        "source_resident_fraction": source_cache,
        "fixture_resident_fraction_after": fixture_cache,
        "sealed_corpus_blake3": sealed,
        "summary": summary,
        "parsed": parsed,
    }
    problems = []
    expected = state.setdefault("sealed_corpus_blake3", sealed)
    if sealed is None or sealed != expected:
        problems.append(f"sealed_corpus_blake3 {sealed} != first rep's {expected}")
    problems += power
    # OI-1003-Q39: an under-load sample records load but does not gate on it,
    # and its rows are gated=false by design; power is checked above instead.
    if not args.dry_run and not args.under_load:
        if float(settled["load1"]) >= LOAD_LIMIT:
            problems.append(
                f"load1={settled['load1']} still >= {LOAD_LIMIT} after "
                f"{args.post_settle_seconds}s (right after rep: {after['load1']})"
            )
        if not summary["all_gated"]:
            problems.append("a bench row was gated=false")
    if problems:
        rep["aborted"] = True
        raise Abort(f"rep{index} {label} post-check failed: {'; '.join(problems)}", rep)
    return rep


def status_counts(statuses: list[object]) -> str:
    """`informational x3`, `pass x2, fail x1`: bench statuses in first-seen order."""
    counts = collections.Counter(str(status) for status in statuses)
    return ", ".join(f"{status} x{n}" for status, n in counts.items()) or "none"


def bench_medians(reps: list[dict[str, object]]) -> dict[str, dict[str, object]]:
    """Median over reps of the bench's own native/rclone medians, per phase."""
    out: dict[str, dict[str, object]] = {}
    for phase in ("initial", "delta"):
        out[phase] = {}
        for arm in ("native_ms", "rclone_ms"):
            values = [
                r["summary"].get("bench_medians", {}).get(phase, {}).get(arm)
                for r in reps
            ]
            out[phase][arm] = median(
                [float(v) for v in values if isinstance(v, int | float)]
            )
    return out


def medians_text(medians: dict[str, dict[str, object]]) -> str:
    parts = [
        f"{phase} native {fmt(m['native_ms'])} ms vs rclone {fmt(m['rclone_ms'])} ms"
        for phase, m in medians.items()
        if m["native_ms"] is not None or m["rclone_ms"] is not None
    ]
    if not parts:
        return "the bench printed no median rows"
    return "median of the bench's informational medians: " + "; ".join(parts)


def gate_rollup(report: dict[str, object]) -> dict[str, object]:
    """OI-1002-Q30: B passes R23 iff every B rep's bench verdict is pass.

    Under load (OI-1003-Q39) there is no gate verdict: the bench reports
    `status=informational` with no wins, so a pass count would read as a
    failure. The rollup names B's statuses and medians instead.
    """
    b_reps = [r for r in report["reps"] if r["label"] == "B"]
    statuses = [r["summary"]["verdict"].get("status") for r in b_reps]
    passed = sum(1 for status in statuses if status == "pass")
    counts = status_counts(statuses)
    medians = bench_medians(b_reps)
    under_load = report["mode"] == "under-load"
    if report["mode"] == "dry-run":
        verdict = "NOT A GATE SAMPLE"
    elif report["status"] not in (
        "complete-draft",
        "complete-under-load-informational",
    ):
        verdict = "NONE (sample aborted or refused)"
    elif under_load:
        verdict = f"{UNDER_LOAD}: B bench statuses {counts}; {medians_text(medians)}"
    elif len(b_reps) != GATE_B_REPS:
        verdict = f"NONE ({len(b_reps)} B reps; the gate needs {GATE_B_REPS})"
    elif passed == len(b_reps):
        verdict = "PASS"
    else:
        verdict = "FAIL"
    return {
        "rule": (
            "under load there is no R23 verdict; B's bench statuses and the"
            " bench's informational native-vs-rclone medians are reported"
            " (OI-1003-Q39, OI-1003-Q50)"
            if under_load
            else "B passes R23 iff every B rep's bench verdict passes;"
            " A is informational (OI-1002-Q30)"
        ),
        "b_reps": len(b_reps),
        "b_reps_pass": passed,
        "b_statuses": statuses,
        "b_status_counts": counts,
        "b_bench_medians": medians,
        "verdict": verdict,
    }


def per_revision(reps: list[dict[str, object]], label: str) -> dict[str, object]:
    chosen = [r for r in reps if r["label"] == label]
    flat = [v for r in chosen for v in r["summary"]["native_initial_ms"]]

    def rep_median(key: str) -> float | None:
        values = [r["summary"][key] for r in chosen if r["summary"][key] is not None]
        return median(values)

    return {
        "reps": len(chosen),
        "native_initial_median_ms_all_samples": median(flat),
        "native_initial_median_ms_of_rep_medians": rep_median(
            "native_initial_median_ms"
        ),
        "rclone_initial_median_ms_all_samples": median(
            [v for r in chosen for v in r["summary"]["rclone_initial_ms"]]
        ),
        "flush_barrier_ns_total_median": rep_median("flush_barrier_ns_total"),
        "flush_full_ns_total_median": rep_median("flush_full_ns_total"),
        "receive_stall_ns_total_median": rep_median("receive_stall_ns_total"),
        "receive_stall_share_median": rep_median("receive_stall_share_of_wall"),
        "files_per_s_median": rep_median("files_per_s_median"),
        "bytes_received_median": rep_median("bytes_received_median"),
        "walk_ns_median": rep_median("walk_ns_median"),
        "walk_ahead_wait_keys": sorted(
            {k for r in chosen for k in r["summary"]["walk_ahead_wait_keys"]}
        ),
    }


def fmt(value: object, digits: int = 3) -> str:
    if value is None:
        return "n/a"
    if isinstance(value, float):
        return f"{value:,.{digits}f}"
    if isinstance(value, int):
        return f"{value:,}"
    return str(value)


def evidence(report: dict[str, object]) -> str:
    dry = report["mode"] == "dry-run"
    aborted = report["status"] in ("aborted", "refused")
    gate = report["gate"]
    lines = []
    title = f"# R23 gate (a) B/A sample for #88 - {report['stamp']}"
    if aborted:
        title += " (ABORTED)"
    elif dry:
        title += f" ({NOT_GATE})"
    elif report["mode"] == "under-load":
        title += f" ({UNDER_LOAD})"
    else:
        title += " (DRAFT)"
    lines += [title, ""]
    if dry:
        lines += [
            f"> **{NOT_GATE}.** Synthetic corpus, `--informational`, no host gating.",
            "",
        ]
    if report.get("refused_reps"):
        lines += [
            "Informational reps the bench refused under load (recorded, not fatal; OI-1003-Q50):",
            "",
        ]
        lines += [f"- {r['reason']}" for r in report["refused_reps"]]
        lines += [""]
    under_load = report["mode"] == "under-load"
    if under_load:
        lines += [
            f"> **{UNDER_LOAD}.** Sealed corpus v1, `--informational`, by operator"
            " rulings OI-1003-Q39 and OI-1003-Q50: the run measures the engine"
            " under the host's real pressure. The R-N81 load gate and R-N91 quiet"
            " lanes are set aside (OI-1003-Q39); load1 is recorded before and right"
            " after every rep. AC power is still required before the"
            " first rep, before and after every rep and on every bench row. A"
            " refused A or V4 rep is recorded and the sample continues"
            " (OI-1003-Q50). It is not an R23 gate verdict.",
            "",
        ]
        result = (
            f"**Informational result for B, not an R23 gate verdict:**"
            f" {gate['verdict']}. Rule: {gate['rule']}."
        )
        quiet = f"coordinator-quiet: `{report['coordinator_quiet']}`" + (
            " (acknowledged only; an under-load sample is not R-N91 gated)"
            if report["coordinator_quiet"]
            else " (not required under load, OI-1003-Q39)"
        )
    else:
        result = (
            f"**R23 gate verdict for B: {gate['verdict']}** ({gate['b_reps_pass']}/"
            f"{gate['b_reps']} B reps pass). Rule: {gate['rule']}."
        )
        quiet = (
            f"coordinator-quiet acknowledged: `{report['coordinator_quiet']}` (R-N91)"
        )
    lines += [
        f"Status: **{report['status']}**"
        + (f" - {report['reason']}" if report.get("reason") else ""),
        "",
        result,
        "",
        f"Rulings: {report['rulings']}. Harness:"
        " `crates/bulkload-bench/scripts/r23_ab.py`.",
        "",
        "## Identity",
        "",
        f"- Host: `{report['host']}` ({report['platform']}); mode `{report['mode']}`;"
        f" order `{report['pattern']}`; {quiet}.",
        f"- Sealed corpus: `{report['sealed_corpus']}`. Working copy read by the bench:"
        f" `{report['corpus']}`, {fmt(report['corpus_files'])} regular files,"
        f" {fmt(report['corpus_bytes'])} bytes.",
        f"- Content identity `{report['content_identity']}`; content_verified before:"
        f" `{report['content_verified_before']}`, after the last rep:"
        f" `{report.get('content_verified_after', 'n/a')}`.",
        f"- Bench `sealed_corpus_blake3` (content-and-metadata hash of the working"
        f" copy, stat fields included; must match in every rep):"
        f" `{report.get('sealed_identity', 'n/a')}`.",
        f"- Work root: `{report['work_root']}`.",
        f"- rclone: `{report['rclone']}` ({report.get('rclone_version', 'n/a')}), the r23-2026-09-18 flags.",
        "- Page cache: never dropped. The bench reads the whole source (BLAKE3) before"
        " every arm, so every timed arm starts source-hot. Residency is logged for the"
        " working copy before each rep and for the rep's private fixture (what the timed"
        " arms read) right after it.",
        "- Destinations: a new work root per repetition and a new destination per arm.",
        "",
        "| label | rev | sha | binary sha256 |",
        "|---|---|---|---|",
    ]
    for label, info in report["builds"].items():
        lines.append(
            f"| {label} | `{info['rev']}` | `{info['sha'][:12]}` | `{info['sha256'][:16]}` |"
        )
    lines += [
        "",
        "## Per-rep bench verdicts",
        "",
        "| # | label | verdict | r23 initial win | r23 delta win | r25 warm zero |"
        " r25 interrupted zero | rss < 2 GiB |",
        "|---:|---|---|---|---|---|---|---|",
    ]
    for rep in report["reps"]:
        v = rep["summary"]["verdict"]
        lines.append(
            f"| {rep['index']} | {rep['label']} | {v.get('status', 'n/a')} |"
            f" {v.get('r23_initial_win', 'n/a')} | {v.get('r23_delta_win', 'n/a')} |"
            f" {v.get('r25_warm_zero', 'n/a')} | {v.get('r25_interrupted_zero', 'n/a')} |"
            f" {v.get('native_rss_below_2gib', 'n/a')} |"
        )
    if under_load:
        lines += [
            "",
            "## Bench medians (informational, under load)",
            "",
            "The bench prints these with `gated=false` when a row ran outside the"
            " R-N81 load gate; they are a measurement under pressure, not a win.",
            "",
            "| # | label | status | initial native ms | initial rclone ms |"
            " delta native ms | delta rclone ms | all rows on AC |",
            "|---:|---|---|---:|---:|---:|---:|---|",
        ]
        for rep in report["reps"]:
            s = rep["summary"]
            m = s.get("bench_medians", {})
            lines.append(
                f"| {rep['index']} | {rep['label']} |"
                f" {s['verdict'].get('status', 'n/a')} |"
                f" {fmt(m.get('initial', {}).get('native_ms'))} |"
                f" {fmt(m.get('initial', {}).get('rclone_ms'))} |"
                f" {fmt(m.get('delta', {}).get('native_ms'))} |"
                f" {fmt(m.get('delta', {}).get('rclone_ms'))} |"
                f" {s.get('all_power_ac', 'n/a')} |"
            )
        b_medians = gate["b_bench_medians"]
        lines.append(
            f"| B median | B | {gate['b_status_counts']} |"
            f" {fmt(b_medians['initial']['native_ms'])} |"
            f" {fmt(b_medians['initial']['rclone_ms'])} |"
            f" {fmt(b_medians['delta']['native_ms'])} |"
            f" {fmt(b_medians['delta']['rclone_ms'])} | |"
        )
    lines += [
        "",
        "## Repetitions",
        "",
        "| # | label | load1 before/after/settled | power | residency source/fixture |"
        " native initial median ms | rclone initial median ms | flush_barrier_ns |"
        " flush_full_ns | stall share | files/s | bytes received | gated |",
        "|---:|---|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|",
    ]
    for rep in report["reps"]:
        s = rep["summary"]
        lines.append(
            f"| {rep['index']} | {rep['label']} | {rep['conditions_before']['load1']}/"
            f"{rep['conditions_after']['load1']}/{rep['conditions_after_settled']['load1']} |"
            f" {rep['conditions_before']['power']} | {fmt(rep['source_resident_fraction'])}/"
            f"{fmt(rep['fixture_resident_fraction_after'])} | {fmt(s['native_initial_median_ms'])} |"
            f" {fmt(s['rclone_initial_median_ms'])} | {fmt(s['flush_barrier_ns_total'])} |"
            f" {fmt(s['flush_full_ns_total'])} | {fmt(s['receive_stall_share_of_wall'], 4)} |"
            f" {fmt(s['files_per_s_median'], 1)} | {fmt(s['bytes_received_median'], 0)} |"
            f" {s['all_gated']} |"
        )
    if not aborted:
        lines += [
            "",
            "## Medians per revision",
            "",
            "| metric | B | A (informational) |",
            "|---|---:|---:|",
        ]
        medians = report["per_revision"]
        for key in medians.get("B", {}):
            lines.append(
                f"| {key} | {fmt(medians['B'][key])} | {fmt(medians.get('A', {}).get(key))} |"
            )
        dedup = report.get("dedup", {})
        lines += [
            "",
            "## Dedup loss (#88)",
            "",
            f"- Payload P: {fmt(dedup.get('payload_bytes'))} bytes.",
            f"- v4 reference ({report['builds'].get('V4', {}).get('rev', 'skipped')}) bytes received"
            f" (unique chunk bytes U): {fmt(dedup.get('v4_bytes_received'), 0)}.",
            f"- B bytes received: {fmt(dedup.get('b_bytes_received'), 0)}; duplicate share"
            f" (B - U) / P: {fmt(dedup.get('b_dup_share'), 4)}.",
            f"- A bytes received: {fmt(dedup.get('a_bytes_received'), 0)}; duplicate share"
            f" (A - U) / P: {fmt(dedup.get('a_dup_share'), 4)}.",
            f"- v4 native initial ms (one rep): {fmt(dedup.get('v4_native_initial_ms'))}.",
        ]
    lines += [
        "",
        "## Notes",
        "",
        "- Stall: `receive_stall_source` in the JSON names a direct counter when the build"
        " has one; otherwise it is the seal-time proxy (all flush_*_ns, process-wide), an"
        " upper bound on receive-thread stall.",
        "- Walk: walk-ahead wait keys found (#112):"
        f" `{sorted({k for r in report['reps'] for k in r['summary']['walk_ahead_wait_keys']})}`.",
        "- Gate (a) is single-host (R-N134: no cross-host run before W5).",
        "- Raw bench stdout/stderr: `logs/` under the work root; every counter in `r23-ab.json`.",
        "",
    ]
    return "\n".join(lines)


def finish(report: dict[str, object], work: Path, evidence_path: Path) -> None:
    builds = report["builds"]
    reps = report["reps"]
    report["per_revision"] = {
        label: per_revision(reps, label) for label in ("B", "A") if label in builds
    }
    v4 = next((r for r in reps if r["label"] == "V4"), None)
    payload = next(
        (r["summary"]["payload_bytes"] for r in reps if r["summary"]["payload_bytes"]),
        None,
    )
    dedup: dict[str, object] = {"payload_bytes": payload}
    if v4 is not None:
        unique = v4["summary"]["bytes_received_median"]
        dedup["v4_bytes_received"] = unique
        dedup["v4_native_initial_ms"] = v4["summary"]["native_initial_median_ms"]
        for label in ("A", "B"):
            got = report["per_revision"].get(label, {}).get("bytes_received_median")
            dedup[f"{label.lower()}_bytes_received"] = got
            if got is not None and unique is not None and payload:
                dedup[f"{label.lower()}_dup_share"] = (got - unique) / payload
    report["dedup"] = dedup
    first = next((r for r in reps if r["parsed"].get("header")), None)
    if first:
        header = first["parsed"]["header"]
        report["sealed_identity"] = header.get("sealed_corpus_blake3")
        report["rclone_version"] = header.get("rclone_version")
    report["gate"] = gate_rollup(report)
    (work / "r23-ab.json").write_text(json.dumps(report, indent=2, default=str))
    evidence_path.parent.mkdir(parents=True, exist_ok=True)
    evidence_path.write_text(evidence(report))
    say(
        f"status={report['status']} gate={report['gate']['verdict']} "
        f"json={work / 'r23-ab.json'} evidence={evidence_path}"
    )


def working_copy(sealed: Path, work: Path) -> Path:
    """Copy the (read-only) sealed corpus into the work root, writable.

    The bench copies its source into a private fixture with modes preserved
    and then mutates that fixture for the 1 % delta, so it cannot read a
    0444/0555 sealed tree directly. The copy keeps mtimes and gets 0644/0755.
    """
    corpus = work / "corpus"
    shutil.copytree(sealed, corpus, symlinks=True)
    for dirpath, dirs, names in os.walk(corpus):
        for name in dirs:
            (Path(dirpath) / name).chmod(0o755)
        for name in names:
            (Path(dirpath) / name).chmod(0o644)
    corpus.chmod(0o755)
    return corpus


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--repo", default=str(Path(__file__).resolve().parents[3]))
    parser.add_argument(
        "--corpus", default=DEFAULT_CORPUS, help="sealed corpus root (gated mode)"
    )
    parser.add_argument(
        "--work-root", required=True, help="new directory on the volume under test"
    )
    parser.add_argument(
        "--rclone", help="rclone binary (default: nixpkgs from the repo flake)"
    )
    parser.add_argument("--rev-a", default=DEFAULT_A)
    parser.add_argument("--rev-b", default=DEFAULT_B)
    parser.add_argument(
        "--rev-v4", default=DEFAULT_V4, help="'' skips the dedup reference"
    )
    parser.add_argument("--pattern", default=DEFAULT_PATTERN)
    parser.add_argument("--build-root", help="default: <repo>/target/r23-ab")
    parser.add_argument("--build-jobs", type=int, default=4)
    parser.add_argument("--settle-seconds", type=int, default=900)
    parser.add_argument("--post-settle-seconds", type=int, default=180)
    parser.add_argument("--expect-files", type=int, default=RECORD_FILES)
    parser.add_argument("--expect-bytes", type=int, default=RECORD_BYTES)
    parser.add_argument(
        "--evidence",
        help="default: docs/evidence/r23-<date>-<HHMM>Z.md (dry run: work root)",
    )
    parser.add_argument(
        "--coordinator-quiet",
        action="store_true",
        help="other lanes are held quiet (R-N91); required in gated mode,"
        " only acknowledged under --under-load",
    )
    parser.add_argument("--dry-run", action="store_true", help=NOT_GATE)
    parser.add_argument(
        "--under-load",
        action="store_true",
        help=f"{UNDER_LOAD}: sealed corpus, bench --informational, load recorded"
        " but not gated, AC power still required (OI-1003-Q39, OI-1003-Q50)",
    )
    args = parser.parse_args(argv)

    repo = Path(args.repo).resolve()
    work = Path(args.work_root).resolve()
    now_utc = dt.datetime.now(dt.UTC)
    date = now_utc.strftime("%Y-%m-%d")
    stamp = now_utc.strftime("%Y-%m-%d-%H%MZ")
    if work.exists() or not work.parent.is_dir():
        say(f"refused: work root must be new under an existing parent: {work}")
        return 2
    if not set(args.pattern) <= {"A", "B"} or "B" not in args.pattern:
        say("refused: --pattern uses only A and B and needs at least one B")
        return 2
    if args.dry_run and args.under_load:
        say("refused: --dry-run and --under-load are exclusive")
        return 2
    sealed = Path(args.corpus).resolve()
    if not args.dry_run:
        if platform.system() != "Darwin":
            say(
                "refused: gated samples need Darwin (the bench R-N81 preflight reads pmset); run on neo"
            )
            return 2
        # OI-1003-Q39: an under-load sample runs with the lanes as they are.
        if not args.coordinator_quiet and not args.under_load:
            say("refused: --coordinator-quiet is required (R-N91)")
            return 2
        if args.pattern != DEFAULT_PATTERN:
            say(
                f"refused: gated mode runs only --pattern {DEFAULT_PATTERN}"
                " (OI-1002-Q30)"
            )
            return 2
        if (args.expect_files, args.expect_bytes) != (RECORD_FILES, RECORD_BYTES):
            say("refused: gated mode only runs R23 corpus v1 (default --expect-*)")
            return 2
        files, size = corpus_shape(sealed)
        if (files, size) != (RECORD_FILES, RECORD_BYTES):
            say(
                f"refused: corpus shape {files} files/{size} bytes, expected "
                f"{RECORD_FILES}/{RECORD_BYTES}"
            )
            return 2
        if corpus_verify(sealed) != 0:
            say(f"refused: sealed corpus does not verify to {CONTENT_IDENTITY}")
            return 2
    evidence_dir = (repo / "docs" / "evidence").resolve()
    evidence_path = (
        Path(args.evidence).resolve()
        if args.evidence
        else (
            work / f"r23-dryrun-{stamp}.md"
            if args.dry_run
            else evidence_dir
            / (f"r23-underload-{stamp}.md" if args.under_load else f"r23-{stamp}.md")
        )
    )
    if args.dry_run and evidence_path.is_relative_to(evidence_dir):
        say("refused: dry-run evidence never goes under docs/evidence")
        return 2
    if args.under_load and "underload" not in evidence_path.name:
        say(
            "refused: under-load evidence must be named *underload* so it is"
            f" never read as a gate sample: {evidence_path.name}"
        )
        return 2
    if evidence_path.exists():
        say(f"refused: evidence file exists: {evidence_path}")
        return 2

    work.mkdir(mode=0o700)
    (work / "logs").mkdir()
    (work / "reps").mkdir()
    (work / "build-src").mkdir()
    if args.dry_run:
        corpus = work / "corpus"
        synthetic_corpus(corpus)
        verified_before: object = "n/a (synthetic dry-run corpus)"
    else:
        corpus = working_copy(sealed, work)
        if corpus_verify(corpus) != 0:
            say("refused: the working copy does not verify")
            return 2
        verified_before = True
    files, size = corpus_shape(corpus)
    build_root = (
        Path(args.build_root) if args.build_root else repo / "target" / "r23-ab"
    )
    build_root.mkdir(parents=True, exist_ok=True)
    scratch = work / "build-src"
    builds = {
        "B": build(repo, args.rev_b, build_root, scratch, args.build_jobs),
        "A": build(repo, args.rev_a, build_root, scratch, args.build_jobs),
    }
    if args.rev_v4:
        builds["V4"] = build(repo, args.rev_v4, build_root, scratch, args.build_jobs)
    rclone = resolve_rclone(repo, args.rclone)
    report: dict[str, object] = {
        "date": date,
        "stamp": stamp,
        "mode": "dry-run"
        if args.dry_run
        else ("under-load" if args.under_load else "gated"),
        "host": platform.node(),
        "platform": platform.platform(),
        "coordinator_quiet": args.coordinator_quiet,
        "coordinator_quiet_meaning": (
            "acknowledged only; an under-load sample is not R-N91 gated (OI-1003-Q39)"
            if args.under_load
            else "R-N91: other lanes held quiet (recorded, not checked)"
        ),
        "sealed_corpus": str(sealed) if not args.dry_run else "n/a (synthetic)",
        "corpus": str(corpus),
        "corpus_files": files,
        "corpus_bytes": size,
        "content_identity": CONTENT_IDENTITY if not args.dry_run else "n/a (synthetic)",
        "content_verified_before": verified_before,
        "work_root": str(work),
        "rclone": str(rclone),
        "builds": builds,
        "pattern": args.pattern,
        "load_limit": LOAD_LIMIT,
        "load_gated": not args.under_load,
        "rulings": RULINGS_UNDER_LOAD if args.under_load else RULINGS,
        "reps": [],
        "status": "running",
    }
    if not args.dry_run:
        # Gated: AC and load1 < 2.5. Under load: AC power once before the
        # first rep; the load gate alone is lifted (OI-1003-Q39).
        deadline = time.monotonic() + args.settle_seconds
        while not host_ready(now := conditions(), args.under_load):
            if time.monotonic() > deadline:
                report["status"], report["reason"] = (
                    "refused",
                    f"host never settled ({'AC power' if args.under_load else 'AC power and load1'}): {now}",
                )
                finish(report, work, evidence_path)
                return 2
            time.sleep(15)
    order = [(label, False) for label in args.pattern]
    if args.rev_v4:
        order.append(("V4", True))
    state: dict[str, object] = {}
    try:
        for index, (label, native_only) in enumerate(order):
            try:
                report["reps"].append(
                    run_rep(
                        label,
                        builds[label],
                        args,
                        work,
                        corpus,
                        rclone,
                        index,
                        native_only,
                        state,
                    )
                )
            except InformationalRefusal as refusal:
                say(f"{refusal}; recorded and continuing (informational, OI-1003-Q50)")
                report.setdefault("refused_reps", []).append(refusal.record)
        if not args.dry_run:
            after = corpus_verify(corpus) == 0 and corpus_verify(sealed) == 0
            report["content_verified_after"] = after
            if not after:
                raise Abort("corpus no longer verifies after the last rep")
    except Abort as reason:
        if reason.rep is not None:
            report["reps"].append(reason.rep)
        report["status"], report["reason"] = "aborted", str(reason)
        finish(report, work, evidence_path)
        return 3
    report["status"] = (
        "dry-run-complete-not-a-gate-sample"
        if args.dry_run
        else (
            "complete-under-load-informational" if args.under_load else "complete-draft"
        )
    )
    finish(report, work, evidence_path)
    return 0


if __name__ == "__main__":
    sys.exit(main())
