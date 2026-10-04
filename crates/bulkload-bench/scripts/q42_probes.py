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
3. refusal: whether the agent refuses an over-cap inventory as a typed value
   (GIT_INVENTORY_MALFORMED or another typed code). A panic (exit 101 or
   "panicked at"), a bare IO and a FRAME_CODEC are not typed (S4).
4. allowance: the inputs of Q45's byte allowance M = c + a*refs + b*trees,
   from a chained capture bundle decomposed object by object
   (`verify-pack -v` after `bundle unbundle` into a scratch repository).

Corpora (`corpus`): one synthetic repository per shape. `carry` mirrors
#48's blahaj inventory: refs in canonical `refs/carry/v1/neo/<digest>/...`
snapshot namespaces, each re-copying the native refs as they stood, over
few distinct oids. `compact` has the same counts and targets under short
names, so its header stays under the cap and the chained path can run. A
`--header-target` corpus sizes the carry inventory to land a plan-base
item's header just over the cap (the cap-window probe).

Subcommands:
  corpus OUT [--shape carry|compact] [--refs N] [--oids N] [--namespaces N]
             [--dirs N] [--files-per-dir N] [--seed N] [--header-target B]
  header REPO                    model the v1 header from REPO's inventory
  bundle-header BUNDLE           measure a bundle's header as the agent reads it
  walk REPO --work DIR [--reps N]
  run --agent BIN --work DIR --out JSON [--skip-large] [--only NAME ...]
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
import statistics
import subprocess
import sys
import time
import unittest
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
SOURCE_SLUG = "neo"
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


def model_header(repo: Path) -> dict:
    """The v1 header's ref term for REPO, through the agent's mapping."""
    rows = inventory(repo)
    oids = {oid for oid, _ in rows}
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
    }


# ---- the corpus ---------------------------------------------------------------


def split_refs(refs: int, oids: int, namespaces: int) -> dict:
    """How `refs` refs and `oids` distinct targets split into a live native
    set plus `namespaces` snapshot namespaces of native copies and 4 metadata
    refs each (head, staged, worktree, filesystem-v1; 3 distinct parentless
    commits each, `head` names a native commit)."""
    if namespaces < 1 or refs < 5 * namespaces + 2:
        raise ValueError("too few refs for the namespaces")
    native = oids - 3 * namespaces
    if native < 2:
        raise ValueError("oids must exceed 3 * namespaces + 1")
    per_namespace = (refs - 4 * namespaces) // (namespaces + 1)
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
) -> dict:
    """Build one synthetic source repository at `out` (which must not exist)."""
    if shape not in ("carry", "compact"):
        raise ValueError(f"unknown shape {shape}")
    if out.exists():
        raise ProbeError(f"{out} already exists")
    started = time.monotonic()
    if header_target is not None:
        refs = fit_refs_to_header(header_target, oids, namespaces, shape, seed)
    split = split_refs(refs, oids, namespaces)
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


def private_like(repo: Path, work: Path) -> dict:
    """A bare repository reading REPO's objects through alternates, holding
    REPO's refs under the export mapping and one new parentless worktree
    commit (HEAD's tree with one changed blob two levels down)."""
    private = work / "private.git"
    git(work, "init", "--quiet", "--bare", "--template=", str(private))
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
    git(private, "update-ref", "--stdin", "-z", data=bytes(commands))
    git(private, "pack-refs", "--all")
    return {"private": private, "refs": len(rows) + 2, "worktree": worktree}


def replay_walk(repo: Path, work: Path, reps: int = 3) -> dict:
    """Replay `write_excluding_tip_trees`' walk and pack over REPO."""
    work.mkdir(parents=True, exist_ok=True)
    built = private_like(repo, work)
    private = built["private"]
    tips = source_held_tips(repo)
    exclusions = "".join(f"^{oid}\n" for oid in tips).encode()
    record: dict = {
        "private_refs": built["refs"],
        "excluded_tips": len(tips),
        "exclusion_bytes": len(exclusions),
        "reps": reps,
        "load1_before": load1(),
    }
    for mode in ("--objects-edge-aggressive", "--objects-edge"):
        runs = []
        listed = b""
        for _ in range(reps):
            run = measured(
                git_argv(private, "rev-list", mode, "--all", "--stdin"), data=exclusions
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
        record[mode.lstrip("-")] = {
            "runs": runs,
            "median_wall_s": statistics.median(r["wall_s"] for r in runs),
            "median_cpu_s": statistics.median(r["cpu_s"] for r in runs),
            "edge_lines": edges,
            "objects_listed": len(objects),
            "listing_bytes": len(listed),
            "pack_bytes": len(packed["stdout"]),
            "pack_wall_s": packed["wall_s"],
            "pack_cpu_s": packed["cpu_s"],
        }
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


def agent_run(agent: Path, args: list[str], label: str) -> dict:
    before = load1()
    run = measured([str(agent), *args])
    stdout = run.pop("stdout").decode(errors="replace")
    stderr = run.pop("stderr").decode(errors="replace")
    counters = parse_counters(stderr)
    record = {
        "label": label,
        "argv": args,
        **run,
        "load1_before": before,
        "load1_after": load1(),
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
    return record


def bundles_in(directory: Path) -> list[Path]:
    if not directory.is_dir():
        return []
    return sorted(p for p in directory.rglob("*.bundle") if p.is_file())


def decompose_bundle(bundle: Path, source: Path, work: Path) -> dict:
    """Split a capture bundle into the M inputs: header terms (signature,
    prerequisite lines, ref lines) and pack objects by role, from
    `verify-pack -v` after unbundling into a scratch repository that reads the
    source's objects (so a chained bundle's prerequisites are present)."""
    header = read_header(bundle)
    scratch = work / f"decompose-{bundle.stem[:24]}.git"
    git(work, "init", "--quiet", "--bare", "--template=", str(scratch))
    objects = git(
        source, "rev-parse", "--path-format=absolute", "--git-path", "objects"
    )
    (scratch / "objects" / "info" / "alternates").write_bytes(objects)
    git(scratch, "bundle", "unbundle", str(bundle))
    packs = sorted((scratch / "objects" / "pack").glob("*.idx"))
    sizes: dict[str, tuple[str, int, int]] = {}
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
            role, {"objects": 0, "raw_bytes": 0, "in_pack_bytes": 0}
        )
        slot["objects"] += 1
        slot["raw_bytes"] += raw
        slot["in_pack_bytes"] += size
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


def estate_passes(
    agent: Path, corpus: dict, work: Path, *, chained_pass: bool, group: bool
) -> dict:
    """estate-add (one item, or the main checkout plus its linked worktree as
    one plan-base group), then estate-capture passes."""
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
    first = agent_run(agent, capture, "pass-1")
    first["bundles"] = [read_header(b) for b in bundles_in(store)]
    first["diagnosis_bundles"] = [read_header(b) for b in bundles_in(state)]
    record["passes"].append(first)
    if chained_pass and first["classified"]["kind"] == "ok":
        record["mutated"] = mutate(repo)
        seen = {b.name for b in bundles_in(store)}
        second = agent_run(agent, capture, "pass-2-chained")
        fresh = [b for b in bundles_in(store) if b.name not in seen]
        second["bundles"] = [read_header(b) for b in fresh]
        second["prior_sidecars"] = sorted(p.name for p in store.glob("*.prior"))
        record["passes"].append(second)
        if fresh:
            record["decomposed"] = decompose_bundle(fresh[0], repo, work)
    if group and first["classified"]["kind"] == "ok":
        again = agent_run(agent, capture, "pass-2-unchanged")
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


def git_export(agent: Path, repo: Path, work: Path) -> dict:
    # NEW_CAPTURE_DIR must not exist: the agent creates it (mode 0700).
    capture = work / "export"
    run = agent_run(agent, ["git-export", str(repo), str(capture)], "git-export")
    run["bundles"] = [read_header(b) for b in bundles_in(capture)]
    return run


# ---- the whole run ------------------------------------------------------------------


def run_all(
    agent: Path,
    work: Path,
    out: Path,
    *,
    skip_large: bool = False,
    only: list[str] | None = None,
) -> dict:
    if work.exists():
        raise ProbeError(f"{work} already exists; give a fresh work directory")
    private_dir(work)
    record: dict = {
        "schema": SCHEMA,
        "label": LABEL,
        "started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "host": host_record(),
        "agent": {
            "path": str(agent),
            "sha256": hashlib.sha256(agent.read_bytes()).hexdigest(),
        },
        "cap": {"header_bytes": HEADER_CAP, "line_bytes": LINE_CAP},
        "corpora": {},
    }

    def save() -> None:
        out.write_text(json.dumps(record, indent=2, sort_keys=True) + "\n")

    plans = [
        (
            "small-compact",
            {"shape": "compact", "refs": 256, "oids": 64, "namespaces": 2},
            "chain",
        ),
    ]
    if not skip_large:
        plans += [
            ("carry-119761", {"shape": "carry"}, "refuse"),
            ("compact-119761", {"shape": "compact"}, "chain"),
            (
                "carry-window",
                {
                    "shape": "carry",
                    "header_target": HEADER_CAP - 20_000,
                    "linked_worktree": True,
                },
                "group",
            ),
        ]
    if only:
        plans = [plan for plan in plans if plan[0] in only]
    for name, options, mode in plans:
        base = private_dir(work / name)
        corpus = build_corpus(base / "source", **options)
        entry: dict = {"corpus": corpus}
        record["corpora"][name] = entry
        save()
        repo = Path(corpus["path"])
        entry["walk"] = replay_walk(repo, base / "walk", reps=3)
        save()
        if mode == "refuse":
            entry["git_export"] = git_export(agent, repo, base)
            save()
            entry["estate"] = estate_passes(
                agent,
                corpus,
                private_dir(base / "estate"),
                chained_pass=False,
                group=False,
            )
        elif mode == "chain":
            entry["estate"] = estate_passes(
                agent,
                corpus,
                private_dir(base / "estate"),
                chained_pass=True,
                group=False,
            )
        else:
            entry["estate"] = estate_passes(
                agent,
                corpus,
                private_dir(base / "estate"),
                chained_pass=False,
                group=True,
            )
        save()
    record["finished_utc"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    save()
    return record


# ---- the allowance terms --------------------------------------------------------------


def allowance_terms(corpus: dict, decomposed: dict) -> dict:
    """Split one chained capture bundle into Q45's terms, byte for byte.

    c: signature and blank line, the pack's header and trailer, the capture's
       own metadata objects and the metadata ref lines;
    a: the source refs' header lines; p: the prerequisite lines (one per
       source-held distinct tip); b: the changed worktree trees;
    s: the filesystem-v1 rows blob (every seat, rewritten whole);
    content: the changed blobs.
    `check` is the bundle size less their sum, and must be 0."""
    header = decomposed["header"]
    roles = decomposed["by_role"]
    model = corpus["model"]
    source_lines = model["ref_line_bytes"]
    refs = model["refs"]
    sig_blank = (
        header["header_bytes"] - header["prerequisite_bytes"] - header["ref_bytes"]
    )
    metadata_refs = header["ref_bytes"] - source_lines

    def take(role: str) -> dict:
        return roles.get(role, {"objects": 0, "in_pack_bytes": 0, "raw_bytes": 0})

    trees = take("worktree:tree")
    seats_blob = take("filesystem-v1:blob")
    content = take("worktree:blob")
    metadata_objects = sum(
        slot["in_pack_bytes"]
        for role, slot in roles.items()
        if role not in ("worktree:tree", "filesystem-v1:blob", "worktree:blob")
    )
    c = sig_blank + decomposed["pack_fixed_bytes"] + metadata_objects + metadata_refs
    seats = corpus["tracked_files"] + corpus["dirs"]
    terms = {
        "c": c,
        "c_parts": {
            "signature_and_blank": sig_blank,
            "pack_header_and_trailer": decomposed["pack_fixed_bytes"],
            "metadata_objects": metadata_objects,
            "metadata_ref_lines": metadata_refs,
        },
        "a_refs": source_lines,
        "p_tips": header["prerequisite_bytes"],
        "b_trees": trees["in_pack_bytes"],
        "s_seats": seats_blob["in_pack_bytes"],
        "content": content["in_pack_bytes"],
    }
    total = sum(v for k, v in terms.items() if k != "c_parts")
    return {
        "bundle_bytes": decomposed["bundle_bytes"],
        "terms": terms,
        "check": decomposed["bundle_bytes"] - total,
        "units": {
            "refs": refs,
            "a_bytes_per_ref": round(source_lines / refs, 3),
            "tips": header["prerequisite_lines"],
            "p_bytes_per_tip": (
                round(header["prerequisite_bytes"] / header["prerequisite_lines"], 3)
                if header["prerequisite_lines"]
                else None
            ),
            "changed_trees": trees["objects"],
            "tree_entries": trees.get("entries"),
            "b_bytes_per_tree": decomposed["b_bytes_per_changed_tree"],
            "b_bytes_per_entry": decomposed["b_bytes_per_tree_entry"],
            "seats": seats,
            "s_bytes_per_seat_in_pack": round(seats_blob["in_pack_bytes"] / seats, 3),
            "s_bytes_per_seat_raw": round(seats_blob["raw_bytes"] / seats, 3),
        },
    }


def summarize(paths: list[Path]) -> dict:
    """The doc's figures from one or more `run` result files."""
    out: dict = {"label": LABEL, "corpora": {}}
    for path in paths:
        record = json.loads(path.read_text())
        for name, entry in record["corpora"].items():
            corpus = entry["corpus"]
            row: dict = {
                "refs": corpus["model"]["refs"],
                "mean_ref_line": corpus["model"]["mean_ref_line"],
                "modelled_header_bytes": corpus["model"]["modelled_header_bytes"],
                "refs_at_cap": corpus["model"]["refs_at_cap_at_this_mean"],
            }
            walk = entry.get("walk", {})
            for mode in ("objects-edge-aggressive", "objects-edge"):
                if mode in walk:
                    row[mode] = {
                        k: walk[mode][k]
                        for k in (
                            "median_wall_s",
                            "median_cpu_s",
                            "objects_listed",
                            "pack_bytes",
                        )
                    }
            runs = []
            if "git_export" in entry:
                runs.append(entry["git_export"])
            runs += entry.get("estate", {}).get("passes", [])
            row["agent"] = [
                {
                    "label": run["label"],
                    "kind": run["classified"]["kind"],
                    "codes": run["classified"]["codes"],
                    "exit": run["exit"],
                    "wall_s": run["wall_s"],
                    "cpu_s": run["cpu_s"],
                    "load1_before": run["load1_before"],
                    "bundles": [
                        (b["header_bytes"], b["agent_reader"])
                        for b in run.get("bundles", [])
                    ],
                }
                for run in runs
            ]
            decomposed = entry.get("estate", {}).get("decomposed")
            if decomposed:
                row["allowance"] = allowance_terms(corpus, decomposed)
            out["corpora"][name] = row
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
    header = sub.add_parser("header", help="model the v1 header from a repository")
    header.add_argument("repo", type=Path)
    bundle = sub.add_parser("bundle-header", help="measure a bundle header")
    bundle.add_argument("bundle", type=Path)
    walk = sub.add_parser("walk", help="replay the edge-aggressive walk")
    walk.add_argument("repo", type=Path)
    walk.add_argument("--work", type=Path, required=True)
    walk.add_argument("--reps", type=int, default=3)
    run = sub.add_parser("run", help="every probe over every corpus")
    run.add_argument("--agent", type=Path, required=True)
    run.add_argument("--work", type=Path, required=True)
    run.add_argument("--out", type=Path, required=True)
    run.add_argument("--skip-large", action="store_true")
    run.add_argument("--only", action="append", help="run only this corpus")
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
        )
    elif args.command == "header":
        result = model_header(args.repo)
    elif args.command == "bundle-header":
        result = read_header(args.bundle)
    elif args.command == "walk":
        result = replay_walk(args.repo, args.work, reps=args.reps)
    else:
        result = run_all(
            args.agent.resolve(),
            args.work,
            args.out,
            skip_large=args.skip_large,
            only=args.only,
        )
        result = {"out": str(args.out), "corpora": sorted(result["corpora"])}
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
