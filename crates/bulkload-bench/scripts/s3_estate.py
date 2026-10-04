#!/usr/bin/env python3
"""S3 on the estate corpus: a first pass, unchanged reruns and delta reruns.

Sprint 2 lane A. Rulings: OI-1003-Q35 (the admissible S3 evidence is the
byte counters `source_bytes_read`, content bytes, `census_walks` and pack
bytes, plus the rusage CPU ratio measured on sting; wall time is
informational; running estate verbs on the synthetic corpus under scratch
is a test, not an R-N56 estate operation), OI-1003-Q18 (the S3 delta
inequalities), OI-1003-Q36 (count SQLite -shm creation), OI-1003-Q15 (the
git engine is chosen on these numbers), R-N13.

`run` works under one private WORK directory and never deletes anything:
  1. Generates the estate corpus (estate_corpus.py, OI-1003-Q19) in place at
     WORK/estate. A corpus is not relocatable, so it is never copied.
  2. Plans the git half as two estate plans, so the counters of each are
     separable: plan `rest` holds every item but git/history-heavy (main
     checkouts, linked worktrees, the nest and the bare mirror), plan
     `history` holds git/history-heavy alone. Both use one CORPUS and one
     PRIVATE_STATE each, kept across passes.
  3. Runs six passes: `first`, `rerun-1` to `rerun-3` (nothing changed), then
     `mutate-1` and `mutate-10`: `estate_corpus.py mutate WORK/estate N`, a
     wait until the sidecar's mutated_at_ns + settle_ns, and a pass. Each
     pass drives:
       - the file half: `bulkload-agent copy` once per top-level directory of
         the corpus other than git/ (copy has no exclude, and the walk has no
         .git partition), each with its own destination and state kept across
         passes. Top-level regular files lie outside every area and are not
         carried (listed as `uncovered`);
       - the SQLite half: `bulkload-agent snapshot` of every SQLite database
         outside git/ into a new private directory per pass (copy refuses
         those seats with SQLITE_STATE_CHANGED; snapshot keeps no ledger, so
         it reads the whole database every pass);
       - the git half: `estate-capture` of plan `rest`, then plan `history`.
     After the v1 pass, the v2 projection: `git-carry-estimate` of every
     repository against a model of what a v2 destination would hold (an
     empty bare repository for `first`; a `clone --mirror --no-local` of the
     source taken after the pass before, plus its stash entries, for the
     reruns). carry_v2 itself is never built or run.
  4. Records, per pass and per child: exit status, the parsed key=value
     lines (tolerantly: unknown keys are kept, malformed tokens skipped), the
     refusals, wall time, rusage user and system CPU (RUSAGE_CHILDREN deltas;
     the children run one at a time) and the priority class the counters
     line reports; per pass: load1 before and after, MemAvailable, CPU
     utilisation, and an S2 stat diff of the corpus (estate_corpus.s2_rows)
     with its -shm creations (OI-1003-Q36), git object freshenings and other
     changes.
  5. Evaluates S3 per pass, per half and per subset (all; without data/;
     without git/history-heavy; without both):
       - unchanged reruns: 0 content bytes read and received, 0 pack bytes,
         census_walks one per censused item, and the rusage CPU ratio
         <= 10 % of the first pass (OI-1003-Q35); the wall ratio is reported
         beside it, informational;
       - delta reruns, the two OI-1003-Q18 inequalities against the round's
         sidecar. Each changed seat counts in one half only: SQLite databases
         and their companions belong to the SQLite half, never the file half.
         Inequality 1: source content bytes read <= the changed or racy
         seats (non-SQLite `walk` seats of the copied areas for the file
         half; `worktree` seats for the git half, whose receipts count
         worktree streaming only, so the git children's object-store reads
         are reported beside it, pending a ruling). Inequality 2: wire
         content bytes <= the absent chunks (per changed file: the changed
         ranges widened by one maximum CDC chunk before and two after,
         capped at the file). The file half bounds only the seats copy
         carried. The git half adds each changed repository's new objects
         once, and is reported at chunk granularity (the absent-chunk bound
         of each changed worktree seat) and at object granularity (each
         changed worktree seat whole);
       - a copy refusal on a seat the round changed whose destination already
         holds an older output is the typed result `blocked by WP0(d)`
         (no-clobber; OI-1003-Q18 (d) superseding publish is not built). The
         file half's inequality 2 is then `n/a: blocked by WP0(d)`, since
         the delta did not converge. Targets are never deleted.
  Everything goes to WORK/s3-estate.json; each child's raw output is kept
  under WORK/logs/. `report` renders the JSON as Markdown tables, and
  `evaluate` re-runs the evaluation of a recorded run into a new JSON.

`build` exports REV with `git archive` into OUT/src-<sha12> and runs
`cargo build --release --locked -p bulkload-agent` in `nix develop` of that
tree, at nice 19 with CARGO_BUILD_JOBS (default 4), under the lanes' shared
flock, with its own CARGO_TARGET_DIR. A binary with a matching .sha256 record
is reused.

Usage:
  s3_estate.py build --out DIR [--repo R] [--rev origin/main] [--jobs 4]
                     [--lock PATH]
  s3_estate.py run --agent BIN --work WORK [--scale small|estate] [--seed S]
                   [--jobs 2] [--min-available-gib 6] [--build-json F]
  s3_estate.py report WORK/s3-estate.json
  s3_estate.py evaluate WORK/s3-estate.json --out NEW.json [--seal SEAL]

Run `run` inside the repository's devShell (`nix develop .#default`): the
corpus identity needs its git, SQLite and zstd. Exit: 0 complete (S3
verdicts are in the JSON, pass or fail), 2 refused before any pass, 3 a pass
could not run, 4 build failure.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import platform
import re
import resource
import shutil
import stat
import subprocess
import sys
import tarfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import estate_corpus as ec  # noqa: E402

RULINGS = "OI-1003-Q15, OI-1003-Q18, OI-1003-Q35, OI-1003-Q36, R-N13"
LABEL = "informational, ungated"
FORMAT = "bulkload-s3-estate-v1"
GIB = 1 << 30
PASSES = (
    ("first", 0),
    ("rerun-1", 0),
    ("rerun-2", 0),
    ("rerun-3", 0),
    ("mutate-1", 1),
    ("mutate-10", 10),
)
HISTORY = ec.HISTORY_REPO
SUBSETS = {
    "all": (False, False),
    "without-data": (True, False),
    "without-history": (False, True),
    "without-both": (True, True),
}
# The unchanged clause's <= 10 % of the first pass (S3; OI-1003-Q6, Q10).
# OI-1003-Q35 makes the rusage CPU ratio the admissible measure; wall time is
# informational.
CPU_RATIO = 0.10
WALL_RATIO = 0.10
SETTLE_MARGIN_S = 1.0
# Refusal codes of a no-clobber publication: the destination already holds an
# output this run would have to replace (OI-1003-Q18 (d) is not built).
NO_CLOBBER = ("GIT_DESTINATION_OCCUPIED", "DESTINATION_OCCUPIED")
SQLITE_ROUTED = ("SQLITE_STATE_CHANGED",)
# Counters summed per half; every other key is kept per child only.
SUM_SKIP = ("verb", "side", "scope", "priority", "priority_from")


# ---- parsing (tolerant) ---------------------------------------------------------


def tokens(line: str) -> list[str]:
    """Whitespace-separated tokens; a double-quoted run (with backslash
    escapes) never splits, so `source="a b"` and `reason=Some("x y")` stay
    whole."""
    out, cur, quoted, escaped = [], [], False, False
    for ch in line:
        if escaped:
            cur.append(ch)
            escaped = False
        elif ch == "\\" and quoted:
            cur.append(ch)
            escaped = True
        elif ch == '"':
            cur.append(ch)
            quoted = not quoted
        elif ch.isspace() and not quoted:
            if cur:
                out.append("".join(cur))
                cur = []
        else:
            cur.append(ch)
    if cur:
        out.append("".join(cur))
    return out


def value(text: str) -> object:
    if re.fullmatch(r"-?\d+", text):
        return int(text)
    if re.fullmatch(r"-?\d+\.\d*", text):
        return float(text)
    if len(text) >= 2 and text[0] == text[-1] == '"':
        return unquote(text[1:-1])
    return text


def unquote(text: str) -> str:
    """Undo Rust's Debug escapes for the common cases; keep the rest."""
    return re.sub(r'\\(["\\])', r"\1", text)


def pairs(line: str) -> dict[str, object]:
    """key=value pairs of one line. Tokens without `=` are skipped; a
    repeated key keeps its last value."""
    row: dict[str, object] = {}
    for token in tokens(line):
        key, sep, raw = token.partition("=")
        if sep and key and re.fullmatch(r"[A-Za-z0-9_.-]+", key):
            row[key] = value(raw)
    return row


def tagged(text: str, tag: str) -> list[dict[str, object]]:
    """Every line whose first token is `tag` (e.g. `counters`)."""
    return [
        pairs(line[len(tag) :])
        for line in text.splitlines()
        if line.startswith(tag + " ")
    ]


def counters(text: str) -> dict[str, object]:
    """The `counters` line(s) of a verb, merged (a verb prints one)."""
    merged: dict[str, object] = {}
    for row in tagged(text, "counters"):
        merged.update(row)
    return merged


def transfer_line(text: str) -> dict[str, object]:
    """copy's summary line: completed= reused= bytes_received= ..."""
    for line in text.splitlines():
        if line.startswith("completed="):
            return pairs(line)
    return {}


def receipts(text: str) -> list[dict[str, object]]:
    """estate-capture item lines (`item=N ... outcome=...`); drift and nest
    lines (no outcome=) are attached to their item."""
    out: list[dict[str, object]] = []
    for line in text.splitlines():
        if not line.startswith("item="):
            continue
        row = pairs(line)
        if "outcome" in row:
            row["extra"] = []
            out.append(row)
        elif out and out[-1].get("item") == row.get("item"):
            out[-1]["extra"].append(line)
    return out


def unescape_ascii(text: str) -> bytes:
    """Invert Rust's `escape_ascii` (\\t \\r \\n \\\\ \\' \\" \\xNN)."""
    out = bytearray()
    i = 0
    simple = {"t": 9, "r": 13, "n": 10, "\\": 92, "'": 39, '"': 34}
    while i < len(text):
        ch = text[i]
        if ch == "\\" and i + 1 < len(text):
            nxt = text[i + 1]
            if nxt in simple:
                out.append(simple[nxt])
                i += 2
                continue
            if nxt == "x" and re.fullmatch(r"[0-9a-fA-F]{2}", text[i + 2 : i + 4]):
                out.append(int(text[i + 2 : i + 4], 16))
                i += 4
                continue
        out += ch.encode("utf-8", "surrogateescape")
        i += 1
    return bytes(out)


def refusals(stderr: str) -> list[tuple[str, str]]:
    """copy's `refused <path>: CODE` lines as (esc path, code)."""
    out = []
    for line in stderr.splitlines():
        if not line.startswith("refused "):
            continue
        path, sep, code = line[len("refused ") :].rpartition(": ")
        if sep:
            rel = os.fsdecode(unescape_ascii(path))
            out.append((ec.esc(rel), code.strip()))
    return out


def final_refusal(stderr: str) -> str | None:
    for line in reversed(stderr.splitlines()):
        if line.startswith("bulkload-agent: refused: "):
            return line[len("bulkload-agent: refused: ") :].strip()
    return None


def estimate_blocks(text: str) -> list[dict[str, object]]:
    """git-carry-estimate's per-pair blocks, separated by blank lines."""
    blocks, cur = [], {}
    for line in text.splitlines():
        if not line.strip():
            if cur:
                blocks.append(cur)
                cur = {}
            continue
        cur.update(pairs(line))
    if cur:
        blocks.append(cur)
    return blocks


def number(row: dict, key: str) -> int:
    got = row.get(key, 0)
    return got if isinstance(got, int) else 0


# ---- bounds (OI-1003-Q18) --------------------------------------------------------


def absent_bound(
    pre: int | None, post: int | None, ranges: list[list[int]], cdc_max: int
) -> int:
    """Upper bound on the absent CDC chunks of one changed file.

    A new file is absent whole; a deleted one sends nothing. Otherwise every
    chunk boundary before a changed range is unchanged, so the chunks that
    can differ start at most one maximum chunk before the range, and the
    chunker resynchronises within two maximum chunks after it. The union of
    those windows, capped at the file, is the bound. A changed file with no
    recorded range is bounded by its whole size.
    """
    if post is None:
        return 0
    if pre is None or not ranges:
        return post
    spans = sorted(
        (max(0, start - cdc_max), min(post, end + 2 * cdc_max))
        for start, end in ranges
        if end >= start
    )
    total, cur_start, cur_end = 0, None, None
    for start, end in spans:
        if cur_end is None or start > cur_end:
            if cur_end is not None:
                total += cur_end - cur_start
            cur_start, cur_end = start, end
        else:
            cur_end = max(cur_end, end)
    if cur_end is not None:
        total += cur_end - cur_start
    return min(total, post)


def file_ops(sidecar: dict) -> dict[str, dict]:
    """Per changed file: pre size, post size and ranges, from the operations
    (a sqlite op contributes each store it changed)."""
    out: dict[str, dict] = {}
    for op in sidecar.get("operations", []):
        for part in [op, *op.get("stores", [])]:
            path = part.get("path")
            if not path or "pre_size" not in part:
                continue
            row = out.setdefault(
                path,
                {
                    "pre": part.get("pre_size"),
                    "post": part.get("post_size"),
                    "ranges": [],
                },
            )
            row["post"] = part.get("post_size")
            row["ranges"] += [list(r) for r in part.get("ranges", [])]
    return out


def area_of(rel: str) -> str | None:
    """The top-level directory a path belongs to, or None for a root file."""
    head, sep, _ = rel.partition("/")
    return head if sep else None


def is_sqlite_seat(rel: str, sqlite: set[str]) -> bool:
    """A SQLite database, or its -wal, -shm or -journal companion."""
    if rel in sqlite:
        return True
    return (
        rel.endswith(("-wal", "-shm", "-journal")) and rel.rsplit("-", 1)[0] in sqlite
    )


def delta_bounds(
    sidecar: dict, areas: list[str], sqlite: set[str], items_by_name: dict
) -> dict:
    """The OI-1003-Q18 bounds of one round, split by half and subset key.

    Each changed seat is counted in exactly one half. A SQLite database and
    its companions belong to the SQLite half only: copy refuses them
    (SQLITE_STATE_CHANGED), so they are never in the file half's bounds.

    The git half is reported at two granularities. `chunk` applies the file
    half's absent-chunk bound (`absent_bound`) to each changed worktree seat;
    `object` counts each changed worktree seat whole, as a new blob. Both add
    each repository's new objects once per round, however many of its items
    changed.
    """
    cdc_max = sidecar.get("cdc_bytes", {}).get("max", 256 * 1024)
    classes = sidecar.get("reads_allowed_class", {})
    sizes = sidecar.get("reads_allowed_sizes", {})
    ops = file_ops(sidecar)
    items_list = list(items_by_name.values())
    file_read: dict[str, int] = {}
    file_wire: dict[str, int] = {}
    file_wire_by_seat: dict[str, int] = {}
    sqlite_changed: dict[str, int] = {}
    uncovered: list[str] = []
    chunk_by_item: dict[str, int] = {}
    object_store_by_item: dict[str, int] = {}
    for rel, cls in classes.items():
        size = int(sizes.get(rel, 0))
        op = ops.get(rel)
        chunk = (
            absent_bound(op["pre"], op["post"], op["ranges"], cdc_max) if op else size
        )
        if cls in ("worktree", "git-objects"):
            _, owner, _ = ec.attribute(rel, "f", items_list)
            if owner is None:
                continue
            if cls == "worktree":
                chunk_by_item[owner] = chunk_by_item.get(owner, 0) + chunk
            else:
                object_store_by_item[owner] = object_store_by_item.get(owner, 0) + size
            continue
        if cls != "walk":
            continue
        area = area_of(rel)
        if area is None or area not in areas:
            uncovered.append(rel)
            continue
        if is_sqlite_seat(rel, sqlite):
            sqlite_changed[rel] = size
            continue
        file_read[area] = file_read.get(area, 0) + size
        file_wire[area] = file_wire.get(area, 0) + chunk
        file_wire_by_seat[rel] = chunk
    sqlite_ranges = sum(
        sum(e - s for s, e in ops[rel]["ranges"]) if rel in ops else size
        for rel, size in sqlite_changed.items()
    )
    worktree = sidecar.get("reads_by_class", {}).get("worktree", {}).get("items", {})
    new_objects: dict[str, int] = {}
    for repo, row in sidecar.get("git_objects", {}).items():
        new_objects[repo] = sum(
            tip.get("new_objects", {}).get("bytes", 0) for tip in row.get("tips", [])
        )
    git_read: dict[str, int] = {}
    git_chunk: dict[str, int] = {}
    item_repo: dict[str, str] = {}
    for change in sidecar.get("items", {}).get("changed", []):
        name = change["item"]
        item = items_by_name.get(name, {"repo": name, "kind": "main"})
        if item.get("kind") == "mirror":
            continue
        git_read[name] = int(worktree.get(name, {}).get("bytes", 0))
        git_chunk[name] = chunk_by_item.get(name, 0)
        item_repo[name] = item["repo"]
    return {
        "cdc_max": cdc_max,
        "file_read_by_area": file_read,
        "file_wire_by_area": file_wire,
        "file_wire_by_seat": file_wire_by_seat,
        "sqlite_changed": sqlite_changed,
        "sqlite_changed_range_bytes": sqlite_ranges,
        "uncovered_walk_seats": sorted(uncovered, key=str.encode),
        "git_read_by_item": git_read,
        "git_worktree_object_bytes_by_item": dict(git_read),
        "git_worktree_chunk_bound_by_item": git_chunk,
        "git_item_repo": item_repo,
        "git_new_object_bytes_by_changed_repo": {
            repo: new_objects.get(repo, 0) for repo in sorted(set(item_repo.values()))
        },
        "git_object_store_seat_bytes_by_item": object_store_by_item,
        "new_object_bytes_by_repo": new_objects,
        "census_walks_expected": sidecar.get("items", {}).get("census_walks_expected"),
    }


# ---- host facts -----------------------------------------------------------------


def meminfo() -> dict[str, int]:
    out = {}
    try:
        for line in Path("/proc/meminfo").read_text().splitlines():
            key, _, rest = line.partition(":")
            if key in ("MemTotal", "MemFree", "MemAvailable"):
                out[key] = int(rest.split()[0]) * 1024
    except OSError:
        pass
    return out


def load1() -> float:
    return round(os.getloadavg()[0], 2)


def now() -> str:
    return dt.datetime.now(dt.UTC).strftime("%Y-%m-%dT%H:%M:%SZ")


def say(text: str) -> None:
    print(f"s3-estate {text}", flush=True)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def private_dir(path: Path) -> Path:
    path.mkdir(parents=True, exist_ok=True)
    path.chmod(0o700)
    return path


# ---- children -------------------------------------------------------------------


class Runner:
    """Runs one child at a time and keeps its raw output under WORK/logs."""

    def __init__(self, work: Path, env: dict[str, str]):
        self.logs = private_dir(work / "logs")
        self.env = env

    def run(self, step: str, argv: list[str], cwd: Path | None = None) -> dict:
        before = resource.getrusage(resource.RUSAGE_CHILDREN)
        start = time.monotonic_ns()
        done = subprocess.run(
            argv,
            capture_output=True,
            env=self.env,
            cwd=cwd,
            check=False,
        )
        wall = time.monotonic_ns() - start
        after = resource.getrusage(resource.RUSAGE_CHILDREN)
        out = done.stdout.decode("utf-8", "surrogateescape")
        err = done.stderr.decode("utf-8", "surrogateescape")
        base = self.logs / step
        base.parent.mkdir(parents=True, exist_ok=True)
        base.with_name(base.name + ".out").write_text(out, errors="surrogateescape")
        base.with_name(base.name + ".err").write_text(err, errors="surrogateescape")
        user = after.ru_utime - before.ru_utime
        system = after.ru_stime - before.ru_stime
        return {
            "step": step,
            "argv": [str(a) for a in argv],
            "exit": done.returncode,
            "wall_ns": wall,
            "user_s": round(user, 6),
            "sys_s": round(system, 6),
            "cpu_s": round(user + system, 6),
            "maxrss_kib": after.ru_maxrss,
            "stdout": out,
            "stderr": err,
        }


def compact(child: dict) -> dict:
    """A child's record without its raw output (kept in WORK/logs)."""
    return {k: v for k, v in child.items() if k not in ("stdout", "stderr")}


def add_counts(total: dict, row: dict) -> None:
    for key, got in row.items():
        if key in SUM_SKIP or not isinstance(got, int):
            continue
        total[key] = total.get(key, 0) + got


def priority_of(rows: list[dict]) -> str:
    seen = sorted(
        {f"{r.get('priority', '?')}/{r.get('priority_from', '?')}" for r in rows}
    )
    return ",".join(seen) if seen else "unknown"


# ---- the corpus and its plans -----------------------------------------------------


def corpus_script() -> Path:
    return Path(ec.__file__).resolve()


def sqlite_databases(corpus: Path) -> list[str]:
    """SQLite databases outside git/, by header (the -wal rides with its db)."""
    found = []
    for rel, info in ec.walk(corpus):
        if rel == "git" or rel.startswith("git/") or not stat.S_ISREG(info.st_mode):
            continue
        if ec.read_head(corpus / rel, 16) == ec.SQLITE_MAGIC:
            found.append(rel)
    return sorted(found, key=str.encode)


def areas_of(corpus: Path) -> tuple[list[str], list[str]]:
    areas, uncovered = [], []
    for entry in sorted(os.scandir(corpus), key=lambda e: os.fsencode(e.name)):
        if entry.is_dir(follow_symlinks=False):
            if entry.name != "git":
                areas.append(entry.name)
        else:
            uncovered.append(entry.name)
    return areas, uncovered


def plans(seal: dict) -> dict[str, list[dict]]:
    items = ec.estate_items(seal)
    return {
        "rest": [i for i in items if i["item"] != HISTORY],
        "history": [i for i in items if i["item"] == HISTORY],
    }


def estimate_repos(seal: dict) -> list[str]:
    repos = []
    for repo in seal["repos"]:
        repos.append(repo["path"])
        if repo.get("nested"):
            repos.append(repo["nested"])
        if repo.get("bare"):
            repos.append(repo["bare"])
    return sorted(repos, key=str.encode)


def new_bundles(out_dir: Path, seen: set[str], rows: list[dict], corpus: Path) -> dict:
    """Bytes of the bundles a capture added to CORPUS, per item.

    A capture bundle is named `<item id>-<digest>.bundle`; a group's shared
    base is `shared-<digest>.bundle`. The item id maps to its source through
    the receipts.
    """
    names = {}
    root = str(corpus) + "/"
    for row in rows:
        source = str(row.get("source", ""))
        names[str(row.get("item"))] = (
            source[len(root) :] if source.startswith(root) else source
        )
    out: dict[str, int] = {}
    for entry in os.scandir(out_dir):
        if entry.name in seen or not entry.name.endswith(".bundle"):
            continue
        head = entry.name.split("-", 1)[0]
        key = "shared-base" if head == "shared" else names.get(head, head)
        out[key] = out.get(key, 0) + entry.stat(follow_symlinks=False).st_size
    return out


def model_name(rel: str) -> str:
    return rel.replace("/", "__") + ("" if rel.endswith(".git") else ".git")


# ---- one pass -------------------------------------------------------------------


def git_env(work: Path) -> dict[str, str]:
    """The children's environment: no inherited GIT_* and an empty global git
    config, so the operator's own git configuration never reaches the corpus."""
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
    empty = work / "gitconfig-empty"
    if not empty.exists():
        empty.write_text("")
    env["GIT_CONFIG_GLOBAL"] = str(empty)
    env["GIT_CONFIG_NOSYSTEM"] = "1"
    env["LC_ALL"] = "C"
    return env


def digest(path: Path) -> str | None:
    try:
        return hashlib.blake2b(path.read_bytes(), digest_size=16).hexdigest()
    except OSError:
        return None


def s2_summary(before: dict, after: dict, sqlite: list[str]) -> dict:
    added, removed, modified = ec.diff_rows(before, after)
    shm_added = [p for p in added if p.endswith("-shm")]
    shm_touched = [p for p in modified if p.endswith("-shm")]
    freshened, other = [], []
    for rel in modified:
        if rel.endswith("-shm"):
            continue
        moved = [n for n, a, b in zip(ec.S2_FIELDS, before[rel], after[rel]) if a != b]
        parts = rel.split("/")
        in_objects = any(
            ec.is_gitdir_name(p) and parts[i + 1 : i + 2] == ["objects"]
            for i, p in enumerate(parts)
        )
        if in_objects and set(moved) <= {"mtime_ns", "ctime_ns"}:
            freshened.append(rel)
        else:
            other.append(f"{rel} {','.join(moved)}")
    other += [f"+ {p}" for p in added if not p.endswith("-shm")]
    other += [f"- {p}" for p in removed]
    return {
        "added": len(added),
        "removed": len(removed),
        "changed": len(modified),
        "shm_created": shm_added,
        "shm_touched": shm_touched,
        "objects_freshened": len(freshened),
        "objects_freshened_sample": freshened[:8],
        "other": len(other),
        "other_sample": other[:40],
        "sqlite_paths": sqlite,
    }


class Measure:
    def __init__(self, args: argparse.Namespace):
        self.agent = Path(args.agent).resolve()
        self.work = Path(args.work).absolute()
        self.scale = args.scale
        self.seed = args.seed
        self.jobs = str(args.jobs)
        self.dest = self.work / "estate"
        self.corpus = self.dest / "corpus"
        self.runner: Runner | None = None
        self.record: dict = {}

    # -- setup --

    def preflight(self, min_available_gib: float) -> str | None:
        if not os.access(self.agent, os.X_OK):
            return f"agent binary not executable: {self.agent}"
        if self.work.exists():
            return f"WORK must be new: {self.work}"
        if self.scale == "estate":
            mem = meminfo()
            if mem.get("MemAvailable", 0) < min_available_gib * GIB:
                return (
                    f"scale estate needs MemAvailable >= {min_available_gib} GiB; "
                    f"have {mem.get('MemAvailable', 0) / GIB:.1f} GiB"
                )
        return None

    def generate(self) -> dict:
        assert self.runner is not None
        child = self.runner.run(
            "setup/generate",
            [
                sys.executable,
                str(corpus_script()),
                "generate",
                str(self.dest),
                "--seed",
                self.seed,
                "--scale",
                self.scale,
            ],
        )
        line = next(
            (ln for ln in child["stdout"].splitlines() if "generated=" in ln), ""
        )
        row = pairs(line)
        return {"child": compact(child), "line": row}

    def plan(self, seal: dict) -> dict:
        assert self.runner is not None
        out = {}
        for name, items in plans(seal).items():
            plan = self.work / "plans" / f"{name}.json"
            private_dir(plan.parent)
            argv = [str(self.agent), "estate-add-batch", str(plan)]
            for item in items:
                argv += [
                    str(self.corpus / item["item"]),
                    str(self.work / "dest" / "git" / item["item"]),
                    "-",
                ]
            child = self.runner.run(f"setup/estate-add-batch-{name}", argv)
            out[name] = {
                "plan": str(plan),
                "items": [i["item"] for i in items],
                "censused": [i["item"] for i in items if i["kind"] != "mirror"],
                "child": compact(child),
            }
            private_dir(self.work / "state" / "git" / name)
            private_dir(self.work / "corpus-out" / name)
        return out

    def model_empty(self, repos: list[str]) -> Path:
        root = private_dir(self.work / "v2-model" / "empty")
        for rel in repos:
            target = root / model_name(rel)
            if not target.exists():
                subprocess.run(
                    ["git", "init", "--quiet", "--bare", str(target)],
                    env=self.runner.env,
                    check=True,
                )
        return root

    def model_mirror(self, repos: list[str], label: str) -> dict:
        """What a v2 destination holds after a pass: every ref and stash entry
        of the source, cloned through upload-pack (read-only on the source)."""
        root = private_dir(self.work / "v2-model" / label)
        env = self.runner.env
        stashes = 0
        start = time.monotonic_ns()
        for rel in repos:
            source = self.corpus / rel
            target = root / model_name(rel)
            subprocess.run(
                [
                    "git",
                    "clone",
                    "--quiet",
                    "--mirror",
                    "--no-local",
                    str(source),
                    str(target),
                ],
                env=env,
                check=True,
            )
            listed = subprocess.run(
                [
                    "git",
                    "-C",
                    str(source),
                    "reflog",
                    "show",
                    "--format=%H",
                    "refs/stash",
                ],
                env=env,
                capture_output=True,
                text=True,
                check=False,
            )
            oids = listed.stdout.split() if listed.returncode == 0 else []
            for index, oid in enumerate(oids):
                subprocess.run(
                    [
                        "git",
                        "-C",
                        str(target),
                        "fetch",
                        "--quiet",
                        "--no-tags",
                        "--upload-pack",
                        "git -c uploadpack.allowAnySHA1InWant=true upload-pack",
                        str(source),
                        f"{oid}:refs/v2-model/stash/{index}",
                    ],
                    env=env,
                    check=True,
                )
                stashes += 1
        return {
            "root": str(root),
            "stash_entries": stashes,
            "seconds": round((time.monotonic_ns() - start) / 1e9, 1),
        }

    # -- a pass --

    def file_half(self, label: str, areas: list[str]) -> dict:
        per_area = {}
        for area in areas:
            dest = self.work / "dest" / "files" / area
            dest.mkdir(parents=True, exist_ok=True)
            state = private_dir(self.work / "state" / "files" / area)
            child = self.runner.run(
                f"{label}/copy-{area}",
                [
                    str(self.agent),
                    "copy",
                    str(self.corpus / area),
                    str(dest),
                    str(state / "source"),
                    str(state / "destination"),
                ],
            )
            root = ec.esc(str(self.corpus)) + "/"
            refused = []
            for path, code in refusals(child["stderr"]):
                if path.startswith(root):
                    path = path[len(root) :]
                elif not path.startswith(area + "/"):
                    path = f"{area}/{path}"
                refused.append((path, code))
            per_area[area] = {
                **compact(child),
                "transfer": transfer_line(child["stdout"]),
                "counters": counters(child["stdout"]),
                "refused": refused,
                "final_refusal": final_refusal(child["stderr"]),
            }
        return per_area

    def sqlite_half(self, label: str, databases: list[str]) -> dict:
        out_dir = private_dir(self.work / "state" / "sqlite" / label)
        per_db = {}
        for index, rel in enumerate(databases):
            source = self.corpus / rel
            sizes = {}
            for suffix in ("", "-wal"):
                companion = source.with_name(source.name + suffix)
                sizes[suffix] = companion.stat().st_size if companion.exists() else 0
            output = out_dir / f"{index:02d}.sqlite"
            child = self.runner.run(
                f"{label}/snapshot-{index:02d}",
                [str(self.agent), "snapshot", str(source), str(output)],
            )
            per_db[rel] = {
                **compact(child),
                "counters": counters(child["stderr"]),
                "final_refusal": final_refusal(child["stderr"]),
                "db_bytes": sizes[""],
                "wal_bytes": sizes["-wal"],
                "output_bytes": output.stat().st_size if output.exists() else 0,
            }
        return per_db

    def git_half(self, label: str, planned: dict) -> dict:
        per_plan = {}
        for name, plan in planned.items():
            out_dir = self.work / "corpus-out" / name
            seen = {e.name for e in os.scandir(out_dir)}
            child = self.runner.run(
                f"{label}/estate-capture-{name}",
                [
                    str(self.agent),
                    "estate-capture",
                    plan["plan"],
                    str(self.work / "state" / "git" / name),
                    str(self.work / "corpus-out" / name),
                    self.jobs,
                ],
            )
            rows = receipts(child["stdout"])
            outcomes: dict[str, int] = {}
            for row in rows:
                outcome = str(row.get("outcome"))
                outcomes[outcome] = outcomes.get(outcome, 0) + 1
            per_plan[name] = {
                **compact(child),
                "counters": counters(child["stderr"]),
                "receipts": rows,
                "bundles": new_bundles(out_dir, seen, rows, self.corpus),
                "outcomes": outcomes,
                "source_bytes_read": sum(number(r, "source_bytes_read") for r in rows),
                "final_refusal": final_refusal(child["stderr"]),
            }
        return per_plan

    def v2_half(self, label: str, repos: list[str], model: Path) -> dict:
        state = private_dir(self.work / "state" / "estimate")
        groups = {
            "rest": [r for r in repos if r != HISTORY],
            "history": [r for r in repos if r == HISTORY],
        }
        out = {}
        for name, members in groups.items():
            argv = [str(self.agent), "git-carry-estimate", "--state-dir", str(state)]
            for rel in members:
                argv += [str(self.corpus / rel), str(model / model_name(rel))]
            child = self.runner.run(f"{label}/estimate-{name}", argv)
            blocks = estimate_blocks(child["stdout"])
            out[name] = {
                **compact(child),
                "counters": counters(child["stderr"]),
                "blocks": blocks,
                "missing_thin_pack_bytes": sum(
                    number(b, "missing_thin_pack_bytes") for b in blocks
                ),
                "missing_objects": sum(number(b, "missing_objects") for b in blocks),
                "source_history_bytes": sum(
                    number(b, "source_history_bytes") for b in blocks
                ),
                "refused": [
                    (b.get("source"), b.get("refused"))
                    for b in blocks
                    if "refused" in b
                ],
            }
        return out

    def one_pass(self, label: str, ctx: dict, model: Path) -> dict:
        say(f"pass {label} start load1={load1()}")
        record: dict = {
            "label": label,
            "started": now(),
            "load1_before": load1(),
            "mem_before": meminfo(),
            "harness_nice": os.nice(0),
        }
        sqlite_before = {
            rel: digest(self.corpus / rel)
            for db in ctx["sqlite"]
            for rel in (db, db + "-wal")
            if (self.corpus / rel).exists()
        }
        rows_before = ec.s2_rows(self.corpus)
        start_ns = time.time_ns()
        record["racy_seats"] = [
            [rel, int(row[4])]
            for rel, row in rows_before.items()
            if row[1] == "f"
            and max(int(row[5]), int(row[6])) >= start_ns - ec.RACY_SETTLE_NS
        ]
        t0 = time.monotonic_ns()
        record["file"] = self.file_half(label, ctx["areas"])
        t1 = time.monotonic_ns()
        record["sqlite"] = self.sqlite_half(label, ctx["sqlite"])
        t2 = time.monotonic_ns()
        record["git"] = self.git_half(label, ctx["plans"])
        t3 = time.monotonic_ns()
        rows_v1 = ec.s2_rows(self.corpus)
        record["s2_v1"] = s2_summary(rows_before, rows_v1, ctx["sqlite"])
        record["sqlite_identical"] = {
            rel: digest(self.corpus / rel) == got for rel, got in sqlite_before.items()
        }
        record["wall_ns"] = {"file": t1 - t0, "sqlite": t2 - t1, "git": t3 - t2}
        record["wall_ns"]["v1"] = t3 - t0
        record["load1_after_v1"] = load1()
        t4 = time.monotonic_ns()
        record["v2"] = self.v2_half(label, ctx["repos"], model)
        record["wall_ns"]["v2"] = time.monotonic_ns() - t4
        record["s2_v2"] = s2_summary(rows_v1, ec.s2_rows(self.corpus), ctx["sqlite"])
        record["load1_after"] = load1()
        record["mem_after"] = meminfo()
        record["finished"] = now()
        say(
            f"pass {label} done wall_v1_s={record['wall_ns']['v1'] / 1e9:.1f} "
            f"load1={record['load1_after']}"
        )
        return record

    def mutate(self, label: str, count: int) -> dict:
        child = self.runner.run(
            f"{label}/mutate",
            [
                sys.executable,
                str(corpus_script()),
                "mutate",
                str(self.dest),
                str(count),
            ],
        )
        if child["exit"] != 0:
            return {"child": compact(child), "sidecar": None}
        seal = json.loads((self.dest / "SEAL.json").read_text(encoding="utf-8"))
        rnd = len(seal["mutations"])
        path = self.dest / "mutations" / f"round-{rnd:04d}.json"
        sidecar = json.loads(path.read_text(encoding="utf-8"))
        timing = sidecar.get("timing", {})
        ready = timing.get("mutated_at_ns", 0) + timing.get("settle_ns", 0)
        wait = max(0.0, (ready - time.time_ns()) / 1e9) + SETTLE_MARGIN_S
        time.sleep(wait)
        return {
            "child": compact(child),
            "sidecar_path": str(path),
            "sidecar": sidecar,
            "waited_s": round(wait, 2),
        }

    # -- the whole run --

    def run(self, args: argparse.Namespace) -> int:
        refusal = self.preflight(args.min_available_gib)
        if refusal:
            say(f"refused: {refusal}")
            return 2
        private_dir(self.work)
        self.runner = Runner(self.work, git_env(self.work))
        build = (
            json.loads(Path(args.build_json).read_text(encoding="utf-8"))
            if args.build_json
            else {}
        )
        self.record = {
            "format": FORMAT,
            "label": LABEL,
            "rulings": RULINGS,
            "started": now(),
            "host": platform.node(),
            "platform": platform.platform(),
            "cpus": os.cpu_count(),
            "mem": meminfo(),
            "agent": str(self.agent),
            "agent_sha256": sha256(self.agent),
            "build": build,
            "scale": self.scale,
            "seed": self.seed,
            "jobs": self.jobs,
            "harness": str(Path(__file__).resolve()),
            "harness_sha256": sha256(Path(__file__).resolve()),
            "passes": [],
        }
        say(f"generate scale={self.scale} dest={self.dest}")
        gen = self.generate()
        self.record["generate"] = gen
        if gen["child"]["exit"] != 0:
            say("generate failed")
            self.save()
            return 3
        seal = json.loads((self.dest / "SEAL.json").read_text(encoding="utf-8"))
        areas, uncovered = areas_of(self.corpus)
        ctx = {
            "areas": areas,
            "uncovered": uncovered,
            "sqlite": sqlite_databases(self.corpus),
            "plans": self.plan(seal),
            "repos": estimate_repos(seal),
            "items": ec.estate_items(seal),
        }
        self.record["context"] = dict(ctx) | {
            "identity": seal["identity"],
            "counts": seal["counts"],
        }
        model = self.model_empty(ctx["repos"])
        time.sleep(ec.RACY_SETTLE_NS / 1e9 + SETTLE_MARGIN_S)
        mirrors = {}
        for label, count in PASSES:
            entry: dict = {"mutation": None}
            if count:
                entry["mutation"] = self.mutate(label, count)
                if entry["mutation"]["sidecar"] is None:
                    say(f"mutate {count} failed")
                    self.record["passes"].append({"label": label, **entry})
                    self.save()
                    return 3
            entry |= self.one_pass(label, ctx, model)
            entry["v2_model"] = str(model)
            self.record["passes"].append(entry)
            self.save()
            if label == "first":
                mirrors["m0"] = self.model_mirror(ctx["repos"], "m0")
                model = Path(mirrors["m0"]["root"])
            elif label == "mutate-1":
                mirrors["m1"] = self.model_mirror(ctx["repos"], "m1")
                model = Path(mirrors["m1"]["root"])
        self.record["v2_models"] = mirrors
        child = self.runner.run(
            "final/verify",
            [sys.executable, str(corpus_script()), "verify", str(self.dest)],
        )
        lines = child["stdout"].splitlines()
        self.record["final_verify"] = {
            "child": compact(child),
            "line": pairs(lines[-1]) if lines else {},
        }
        self.record["evaluation"] = evaluate(self.record, ctx)
        self.record["finished"] = now()
        self.save()
        say(f"done json={self.work / 's3-estate.json'}")
        return 0

    def save(self) -> None:
        path = self.work / "s3-estate.json"
        tmp = path.with_suffix(".json.tmp")
        tmp.write_text(json.dumps(self.record, indent=1, default=str) + "\n")
        tmp.replace(path)


# ---- evaluation -----------------------------------------------------------------


def half_totals(entry: dict, skip_data: bool, skip_history: bool) -> dict:
    """Summed counters and costs of one pass for one subset."""
    out: dict = {}
    copy_rows = {
        area: row
        for area, row in entry["file"].items()
        if not (skip_data and area == "data")
    }
    f: dict = {"counters": {}}
    for row in copy_rows.values():
        add_counts(f["counters"], row["counters"])
    f["source_bytes_read"] = sum(
        number(r["transfer"], "source_bytes_read") for r in copy_rows.values()
    )
    f["source_bytes_read_by_area"] = {
        area: number(r["transfer"], "source_bytes_read")
        for area, r in copy_rows.items()
        if number(r["transfer"], "source_bytes_read")
    }
    f["bytes_received"] = sum(
        number(r["transfer"], "bytes_received") for r in copy_rows.values()
    )
    f["completed"] = sum(number(r["transfer"], "completed") for r in copy_rows.values())
    f["reused"] = sum(number(r["transfer"], "reused") for r in copy_rows.values())
    f["cpu_s"] = sum(r["cpu_s"] for r in copy_rows.values())
    f["wall_ns"] = sum(r["wall_ns"] for r in copy_rows.values())
    f["priority"] = priority_of([r["counters"] for r in copy_rows.values()])
    refused = [x for r in copy_rows.values() for x in r["refused"]]
    f["refused_by_code"] = {}
    for _, code in refused:
        f["refused_by_code"][code] = f["refused_by_code"].get(code, 0) + 1
    out["file"] = f
    s = {
        "db_bytes": sum(
            r["db_bytes"] + r["wal_bytes"] for r in entry["sqlite"].values()
        ),
        "output_bytes": sum(r["output_bytes"] for r in entry["sqlite"].values()),
        "cpu_s": sum(r["cpu_s"] for r in entry["sqlite"].values()),
        "wall_ns": sum(r["wall_ns"] for r in entry["sqlite"].values()),
        "failed": [k for k, r in entry["sqlite"].items() if r["exit"] != 0],
        "priority": priority_of([r["counters"] for r in entry["sqlite"].values()]),
    }
    out["sqlite"] = s
    plans_used = {
        name: row
        for name, row in entry["git"].items()
        if not (skip_history and name == "history")
    }
    g: dict = {"counters": {}, "outcomes": {}}
    for row in plans_used.values():
        add_counts(g["counters"], row["counters"])
        for k, v in row["outcomes"].items():
            g["outcomes"][k] = g["outcomes"].get(k, 0) + v
    g["source_bytes_read"] = sum(r["source_bytes_read"] for r in plans_used.values())
    g["cpu_s"] = sum(r["cpu_s"] for r in plans_used.values())
    g["wall_ns"] = sum(r["wall_ns"] for r in plans_used.values())
    g["priority"] = priority_of([r["counters"] for r in plans_used.values()])
    out["git"] = g
    v2_used = {
        name: row
        for name, row in entry["v2"].items()
        if not (skip_history and name == "history")
    }
    out["v2"] = {
        "missing_thin_pack_bytes": sum(
            r["missing_thin_pack_bytes"] for r in v2_used.values()
        ),
        "missing_objects": sum(r["missing_objects"] for r in v2_used.values()),
        "source_history_bytes": sum(
            r["source_history_bytes"] for r in v2_used.values()
        ),
        "cpu_s": sum(r["cpu_s"] for r in v2_used.values()),
        "wall_ns": sum(r["wall_ns"] for r in v2_used.values()),
        "refused": [x for r in v2_used.values() for x in r["refused"]],
        "priority": priority_of([r["counters"] for r in v2_used.values()]),
    }
    v1_wall = f["wall_ns"] + s["wall_ns"] + g["wall_ns"]
    v1_cpu = f["cpu_s"] + s["cpu_s"] + g["cpu_s"]
    out["v1"] = {
        "wall_ns": v1_wall,
        "cpu_s": round(v1_cpu, 6),
        "cpu_utilisation": round(v1_cpu / (v1_wall / 1e9), 4) if v1_wall else None,
    }
    return out


def git_wire(bounds: dict, skip_history: bool) -> dict:
    """Git inequality 2 per repository: the bundle bytes of its items against
    its changed worktree seats (chunk and object granularity) plus its new
    objects, counted once per repository however many of its items changed."""
    keep = [
        name
        for name in bounds["git_read_by_item"]
        if not (skip_history and name == HISTORY)
    ]
    repos: dict[str, dict] = {}
    for name in keep:
        repo = bounds["git_item_repo"].get(name, name)
        row = repos.setdefault(
            repo,
            {
                "items": [],
                "new_object_bytes": bounds["git_new_object_bytes_by_changed_repo"].get(
                    repo, 0
                ),
                "worktree_chunk_bound": 0,
                "worktree_object_bytes": 0,
            },
        )
        row["items"].append(name)
        row["worktree_chunk_bound"] += bounds["git_worktree_chunk_bound_by_item"].get(
            name, 0
        )
        row["worktree_object_bytes"] += bounds["git_worktree_object_bytes_by_item"].get(
            name, 0
        )
    for row in repos.values():
        row["bound_chunk"] = row["worktree_chunk_bound"] + row["new_object_bytes"]
        row["bound_object"] = row["worktree_object_bytes"] + row["new_object_bytes"]
    return repos


def per_item_pack(
    entry: dict,
    bounds: dict,
    repos: dict[str, dict],
    items_by_name: dict,
    skip_history: bool,
) -> tuple[dict, dict]:
    """Bundle bytes per item the pass wrote a bundle for, and per repository
    against that repository's bounds (`git_wire`)."""
    out: dict[str, dict] = {}
    per_repo: dict[str, dict] = {
        repo: {**row, "bundle_bytes": 0} for repo, row in repos.items()
    }
    for name, row in entry["git"].items():
        if skip_history and name == "history":
            continue
        for item, size in row.get("bundles", {}).items():
            repo = items_by_name.get(item, {}).get("repo", item)
            out[item] = {
                "bundle_bytes": size,
                "repo": repo,
                "worktree_chunk_bound": bounds["git_worktree_chunk_bound_by_item"].get(
                    item, 0
                ),
                "worktree_object_bytes": bounds[
                    "git_worktree_object_bytes_by_item"
                ].get(item, 0),
            }
            target = per_repo.setdefault(
                repo,
                {
                    "items": [],
                    "new_object_bytes": 0,
                    "worktree_chunk_bound": 0,
                    "worktree_object_bytes": 0,
                    "bound_chunk": 0,
                    "bound_object": 0,
                    "bundle_bytes": 0,
                },
            )
            target["bundle_bytes"] += size
    for row in per_repo.values():
        row["excess_chunk"] = max(0, row["bundle_bytes"] - row["bound_chunk"])
        row["excess_object"] = max(0, row["bundle_bytes"] - row["bound_object"])
    return out, per_repo


def verdict(ok: bool | None) -> str:
    return "n/a" if ok is None else ("pass" if ok else "fail")


def classify_refusals(entry: dict, changed: set[str], sqlite: set[str]) -> dict:
    """Typed results of copy's refusals: routed SQLite seats, no-clobber on a
    changed seat (`blocked by WP0(d)`), and anything else."""
    out = {"routed-to-snapshot": [], "blocked by WP0(d)": [], "other": []}
    for row in entry["file"].values():
        for path, code in row["refused"]:
            name = code.split()[0] if code else code
            if name in SQLITE_ROUTED and is_sqlite_seat(path, sqlite):
                out["routed-to-snapshot"].append(path)
            elif name in NO_CLOBBER and path in changed:
                out["blocked by WP0(d)"].append(path)
            else:
                out["other"].append(f"{path}: {code}")
    return out


def evaluate(record: dict, ctx: dict) -> dict:
    """S3 verdicts per pass and subset (OI-1003-Q18, OI-1003-Q35)."""
    passes = record["passes"]
    sqlite = set(ctx["sqlite"])
    items_by_name = {i["item"]: i for i in ctx["items"]}
    censused = {name: plan["censused"] for name, plan in ctx["plans"].items()}
    first = next(p for p in passes if p["label"] == "first")
    out: dict = {}
    for entry in passes:
        label = entry["label"]
        sidecar = (entry.get("mutation") or {}).get("sidecar")
        bounds = (
            delta_bounds(sidecar, ctx["areas"], sqlite, items_by_name)
            if sidecar
            else None
        )
        changed_paths = set()
        if sidecar:
            for key in ("added", "modified"):
                changed_paths |= set(sidecar.get("changed", {}).get(key, []))
            changed_paths |= set(sidecar.get("stat_only", []))
        pass_refusals = classify_refusals(entry, changed_paths, sqlite)
        per_subset = {}
        for subset, (skip_data, skip_history) in SUBSETS.items():
            tot = half_totals(entry, skip_data, skip_history)
            base = half_totals(first, skip_data, skip_history)
            row: dict = {"totals": tot}
            plan_names = [n for n in censused if not (skip_history and n == "history")]
            items_in = [i for n in plan_names for i in censused[n]]
            if label == "first":
                expected_walks = len(items_in) * ec.CENSUS_WALKS_CHANGED
            elif not sidecar:
                expected_walks = len(items_in) * ec.CENSUS_WALKS_REUSED
            else:
                moved = {c["item"] for c in sidecar["items"]["changed"]}
                expected_walks = sum(
                    ec.CENSUS_WALKS_CHANGED if i in moved else ec.CENSUS_WALKS_REUSED
                    for i in items_in
                )
            measured_walks = number(tot["git"]["counters"], "census_walks")
            row["census_walks"] = {
                "measured": measured_walks,
                "expected": expected_walks,
                "verdict": verdict(measured_walks == expected_walks),
            }
            ratio = (
                tot["v1"]["wall_ns"] / base["v1"]["wall_ns"]
                if base["v1"]["wall_ns"]
                else None
            )
            row["wall_ratio_vs_first"] = round(ratio, 4) if ratio is not None else None
            cpu_ratio = (
                tot["v1"]["cpu_s"] / base["v1"]["cpu_s"]
                if base["v1"]["cpu_s"]
                else None
            )
            row["cpu_ratio_vs_first"] = (
                round(cpu_ratio, 4) if cpu_ratio is not None else None
            )
            if label == "first":
                row["s3"] = "baseline"
            elif not sidecar:
                f, g, s = tot["file"], tot["git"], tot["sqlite"]
                row["unchanged"] = {
                    "file_source_bytes_read": verdict(f["source_bytes_read"] == 0),
                    "file_bytes_received": verdict(f["bytes_received"] == 0),
                    "git_source_bytes_read": verdict(g["source_bytes_read"] == 0),
                    "git_pack_bytes_written": verdict(
                        number(g["counters"], "write_source_pack_bytes") == 0
                    ),
                    "sqlite_whole_database_reads": verdict(s["db_bytes"] == 0),
                    "cpu_ratio_le_10pct": verdict(
                        cpu_ratio is not None and cpu_ratio <= CPU_RATIO
                    ),
                    "wall_ratio_le_10pct_informational": verdict(
                        ratio is not None and ratio <= WALL_RATIO
                    ),
                }
            else:
                b = bounds
                areas = [a for a in ctx["areas"] if not (skip_data and a == "data")]
                f_read_bound = sum(
                    v for a, v in b["file_read_by_area"].items() if a in areas
                )
                blocked = sorted(
                    (
                        p
                        for p in pass_refusals["blocked by WP0(d)"]
                        if area_of(p) in areas
                    ),
                    key=str.encode,
                )
                f_wire_carried = sum(
                    v
                    for rel, v in b["file_wire_by_seat"].items()
                    if area_of(rel) in areas and rel not in blocked
                )
                f_wire_blocked = sum(
                    b["file_wire_by_seat"].get(rel, 0) for rel in blocked
                )
                refused_dbs = [
                    p
                    for p in pass_refusals["routed-to-snapshot"]
                    if p in sqlite and area_of(p) in areas
                ]
                racy = entry.get("racy_seats", [])
                racy_files = [[r, n] for r, n in racy if area_of(r) in areas]
                racy_git = [
                    [r, n]
                    for r, n in racy
                    if r.startswith("git/")
                    and not (skip_history and ec.inside(r, HISTORY))
                ]
                racy_bytes = sum(n for _, n in racy_files)
                racy_git_bytes = sum(n for _, n in racy_git)
                g_items = {
                    k: v
                    for k, v in b["git_read_by_item"].items()
                    if not (skip_history and k == HISTORY)
                }
                repos = git_wire(b, skip_history)
                per_item, per_repo = per_item_pack(
                    entry, b, repos, items_by_name, skip_history
                )
                bound_chunk = sum(r["bound_chunk"] for r in repos.values())
                bound_object = sum(r["bound_object"] for r in repos.values())
                object_store_seats = sum(
                    v
                    for k, v in b["git_object_store_seat_bytes_by_item"].items()
                    if not (skip_history and k == HISTORY)
                )
                f, g, s = tot["file"], tot["git"], tot["sqlite"]
                pack = number(g["counters"], "write_source_pack_bytes")
                readback = number(g["counters"], "read_source_pack_readback_bytes")
                f_read_measured = f["source_bytes_read"]
                g_read_bound = sum(g_items.values()) + racy_git_bytes
                carried_ok = f["bytes_received"] <= f_wire_carried
                row["delta"] = {
                    "file_ineq1": {
                        "measured": f_read_measured,
                        "bound": f_read_bound + racy_bytes,
                        "excess": max(0, f_read_measured - f_read_bound - racy_bytes),
                        "racy_seats": racy_files,
                        "sqlite_header_bytes": len(ec.SQLITE_MAGIC) * len(refused_dbs),
                        "verdict": verdict(
                            f_read_measured <= f_read_bound + racy_bytes
                        ),
                    },
                    "file_ineq2": {
                        "measured": f["bytes_received"],
                        "bound": f_wire_carried,
                        "bound_with_blocked": f_wire_carried + f_wire_blocked,
                        "blocked_seats": blocked,
                        "carried_verdict": verdict(carried_ok),
                        "verdict": (
                            "n/a: blocked by WP0(d)" if blocked else verdict(carried_ok)
                        ),
                    },
                    "git_ineq1": {
                        "measured": g["source_bytes_read"],
                        "bound": g_read_bound,
                        "scope": "worktree seats",
                        "racy_seats": racy_git,
                        "verdict": verdict(g["source_bytes_read"] <= g_read_bound),
                        "object_store": {
                            "readback_lower_bound": readback,
                            "pack_bytes_logical": pack,
                            "changed_seat_bytes": object_store_seats,
                            "verdict_if_counted": (
                                "fail"
                                if g["source_bytes_read"] + readback
                                > g_read_bound + object_store_seats
                                else "unknown (readback is a lower bound)"
                            ),
                        },
                    },
                    "git_ineq2": {
                        "measured": pack,
                        "granularity": "chunk",
                        "bound": bound_chunk,
                        "excess": max(0, pack - bound_chunk),
                        "verdict": verdict(pack <= bound_chunk),
                        "bound_object": bound_object,
                        "excess_object": max(0, pack - bound_object),
                        "verdict_object": verdict(pack <= bound_object),
                        "per_item": per_item,
                        "per_repo": per_repo,
                    },
                    "sqlite_ineq1": {
                        "measured_derived": s["db_bytes"],
                        "bound": sum(b["sqlite_changed"].values()),
                        "verdict": verdict(
                            s["db_bytes"] <= sum(b["sqlite_changed"].values())
                        ),
                    },
                    "sqlite_ineq2": {
                        "measured_output_bytes": s["output_bytes"],
                        "bound_changed_range_bytes": b["sqlite_changed_range_bytes"],
                        "verdict": verdict(
                            s["output_bytes"] <= b["sqlite_changed_range_bytes"]
                        ),
                    },
                }
            per_subset[subset] = row
        out[label] = {
            "bounds": bounds,
            "refusals": pass_refusals,
            "subsets": per_subset,
            "s2_v1": entry["s2_v1"],
            "s2_v2": entry["s2_v2"],
            "sqlite_identical": entry["sqlite_identical"],
            "load1": [entry["load1_before"], entry["load1_after"]],
        }
    return out


def reevaluate(path: Path, out: Path, seal_path: Path | None) -> int:
    """Re-run `evaluate` on a recorded run into a new JSON at OUT.

    The measured record is never rewritten: OUT must be a new path. The
    estate items come from the record's context, or for a record from before
    they were kept there, from the corpus seal (default WORK/estate/SEAL.json
    beside the JSON).
    """
    if out.exists() or out.resolve() == path.resolve():
        say(f"refused: OUT must be new: {out}")
        return 2
    record = json.loads(path.read_text(encoding="utf-8"))
    ctx = dict(record["context"])
    if "items" not in ctx:
        seal_file = seal_path or path.parent / "estate" / "SEAL.json"
        ctx["items"] = ec.estate_items(
            json.loads(seal_file.read_text(encoding="utf-8"))
        )
    record["evaluation"] = evaluate(record, ctx)
    record["reevaluated"] = {
        "at": now(),
        "from": str(path),
        "from_sha256": sha256(path),
        "harness": str(Path(__file__).resolve()),
        "harness_sha256": sha256(Path(__file__).resolve()),
    }
    out.write_text(json.dumps(record, indent=1, default=str) + "\n")
    say(f"reevaluated json={out}")
    return 0


# ---- report ---------------------------------------------------------------------


def fmt(n: object) -> str:
    if isinstance(n, int):
        return f"{n:,}"
    if isinstance(n, float):
        return f"{n:,.3f}"
    return str(n)


def report(path: Path) -> int:
    record = json.loads(path.read_text(encoding="utf-8"))
    ev = record.get("evaluation", {})
    print(f"# S3 estate run: scale {record['scale']} ({record['label']})\n")
    print(
        f"- agent sha256 `{record['agent_sha256'][:16]}`, build `{record.get('build', {}).get('sha', '?')}`"
    )
    print(f"- identity `{record.get('context', {}).get('identity', '?')}`\n")
    for subset in SUBSETS:
        print(f"## Subset {subset}\n")
        print(
            "| pass | file src read | file recv | git src read | pack written | pack readback "
            "| census (exp) | sqlite whole-db | v2 thin pack | v1 wall s | v1 cpu s | cpu util "
            "| wall ratio | cpu ratio | load1 before/after |"
        )
        print("|---|" + "---:|" * 14)
        for entry in record["passes"]:
            label = entry["label"]
            row = ev.get(label, {}).get("subsets", {}).get(subset)
            if not row:
                continue
            t = row["totals"]
            print(
                f"| {label} | {fmt(t['file']['source_bytes_read'])} | {fmt(t['file']['bytes_received'])} "
                f"| {fmt(t['git']['source_bytes_read'])} "
                f"| {fmt(number(t['git']['counters'], 'write_source_pack_bytes'))} "
                f"| {fmt(number(t['git']['counters'], 'read_source_pack_readback_bytes'))} "
                f"| {row['census_walks']['measured']} ({row['census_walks']['expected']}) "
                f"| {fmt(t['sqlite']['db_bytes'])} | {fmt(t['v2']['missing_thin_pack_bytes'])} "
                f"| {t['v1']['wall_ns'] / 1e9:.2f} | {t['v1']['cpu_s']:.2f} "
                f"| {fmt(t['v1']['cpu_utilisation'])} | {fmt(row['wall_ratio_vs_first'])} "
                f"| {fmt(row['cpu_ratio_vs_first'])} "
                f"| {ev[label]['load1'][0]} / {ev[label]['load1'][1]} |"
            )
        print()
        for entry in record["passes"]:
            label = entry["label"]
            row = ev.get(label, {}).get("subsets", {}).get(subset, {})
            if "unchanged" in row:
                print(
                    f"- {label} unchanged: "
                    + ", ".join(f"{k}={v}" for k, v in row["unchanged"].items())
                )
            if "delta" in row:
                parts = []
                for k, v in row["delta"].items():
                    measured = v.get(
                        "measured",
                        v.get("measured_derived", v.get("measured_output_bytes")),
                    )
                    bound = v.get("bound", v.get("bound_changed_range_bytes"))
                    parts.append(f"{k} {fmt(measured)} <= {fmt(bound)} {v['verdict']}")
                print(f"- {label} delta: " + "; ".join(parts))
                git2 = row["delta"]["git_ineq2"]
                print(
                    f"  - git_ineq2 object granularity: {fmt(git2['measured'])} <= "
                    f"{fmt(git2['bound_object'])} {git2['verdict_object']}"
                )
                for repo, r in sorted(git2.get("per_repo", {}).items()):
                    print(
                        f"  - {repo}: bundles {fmt(r['bundle_bytes'])}, new objects "
                        f"{fmt(r['new_object_bytes'])}, bound chunk "
                        f"{fmt(r['bound_chunk'])} / object {fmt(r['bound_object'])}"
                    )
        print()
    print("## Refusals, S2 and SQLite\n")
    for entry in record["passes"]:
        label = entry["label"]
        e = ev.get(label, {})
        r = e.get("refusals", {})
        s2 = e.get("s2_v1", {})
        print(
            f"- {label}: routed={len(r.get('routed-to-snapshot', []))} "
            f"blocked-by-WP0(d)={len(r.get('blocked by WP0(d)', []))} other={len(r.get('other', []))}; "
            f"s2 shm_created={s2.get('shm_created')} shm_touched={len(s2.get('shm_touched', []))} "
            f"objects_freshened={s2.get('objects_freshened')} other={s2.get('other')}; "
            f"v2 s2 other={e.get('s2_v2', {}).get('other')}; "
            f"sqlite identical={all(e.get('sqlite_identical', {}).values())}"
        )
    return 0


# ---- build ----------------------------------------------------------------------


def build(args: argparse.Namespace) -> int:
    repo = Path(args.repo).resolve()
    out = private_dir(Path(args.out).absolute())
    sha = subprocess.run(
        ["git", "-C", str(repo), "rev-parse", "--verify", f"{args.rev}^{{commit}}"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    short = sha[:12]
    binary = out / "bin" / f"bulkload-agent-{short}"
    record = binary.with_name(binary.name + ".sha256")
    info = {"rev": args.rev, "sha": sha, "binary": str(binary), "jobs": args.jobs}
    if not (
        binary.is_file()
        and record.is_file()
        and record.read_text().strip() == sha256(binary)
    ):
        source = out / f"src-{short}"
        if not source.exists():
            source.mkdir(parents=True)
            archive = out / f"src-{short}.tar"
            subprocess.run(
                [
                    "git",
                    "-C",
                    str(repo),
                    "archive",
                    "--format=tar",
                    "-o",
                    str(archive),
                    sha,
                ],
                check=True,
            )
            with tarfile.open(archive) as tree:
                tree.extractall(source, filter="data")
            archive.unlink()
        env = dict(os.environ)
        env["CARGO_TARGET_DIR"] = str(out / f"target-{short}")
        env["CARGO_BUILD_JOBS"] = str(args.jobs)
        argv = ["flock", args.lock] if args.lock else []
        argv += ["nice", "-n", "19", "nix", "develop", f"path:{source}#default"]
        argv += ["--command", "cargo", "build", "--release", "--locked"]
        argv += ["-p", "bulkload-agent"]
        say(f"build sha={sha} jobs={args.jobs} lock={args.lock}")
        info["started"] = now()
        status = subprocess.run(argv, cwd=source, env=env, check=False).returncode
        info["finished"] = now()
        if status != 0:
            say(f"build failed status={status}")
            return 4
        binary.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(out / f"target-{short}" / "release" / "bulkload-agent", binary)
        record.write_text(sha256(binary) + "\n")
    info["sha256"] = sha256(binary)
    (out / f"build-{short}.json").write_text(json.dumps(info, indent=1) + "\n")
    say(f"binary={binary} sha256={info['sha256']}")
    return 0


# ---- main -----------------------------------------------------------------------


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)
    bld = sub.add_parser("build", help="release-build bulkload-agent at REV")
    bld.add_argument("--out", required=True)
    bld.add_argument("--repo", default=str(Path(__file__).resolve().parents[3]))
    bld.add_argument("--rev", default="origin/main")
    bld.add_argument("--jobs", type=int, default=4)
    bld.add_argument(
        "--lock", help="flock this file around the build (shared lane lock)"
    )
    run = sub.add_parser("run", help="generate, run six passes and evaluate S3")
    run.add_argument("--agent", required=True)
    run.add_argument("--work", required=True)
    run.add_argument("--scale", choices=sorted(ec.SCALES), default="small")
    run.add_argument("--seed", default=ec.DEFAULT_SEED)
    run.add_argument("--jobs", type=int, choices=(1, 2), default=2)
    run.add_argument("--min-available-gib", type=float, default=6.0)
    run.add_argument("--build-json")
    rep = sub.add_parser("report", help="render a run's JSON as Markdown")
    rep.add_argument("json")
    rev = sub.add_parser(
        "evaluate", help="re-run the S3 evaluation of a recorded run into a new JSON"
    )
    rev.add_argument("json")
    rev.add_argument("--out", required=True)
    rev.add_argument("--seal", help="the corpus SEAL.json (default WORK/estate)")
    args = parser.parse_args(argv)
    if args.command == "build":
        return build(args)
    if args.command == "report":
        return report(Path(args.json))
    if args.command == "evaluate":
        return reevaluate(
            Path(args.json), Path(args.out), Path(args.seal) if args.seal else None
        )
    os.umask(0o077)
    return Measure(args).run(args)


if __name__ == "__main__":
    ec.utf8_mode()
    sys.exit(main())
