#!/usr/bin/env python3
"""Gate (a) measurement: corpus-shaped durable floor interleaved with rclone.

M2 W2 lands this script; M2 W3 takes the gated measurement (R-N62, R-N81,
R-N87, TIN-4541). One process runs every arm, and each repetition runs the
arms in an order that rotates per repetition. No arm runs in a fixed block.

Arms (per repetition):
  floor-full     bulkload-bench micro durable-corpus --variant full
  floor-barrier  bulkload-bench micro durable-corpus --variant barrier
  rclone-shipped rclone copy with the R23 bench flags (clones on APFS)
  rclone-noclone the same plus --local-no-clone

Every floor sample reads the source (--read-source) and writes one file per
corpus file at its real size. Each file gets its own flush, and the created
directories and the parent get the same kind. Source page-cache residency is
logged before every floor sample. The page cache is never dropped. Every row
records the 1-minute load and the power source. The script refuses to start
unless the host is on AC power with load1 < 2.5 (R-N81).
M0_ALLOW_UNGATED=1 overrides that for rehearsal, and those rows say gated=0.

What each elapsed time covers. A floor row is the benchmark's own in-process
total_ms: from creating the destination to the last directory flush,
including the source reads. It excludes process start, the residency probe
and the untimed drain flush. An rclone row is the wall time of the whole
rclone process, including its start, listing and MD5 checks. The floor
writes one repeated block only when --read-source is off, which this script
never does. Whether F_FULLFSYNC reaches media on the enclosure under test is
not proven by this script.

Usage:
  m0_gate_a.py BENCH RCLONE CORPUS WORK_ROOT [REPS]

WORK_ROOT must not exist. Its parent must be on the volume under test.
REPS defaults to 7. Output is key=value lines on stdout, ending with
min/median/max per arm.
"""

from __future__ import annotations

import os
import re
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path

RCLONE_FLAGS = [
    "--config",
    "/dev/null",
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
]
ARMS = ["floor-full", "floor-barrier", "rclone-shipped", "rclone-noclone"]
LOAD_LIMIT = 2.5


def power_source() -> str:
    """Return 'ac', 'battery' or 'unknown' from pmset."""
    try:
        text = subprocess.run(
            ["/usr/bin/pmset", "-g", "batt"],
            capture_output=True,
            text=True,
            check=False,
        ).stdout
    except OSError:
        return "unknown"
    if "AC Power" in text:
        return "ac"
    if "Battery Power" in text:
        return "battery"
    return "unknown"


def conditions() -> tuple[float, str, bool]:
    load1 = os.getloadavg()[0]
    power = power_source()
    return load1, power, power == "ac" and load1 < LOAD_LIMIT


def regular_sizes(root: Path) -> list[tuple[str, int]]:
    rows = []
    for path in sorted(root.rglob("*")):
        if path.is_file() and not path.is_symlink():
            rows.append((str(path.relative_to(root)), path.stat().st_size))
    return rows


def floor(bench: Path, corpus: Path, work: Path, variant: str) -> tuple[float, str]:
    target = work / "floor"
    target.mkdir(exist_ok=True)
    result = subprocess.run(
        [
            str(bench),
            "micro",
            "durable-corpus",
            "--dir",
            str(target),
            "--source",
            str(corpus),
            "--read-source",
            "--variant",
            variant,
            "--reps",
            "1",
        ],
        capture_output=True,
        text=True,
        check=True,
    )
    line = next(
        row for row in result.stdout.splitlines() if row.startswith("micro name=")
    )
    total = float(re.search(r"total_ms=([0-9.]+)", line).group(1))
    return total, line


def rclone(
    binary: Path, corpus: Path, work: Path, arm: str, rep: int, clone: bool
) -> tuple[float, str]:
    destination = work / "rclone" / f"{arm}-{rep}"
    if destination.exists():
        raise SystemExit(f"refusing to reuse {destination}")
    command = [str(binary), "copy", str(corpus), str(destination), *RCLONE_FLAGS]
    if not clone:
        command.append("--local-no-clone")
    env = {k: v for k, v in os.environ.items() if not k.startswith("RCLONE_")}
    started = time.monotonic_ns()
    subprocess.run(command, env=env, stdin=subprocess.DEVNULL, check=True)
    elapsed = (time.monotonic_ns() - started) / 1e6
    # Outside timing: the copy must match the corpus shape, then it is removed.
    if regular_sizes(destination) != regular_sizes(corpus):
        raise SystemExit(f"rclone output differs from corpus: {destination}")
    shutil.rmtree(destination)
    return elapsed, f"dest={destination}"


def main(argv: list[str]) -> int:
    if len(argv) not in (5, 6):
        print(__doc__, file=sys.stderr)
        return 2
    bench, binary, corpus, work = (Path(value) for value in argv[1:5])
    reps = int(argv[5]) if len(argv) == 6 else 7
    if reps < 1:
        raise SystemExit("REPS must be at least 1")
    corpus = corpus.resolve(strict=True)
    for executable in (bench, binary):
        if not (executable.is_file() and os.access(executable, os.X_OK)):
            raise SystemExit(f"not an executable: {executable}")
    if work.exists():
        raise SystemExit(f"WORK_ROOT must not exist: {work}")
    ungated = os.environ.get("M0_ALLOW_UNGATED") == "1"
    load1, power, gated = conditions()
    if not gated and not ungated:
        raise SystemExit(
            f"refusing: power={power} load1={load1:.2f}; R-N81 needs AC power and "
            f"load1 < {LOAD_LIMIT} (M0_ALLOW_UNGATED=1 for a rehearsal)"
        )
    work.mkdir(mode=0o700)
    (work / "rclone").mkdir(mode=0o700)
    shape = regular_sizes(corpus)
    print(
        f"gate_a start={time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())} "
        f"corpus={corpus} files={len(shape)} bytes={sum(s for _, s in shape)} "
        f"work={work} reps={reps} arms={','.join(ARMS)} order=rotating-per-rep "
        f"floor_read_source=1 page_cache=not-dropped rclone_flags={' '.join(RCLONE_FLAGS)}",
        flush=True,
    )
    samples: dict[str, list[float]] = {arm: [] for arm in ARMS}
    all_gated = True
    for rep in range(reps):
        for step in range(len(ARMS)):
            arm = ARMS[(rep + step) % len(ARMS)]
            load1, power, gated = conditions()
            all_gated = all_gated and gated
            if arm == "floor-full":
                elapsed, detail = floor(bench, corpus, work, "full")
            elif arm == "floor-barrier":
                elapsed, detail = floor(bench, corpus, work, "barrier")
            else:
                elapsed, detail = rclone(
                    binary, corpus, work, arm, rep, arm == "rclone-shipped"
                )
            samples[arm].append(elapsed)
            print(
                f"gate_a_sample arm={arm} rep={rep} order={step} elapsed_ms={elapsed:.3f} "
                f"load1={load1:.2f} power={power} gated={int(gated)} {detail}",
                flush=True,
            )
    for arm in ARMS:
        values = samples[arm]
        print(
            f"gate_a_summary arm={arm} reps={len(values)} min_ms={min(values):.3f} "
            f"median_ms={statistics.median(values):.3f} max_ms={max(values):.3f}",
            flush=True,
        )
    floor_median = statistics.median(samples["floor-full"])
    rclone_median = statistics.median(samples["rclone-shipped"])
    print(
        f"gate_a_compare floor_full_median_ms={floor_median:.3f} "
        f"rclone_shipped_median_ms={rclone_median:.3f} "
        f"floor_below_rclone={int(floor_median < rclone_median)} "
        f"all_rows_gated={int(all_gated)}",
        flush=True,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
