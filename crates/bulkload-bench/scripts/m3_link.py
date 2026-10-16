#!/usr/bin/env python3
"""M3 real-link proof harness: bulkload against rclone between two hosts.

Rulings: axiom 7 (bulkload must beat rclone on real many-file estates, by
measurement), OI-1003-Q3 (gate (b) is a neo->sting pull against rclone over
sftp), OI-1003-Q97 (a neo->sting run is a field confirmation: it never
decides S1 in either direction), OI-1002-Q30 (B/A/B/A/B), OI-1003-Q105 (the
flake-pinned rclone), OI-1003-Q165 (no clone of a source file), R-N13. The
plan is docs/agent-notes/2026-10-16-m3-real-link-proof-plan.md.

Subcommands:
  census ROOT --label L [--kind state|git] [--out JSON]
      Read-only distribution of a tree: lstat only, no file content. Sizes,
      classes, fan-out, depth and age histograms, and for --kind git the
      per-repository shape (checkouts, linked worktrees, loose objects,
      packs). Paths and names never leave the walk (the output holds counts,
      bytes and the generic names of ROOT's direct children for state roots).
  generate DEST --profile CENSUS.json [--profile ...] --bytes N [--seed S]
      A synthetic corpus with the censused distribution, scaled to N bytes.
      Deterministic from (profiles, bytes, seed): SHAKE-256 bytes, as
      r23_corpus.py and estate_corpus.py. Writes DEST/corpus, DEST/SEAL.json
      (identity = sha256 of the path/size/sha256 manifest) and seals it
      read-only.
  verify DEST
      Recompute the identity of DEST/corpus against DEST/SEAL.json.
  run --work-root NEW --agent BIN (--source-corpus DEST | --source-subset
      LABEL=PATH ...) --source-work NEW --remote-agent BIN [--ssh-config ABS]
      The arm matrix between the source host (over ssh) and this host. Order
      letters: B bulkload `pull`; P `rclone copy` over sftp with gate (a)'s
      flags (plain rclone, the gate comparator, OI-1003-Q150); R the same
      rclone tuned (--transfers/--checkers, optionally the fastest of a
      sweep); S rclone followed by `sync -f` of the destination (equal
      durability: pull's outputs are durable at exit); Y `rsync -a` over the
      same ssh (a reference row, never a transport). Each rep runs three
      phases over the order: initial copy, an unchanged rerun (S3: the native
      rerun must read 0 content bytes and receive 0), and the 1 % delta of
      OI-1003-Q194 C3: an in-place change (XOR over at most 64 KiB) to about
      1 % of the files of the source's private working copy, restored after.
      --git adds the Git arm: `estate-capture` on the
      source, `pull` of the capture, `estate-apply` here, against rclone of
      the same checkouts (order G/R/G), initial and unchanged rerun. Every
      rep records an iperf3 baseline (when both hosts have iperf3), the ssh
      ceiling (gate_b's dd calibration) and both hosts' load before and
      after every arm.
      --dry-run: one-host loopback (gate_b's ssh shim; the sftp subsystem is
      the local sftp-server) on a generated corpus. NOT A SAMPLE.

Nothing here signals a process, writes a source tree other than the
harness's own new working copy, or takes a lock on a source repository.
Evidence: WORK/m3-link.json and a Markdown draft. A field run writes the
draft to docs/evidence/m3-link-<stamp>-<source>-<dest>.md with the JSON
beside it; a dry run keeps both under WORK.
Exit: 0 complete, 2 refused before the sample, 3 aborted during it.
"""

from __future__ import annotations

import argparse
import dataclasses
import datetime as dt
import hashlib
import json
import os
import platform
import shlex
import shutil
import subprocess
import sys
import time
from pathlib import Path

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
import gate_b as gb  # noqa: E402
import r23_ab as ab  # noqa: E402

FORMAT = "bulkload-m3-link-v1"
CORPUS_FORMAT = "bulkload-m3-corpus-v1"
CENSUS_FORMAT = "bulkload-m3-census-v1"
RULINGS = "axiom 7, OI-1003-Q3, OI-1003-Q97, OI-1002-Q30, OI-1003-Q105, R-N13"
NOT_SAMPLE = "DRY RUN - NOT A SAMPLE"
FIELD = (
    "FIELD RUN ON LIVE HOSTS - INFORMATIONAL: OI-1003-Q194 C2 makes cross-host"
    " gate (b) S1's headline, but which hosts carry its gated samples is unruled"
)
DEFAULT_SEED = "bulkload-m3-corpus-v1"
# B/A/B/A/B against plain rclone (the gate comparator, OI-1003-Q150), the
# alternation continued against tuned rclone, then one equal-durability and
# one rsync reference slot.
DEFAULT_ORDER = "BPBPBRBRBSY"
RSS_CAP_BYTES = gb.RSS_CAP_BYTES
DELTA_FILE_PERCENT = 1.0
DELTA_MAX_BYTES_PER_FILE = 64 << 10
GIT_ORDER = "GRG"
ARM_NAMES = {
    "B": "bulkload pull",
    "P": "rclone copy, plain (gate flags)",
    "R": "rclone copy, tuned",
    "S": "rclone copy + sync -f",
    "Y": "rsync -a (reference)",
    "G": "bulkload estate-capture + pull + estate-apply",
}
MIB = 1 << 20
DELTA_SEED = "bulkload-m3-delta-v1"
SETTLE_S = gb.SETTLE_S
MTIME = 1_790_899_200  # 2026-10-02T00:00:00Z, as r23_corpus.py

# --------------------------------------------------------------- helper code

# Runs on either host (python 3.8+, standard library only), appended to
# gate_b's REMOTE_HELPER so its conditions/manifest/xor ops are shared.
M3_HELPER = r'''
SIZE_EDGES = (0, 512, 4096, 16384, 65536, 262144, 1 << 20, 4 << 20, 16 << 20,
              64 << 20, 256 << 20, 1 << 30)
FANOUT_EDGES = (0, 1, 4, 16, 64, 256, 1024, 4096)
DEPTH_CAP = 32
AGE_EDGES = (86400, 7 * 86400, 30 * 86400, 365 * 86400)
SQLITE_EXT = (".sqlite", ".sqlite3", ".db")
CLASS_EXT = {
    "jsonl": (".jsonl", ".ndjson"),
    "json": (".json",),
    "text": (".md", ".txt", ".toml", ".yaml", ".yml", ".csv", ".html"),
    "log": (".log",),
    "compressed": (".zst", ".gz", ".xz", ".zip", ".tar", ".tgz", ".bz2"),
    "media": (".png", ".jpg", ".jpeg", ".gif", ".webp", ".pdf", ".svg"),
    "source": (".py", ".rs", ".ts", ".tsx", ".js", ".mjs", ".go", ".sh", ".c",
               ".h", ".java", ".rb", ".lua", ".nix", ".svelte", ".css", ".jl"),
}


def bucket(value, edges):
    for i, edge in enumerate(edges):
        if value <= edge:
            return i
    return len(edges)


def file_class(name, in_git):
    if in_git is not None:
        return in_git
    low = name.lower()
    if low.endswith(("-wal", "-shm", "-journal")):
        return "sqlite-sidecar"
    if low.endswith(SQLITE_EXT):
        return "sqlite"
    for cls, exts in CLASS_EXT.items():
        if low.endswith(exts):
            return cls
    return "other"


def git_class(parts):
    """The class of a path inside a git directory, from its parts below it."""
    if len(parts) >= 3 and parts[0] == "objects" and parts[1] == "pack":
        return "git-pack" if parts[2].endswith(".pack") else "git-pack-index"
    if len(parts) == 3 and parts[0] == "objects" and len(parts[1]) == 2:
        return "git-loose"
    return "git-admin"


def new_hist(n):
    return {"count": [0] * n, "bytes": [0] * n}


def pct(values, q):
    if not values:
        return None
    values = sorted(values)
    return values[min(len(values) - 1, int(q * (len(values) - 1) + 0.5))]


def background():
    """nice 19 plus the idle IO class (Linux) or IOPOL_THROTTLE (Darwin)."""
    try:
        os.nice(19 - os.nice(0))
    except OSError:
        pass
    try:
        import ctypes
        libc = ctypes.CDLL(None, use_errno=True)
        if sys.platform == "darwin":
            libc.setiopolicy_np(0, 0, 3)  # IOPOL_TYPE_DISK, SCOPE_PROCESS, THROTTLE
        elif sys.platform.startswith("linux"):
            number = {"x86_64": 251, "aarch64": 30}.get(platform.machine())
            if number is not None:
                libc.syscall(number, 1, 0, 3 << 13)  # IOPRIO_WHO_PROCESS, self, IDLE
    except (OSError, AttributeError):
        pass


def op_census(args):
    background()
    root = os.path.abspath(args["root"])
    kind = args.get("kind", "state")
    now = time.time()
    out = {"format": "bulkload-m3-census-v1", "label": args["label"], "kind": kind,
           "files": 0, "bytes": 0, "allocated_bytes": 0, "dirs": 0, "symlinks": 0,
           "others": 0, "hardlinked_files": 0, "vanished": 0, "unreadable_dirs": 0,
           "size_edges": list(SIZE_EDGES), "fanout_edges": list(FANOUT_EDGES),
           "age_edges_s": list(AGE_EDGES),
           "size_hist": new_hist(len(SIZE_EDGES) + 1),
           "fanout_hist": [0] * (len(FANOUT_EDGES) + 1),
           "file_depth_hist": [0] * (DEPTH_CAP + 1),
           "dir_depth_hist": [0] * (DEPTH_CAP + 1),
           "age_hist": new_hist(len(AGE_EDGES) + 1),
           "classes": {}, "top": {}}
    repos = {}
    worktrees = 0
    bare = 0
    # stack rows: (path, depth, top name, repo key or None, git-dir parts or None)
    stack = [(root, 0, None, None, None)]
    while stack:
        path, depth, top, repo, gparts = stack.pop()
        try:
            entries = list(os.scandir(path))
        except FileNotFoundError:
            out["vanished"] += 1
            continue
        except OSError:
            out["unreadable_dirs"] += 1
            continue
        out["fanout_hist"][bucket(len(entries), FANOUT_EDGES)] += 1
        here_repo = repo
        if kind == "git" and gparts is None:
            names = set(e.name for e in entries)
            if ".git" in names:
                dot = os.path.join(path, ".git")
                if os.path.isdir(dot) and not os.path.islink(dot):
                    here_repo = path
                    repos.setdefault(path, {"files": 0, "bytes": 0, "git_files": 0,
                                            "git_bytes": 0, "loose": 0, "loose_bytes": 0,
                                            "packs": 0, "pack_bytes": 0, "checkout_files": 0,
                                            "checkout_bytes": 0, "worktree_admins": 0,
                                            "nested_in": repo is not None})
                else:
                    worktrees += 1
                    here_repo = path
                    repos.setdefault(path, {"files": 0, "bytes": 0, "git_files": 0,
                                            "git_bytes": 0, "loose": 0, "loose_bytes": 0,
                                            "packs": 0, "pack_bytes": 0, "checkout_files": 0,
                                            "checkout_bytes": 0, "worktree_admins": 0,
                                            "linked": True, "nested_in": False})
            elif {"HEAD", "objects", "refs"} <= names and path.endswith(".git"):
                bare += 1
                here_repo = path
                repos.setdefault(path, {"files": 0, "bytes": 0, "git_files": 0,
                                        "git_bytes": 0, "loose": 0, "loose_bytes": 0,
                                        "packs": 0, "pack_bytes": 0, "checkout_files": 0,
                                        "checkout_bytes": 0, "worktree_admins": 0,
                                        "bare": True, "nested_in": False})
                gparts = []
        for entry in entries:
            name = entry.name
            child_top = top if top is not None else name
            try:
                info = entry.stat(follow_symlinks=False)
            except FileNotFoundError:
                out["vanished"] += 1
                continue
            mode = info.st_mode
            if stat.S_ISDIR(mode):
                out["dirs"] += 1
                out["dir_depth_hist"][min(depth + 1, DEPTH_CAP)] += 1
                child_parts = None
                if gparts is not None:
                    child_parts = gparts + [name]
                elif kind == "git" and name == ".git":
                    child_parts = []
                if (child_parts is not None and len(child_parts) == 1
                        and child_parts[0] == "worktrees" and here_repo in repos):
                    try:
                        repos[here_repo]["worktree_admins"] += len(
                            os.listdir(entry.path))
                    except OSError:
                        pass
                stack.append((entry.path, depth + 1, child_top, here_repo, child_parts))
                continue
            if stat.S_ISLNK(mode):
                out["symlinks"] += 1
                continue
            if not stat.S_ISREG(mode):
                out["others"] += 1
                continue
            size = info.st_size
            out["files"] += 1
            out["bytes"] += size
            out["allocated_bytes"] += getattr(info, "st_blocks", 0) * 512
            if info.st_nlink > 1:
                out["hardlinked_files"] += 1
            b = bucket(size, SIZE_EDGES)
            out["size_hist"]["count"][b] += 1
            out["size_hist"]["bytes"][b] += size
            out["file_depth_hist"][min(depth + 1, DEPTH_CAP)] += 1
            a = bucket(max(0.0, now - info.st_mtime), AGE_EDGES)
            out["age_hist"]["count"][a] += 1
            out["age_hist"]["bytes"][a] += size
            in_git = git_class(gparts + [name]) if gparts is not None else None
            cls = file_class(name, in_git)
            row = out["classes"].setdefault(cls, {"files": 0, "bytes": 0,
                                                  "size_hist": new_hist(len(SIZE_EDGES) + 1)})
            row["files"] += 1
            row["bytes"] += size
            row["size_hist"]["count"][b] += 1
            row["size_hist"]["bytes"][b] += size
            if kind == "state":
                t = out["top"].setdefault(child_top, {"files": 0, "bytes": 0})
                t["files"] += 1
                t["bytes"] += size
            if here_repo in repos:
                r = repos[here_repo]
                r["files"] += 1
                r["bytes"] += size
                if gparts is not None:
                    r["git_files"] += 1
                    r["git_bytes"] += size
                    if in_git == "git-loose":
                        r["loose"] += 1
                        r["loose_bytes"] += size
                    elif in_git == "git-pack":
                        r["packs"] += 1
                        r["pack_bytes"] += size
                else:
                    r["checkout_files"] += 1
                    r["checkout_bytes"] += size
    if kind == "git":
        mains = [r for r in repos.values() if not r.get("linked") and not r.get("bare")]
        out["repos"] = {"checkouts": len(mains), "linked_worktrees": worktrees,
                        "bare": bare, "nested": sum(1 for r in mains if r["nested_in"])}
        dist = {}
        for key in ("files", "bytes", "git_files", "git_bytes", "loose", "loose_bytes",
                    "packs", "pack_bytes", "checkout_files", "checkout_bytes",
                    "worktree_admins"):
            values = [r[key] for r in repos.values()]
            dist[key] = {"sum": sum(values), "p10": pct(values, 0.1),
                         "p50": pct(values, 0.5), "p90": pct(values, 0.9),
                         "p99": pct(values, 0.99), "max": max(values) if values else None}
        out["per_repo"] = dist
    if kind != "state":
        out.pop("top")
    return out


def op_subset(args):
    """A private working copy of real source directories (read-only on them).

    Each SOURCE is copied by plain read/write into WORK/corpus/LABEL; nodes
    other than files, directories and symlinks, and files that vanish under
    the copy, are skipped and counted. The originals are only read.
    """
    work = args["work"]
    if os.path.lexists(work) or not os.path.isdir(os.path.dirname(work)):
        raise RuntimeError("source work root must be new under an existing parent")
    os.mkdir(work, 0o700)
    corpus = os.path.join(work, "corpus")
    os.mkdir(corpus, 0o700)
    skipped = {"others": 0, "vanished": 0}
    for label, source in args["sources"]:
        target = os.path.join(corpus, label)
        os.mkdir(target, 0o700)
        for dirpath, dirs, files in os.walk(source):
            here = os.path.join(target, os.path.relpath(dirpath, source))
            keep = []
            for name in sorted(dirs) + sorted(files):
                path, dest = os.path.join(dirpath, name), os.path.join(here, name)
                try:
                    info = os.lstat(path)
                    if stat.S_ISLNK(info.st_mode):
                        os.symlink(os.readlink(path), dest)
                    elif stat.S_ISDIR(info.st_mode):
                        os.mkdir(dest, 0o700)
                        keep.append(name)
                    elif stat.S_ISREG(info.st_mode):
                        inp = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
                        try:
                            out = os.open(dest, os.O_WRONLY | os.O_CREAT | os.O_EXCL
                                          | os.O_CLOEXEC, 0o600)
                            try:
                                while True:
                                    block = os.read(inp, 1 << 20)
                                    if not block:
                                        break
                                    view = memoryview(block)
                                    while view:
                                        view = view[os.write(out, view):]
                            finally:
                                os.close(out)
                        finally:
                            os.close(inp)
                        os.chmod(dest, stat.S_IMODE(info.st_mode) | 0o600)
                        os.utime(dest, ns=(info.st_atime_ns, info.st_mtime_ns))
                    else:
                        skipped["others"] += 1
                except FileNotFoundError:
                    skipped["vanished"] += 1
            dirs[:] = [d for d in dirs if d in keep]
    relinked = 0
    for label, source in args["sources"]:
        relinked += relink_worktrees(os.path.abspath(source),
                                     os.path.join(corpus, label))
    skipped["worktree_links_rewritten"] = relinked
    for dirpath, dirs, files in os.walk(corpus):
        os.chmod(dirpath, 0o755)
    for sub in ("states", "bin"):
        os.mkdir(os.path.join(work, sub), 0o700)
    wrapper = os.path.join(work, "bin", "gate-b-serve")
    with open(wrapper, "w") as handle:
        handle.write(WRAPPER.format(python=args["python"], agent=args["agent"],
                                    flags=list(args.get("flags") or [])))
    os.chmod(wrapper, 0o700)
    shared = tree_shared(corpus)
    if isinstance(shared, int) and shared:
        raise RuntimeError("the working copy shares %d extent(s): a clone (OI-1003-Q165)"
                           % shared)
    return {"corpus": corpus, "wrapper": wrapper, "states": os.path.join(work, "states"),
            "shared_extents": shared, "skipped": skipped}


def relink_worktrees(source, target):
    """Point the copy's linked-worktree links at the copy, not the original.

    A linked worktree's `.git` file and its admin directory's `gitdir` name
    absolute paths; inside the private working copy, those that name a path
    under SOURCE are rewritten to the same path under TARGET. Only the copy
    is written.
    """
    count = 0
    for dirpath, dirs, files in os.walk(target):
        for name in files:
            path = os.path.join(dirpath, name)
            linked = name == ".git"
            admin = name == "gitdir" and os.sep + "worktrees" + os.sep in path
            if not (linked or admin) or os.path.islink(path):
                continue
            with open(path) as handle:
                text = handle.read()
            body = text[len("gitdir:"):].strip() if linked else text.strip()
            if linked and not text.startswith("gitdir:"):
                continue
            if body == source or body.startswith(source + os.sep):
                new = target + body[len(source):]
                with open(path, "w") as handle:
                    handle.write(("gitdir: " + new if linked else new) + "\n")
                count += 1
    return count


def op_repos(args):
    """Git checkouts under ROOT: [rel, kind, main rel or None, HEAD]."""
    root = os.path.abspath(args["root"])
    rows = []
    for dirpath, dirs, files in os.walk(root):
        dirs.sort()
        names = set(dirs) | set(files)
        if ".git" in names:
            rel = os.path.relpath(dirpath, root)
            dot = os.path.join(dirpath, ".git")
            head = subprocess.run(["git", "-C", dirpath, "rev-parse", "HEAD"],
                                  stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                  universal_newlines=True, check=False).stdout.strip()
            if os.path.isdir(dot):
                rows.append([rel, "main", None, head])
            else:
                with open(dot) as handle:
                    admin = handle.read().split("gitdir:", 1)[-1].strip()
                main = os.path.dirname(os.path.dirname(os.path.dirname(admin)))
                rows.append([rel, "linked", os.path.relpath(main, root), head])
            dirs[:] = [d for d in dirs if d != ".git"]
    return {"rows": rows}


def op_run(args):
    """Run one command on this host, timed (wall ms) and with its max RSS."""
    started = time.monotonic()
    proc = subprocess.Popen(args["argv"], stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            universal_newlines=True, env=dict(os.environ, **args.get("env", {})))
    out, err = proc.communicate()
    elapsed = time.monotonic() - started
    import resource
    usage = resource.getrusage(resource.RUSAGE_CHILDREN)
    scale = 1 if sys.platform == "darwin" else 1024
    return {"exit": proc.returncode, "elapsed_ms": round(elapsed * 1000, 3),
            "max_rss_bytes": usage.ru_maxrss * scale, "stdout": out[-200000:],
            "stderr": err[-200000:]}


def op_mkdirs(args):
    for path in args["paths"]:
        os.makedirs(path, 0o700, exist_ok=True)
    return {"made": len(args["paths"])}


OPS.update({"census": op_census, "subset": op_subset, "repos": op_repos,
            "run": op_run, "mkdirs": op_mkdirs})
'''

PROGRAM = gb.REMOTE_HELPER + M3_HELPER
HELPER: dict[str, object] = {"__name__": "m3_helper"}
exec(compile(PROGRAM, "m3_helper", "exec"), HELPER)  # noqa: S102


def local(op: str, args: dict[str, object]) -> dict[str, object]:
    return HELPER["OPS"][op](args)  # type: ignore[index]


@dataclasses.dataclass
class Transport(gb.Transport):
    """gate_b's one ssh transport, with the M3 ops in its helper program."""

    def helper(self, op: str, args: dict[str, object]) -> dict[str, object]:
        program = (
            PROGRAM + f"\nmain({op!r}, json.loads({json.dumps(json.dumps(args))}))\n"
        )
        result = subprocess.run(
            self.argv([self.python, "-I", "-"]),
            input=program,
            capture_output=True,
            text=True,
            check=False,
        )
        return gb.helper_result(op, result.returncode, result.stdout, result.stderr)


def say(message: str) -> None:
    print(f"m3-link {gb.redact(message)}", flush=True)


def background_priority() -> None:
    """nice 19 and, on Linux, the idle IO class (the census and generator)."""
    try:
        os.nice(19 - os.nice(0))
    except OSError:
        pass
    if sys.platform.startswith("linux"):
        import ctypes

        numbers = {"x86_64": 251, "aarch64": 30}
        number = numbers.get(platform.machine())
        if number is not None:
            libc = ctypes.CDLL(None, use_errno=True)
            libc.syscall(number, 1, 0, 3 << 13)  # IOPRIO_WHO_PROCESS, self, IDLE


# ------------------------------------------------------------------- census


# Direct children of a state root that are generic product names; any other
# name is reported as "(other)" so a census never carries a private name.
PUBLIC_TOP = frozenset(
    (
        "projects jobs plans skills todos sessions archived_sessions sqlite"
        " db-backups packages plugins cache .tmp tmp log logs memories"
        " checkpoints shell_snapshots shell-snapshots file-history statsig ide"
        " history.jsonl session_index.jsonl vendor_imports computer-use"
        " plan-reviews agents commands hooks rules"
    ).split()
)


def public_top(top: dict[str, dict[str, int]]) -> dict[str, dict[str, int]]:
    out: dict[str, dict[str, int]] = {}
    for name, row in top.items():
        key = name if name in PUBLIC_TOP else "(other)"
        acc = out.setdefault(key, {"files": 0, "bytes": 0})
        acc["files"] += row["files"]
        acc["bytes"] += row["bytes"]
    return out


def cmd_census(args: argparse.Namespace) -> int:
    started = time.monotonic()
    request = {"root": args.root, "label": args.label, "kind": args.kind}
    if args.ssh_host:
        # Read-only on the remote host too: lstat only, at its nice/idle class.
        transport = Transport(
            args.ssh, args.ssh_host, args.ssh_config, args.source_python
        )
        out = transport.helper("census", request)
    else:
        background_priority()
        out = local("census", request)
    if "top" in out:
        out["top"] = public_top(out["top"])
    out["walk_seconds"] = round(time.monotonic() - started, 1)
    out["date"] = dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%dT%H:%MZ")
    text = json.dumps(out, indent=1, sort_keys=True)
    if args.out:
        Path(args.out).write_text(text + "\n")
    print(text if not args.out else census_summary(out))
    return 0


def census_summary(c: dict) -> str:
    gib = c["bytes"] / (1 << 30)
    small = sum(c["size_hist"]["count"][:3])
    line = (
        f"{c['label']}: {c['files']:,} files, {gib:.1f} GiB, {c['dirs']:,} dirs,"
        f" {c['symlinks']:,} symlinks; <=4 KiB: {small / max(1, c['files']):.0%};"
        f" mean {c['bytes'] / max(1, c['files']) / 1024:.1f} KiB"
    )
    if c.get("repos"):
        line += f"; repos {c['repos']}"
    return line


# ---------------------------------------------------------------- generator

TEXT_CLASSES = ("jsonl", "json", "text", "log", "source", "git-admin")
RANDOM_CLASSES = ("compressed", "media", "git-loose", "git-pack", "git-pack-index")
CLASS_SUFFIX = {
    "jsonl": ".jsonl",
    "json": ".json",
    "text": ".md",
    "log": ".log",
    "compressed": ".zst",
    "media": ".png",
    "source": ".rs",
    "sqlite": ".sqlite",
    "sqlite-sidecar": ".sqlite-wal",
    "other": "",
    "git-admin": "",
    "git-loose": "",
    "git-pack": ".pack",
    "git-pack-index": ".idx",
}
SQLITE_MAGIC = b"SQLite format 3\x00"


def scaled_counts(census: dict, factor: float) -> list[tuple[str, int, int]]:
    """(class, files, mean size) per class and size bucket, scaled by factor.

    Each bucket keeps floor(count * factor) files at its census mean size.
    The class's remaining round(files * factor) - sum(floor) files go first to
    the buckets with the largest byte remainder whose mean still fits the
    class's byte deficit (half a mean of overshoot allowed), then to the
    buckets with the largest count remainder. So counts and bytes both stay
    close to census * factor and a rare large-file bucket is kept only when
    the scale has room for it; the SEAL records census and achieved numbers.
    """
    rows: list[tuple[str, int, int]] = []
    for cls in sorted(census["classes"]):
        hist = census["classes"][cls]["size_hist"]
        want = max(1, round(census["classes"][cls]["files"] * factor))
        deficit = census["classes"][cls]["bytes"] * factor
        means = [b // c if c else 0 for b, c in zip(hist["bytes"], hist["count"])]
        raw = [c * factor for c in hist["count"]]
        floor = [int(x) for x in raw]
        deficit -= sum(f * m for f, m in zip(floor, means))
        left = want - sum(floor)
        by_bytes = sorted(
            range(len(raw)),
            key=lambda i: ((raw[i] - floor[i]) * means[i], -i),
            reverse=True,
        )
        for i in by_bytes:
            if left <= 0:
                break
            if hist["count"][i] and raw[i] > floor[i] and means[i] <= deficit * 1.5:
                floor[i] += 1
                deficit -= means[i]
                left -= 1
        by_count = sorted(
            range(len(raw)), key=lambda i: (raw[i] - floor[i], -means[i]), reverse=True
        )
        for i in by_count:
            if left <= 0:
                break
            if hist["count"][i] and means[i] <= max(deficit, 0) + 65536:
                floor[i] += 1
                deficit -= means[i]
                left -= 1
        for i, count in enumerate(floor):
            if count and hist["count"][i]:
                rows.append((cls, count, means[i]))
    return rows


def content(label: str, cls: str, size: int):
    if cls == "sqlite":
        head = SQLITE_MAGIC[: min(size, 16)]
        yield head
        if size > len(head):
            yield from ab_blob(label, "random", size - len(head))
    elif cls in TEXT_CLASSES:
        yield from ab_blob(label, "text", size)
    else:
        yield from ab_blob(label, "random", size)


def ab_blob(label: str, kind: str, size: int):
    import estate_corpus as ec

    return ec.blob(label, kind, size)


def area_dirs(
    det, area: str, census: dict, factor: float, files: int
) -> list[tuple[str, int]]:
    """Directories (rel, depth) for one area, by the census dir-depth shape."""
    dirs = max(1, round(census["dirs"] * factor))
    dirs = min(dirs, max(1, files))
    hist = census["dir_depth_hist"]
    total = sum(hist) or 1
    out: list[tuple[str, int]] = [(area, 0)]
    by_depth: dict[int, list[str]] = {0: [area]}
    for depth, count in enumerate(hist):
        want = round(count / total * dirs)
        for _ in range(want):
            parent_depth = max(d for d in by_depth if d < max(depth, 1))
            parents = by_depth[parent_depth]
            parent = parents[det.below(len(parents))]
            rel = f"{parent}/d{len(out):05d}"
            out.append((rel, parent_depth + 1))
            by_depth.setdefault(parent_depth + 1, []).append(rel)
    return out


def cmd_generate(args: argparse.Namespace) -> int:
    import estate_corpus as ec

    background_priority()
    dest = Path(args.dest).absolute()
    if dest.exists() or not dest.parent.is_dir():
        print(f"m3-link: DEST must be new under an existing parent: {dest}")
        return 2
    profiles = [json.loads(Path(p).read_text()) for p in args.profile]
    total = sum(p["bytes"] for p in profiles)
    factor = args.bytes / total
    corpus = dest / "corpus"
    corpus.mkdir(parents=True)
    det = ec.Det(f"{args.seed}/{args.bytes}")
    areas = []
    for profile in profiles:
        area = profile["label"]
        rows = scaled_counts(profile, factor)
        files = sum(count for _, count, _ in rows)
        dirs = area_dirs(det, area, profile, factor, files)
        weights = [1 + det.below(64) ** 2 for _ in dirs]  # heavy-tailed fan-out
        cum = []
        acc = 0
        for w in weights:
            acc += w
            cum.append(acc)
        for rel, _depth in dirs:
            (corpus / rel).mkdir(parents=True, exist_ok=True)
        made = written = 0
        placed: list[str] = []
        for cls, count, mean in rows:
            for n in range(count):
                pick = det.below(acc)
                lo, hi = 0, len(cum) - 1
                while lo < hi:
                    mid = (lo + hi) // 2
                    if cum[mid] > pick:
                        hi = mid
                    else:
                        lo = mid + 1
                parent = dirs[lo][0]
                size = max(0, mean + (det.below(max(1, mean // 2)) - mean // 4))
                name = f"{cls}-{made:07d}{CLASS_SUFFIX.get(cls, '')}"
                written += ec.put(
                    corpus / parent / name,
                    content(f"{args.seed}/{area}/{made}", cls, size),
                )
                made += 1
                placed.append(f"{parent}/{name}")
        # Symlinks at the census rate, each relative to a generated file of
        # the same area (the shape of in-tree links such as node_modules/.bin).
        links = round(profile.get("symlinks", 0) * factor) if placed else 0
        for n in range(links):
            target = placed[det.below(len(placed))]
            parent = dirs[det.below(len(dirs))][0]
            os.symlink(
                os.path.relpath(target, parent), corpus / parent / f"link-{n:07d}"
            )
        areas.append(
            {
                "label": area,
                "census_files": profile["files"],
                "census_bytes": profile["bytes"],
                "census_symlinks": profile.get("symlinks", 0),
                "files": made,
                "bytes": written,
                "symlinks": links,
                "dirs": len(dirs),
            }
        )
    for path in sorted(corpus.rglob("*"), key=lambda p: len(p.parts), reverse=True):
        os.utime(path, (MTIME, MTIME), follow_symlinks=False)
    rows, identity = corpus_identity(corpus)
    seal = {
        "format": CORPUS_FORMAT,
        "seed": args.seed,
        "scale": f"bytes={args.bytes}",
        "factor": factor,
        "identity": identity,
        "files": sum(1 for r in rows if r[1] == "f"),
        "bytes": sum(int(r[2]) for r in rows if r[1] == "f"),
        "areas": areas,
        "profiles": [
            {"label": p["label"], "kind": p["kind"], "date": p.get("date")}
            for p in profiles
        ],
        "readonly": not args.no_seal,
        "mutations": [],
    }
    (dest / "SEAL.json").write_text(json.dumps(seal, indent=1, sort_keys=True) + "\n")
    if not args.no_seal:
        for path in corpus.rglob("*"):
            if path.is_dir() and not path.is_symlink():
                path.chmod(0o555)
            elif path.is_file() and not path.is_symlink():
                path.chmod(0o444)
        corpus.chmod(0o555)
    print(json.dumps({k: seal[k] for k in ("identity", "files", "bytes", "areas")}))
    return 0


def corpus_identity(corpus: Path) -> tuple[list[list[object]], str]:
    rows: list[list[object]] = []
    for path in sorted(corpus.rglob("*"), key=lambda p: os.fsencode(p)):
        rel = str(path.relative_to(corpus))
        if path.is_symlink():
            rows.append([rel, "l", 0, os.readlink(path)])
        elif path.is_dir():
            rows.append([rel, "d", 0, ""])
        else:
            rows.append([rel, "f", path.stat().st_size, ab.sha256(path)])
    digest = hashlib.sha256()
    for row in rows:
        digest.update(json.dumps(row).encode() + b"\n")
    return rows, digest.hexdigest()


def cmd_verify(args: argparse.Namespace) -> int:
    dest = Path(args.dest)
    seal = json.loads((dest / "SEAL.json").read_text())
    _rows, identity = corpus_identity(dest / "corpus")
    ok = identity == seal["identity"]
    print(f"m3-link verify identity={identity} sealed={seal['identity']} ok={ok}")
    return 0 if ok else 1


def cmd_fixture(args: argparse.Namespace) -> int:
    """A tiny Git estate for the dry run's Git arm: two checkouts with
    history, a dirty index, an untracked file and one linked worktree."""
    root = Path(args.dest).absolute() / "git"
    if root.parent.exists():
        print(f"m3-link: DEST must be new: {root.parent}")
        return 2
    root.mkdir(parents=True)
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")} | {
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_AUTHOR_NAME": "m3",
        "GIT_AUTHOR_EMAIL": "m3@example.invalid",
        "GIT_COMMITTER_NAME": "m3",
        "GIT_COMMITTER_EMAIL": "m3@example.invalid",
    }

    def git(cwd: Path, *argv: str) -> None:
        subprocess.run(
            ["git", *argv], cwd=cwd, env=env, check=True, capture_output=True
        )

    import estate_corpus as ec

    for name in ("r1", "r2"):
        repo = root / name
        repo.mkdir()
        git(repo, "init", "-q", "-b", "main")
        for i in range(3):
            ec.put(repo / f"f{i}.txt", ec.blob(f"m3-fixture/{name}/{i}", "text", 20000))
            git(repo, "add", ".")
            git(repo, "commit", "-qm", f"c{i}")
        ec.put(repo / "f0.txt", b"staged\n")
        git(repo, "add", "f0.txt")
        ec.put(repo / "untracked.txt", b"untracked\n")
    git(
        root / "r1",
        "worktree",
        "add",
        "-q",
        str(root / "r1.worktrees" / "wt"),
        "-b",
        "side",
    )
    ec.put(root / "r1.worktrees" / "wt" / "extra.txt", b"worktree-only\n")
    print(f"m3-link fixture {root}")
    return 0


# ---------------------------------------------------------------------- run


class Abort(gb.Abort):
    pass


@dataclasses.dataclass
class Ctx:
    args: argparse.Namespace
    mode: str
    work: Path
    transport: Transport
    agent: str
    corpus: str  # the source working copy's corpus root
    wrapper: str
    states: str
    rows: list[list[object]]
    exclusions: dict[str, str]
    digest: str
    plan: list[list[object]]
    rclone: str
    rclone_conf: Path
    rclone_excludes: Path
    rsync_excludes: Path
    rclone_tuning: list[str]
    env: dict[str, str]
    native_env: dict[str, str]
    native_flags: list[str]
    git_items: list[list[object]]


def conditions_pair(ctx: Ctx) -> dict[str, object]:
    return {
        "source": ctx.transport.helper("conditions", {}),
        "destination": local("conditions", {}),
    }


def rclone_tuned_argv(
    ctx: Ctx, source: str, dest: Path, tuning: list[str]
) -> list[str]:
    return [
        ctx.rclone,
        "copy",
        f"{gb.REMOTE}:{source}",
        str(dest),
        "--config",
        str(ctx.rclone_conf),
        "--exclude-from",
        str(ctx.rclone_excludes),
        "--create-empty-src-dirs",
        "--links",
        "--metadata",
        *tuning,
        "--stats",
        "0",
        "--log-level",
        "ERROR",
    ]


# rclone as gate (a) and gate_b run it: --transfers 4 --checkers 4.
PLAIN_TUNING = ("--transfers", "4", "--checkers", "4")


def delta_plan_files(
    rows: list[list[object]],
    exclusions: dict[str, str],
    percent: float = DELTA_FILE_PERCENT,
    cap: int = DELTA_MAX_BYTES_PER_FILE,
    seed: str = DELTA_SEED,
) -> list[list[object]]:
    """OI-1003-Q194 C3: a real in-place change to about 1 % of files.

    ceil(percent of the comparable non-empty regular files), chosen in a
    seeded path-hash order; each is changed in place by XOR over its first
    min(size, cap) bytes (gate_b's `xor` op), so its size is kept, its bytes
    and mtime change, and nothing is removed at any destination.
    """
    files = [r for r in rows if r[1] == "f" and r[0] not in exclusions and r[3] > 0]
    files.sort(
        key=lambda r: hashlib.sha256(seed.encode() + b"\0" + os.fsencode(r[0])).digest()
    )
    count = -(-len(files) * int(percent * 100) // 10000) if files else 0
    return [[r[0], min(int(r[3]), cap)] for r in files[:count]]


def rsync_pattern(path: str) -> str:
    """An anchored rsync exclude that matches exactly this path."""
    if any(c in path for c in "*?["):
        path = "".join("\\" + c if c in "\\*?[]" else c for c in path)
    return "/" + path


def arm_argv(
    ctx: Ctx,
    letter: str,
    dest: Path,
    state: str,
    dest_state: Path,
    source: str | None = None,
) -> tuple[list[str], dict[str, str]]:
    source = source or ctx.corpus
    t = ctx.transport
    if letter == "B":
        argv = gb.native_argv(
            ctx.agent, t.host, source, dest, state, dest_state, ctx.wrapper, t.config
        )
        return [argv[0], *ctx.native_flags, *argv[1:]], ctx.native_env
    if letter in ("P", "R", "S"):
        tuning = list(PLAIN_TUNING) if letter == "P" else ctx.rclone_tuning
        argv = rclone_tuned_argv(ctx, source, dest, tuning)
        if letter == "S":
            # Equal durability: pull seals every output before it exits.
            argv = [
                "/bin/sh",
                "-c",
                'd=$1; shift; "$@" && exec sync -f -- "$d"',
                "m3-sync",
                str(dest),
                *argv,
            ]
        return argv, ctx.env
    if letter == "Y":
        ssh = (
            [t.ssh] + (["-F", t.config] if t.config else []) + ["-T", "-oBatchMode=yes"]
        )
        argv = [
            ctx.args.rsync,
            "-a",
            "--exclude-from",
            str(ctx.rsync_excludes),
            "-e",
            shlex.join(ssh),
        ]
        if ctx.args.remote_rsync:
            argv += ["--rsync-path", ctx.args.remote_rsync]
        return [*argv, f"{t.host}:{source}/", f"{dest}/"], ctx.env
    raise ValueError(letter)


def run_slot(
    ctx: Ctx,
    rep_dir: Path,
    rep: int,
    seq: int,
    letter: str,
    phase: str,
    expected: list[list[object]],
    workload: int,
    link: dict[str, object],
) -> dict[str, object]:
    slot = rep_dir / f"{seq}-{letter}"
    dest = slot / "destination"
    if phase == "initial":
        slot.mkdir()
        dest.mkdir()
    before = conditions_pair(ctx)
    argv, env = arm_argv(
        ctx, letter, dest, f"{ctx.states}/rep{rep}-{seq}", slot / "destination-state"
    )
    stem = f"rep{rep}-{seq}-{letter}-{phase}"
    logs = ctx.work / "logs"
    say(
        f"rep={rep} seq={seq} arm={letter} phase={phase}"
        f" source_load1={before['source']['load1']}"
        f" dest_load1={before['destination']['load1']}"
    )
    run = gb.run_measured(argv, env, logs / f"{stem}.stdout", logs / f"{stem}.stderr")
    after = conditions_pair(ctx)
    sample: dict[str, object] = {
        "rep": rep,
        "sequence": seq,
        "arm": letter,
        "phase": phase,
        "argv": gb.redact_argv(argv),
        "exit": run["exit"],
        "elapsed_ms": run["elapsed_ms"],
        "workload_bytes": workload,
        "max_rss_bytes": run["max_rss_bytes"],
        "conditions": {"before": before, "after": after},
        # R-N81 before the arm (AC power, load1 < 2.5 on both hosts); a
        # sample that misses it is recorded as under load (SPEC R3).
        "gated": gb.sample_gated(before["source"], before["destination"]),
        "link_fraction": gb.link_fraction(workload, run["elapsed_ms"], link),
    }
    problems: list[str] = []
    if letter == "B":
        parsed = gb.parse_native(run["stdout"], run["stderr"])
        problems += gb.native_problems(run, parsed, ctx.exclusions)
        transfer = parsed["transfer"]
        sample.update(
            {
                "bytes_received": transfer.get("bytes_received"),
                "source_bytes_read": transfer.get("source_bytes_read"),
                "completed": transfer.get("completed"),
                "reused": transfer.get("reused"),
                "serve_max_rss_bytes": parsed["serve_max_rss_bytes"],
                "source_priority": parsed["serve"].get("priority"),
                "timing": parsed["timing"],
            }
        )
    elif run["exit"] != 0:
        problems.append(
            f"{letter} exit {run['exit']}: {gb.redact(run['stderr'].strip())[-300:]}"
        )
    got, _ = HELPER["manifest"](str(dest))  # type: ignore[operator]
    sample["verified"] = gb.verify_tree(expected, ctx.exclusions, got)
    if not sample["verified"]["ok"]:
        problems.append(f"destination does not verify: {sample['verified']}")
    if problems:
        sample["problems"] = problems
        raise Abort(f"{stem}: {'; '.join(problems)}", sample=sample)
    return sample


def link_baseline(ctx: Ctx) -> dict[str, object]:
    a = ctx.args
    out: dict[str, object] = {
        "ssh_single": ctx.transport.calibrate(a.calibrate_mib, 1),
        "ssh_parallel": ctx.transport.calibrate(a.calibrate_mib, a.calibrate_streams),
    }
    out["iperf3"] = iperf3_baseline(ctx)
    return out


def iperf3_baseline(ctx: Ctx) -> dict[str, object]:
    a = ctx.args
    if not a.iperf3 or not a.remote_iperf3:
        return {"skipped": "no iperf3 on one of the hosts (--iperf3/--remote-iperf3)"}
    host = "127.0.0.1" if ctx.mode == "dry-run" else iperf3_host(ctx)
    if host is None:
        return {"skipped": "no HostName from ssh -G"}
    out: dict[str, object] = {}
    for name, flags in (("source_to_dest", ["-R"]), ("dest_to_source", [])):
        server = subprocess.Popen(
            ctx.transport.argv(
                [
                    *shlex.split(a.remote_iperf3),
                    "-s",
                    "-1",
                    "-p",
                    str(a.iperf3_port),
                    "--idle-timeout",
                    "30",
                ]
            ),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        time.sleep(2)
        client = subprocess.run(
            [
                a.iperf3,
                "-c",
                host,
                "-p",
                str(a.iperf3_port),
                "-t",
                str(a.iperf3_seconds),
                "-J",
                *flags,
            ],
            capture_output=True,
            text=True,
            check=False,
        )
        try:
            server.wait(timeout=60)
        except subprocess.TimeoutExpired:
            out[f"{name}_server"] = "still running; left to its --idle-timeout"
        try:
            end = json.loads(client.stdout)["end"]
            out[name] = {
                "bits_per_second": round(end["sum_received"]["bits_per_second"]),
                "bytes_per_s": round(end["sum_received"]["bits_per_second"] / 8),
                "retransmits": end.get("sum_sent", {}).get("retransmits"),
            }
        except (ValueError, KeyError, TypeError):
            out[name] = {
                "error": gb.redact(client.stderr.strip())[-200:]
                or f"exit {client.returncode}"
            }
    return out


def iperf3_host(ctx: Ctx) -> str | None:
    """The HostName ssh resolves for the source; used, never recorded."""
    resolved = gb.ssh_g(ctx.transport)
    return (resolved.get("hostname") or [None])[0]


def run_rep(ctx: Ctx, rep: int, report: dict[str, object]) -> dict[str, object]:
    rep_dir = ctx.work / "reps" / f"rep{rep}"
    rep_dir.mkdir(parents=True)
    out: dict[str, object] = {"index": rep, "samples": []}
    samples: list[dict[str, object]] = out["samples"]  # type: ignore[assignment]
    out["link"] = link = link_baseline(ctx)
    calib = {k: v for k, v in link.items() if k.startswith("ssh_")}
    order = list(ctx.args.order)
    total = gb.comparable_bytes(ctx.rows, ctx.exclusions)
    try:
        for phase in ("initial", "rerun"):
            for seq, letter in enumerate(order):
                samples.append(
                    run_slot(
                        ctx,
                        rep_dir,
                        rep,
                        seq,
                        letter,
                        phase,
                        ctx.rows,
                        total if phase == "initial" else 0,
                        calib,
                    )
                )
        plan_bytes = sum(int(p[1]) for p in ctx.plan)
        ctx.transport.helper("xor", {"root": ctx.corpus, "plan": ctx.plan})
        out["delta_applied"] = True
        after = ctx.transport.helper("manifest", {"root": ctx.corpus})
        rows_after = after["rows"]
        changed = gb.changed_paths(ctx.rows, rows_after)
        if changed != {str(p[0]) for p in ctx.plan}:
            raise Abort(
                f"rep{rep} delta changed {len(changed)} paths, planned {len(ctx.plan)}"
            )
        sizes = {str(r[0]): int(r[3]) for r in rows_after if r[1] == "f"}
        out["delta"] = {
            "files": len(ctx.plan),
            "bytes": plan_bytes,
            # S3 inequality 1 (OI-1003-Q18): reads <= sizes of changed seats.
            "changed_seat_bytes": sum(sizes.get(p, 0) for p in changed),
        }
        time.sleep(SETTLE_S)
        for seq, letter in enumerate(order):
            if ctx.args.delta_mode == "remove":
                gb.remove_targets(rep_dir / f"{seq}-{letter}" / "destination", ctx.plan)
            samples.append(
                run_slot(
                    ctx,
                    rep_dir,
                    rep,
                    seq,
                    letter,
                    "delta",
                    rows_after,
                    plan_bytes,
                    calib,
                )
            )
    except gb.Abort as abort:
        if abort.sample is not None:
            samples.append(abort.sample)
        out["aborted"] = str(abort)
        restore(ctx, out)
        raise Abort(str(abort), out) from abort
    restore(ctx, out)
    time.sleep(SETTLE_S)
    if ctx.git_items:
        try:
            out["git"] = run_git(ctx, rep_dir, rep, calib)
        except gb.Abort as abort:
            out["git"] = (abort.rep or {}).get("git")
            out["aborted"] = str(abort)
            raise Abort(str(abort), out) from abort
    out["verdict"] = rep_verdict(out)
    if not ctx.args.keep_destinations:
        shutil.rmtree(rep_dir, ignore_errors=True)
        out["destinations_released"] = True
    return out


def restore(ctx: Ctx, rep: dict[str, object]) -> None:
    if not rep.get("delta_applied"):
        rep["restored"] = True
        return
    ctx.transport.helper("xor", {"root": ctx.corpus, "plan": ctx.plan})
    final = ctx.transport.helper("manifest", {"root": ctx.corpus})
    rep["restored"] = final["digest"] == ctx.digest
    if not rep["restored"]:
        raise Abort("the source working copy did not restore after the delta", rep)


# ------------------------------------------------------------------ git arm


def git_items(ctx: Ctx, rows: list[list[object]], dest_root: Path) -> list[str]:
    """estate-add-batch triples: one item per checkout (R-N114)."""
    words: list[str] = []
    for rel, kind, main, _head in rows:
        src = f"{ctx.corpus}/{ctx.args.git_subtree}/{rel}"
        dest = str(dest_root / str(rel))
        if kind == "main":
            words += [src, dest, dest]
        else:
            words += [src, str(dest_root / str(main)), dest]
    return words


def run_git(ctx: Ctx, rep_dir: Path, rep: int, link: dict) -> dict[str, object]:
    """G/R/G: capture on the source, pull the capture, apply here; rclone of
    the same checkouts. Initial and an unchanged rerun."""
    out: dict[str, object] = {"samples": []}
    samples: list[dict[str, object]] = out["samples"]  # type: ignore[assignment]
    subtree = f"{ctx.corpus}/{ctx.args.git_subtree}"
    expected = ctx.transport.helper("manifest", {"root": subtree})
    gbytes = gb.comparable_bytes(expected["rows"], expected["exclusions"])
    swork = f"{ctx.states}/git-rep{rep}"
    for phase in ("initial", "rerun"):
        for seq, letter in enumerate(GIT_ORDER):
            slot = rep_dir / f"git-{seq}-{letter}"
            dest = slot / "destination"
            if phase == "initial":
                slot.mkdir()
                dest.mkdir()
            before = conditions_pair(ctx)
            stem = f"rep{rep}-git-{seq}-{letter}-{phase}"
            if letter == "R":
                argv, env = arm_argv(ctx, "R", dest, "", slot / "s", source=subtree)
                run = gb.run_measured(
                    argv,
                    env,
                    ctx.work / "logs" / f"{stem}.stdout",
                    ctx.work / "logs" / f"{stem}.stderr",
                )
                sample = {
                    "arm": "R",
                    "exit": run["exit"],
                    "elapsed_ms": run["elapsed_ms"],
                }
                got, _ = HELPER["manifest"](str(dest))  # type: ignore[operator]
                sample["verified"] = gb.verify_tree(
                    expected["rows"], expected["exclusions"], got
                )
            else:
                sample = git_slot(ctx, slot, dest, f"{swork}-{seq}", stem, phase)
            sample.update(
                {
                    "rep": rep,
                    "sequence": seq,
                    "phase": phase,
                    "workload_bytes": gbytes if phase == "initial" else 0,
                    "conditions": {"before": before, "after": conditions_pair(ctx)},
                }
            )
            sample["link_fraction"] = gb.link_fraction(
                int(sample["workload_bytes"]), float(sample["elapsed_ms"]), link
            )
            samples.append(sample)
            if sample.get("exit") != 0 or not sample.get("verified", {}).get("ok"):
                raise Abort(
                    f"{stem}: git slot failed: "
                    f"{json.dumps(sample.get('problems') or sample)[:600]}",
                    {"git": out},
                )
    return out


def git_slot(
    ctx: Ctx, slot: Path, dest: Path, swork: str, stem: str, phase: str
) -> dict[str, object]:
    agent = ctx.args.remote_agent
    jobs = str(ctx.args.git_jobs)
    plan, corpus, private = (
        f"{swork}/carry/plan.postcard",
        f"{swork}/carry/corpus",
        f"{swork}/private",
    )
    if phase == "initial":
        # Outside the timing, as the live move premakes them: estate-apply
        # refuses GIT_DESTINATION_PARENT_MISSING for a checkout whose parent
        # directory is absent.
        for rel, _kind, _main, _head in ctx.git_items:
            (dest / str(rel)).parent.mkdir(parents=True, exist_ok=True)
        ctx.transport.helper("mkdirs", {"paths": [f"{swork}/carry/corpus", private]})
        add = ctx.transport.helper(
            "run",
            {
                "argv": [
                    agent,
                    "estate-add-batch",
                    plan,
                    *git_items(ctx, ctx.git_items, dest),
                ]
            },
        )
        if add["exit"] != 0:
            raise Abort(
                f"{stem}: estate-add-batch exit {add['exit']}:"
                f" {gb.redact(add['stderr'])[-300:]}"
            )
    flags = [f"--min-free-percent={ctx.args.source_min_free_percent}"]
    if ctx.args.source_priority == "normal":
        flags.append("--priority=normal")
    t0 = time.monotonic()
    cap = ctx.transport.helper(
        "run", {"argv": [agent, *flags, "estate-capture", plan, private, corpus, jobs]}
    )
    capture_ms = round((time.monotonic() - t0) * 1000, 3)
    # Outside the timing: the capture's files are racy for 2 s (R-N76), and a
    # pull inside that window re-reads them by design.
    time.sleep(SETTLE_S)
    carry = slot / "carry"
    if phase == "initial":
        carry.mkdir()
        (slot / "private").mkdir(mode=0o700)
    argv = [
        ctx.agent,
        *ctx.native_flags,
        "pull",
        ctx.transport.host,
        f"{swork}/carry",
        str(carry),
        f"{swork}/pull-state",
        str(slot / "pull-dest-state"),
        ctx.wrapper,
    ]
    if ctx.transport.config:
        argv.append(ctx.transport.config)
    logs = ctx.work / "logs"
    pull = gb.run_measured(
        argv, ctx.native_env, logs / f"{stem}-pull.stdout", logs / f"{stem}-pull.stderr"
    )
    parsed = gb.parse_native(pull["stdout"], pull["stderr"])
    apply_argv = [
        ctx.agent,
        *ctx.native_flags,
        "estate-apply",
        str(carry / "plan.postcard"),
        str(carry / "corpus"),
        str(slot / "private"),
        "m3-source",
        jobs,
    ]
    applied = gb.run_measured(
        apply_argv,
        ctx.env,
        logs / f"{stem}-apply.stdout",
        logs / f"{stem}-apply.stderr",
    )
    src_heads = {r[0]: r[3] for r in ctx.git_items}
    got = {r[0]: r[3] for r in local("repos", {"root": str(dest)})["rows"]}
    missing = sorted(k for k in src_heads if got.get(k) != src_heads[k])
    problems = []
    for name, code in (
        ("capture", cap["exit"]),
        ("pull", pull["exit"]),
        ("apply", applied["exit"]),
    ):
        if code != 0:
            problems.append(f"{name} exit {code}")
    if missing:
        problems.append(f"{len(missing)} checkouts missing or at another HEAD")
    sample: dict[str, object] = {
        "arm": "G",
        "exit": 0 if not problems else 1,
        "elapsed_ms": round(
            capture_ms + float(pull["elapsed_ms"]) + float(applied["elapsed_ms"]), 3
        ),
        "capture_ms": capture_ms,
        "capture_source_ms": cap["elapsed_ms"],
        "pull_ms": pull["elapsed_ms"],
        "apply_ms": applied["elapsed_ms"],
        "pull_bytes_received": parsed["transfer"].get("bytes_received"),
        "pull_source_bytes_read": parsed["transfer"].get("source_bytes_read"),
        "capture_counters": ab.pairs(
            next(
                (ln for ln in cap["stderr"].splitlines() if ln.startswith("counters ")),
                "",
            )
        ),
        "checkouts": len(src_heads),
        "verified": {
            "ok": not missing,
            "heads_differ": len(missing),
            "examples": missing[:5],
        },
    }
    if problems:
        sample["problems"] = problems + [
            gb.redact(cap["stderr"])[-300:],
            gb.redact(applied["stderr"])[-300:],
        ]
    return sample


# ----------------------------------------------------------------- verdicts


def med(samples: list[dict], arm: str, phase: str) -> float | None:
    return gb.median(
        [
            float(s["elapsed_ms"])
            for s in samples
            if s.get("arm") == arm and s.get("phase") == phase
        ]
    )


def rep_verdict(rep: dict[str, object]) -> dict[str, object]:
    samples = rep["samples"]  # type: ignore[assignment]
    out: dict[str, object] = {"medians_ms": {}}
    for phase in ("initial", "rerun", "delta"):
        out["medians_ms"][phase] = {  # type: ignore[index]
            arm: med(samples, arm, phase)  # type: ignore[arg-type]
            for arm in sorted({str(s["arm"]) for s in samples})  # type: ignore[union-attr]
        }
    m = out["medians_ms"]
    for base in ("P", "R", "S", "Y"):
        for phase in ("initial", "rerun", "delta"):
            b, x = m[phase].get("B"), m[phase].get(base)  # type: ignore[index]
            if b is not None and x is not None:
                out[f"B_vs_{base}_{phase}"] = round(b / x, 4)
    reruns = [s for s in samples if s["arm"] == "B" and s["phase"] == "rerun"]  # type: ignore[index]
    out["s3_rerun_zero"] = bool(reruns) and all(
        s.get("bytes_received") == 0 and s.get("source_bytes_read") == 0 for s in reruns
    )
    first = m["initial"].get("B")  # type: ignore[index]
    again = m["rerun"].get("B")  # type: ignore[index]
    out["s3_rerun_ratio"] = round(again / first, 4) if first and again else None
    deltas = [s for s in samples if s["arm"] == "B" and s["phase"] == "delta"]  # type: ignore[index]
    plan_bytes = (rep.get("delta") or {}).get("changed_seat_bytes")  # type: ignore[union-attr]
    out["s3_delta_reads_bounded"] = bool(deltas) and all(
        isinstance(s.get("source_bytes_read"), int)
        and s["source_bytes_read"] <= int(plan_bytes or 0)
        for s in deltas
    )
    natives = [s for s in samples if s["arm"] == "B"]  # type: ignore[index]
    out["rss_ok"] = bool(natives) and all(
        int(s.get("max_rss_bytes") or 0) < RSS_CAP_BYTES
        and isinstance(s.get("serve_max_rss_bytes"), int)
        and int(s["serve_max_rss_bytes"]) < RSS_CAP_BYTES
        for s in natives
    )
    out["under_load"] = any(not s.get("gated", True) for s in samples)  # type: ignore[union-attr]
    for base in ("P", "R", "S"):
        for phase in ("initial", "delta"):
            ratio = out.get(f"B_vs_{base}_{phase}")
            out[f"{phase}_win_vs_{base}"] = ratio is not None and ratio < 1
    # Gate (b)'s rep rule (OI-1002-Q30, Q82/Q89) against plain rclone
    # (Q150): initial and delta medians win, the warm rerun reads and
    # receives 0, RSS under 2 GiB. The S3 bound on delta reads is added.
    out["gate_b_rule"] = bool(
        out["initial_win_vs_P"]
        and out["delta_win_vs_P"]
        and out["s3_rerun_zero"]
        and out["s3_delta_reads_bounded"]
        and out["rss_ok"]
    )
    git = rep.get("git")
    if isinstance(git, dict):
        gs = git["samples"]
        out["git_medians_ms"] = {
            p: {a: med(gs, a, p) for a in ("G", "R")} for p in ("initial", "rerun")
        }
        g, r = (
            out["git_medians_ms"]["initial"]["G"],
            out["git_medians_ms"]["initial"]["R"],
        )
        out["G_vs_R_initial"] = round(g / r, 4) if g and r else None
    return out


# ----------------------------------------------------------------- evidence


def fmt(v: object, digits: int = 1) -> str:
    if v is None:
        return "n/a"
    if isinstance(v, float):
        return f"{v:,.{digits}f}"
    if isinstance(v, int):
        return f"{v:,}"
    return str(v)


def evidence(report: dict[str, object]) -> str:
    w = report["workload"]
    lines = [
        f"# M3 real-link proof: {report['source']['host']} -> "
        f"{report['destination']['node']}, {report['stamp']}",
        "",
        f"**{NOT_SAMPLE if report['mode'] == 'dry-run' else FIELD}.**"
        f" Rulings: {report['rulings']}. Format `{FORMAT}`. Status:"
        f" `{report['status']}`{(' - ' + str(report.get('reason'))) if report.get('reason') else ''}.",
        "",
        "## Workload",
        "",
        f"- Corpus: {w.get('kind')} `{w.get('identity') or w.get('subset_digest')}`;"
        f" {fmt(w.get('comparable_files'))} comparable files,"
        f" {fmt(w.get('comparable_bytes'))} bytes; {fmt(w.get('excluded'))} SQLite"
        f" seats excluded from every arm.",
        f"- Delta: {fmt(w.get('delta_files'))} files, {fmt(w.get('delta_bytes'))}"
        f" bytes (XOR, `{report['args']['delta_mode']}`).",
        f"- Order per phase: `{report['args']['order']}`"
        + (
            f"; Git stage `{GIT_ORDER}` over {fmt(w.get('git_checkouts'))} checkouts"
            if w.get("git_checkouts")
            else ""
        ),
        f"- rclone: `{report['arms'].get('rclone_version')}` with"
        f" `{' '.join(report['arms'].get('rclone_tuning', []))}`"
        f" (tuning: {report['arms'].get('tuning_note', 'fixed')}).",
        f"- Native: `{' '.join(report['arms'].get('native_flags', []))}`,"
        f" source priority `{report['args']['source_priority']}`.",
        "",
        "## Results (median wall ms per rep)",
        "",
        "| rep | phase | "
        + " | ".join(ARM_NAMES[a] for a in "BPRSY")
        + " | B/P | B/R | B/S |",
        "|---|---|" + "---:|" * 8,
    ]
    for rep in report["reps"]:
        v = rep.get("verdict") or {}
        rep.setdefault("index", "?")
        rep.setdefault("samples", [])
        for phase in ("initial", "rerun", "delta"):
            m = (v.get("medians_ms") or {}).get(phase, {})
            lines.append(
                f"| {rep['index']} | {phase} | "
                + " | ".join(fmt(m.get(a)) for a in "BPRSY")
                + "".join(f" | {fmt(v.get(f'B_vs_{x}_{phase}'), 3)}" for x in "PRS")
                + " |"
            )
    lines += ["", "## Per-rep checks", ""]
    for rep in report["reps"]:
        v = rep.get("verdict") or {}
        link = rep.get("link") or {}
        ip = link.get("iperf3") or {}
        lines.append(
            f"- rep {rep['index']}: gate (b) rule vs plain rclone"
            f" `{v.get('gate_b_rule')}` (RSS ok `{v.get('rss_ok')}`, under load"
            f" `{v.get('under_load')}`);"
            f" S3 rerun zero `{v.get('s3_rerun_zero')}` (rerun/initial"
            f" {fmt(v.get('s3_rerun_ratio'), 3)}, S3 bound 0.10); delta reads bounded"
            f" `{v.get('s3_delta_reads_bounded')}`; ssh ceiling"
            f" {fmt((link.get('ssh_single') or {}).get('bytes_per_s'))} B/s single,"
            f" {fmt((link.get('ssh_parallel') or {}).get('bytes_per_s'))} B/s parallel;"
            f" iperf3 source->dest {fmt((ip.get('source_to_dest') or {}).get('bytes_per_s'))}"
            f" B/s{(' (' + str(ip['skipped']) + ')') if ip.get('skipped') else ''}."
        )
        if v.get("git_medians_ms"):
            lines.append(
                f"  Git: G initial {fmt(v['git_medians_ms']['initial']['G'])} ms vs R"
                f" {fmt(v['git_medians_ms']['initial']['R'])} ms (G/R"
                f" {fmt(v.get('G_vs_R_initial'), 3)}); rerun G"
                f" {fmt(v['git_medians_ms']['rerun']['G'])} ms vs R"
                f" {fmt(v['git_medians_ms']['rerun']['R'])} ms."
            )
    lines += [
        "",
        "## Load",
        "",
        "| rep | arm | phase | source load1 before/after | dest load1 before/after |",
        "|---|---|---|---|---|",
    ]
    for rep in report["reps"]:
        for s in rep["samples"] + ((rep.get("git") or {}).get("samples") or []):
            c = s.get("conditions") or {}
            b, a = c.get("before") or {}, c.get("after") or {}
            lines.append(
                f"| {rep['index']} | {s['arm']} | {s['phase']} |"
                f" {(b.get('source') or {}).get('load1')}/{(a.get('source') or {}).get('load1')} |"
                f" {(b.get('destination') or {}).get('load1')}/{(a.get('destination') or {}).get('load1')} |"
            )
    lines += [
        "",
        "Every sample's argv, counters, RSS and conditions are in the JSON beside"
        " this file. A field run never decides S1 (OI-1003-Q97); it is reported"
        " beside the rig verdict, and a field fail is filed as an issue.",
        "",
    ]
    return "\n".join(lines)


def finish(report: dict[str, object], work: Path, md: Path | None) -> None:
    report = gb.scrub(report)  # type: ignore[assignment]
    text = json.dumps(report, indent=1, default=str)
    (work / "m3-link.json").write_text(text)
    (work / "m3-link.md").write_text(evidence(report))
    if md is not None:
        md.parent.mkdir(parents=True, exist_ok=True)
        md.write_text(evidence(report))
        md.with_suffix(".json").write_text(text)
    say(f"status={report['status']} json={work / 'm3-link.json'} evidence={md}")


# ---------------------------------------------------------------- run setup


def nix_tool(repo: Path, attr: str, binary: str) -> str | None:
    """A tool from the nixpkgs the repo flake pins (no system package)."""
    try:
        outputs = subprocess.run(
            [
                "nix",
                "build",
                "--no-link",
                "--print-out-paths",
                "--inputs-from",
                str(repo),
                f"nixpkgs#{attr}",
            ],
            capture_output=True,
            text=True,
            check=True,
        ).stdout.split()
    except (OSError, subprocess.CalledProcessError):
        return None
    for output in outputs:
        if (Path(output) / "bin" / binary).is_file():
            return str(Path(output) / "bin" / binary)
    return None


def refuse(code: str, reason: str, report: dict | None, work: Path | None) -> int:
    say(f"refused {code}: {reason}")
    if report is not None and work is not None:
        report["status"], report["reason"] = "refused", f"{code}: {reason}"
        finish(report, work, None)
    return 2


def tune_rclone(ctx: Ctx, values: list[int]) -> tuple[list[str], dict[str, object]]:
    """One initial copy per --transfers value (checkers = 2x); the fastest wins.

    Run before the sample, outside every verdict, so R and S use a tuned
    rclone and not the gate (a) default of 4/4.
    """
    trials: dict[str, object] = {}
    best: tuple[float, int] | None = None
    for value in values:
        dest = ctx.work / "tune" / f"t{value}"
        dest.mkdir(parents=True)
        tuning = ["--transfers", str(value), "--checkers", str(2 * value)]
        argv = rclone_tuned_argv(ctx, ctx.corpus, dest, tuning)
        run = gb.run_measured(
            argv,
            ctx.env,
            ctx.work / "logs" / f"tune-{value}.stdout",
            ctx.work / "logs" / f"tune-{value}.stderr",
        )
        trials[str(value)] = {"exit": run["exit"], "elapsed_ms": run["elapsed_ms"]}
        if run["exit"] == 0 and (best is None or run["elapsed_ms"] < best[0]):
            best = (float(run["elapsed_ms"]), value)
        shutil.rmtree(dest, ignore_errors=True)
    chosen = best[1] if best else values[0]
    return ["--transfers", str(chosen), "--checkers", str(2 * chosen)], trials


def cmd_run(args: argparse.Namespace) -> int:
    mode = "dry-run" if args.dry_run else "field"
    repo = Path(args.repo).resolve()
    work = Path(args.work_root).absolute()
    if work.exists() or not work.parent.is_dir():
        return refuse(
            "WORK_ROOT", f"must be new under an existing parent: {work}", None, None
        )
    if set(args.order) - set("BPRSY") or "B" not in args.order:
        return refuse(
            "ORDER",
            f"--order letters are B, P, R, S, Y with B: {args.order}",
            None,
            None,
        )
    work.mkdir(mode=0o700)
    for sub in ("logs", "reps", "bin"):
        (work / sub).mkdir()
    now = dt.datetime.now(dt.timezone.utc)
    agent = Path(args.agent).resolve()
    report: dict[str, object] = {
        "format": FORMAT,
        "mode": mode,
        "status": "running",
        "rulings": RULINGS,
        "date": now.strftime("%Y-%m-%d"),
        "stamp": now.strftime("%Y-%m-%d-%H%MZ"),
        "args": {
            k: v
            for k, v in vars(args).items()
            if k
            in (
                "order",
                "reps",
                "delta_mode",
                "source_priority",
                "min_free_percent",
                "git_jobs",
                "git_subtree",
                "rclone_tune",
                "calibrate_mib",
                "calibrate_streams",
            )
        },
        "source": {"host": args.source_host},
        "destination": {
            "node": platform.node(),
            "system": platform.system(),
            "agent_sha256": ab.sha256(agent),
        },
        "workload": {},
        "arms": {},
        "reps": [],
    }
    ssh_bin = shutil.which(args.ssh) or args.ssh
    if mode == "dry-run":
        sftp = next(
            (
                p
                for p in (
                    shutil.which("sftp-server"),
                    "/usr/libexec/openssh/sftp-server",
                    "/usr/lib/openssh/sftp-server",
                )
                if p and os.path.isfile(p)
            ),
            None,
        )
        if sftp is None:
            return refuse("DRY_RUN", "no local sftp-server", report, work)
        shim = work / "bin" / "ssh"
        shim.write_text(gb.LOOPBACK_SHIM.format(sftp_server=shlex.quote(sftp)))
        shim.chmod(0o700)
        transport = Transport(str(shim), "m3-loopback", None, sys.executable)
        args.source_work = str(work / "source")
        args.remote_agent = str(agent)
        args.remote_rsync = args.remote_rsync or None
        args.calibrate_mib = min(args.calibrate_mib, 8)
        if args.iperf3 and not args.remote_iperf3:
            args.remote_iperf3 = args.iperf3
        report["source"]["host"] = "m3-loopback"
    else:
        (work / "bin" / "ssh").symlink_to(ssh_bin)
        transport = Transport(
            str(work / "bin" / "ssh"),
            args.source_host,
            args.ssh_config,
            args.source_python,
        )
    if args.iperf3 == "nix":
        args.iperf3 = nix_tool(repo, "iperf3", "iperf3")
        if mode == "dry-run" and args.remote_iperf3 in (None, "nix"):
            args.remote_iperf3 = args.iperf3
    flags = ["--priority=normal"] if args.source_priority == "normal" else []
    try:
        probe = transport.helper(
            "probe",
            {
                "agent": args.remote_agent,
                "corpus": args.source_corpus,
                "work": args.source_work,
            },
        )
        report["source"].update({k: probe.get(k) for k in ("system", "python", "cpus")})
        report["source"]["agent_sha256"] = (probe.get("agent") or {}).get("sha256")
        if not (probe.get("agent") or {}).get("executable"):
            return refuse(
                "NATIVE_REMOTE_ARM_MISSING", "no agent on the source", report, work
            )
        if probe.get("work_exists") or not probe.get("work_parent_is_dir"):
            return refuse("SOURCE_WORK", "--source-work must be new", report, work)
        if args.source_subset:
            sources = [s.split("=", 1) for s in args.source_subset]
            prepared = transport.helper(
                "subset",
                {
                    "sources": sources,
                    "work": args.source_work,
                    "python": args.source_python
                    if mode != "dry-run"
                    else sys.executable,
                    "agent": args.remote_agent,
                    "flags": flags,
                },
            )
            report["workload"].update(
                {
                    "kind": "real-subset",
                    "labels": [s[0] for s in sources],
                    "subset_digest": hashlib.sha256(
                        json.dumps(sources).encode()
                    ).hexdigest()[:16],
                    "skipped": prepared.get("skipped"),
                }
            )
        else:
            seal = probe.get("seal") or {}
            report["workload"].update(
                {
                    "kind": seal.get("format"),
                    "identity": seal.get("identity"),
                    "sealed": seal.get("sealed"),
                }
            )
            if mode == "field" and seal.get("format") not in (
                CORPUS_FORMAT,
                "bulkload-estate-corpus-v1",
            ):
                return refuse(
                    "CORPUS", f"not an M3 or estate corpus: {seal}", report, work
                )
            measured = transport.helper("measure", {"corpus": args.source_corpus})
            copies = (
                len(args.order)
                + (2 if args.git else 0)
                + (1 if mode == "dry-run" else 0)
            )
            budget = gb.disk_budget(
                *gb.volume_space(work),
                copies,
                int(measured["comparable_bytes"]),
                int(measured["comparable_files"]) + int(measured["directories"]),
                floor_percent=args.min_free_percent,
            )
            report["destination"]["disk"] = budget
            if not budget["ok"] and not args.keep_destinations:
                return refuse("DEST_SPACE", json.dumps(budget), report, work)
            prepared = transport.helper(
                "prepare",
                {
                    "corpus": args.source_corpus,
                    "work": args.source_work,
                    "python": args.source_python
                    if mode != "dry-run"
                    else sys.executable,
                    "agent": args.remote_agent,
                    "flags": flags,
                },
            )
        manifest = transport.helper("manifest", {"root": prepared["corpus"]})
        git_rows: list[list[object]] = []
        if args.git:
            git_rows = transport.helper(
                "repos", {"root": f"{prepared['corpus']}/{args.git_subtree}"}
            )["rows"]
            if not git_rows:
                return refuse(
                    "GIT", f"no checkouts under {args.git_subtree}", report, work
                )
    except gb.Abort as abort:
        return refuse("SOURCE", str(abort), report, work)
    rows, exclusions = manifest["rows"], manifest["exclusions"]
    plan = delta_plan_files(rows, exclusions, args.delta_percent, args.delta_cap)
    report["workload"].update(
        {
            "entries": len(rows),
            "comparable_files": sum(
                1 for r in rows if r[1] == "f" and r[0] not in exclusions
            ),
            "comparable_bytes": gb.comparable_bytes(rows, exclusions),
            "excluded": len(exclusions),
            "manifest_digest": manifest["digest"],
            "delta_files": len(plan),
            "delta_bytes": sum(int(p[1]) for p in plan),
            "git_checkouts": len(git_rows),
            "source_shared_extents": prepared.get("shared_extents"),
        }
    )
    rclone_ex = work / "rclone-excludes.txt"
    gb.write_private(
        rclone_ex, "".join(gb.rclone_glob(p) + "\n" for p in sorted(exclusions))
    )
    rsync_ex = work / "rsync-excludes.txt"
    gb.write_private(
        rsync_ex, "".join(rsync_pattern(p) + "\n" for p in sorted(exclusions))
    )
    env = gb.rclone_env()
    remote, refusal = gb.rclone_remote(transport, "external", None, env)
    if refusal:
        return refuse(refusal.code, refusal.reason, report, work)
    conf = work / "rclone.conf"
    gb.write_private(conf, gb.rclone_config_text(remote))
    rclone = args.rclone or str(ab.flake_rclone(repo))
    native_env = dict(os.environ)
    native_env["PATH"] = f"{work / 'bin'}{os.pathsep}{native_env.get('PATH', '')}"
    native_flags = [f"--min-free-percent={args.min_free_percent}"]
    ctx = Ctx(
        args=args,
        mode=mode,
        work=work,
        transport=transport,
        agent=str(agent),
        corpus=prepared["corpus"],
        wrapper=prepared["wrapper"],
        states=prepared["states"],
        rows=rows,
        exclusions=exclusions,
        digest=manifest["digest"],
        plan=plan,
        rclone=rclone,
        rclone_conf=conf,
        rclone_excludes=rclone_ex,
        rsync_excludes=rsync_ex,
        rclone_tuning=[
            "--transfers",
            str(args.rclone_transfers),
            "--checkers",
            str(args.rclone_checkers),
        ],
        env=env,
        native_env=native_env,
        native_flags=native_flags,
        git_items=git_rows,
    )
    tuning_note = "fixed by flags"
    if args.rclone_tune and ("R" in args.order or "S" in args.order):
        ctx.rclone_tuning, trials = tune_rclone(
            ctx, [int(v) for v in args.rclone_tune.split(",")]
        )
        report["arms"]["rclone_tune_trials"] = trials
        tuning_note = f"fastest of --transfers {args.rclone_tune} (checkers 2x)"
    report["arms"].update(
        {
            "rclone": rclone,
            "rclone_version": ab.rclone_version(Path(rclone)),
            "rclone_tuning": ctx.rclone_tuning,
            "tuning_note": tuning_note,
            "rsync": args.rsync,
            "native_flags": native_flags,
            "iperf3": bool(args.iperf3 and args.remote_iperf3),
            "names": ARM_NAMES,
        }
    )
    evidence_md = None
    if mode == "field":
        evidence_md = (
            repo
            / "docs"
            / "evidence"
            / f"m3-link-{report['stamp']}-{args.source_host}-{platform.node()}.md"
        )
    try:
        time.sleep(SETTLE_S)
        for index in range(args.reps):
            report["reps"].append(run_rep(ctx, index, report))
    except gb.Abort as abort:
        if abort.rep is not None:
            report["reps"].append(abort.rep)
        report["status"], report["reason"] = "aborted", str(abort)[:2000]
        finish(report, work, evidence_md)
        return 3
    report["status"] = (
        "dry-run-complete-not-a-sample"
        if mode == "dry-run"
        else "complete-field-informational"
    )
    finish(report, work, evidence_md)
    return 0


# ---------------------------------------------------------------------- cli


def parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = p.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("census", help="read-only distribution of a tree")
    c.add_argument("root")
    c.add_argument("--label", required=True)
    c.add_argument("--kind", choices=("state", "git"), default="state")
    c.add_argument("--out")
    c.add_argument("--ssh-host", help="census a tree on this ssh host instead")
    c.add_argument("--ssh-config")
    c.add_argument("--ssh", default="ssh")
    c.add_argument("--source-python", default="/usr/bin/python3")
    g = sub.add_parser("generate", help="synthetic corpus from census profiles")
    g.add_argument("dest")
    g.add_argument("--profile", action="append", required=True)
    g.add_argument("--bytes", type=int, required=True)
    g.add_argument("--seed", default=DEFAULT_SEED)
    g.add_argument("--no-seal", action="store_true")
    v = sub.add_parser("verify", help="recompute a generated corpus's identity")
    v.add_argument("dest")
    f = sub.add_parser("fixture", help="a tiny Git estate for the dry run")
    f.add_argument("dest")
    r = sub.add_parser("run", help="the arm matrix between two hosts")
    r.add_argument("--repo", default=str(Path(__file__).resolve().parents[3]))
    r.add_argument("--work-root", required=True, help="new local directory")
    r.add_argument("--agent", required=True, help="local bulkload-agent")
    r.add_argument("--source-host", default="neo")
    r.add_argument("--ssh", default="ssh")
    r.add_argument("--ssh-config", help="absolute ssh config, used by every arm")
    r.add_argument("--source-python", default="/usr/bin/python3")
    r.add_argument(
        "--source-corpus", help="generated (or estate) corpus DEST on the source"
    )
    r.add_argument(
        "--source-subset",
        action="append",
        default=[],
        help="LABEL=PATH of a real source directory (read-only), repeatable",
    )
    r.add_argument("--source-work", help="new working-copy root on the source")
    r.add_argument("--remote-agent", help="bulkload-agent on the source")
    r.add_argument(
        "--source-priority", choices=("background", "normal"), default="background"
    )
    r.add_argument("--order", default=DEFAULT_ORDER)
    r.add_argument("--reps", type=int, default=3)
    r.add_argument("--delta-mode", choices=("inplace", "remove"), default="inplace")
    r.add_argument("--delta-percent", type=float, default=DELTA_FILE_PERCENT)
    r.add_argument("--delta-cap", type=int, default=DELTA_MAX_BYTES_PER_FILE)
    r.add_argument(
        "--min-free-percent",
        type=int,
        default=25,
        help="the native arms' destination floor (agent default 25)",
    )
    r.add_argument("--source-min-free-percent", type=int, default=25)
    r.add_argument("--rclone", help="default: nixpkgs#rclone from the repo flake")
    r.add_argument("--rclone-transfers", type=int, default=16)
    r.add_argument("--rclone-checkers", type=int, default=32)
    r.add_argument("--rclone-tune", default="", help="e.g. 4,8,16,32: tune first")
    r.add_argument("--rsync", default="rsync")
    r.add_argument("--remote-rsync")
    r.add_argument("--iperf3", default="nix", help="local iperf3, 'nix', or ''")
    r.add_argument("--remote-iperf3", help="source iperf3 command (shell words)")
    r.add_argument("--iperf3-port", type=int, default=5299)
    r.add_argument("--iperf3-seconds", type=int, default=10)
    r.add_argument("--calibrate-mib", type=int, default=64)
    r.add_argument("--calibrate-streams", type=int, default=4)
    r.add_argument("--git", action="store_true", help="add the Git arm (G/R/G)")
    r.add_argument("--git-subtree", default="git")
    r.add_argument("--git-jobs", type=int, default=2)
    r.add_argument("--keep-destinations", action="store_true")
    r.add_argument("--dry-run", action="store_true", help=NOT_SAMPLE)
    return p


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    return {
        "census": cmd_census,
        "generate": cmd_generate,
        "verify": cmd_verify,
        "fixture": cmd_fixture,
        "run": cmd_run,
    }[args.cmd](args)


if __name__ == "__main__":
    sys.exit(main())
