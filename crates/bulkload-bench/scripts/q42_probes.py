#!/usr/bin/env python3
"""Q42 lane L3: large-ref hazard probes (OI-1003-Q42..Q45). Stdlib only.

Label: informational, sting, ungated. Nothing here is a gate sample: R-N81's
power and load rule does not gate it, but every record still carries the
host, load1 before and after, and the priority class it ran at.

It collects the sting-side "evidence that would change it" for the carry_v2
deletion while v2 is not compiled into any verb path (OI-1003-Q15 froze it).
It runs a release `bulkload-agent` built from main against synthetic
repositories in a private scratch directory, and only there.

What it measures:

1. header: v1 capture and bundle header bytes against the 16 MiB cap that
   the agent's four header readers enforce: `shared::prerequisites`,
   `shared::pack_object_count`, `shared::write_with_prerequisites` and
   `chain::advertised`. Each reads lines of at most 1 MiB and refuses
   GIT_INVENTORY_MALFORMED once the header passes 16 MiB. The header is
   modelled from the source inventory through the agent's own ref mapping
   (`capture_refs`: a canonical `refs/carry/v1/<src>/<64 hex>/<suffix>`
   becomes `refs/carry-export/union/v1/...`, anything else
   `refs/carry-export/<name>`), and measured from the bundles the agent
   wrote or kept for diagnosis.
2. walk: wall and CPU of the chained capture's edge-aggressive walk, the
   `rev-list --objects-edge-aggressive --all --stdin` child of
   `write_excluding_tip_trees`, replayed in a private-like repository: a
   bare repository whose alternates name the source's objects, holding the
   source's refs under the export mapping plus one new parentless worktree
   commit. The exclusions are the source-held tips (`chain::source_held_tips`).
   `--objects-edge` over the same request is the control, and
   `pack-objects` over the listed objects gives the pack the walk feeds.
   The agent's private repository keeps its refs loose: `capture_refs`
   creates them with one `update-ref --stdin -z` transaction and nothing
   packs them. So `loose` is the agent's storage and the default; `packed`
   (run A's replay, which ran `pack-refs --all`) and `reftable` are the
   comparison. Each storage also times the ref-creating transaction and a
   `for-each-ref` scan.
   Inside a real agent pass, `run` attributes CPU to each Git child through
   a PATH shim (bash `times`), so the walk is also timed where the agent
   runs it.
3. refusal: whether the agent refuses an over-cap inventory as a typed value
   (GIT_INVENTORY_MALFORMED or another typed code). A panic (exit 101 or
   "panicked at"), a bare IO and a FRAME_CODEC are not typed (S4).
4. allowance: the inputs of Q45's byte allowance M = c + a*refs + b*trees,
   from a chained capture bundle decomposed object by object
   (`verify-pack -v` after `bundle unbundle` into a scratch repository).
   a*refs is measured from the header lines whose names map from a source
   ref; the other ref lines are the capture's metadata refs. `check` is a
   pack and header identity (bundle = header lines + 32 B + the in-pack
   sizes), true of any well-formed bundle, not a validation of the terms;
   `a_model_delta` (measured less modelled a*refs) is the model check.
5. spawn: `spawn-micro` times `shared::prerequisite_commits`' two Git
   children per base head (`cat-file -e`, `rev-parse --verify`).

Corpora (`corpus`): one synthetic repository per shape. `carry` mirrors
#48's blahaj inventory: refs in canonical `refs/carry/v1/neo/<digest>/...`
snapshot namespaces, each re-copying the native refs as they stood, over
few distinct oids. `compact` has the same counts and targets under short
names, so its header stays under the cap and the chained path can run. A
`--header-target` corpus sizes the carry inventory to land a plan-base
item's header just over the cap (the cap-window probe).

Every `run` record carries `script`: this file's sha256 and Git blob id,
the commit that last changed it, and whether the file matched HEAD, so each
result names the revision that produced it.

Subcommands:
  corpus OUT [--shape carry|compact] [--refs N] [--oids N] [--namespaces N]
             [--dirs N] [--files-per-dir N] [--seed N] [--header-target B]
             [--cover]
  header REPO                    model the v1 header from REPO's inventory
                                 (read-only: for-each-ref and rev-parse)
  bundle-header BUNDLE           measure a bundle's header as the agent reads it
  walk REPO --work DIR [--reps N] [--storage loose|packed|reftable ...]
       [--spawn-pairs N]
  spawn-micro REPO [--pairs N]   prerequisite_commits' per-head children
  run --agent BIN --work DIR --out JSON [--suite v1|review] [--skip-large]
      [--only NAME ...]
  summarize RESULTS.json [...]   Q45's allowance terms and the doc's figures
  selftest                       run test_q42_probes.py
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import resource
import shutil
import statistics
import subprocess
import sys
import time
import unittest
from collections import Counter
from pathlib import Path

SCHEMA = "bulkload.q42-probes.v1"
LABEL = "informational, sting, ungated"
HERE = Path(__file__).resolve().parent

# The agent's header bounds (git_carry/shared.rs, git_carry/chain.rs).
HEADER_CAP = 16 * 1024 * 1024
LINE_CAP = 1024 * 1024
# A plan base's prerequisite line: `-<oid> shared base\n`.
PREREQ_SUFFIX = " shared base"
CARRY_PREFIX = "refs/carry/v1/"
EXPORT_PREFIX = "refs/carry-export/"
UNION_PREFIX = "refs/carry-export/union/v1/"
SIGNATURES = (b"# v2 git bundle\n", b"# v3 git bundle\n")
# `-c` overrides every agent Git child runs with that change what a walk or a
# pack does (git_env::CONFIG, less the hook override: these scratch
# repositories are made with --template= and hold no hooks).
AGENT_GIT_CONFIG = (
    "core.fsmonitor=false",
    "gc.auto=0",
    "maintenance.auto=false",
    "pack.threads=2",
    "pack.windowMemory=64m",
)
# The agent's capture commits are dated 2000-01-01; the corpus uses it too.
FIXED_DATE = 946684800
WORDS = (
    "alder",
    "birch",
    "cedar",
    "delta",
    "ember",
    "fjord",
    "grove",
    "heron",
    "islet",
    "juniper",
    "kestrel",
    "larch",
)
# The synthetic corpora's source slug. It is 3 B long; a real inventory's
# slugs vary (`header` reports them), and a canonical carry ref's header line
# is 135 B + slug + suffix (SHA-1).
SOURCE_SLUG = "neo"
UNION_LINE_FIXED = 40 + 1 + len(UNION_PREFIX) + 1 + 64 + 1 + 1
REF_STORAGES = ("loose", "packed", "reftable")
CODE = re.compile(r"\b[A-Z][A-Z0-9]*(?:_[A-Z0-9]+)+\b|\bIO\b")
UNTYPED = ("IO", "FRAME_CODEC")


class ProbeError(RuntimeError):
    """A probe step failed outside the agent: the corpus or a git child."""


# ---- git and measured children ----------------------------------------------


def git_env() -> dict[str, str]:
    """No inherited GIT_* variable, no system or global configuration."""
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
    env.update(
        {
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_CONFIG_GLOBAL": os.devnull,
            "GIT_TERMINAL_PROMPT": "0",
            "GIT_OPTIONAL_LOCKS": "0",
            "LC_ALL": "C",
            "GIT_AUTHOR_NAME": "Q42 probe",
            "GIT_AUTHOR_EMAIL": "q42@localhost",
            "GIT_COMMITTER_NAME": "Q42 probe",
            "GIT_COMMITTER_EMAIL": "q42@localhost",
            "GIT_AUTHOR_DATE": f"{FIXED_DATE} +0000",
            "GIT_COMMITTER_DATE": f"{FIXED_DATE} +0000",
        }
    )
    return env


def git_argv(repo: Path, *args: str) -> list[str]:
    argv = ["git", "--no-optional-locks"]
    for value in AGENT_GIT_CONFIG:
        argv += ["-c", value]
    return [*argv, "-C", str(repo), *args]


def git(repo: Path, *args: str, data: bytes | None = None) -> bytes:
    done = subprocess.run(
        git_argv(repo, *args), input=data, capture_output=True, env=git_env()
    )
    if done.returncode != 0:
        tail = done.stderr.decode(errors="replace").strip()[-400:]
        raise ProbeError(f"git {' '.join(args[:3])} exited {done.returncode}: {tail}")
    return done.stdout


def measured(
    argv: list[str],
    *,
    data: bytes | None = None,
    env: dict[str, str] | None = None,
    keep_stdout: bool = True,
) -> dict:
    """Run one child to completion; wall from a monotonic clock, CPU from the
    RUSAGE_CHILDREN delta (the probe runs one child at a time, so the delta is
    this child and every descendant it reaped)."""
    before = resource.getrusage(resource.RUSAGE_CHILDREN)
    start = time.monotonic_ns()
    done = subprocess.run(
        argv,
        input=data,
        stdout=subprocess.PIPE if keep_stdout else subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        env=env if env is not None else git_env(),
    )
    wall_ns = time.monotonic_ns() - start
    after = resource.getrusage(resource.RUSAGE_CHILDREN)
    user = after.ru_utime - before.ru_utime
    system = after.ru_stime - before.ru_stime
    return {
        "exit": done.returncode,
        "wall_s": round(wall_ns / 1e9, 4),
        "cpu_user_s": round(user, 4),
        "cpu_sys_s": round(system, 4),
        "cpu_s": round(user + system, 4),
        "stdout": done.stdout if keep_stdout else b"",
        "stderr": done.stderr,
    }


def load1() -> float | None:
    try:
        return float(Path("/proc/loadavg").read_text().split()[0])
    except (OSError, ValueError, IndexError):
        return os.getloadavg()[0] if hasattr(os, "getloadavg") else None


def cpu_pressure() -> float | None:
    """PSI `some avg10` for CPU: the share of the last 10 s in which some
    runnable task waited for a CPU (Linux only)."""
    try:
        first = Path("/proc/pressure/cpu").read_text().splitlines()[0]
        return float(dict(f.split("=") for f in first.split()[1:])["avg10"])
    except (OSError, ValueError, IndexError, KeyError):
        return None


def host_record() -> dict:
    nice = os.getpriority(os.PRIO_PROCESS, 0)
    git_version = subprocess.run(
        ["git", "--version"], capture_output=True, text=True, env=git_env()
    ).stdout.strip()
    return {
        "host": platform.node(),
        "kernel": platform.release(),
        "cpus": os.cpu_count(),
        "python": platform.python_version(),
        "git": git_version,
        "probe_nice": nice,
        "label": LABEL,
    }


def script_provenance() -> dict:
    """Which revision of this script produced a result: its sha256 and Git
    blob id, the commit that last changed it, and whether it matched HEAD."""
    me = Path(__file__).resolve()
    tests = HERE / "test_q42_probes.py"
    record: dict = {
        "file": me.name,
        "sha256": hashlib.sha256(me.read_bytes()).hexdigest(),
        "selftest_sha256": (
            hashlib.sha256(tests.read_bytes()).hexdigest() if tests.is_file() else None
        ),
    }
    try:
        blob = git(HERE, "hash-object", "--", me.name).decode().strip()
        head = git(HERE, "rev-parse", "HEAD").decode().strip()
        committed = git(HERE, "rev-parse", f"HEAD:./{me.name}").decode().strip()
        last = git(HERE, "log", "-1", "--format=%H", "--", me.name).decode().strip()
    except ProbeError as error:
        record["git"] = f"unavailable: {error}"
        return record
    record.update(
        {
            "blob": blob,
            "head": head,
            "last_commit": last or None,
            "matches_head": blob == committed,
        }
    )
    return record


def spread(values: list[float]) -> dict:
    """Median, minimum, maximum and count of repeated samples."""
    if not values:
        return {"n": 0}
    return {
        "n": len(values),
        "median": round(statistics.median(values), 4),
        "min": round(min(values), 4),
        "max": round(max(values), 4),
    }


def linear_fit(points: list[tuple[float, float]]) -> dict | None:
    """Least squares y = intercept + slope * x, with r^2."""
    if len(points) < 2:
        return None
    xs = [x for x, _ in points]
    ys = [y for _, y in points]
    mx, my = statistics.fmean(xs), statistics.fmean(ys)
    sxx = sum((x - mx) ** 2 for x in xs)
    if sxx == 0:
        return None
    slope = sum((x - mx) * (y - my) for x, y in points) / sxx
    intercept = my - slope * mx
    ss_tot = sum((y - my) ** 2 for y in ys)
    ss_res = sum((y - intercept - slope * x) ** 2 for x, y in points)
    return {
        "n": len(points),
        "distinct_x": len(set(xs)),
        "slope": slope,
        "intercept": intercept,
        "r2": 1 - ss_res / ss_tot if ss_tot else 1.0,
    }


# ---- the agent's ref mapping and header rules ---------------------------------


def source_slug(value: str) -> bool:
    """git_carry.rs `source_slug`."""
    return (
        0 < len(value) <= 64
        and value.isascii()
        and all(c.isalnum() or c in "-_" for c in value)
    )


def is_oid(value: str) -> bool:
    return len(value) in (40, 64) and all(c in "0123456789abcdefABCDEF" for c in value)


def canonical_tail(tail: str) -> bool:
    """git_carry.rs `canonical_tail`: `<source>/<64 hex>/<suffix>`."""
    parts = tail.split("/", 2)
    while len(parts) < 3:
        parts.append("")
    source, snapshot, suffix = parts
    return (
        source_slug(source)
        and len(snapshot) == 64
        and is_oid(snapshot)
        and suffix != ""
    )


def exported_name(name: str) -> str:
    """git_carry.rs `capture_refs`: the private name a source ref is carried as."""
    if name.startswith(CARRY_PREFIX) and canonical_tail(name[len(CARRY_PREFIX) :]):
        return UNION_PREFIX + name[len(CARRY_PREFIX) :]
    return EXPORT_PREFIX + name


def ref_line_bytes(oid: str, name: str) -> int:
    """One `<oid> <refname>\\n` header line."""
    return len(oid) + 1 + len(name.encode()) + 1


def prereq_line_bytes(oid: str) -> int:
    return 1 + len(oid) + len(PREREQ_SUFFIX) + 1


def refs_at_cap(mean_line: float, fixed: int) -> int:
    """Refs a header holds before the cap, at `mean_line` bytes per ref line
    and `fixed` bytes of everything else."""
    if mean_line <= 0:
        raise ValueError("mean_line must be positive")
    return max(0, int((HEADER_CAP - fixed) // mean_line))


def read_header(path: Path, limit: int = 1 << 30) -> dict:
    """Measure a bundle header the way the agent's readers read it.

    `agent_reader` is what `shared::prerequisites` (and, but for its
    signature check, the other three readers) answers: `ok`, or the refusal
    with its cause. Unlike the agent this keeps counting past the cap (up to
    `limit`), so an over-cap header's full size is known."""
    consumed = 0
    verdict = None
    cause = None
    signature = None
    capabilities = prereqs = prereq_bytes = refs = ref_bytes = longest = 0
    with open(path, "rb") as handle:
        first = True
        while True:
            line = handle.readline(LINE_CAP)
            count = len(line)
            if first:
                first = False
                signature = line.decode(errors="replace").strip()
                if line not in SIGNATURES:
                    verdict, cause = "GIT_INVENTORY_MALFORMED", "signature"
                    break
            consumed += count
            longest = max(longest, count)
            if count == 0 or not line.endswith(b"\n"):
                verdict = verdict or "GIT_INVENTORY_MALFORMED"
                cause = cause or ("eof" if count == 0 else "line_over_1MiB")
                break
            if consumed > HEADER_CAP and verdict is None:
                verdict, cause = "GIT_INVENTORY_MALFORMED", "header_over_16MiB"
            if line == b"\n":
                break
            if line.startswith(b"@"):
                capabilities += 1
            elif line.startswith(b"-"):
                prereqs += 1
                prereq_bytes += count
            elif not line.startswith(b"# v"):
                refs += 1
                ref_bytes += count
            if consumed > limit:
                cause = cause or "probe_limit"
                break
    return {
        "path": str(path),
        "header_bytes": consumed,
        "cap": HEADER_CAP,
        "over_cap": consumed > HEADER_CAP,
        "headroom_bytes": HEADER_CAP - consumed,
        "signature": signature,
        "capability_lines": capabilities,
        "prerequisite_lines": prereqs,
        "prerequisite_bytes": prereq_bytes,
        "ref_lines": refs,
        "ref_bytes": ref_bytes,
        "mean_ref_line": round(ref_bytes / refs, 3) if refs else None,
        "longest_line": longest,
        "agent_reader": verdict or "ok",
        "agent_reader_cause": cause,
        "bundle_bytes": path.stat().st_size,
    }


def inventory(repo: Path) -> list[tuple[str, str]]:
    out = git(repo, "for-each-ref", "--format=%(objectname) %(refname)")
    rows = []
    for line in out.decode().splitlines():
        oid, _, name = line.partition(" ")
        rows.append((oid, name))
    return rows


def typed_inventory(repo: Path) -> list[tuple[str, str, str, str]]:
    """(oid, object type, the commit it is or peels to (one level) or "",
    refname) for every ref."""
    out = git(
        repo,
        "for-each-ref",
        "--format=%(objectname) %(objecttype) %(*objectname) %(*objecttype) %(refname)",
    )
    rows = []
    for line in out.decode().splitlines():
        oid, kind, peeled, peeled_kind, name = line.split(" ", 4)
        commit = oid if kind == "commit" else peeled if peeled_kind == "commit" else ""
        rows.append((oid, kind, commit, name))
    return rows


def inventory_shape(rows: list[tuple[str, str, str, str]]) -> dict:
    """The inventory's shape as the header sees it: canonical carry refs by
    source slug, their suffix lengths, the other refs, and target types."""
    slugs: Counter = Counter()
    suffixes: list[int] = []
    union_lines = formula = 0
    other_lines: list[int] = []
    for oid, _kind, _peeled, name in rows:
        exported = exported_name(name)
        line = ref_line_bytes(oid, exported)
        if exported.startswith(UNION_PREFIX):
            slug, _, rest = name[len(CARRY_PREFIX) :].partition("/")
            suffix = rest.partition("/")[2]
            slugs[slug] += 1
            suffixes.append(len(suffix.encode()))
            union_lines += line
            formula += (
                UNION_LINE_FIXED + len(oid) - 40 + len(slug.encode()) + suffixes[-1]
            )
        else:
            other_lines.append(line)
    return {
        "union_mapped_refs": len(suffixes),
        "union_line_bytes": union_lines,
        # 0 when every union line is 135 B + slug + suffix (+24 B for SHA-256).
        "union_formula_check": union_lines - formula,
        "slugs": {
            slug: {"refs": count, "slug_bytes": len(slug.encode())}
            for slug, count in slugs.most_common(8)
        },
        "distinct_slugs": len(slugs),
        "suffix_bytes": spread([float(n) for n in suffixes]),
        "suffix_bytes_mean": (
            round(statistics.fmean(suffixes), 3) if suffixes else None
        ),
        "other_refs": len(other_lines),
        "other_mean_line": (
            round(statistics.fmean(other_lines), 3) if other_lines else None
        ),
        "target_types": dict(Counter(kind for _, kind, _, _ in rows)),
    }


def model_header(repo: Path) -> dict:
    """The v1 header's ref term for REPO, through the agent's mapping.

    Read-only: one `for-each-ref` and one `rev-parse`, with optional locks
    off, so it may run against a real repository."""
    typed = typed_inventory(repo)
    rows = [(oid, name) for oid, _, _, name in typed]
    oids = {oid for oid, _ in rows}
    tip_commits = {commit for _, _, commit, _ in typed if commit}
    exported = [(oid, exported_name(name)) for oid, name in rows]
    union = sum(1 for _, name in exported if name.startswith(UNION_PREFIX))
    line_bytes = [ref_line_bytes(oid, name) for oid, name in exported]
    total = sum(line_bytes)
    mean = total / len(line_bytes) if line_bytes else 0.0
    fmt = git(repo, "rev-parse", "--show-object-format").decode().strip()
    signature = (
        16 if fmt == "sha1" else len(b"# v3 git bundle\n@object-format=sha256\n")
    )
    fixed = signature + 1
    return {
        "repo": str(repo),
        "measured_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "object_format": fmt,
        "refs": len(rows),
        "distinct_oids": len(oids),
        "union_mapped_refs": union,
        "ref_line_bytes": total,
        "mean_ref_line": round(mean, 3),
        "min_ref_line": min(line_bytes) if line_bytes else 0,
        "max_ref_line": max(line_bytes) if line_bytes else 0,
        "signature_and_blank_bytes": fixed,
        "modelled_header_bytes": total + fixed,
        "over_cap": total + fixed > HEADER_CAP,
        "headroom_bytes": HEADER_CAP - total - fixed,
        "refs_at_cap_at_this_mean": refs_at_cap(mean, fixed) if mean else None,
        "prerequisite_bytes_if_all_tips": sum(prereq_line_bytes(o) for o in oids),
        # Distinct tips that are or peel (one level) to a commit: the
        # prerequisite lines a chained capture of this inventory can declare.
        "distinct_tip_commits": len(tip_commits),
        "prerequisite_bytes_if_all_tip_commits": sum(
            prereq_line_bytes(o) for o in tip_commits
        ),
        "shape": inventory_shape(typed),
    }


# ---- the corpus ---------------------------------------------------------------


def split_refs(refs: int, oids: int, namespaces: int, cover: bool = False) -> dict:
    """How `refs` refs and `oids` distinct targets split into a live native
    set plus `namespaces` snapshot namespaces of native copies and 4 metadata
    refs each (head, staged, worktree, filesystem-v1; 3 distinct parentless
    commits each, `head` names a native commit).

    The live set and each namespace hold about the same number of native
    refs. With `cover`, a count too small for that (the live set must name
    every native commit) lifts the live set to cover them and shrinks the
    namespaces instead, so a ref-count series can hold tips and namespaces
    fixed; a count that already splits is unchanged."""
    if namespaces < 1 or refs < 5 * namespaces + 2:
        raise ValueError("too few refs for the namespaces")
    native = oids - 3 * namespaces
    if native < 2:
        raise ValueError("oids must exceed 3 * namespaces + 1")
    per_namespace = (refs - 4 * namespaces) // (namespaces + 1)
    live = refs - 4 * namespaces - namespaces * per_namespace
    if cover and live - 1 < native:
        per_namespace = (refs - 4 * namespaces - native - 1) // namespaces
        if per_namespace < 0:
            raise ValueError("too few refs to name every native commit")
        live = refs - 4 * namespaces - namespaces * per_namespace
    if live - 1 < native:
        raise ValueError("the live native set must cover every native commit")
    return {
        "refs": refs,
        "oids": oids,
        "namespaces": namespaces,
        "native_commits": native,
        "metadata_commits": 3 * namespaces,
        "live_native_refs": live,
        "native_refs_per_namespace": per_namespace,
        "metadata_refs_per_namespace": 4,
    }


def native_names(count: int, shape: str) -> list[str]:
    names = ["refs/heads/main"]
    for j in range(1, count):
        if shape == "compact":
            names.append(f"refs/heads/b{j}")
            continue
        word = WORDS[j % len(WORDS)]
        kind = j % 20
        if kind < 10:
            names.append(f"refs/heads/feat/{word}-{j:04d}-20261004")
        elif kind < 17:
            names.append(f"refs/remotes/origin/feat/{word}-{j:04d}-20261004")
        else:
            names.append(f"refs/tags/v0.{j}.0")
    return names


def namespace_digest(seed: int, k: int) -> str:
    return hashlib.sha256(f"q42 seed={seed} snapshot={k}".encode()).hexdigest()


def plan_refs(split: dict, shape: str, seed: int) -> list[tuple[str, tuple[str, int]]]:
    """Every ref as (name, target); a target is ("native", i) or
    ("meta-<kind>", k)."""
    native = split["native_commits"]
    live = split["live_native_refs"]
    per = split["native_refs_per_namespace"]
    names = native_names(max(live, per), shape)
    rows: list[tuple[str, tuple[str, int]]] = [
        ("refs/heads/main", ("native", native - 1))
    ]
    for j in range(1, live):
        rows.append((names[j], ("native", ((j - 1) * native) // (live - 1))))
    meta_suffix = {
        "carry": ("head", "staged", "worktree", "filesystem-v1"),
        "compact": ("h", "s", "w", "f"),
    }[shape]
    for k in range(split["namespaces"]):
        t = ((k + 1) * (native - 1)) // split["namespaces"]
        if shape == "carry":
            prefix = f"{CARRY_PREFIX}{SOURCE_SLUG}/{namespace_digest(seed, k)}/"
        else:
            prefix = f"refs/s/{k}/"
        for j in range(per):
            target = t if j == 0 else (j * (t + 1)) // per
            suffix = names[j] if shape == "carry" else str(j)
            rows.append((prefix + suffix, ("native", target)))
        rows.append((prefix + meta_suffix[0], ("native", t)))
        rows.append((prefix + meta_suffix[1], ("meta-staged", k)))
        rows.append((prefix + meta_suffix[2], ("meta-worktree", k)))
        rows.append((prefix + meta_suffix[3], ("meta-filesystem", k)))
    return rows


def planned_header_bytes(
    rows: list[tuple[str, tuple[str, int]]], oid_len: int = 40
) -> int:
    return sum(oid_len + 1 + len(exported_name(name).encode()) + 1 for name, _ in rows)


def fit_refs_to_header(
    target: int, oids: int, namespaces: int, shape: str, seed: int
) -> int:
    """Largest ref count whose exported ref lines stay within `target` bytes."""
    lo, hi = 5 * namespaces + 2, 1
    while True:
        hi *= 2
        try:
            split = split_refs(hi, oids, namespaces)
        except ValueError:
            continue
        if planned_header_bytes(plan_refs(split, shape, seed)) > target:
            break
    while lo < hi - 1:
        mid = (lo + hi) // 2
        try:
            fits = (
                planned_header_bytes(
                    plan_refs(split_refs(mid, oids, namespaces), shape, seed)
                )
                <= target
            )
        except ValueError:
            # split_refs refuses only a count too small for the namespaces or
            # the native commits, so an invalid count is below the answer.
            fits = True
        if fits:
            lo = mid
        else:
            hi = mid
    return lo


def file_bytes(seed: int, d: int, f: int, version: int) -> bytes:
    head = f"q42 seed={seed} dir={d} file={f} version={version}\n".encode()
    filler = hashlib.sha256(head).hexdigest().encode()
    return head + filler + b"\n"


def fast_import_stream(
    native: int, dirs: int, files_per_dir: int, seed: int
) -> tuple[bytes, list[tuple[int, int]]]:
    """Native history: one root commit with every file, then one commit per
    changed file, all on refs/heads/main with marks :1..:native."""
    import random

    rng = random.Random(seed)
    out = bytearray()
    changes: list[tuple[int, int]] = []
    for i in range(native):
        message = f"q42 native {i}\n".encode()
        out += b"commit refs/heads/main\n"
        out += f"mark :{i + 1}\n".encode()
        out += f"committer Q42 probe <q42@localhost> {FIXED_DATE + i} +0000\n".encode()
        out += f"data {len(message)}\n".encode() + message
        if i == 0:
            for d in range(dirs):
                for f in range(files_per_dir):
                    body = file_bytes(seed, d, f, 0)
                    out += f"M 100644 inline d{d:03d}/f{f:03d}.txt\n".encode()
                    out += f"data {len(body)}\n".encode() + body
        else:
            d, f = rng.randrange(dirs), rng.randrange(files_per_dir)
            changes.append((d, f))
            body = file_bytes(seed, d, f, i)
            out += f"M 100644 inline d{d:03d}/f{f:03d}.txt\n".encode()
            out += f"data {len(body)}\n".encode() + body
        out += b"\n"
    out += b"done\n"
    return bytes(out), changes


def build_corpus(
    out: Path,
    *,
    shape: str = "carry",
    refs: int = 119_761,
    oids: int = 1_032,
    namespaces: int = 157,
    dirs: int = 64,
    files_per_dir: int = 64,
    seed: int = 42,
    header_target: int | None = None,
    linked_worktree: bool = False,
    cover: bool = False,
) -> dict:
    """Build one synthetic source repository at `out` (which must not exist).

    The source's own refs are packed (`pack-refs --all`), as a long-lived
    repository's usually are; the agent's private repository is not."""
    if shape not in ("carry", "compact"):
        raise ValueError(f"unknown shape {shape}")
    if out.exists():
        raise ProbeError(f"{out} already exists")
    started = time.monotonic()
    if header_target is not None:
        refs = fit_refs_to_header(header_target, oids, namespaces, shape, seed)
    split = split_refs(refs, oids, namespaces, cover=cover)
    rows = plan_refs(split, shape, seed)
    out.parent.mkdir(parents=True, exist_ok=True)
    git(out.parent, "init", "--quiet", "--template=", "-b", "main", str(out))
    stream, _ = fast_import_stream(split["native_commits"], dirs, files_per_dir, seed)
    marks = out / ".git" / "q42-marks"
    git(out, "fast-import", "--quiet", f"--export-marks={marks}", data=stream)
    by_mark = {}
    for line in marks.read_text().splitlines():
        mark, _, oid = line.partition(" ")
        by_mark[int(mark[1:])] = oid
    marks.unlink()
    commits = [by_mark[i + 1] for i in range(split["native_commits"])]
    meta: dict[tuple[str, int], str] = {}
    for k in range(namespaces):
        t = ((k + 1) * (split["native_commits"] - 1)) // namespaces
        tree = git(out, "rev-parse", f"{commits[t]}^{{tree}}").decode().strip()
        meta[("meta-staged", k)] = (
            git(
                out, "commit-tree", tree, "-m", f"bulkload staged tree q42 snapshot {k}"
            )
            .decode()
            .strip()
        )
        extra = git(
            out, "hash-object", "-w", "--stdin", data=f"untracked {k}\n".encode()
        )
        listing = (
            git(out, "ls-tree", tree)
            + b"100644 blob "
            + extra.strip()
            + b"\tq42-untracked\n"
        )
        worktree = git(out, "mktree", data=listing).decode().strip()
        meta[("meta-worktree", k)] = (
            git(
                out,
                "commit-tree",
                worktree,
                "-m",
                f"bulkload worktree q42 snapshot {k}",
            )
            .decode()
            .strip()
        )
        rows_blob = git(
            out, "hash-object", "-w", "--stdin", data=f"filesystem rows {k}\n".encode()
        )
        fs_tree = git(
            out, "mktree", data=b"100644 blob " + rows_blob.strip() + b"\tvalue\n"
        )
        meta[("meta-filesystem", k)] = (
            git(
                out, "commit-tree", fs_tree.decode().strip(), "-m", f"filesystem-v1 {k}"
            )
            .decode()
            .strip()
        )
    commands = bytearray()
    targets = set()
    for name, (kind, index) in rows:
        oid = commits[index] if kind == "native" else meta[(kind, index)]
        targets.add(oid)
        if name == "refs/heads/main":
            continue
        commands += b"create " + name.encode() + b"\0" + oid.encode() + b"\0"
    git(out, "update-ref", "--stdin", "-z", data=bytes(commands))
    git(out, "pack-refs", "--all")
    git(out, "read-tree", "-u", "--reset", "HEAD")
    worktree_path = None
    if linked_worktree:
        worktree_path = out.parent / f"{out.name}-linked"
        git(out, "worktree", "add", "--quiet", "--detach", str(worktree_path), "HEAD")
    model = model_header(out)
    if model["refs"] != split["refs"] or model["distinct_oids"] != oids:
        raise ProbeError(
            f"corpus mismatch: {model['refs']} refs, {model['distinct_oids']} oids"
        )
    counts = git(out, "count-objects", "-v").decode()
    return {
        "path": str(out),
        "linked_worktree": str(worktree_path) if worktree_path else None,
        "shape": shape,
        "seed": seed,
        "dirs": dirs,
        "files_per_dir": files_per_dir,
        "tracked_files": dirs * files_per_dir,
        "header_target": header_target,
        "cover": cover,
        "split": split,
        "model": model,
        "count_objects": dict(
            line.split(": ", 1) for line in counts.splitlines() if ": " in line
        ),
        "build_s": round(time.monotonic() - started, 2),
    }


# ---- the walk replay ------------------------------------------------------------


def source_held_tips(repo: Path) -> list[str]:
    """`chain::source_held_tips` against a prior advertising REPO's whole
    inventory: every distinct tip, peeled to a commit the source holds."""
    tips = sorted({oid for oid, _ in inventory(repo)})
    request = "".join(f"{oid}^{{commit}}\n" for oid in tips).encode()
    answer = git(
        repo, "cat-file", "--batch-check=%(objectname) %(objecttype)", data=request
    )
    held = set()
    for line in answer.decode().splitlines():
        value, _, kind = line.partition(" ")
        if kind == "commit" and is_oid(value):
            held.add(value)
    return sorted(held)


def timed_child(argv: list[str], data: bytes | None = None, what: str = "") -> dict:
    """One measured child that must succeed; its output is discarded."""
    run = measured(argv, data=data, keep_stdout=False)
    stderr = run.pop("stderr")
    run.pop("stdout")
    if run["exit"] != 0:
        tail = stderr.decode(errors="replace").strip()[-400:]
        raise ProbeError(f"{what or argv[-1]} exited {run['exit']}: {tail}")
    return run


def private_like(repo: Path, work: Path, storage: str = "loose") -> dict:
    """A bare repository reading REPO's objects through alternates, holding
    REPO's refs under the export mapping and one new parentless worktree
    commit (HEAD's tree with one changed blob two levels down).

    `storage` is how its refs are kept. `loose` is the agent's: `capture_refs`
    creates every ref in one `update-ref --stdin -z` transaction, which
    writes one loose file per ref, and nothing packs them. `packed` adds
    `pack-refs --all` (run A's replay); `reftable` is `--ref-format=reftable`.
    The ref-creating transaction is timed."""
    if storage not in REF_STORAGES:
        raise ValueError(f"unknown ref storage {storage}")
    private = work / f"private-{storage}.git"
    init = ["init", "--quiet", "--bare", "--template="]
    if storage == "reftable":
        init.append("--ref-format=reftable")
    git(work, *init, str(private))
    objects = git(repo, "rev-parse", "--path-format=absolute", "--git-path", "objects")
    (private / "objects" / "info" / "alternates").write_bytes(objects)
    rows = inventory(repo)
    commands = bytearray()
    for oid, name in rows:
        commands += (
            b"create " + exported_name(name).encode() + b"\0" + oid.encode() + b"\0"
        )
    head = git(repo, "rev-parse", "HEAD").decode().strip()
    commands += b"create " + EXPORT_PREFIX.encode() + b"head\0" + head.encode() + b"\0"
    tree = git(repo, "rev-parse", "HEAD^{tree}").decode().strip()
    listing = git(private, "ls-tree", tree).decode().splitlines()
    first_dir = next(line for line in listing if line.split()[1] == "tree")
    sub_mode_type_oid, _, sub_name = first_dir.partition("\t")
    sub_oid = sub_mode_type_oid.split()[2]
    sub_listing = git(private, "ls-tree", sub_oid).decode().splitlines()
    blob_line = next(line for line in sub_listing if " blob " in line)
    blob_name = blob_line.partition("\t")[2]
    blob = (
        git(private, "hash-object", "-w", "--stdin", data=b"q42 changed seat\n")
        .decode()
        .strip()
    )
    new_sub = [
        f"100644 blob {blob}\t{blob_name}" if line == blob_line else line
        for line in sub_listing
    ]
    sub_new = (
        git(private, "mktree", data=("\n".join(new_sub) + "\n").encode())
        .decode()
        .strip()
    )
    root_new = [
        f"040000 tree {sub_new}\t{sub_name}" if line == first_dir else line
        for line in listing
    ]
    root = (
        git(private, "mktree", data=("\n".join(root_new) + "\n").encode())
        .decode()
        .strip()
    )
    worktree = (
        git(private, "commit-tree", root, "-m", "bulkload worktree q42 replay")
        .decode()
        .strip()
    )
    commands += (
        b"create " + EXPORT_PREFIX.encode() + b"worktree\0" + worktree.encode() + b"\0"
    )
    created = timed_child(
        git_argv(private, "update-ref", "--stdin", "-z"),
        data=bytes(commands),
        what="update-ref --stdin",
    )
    packed = None
    if storage == "packed":
        packed = timed_child(git_argv(private, "pack-refs", "--all"), what="pack-refs")
    loose = (
        sum(1 for path in (private / "refs").rglob("*") if path.is_file())
        if storage != "reftable"
        else 0
    )
    return {
        "private": private,
        "storage": storage,
        "refs": len(rows) + 2,
        "worktree": worktree,
        "loose_ref_files": loose,
        "create_refs": created,
        "pack_refs": packed,
    }


def spawn_micro(repo: Path, pairs: int) -> dict:
    """`shared::prerequisite_commits`' per-head children, `pairs` times: one
    `cat-file -e <oid>` and one `rev-parse --verify <oid>^{commit}` per head,
    in REPO, with the agent's Git flags. The heads are REPO's first `pairs`
    refs in `for-each-ref` order (a base bundle's `list-heads` order)."""
    rows = inventory(repo)
    heads = [oid for oid, _ in rows][:pairs]
    if len(heads) < pairs:
        raise ProbeError(f"{repo} has only {len(heads)} refs")
    before, pressure = load1(), cpu_pressure()
    totals = {"wall_s": 0.0, "cpu_user_s": 0.0, "cpu_sys_s": 0.0}
    for oid in heads:
        for args in (
            ("cat-file", "-e", oid),
            ("rev-parse", "--verify", f"{oid}^{{commit}}"),
        ):
            run = measured(git_argv(repo, *args), keep_stdout=False)
            if run["exit"] != 0 and args[0] == "cat-file":
                raise ProbeError(f"cat-file -e {oid} exited {run['exit']}")
            for key in totals:
                totals[key] += run[key]
    cpu = totals["cpu_user_s"] + totals["cpu_sys_s"]
    return {
        "repo_refs": len(rows),
        "pairs": pairs,
        "wall_ms_per_pair": round(1000 * totals["wall_s"] / pairs, 3),
        "cpu_ms_per_pair": round(1000 * cpu / pairs, 3),
        "user_ms_per_pair": round(1000 * totals["cpu_user_s"] / pairs, 3),
        "sys_ms_per_pair": round(1000 * totals["cpu_sys_s"] / pairs, 3),
        "load1_before": before,
        "load1_after": load1(),
        "psi_cpu_avg10_before": pressure,
    }


def replay_walk(
    repo: Path,
    work: Path,
    reps: int = 3,
    storages: tuple[str, ...] = ("loose",),
    spawn_pairs: int = 0,
) -> dict:
    """Replay `write_excluding_tip_trees`' walk and pack over REPO, once per
    ref storage. Each storage's private repository is removed after it is
    measured (it is this probe's own scratch)."""
    work.mkdir(parents=True, exist_ok=True)
    tips = source_held_tips(repo)
    exclusions = "".join(f"^{oid}\n" for oid in tips).encode()
    record: dict = {
        "excluded_tips": len(tips),
        "exclusion_bytes": len(exclusions),
        "reps": reps,
        "load1_before": load1(),
        "psi_cpu_avg10_before": cpu_pressure(),
        "storages": {},
    }
    for storage in storages:
        built = private_like(repo, work, storage)
        private = built["private"]
        entry: dict = {
            "private_refs": built["refs"],
            "loose_ref_files": built["loose_ref_files"],
            "create_refs": built["create_refs"],
            "pack_refs": built["pack_refs"],
            "load1_before": load1(),
        }
        scans = [
            timed_child(
                git_argv(private, "for-each-ref", "--format=%(objectname) %(refname)"),
                what="for-each-ref",
            )
            for _ in range(reps)
        ]
        entry["for_each_ref"] = {
            "runs": scans,
            "cpu_s": spread([r["cpu_s"] for r in scans]),
            "wall_s": spread([r["wall_s"] for r in scans]),
        }
        for mode in ("--objects-edge-aggressive", "--objects-edge"):
            runs = []
            listed = b""
            for _ in range(reps):
                run = measured(
                    git_argv(private, "rev-list", mode, "--all", "--stdin"),
                    data=exclusions,
                )
                if run["exit"] != 0:
                    raise ProbeError(f"rev-list {mode} exited {run['exit']}")
                listed = run.pop("stdout")
                run.pop("stderr")
                runs.append(run)
            lines = listed.splitlines()
            edges = sum(1 for line in lines if line.startswith(b"-"))
            objects = [line for line in lines if not line.startswith(b"-")]
            packed = measured(
                git_argv(private, "pack-objects", "--stdout", "--delta-base-offset"),
                data=b"\n".join(objects) + b"\n",
            )
            if packed["exit"] != 0:
                raise ProbeError(f"pack-objects after {mode} exited {packed['exit']}")
            entry[mode.lstrip("-")] = {
                "runs": runs,
                "median_wall_s": statistics.median(r["wall_s"] for r in runs),
                "median_cpu_s": statistics.median(r["cpu_s"] for r in runs),
                "cpu_s": spread([r["cpu_s"] for r in runs]),
                "user_s": spread([r["cpu_user_s"] for r in runs]),
                "sys_s": spread([r["cpu_sys_s"] for r in runs]),
                "wall_s": spread([r["wall_s"] for r in runs]),
                "edge_lines": edges,
                "objects_listed": len(objects),
                "listing_bytes": len(listed),
                "pack_bytes": len(packed["stdout"]),
                "pack_wall_s": packed["wall_s"],
                "pack_cpu_s": packed["cpu_s"],
            }
        if spawn_pairs:
            entry["spawn_micro"] = spawn_micro(private, spawn_pairs)
        entry["load1_after"] = load1()
        record["storages"][storage] = entry
        shutil.rmtree(private)
    if spawn_pairs:
        record["spawn_micro_source"] = spawn_micro(repo, spawn_pairs)
    record["load1_after"] = load1()
    return record


# ---- driving the agent ------------------------------------------------------------


def parse_counters(stderr: str) -> dict:
    """The agent's last `counters` key=value line, as a dict."""
    found: dict = {}
    for line in stderr.splitlines():
        if line.startswith("counters ") or line.startswith("counters\t"):
            found = {}
            for token in line.split()[1:]:
                key, sep, value = token.partition("=")
                if sep:
                    found[key] = int(value) if value.isdigit() else value
    return found


def classify(exit_code: int, stdout: str, stderr: str) -> dict:
    """Typed refusal, panic, untyped IO, or success."""
    panicked = exit_code == 101 or "panicked at" in stderr
    refused = [
        line
        for line in stderr.splitlines()
        if line.startswith("bulkload-agent: refused:")
    ]
    reasons = re.findall(r"reason=Some\(\"([^\"]+)\"\)", stdout)
    outcomes = re.findall(r"\boutcome=([a-z-]+)", stdout)
    codes = []
    for text in [*(line.split(":", 2)[2] for line in refused), *reasons]:
        codes += CODE.findall(text)
    untyped = [code for code in codes if code in UNTYPED]
    if panicked:
        kind = "panic"
    elif untyped:
        kind = "untyped"
    elif codes:
        kind = "typed"
    elif exit_code == 0:
        kind = "ok"
    else:
        kind = "unclassified"
    return {
        "exit": exit_code,
        "kind": kind,
        "codes": sorted(set(codes)),
        "outcomes": outcomes,
        "refused_lines": refused,
        "panicked": panicked,
    }


SHIM = """#!{bash}
# q42_probes child attribution: run the real Git, then write one file for
# this child: start and end (EPOCHREALTIME), exit code, its arguments, and
# bash's `times` (the shell's own, then its waited-for children's user and
# sys CPU, in ms). One file per child, so concurrent children never mix.
s=$EPOCHREALTIME
{git} "$@"
rc=$?
e=$EPOCHREALTIME
{{ printf '%s\\t%s\\t%s\\n' "$s" "$e" "$rc"; printf '%s\\n' "$*"; times; }} \\
  > "$Q42_CHILD_DIR/$s-$RANDOM"
exit "$rc"
"""


def child_shim(directory: Path) -> dict | None:
    """A `git` on PATH ahead of the real one that records each Git child the
    agent runs (the agent spawns `git` by name). None without bash or Git."""
    real = shutil.which("git")
    bash = shutil.which("bash")
    if real is None or bash is None:
        return None
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    shim = directory / "git"
    shim.write_text(SHIM.format(bash=bash, git=real))
    shim.chmod(0o700)
    return {"dir": str(directory), "git": real, "bash": bash}


def shim_env(shim: dict, log: Path) -> dict[str, str]:
    """The agent's environment with the shim first on PATH; LOG is a
    directory that receives one file per Git child."""
    env = git_env()
    env["PATH"] = f"{shim['dir']}{os.pathsep}{env.get('PATH', '')}"
    env["Q42_CHILD_DIR"] = str(log)
    return env


def shim_overhead(shim: dict, work: Path, calls: int = 50) -> dict:
    """CPU the shim adds per Git child: `git version`, direct and shimmed."""
    log = private_dir(work / f"shim-overhead-{time.monotonic_ns()}")
    direct = shimmed = 0.0
    for _ in range(calls):
        direct += measured(["git", "version"], keep_stdout=False)["cpu_s"]
        shimmed += measured(
            ["git", "version"], env=shim_env(shim, log), keep_stdout=False
        )["cpu_s"]
    recorded = parse_child_log(log)
    shutil.rmtree(log)
    return {
        "calls": calls,
        "recorded": recorded["children"],
        "direct_ms_per_call": round(1000 * direct / calls, 3),
        "shimmed_ms_per_call": round(1000 * shimmed / calls, 3),
        "overhead_ms_per_call": round(1000 * (shimmed - direct) / calls, 3),
    }


def bash_seconds(value: str) -> float:
    """`0m1.234s` (bash `times`) in seconds."""
    minutes, _, seconds = value.rstrip("s").partition("m")
    return 60 * float(minutes) + float(seconds)


def git_subcommand(command: str) -> str:
    """The subcommand of a logged `git [-c X] [-C X] [--flag] sub ...` line."""
    tokens = command.split()[1:]
    skip = False
    for token in tokens:
        if skip:
            skip = False
            continue
        if token in ("-c", "-C"):
            skip = True
            continue
        if token.startswith("-"):
            continue
        return token
    return "?"


def git_detail(command: str) -> str:
    """The subcommand and its arguments, without the global flags."""
    tokens = command.split()
    sub = git_subcommand(command)
    if sub in tokens:
        tokens = tokens[tokens.index(sub) :]
    return " ".join(tokens)[:200]


def parse_child_log(directory: Path) -> dict:
    """Per-subcommand counts and times of the Git children a run spawned,
    from the shim's one file per child (CPU at bash's 1 ms resolution)."""
    rows = []
    files = sorted(directory.iterdir()) if directory.is_dir() else []
    for path in files:
        lines = path.read_text(errors="replace").splitlines()
        if len(lines) < 4:
            continue
        try:
            start, end, code = lines[0].split("\t")
            user, system = (bash_seconds(v) for v in lines[-1].split())
            wall, code_value = float(end) - float(start), int(code)
        except ValueError:
            continue
        command = "git " + " ".join(lines[1:-2])
        rows.append((git_subcommand(command), wall, user, system, code_value, command))
    by: dict[str, dict] = {}
    for sub, wall, user, system, code, _ in rows:
        slot = by.setdefault(
            sub, {"count": 0, "wall_s": 0.0, "user_s": 0.0, "sys_s": 0.0, "failed": 0}
        )
        slot["count"] += 1
        slot["wall_s"] += wall
        slot["user_s"] += user
        slot["sys_s"] += system
        slot["failed"] += code != 0
    for slot in by.values():
        for key in ("wall_s", "user_s", "sys_s"):
            slot[key] = round(slot[key], 3)
        slot["cpu_s"] = round(slot["user_s"] + slot["sys_s"], 3)
    top = sorted(rows, key=lambda r: r[2] + r[3], reverse=True)[:12]
    user = sum(r[2] for r in rows)
    system = sum(r[3] for r in rows)
    return {
        "children": len(rows),
        "wall_s": round(sum(r[1] for r in rows), 3),
        "user_s": round(user, 3),
        "sys_s": round(system, 3),
        "cpu_s": round(user + system, 3),
        "by_subcommand": dict(
            sorted(by.items(), key=lambda kv: kv[1]["cpu_s"], reverse=True)
        ),
        "top": [
            {
                "subcommand": sub,
                "detail": git_detail(command),
                "wall_s": round(wall, 3),
                "user_s": user_s,
                "sys_s": sys_s,
                "exit": code,
            }
            for sub, wall, user_s, sys_s, code, command in top
        ],
    }


def agent_run(
    agent: Path, args: list[str], label: str, shim: dict | None = None
) -> dict:
    """One agent verb, measured; with `shim`, every Git child is attributed."""
    before, pressure = load1(), cpu_pressure()
    log = None
    env = None
    if shim is not None:
        log = private_dir(Path(shim["dir"]) / f"children-{time.monotonic_ns()}")
        env = shim_env(shim, log)
    run = measured([str(agent), *args], env=env)
    stdout = run.pop("stdout").decode(errors="replace")
    stderr = run.pop("stderr").decode(errors="replace")
    counters = parse_counters(stderr)
    record = {
        "label": label,
        "argv": args,
        **run,
        "load1_before": before,
        "load1_after": load1(),
        "psi_cpu_avg10_before": pressure,
        "psi_cpu_avg10_after": cpu_pressure(),
        "classified": classify(run["exit"], stdout, stderr),
        "stdout_lines": stdout.splitlines()[:40],
        "priority": counters.get("priority"),
        "counters": {
            k: v
            for k, v in counters.items()
            if k
            in (
                "elapsed_ns",
                "write_source_pack_bytes",
                "write_source_pack_objects",
                "read_source_pack_readback_bytes",
                "read_source_capture_reuse_bytes",
                "census_walks",
                "priority",
                "priority_from",
            )
        },
    }
    if log is not None:
        children = parse_child_log(log)
        shutil.rmtree(log)
        record["children"] = children
        # The agent process itself, less every Git child and its descendants
        # (and the shim's own few ms per child).
        record["agent_self_cpu_s"] = round(run["cpu_s"] - children["cpu_s"], 2)
    return record


def bundles_in(directory: Path) -> list[Path]:
    if not directory.is_dir():
        return []
    return sorted(p for p in directory.rglob("*.bundle") if p.is_file())


def header_terms(bundle: Path, source_names: set[str]) -> dict:
    """The header's lines by class, measured line by line: the signature
    and capability lines, prerequisites, ref lines whose name maps from a
    source ref (a*refs), the other ref lines (the capture's metadata refs),
    and the blank line. Only for headers under the probe limit."""
    terms = {
        "signature_bytes": 0,
        "capability_bytes": 0,
        "prerequisite_bytes": 0,
        "source_ref_lines": 0,
        "source_ref_bytes": 0,
        "metadata_ref_lines": 0,
        "metadata_ref_bytes": 0,
        "blank_bytes": 0,
    }
    metadata: list[str] = []
    with open(bundle, "rb") as handle:
        first = handle.readline(LINE_CAP)
        terms["signature_bytes"] = len(first)
        while True:
            line = handle.readline(LINE_CAP)
            if not line or not line.endswith(b"\n"):
                raise ProbeError(f"{bundle.name}: header ends without a blank line")
            if line == b"\n":
                terms["blank_bytes"] = 1
                break
            if line.startswith(b"@"):
                terms["capability_bytes"] += len(line)
            elif line.startswith(b"-"):
                terms["prerequisite_bytes"] += len(line)
            else:
                name = line.rstrip(b"\n").partition(b" ")[2].decode(errors="replace")
                if name in source_names:
                    terms["source_ref_lines"] += 1
                    terms["source_ref_bytes"] += len(line)
                else:
                    terms["metadata_ref_lines"] += 1
                    terms["metadata_ref_bytes"] += len(line)
                    metadata.append(name)
    terms["metadata_ref_names"] = sorted(metadata)[:32]
    terms["header_bytes"] = sum(v for k, v in terms.items() if k.endswith("_bytes"))
    return terms


def decompose_bundle(bundle: Path, source: Path, work: Path) -> dict:
    """Split a capture bundle into the M inputs: header terms (signature,
    prerequisite lines, ref lines) and pack objects by role, from
    `verify-pack -v` after unbundling into a scratch repository that reads the
    source's objects (so a chained bundle's prerequisites are present)."""
    header = read_header(bundle)
    source_names = {exported_name(name) for _, name in inventory(source)}
    lines = header_terms(bundle, source_names)
    scratch = work / f"decompose-{bundle.stem[:24]}.git"
    git(work, "init", "--quiet", "--bare", "--template=", str(scratch))
    objects = git(
        source, "rev-parse", "--path-format=absolute", "--git-path", "objects"
    )
    (scratch / "objects" / "info" / "alternates").write_bytes(objects)
    git(scratch, "bundle", "unbundle", str(bundle))
    packs = sorted((scratch / "objects" / "pack").glob("*.idx"))
    sizes: dict[str, tuple[str, int, int]] = {}
    deltas: set[str] = set()
    pack_file_bytes = 0
    for idx in packs:
        pack_file_bytes += idx.with_suffix(".pack").stat().st_size
        for line in git(scratch, "verify-pack", "-v", str(idx)).decode().splitlines():
            parts = line.split()
            if (
                len(parts) >= 5
                and is_oid(parts[0])
                and parts[1] in ("commit", "tree", "blob", "tag")
            ):
                sizes[parts[0]] = (parts[1], int(parts[2]), int(parts[3]))
                # `oid type size size-in-pack offset depth base`: a delta.
                if len(parts) >= 7:
                    deltas.add(parts[0])
    heads = git(scratch, "bundle", "list-heads", str(bundle)).decode().splitlines()
    role_of: dict[str, str] = {}
    order = sorted(
        (line.split(" ", 1) for line in heads),
        key=lambda pair: (
            0
            if pair[1].endswith("/worktree")
            else 1
            if pair[1].endswith("/staged")
            else 2
        ),
    )
    for oid, name in order:
        if oid not in sizes:
            continue
        role = name.rsplit("/", 1)[-1] if name.startswith(EXPORT_PREFIX) else "other"
        kind = sizes[oid][0]
        role_of.setdefault(oid, f"{role}:{kind}")
        if kind != "commit":
            continue
        root = git(scratch, "rev-parse", f"{oid}^{{tree}}").decode().strip()
        role_of.setdefault(root, f"{role}:tree")
        listing = git(scratch, "ls-tree", "-r", "-t", oid).decode().splitlines()
        for line in listing:
            meta, _, _path = line.partition("\t")
            _mode, kind, obj = meta.split()
            if obj in sizes:
                role_of.setdefault(obj, f"{role}:{kind}")
    roles: dict[str, dict] = {}
    for oid, (kind, raw, size) in sizes.items():
        role = role_of.get(oid, f"unattributed:{kind}")
        slot = roles.setdefault(
            role, {"objects": 0, "raw_bytes": 0, "in_pack_bytes": 0, "deltas": 0}
        )
        slot["objects"] += 1
        slot["raw_bytes"] += raw
        slot["in_pack_bytes"] += size
        slot["deltas"] += oid in deltas
    by_type: dict[str, dict] = {}
    for kind, raw, size in sizes.values():
        slot = by_type.setdefault(
            kind, {"objects": 0, "raw_bytes": 0, "in_pack_bytes": 0}
        )
        slot["objects"] += 1
        slot["raw_bytes"] += raw
        slot["in_pack_bytes"] += size
    worktree_trees = roles.get("worktree:tree", {"objects": 0, "in_pack_bytes": 0})
    entries = sum(
        len(git(scratch, "cat-file", "-p", oid).splitlines())
        for oid, role in role_of.items()
        if role == "worktree:tree" and oid in sizes
    )
    worktree_trees["entries"] = entries
    return {
        "bundle": bundle.name,
        "bundle_bytes": bundle.stat().st_size,
        "header": header,
        "header_terms": lines,
        "pack_bytes": bundle.stat().st_size - header["header_bytes"],
        "pack_objects": len(sizes),
        "pack_file_bytes_unbundled": pack_file_bytes,
        "pack_fixed_bytes": 12 + 20,
        "by_type": by_type,
        "by_role": dict(sorted(roles.items())),
        "b_bytes_per_changed_tree": (
            round(worktree_trees["in_pack_bytes"] / worktree_trees["objects"], 1)
            if worktree_trees["objects"]
            else None
        ),
        "b_bytes_per_tree_entry": (
            round(worktree_trees["in_pack_bytes"] / entries, 2) if entries else None
        ),
    }


def mutate(repo: Path, depth_dir: str = "d001", name: str = "f001.txt") -> str:
    """One unstaged edit two levels down: a changed seat, one changed blob and
    two changed trees (root and its directory) in the worktree snapshot."""
    target = repo / depth_dir / name
    with open(target, "ab") as handle:
        handle.write(b"q42 mutation\n")
    return f"{depth_dir}/{name}"


def private_dir(path: Path) -> Path:
    path.mkdir(mode=0o700, parents=True, exist_ok=False)
    return path


def stat_shape(repo: Path) -> dict:
    """The stat values filesystem-v1 rows carry, summarized over the
    worktree's files (not .git): they vary with when and where a corpus was
    written, and the rows blob's packed size varies with them."""
    inodes: list[int] = []
    mtimes: list[int] = []
    for root, dirs, files in os.walk(repo):
        if ".git" in dirs:
            dirs.remove(".git")
        for name in files:
            st = os.lstat(os.path.join(root, name))
            inodes.append(st.st_ino)
            mtimes.append(st.st_mtime_ns)
    if not inodes:
        return {"files": 0}
    return {
        "files": len(inodes),
        "inode_span": max(inodes) - min(inodes),
        "inode_digits_max": len(str(max(inodes))),
        "distinct_mtime_ns": len(set(mtimes)),
        "distinct_mtime_s": len({m // 1_000_000_000 for m in mtimes}),
        "mtime_span_ms": round((max(mtimes) - min(mtimes)) / 1e6, 3),
    }


def estate_rep(
    agent: Path,
    corpus: dict,
    work: Path,
    *,
    chained_pass: bool,
    group: bool,
    shim: dict | None = None,
) -> dict:
    """estate-add (one item, or the main checkout plus its linked worktree as
    one plan-base group), then estate-capture passes, in a fresh plan."""
    repo = Path(corpus["path"])
    plan = work / "plan.json"
    state = private_dir(work / "state")
    store = private_dir(work / "corpus")
    record: dict = {"group": group, "passes": []}
    if group:
        linked = Path(corpus["linked_worktree"])
        add = agent_run(
            agent,
            [
                "estate-add-batch",
                str(plan),
                str(repo),
                str(work / "dest-main"),
                "-",
                str(linked),
                str(work / "dest-linked"),
                "-",
            ],
            "estate-add-batch",
        )
    else:
        add = agent_run(
            agent,
            ["estate-add", str(plan), str(repo), str(work / "dest")],
            "estate-add",
        )
    record["add"] = add["classified"]
    capture = ["estate-capture", str(plan), str(state), str(store), "1"]
    first = agent_run(agent, capture, "pass-1", shim)
    first["bundles"] = [read_header(b) for b in bundles_in(store)]
    first["diagnosis_bundles"] = [read_header(b) for b in bundles_in(state)]
    record["passes"].append(first)
    if chained_pass and first["classified"]["kind"] == "ok":
        record["mutated"] = mutate(repo)
        record["stat_shape"] = stat_shape(repo)
        seen = {b.name for b in bundles_in(store)}
        second = agent_run(agent, capture, "pass-2-chained", shim)
        fresh = [b for b in bundles_in(store) if b.name not in seen]
        second["bundles"] = [read_header(b) for b in fresh]
        second["prior_sidecars"] = sorted(p.name for p in store.glob("*.prior"))
        record["passes"].append(second)
        if fresh:
            record["decomposed"] = decompose_bundle(fresh[0], repo, work)
    if group and first["classified"]["kind"] == "ok":
        again = agent_run(agent, capture, "pass-2-unchanged", shim)
        record["passes"].append(again)
        apply = agent_run(
            agent,
            [
                "estate-apply",
                str(plan),
                str(store),
                str(private_dir(work / "apply-state")),
                SOURCE_SLUG,
                "1",
            ],
            "estate-apply",
        )
        record["passes"].append(apply)
    return record


def estate_passes(
    agent: Path,
    corpus: dict,
    work: Path,
    *,
    chained_pass: bool,
    group: bool,
    reps: int = 1,
    shim: dict | None = None,
    prune: bool = False,
) -> dict:
    """`reps` independent repetitions of `estate_rep`, each in its own plan,
    state and store under WORK/rep-N. A chained repetition appends one more
    line to the same tracked file, so every pass 2 sees a one-line change.
    With `prune`, each repetition's own scratch is removed once recorded."""
    if reps == 1 and not prune and shim is None:
        return estate_rep(agent, corpus, work, chained_pass=chained_pass, group=group)
    record: dict = {"group": group, "reps": []}
    for index in range(reps):
        rep_dir = private_dir(work / f"rep-{index}")
        record["reps"].append(
            estate_rep(
                agent,
                corpus,
                rep_dir,
                chained_pass=chained_pass,
                group=group,
                shim=shim,
            )
        )
        if prune:
            shutil.rmtree(rep_dir)
    return record


def git_export(agent: Path, repo: Path, work: Path, shim: dict | None = None) -> dict:
    # NEW_CAPTURE_DIR must not exist: the agent creates it (mode 0700).
    capture = work / "export"
    run = agent_run(agent, ["git-export", str(repo), str(capture)], "git-export", shim)
    run["bundles"] = [read_header(b) for b in bundles_in(capture)]
    return run


# ---- the whole run ------------------------------------------------------------------


SMALL = {"shape": "compact", "refs": 256, "oids": 64, "namespaces": 2}
WINDOW = {
    "shape": "carry",
    "header_target": HEADER_CAP - 20_000,
    "linked_worktree": True,
}
SLOPE_REFS = (2_000, 30_000, 60_000, 90_000)


def suite_plans(suite: str) -> list[dict]:
    """The corpora and probes of a suite.

    `v1` is run A as it ran: one agent sample per pass, no child attribution,
    and a walk replayed over packed private refs. `review` re-runs it for
    the review findings: the walk over each ref storage (loose is the
    agent's), 3 repetitions of every agent pass with each Git child
    attributed, a ref-count series at fixed tips and namespaces
    (`slope-compact-*`, with `compact-119761` its top point), corpora that
    vary seats and tree width (`shape-*`), and the spawn micro-measure in the
    cap-window corpus. The grouped cap-window pass (1,432 s CPU) is not
    repeated: its evidence is run A's."""
    if suite == "v1":
        return [
            {"name": "small-compact", "corpus": SMALL, "mode": "chain"},
            {"name": "carry-119761", "corpus": {"shape": "carry"}, "mode": "refuse"},
            {"name": "compact-119761", "corpus": {"shape": "compact"}, "mode": "chain"},
            {"name": "carry-window", "corpus": WINDOW, "mode": "group"},
        ]
    if suite != "review":
        raise ValueError(f"unknown suite {suite}")
    every = {"storages": REF_STORAGES, "reps": 3, "attribute": True, "prune": True}
    plans = [{"name": "small-compact", "corpus": SMALL, "mode": "chain", **every}]
    for dirs, files in ((16, 64), (256, 64), (64, 16), (64, 256)):
        plans.append(
            {
                "name": f"shape-{dirs}x{files}",
                "corpus": {**SMALL, "dirs": dirs, "files_per_dir": files},
                "mode": "chain",
                "storages": (),
                "reps": 1,
                "attribute": True,
                "prune": True,
            }
        )
    for refs in SLOPE_REFS:
        plans.append(
            {
                "name": f"slope-compact-{refs}",
                "corpus": {"shape": "compact", "refs": refs, "cover": True},
                "mode": "chain",
                **every,
                "storages": ("loose",),
            }
        )
    plans += [
        {
            "name": "compact-119761",
            "corpus": {"shape": "compact"},
            "mode": "chain",
            **every,
        },
        {
            "name": "carry-119761",
            "corpus": {"shape": "carry"},
            "mode": "refuse",
            **every,
        },
        {
            "name": "carry-window",
            "corpus": WINDOW,
            "mode": "walk",
            "storages": REF_STORAGES,
            "spawn_pairs": 400,
        },
    ]
    return plans


def is_large(plan: dict) -> bool:
    corpus = plan["corpus"]
    return corpus.get("refs", 119_761) > 10_000 or "header_target" in corpus


def run_all(
    agent: Path,
    work: Path,
    out: Path,
    *,
    skip_large: bool = False,
    only: list[str] | None = None,
    suite: str = "v1",
    resume: bool = False,
) -> dict:
    """Run a suite's probes, saving OUT after every step.

    With `resume`, an existing OUT from the same suite, script revision and
    agent is continued in place: corpora already complete are kept, an
    interrupted one is redone from scratch (its directory under WORK is this
    probe's own scratch), and each launch is one entry in `segments`. So a
    suite longer than one sitting stays one raw file that only this code
    wrote."""
    plans = suite_plans(suite)
    if skip_large:
        plans = [plan for plan in plans if not is_large(plan)]
    if only:
        plans = [plan for plan in plans if plan["name"] in only]
    script = script_provenance()
    agent_sha = hashlib.sha256(agent.read_bytes()).hexdigest()
    if resume and out.exists():
        record = json.loads(out.read_text())
        if (record.get("suite"), record.get("script", {}).get("sha256")) != (
            suite,
            script["sha256"],
        ):
            raise ProbeError("resume needs the same suite and script revision")
        if record["agent"]["sha256"] != agent_sha:
            raise ProbeError("resume needs the same agent binary")
        if not work.is_dir():
            raise ProbeError(f"{work} is missing; resume continues in place")
    else:
        if work.exists():
            raise ProbeError(f"{work} already exists; give a fresh work directory")
        private_dir(work)
        record = {
            "schema": SCHEMA,
            "label": LABEL,
            "suite": suite,
            "argv": sys.argv[1:],
            "started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "host": host_record(),
            "script": script,
            "agent": {"path": str(agent), "sha256": agent_sha},
            "cap": {"header_bytes": HEADER_CAP, "line_bytes": LINE_CAP},
            "plans": [plan["name"] for plan in suite_plans(suite)],
            "corpora": {},
            "segments": [],
        }
    segment: dict = {
        "argv": sys.argv[1:],
        "started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "script_matches_head": script.get("matches_head"),
        "load1_before": load1(),
        "psi_cpu_avg10_before": cpu_pressure(),
        "corpora": [],
    }
    record.setdefault("segments", []).append(segment)
    shim = None
    if any(plan.get("attribute") for plan in plans):
        shim = child_shim(work / "shim")
        if shim is None:
            raise ProbeError("child attribution needs git and bash")
        segment["shim_overhead"] = shim_overhead(shim, work)
        record["shim"] = {**shim, "overhead": segment["shim_overhead"]}

    def save() -> None:
        out.write_text(json.dumps(record, indent=2, sort_keys=True) + "\n")

    save()
    for plan in plans:
        name, mode = plan["name"], plan["mode"]
        if record["corpora"].get(name, {}).get("complete"):
            continue
        # An attempt a stopped launch left behind is this probe's own scratch.
        if (work / name).exists():
            shutil.rmtree(work / name)
        record["corpora"].pop(name, None)
        reps = plan.get("reps", 1)
        attribute = shim if plan.get("attribute") else None
        prune = plan.get("prune", False)
        base = private_dir(work / name)
        corpus = build_corpus(base / "source", **plan["corpus"])
        entry: dict = {"corpus": corpus, "plan": {k: v for k, v in plan.items()}}
        record["corpora"][name] = entry
        save()
        repo = Path(corpus["path"])
        storages = plan.get("storages", ("packed",))
        if storages:
            entry["walk"] = replay_walk(
                repo,
                base / "walk",
                reps=3,
                storages=tuple(storages),
                spawn_pairs=plan.get("spawn_pairs", 0),
            )
            save()
        if mode == "refuse":
            if reps == 1 and attribute is None:
                entry["git_export"] = git_export(agent, repo, base)
            else:
                entry["git_export_reps"] = []
                for index in range(reps):
                    rep_dir = private_dir(base / f"export-rep-{index}")
                    entry["git_export_reps"].append(
                        git_export(agent, repo, rep_dir, attribute)
                    )
                    if prune:
                        shutil.rmtree(rep_dir)
                    save()
            entry["estate"] = estate_passes(
                agent,
                corpus,
                private_dir(base / "estate"),
                chained_pass=False,
                group=False,
                reps=reps,
                shim=attribute,
                prune=prune,
            )
        elif mode == "chain":
            entry["estate"] = estate_passes(
                agent,
                corpus,
                private_dir(base / "estate"),
                chained_pass=True,
                group=False,
                reps=reps,
                shim=attribute,
                prune=prune,
            )
        elif mode == "group":
            entry["estate"] = estate_passes(
                agent,
                corpus,
                private_dir(base / "estate"),
                chained_pass=False,
                group=True,
                reps=reps,
                shim=attribute,
                prune=prune,
            )
        entry["complete"] = True
        segment["corpora"].append(name)
        save()
    now = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    segment["finished_utc"] = now
    if all(record["corpora"].get(n, {}).get("complete") for n in record["plans"]):
        record["finished_utc"] = now
    save()
    return record


# ---- the allowance terms --------------------------------------------------------------


def allowance_terms(corpus: dict, decomposed: dict) -> dict:
    """Split one chained capture bundle into Q45's terms.

    Header terms are measured line by line (`header_terms`): a*refs is the
    ref lines whose names map from a source ref, the metadata ref lines are
    the others, p*tips the prerequisite lines. Pack terms are in-pack sizes
    by object role: b the changed worktree trees, s the filesystem-v1 rows
    blob (every seat, rewritten whole), content the changed blobs, and c the
    capture's other metadata objects (unattributed objects listed apart).

    `check` (the bundle less every term) is 0 for any well-formed bundle
    whatever the role split: bundle = header + 32 B + the in-pack sizes. It
    checks the parse, not the terms. `a_model_delta` (measured a*refs less
    the corpus model's) checks the header model, and `metadata_ref_lines`
    counts the lines that are not source refs.

    A record without `header_terms` (run A) falls back to its old split:
    a*refs is the model and the metadata ref lines are the residual
    (`a_source` says which)."""
    header = decomposed["header"]
    roles = decomposed["by_role"]
    model = corpus["model"]
    refs = model["refs"]
    lines = decomposed.get("header_terms")
    if lines:
        a_source = "measured"
        a_refs = lines["source_ref_bytes"]
        a_lines = lines["source_ref_lines"]
        metadata_refs = lines["metadata_ref_bytes"]
        metadata_lines = lines["metadata_ref_lines"]
        sig_blank = (
            lines["signature_bytes"] + lines["capability_bytes"] + lines["blank_bytes"]
        )
        header_check = header["header_bytes"] - lines["header_bytes"]
    else:
        a_source = "modelled"
        a_refs = model["ref_line_bytes"]
        a_lines = refs
        metadata_refs = header["ref_bytes"] - a_refs
        metadata_lines = header["ref_lines"] - refs
        sig_blank = (
            header["header_bytes"] - header["prerequisite_bytes"] - header["ref_bytes"]
        )
        header_check = None

    def take(role: str) -> dict:
        return roles.get(
            role, {"objects": 0, "in_pack_bytes": 0, "raw_bytes": 0, "deltas": 0}
        )

    trees = take("worktree:tree")
    seats_blob = take("filesystem-v1:blob")
    content = take("worktree:blob")
    named = ("worktree:tree", "filesystem-v1:blob", "worktree:blob")
    metadata_objects = sum(
        slot["in_pack_bytes"]
        for role, slot in roles.items()
        if role not in named and not role.startswith("unattributed:")
    )
    unattributed = sum(
        slot["in_pack_bytes"]
        for role, slot in roles.items()
        if role.startswith("unattributed:")
    )
    pack_fixed = decomposed["pack_fixed_bytes"]
    c = sig_blank + pack_fixed + metadata_objects + unattributed + metadata_refs
    seats = corpus["tracked_files"] + corpus["dirs"]
    entries = decomposed.get("by_role", {}).get("worktree:tree", {}).get("entries")
    terms = {
        "c": c,
        "c_parts": {
            "signature_and_blank": sig_blank,
            "pack_header_and_trailer": pack_fixed,
            "metadata_objects": metadata_objects,
            "unattributed_objects": unattributed,
            "metadata_ref_lines": metadata_refs,
        },
        "a_refs": a_refs,
        "p_tips": header["prerequisite_bytes"],
        "b_trees": trees["in_pack_bytes"],
        "s_seats": seats_blob["in_pack_bytes"],
        "content": content["in_pack_bytes"],
    }
    total = sum(v for k, v in terms.items() if k != "c_parts")
    in_pack = sum(slot["in_pack_bytes"] for slot in roles.values())
    return {
        "bundle_bytes": decomposed["bundle_bytes"],
        "terms": terms,
        "a_source": a_source,
        "check": decomposed["bundle_bytes"] - total,
        "pack_check": decomposed["pack_bytes"] - pack_fixed - in_pack,
        "header_check": header_check,
        "a_model_delta": a_refs - model["ref_line_bytes"] if lines else None,
        "a_lines_delta": a_lines - refs,
        "metadata_ref_lines": metadata_lines,
        "metadata_ref_names": (lines or {}).get("metadata_ref_names"),
        "units": {
            "refs": refs,
            "a_lines": a_lines,
            "a_bytes_per_ref": round(a_refs / a_lines, 3) if a_lines else None,
            "tips": header["prerequisite_lines"],
            "p_bytes_per_tip": (
                round(header["prerequisite_bytes"] / header["prerequisite_lines"], 3)
                if header["prerequisite_lines"]
                else None
            ),
            "changed_trees": trees["objects"],
            "tree_entries": entries,
            "b_bytes_per_tree": decomposed["b_bytes_per_changed_tree"],
            "b_bytes_per_entry": decomposed["b_bytes_per_tree_entry"],
            "seats": seats,
            "s_deltified": seats_blob.get("deltas"),
            "s_bytes_per_seat_in_pack": round(seats_blob["in_pack_bytes"] / seats, 3),
            "s_bytes_per_seat_raw": round(seats_blob["raw_bytes"] / seats, 3),
        },
    }


def estate_reps(estate: dict | None) -> list[dict]:
    """A record's estate repetitions: `reps`, or the record itself (run A)."""
    if not estate:
        return []
    return estate["reps"] if "reps" in estate else [estate]


def agent_row(run: dict, rep: int) -> dict:
    row = {
        "label": run["label"],
        "rep": rep,
        "kind": run["classified"]["kind"],
        "codes": run["classified"]["codes"],
        "exit": run["exit"],
        "wall_s": run["wall_s"],
        "cpu_s": run["cpu_s"],
        "user_s": run.get("cpu_user_s"),
        "sys_s": run.get("cpu_sys_s"),
        "load1_before": run["load1_before"],
        "psi_cpu_avg10_before": run.get("psi_cpu_avg10_before"),
        "priority": run.get("priority"),
        "bundles": [
            (b["header_bytes"], b["agent_reader"]) for b in run.get("bundles", [])
        ],
    }
    if "children" in run:
        children = run["children"]
        row["children"] = {
            k: children[k] for k in ("children", "cpu_s", "user_s", "sys_s", "wall_s")
        }
        row["by_subcommand"] = children["by_subcommand"]
        row["agent_self_cpu_s"] = run.get("agent_self_cpu_s")
    return row


def by_label(rows: list[dict]) -> dict:
    """Repeated samples of each pass: spreads of CPU (user, sys), wall and
    load, and of each Git subcommand's CPU when children were attributed."""
    groups: dict[str, list[dict]] = {}
    for row in rows:
        groups.setdefault(row["label"], []).append(row)
    out: dict = {}
    for label, group in groups.items():
        entry: dict = {
            "n": len(group),
            "kinds": sorted({row["kind"] for row in group}),
            "codes": sorted({code for row in group for code in row["codes"]}),
            "cpu_s": spread([row["cpu_s"] for row in group]),
            "wall_s": spread([row["wall_s"] for row in group]),
            "load1_before": spread([row["load1_before"] for row in group]),
        }
        if all(row["user_s"] is not None for row in group):
            entry["user_s"] = spread([row["user_s"] for row in group])
            entry["sys_s"] = spread([row["sys_s"] for row in group])
        pressures = [row["psi_cpu_avg10_before"] for row in group]
        if all(p is not None for p in pressures):
            entry["psi_cpu_avg10_before"] = spread(pressures)
        if all("children" in row for row in group):
            entry["children_cpu_s"] = spread(
                [row["children"]["cpu_s"] for row in group]
            )
            entry["agent_self_cpu_s"] = spread(
                [row["agent_self_cpu_s"] for row in group]
            )
            subs = {sub for row in group for sub in row["by_subcommand"]}
            table = {}
            for sub in subs:
                slots = [row["by_subcommand"].get(sub, {}) for row in group]
                table[sub] = {
                    key: spread([float(slot.get(key, 0)) for slot in slots])
                    for key in ("count", "cpu_s", "user_s", "sys_s")
                }
            entry["by_subcommand"] = dict(
                sorted(
                    table.items(), key=lambda kv: kv[1]["cpu_s"]["median"], reverse=True
                )[:12]
            )
        out[label] = entry
    return out


def walk_summary(walk: dict) -> dict:
    """The walk replay per ref storage; run A's record is one packed replay."""
    storages = walk["storages"] if "storages" in walk else {"packed (run A)": walk}
    out: dict = {"excluded_tips": walk.get("excluded_tips")}
    for name, entry in storages.items():
        row: dict = {
            "private_refs": entry.get("private_refs"),
            "loose_ref_files": entry.get("loose_ref_files"),
        }
        for key in ("create_refs", "pack_refs"):
            if entry.get(key):
                row[key] = {
                    k: entry[key][k]
                    for k in ("wall_s", "cpu_s", "cpu_user_s", "cpu_sys_s")
                }
        if "for_each_ref" in entry:
            row["for_each_ref_cpu_s"] = entry["for_each_ref"]["cpu_s"]
        for mode in ("objects-edge-aggressive", "objects-edge"):
            if mode not in entry:
                continue
            runs = entry[mode]["runs"]
            row[mode] = {
                "cpu_s": spread([r["cpu_s"] for r in runs]),
                "user_s": spread([r["cpu_user_s"] for r in runs]),
                "sys_s": spread([r["cpu_sys_s"] for r in runs]),
                "wall_s": spread([r["wall_s"] for r in runs]),
                "objects_listed": entry[mode]["objects_listed"],
                "pack_bytes": entry[mode]["pack_bytes"],
            }
        if "spawn_micro" in entry:
            row["spawn_micro"] = entry["spawn_micro"]
        out[name] = row
    if "spawn_micro_source" in walk:
        out["spawn_micro_source"] = walk["spawn_micro_source"]
    return out


def slope_family(corpus: dict) -> bool:
    """The ref-count series: compact, 1,032 distinct oids, 157 namespaces,
    64 x 64 tracked files (so tips, namespaces and seats are fixed)."""
    split = corpus["split"]
    return (
        corpus["shape"] == "compact"
        and split["oids"] == 1_032
        and split["namespaces"] == 157
        and (corpus["dirs"], corpus["files_per_dir"]) == (64, 64)
    )


def fitted(points: list[tuple[float, float]], scale: float = 1.0) -> dict | None:
    """`linear_fit` with the slope scaled (e.g. to ms per ref) and rounded."""
    fit = linear_fit(points)
    if fit is None:
        return None
    return {
        "n": fit["n"],
        "distinct_x": fit["distinct_x"],
        "slope": round(fit["slope"] * scale, 4),
        "intercept": round(fit["intercept"], 3),
        "r2": round(fit["r2"], 4),
    }


def fits(rows: dict) -> dict:
    """The per-ref CPU slope over the ref-count series (every sample, by
    pass, user and sys apart, and by Git subcommand), the walk and ref
    creation against private refs, and s, b and a against their drivers."""
    out: dict = {}
    family = [row for row in rows.values() if row.get("slope_family")]
    for label in ("pass-1", "pass-2-chained"):
        samples = [
            (row["refs"], sample)
            for row in family
            for sample in row.get("agent", [])
            if sample["label"] == label
        ]
        if len({refs for refs, _ in samples}) < 2:
            continue
        entry = {
            key: fitted([(refs, s[key]) for refs, s in samples], 1000)
            for key in ("cpu_s", "user_s", "sys_s", "wall_s")
            if all(s.get(key) is not None for _, s in samples)
        }
        attributed = [(refs, s) for refs, s in samples if "by_subcommand" in s]
        if len(attributed) == len(samples):
            entry["agent_self_cpu_s"] = fitted(
                [(refs, s["agent_self_cpu_s"]) for refs, s in attributed], 1000
            )
            subs = {sub for _, s in attributed for sub in s["by_subcommand"]}
            per_sub = {
                sub: fitted(
                    [
                        (refs, s["by_subcommand"].get(sub, {}).get("cpu_s", 0.0))
                        for refs, s in attributed
                    ],
                    1000,
                )
                for sub in subs
            }
            entry["by_subcommand"] = dict(
                sorted(
                    ((k, v) for k, v in per_sub.items() if v),
                    key=lambda kv: kv[1]["slope"],
                    reverse=True,
                )[:10]
            )
        entry["unit"] = "slope in ms per ref; intercept in s"
        out[f"cpu_per_ref:{label}"] = entry
    walks = [
        (row["walk"]["loose"], row)
        for row in rows.values()
        if "loose" in row.get("walk", {})
    ]
    if len(walks) >= 2:
        out["walk_loose_per_private_ref"] = {
            "aggressive_cpu_s": fitted(
                [
                    (w["private_refs"], w["objects-edge-aggressive"]["cpu_s"]["median"])
                    for w, _ in walks
                ],
                1000,
            ),
            "create_refs_cpu_s": fitted(
                [(w["private_refs"], w["create_refs"]["cpu_s"]) for w, _ in walks],
                1000,
            ),
            "for_each_ref_cpu_s": fitted(
                [
                    (w["private_refs"], w["for_each_ref_cpu_s"]["median"])
                    for w, _ in walks
                ],
                1000,
            ),
            "unit": "slope in ms per private ref; intercept in s",
        }
    allowances = [
        allowance for row in rows.values() for allowance in row.get("allowance", [])
    ]
    if allowances:
        units = [a["units"] for a in allowances]
        terms = [a["terms"] for a in allowances]
        out["s_per_seat"] = {
            "in_pack": fitted(
                [(u["seats"], t["s_seats"]) for u, t in zip(units, terms)]
            ),
            "raw": fitted(
                [(u["seats"], u["s_bytes_per_seat_raw"] * u["seats"]) for u in units]
            ),
            "in_pack_per_seat": spread([u["s_bytes_per_seat_in_pack"] for u in units]),
            "raw_per_seat": spread([u["s_bytes_per_seat_raw"] for u in units]),
            "distinct_seats": sorted({u["seats"] for u in units}),
            "deltified": sum(1 for u in units if u.get("s_deltified")),
        }
        with_entries = [(u, t) for u, t in zip(units, terms) if u.get("tree_entries")]
        out["b_per_entry"] = {
            "in_pack": fitted(
                [(u["tree_entries"], t["b_trees"]) for u, t in with_entries]
            ),
            "per_entry": spread([u["b_bytes_per_entry"] for u, _ in with_entries]),
            "distinct_entries": sorted({u["tree_entries"] for u, _ in with_entries}),
        }
        out["a_per_ref"] = {
            "line_bytes": fitted(
                [(u["a_lines"], t["a_refs"]) for u, t in zip(units, terms)]
            ),
            "per_ref": spread([u["a_bytes_per_ref"] for u in units]),
        }
    return out


def summarize(paths: list[Path]) -> dict:
    """The doc's figures from one or more `run` result files.

    Run A's file (2026-10-04-q42-probes.json) was merged and annotated by
    hand from two runs of an unrecorded script revision; its `runs`, `build`
    and `spawn_micro` keys are hand-written and reported as such here."""
    out: dict = {"label": LABEL, "files": [], "corpora": {}}
    for path in paths:
        record = json.loads(path.read_text())
        provenance = {
            "path": str(path),
            "suite": record.get("suite", "v1"),
            "script": record.get("script", "not recorded (run A)"),
            "agent_sha256": record.get("agent", {}).get("sha256"),
            "started_utc": record.get("started_utc"),
            "finished_utc": record.get("finished_utc"),
        }
        if "runs" in record:
            provenance["hand_merged_runs"] = record["runs"]
        if "spawn_micro" in record:
            provenance["spawn_micro_hand_measured"] = {
                **record["spawn_micro"],
                "note": "added by hand; no code in the script produced it",
            }
        if "shim" in record:
            provenance["shim_overhead"] = record["shim"].get("overhead")
        if "segments" in record:
            provenance["segments"] = record["segments"]
        out["files"].append(provenance)
        for name, entry in record["corpora"].items():
            if name in out["corpora"]:
                name = f"{name}@{path.name}"
            corpus = entry["corpus"]
            model = corpus["model"]
            row: dict = {
                "source_file": path.name,
                "complete": entry.get("complete", True),
                "refs": model["refs"],
                "slope_family": slope_family(corpus),
                "seats": corpus["tracked_files"] + corpus["dirs"],
                "mean_ref_line": model["mean_ref_line"],
                "modelled_header_bytes": model["modelled_header_bytes"],
                "refs_at_cap": model["refs_at_cap_at_this_mean"],
            }
            if "walk" in entry:
                row["walk"] = walk_summary(entry["walk"])
            samples = []
            exports = entry.get("git_export_reps") or (
                [entry["git_export"]] if "git_export" in entry else []
            )
            for index, run in enumerate(exports):
                samples.append(agent_row(run, index))
            allowances = []
            stats = []
            for index, rep in enumerate(estate_reps(entry.get("estate"))):
                samples += [agent_row(run, index) for run in rep["passes"]]
                if rep.get("decomposed"):
                    allowances.append(allowance_terms(corpus, rep["decomposed"]))
                if rep.get("stat_shape"):
                    stats.append(rep["stat_shape"])
            row["agent"] = samples
            row["agent_by_label"] = by_label(samples)
            if allowances:
                row["allowance"] = allowances
            if stats:
                row["stat_shape"] = stats
            out["corpora"][name] = row
    out["fits"] = {
        path.name: fits(
            {
                name: row
                for name, row in out["corpora"].items()
                if row["source_file"] == path.name
            }
        )
        for path in paths
    }
    return out


# ---- command line --------------------------------------------------------------------


def selftest() -> int:
    suite = unittest.defaultTestLoader.discover(
        str(HERE), pattern="test_q42_probes.py", top_level_dir=str(HERE)
    )
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    return 0 if result.wasSuccessful() else 1


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)
    corpus = sub.add_parser("corpus", help="build one synthetic repository")
    corpus.add_argument("out", type=Path)
    corpus.add_argument("--shape", choices=("carry", "compact"), default="carry")
    corpus.add_argument("--refs", type=int, default=119_761)
    corpus.add_argument("--oids", type=int, default=1_032)
    corpus.add_argument("--namespaces", type=int, default=157)
    corpus.add_argument("--dirs", type=int, default=64)
    corpus.add_argument("--files-per-dir", type=int, default=64)
    corpus.add_argument("--seed", type=int, default=42)
    corpus.add_argument("--header-target", type=int)
    corpus.add_argument("--cover", action="store_true")
    header = sub.add_parser("header", help="model the v1 header from a repository")
    header.add_argument("repo", type=Path)
    bundle = sub.add_parser("bundle-header", help="measure a bundle header")
    bundle.add_argument("bundle", type=Path)
    walk = sub.add_parser("walk", help="replay the edge-aggressive walk")
    walk.add_argument("repo", type=Path)
    walk.add_argument("--work", type=Path, required=True)
    walk.add_argument("--reps", type=int, default=3)
    walk.add_argument("--storage", action="append", choices=REF_STORAGES)
    walk.add_argument("--spawn-pairs", type=int, default=0)
    spawn = sub.add_parser("spawn-micro", help="prerequisite_commits' children")
    spawn.add_argument("repo", type=Path)
    spawn.add_argument("--pairs", type=int, default=400)
    run = sub.add_parser("run", help="every probe over every corpus")
    run.add_argument("--agent", type=Path, required=True)
    run.add_argument("--work", type=Path, required=True)
    run.add_argument("--out", type=Path, required=True)
    run.add_argument("--suite", choices=("v1", "review"), default="v1")
    run.add_argument("--skip-large", action="store_true")
    run.add_argument("--only", action="append", help="run only this corpus")
    run.add_argument("--resume", action="store_true", help="continue OUT in place")
    summary = sub.add_parser("summarize", help="the doc's figures from run results")
    summary.add_argument("results", type=Path, nargs="+")
    sub.add_parser("selftest", help="run test_q42_probes.py")
    args = parser.parse_args(argv)
    if args.command == "selftest":
        return selftest()
    if args.command == "summarize":
        print(json.dumps(summarize(args.results), indent=2, sort_keys=True))
        return 0
    if args.command == "corpus":
        result = build_corpus(
            args.out,
            shape=args.shape,
            refs=args.refs,
            oids=args.oids,
            namespaces=args.namespaces,
            dirs=args.dirs,
            files_per_dir=args.files_per_dir,
            seed=args.seed,
            header_target=args.header_target,
            cover=args.cover,
        )
    elif args.command == "header":
        result = model_header(args.repo)
    elif args.command == "bundle-header":
        result = read_header(args.bundle)
    elif args.command == "walk":
        result = replay_walk(
            args.repo,
            args.work,
            reps=args.reps,
            storages=tuple(args.storage or ("loose",)),
            spawn_pairs=args.spawn_pairs,
        )
    elif args.command == "spawn-micro":
        result = spawn_micro(args.repo, args.pairs)
    else:
        result = run_all(
            args.agent.resolve(),
            args.work,
            args.out,
            skip_large=args.skip_large,
            only=args.only,
            suite=args.suite,
            resume=args.resume,
        )
        result = {"out": str(args.out), "corpora": sorted(result["corpora"])}
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
