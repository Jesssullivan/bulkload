#!/usr/bin/env python3
"""Local-to-local mover benchmark: native vs rsync vs rclone.

This measures the part of the transport that the mover can actually change.
It deliberately does NOT measure the wire: LANE F measured neo->sting ssh at
14.8-18.5 MiB/s, cipher-independent, with four parallel streams giving zero
gain, and called that 83% of the cold floor. Anything this harness printed
over a real link would be a measurement of that link. So both ends are local
directories, and what is left is per-file protocol cost, deduplication, and
parallelism.

Read the numbers with that fence in mind. A local-to-local result is an upper
bound on the mover's contribution and says nothing about a ceremony's wall
clock. The estate already fences this shape of claim twice:
`prompts-enqueue/prompts/47:50-51` — "APFS is a client substrate and
performance baseline. APFS-only evidence is not a TCFS benchmark result."

Usage:

    scripts/bench_mover.py --corpus /path/to/corpus --workdir /path/to/scratch
    scripts/bench_mover.py --generate --files 100000 --mean-bytes 13000 \\
        --duplicate-fraction 0.25 --workdir /path/to/scratch

Only `--mover native` needs this repo. `rsync` needs a GNU rsync >= 3.x on
PATH or at `--rsync-path`; the rclone rows are skipped when rclone is absent.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import random
import re
import shutil
import subprocess
import sys
import tempfile
import time


HERE = Path(__file__).resolve().parent
RUNTIME = HERE.parent / ".agents/skills/bulkload/scripts"
if str(RUNTIME) not in sys.path:
    sys.path.insert(0, str(RUNTIME))

from bulkload_lib import mover  # noqa: E402
from bulkload_lib.model import sha256_file  # noqa: E402


MIB = 1024 * 1024


class BenchError(RuntimeError):
    pass


def generate_corpus(
    root: Path,
    *,
    files: int,
    mean_bytes: int,
    duplicate_fraction: float,
    seed: int,
) -> None:
    """Write a synthetic corpus shaped like the review's measurement corpus.

    The review measured `/Users/jess/.claude-boundary/agent-notes-rescue`:
    268,406 files / 3.4 GiB / ~13 KB mean (VERDICTS.md, Lane A). The shape
    that matters is small files in deep-ish directories, with a real duplicate
    fraction — the ceremony's own plan charged 112.04 GB against 84.11 GB
    unique, i.e. 24.9% of the corpus was a duplicate of something else in it
    (VERDICTS.md, LANE F).
    """
    if root.exists():
        raise BenchError(f"corpus root already exists: {root}")
    rng = random.Random(seed)
    root.mkdir(parents=True)
    pool_size = max(1, int(files * duplicate_fraction))
    pool = [
        os.urandom(max(1, int(rng.gauss(mean_bytes, mean_bytes / 3))))
        for _ in range(min(pool_size, 512))
    ]
    written = 0
    for index in range(files):
        directory = root / f"{index % 97:02d}" / f"{(index // 97) % 89:02d}"
        directory.mkdir(parents=True, exist_ok=True)
        path = directory / f"f{index:07d}.bin"
        if pool and rng.random() < duplicate_fraction:
            payload = pool[rng.randrange(len(pool))]
        else:
            payload = os.urandom(max(1, int(rng.gauss(mean_bytes, mean_bytes / 3))))
        path.write_bytes(payload)
        written += len(payload)
    print(f"corpus: {files} files / {written / MIB:.1f} MiB at {root}", flush=True)


def census(root: Path) -> tuple[int, int, int]:
    """Return (files, bytes, unique_bytes) for ``root``."""
    files = 0
    total = 0
    unique: dict[str, int] = {}
    for directory, _, names in os.walk(root):
        for name in names:
            path = Path(directory) / name
            info = path.lstat()
            if not path.is_file() or path.is_symlink():
                continue
            files += 1
            total += info.st_size
            unique.setdefault(sha256_file(path), info.st_size)
    return files, total, sum(unique.values())


def relatives_of(root: Path) -> list[str]:
    items = []
    for directory, _, names in os.walk(root):
        for name in names:
            items.append((Path(directory) / name).relative_to("/").as_posix())
    return sorted(items)


def run_native(
    corpus: Path, destination: Path, *, streams: int, python: str
) -> dict[str, object]:
    destination.mkdir(parents=True, exist_ok=True)
    module = Path(mover.__file__)
    with tempfile.TemporaryDirectory(prefix="bench-mover-spill-") as spill:
        started = time.monotonic()
        summary = mover.push(
            source_root=Path("/"),
            objects=mover.plan_objects(
                Path("/"), relatives_of(corpus), spill_dir=Path(spill)
            ),
            channel_factory=mover.local_channel_factory(
                python, str(module), os.fspath(destination)
            ),
            streams=streams,
            expected_receiver_sha256=hashlib.sha256(module.read_bytes()).hexdigest(),
        )
        seconds = time.monotonic() - started
    return {
        "seconds": seconds,
        "paths": summary.paths,
        "objects": summary.objects,
        "bytes_on_wire": summary.bytes_sent,
        "bytes_deduplicated": summary.bytes_deduplicated,
        "detail": f"{streams} streams",
    }


def rsync_protocol(rsync_path: str) -> int:
    """The engine binds one GNU rsync at protocol >= 30 (`scanner.py`,
    `inspect_rsync`). macOS ships openrsync at protocol 29, which does not
    understand `--ignore-missing-args`, so refuse it here with the same
    reason rather than reporting a mystery exit code."""
    result = subprocess.run(
        [rsync_path, "--version"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    match = re.search(rb"protocol version (\d+)", result.stdout)
    return int(match.group(1)) if match else 0


def run_rsync(
    corpus: Path, destination: Path, *, rsync_path: str, extra: list[str]
) -> dict[str, object]:
    protocol = rsync_protocol(rsync_path)
    if protocol < 30:
        raise BenchError(
            f"{rsync_path} speaks protocol {protocol}; the engine binds GNU "
            "rsync >= 30. Pass --rsync-path."
        )
    destination.mkdir(parents=True, exist_ok=True)
    allowlist = b"".join(os.fsencode(item) + b"\0" for item in relatives_of(corpus))
    started = time.monotonic()
    result = subprocess.run(
        [
            rsync_path,
            "-a",
            "--from0",
            "--files-from=-",
            "--delay-updates",
            "--ignore-missing-args",
            "--no-devices",
            "--no-specials",
            *extra,
            "/",
            f"{destination}/",
        ],
        input=allowlist,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    seconds = time.monotonic() - started
    if result.returncode != 0:
        raise BenchError(
            f"rsync failed ({result.returncode}): "
            f"{result.stderr.decode('utf-8', 'replace')[:400]}"
        )
    return {"seconds": seconds, "detail": " ".join(extra) or "shipped flags"}


def run_rclone(
    corpus: Path,
    destination: Path | None,
    *,
    rclone_path: str,
    subcommand: list[str],
    detail: str,
) -> dict[str, object]:
    if destination is not None:
        destination.mkdir(parents=True, exist_ok=True)
    argv = [rclone_path, *subcommand]
    started = time.monotonic()
    result = subprocess.run(
        argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False
    )
    seconds = time.monotonic() - started
    if result.returncode != 0:
        raise BenchError(
            f"rclone failed ({result.returncode}): "
            f"{result.stderr.decode('utf-8', 'replace')[:400]}"
        )
    return {"seconds": seconds, "detail": detail}


def run_engine_hash(corpus: Path) -> dict[str, object]:
    """The engine's own primitive, in its single-threaded walk shape.

    This is the 4,523 files/s / 44 MiB/s row the review measured; it is here so
    every other row on this machine is comparable to a number the review
    already published.
    """
    started = time.monotonic()
    count = 0
    for directory, _, names in os.walk(corpus):
        for name in names:
            sha256_file(Path(directory) / name)
            count += 1
    return {"seconds": time.monotonic() - started, "detail": "1 thread"}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="bench_mover", description=__doc__.split("\n\n")[0]
    )
    parser.add_argument("--corpus", help="Existing corpus to move (read-only).")
    parser.add_argument(
        "--generate", action="store_true", help="Synthesize a corpus first."
    )
    parser.add_argument("--files", type=int, default=100_000)
    parser.add_argument("--mean-bytes", type=int, default=13_000)
    parser.add_argument("--duplicate-fraction", type=float, default=0.25)
    parser.add_argument("--seed", type=int, default=20260829)
    parser.add_argument("--workdir", required=True, help="Scratch for destinations.")
    parser.add_argument("--streams", type=int, default=mover.DEFAULT_STREAMS)
    parser.add_argument("--rsync-path", default=shutil.which("rsync"))
    parser.add_argument("--rclone-path", default=shutil.which("rclone"))
    parser.add_argument("--python", default=sys.executable)
    parser.add_argument("--transfers", type=int, default=16)
    parser.add_argument("--checkers", type=int, default=16)
    parser.add_argument("--keep", action="store_true", help="Keep destinations.")
    parser.add_argument("--json", help="Write the result rows here.")
    arguments = parser.parse_args(argv)

    workdir = Path(arguments.workdir).resolve()
    workdir.mkdir(parents=True, exist_ok=True)
    if arguments.generate:
        corpus = Path(arguments.corpus or workdir / "corpus").resolve()
        generate_corpus(
            corpus,
            files=arguments.files,
            mean_bytes=arguments.mean_bytes,
            duplicate_fraction=arguments.duplicate_fraction,
            seed=arguments.seed,
        )
    elif arguments.corpus:
        corpus = Path(arguments.corpus).resolve()
    else:
        parser.error("pass --corpus, --generate, or both")

    print("census...", flush=True)
    files, total, unique = census(corpus)
    print(
        f"corpus: {files} files / {total / MIB:.1f} MiB "
        f"/ {unique / MIB:.1f} MiB unique "
        f"({100 * (total - unique) / max(total, 1):.1f}% duplicate)",
        flush=True,
    )

    rows: list[dict[str, object]] = []

    def record(name: str, result: dict[str, object]) -> None:
        seconds = float(result["seconds"])
        row = {
            "mover": name,
            "seconds": round(seconds, 2),
            "files_per_second": round(files / seconds, 1) if seconds else None,
            "mib_per_second": round(total / MIB / seconds, 1) if seconds else None,
            **{key: value for key, value in result.items() if key != "seconds"},
        }
        rows.append(row)
        print(
            f"{name:<28} {seconds:8.2f}s  "
            f"{row['files_per_second']:>10} files/s  "
            f"{row['mib_per_second']:>8} MiB/s  {result.get('detail', '')}",
            flush=True,
        )

    destinations: list[Path] = []

    def destination(name: str) -> Path:
        path = workdir / f"dest-{name}"
        shutil.rmtree(path, ignore_errors=True)
        destinations.append(path)
        return path

    try:
        record("engine-sha256-walk", run_engine_hash(corpus))
        native_destination = destination("native")
        # Two passes, always. The review's own conclusion is that the repeat
        # run is the product: "Incremental capture on the
        # (dev,ino,size,mtime_ns,ctime_ns) key ... is the difference between
        # an 8-hour ceremony and a 5-minute one" (VERDICTS.md, LANE F). A cold
        # number that hides what a resume costs is the number that let a
        # 92-hour ledger accumulate 30 FAILED lines and 0 completions.
        for repeat in (False, True):
            record(
                f"native ({arguments.streams} streams)"
                + (" [repeat]" if repeat else ""),
                run_native(
                    corpus,
                    native_destination,
                    streams=arguments.streams,
                    python=arguments.python,
                ),
            )
        if arguments.rsync_path:
            rsync_destination = destination("rsync")
            for repeat in (False, True):
                record(
                    "rsync (shipped flags)" + (" [repeat]" if repeat else ""),
                    run_rsync(
                        corpus,
                        rsync_destination,
                        rsync_path=arguments.rsync_path,
                        extra=[],
                    ),
                )
        else:
            print("rsync: not found, skipped", flush=True)
        if arguments.rclone_path:
            target = destination("rclone")
            for repeat in (False, True):
                record(
                    f"rclone sync --transfers {arguments.transfers}"
                    + (" [repeat]" if repeat else ""),
                    run_rclone(
                        corpus,
                        target,
                        rclone_path=arguments.rclone_path,
                        subcommand=[
                            "sync",
                            "--transfers",
                            str(arguments.transfers),
                            "--checkers",
                            str(arguments.checkers),
                            os.fspath(corpus),
                            os.fspath(target),
                        ],
                        detail=f"--transfers {arguments.transfers}",
                    ),
                )
            record(
                f"rclone check --checksum --checkers {arguments.checkers}",
                run_rclone(
                    corpus,
                    None,
                    rclone_path=arguments.rclone_path,
                    subcommand=[
                        "check",
                        "--checksum",
                        "--checkers",
                        str(arguments.checkers),
                        os.fspath(corpus),
                        os.fspath(target),
                    ],
                    detail="verify both sides",
                ),
            )
        else:
            print("rclone: not found, skipped", flush=True)
    finally:
        if not arguments.keep:
            for path in destinations:
                shutil.rmtree(path, ignore_errors=True)

    summary = {
        "corpus": os.fspath(corpus),
        "files": files,
        "bytes": total,
        "unique_bytes": unique,
        "rows": rows,
    }
    if arguments.json:
        Path(arguments.json).write_text(
            json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
