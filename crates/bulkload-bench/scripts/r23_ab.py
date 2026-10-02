#!/usr/bin/env python3
"""Gate (a) / R23 A/B harness for #88 (OI-1002-Q27, R23, R-N57, R-N81, R-N91).

Runs `bulkload-bench` built at two revisions in the order A/B/A/B/A, one
bench invocation per repetition. Each invocation is the full R23 bench as in
docs/evidence/r23-2026-09-18.md: native and rclone alternate N/R/N/R/N
(`--reps 3`), then warm resume, interrupted resume and the 1 % delta, so the
rclone baseline runs the same way, with the same flags, inside every
repetition. One extra native-only repetition of the v4 engine (41bf9a4 by
default) is run last, as the reference for the dedup-loss measurement.

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
    (R-N91). It is recorded, not checked.
  - The corpus is R23 corpus v1 (r23_corpus.py, OI-1002-Q28): 23 regular
    files, 239,819,837 bytes, and `r23_corpus.py verify` matches the
    committed manifest (content identity f4a7619f...). With other
    --expect-files/--expect-bytes only the shape is checked.
  - The work root does not exist yet. Its parent should be on the volume
    under test (TinylandState for gate a).
  - After the builds, the harness waits up to --settle-seconds for load1 to
    fall below 2.5 on AC power. It checks again before every repetition and
    after every repetition (power, and every bench row gated=true). The
    bench itself checks before every arm. A failed check ends the sample:
    the evidence draft is written with status=aborted and the exit is 3.

Page cache: the cache is never dropped, on either host. The bench verifies
the source with a full BLAKE3 walk before every arm, so every timed arm,
native and rclone alike, starts with the source hot. That is the one
consistent state reachable without root (`purge` needs root on Darwin, and
a drop would be undone by that verification read). Source residency is
recorded before every repetition (`bulkload-bench micro residency`).
Destinations are always fresh: every repetition gets a new work root, and
the bench makes a new destination per arm.

--dry-run makes a tiny synthetic corpus, passes --informational and skips
the platform, shape, quiet and load checks. Its output says
NOT A GATE SAMPLE everywhere and goes to the work root, never docs/evidence.

Exit: 0 complete (the R23 verdict is in the evidence, pass or fail),
2 refused before the sample, 3 aborted during the sample, 4 build failure.
"""

from __future__ import annotations

import argparse
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
RULINGS = "OI-1002-Q27, R23, R-N57, R-N81, R-N91, R-N134, R-N13"
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


def build(repo: Path, rev: str, build_root: Path, jobs: int) -> dict[str, str]:
    """Build bulkload-bench at `rev` in its own tree and target dir."""
    sha = git(repo, "rev-parse", "--verify", f"{rev}^{{commit}}")
    short = sha[:12]
    binary = build_root / "bin" / f"bulkload-bench-{short}"
    if not binary.is_file():
        source = build_root / f"src-{short}"
        if source.exists():
            shutil.rmtree(source)
        source.mkdir(parents=True)
        archive = build_root / f"src-{short}.tar"
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
    return {"rev": rev, "sha": sha, "binary": str(binary), "sha256": sha256(binary)}


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
        "verdict": parsed.get("verdict", {}),
    }


def run_rep(
    label: str,
    info: dict[str, str],
    args: argparse.Namespace,
    work: Path,
    corpus: Path,
    rclone: Path,
    index: int,
    native_only: bool,
) -> dict[str, object]:
    root = work / "reps" / f"rep{index}-{label}-{info['sha'][:12]}"
    logs = work / "logs"
    before = conditions()
    if not args.dry_run and not before["ok"]:
        raise Abort(f"rep{index} {label} precondition failed: {before}")
    cache = residency(Path(info["binary"]), corpus)
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
    if args.dry_run:
        command.append("--informational")
    say(
        f"rep={index} label={label} sha={info['sha'][:12]} load1={before['load1']} power={before['power']} residency={cache}"
    )
    started = time.monotonic()
    result = subprocess.run(command, capture_output=True, text=True, check=False)
    wall_s = time.monotonic() - started
    (logs / f"rep{index}-{label}.stdout").write_text(result.stdout)
    (logs / f"rep{index}-{label}.stderr").write_text(result.stderr)
    after = conditions()
    parsed = parse_bench(result.stdout)
    if "verdict" not in parsed:
        raise Abort(
            f"rep{index} {label} bench refused (exit {result.returncode}): "
            f"{result.stderr.strip().splitlines()[-1:]}"
        )
    summary = summarize(parsed)
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
        "source_resident_fraction": cache,
        "summary": summary,
        "parsed": parsed,
    }
    if not args.dry_run and (after["power"] != "ac" or not summary["all_gated"]):
        rep["aborted"] = True
        raise Abort(
            f"rep{index} {label} post-check failed: power={after['power']} "
            f"all_gated={summary['all_gated']}",
            rep,
        )
    return rep


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
    lines = []
    title = f"# R23 gate (a) A/B sample for #88 - {report['date']}"
    lines.append(title + (f" ({NOT_GATE})" if dry else " (DRAFT)"))
    lines.append("")
    if dry:
        lines += [
            f"> **{NOT_GATE}.** Synthetic corpus, `--informational`, no host gating.",
            "",
        ]
    lines += [
        f"Status: **{report['status']}**"
        + (f" - {report['reason']}" if report.get("reason") else ""),
        "",
        f"Rulings: {RULINGS}. Harness: `crates/bulkload-bench/scripts/r23_ab.py`.",
        "",
        "## Identity",
        "",
        f"- Host: `{report['host']}` ({report['platform']}); mode `{report['mode']}`;"
        f" coordinator-quiet acknowledged: `{report['coordinator_quiet']}` (R-N91).",
        f"- Corpus: `{report['corpus']}`, {fmt(report['corpus_files'])} regular files,"
        f" {fmt(report['corpus_bytes'])} bytes; sealed identity"
        f" `{report.get('sealed_identity', 'n/a')}`.",
        f"- Work root: `{report['work_root']}`.",
        f"- rclone: `{report['rclone']}` ({report.get('rclone_version', 'n/a')}), the r23-2026-09-18 flags.",
        "- Page cache: never dropped. The bench reads the whole source (BLAKE3) before"
        " every arm, so every timed arm starts source-hot; residency is logged per rep.",
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
        "## Repetitions",
        "",
        "| # | label | load1 before/after | power | residency | native initial median ms |"
        " rclone initial median ms | flush_barrier_ns | flush_full_ns | stall share | files/s |"
        " bytes received | gated |",
        "|---:|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---|",
    ]
    for rep in report["reps"]:
        s = rep["summary"]
        lines.append(
            f"| {rep['index']} | {rep['label']} | {rep['conditions_before']['load1']}/"
            f"{rep['conditions_after']['load1']} | {rep['conditions_before']['power']} |"
            f" {fmt(rep['source_resident_fraction'])} | {fmt(s['native_initial_median_ms'])} |"
            f" {fmt(s['rclone_initial_median_ms'])} | {fmt(s['flush_barrier_ns_total'])} |"
            f" {fmt(s['flush_full_ns_total'])} | {fmt(s['receive_stall_share_of_wall'], 4)} |"
            f" {fmt(s['files_per_s_median'], 1)} | {fmt(s['bytes_received_median'], 0)} |"
            f" {s['all_gated']} |"
        )
    lines += [
        "",
        "## Medians per revision",
        "",
        "| metric | A | B |",
        "|---|---:|---:|",
    ]
    medians = report["per_revision"]
    for key in medians.get("A", {}):
        lines.append(
            f"| {key} | {fmt(medians['A'][key])} | {fmt(medians.get('B', {}).get(key))} |"
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
        label: per_revision(reps, label) for label in ("A", "B") if label in builds
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
    (work / "r23-ab.json").write_text(json.dumps(report, indent=2, default=str))
    evidence_path.parent.mkdir(parents=True, exist_ok=True)
    evidence_path.write_text(evidence(report))
    say(
        f"status={report['status']} json={work / 'r23-ab.json'} evidence={evidence_path}"
    )


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
    parser.add_argument("--pattern", default="ABABA")
    parser.add_argument("--build-root", help="default: <repo>/target/r23-ab")
    parser.add_argument("--build-jobs", type=int, default=4)
    parser.add_argument("--settle-seconds", type=int, default=900)
    parser.add_argument("--expect-files", type=int, default=RECORD_FILES)
    parser.add_argument("--expect-bytes", type=int, default=RECORD_BYTES)
    parser.add_argument(
        "--evidence", help="default: docs/evidence/r23-<date>.md (dry run: work root)"
    )
    parser.add_argument(
        "--coordinator-quiet",
        action="store_true",
        help="other lanes are held quiet (R-N91); required in gated mode",
    )
    parser.add_argument("--dry-run", action="store_true", help=NOT_GATE)
    args = parser.parse_args(argv)

    repo = Path(args.repo).resolve()
    work = Path(args.work_root).resolve()
    date = dt.datetime.now(dt.UTC).strftime("%Y-%m-%d")
    if work.exists() or not work.parent.is_dir():
        say(f"refused: work root must be new under an existing parent: {work}")
        return 2
    if not set(args.pattern) <= {"A", "B"} or not args.pattern:
        say("refused: --pattern uses only A and B")
        return 2
    if not args.dry_run:
        if platform.system() != "Darwin":
            say(
                "refused: gated samples need Darwin (the bench R-N81 preflight reads pmset); run on neo"
            )
            return 2
        if not args.coordinator_quiet:
            say("refused: --coordinator-quiet is required (R-N91)")
            return 2
        if not args.corpus:
            say("refused: --corpus is required")
            return 2
        files, size = corpus_shape(Path(args.corpus))
        if (files, size) != (args.expect_files, args.expect_bytes):
            say(
                f"refused: corpus shape {files} files/{size} bytes, expected "
                f"{args.expect_files}/{args.expect_bytes}"
            )
            return 2
        if (args.expect_files, args.expect_bytes) == (RECORD_FILES, RECORD_BYTES):
            if corpus_verify(Path(args.corpus)) != 0:
                say("refused: corpus does not match r23_corpus.manifest.tsv")
                return 2
    evidence_path = (
        Path(args.evidence)
        if args.evidence
        else (
            work / f"r23-dryrun-{date}.md"
            if args.dry_run
            else repo / "docs" / "evidence" / f"r23-{date}.md"
        )
    )
    if evidence_path.exists():
        say(f"refused: evidence file exists: {evidence_path}")
        return 2

    work.mkdir(mode=0o700)
    (work / "logs").mkdir()
    (work / "reps").mkdir()
    if args.dry_run:
        corpus = work / "corpus"
        synthetic_corpus(corpus)
    else:
        corpus = Path(args.corpus).resolve()
    files, size = corpus_shape(corpus)
    build_root = (
        Path(args.build_root) if args.build_root else repo / "target" / "r23-ab"
    )
    build_root.mkdir(parents=True, exist_ok=True)
    builds = {
        "A": build(repo, args.rev_a, build_root, args.build_jobs),
        "B": build(repo, args.rev_b, build_root, args.build_jobs),
    }
    if args.rev_v4:
        builds["V4"] = build(repo, args.rev_v4, build_root, args.build_jobs)
    rclone = resolve_rclone(repo, args.rclone)
    report: dict[str, object] = {
        "date": date,
        "mode": "dry-run" if args.dry_run else "gated",
        "host": platform.node(),
        "platform": platform.platform(),
        "coordinator_quiet": args.coordinator_quiet,
        "corpus": str(corpus),
        "corpus_files": files,
        "corpus_bytes": size,
        "work_root": str(work),
        "rclone": str(rclone),
        "builds": builds,
        "pattern": args.pattern,
        "load_limit": LOAD_LIMIT,
        "rulings": RULINGS,
        "reps": [],
        "status": "running",
    }
    if not args.dry_run:
        deadline = time.monotonic() + args.settle_seconds
        while not (now := conditions())["ok"]:
            if time.monotonic() > deadline:
                report["status"], report["reason"] = (
                    "refused",
                    f"host never settled: {now}",
                )
                finish(report, work, evidence_path)
                return 2
            time.sleep(15)
    order = [(label, False) for label in args.pattern]
    if args.rev_v4:
        order.append(("V4", True))
    try:
        for index, (label, native_only) in enumerate(order):
            report["reps"].append(
                run_rep(
                    label, builds[label], args, work, corpus, rclone, index, native_only
                )
            )
    except Abort as reason:
        if reason.rep is not None:
            report["reps"].append(reason.rep)
        report["status"], report["reason"] = "aborted", str(reason)
        finish(report, work, evidence_path)
        return 3
    report["status"] = (
        "dry-run-complete-not-a-gate-sample" if args.dry_run else "complete-draft"
    )
    finish(report, work, evidence_path)
    return 0


if __name__ == "__main__":
    sys.exit(main())
