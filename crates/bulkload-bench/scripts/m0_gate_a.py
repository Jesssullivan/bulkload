#!/usr/bin/env python3
"""Gate (a) measurement: corpus-shaped durable floor interleaved with rclone.

M2 W2 lands this script; M2 W3 takes the gated measurement (R-N62, R-N81,
R-N87, TIN-4541). One process runs every arm.

Arms:
  floor-full      bulkload-bench micro durable-corpus --variant full   (jobs=1)
  floor-barrier   bulkload-bench micro durable-corpus --variant barrier (jobs=1)
  floor-full-j4   the same as floor-full with --jobs 4; only with --floor-jobs4
  rclone-shipped  rclone copy with the R23 bench flags (clones on APFS)
  rclone-noclone  the same plus --local-no-clone

Order: first, `--warmup` repetitions (default 1) run every arm once. Their
rows say warmup=1 and they are left out of every summary. Each measured
repetition then runs every arm once, in the order of one row of a Williams
design: an n x n square for an even arm count, the 2n-row design for an
odd one. The balance is within-row Williams only: inside each row every
ordered pair of adjacent arms occurs equally often over the design, and
every arm occupies every position equally often. The pairs across row
boundaries (the last arm of one row followed by the first arm of the next,
and warm-up into rep 0) are unbalanced. W3 options, if that matters: a
serially balanced sequence, or a washout arm between rows. Every row logs
its sequence. The default repetition count is the smallest multiple of the
design length that is at least 7: 8 for four arms, 10 for five.
balanced=1 in the header means the count is a multiple of the design length,
which is within-row balance only. Fewer than 7 repetitions is refused unless
the override is set; min_reps_met=0|1 is on the header either way.

Every floor sample reads the source (--read-source) and writes one file per
corpus file at its real size. Each file gets its own flush, and the created
directories and the parent get the same kind. The page cache is never
dropped. Before every arm, rclone included, the script logs source
page-cache residency (`bulkload-bench micro residency`) outside timing.

Gating (R-N81): a measured row is gated only if all three hold:
  - the host is on AC power with load1 < 2.5 just before the arm;
  - the host is still on AC power just after the arm;
  - the override is off.
Load1 is checked before each arm only. Power is re-sampled after each arm
and once more after the last arm, before the compare line; the compare line
prints that final power as final_power. Warm-up rows always print gated=0.
Without the override the script refuses to start on a host that fails the
before-arm check. With M0_ALLOW_UNGATED=1 (exactly "1") it runs anyway for a
rehearsal. Then every line carries ungated_override=1, every row says
gated=0, and the compare line says all_rows_gated=0, whatever the host
conditions were.

Statistics: summary and compare lines carry gated_reps. When an arm has any
gated rows, its min/median/max are over the gated rows only (stats_over=
gated). Otherwise they are over all measured rows (stats_over=all), which
can only be a rehearsal or a failed run.

Timing asymmetry (on the compare line as well):
  floor  = the benchmark's in-process total_ms: from creating the destination
           to the last directory flush, source reads included. It excludes
           process start, any directory walk of the source, hashing and the
           untimed drain flush. Writers: jobs=1, or 4 for floor-full-j4.
  rclone = wall time of the whole rclone process: start, source walk,
           --transfers 4 copy, MD5 checks of both sides.

Usage:
  m0_gate_a.py BENCH RCLONE CORPUS WORK_ROOT [REPS] [--warmup N] [--floor-jobs4]

WORK_ROOT must not exist. Its parent must be on the volume under test. Output
is key=value lines on stdout, ending with min/median/max per arm.
"""

from __future__ import annotations

import argparse
import math
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
BASE_ARMS = ["floor-full", "floor-barrier", "rclone-shipped", "rclone-noclone"]
JOBS4_ARM = "floor-full-j4"
LOAD_LIMIT = 2.5
MIN_REPS = 7
ASYMMETRY = (
    "floor=in-process-total_ms(read+write+per-file-flush+dir-flush;"
    "no-process-start,no-walk,no-hash);"
    "rclone=whole-process-wall(start+walk+copy+md5;transfers=4)"
)


def williams(count: int) -> list[list[int]]:
    """Williams design rows over `count` arms.

    Within-row balance only: across the design, each ordered pair of adjacent
    arms inside a row occurs equally often, and each arm occupies each
    position equally often. Pairs across row boundaries are not balanced.
    """
    first = [0]
    low, high = 1, count - 1
    take_low = True
    while len(first) < count:
        if take_low:
            first.append(low)
            low += 1
        else:
            first.append(high)
            high -= 1
        take_low = not take_low
    rows = [[(arm + shift) % count for arm in first] for shift in range(count)]
    if count % 2 == 1:
        rows += [list(reversed(row)) for row in rows]
    return rows


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
    """Return load1, power source, and whether the host meets R-N81."""
    load1 = os.getloadavg()[0]
    power = power_source()
    return load1, power, power == "ac" and load1 < LOAD_LIMIT


def regular_sizes(root: Path) -> list[tuple[str, int]]:
    rows = []
    for path in sorted(root.rglob("*")):
        if path.is_file() and not path.is_symlink():
            rows.append((str(path.relative_to(root)), path.stat().st_size))
    return rows


def field(line: str, key: str) -> str:
    match = re.search(rf"\b{key}=(\S+)", line)
    if match is None:
        raise SystemExit(f"missing {key}= in: {line}")
    return match.group(1)


def residency(bench: Path, corpus: Path) -> str:
    result = subprocess.run(
        [str(bench), "micro", "residency", "--source", str(corpus)],
        capture_output=True,
        text=True,
        check=True,
    )
    line = next(
        row for row in result.stdout.splitlines() if row.startswith("micro name=")
    )
    return field(line, "source_resident_fraction")


def floor(
    bench: Path, corpus: Path, work: Path, variant: str, jobs: int
) -> tuple[float, str]:
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
            "--jobs",
            str(jobs),
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
    return float(field(line, "total_ms")), f"floor_line=[{line}]"


def rclone(
    binary: Path, corpus: Path, work: Path, name: str, clone: bool
) -> tuple[float, str]:
    destination = work / "rclone" / name
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


def run_arm(
    arm: str, bench: Path, binary: Path, corpus: Path, work: Path, tag: str
) -> tuple[float, int, str]:
    """Run one arm; return elapsed ms, writer/transfer jobs, and detail."""
    if arm == "floor-full":
        elapsed, detail = floor(bench, corpus, work, "full", 1)
        return elapsed, 1, detail
    if arm == "floor-barrier":
        elapsed, detail = floor(bench, corpus, work, "barrier", 1)
        return elapsed, 1, detail
    if arm == JOBS4_ARM:
        elapsed, detail = floor(bench, corpus, work, "full", 4)
        return elapsed, 4, detail
    elapsed, detail = rclone(
        binary, corpus, work, f"{arm}-{tag}", arm == "rclone-shipped"
    )
    return elapsed, 4, detail


def parse(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("bench", type=Path)
    parser.add_argument("rclone", type=Path)
    parser.add_argument("corpus", type=Path)
    parser.add_argument("work", type=Path)
    parser.add_argument("reps", type=int, nargs="?")
    parser.add_argument("--warmup", type=int, default=1)
    parser.add_argument("--floor-jobs4", action="store_true")
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse(argv)
    arms = BASE_ARMS + ([JOBS4_ARM] if args.floor_jobs4 else [])
    design = williams(len(arms))
    reps = (
        args.reps
        if args.reps is not None
        else len(design) * math.ceil(MIN_REPS / len(design))
    )
    if reps < 1 or args.warmup < 0:
        raise SystemExit("REPS must be at least 1 and --warmup at least 0")
    min_reps_met = reps >= MIN_REPS
    corpus = args.corpus.resolve(strict=True)
    for executable in (args.bench, args.rclone):
        if not (executable.is_file() and os.access(executable, os.X_OK)):
            raise SystemExit(f"not an executable: {executable}")
    work = args.work
    if work.exists():
        raise SystemExit(f"WORK_ROOT must not exist: {work}")
    ungated = os.environ.get("M0_ALLOW_UNGATED") == "1"
    override = int(ungated)
    load1, power, host_ok = conditions()
    preflight_gated = host_ok and not ungated
    if not min_reps_met and not ungated:
        raise SystemExit(
            f"refusing: REPS={reps} < {MIN_REPS} (M0_ALLOW_UNGATED=1 for a rehearsal)"
        )
    if not host_ok and not ungated:
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
        f"work={work} reps={reps} warmup={args.warmup} arms={','.join(arms)} "
        f"order=williams design_rows={len(design)} "
        f"balanced={int(reps % len(design) == 0)} balance=within-row-only "
        f"min_reps_met={int(min_reps_met)} floor_read_source=1 "
        f"page_cache=not-dropped preflight_load1={load1:.2f} "
        f"preflight_power={power} preflight_gated={int(preflight_gated)} "
        f"ungated_override={override} rclone_flags={','.join(RCLONE_FLAGS)}",
        flush=True,
    )
    samples: dict[str, list[tuple[float, bool]]] = {arm: [] for arm in arms}
    all_gated = preflight_gated
    schedule = [(True, w, design[0]) for w in range(args.warmup)] + [
        (False, r, design[r % len(design)]) for r in range(reps)
    ]
    for warmup, rep, row in schedule:
        sequence = [arms[index] for index in row]
        tag = f"warmup{rep}" if warmup else f"rep{rep}"
        for step, arm in enumerate(sequence):
            load1, power, before_ok = conditions()
            resident = residency(args.bench, corpus)
            elapsed, jobs, detail = run_arm(
                arm, args.bench, args.rclone, corpus, work, tag
            )
            # Load1 is a before-arm check only; power is re-sampled after.
            power_after = conditions()[1]
            after_power_ok = power_after == "ac"
            gated = before_ok and after_power_ok and not ungated and not warmup
            if not warmup:
                all_gated = all_gated and gated
                samples[arm].append((elapsed, gated))
            print(
                f"gate_a_sample arm={arm} warmup={int(warmup)} rep={rep} "
                f"order={step} sequence={','.join(sequence)} jobs={jobs} "
                f"elapsed_ms={elapsed:.3f} source_resident_fraction_before={resident} "
                f"load1={load1:.2f} power={power} power_after={power_after} "
                f"gated={int(gated)} ungated_override={override} {detail}",
                flush=True,
            )
    final_power = conditions()[1]
    all_gated = all_gated and final_power == "ac" and not ungated
    chosen: dict[str, tuple[list[float], str, int]] = {}
    for arm in arms:
        gated_values = [value for value, ok in samples[arm] if ok]
        if gated_values:
            chosen[arm] = (gated_values, "gated", len(gated_values))
        else:
            chosen[arm] = ([value for value, _ in samples[arm]], "all", 0)
        values, over, gated_reps = chosen[arm]
        print(
            f"gate_a_summary arm={arm} reps={len(samples[arm])} "
            f"gated_reps={gated_reps} stats_over={over} "
            f"jobs={4 if arm == JOBS4_ARM or arm.startswith('rclone') else 1} "
            f"min_ms={min(values):.3f} median_ms={statistics.median(values):.3f} "
            f"max_ms={max(values):.3f} warmup_excluded={args.warmup} "
            f"ungated_override={override}",
            flush=True,
        )
    floor_values, floor_over, floor_gated = chosen["floor-full"]
    rclone_values, rclone_over, rclone_gated = chosen["rclone-shipped"]
    floor_median = statistics.median(floor_values)
    rclone_median = statistics.median(rclone_values)
    print(
        f"gate_a_compare floor_full_median_ms={floor_median:.3f} "
        f"rclone_shipped_median_ms={rclone_median:.3f} "
        f"floor_below_rclone={int(floor_median < rclone_median)} "
        f"floor_gated_reps={floor_gated} rclone_gated_reps={rclone_gated} "
        f"stats_over={floor_over},{rclone_over} final_power={final_power} "
        f"all_rows_gated={int(all_gated)} ungated_override={override} "
        f"caveat=asymmetric-timing:{ASYMMETRY}",
        flush=True,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
