#!/usr/bin/env python3
"""S2 measured-budget instrument (OI-1003-Q34; S2 per OI-1003-Q5 and Q9).

S2 in docs/slo.md: with bulkload running, a reference agent workload on the
source shows at most +25 % p95 latency, and bulkload adds at most +2.0 to
load1. This script makes that budget measurable. It is stdlib only.

Reference workload v0 (OI-1003-Q34). It runs in a sibling directory on the
source's device, never inside the corpus: `run` refuses a work directory on
another device, inside the source, or already existing. One step runs at a fixed
1 Hz cadence and times each of its four operations on its own:
  - jsonl:  append one line to a JSONL file, then fsync it;
  - sqlite: one WAL-mode transaction (synchronous=FULL, so it is durable);
  - git:    `git status` plus `git diff` in a small fixture repository;
  - search: `rg` over a small fixture tree, or `grep -r` when rg is absent
            (the tool used is recorded).
`step` is the sum of the four: the latency of one agent step. A step that
overruns its second skips the ticks it missed, never runs late. Each missed
tick is recorded with its due time and how long after it the loop was free
(`missed_ticks`), and the verdict back-fills it into every latency metric
with that wait (coordinated omission: otherwise the slowest stretches give
the fewest samples and p95 reads low). Missed ticks are reported per window
and per state; sample floors count measured samples only.
Every git child runs without any inherited GIT_* variable and without the
global or system configuration (`git_env`), so a GIT_DIR or GIT_INDEX_FILE
exported by a hook, `rebase -x` or `bisect run` cannot point the fixture
build or the workload at a repository outside the work directory.

Sampler: load1 at 1 Hz, from /proc/loadavg on Linux and `sysctl -n
vm.loadavg` on Darwin (os.getloadavg elsewhere; the source is recorded).

Protocol: windows alternate OFF and ON, OFF/ON/OFF/ON/OFF by default, each
--window-seconds long (300 by default, the OI-1003-Q34 minimum; shorter only
for tests and smoke runs, which are never evidence). An ON window runs the ON
command, the bulkload command after `--`, at the --priority class, again and
again until the window's deadline (--no-repeat runs it once). The command
should pass the class to bulkload (`--priority={priority}`); every bulkload
`counters` line reports `priority=`, and the class it reports is recorded per
run and checked. ON-command placeholders: {priority}, {run} (1-based run
number) and {run_dir} (an empty directory made for that run). A command
that needs shell quoting goes in a script file, because `just` re-joins
arguments. Power is recorded at every window boundary (R-N81).

Verdict. The first --settle-seconds of every window are left out of both
metrics (the switch transient). load1 is the kernel's one-minute EWMA,
updated every 5 s, so it lags each switch; a plain mean of the settled rows
read a steady add X as about 0.856 X at 300 s windows and a 60 s settle
(#165). Each window's load1 is therefore its lag-corrected level
(`load1_level`): the mean of its settled rows plus tau times their slope,
which inverts the EWMA whatever the settle (within about 0.5 %). The trace's
verdict records the model and what a unit add reads as, plain and corrected
(`load1_model`). Over the settled rows, pooled per state:
  d_p95   = p95(ON) / p95(OFF) - 1 of the gate metric: `step` by default,
            or with --gate-metric each-op the worst of the four operations;
  d_load1 = level(ON) - level(OFF), each window weighted by its rows
            (`delta_load1_plain` keeps the plain-mean difference).
INCONCLUSIVE when the OFF-window noise floor exceeds half the budget (latency:
max - min of the OFF windows' p95 over the pooled OFF p95; load1: max - min of
the OFF windows' levels, so an ON window's EWMA tail is not counted as host
noise); when a window has fewer than
--min-window-samples or a pooled state fewer than --min-samples samples;
when there are fewer than 2 OFF windows or no ON window; when a workload
operation or the sampler failed; when the run was cut short; or when an ON
run failed, reported another priority class than the one recorded, or kept
its window busy less than --min-on-busy of the time. Otherwise PASS when
d_p95 <= +25 % and d_load1 <= +2.0, else FAIL. `raw_comparison` keeps the
PASS/FAIL the numbers alone would give.

A/A noise mode (--aa): the ON windows run nothing, so the same statistics
measure this host's own noise. QUIET when the noise floor holds and
|d_p95| and |d_load1| are both within half the budget, NOISY otherwise,
INCONCLUSIVE on the structural reasons above. An A/A run never gives a
budget verdict.

Process safety (R-N11). The instrument never signals a process and never
asks whether one is alive. Every child it starts ends on its own: the git,
rg/grep and sysctl children are bounded by their small fixed inputs, and the
ON command is waited for until it exits. No child has a deadline that would
end it early. An ON command still running at its window's deadline extends
that window (the overrun is recorded); the next OFF window starts only after
it exits. The workload and sampler loops end when the protocol ends, and in
any case at the planned session length plus --max-overrun-seconds; past that
bound no ON command is started again, the run ends as cut short, and the
verdict is INCONCLUSIVE.

Evidence (R-N81, R-N91, OI-1003-Q34). A run is NOT EVIDENCE unless
--evidence is given, and --evidence refuses unless the whole measurement
protocol is the SLO's (`protocol_problems`): windows of at least 300 s, at
least five of them, the ON command repeated for the whole window (no
--no-repeat) with --max-runs at its default or more, not an A/A run,
--coordinator-quiet given (recorded, not checked), the budgets, the gate
metric and --settle-seconds exactly at their pinned values, and
--min-samples, --min-window-samples and --min-on-busy at their defaults or
stricter; and the host must be on AC power with load1 < 2.5 at the start.
In evidence mode every ON run must report its priority class, an ON
window that reaches the run cap is INCONCLUSIVE, and R-N81 is checked over
the whole run, not only at the start: a window whose power_start or
power_end is not AC, or an OFF window whose lag-corrected load1 level is
not under 2.5, makes the run INCONCLUSIVE and not evidence. `analyze` labels a trace
evidence only when it was run with --evidence, its schema is this one, its
recorded configuration and gate pass the same protocol check, and no
override differs from the recorded gate; `evidence_problems` says why not.
So a verdict cannot be chosen after the fact and keep the label. Gated S2
runs are separate from the S1 gate (OI-1003-Q34).

Output: OUT/trace.json (config, host, every sample, windows, verdict),
OUT/summary.md, and OUT/on-runs/ (a log and a run_dir per ON run). The work
directory is removed at the end unless --keep-workdir.

  run --source SRC --out OUT [options] -- ON_COMMAND...
  run --source SRC --out OUT --aa [options]
  analyze TRACE.json [gate options]   re-derive the verdict from a trace
  selftest                            run test_s2_budget.py

Exit: 0 complete (the verdict is in the output, whatever it is), 2 refused
before the run, 3 aborted by a setup or instrument failure (every child has
still exited and both loops have ended), 1 selftest failure.
"""

from __future__ import annotations

import argparse
import bisect
import datetime as dt
import json
import math
import os
import platform
import random
import re
import shutil
import sqlite3
import statistics
import subprocess
import sys
import threading
import time
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import r23_ab  # noqa: E402  (power_source: one R-N81 power probe for every bench)

SCHEMA = "bulkload.s2-budget.v1"
RULINGS = "OI-1003-Q34, OI-1003-Q5, OI-1003-Q9, R-N11, R-N81, R-N91, R-N13"
BUDGET_P95 = 0.25
BUDGET_LOAD1 = 2.0
GATE_METRIC = "step"
EVIDENCE_MIN_WINDOW = 300.0
EVIDENCE_MIN_WINDOWS = 5
LOAD_LIMIT = r23_ab.LOAD_LIMIT
OFF, ON = "OFF", "ON"
OPS = ("jsonl", "sqlite", "git", "search")
STEP = "step"
METRICS = (STEP, *OPS)
EPS = 1e-9
NEEDLE = "s2_budget_needle"
NOT_EVIDENCE = "NOT EVIDENCE"
DEFAULT_GATE = {
    "budget_p95": BUDGET_P95,
    "budget_load1": BUDGET_LOAD1,
    "gate_metric": GATE_METRIC,
    "settle_seconds": 60.0,
    "min_samples": 100,
    "min_window_samples": 20,
    "min_on_busy": 0.8,
}
DEFAULT_MAX_RUNS = 1000
# The evidence protocol (OI-1003-Q34, S2): these gate values are pinned
# exactly, so a verdict cannot be chosen after the fact, ...
EVIDENCE_PINNED = ("budget_p95", "budget_load1", "gate_metric", "settle_seconds")
# ... and these floors may only be raised, which can only add INCONCLUSIVE.
EVIDENCE_FLOORS = ("min_samples", "min_window_samples", "min_on_busy")
# Fixed identity for the fixture commit, so it needs no user configuration.
FIXTURE_ENV = {
    "GIT_AUTHOR_NAME": "s2-budget fixture",
    "GIT_AUTHOR_EMAIL": "s2-budget@fixture.invalid",
    "GIT_AUTHOR_DATE": "2026-10-03T00:00:00Z",
    "GIT_COMMITTER_NAME": "s2-budget fixture",
    "GIT_COMMITTER_EMAIL": "s2-budget@fixture.invalid",
    "GIT_COMMITTER_DATE": "2026-10-03T00:00:00Z",
}
COUNTERS_PRIORITY = re.compile(r"(?:^|\s)priority=(\S+)")
# The kernel's load1 model (Linux and Darwin alike): every 5 s,
# load1 <- load1 * e + n * (1 - e), with e = exp(-5/60) and n the run queue.
# Over the K updates in (t0, t1], sum(n) = sum(load1) + e/(1-e) (load1(t1) -
# load1(t0)), so the mean run queue over a span is the mean load1 sampled in
# it plus tau (L(t1) - L(t0)) / (t1 - t0): `load1_level`. With 1 Hz rows,
# each update is held for 5 rows, and the first update after t0 lands 0 to 4
# rows into the span (the sampler's phase against the kernel's is unknown),
# which adds that many rows of the old value. So tau = 5 e/(1-e) + c, with c
# in 0..4; LOAD1_TAU_S takes the mean c = 2. The residual is at most
# 2 (L(t1) - L(t0)) / span per window: about +-0.5 % of a steady add at 300 s
# windows and a 60 s settle, against -14 % for the plain mean.
LOAD1_UPDATE_S = 5.0
LOAD1_DECAY = math.exp(-LOAD1_UPDATE_S / 60.0)
LOAD1_TAU_S = (
    LOAD1_UPDATE_S * LOAD1_DECAY / (1.0 - LOAD1_DECAY) + (LOAD1_UPDATE_S - 1.0) / 2.0
)
# The sample just before a settled span stands for load1(t0) when it is in
# the same window and at most this far before the span (the sampler runs at
# 1 Hz).
LOAD1_PREV_GAP_S = 2.0


def say(message: str) -> None:
    print(f"s2-budget {message}", flush=True)


def utc_now() -> str:
    return dt.datetime.now(dt.UTC).strftime("%Y-%m-%dT%H:%M:%SZ")


def rounded(value: float | None, places: int = 4) -> float | None:
    return None if value is None else round(value, places)


# --- sampler -----------------------------------------------------------------


def load_source() -> str:
    system = platform.system()
    if system == "Linux" and Path("/proc/loadavg").is_file():
        return "/proc/loadavg"
    if system == "Darwin":
        return "sysctl -n vm.loadavg"
    return "os.getloadavg"


def read_load1(source: str) -> float:
    if source == "/proc/loadavg":
        return float(Path("/proc/loadavg").read_text().split()[0])
    if source == "sysctl -n vm.loadavg":
        sysctl = shutil.which("sysctl") or "/usr/sbin/sysctl"
        out = subprocess.run(
            [sysctl, "-n", "vm.loadavg"],
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            check=True,
        ).stdout
        return float(out.replace("{", " ").replace("}", " ").split()[0])
    return os.getloadavg()[0]


def cadence(
    origin: float,
    phase: float,
    stop: threading.Event,
    until: float,
    body,
    missed: list | None = None,
) -> int:
    """Call body() at origin + phase + k seconds, k = 0, 1, ..., until `stop`
    is set or the next tick would fall at or past `until`. A tick that
    overruns makes the loop skip the ticks it missed; returns how many. Each
    missed tick is appended to `missed` as (its due time from origin, how
    long after it was due the loop was free again, in ms): the wait that
    tick would have had before it could even start."""
    skipped = 0
    k = 0
    while not stop.is_set():
        due = origin + phase + k
        if due >= until:
            break
        delay = due - time.monotonic()
        if delay > 0 and stop.wait(delay):
            break
        body()
        now = time.monotonic()
        following = max(k + 1, math.ceil(now - origin - phase))
        for j in range(k + 1, following):
            tick = origin + phase + j
            if tick >= until:
                break
            skipped += 1
            if missed is not None:
                missed.append((round(tick - origin, 3), round((now - tick) * 1e3, 3)))
        k = following
    return skipped


# --- reference workload v0 ---------------------------------------------------


def fixture_text(rng: random.Random, lines: int, needle: bool) -> str:
    rows = [
        " ".join(f"w{rng.randrange(4096):03x}" for _ in range(8)) for _ in range(lines)
    ]
    if needle:
        rows[rng.randrange(lines)] += f" {NEEDLE}"
    return "\n".join(rows) + "\n"


def git_base(git: str) -> list[str]:
    # No fsmonitor daemon (a child that would outlive the step) and no gc.
    return [git, "-c", "core.fsmonitor=false", "-c", "gc.auto=0"]


# Every git child runs in an environment of its own, matching the agent's
# `git_carry::git_env`. An inherited GIT_DIR, GIT_INDEX_FILE or GIT_WORK_TREE
# (git exports them to hooks, `rebase -x` and `bisect run`) would point the
# fixture build and the 1 Hz workload at a repository outside the workdir, so
# every inherited GIT_* variable is dropped (a superset of git_env::CLEARED,
# which also covers GIT_CONFIG_KEY_<n>/GIT_CONFIG_VALUE_<n> and
# GIT_TEMPLATE_DIR). The global and system configuration are not read, git
# never prompts, and discovery never climbs above the work directory.
GIT_SET = {
    "GIT_CONFIG_GLOBAL": "/dev/null",
    "GIT_CONFIG_NOSYSTEM": "1",
    "GIT_TERMINAL_PROMPT": "0",
}


def git_env(ceiling: Path, base: dict[str, str] | None = None) -> dict[str, str]:
    """The environment of every git child: `base` (the process environment
    by default) without any GIT_* variable, plus GIT_SET, plus
    GIT_CEILING_DIRECTORIES at `ceiling`."""
    source = os.environ if base is None else base
    env = {key: value for key, value in source.items() if not key.startswith("GIT_")}
    env.update(GIT_SET)
    env["GIT_CEILING_DIRECTORIES"] = str(ceiling)
    return env


def build_fixtures(root: Path, git: str) -> None:
    """A 64-file fixture repository with one modified file, so `git diff`
    has output, and a 256-file tree for the search. Deterministic. The
    commit is made with plumbing (write-tree, commit-tree, update-ref), which
    runs no commit hooks, so none are invoked or bypassed. Every git child
    runs with `git_env`, so nothing outside `root` is read or written."""
    rng = random.Random(34)
    repo = root / "repo"
    for d in range(8):
        for f in range(8):
            path = repo / "src" / f"d{d}" / f"f{f}.txt"
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(fixture_text(rng, 48, needle=False))
    for t in range(16):
        for f in range(16):
            path = root / "tree" / f"t{t:02}" / f"f{f:02}.txt"
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(fixture_text(rng, 64, needle=(t * 16 + f) % 7 == 0))
    env = {**git_env(root.resolve()), **FIXTURE_ENV}
    base = git_base(git)

    def plumb(*args: str) -> str:
        return subprocess.run(
            [*base, "-C", str(repo), *args],
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            check=True,
            env=env,
        ).stdout.strip()

    subprocess.run(
        [*base, "init", "-q", "-b", "main", str(repo)],
        stdin=subprocess.DEVNULL,
        capture_output=True,
        check=True,
        env=env,
    )
    plumb("add", "-A")
    tree = plumb("write-tree")
    commit = plumb("commit-tree", "--no-gpg-sign", "-m", "s2-budget fixture", tree)
    plumb("update-ref", "refs/heads/main", commit)
    with (repo / "src" / "d0" / "f0.txt").open("a") as handle:
        handle.write("an uncommitted agent edit\n")


def search_command(tree: Path) -> tuple[str, list[str]] | None:
    rg = shutil.which("rg")
    if rg:
        return "rg", [rg, "--no-config", "-c", NEEDLE, str(tree)]
    grep = shutil.which("grep")
    if grep:
        return "grep -r", [grep, "-r", "-c", NEEDLE, str(tree)]
    return None


class Workload:
    """Reference agent workload v0: one step, four timed operations."""

    def __init__(self, root: Path, git: str, search: list[str]) -> None:
        self.root = root
        self.git = git_base(git)
        self.git_env = git_env(root.resolve())
        self.search = search
        self.repo = root / "repo"
        self.steps = 0
        self.fd = os.open(
            root / "agent.jsonl", os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600
        )
        self.db = sqlite3.connect(
            root / "agent.sqlite", isolation_level=None, check_same_thread=False
        )
        self.db.execute("PRAGMA journal_mode=WAL")
        self.db.execute("PRAGMA synchronous=FULL")
        self.db.execute(
            "CREATE TABLE IF NOT EXISTS events"
            " (n INTEGER PRIMARY KEY, t REAL, body TEXT)"
        )
        self.db.execute(
            "CREATE TABLE IF NOT EXISTS counters (name TEXT PRIMARY KEY, value INTEGER)"
        )
        self.db.execute("INSERT OR IGNORE INTO counters VALUES ('steps', 0)")

    def close(self) -> None:
        os.close(self.fd)
        self.db.close()

    def jsonl(self) -> None:
        line = json.dumps({"n": self.steps, "t": time.time(), "pad": "x" * 192})
        os.write(self.fd, (line + "\n").encode())
        os.fsync(self.fd)

    def sqlite(self) -> None:
        self.db.execute("BEGIN IMMEDIATE")
        try:
            self.db.execute(
                "INSERT INTO events (t, body) VALUES (?, ?)", (time.time(), "y" * 192)
            )
            self.db.execute("UPDATE counters SET value = value + 1 WHERE name='steps'")
            self.db.execute("COMMIT")
        except sqlite3.Error:
            if self.db.in_transaction:
                self.db.execute("ROLLBACK")
            raise

    def git_status_diff(self) -> None:
        for args in (("status", "--porcelain=v1"), ("diff",)):
            subprocess.run(
                [*self.git, "-C", str(self.repo), *args],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                check=True,
                env=self.git_env,
            )

    def search_tree(self) -> None:
        status = subprocess.run(
            self.search,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
        ).returncode
        if status != 0:
            raise RuntimeError(f"search exited {status}")

    def step(self) -> list[tuple[str, float, str | None]]:
        self.steps += 1
        out = []
        for name, operation in (
            ("jsonl", self.jsonl),
            ("sqlite", self.sqlite),
            ("git", self.git_status_diff),
            ("search", self.search_tree),
        ):
            error = None
            start = time.perf_counter()
            try:
                operation()
            except (
                OSError,
                sqlite3.Error,
                subprocess.SubprocessError,
                RuntimeError,
            ) as exc:
                error = f"{type(exc).__name__}: {exc}"[:200]
            out.append((name, (time.perf_counter() - start) * 1000.0, error))
        return out


# --- analysis ----------------------------------------------------------------


def p95(values: list[float]) -> float | None:
    """Nearest-rank 95th percentile."""
    if not values:
        return None
    ordered = sorted(values)
    return ordered[max(0, math.ceil(0.95 * len(ordered)) - 1)]


def busy_fraction(window: dict) -> float:
    length = window["end"] - window["start"]
    if length <= 0:
        return 0.0
    busy = sum(
        max(0.0, min(run["end"], window["end"]) - max(run["start"], window["start"]))
        for run in window.get("runs", [])
    )
    return busy / length


def protocol_problems(config: dict, gate: dict) -> list[str]:
    """Why a run with this configuration and gate is not the S2 evidence
    protocol, or [] when it is. `run --evidence` refuses on any of them and
    `analyze` labels a trace evidence only when there are none."""
    out = []
    if config.get("aa"):
        out.append("an A/A run is a noise measurement, not evidence")
    if (config.get("window_seconds") or 0) < EVIDENCE_MIN_WINDOW:
        out.append(f"evidence needs windows of at least {EVIDENCE_MIN_WINDOW:.0f} s")
    if len(config.get("pattern") or []) < EVIDENCE_MIN_WINDOWS:
        out.append(f"evidence needs at least {EVIDENCE_MIN_WINDOWS} windows")
    if not config.get("repeat"):
        out.append(
            "evidence repeats the ON command for the whole window (no --no-repeat)"
        )
    if (config.get("max_runs") or 0) < DEFAULT_MAX_RUNS:
        out.append(f"evidence keeps --max-runs at {DEFAULT_MAX_RUNS} or more")
    if not config.get("coordinator_quiet"):
        out.append("evidence needs --coordinator-quiet (R-N91)")
    for key in EVIDENCE_PINNED:
        if gate.get(key) != DEFAULT_GATE[key]:
            out.append(
                f"evidence uses the SLO protocol unchanged: {key} is pinned at "
                f"{DEFAULT_GATE[key]} (got {gate.get(key)})"
            )
    for key in EVIDENCE_FLOORS:
        value = gate.get(key)
        if value is None or value < DEFAULT_GATE[key]:
            out.append(
                f"evidence needs {key} of at least {DEFAULT_GATE[key]} (got {value})"
            )
    return out


def load1_level(
    rows: list[tuple[float, float]], prev: tuple[float, float] | None
) -> float | None:
    """The lag-corrected mean run queue over a window's settled load1 rows
    (time, load1), given the sample just before them (or None): the mean
    load1 plus LOAD1_TAU_S times its slope from `prev` (or the first row) to
    the last row. Unbiased for the kernel's 5 s EWMA whatever the settle."""
    if not rows:
        return None
    mean = statistics.fmean(value for _, value in rows)
    first_t, first_v = prev if prev is not None else rows[0]
    last_t, last_v = rows[-1]
    if last_t <= first_t:
        return mean
    return mean + LOAD1_TAU_S * (last_v - first_v) / (last_t - first_t)


def load1_windows(
    windows: list[dict], load: list[dict], settle: float
) -> list[tuple[list[tuple[float, float]], float | None]]:
    """Per window: its settled load1 rows and their lag-corrected level."""
    times = [row["t"] for row in load]
    out = []
    for window in windows:
        begin = window["start"] + settle
        low = bisect.bisect_left(times, begin)
        high = bisect.bisect_left(times, window["end"])
        rows = [(load[i]["t"], load[i]["load1"]) for i in range(low, high)]
        prev = None
        # Only a sample inside the window: with no settle the span starts at
        # the switch, and the first settled row stands in for load1(t0).
        if (
            low > 0
            and times[low - 1] >= window["start"]
            and begin - times[low - 1] <= LOAD1_PREV_GAP_S
        ):
            prev = (times[low - 1], load[low - 1]["load1"])
        out.append((rows, load1_level(rows, prev)))
    return out


def weighted_level(
    estimates: list[tuple[list, float | None]], windows: list[dict], state: str
) -> tuple[float | None, float | None]:
    """(lag-corrected level, plain mean) of one state, weighting each window
    by its settled sample count."""
    chosen = [
        (rows, level)
        for (rows, level), window in zip(estimates, windows, strict=True)
        if window["state"] == state and rows
    ]
    total = sum(len(rows) for rows, _ in chosen)
    if not total:
        return None, None
    level = sum(len(rows) * lvl for rows, lvl in chosen) / total
    plain = sum(value for rows, _ in chosen for _, value in rows) / total
    return level, plain


def load1_response(windows: list[dict], times: list[float], settle: float) -> dict:
    """What a steady +1.0 run-queue add in the ON windows reads as, on this
    trace's windows, sample times and settle, through the kernel model: the
    plain pooled mean difference (`plain`, the attenuation the old estimator
    had) and the lag-corrected one (`corrected`, ~1.0)."""
    if not windows or not times:
        return {"plain": None, "corrected": None}
    starts = [w["start"] for w in windows]
    level, update = 0.0, windows[0]["start"]
    load = []
    for t in times:
        while update <= t:
            index = bisect.bisect_right(starts, update) - 1
            on = 0 <= index < len(windows) and windows[index]["state"] == ON
            if on and update >= windows[index]["end"]:
                on = False
            level = level * LOAD1_DECAY + (1.0 if on else 0.0) * (1.0 - LOAD1_DECAY)
            update += LOAD1_UPDATE_S
        load.append({"t": t, "load1": level})
    estimates = load1_windows(windows, load, settle)
    on_level, on_plain = weighted_level(estimates, windows, ON)
    off_level, off_plain = weighted_level(estimates, windows, OFF)
    if None in (on_level, off_level):
        return {"plain": None, "corrected": None}
    return {
        "plain": rounded(on_plain - off_plain),
        "corrected": rounded(on_level - off_level),
    }


def analyze(trace: dict, overrides: dict | None = None) -> dict:
    """The verdict of one trace: a pure function of its windows and samples."""
    recorded = {**DEFAULT_GATE, **trace.get("gate", {})}
    gate = {**recorded, **(overrides or {})}
    config = trace.get("config", {})
    aa = bool(config.get("aa"))
    windows = trace["windows"]
    settle = gate["settle_seconds"]
    budget, budget_load = gate["budget_p95"], gate["budget_load1"]
    gated = (STEP,) if gate["gate_metric"] == "step" else OPS
    starts = [w["start"] for w in windows]

    def locate(t: float) -> int | None:
        index = bisect.bisect_right(starts, t) - 1
        if index >= 0 and t < windows[index]["end"]:
            return index
        return None

    buckets = [
        {"lat": {m: [] for m in METRICS}, "load": [], "missed": 0} for _ in windows
    ]
    failed_ops = 0
    for row in trace.get("latency", []):
        index = locate(row["t"])
        if not row["ok"]:
            failed_ops += row["op"] != STEP
            continue
        if index is not None and row["t"] >= windows[index]["start"] + settle:
            buckets[index]["lat"][row["op"]].append(row["ms"])
    # Coordinated omission: a step that overran skipped the ticks it missed,
    # so the slowest stretches would contribute the fewest samples and p95
    # would read low. Each missed workload tick is back-filled into every
    # metric with the wait it would have had (HdrHistogram's correction for
    # the expected 1 s interval; a lower bound on what that tick would have
    # seen). Sample floors count measured samples only.
    for row in trace.get("missed_ticks", {}).get("workload", []):
        index = locate(row["t"])
        if index is not None and row["t"] >= windows[index]["start"] + settle:
            buckets[index]["missed"] += 1
            for metric in METRICS:
                buckets[index]["lat"][metric].append(row["lag_ms"])
    load = sorted(trace.get("load", []), key=lambda row: row["t"])
    estimates = load1_windows(windows, load, settle)
    for bucket, (rows, _) in zip(buckets, estimates, strict=True):
        bucket["load"] = [value for _, value in rows]

    per_window = []
    off_p95s, off_levels = {m: [] for m in METRICS}, []
    pooled = {s: {"lat": {m: [] for m in METRICS}, "load": []} for s in (OFF, ON)}
    missed = {OFF: 0, ON: 0}
    for window, bucket, (_, level) in zip(windows, buckets, estimates, strict=True):
        state = window["state"]
        window_p95 = {m: p95(bucket["lat"][m]) for m in METRICS}
        window_mean = statistics.fmean(bucket["load"]) if bucket["load"] else None
        for metric in METRICS:
            pooled[state]["lat"][metric].extend(bucket["lat"][metric])
            if state == OFF:
                off_p95s[metric].append(window_p95[metric])
        pooled[state]["load"].extend(bucket["load"])
        missed[state] += bucket["missed"]
        if state == OFF:
            off_levels.append(level)
        per_window.append(
            {
                "index": window["index"],
                "state": state,
                "n": {m: len(bucket["lat"][m]) - bucket["missed"] for m in METRICS},
                "missed": bucket["missed"],
                "p95_ms": {m: rounded(window_p95[m]) for m in METRICS},
                "load_n": len(bucket["load"]),
                "load1_mean": rounded(window_mean),
                "load1_level": rounded(level),
                "busy": rounded(busy_fraction(window)) if state == ON else None,
            }
        )

    p95s = {s: {m: p95(pooled[s]["lat"][m]) for m in METRICS} for s in (OFF, ON)}
    delta = {}
    for metric in METRICS:
        off, on = p95s[OFF][metric], p95s[ON][metric]
        delta[metric] = on / off - 1.0 if off and on is not None else None
    levels, means = {}, {}
    for state in (OFF, ON):
        levels[state], means[state] = weighted_level(estimates, windows, state)
    d_load1 = (
        levels[ON] - levels[OFF] if None not in (levels[ON], levels[OFF]) else None
    )
    d_load1_plain = (
        means[ON] - means[OFF] if None not in (means[ON], means[OFF]) else None
    )
    offs = [w for w in per_window if w["state"] == OFF]
    ons = [w for w in per_window if w["state"] == ON]
    noise = {}
    for metric in METRICS:
        values = off_p95s[metric]
        pooled_off = p95s[OFF][metric]
        noise[metric] = (
            (max(values) - min(values)) / pooled_off
            if len(values) >= 2 and None not in values and pooled_off
            else None
        )
    noise_load = (
        max(off_levels) - min(off_levels)
        if len(off_levels) >= 2 and None not in off_levels
        else None
    )

    if gate["gate_metric"] == "step":
        worst = STEP
    else:
        worst = max(OPS, key=lambda m: delta[m] if delta[m] is not None else math.inf)
    d_p95 = delta[worst]
    gate_noise = (
        None if any(noise[m] is None for m in gated) else max(noise[m] for m in gated)
    )

    structural = []
    if trace.get("cut_short"):
        structural.append(f"cut short: {trace['cut_short']}")
    if trace.get("errors"):
        structural.append(f"instrument errors: {len(trace['errors'])}")
    if failed_ops:
        structural.append(f"workload errors: {failed_ops} failed operations")
    if len(offs) < 2 or not ons:
        structural.append(
            f"too few windows: {len(offs)} OFF and {len(ons)} ON "
            "(need at least 2 OFF and 1 ON)"
        )
    floor_window, floor_pool = gate["min_window_samples"], gate["min_samples"]
    for w in per_window:
        n = min(w["n"][m] for m in gated)
        if n < floor_window or w["load_n"] < floor_window:
            structural.append(
                f"too few samples: window {w['index']} ({w['state']}) has {n} "
                f"latency and {w['load_n']} load1 samples after settle "
                f"(min {floor_window})"
            )
    for state in (OFF, ON):
        n = min(len(pooled[state]["lat"][m]) for m in gated) - missed[state]
        n_load = len(pooled[state]["load"])
        if n < floor_pool or n_load < floor_pool:
            structural.append(
                f"too few samples: pooled {state} has {n} latency and {n_load} "
                f"load1 samples (min {floor_pool})"
            )
    evidence = bool(config.get("evidence"))
    # R-N81: a gated sample is taken only on AC power with load1 under the
    # limit. Under evidence, power is checked at every window boundary and
    # each OFF window's lag-corrected level (the host baseline, without the
    # previous ON window's EWMA tail) must stay under LOAD_LIMIT.
    rn81 = []
    if evidence:
        for w in windows:
            for key in ("power_start", "power_end"):
                if w.get(key) != "ac":
                    rn81.append(
                        f"R-N81: window {w['index']} {key.replace('_', ' ')} "
                        f"was {w.get(key, 'unrecorded')}, not AC power"
                    )
        for w in per_window:
            level = w["load1_level"]
            if w["state"] == OFF and level is not None and level >= LOAD_LIMIT:
                rn81.append(
                    f"R-N81: OFF window {w['index']} load1 level {level:.2f} is "
                    f"not under {LOAD_LIMIT}"
                )
    structural.extend(rn81)
    if not aa:
        requested = config.get("priority")
        for w in windows:
            if w["state"] != ON:
                continue
            runs = w.get("runs", [])
            if not runs:
                structural.append(f"ON window {w['index']} ran no command")
            for run in runs:
                if run.get("exit") != 0:
                    structural.append(
                        f"ON run {run['run']} failed: exit {run.get('exit')}"
                        + (f" ({run['error']})" if run.get("error") else "")
                    )
                seen = run.get("priority_observed", [])
                if seen and seen != [requested]:
                    structural.append(
                        f"ON run {run['run']} reported priority {','.join(seen)}, "
                        f"recorded {requested}"
                    )
                if not seen and evidence:
                    structural.append(
                        f"ON run {run['run']} did not report its priority class"
                    )
            if w.get("run_cap_reached") and evidence:
                structural.append(
                    f"ON window {w['index']} reached the run cap (--max-runs)"
                )
            busy = busy_fraction(w)
            if runs and busy < gate["min_on_busy"]:
                structural.append(
                    f"ON window {w['index']} was busy {busy:.0%} of the time "
                    f"(min {gate['min_on_busy']:.0%})"
                )

    noisy = []
    if gate_noise is not None and gate_noise > budget / 2 + EPS:
        noisy.append(
            f"noise floor: OFF-window p95 spread {gate_noise:.1%} exceeds half "
            f"the budget ({budget / 2:.1%})"
        )
    if noise_load is not None and noise_load > budget_load / 2 + EPS:
        noisy.append(
            f"noise floor: OFF-window load1 spread {noise_load:.2f} exceeds half "
            f"the budget ({budget_load / 2:.2f})"
        )

    over = []
    if d_p95 is not None and d_p95 > budget + EPS:
        over.append(f"p95 {worst} {d_p95:+.1%} exceeds +{budget:.0%}")
    if d_load1 is not None and d_load1 > budget_load + EPS:
        over.append(f"load1 {d_load1:+.2f} exceeds +{budget_load:.1f}")
    raw = None
    if d_p95 is not None and d_load1 is not None:
        raw = "FAIL" if over else "PASS"

    if aa:
        mode = "aa"
        if structural:
            status, reasons = "INCONCLUSIVE", structural
        else:
            wide = []
            if d_p95 is not None and abs(d_p95) > budget / 2 + EPS:
                wide.append(f"A/A p95 {worst} {d_p95:+.1%} beyond half the budget")
            if d_load1 is not None and abs(d_load1) > budget_load / 2 + EPS:
                wide.append(f"A/A load1 {d_load1:+.2f} beyond half the budget")
            reasons = noisy + wide
            status = "NOISY" if reasons else "QUIET"
    else:
        mode = "budget"
        if structural or noisy:
            status, reasons = "INCONCLUSIVE", structural + noisy
        elif raw is None:
            status, reasons = (
                "INCONCLUSIVE",
                ["no comparison: a p95 or load1 is missing"],
            )
        else:
            status, reasons = raw, over
    if not config.get("evidence"):
        evidence_problems = ["the run was made without --evidence"]
    else:
        evidence_problems = protocol_problems(config, trace.get("gate", {})) + rn81
        if trace.get("schema") != SCHEMA:
            evidence_problems.append(
                f"trace schema {trace.get('schema')} is not {SCHEMA}"
            )
        for key in sorted(overrides or {}):
            if overrides[key] != recorded.get(key):
                evidence_problems.append(
                    f"analyze override {key}={overrides[key]} differs from the "
                    f"recorded {recorded.get(key)}"
                )
    return {
        "status": status,
        "mode": mode,
        "evidence": not evidence_problems,
        "evidence_problems": evidence_problems,
        "reasons": reasons,
        "raw_comparison": raw,
        "gate": gate,
        "gate_metric": gate["gate_metric"],
        "worst_metric": worst,
        "delta_p95": rounded(d_p95),
        "delta_load1": rounded(d_load1),
        "delta_load1_plain": rounded(d_load1_plain),
        "load1_model": {
            "update_s": LOAD1_UPDATE_S,
            "decay": rounded(LOAD1_DECAY, 6),
            "tau_s": rounded(LOAD1_TAU_S),
            "response_to_unit_add": load1_response(
                windows, [row["t"] for row in load], settle
            ),
        },
        "noise_p95": rounded(gate_noise),
        "noise_load1": rounded(noise_load),
        "delta_p95_by_metric": {m: rounded(delta[m]) for m in METRICS},
        "noise_p95_by_metric": {m: rounded(noise[m]) for m in METRICS},
        "p95_ms": {s: {m: rounded(p95s[s][m]) for m in METRICS} for s in (OFF, ON)},
        "load1_mean": {s: rounded(means[s]) for s in (OFF, ON)},
        "load1_level": {s: rounded(levels[s]) for s in (OFF, ON)},
        "samples": {
            s: {
                "latency": min(len(pooled[s]["lat"][m]) for m in gated) - missed[s],
                "missed_ticks": missed[s],
                "load1": len(pooled[s]["load"]),
            }
            for s in (OFF, ON)
        },
        "failed_operations": failed_ops,
        "windows": per_window,
    }


# --- run ---------------------------------------------------------------------


def parse_pattern(text: str) -> tuple[list[str] | None, str | None]:
    states = [part.strip().upper() for part in text.split(",") if part.strip()]
    if any(s not in (OFF, ON) for s in states):
        return None, f"--pattern takes off and on, comma-separated: {text!r}"
    if len(states) < 3 or states[0] != OFF or states[-1] != OFF:
        return None, "--pattern must start and end OFF and have at least 3 windows"
    if any(a == b for a, b in zip(states, states[1:], strict=False)):
        return None, "--pattern must alternate OFF and ON"
    return states, None


def is_within(path: Path, root: Path) -> bool:
    try:
        path.relative_to(root)
    except ValueError:
        return False
    return True


def workdir_refusal(source: Path, workdir: Path, device_of=None) -> str | None:
    """Why the workload may not run in `workdir`, or None. It must be new
    (so it cannot hold the source), outside the source, and on the source's
    device."""
    device_of = device_of or (lambda p: p.stat().st_dev)
    source, workdir = source.resolve(), workdir.resolve()
    if workdir.exists():
        return f"workdir {workdir} already exists; the instrument makes its own"
    if not workdir.parent.is_dir():
        return f"workdir parent {workdir.parent} is not a directory"
    if is_within(workdir, source):
        return f"workdir {workdir} is inside the source; it must be a sibling"
    if device_of(workdir.parent) != device_of(source):
        return (
            f"workdir {workdir} is not on the source's device; pass a sibling "
            "directory on that device with --workdir"
        )
    return None


def run_refusals(args: argparse.Namespace, states: list[str] | None) -> list[str]:
    out = []
    if args.window_seconds <= 0:
        out.append("--window-seconds must be positive")
    if args.settle_seconds < 0 or args.settle_seconds >= args.window_seconds / 2:
        out.append("--settle-seconds must be at least 0 and under half a window")
    if args.aa and args.on_command:
        out.append("--aa runs nothing in the ON windows; drop the ON command")
    if not args.aa and not args.on_command:
        out.append("the ON windows need a command after -- (or --aa)")
    if args.max_runs < 1:
        out.append("--max-runs must be at least 1")
    if args.evidence:
        config = {
            "aa": args.aa,
            "window_seconds": args.window_seconds,
            # A refused pattern is reported on its own; do not double it here.
            "pattern": states if states is not None else [OFF] * EVIDENCE_MIN_WINDOWS,
            "repeat": args.repeat,
            "max_runs": args.max_runs,
            "coordinator_quiet": args.coordinator_quiet,
        }
        out.extend(protocol_problems(config, gate_of(args)))
    source = args.source.resolve()
    if not source.is_dir():
        out.append(f"source {source} is not a directory")
    out_dir = args.out.resolve()
    if out_dir.exists():
        out.append(f"out {out_dir} already exists")
    elif not out_dir.parent.is_dir():
        out.append(f"out parent {out_dir.parent} is not a directory")
    if source.is_dir() and is_within(out_dir, source):
        out.append(f"out {out_dir} is inside the source")
    return out


def gate_of(args: argparse.Namespace) -> dict:
    return {key: getattr(args, key) for key in DEFAULT_GATE}


def priorities_in(log: Path) -> list[str]:
    seen = set()
    try:
        text = log.read_text(errors="replace")
    except OSError:
        return []
    for line in text.splitlines():
        if line.startswith("counters"):
            seen.update(COUNTERS_PRIORITY.findall(line))
    return sorted(seen)


def run_once(command, priority, run, runs_dir, origin) -> dict:
    """Start one ON run and wait for it to exit: no deadline, no signal."""
    run_dir = runs_dir / f"r{run:04}"
    run_dir.mkdir()
    log = runs_dir / f"r{run:04}.log"
    argv = [
        token.replace("{priority}", priority)
        .replace("{run}", str(run))
        .replace("{run_dir}", str(run_dir))
        for token in command
    ]
    record = {"run": run, "argv": argv, "start": time.monotonic() - origin}
    try:
        with log.open("wb") as handle:
            child = subprocess.Popen(
                argv,
                stdin=subprocess.DEVNULL,
                stdout=handle,
                stderr=subprocess.STDOUT,
            )
            record["exit"] = child.wait()
    except OSError as exc:
        record["exit"] = None
        record["error"] = f"{type(exc).__name__}: {exc}"
    record["end"] = time.monotonic() - origin
    record["seconds"] = round(record["end"] - record["start"], 3)
    record["log"] = str(log)
    record["priority_observed"] = priorities_in(log)
    return record


def host_record(git: str, search: list[str], source: str, env: dict) -> dict:
    def version(argv: list[str]) -> str:
        try:
            out = subprocess.run(
                argv,
                stdin=subprocess.DEVNULL,
                capture_output=True,
                text=True,
                check=False,
                env=env,
            ).stdout
        except OSError:
            return "unknown"
        return out.splitlines()[0] if out else "unknown"

    return {
        "hostname": platform.node(),
        "system": platform.system(),
        "release": platform.release(),
        "machine": platform.machine(),
        "cpus": os.cpu_count(),
        "python": platform.python_version(),
        "git": version([git, "--version"]),
        "search": version([search[0], "--version"]),
        "load_source": source,
    }


def session(args: argparse.Namespace, states: list[str], workdir: Path) -> dict:
    """Run the protocol and return the trace (not yet analysed)."""
    git = shutil.which("git")
    out_dir = args.out.resolve()
    tool, search = search_command(workdir / "tree")
    source_name = load_source()
    workdir.mkdir()
    build_fixtures(workdir, git)
    workload = Workload(workdir, git, search)
    workload.step()  # warm-up, not recorded: caches and the git index refresh
    runs_dir = out_dir / "on-runs"
    runs_dir.mkdir()
    window = args.window_seconds
    trace = {
        "schema": SCHEMA,
        "rulings": RULINGS,
        "label": args.label,
        "config": {
            "evidence": bool(args.evidence),
            "aa": bool(args.aa),
            "source": str(args.source.resolve()),
            "workdir": str(workdir),
            "out": str(out_dir),
            "pattern": states,
            "window_seconds": window,
            "priority": args.priority,
            "repeat": args.repeat,
            "max_runs": args.max_runs,
            "max_overrun_seconds": args.max_overrun_seconds,
            "on_command": list(args.on_command),
            "coordinator_quiet": bool(args.coordinator_quiet),
            "search_tool": tool,
            "workload": "v0: jsonl+fsync, sqlite WAL synchronous=FULL, "
            "git status+diff, search",
        },
        "gate": gate_of(args),
        "host": host_record(git, search, source_name, workload.git_env),
        "start_utc": utc_now(),
        "windows": [],
        "latency": [],
        "load": [],
        "errors": [],
        "skipped_ticks": {},
        "missed_ticks": {},
        "cut_short": None,
    }
    latency, load, errors = trace["latency"], trace["load"], trace["errors"]
    stop = threading.Event()
    origin = time.monotonic()
    until = origin + len(states) * window + args.max_overrun_seconds

    def work() -> None:
        t = round(time.monotonic() - origin, 3)
        results = workload.step()
        for name, ms, error in results:
            row = {"t": t, "op": name, "ms": round(ms, 3), "ok": error is None}
            if error:
                row["error"] = error
            latency.append(row)
        latency.append(
            {
                "t": t,
                "op": STEP,
                "ms": round(sum(ms for _, ms, _ in results), 3),
                "ok": all(error is None for _, _, error in results),
            }
        )

    def sample() -> None:
        t = round(time.monotonic() - origin, 3)
        try:
            load.append({"t": t, "load1": read_load1(source_name)})
        except (OSError, ValueError, subprocess.SubprocessError) as exc:
            errors.append(f"load1 at {t}: {type(exc).__name__}: {exc}")

    def loop(name: str, phase: float, body) -> threading.Thread:
        def target() -> None:
            missed: list[tuple[float, float]] = []
            try:
                trace["skipped_ticks"][name] = cadence(
                    origin, phase, stop, until, body, missed
                )
            except Exception as exc:  # recorded; the verdict is INCONCLUSIVE
                errors.append(f"{name} loop: {type(exc).__name__}: {exc}")
            trace["missed_ticks"][name] = [{"t": t, "lag_ms": lag} for t, lag in missed]

        return threading.Thread(target=target, name=f"s2-budget-{name}")

    loops = [loop("sampler", 0.0, sample), loop("workload", 0.5, work)]
    for thread in loops:
        thread.start()
    pause = threading.Event()
    run_count = 0
    try:
        for index, state in enumerate(states):
            start = time.monotonic()
            record = {
                "index": index,
                "state": state,
                "start": round(start - origin, 3),
                "planned_end": round(start - origin + window, 3),
                "power_start": r23_ab.power_source(),
            }
            deadline = start + window
            if state == ON and not args.aa:
                record["runs"] = []
                while True:
                    now = time.monotonic()
                    if record["runs"] and (not args.repeat or now >= deadline):
                        break
                    if now >= until:
                        trace["cut_short"] = "session bound reached in an ON window"
                        break
                    if run_count >= args.max_runs:
                        record["run_cap_reached"] = True
                        break
                    run_count += 1
                    run = run_once(
                        args.on_command, args.priority, run_count, runs_dir, origin
                    )
                    record["runs"].append(run)
                    say(
                        f"on_run run={run['run']} window={index} exit={run['exit']} "
                        f"seconds={run['seconds']} "
                        f"priority={','.join(run['priority_observed']) or 'unreported'}"
                    )
                    if run["exit"] != 0:
                        trace["cut_short"] = f"ON run {run['run']} failed"
                        break
            remaining = deadline - time.monotonic()
            if remaining > 0 and not trace["cut_short"]:
                pause.wait(remaining)
            end = time.monotonic()
            record["end"] = round(end - origin, 3)
            record["overrun_s"] = round(max(0.0, end - deadline), 3)
            record["power_end"] = r23_ab.power_source()
            trace["windows"].append(record)
            say(
                f"window index={index} state={state} "
                f"seconds={record['end'] - record['start']:.1f} "
                f"overrun_s={record['overrun_s']} runs={len(record.get('runs', []))}"
            )
            if not trace["cut_short"] and end >= until and index + 1 < len(states):
                trace["cut_short"] = "session bound reached; the loops have ended"
            if trace["cut_short"]:
                break
    finally:
        stop.set()
        for thread in loops:
            thread.join()
        workload.close()
    trace["end_utc"] = utc_now()
    return trace


def render_summary(trace: dict, verdict: dict) -> str:
    config, host = trace["config"], trace["host"]
    label = "" if verdict["evidence"] else f" ({NOT_EVIDENCE})"
    kind = "A/A noise run" if config["aa"] else "S2 budget run"
    gate = verdict["gate"]
    response = verdict["load1_model"]["response_to_unit_add"]

    def pct(value: float | None) -> str:
        return "n/a" if value is None else f"{value:+.1%}"

    def num(value: float | None, fmt: str = ".3f") -> str:
        return "n/a" if value is None else format(value, fmt)

    lines = [
        f"# {kind}: {verdict['status']}{label}",
        "",
    ]
    if not verdict["evidence"]:
        lines += [
            f"**{NOT_EVIDENCE}.** Not a gated S2 sample: "
            + "; ".join(verdict["evidence_problems"])
            + ".",
            "",
        ]
    lines += [
        f"- Label: {trace.get('label') or '(none)'}",
        f"- Verdict: **{verdict['status']}** (mode {verdict['mode']}; raw "
        f"comparison {verdict['raw_comparison']})",
        "- Reasons: " + ("; ".join(verdict["reasons"]) or "none"),
        f"- Budget: d_p95 <= +{gate['budget_p95']:.0%} on `{gate['gate_metric']}`, "
        f"d_load1 <= +{gate['budget_load1']:.1f}; INCONCLUSIVE when the OFF noise "
        "floor exceeds half the budget",
        f"- Result: d_p95 {pct(verdict['delta_p95'])} (worst "
        f"`{verdict['worst_metric']}`), d_load1 {num(verdict['delta_load1'], '+.2f')}; "
        f"OFF noise p95 {num(verdict['noise_p95'], '.1%')}, load1 "
        f"{num(verdict['noise_load1'], '.2f')}",
        f"- Host: {host['hostname']} {host['system']} {host['release']} "
        f"{host['machine']}, {host['cpus']} CPUs; load1 from `{host['load_source']}`; "
        f"{host['git']}; search `{config['search_tool']}`",
        f"- Windows: {','.join(config['pattern'])} x {config['window_seconds']:.0f} s, "
        f"settle {gate['settle_seconds']:.0f} s; {trace['start_utc']} to "
        f"{trace.get('end_utc')}",
        f"- ON command: `{' '.join(config['on_command']) or '(none, A/A)'}` at "
        f"priority `{config['priority']}`, repeat={config['repeat']}",
        f"- Workload: {config['workload']}; source `{config['source']}`, workdir "
        f"`{config['workdir']}`",
        f"- Skipped ticks: {trace.get('skipped_ticks')}; settled workload ticks "
        f"missed and back-filled: OFF {verdict['samples'][OFF]['missed_ticks']}, "
        f"ON {verdict['samples'][ON]['missed_ticks']}; instrument errors: "
        f"{len(trace.get('errors', []))}; failed operations: "
        f"{verdict['failed_operations']}",
        f"- Rulings: {trace['rulings']}",
        "",
        "| metric | OFF p95 ms | ON p95 ms | d_p95 | OFF noise |",
        "|---|---:|---:|---:|---:|",
    ]
    for metric in METRICS:
        lines.append(
            f"| {metric} | {num(verdict['p95_ms'][OFF][metric])} | "
            f"{num(verdict['p95_ms'][ON][metric])} | "
            f"{pct(verdict['delta_p95_by_metric'][metric])} | "
            f"{num(verdict['noise_p95_by_metric'][metric], '.1%')} |"
        )
    lines += [
        "",
        f"load1 level (lag-corrected): OFF {num(verdict['load1_level'][OFF], '.2f')}"
        f", ON {num(verdict['load1_level'][ON], '.2f')}; plain mean: OFF "
        f"{num(verdict['load1_mean'][OFF], '.2f')}, ON "
        f"{num(verdict['load1_mean'][ON], '.2f')} (d_load1 plain "
        f"{num(verdict['delta_load1_plain'], '+.2f')}). On these windows a "
        f"steady +1.0 reads as {num(response['plain'], '.3f')} plain and "
        f"{num(response['corrected'], '.3f')} corrected (tau "
        f"{verdict['load1_model']['tau_s']} s).",
        "",
        "| window | state | start s | end s | overrun s | step n | missed "
        "| step p95 ms | load1 mean | load1 level | load1 n | busy | runs | power |",
        "|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|",
    ]
    for raw, stats in zip(trace["windows"], verdict["windows"], strict=True):
        busy = "" if stats["busy"] is None else f"{stats['busy']:.0%}"
        lines.append(
            f"| {raw['index']} | {raw['state']} | {raw['start']:.1f} | "
            f"{raw['end']:.1f} | {raw.get('overrun_s', 0.0):.1f} | "
            f"{stats['n'][STEP]} | {stats['missed']} | "
            f"{num(stats['p95_ms'][STEP])} | {num(stats['load1_mean'], '.2f')} | "
            f"{num(stats['load1_level'], '.2f')} | {stats['load_n']} | {busy} | "
            f"{len(raw.get('runs', []))} | "
            f"{raw.get('power_start', '?')}/{raw.get('power_end', '?')} |"
        )
    return "\n".join(lines) + "\n"


def emit(verdict: dict) -> None:
    say(
        f"verdict status={verdict['status']} mode={verdict['mode']} "
        f"evidence={str(verdict['evidence']).lower()} "
        f"delta_p95={verdict['delta_p95']} worst={verdict['worst_metric']} "
        f"delta_load1={verdict['delta_load1']} noise_p95={verdict['noise_p95']} "
        f"noise_load1={verdict['noise_load1']} raw={verdict['raw_comparison']}"
    )
    for reason in verdict["reasons"]:
        say(f"reason {reason}")
    if not verdict["evidence"]:
        for problem in verdict["evidence_problems"]:
            say(f"not_evidence {problem}")
        say(NOT_EVIDENCE)


def command_run(args: argparse.Namespace) -> int:
    if args.on_command and args.on_command[0] == "--":
        args.on_command = args.on_command[1:]
    states, pattern_refusal = parse_pattern(args.pattern)
    refusals = run_refusals(args, states)
    if pattern_refusal:
        refusals.insert(0, pattern_refusal)
    source = args.source.resolve()
    stamp = dt.datetime.now(dt.UTC).strftime("%Y%m%dT%H%M%SZ")
    workdir = (args.workdir or source.parent / f"s2-budget-workload-{stamp}").resolve()
    if source.is_dir():
        refusal = workdir_refusal(source, workdir)
        if refusal:
            refusals.append(refusal)
    if not shutil.which("git"):
        refusals.append("git is not on PATH")
    if search_command(workdir / "tree") is None:
        refusals.append("neither rg nor grep is on PATH")
    if args.evidence and not refusals:
        power = r23_ab.power_source()
        load1 = read_load1(load_source())
        if power != "ac" or load1 >= LOAD_LIMIT:
            refusals.append(
                f"evidence needs AC power and load1 < {LOAD_LIMIT} at the start "
                f"(R-N81): power={power} load1={load1:.2f}"
            )
    if refusals:
        for refusal in refusals:
            say(f"refused reason={refusal!r}")
        return 2
    if not args.aa and not any("{priority}" in token for token in args.on_command):
        say(
            "note the ON command has no {priority} placeholder; its own default applies"
        )
    args.out.resolve().mkdir()
    say(
        f"start workdir={workdir} out={args.out.resolve()} pattern={args.pattern} "
        f"window_seconds={args.window_seconds} evidence={str(args.evidence).lower()}"
    )
    try:
        trace = session(args, states, workdir)
    except (OSError, subprocess.SubprocessError, sqlite3.Error) as exc:
        say(f"aborted reason={f'{type(exc).__name__}: {exc}'!r}")
        return 3
    finally:
        if workdir.exists() and not args.keep_workdir:
            shutil.rmtree(workdir)
    verdict = analyze(trace)
    trace["verdict"] = verdict
    out_dir = args.out.resolve()
    (out_dir / "trace.json").write_text(json.dumps(trace, indent=1) + "\n")
    (out_dir / "summary.md").write_text(render_summary(trace, verdict))
    emit(verdict)
    say(f"trace={out_dir / 'trace.json'} summary={out_dir / 'summary.md'}")
    return 0


def command_analyze(args: argparse.Namespace) -> int:
    trace = json.loads(args.trace.read_text())
    overrides = {
        key: getattr(args, key)
        for key in DEFAULT_GATE
        if getattr(args, key, None) is not None
    }
    verdict = analyze(trace, overrides)
    print(json.dumps(verdict, indent=1))
    emit(verdict)
    return 0


def command_selftest(_args: argparse.Namespace) -> int:
    suite = unittest.defaultTestLoader.discover(
        str(HERE), pattern="test_s2_budget.py", top_level_dir=str(HERE)
    )
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    return 0 if result.wasSuccessful() else 1


def gate_arguments(parser: argparse.ArgumentParser, defaults: bool) -> None:
    def default(key: str):
        return DEFAULT_GATE[key] if defaults else None

    parser.add_argument("--budget-p95", type=float, default=default("budget_p95"))
    parser.add_argument("--budget-load1", type=float, default=default("budget_load1"))
    parser.add_argument(
        "--gate-metric", choices=("step", "each-op"), default=default("gate_metric")
    )
    parser.add_argument(
        "--settle-seconds", type=float, default=default("settle_seconds")
    )
    parser.add_argument("--min-samples", type=int, default=default("min_samples"))
    parser.add_argument(
        "--min-window-samples", type=int, default=default("min_window_samples")
    )
    parser.add_argument("--min-on-busy", type=float, default=default("min_on_busy"))


def parser() -> argparse.ArgumentParser:
    top = argparse.ArgumentParser(
        description="S2 measured-budget instrument (OI-1003-Q34)."
    )
    sub = top.add_subparsers(dest="command", required=True)
    run = sub.add_parser("run", help="measure the budget (or --aa noise)")
    run.add_argument("--source", type=Path, required=True)
    run.add_argument("--out", type=Path, required=True)
    run.add_argument("--workdir", type=Path)
    run.add_argument("--keep-workdir", action="store_true")
    run.add_argument("--pattern", default="off,on,off,on,off")
    run.add_argument("--window-seconds", type=float, default=EVIDENCE_MIN_WINDOW)
    run.add_argument(
        "--priority", choices=("background", "normal"), default="background"
    )
    run.add_argument("--no-repeat", dest="repeat", action="store_false")
    run.add_argument("--max-runs", type=int, default=DEFAULT_MAX_RUNS)
    run.add_argument("--max-overrun-seconds", type=float, default=1800.0)
    run.add_argument("--aa", action="store_true")
    run.add_argument("--evidence", action="store_true")
    run.add_argument("--coordinator-quiet", action="store_true")
    run.add_argument("--label", default="")
    gate_arguments(run, defaults=True)
    run.add_argument("on_command", nargs=argparse.REMAINDER)
    run.set_defaults(handler=command_run)
    again = sub.add_parser("analyze", help="re-derive the verdict from a trace")
    again.add_argument("trace", type=Path)
    gate_arguments(again, defaults=False)
    again.set_defaults(handler=command_analyze)
    selftest = sub.add_parser("selftest", help="run test_s2_budget.py")
    selftest.set_defaults(handler=command_selftest)
    return top


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    return args.handler(args)


if __name__ == "__main__":
    raise SystemExit(main())
