#!/usr/bin/env python3
"""Q54 precondition: does the real blahaj carry and restore exactly under v1's
ref table (main 40adca8, #182)?

Rulings: OI-1003-Q54 and OI-1003-Q55 (prove a blahaj-scale repository carries
under v1 before carry_v2 is deleted), OI-1003-Q35 (byte counters and the
rusage CPU are the admissible measures; wall time is informational), R-N13.
Label: informational, sting, ungated.

The source repository is read only. Every git command this script runs in it
is a read (for-each-ref, rev-parse, symbolic-ref, worktree list, stash list)
with GIT_OPTIONAL_LOCKS=0; its `.git` is only lstat'ed by the census. The
agent's own source reads are what S2 is about, and the census proves them.

The git half is driven the way crates/bulkload-bench/scripts/s3_estate.py
drives it (main 40adca8): `estate-add-batch` of a one-item plan, then
`estate-capture PLAN PRIVATE_STATE CORPUS 2`, each child under s3_estate's
Runner (RUSAGE_CHILDREN deltas, raw output kept under WORK/logs) and its
environment (no inherited GIT_*, an empty global config, no system config).
Restore is `estate-apply PLAN CORPUS APPLY_STATE SOURCE_LABEL 1` with no
workspace: refs only, into a fresh bare repository.

Subcommands, each appending to WORK/q54-blahaj.json:
  census LABEL      lstat census of SOURCE/.git (mode, size, mtime_ns,
                    ctime_ns, ino, nlink), saved to WORK/census/LABEL.json
  baseline LABEL    for-each-ref (objectname, refname, symref), HEAD, the
                    symbolic HEAD, worktree list and stash list
  plan              estate-add-batch of SOURCE -> WORK/dest/blahaj.git, no workspace
  capture LABEL     estate-capture, with the new corpus files, the header
                    bytes and the shallow envelope's manifest
  apply             git init --bare the destination, then estate-apply
  compare           restored ref set against baseline `before`
  census-diff A B   compare two censuses

Usage: harness.py --agent BIN --work WORK [--source DIR] SUBCOMMAND ...
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import stat
import subprocess
import sys
import time
from pathlib import Path

SCRIPTS = Path(
    os.environ.get(
        "BULKLOAD_SCRIPTS",
        "/srv/fast-local/jess/git/bulkload.worktrees/q54-blahaj-carry-20261005"
        "/crates/bulkload-bench/scripts",
    )
)
sys.path.insert(0, str(SCRIPTS))
import s3_estate as s3  # noqa: E402

SOURCE_LABEL = "blahaj-q54"
LABEL = "informational, sting, ungated"
RULINGS = "OI-1003-Q54, OI-1003-Q55, OI-1003-Q35, R-N13"
CARRY = "refs/carry/v1/"


def now() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def load1() -> float:
    return float(Path("/proc/loadavg").read_text().split()[0])


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


class Work:
    def __init__(self, args: argparse.Namespace):
        self.agent = Path(args.agent).resolve()
        self.work = Path(args.work).absolute()
        self.source = Path(args.source).resolve()
        self.record_path = self.work / "q54-blahaj.json"
        self.plan = self.work / "plan" / "plan.json"
        self.state = self.work / "state"
        self.corpus = self.work / "corpus"
        self.apply_state = self.work / "apply-state"
        self.dest = self.work / "dest" / "blahaj.git"
        s3.private_dir(self.work)
        self.env = s3.git_env(self.work)
        self.runner = s3.Runner(self.work, self.env)

    # -- record --

    def load(self) -> dict:
        if self.record_path.exists():
            return json.loads(self.record_path.read_text())
        return {
            "format": "bulkload-q54-blahaj-carry-v1",
            "label": LABEL,
            "rulings": RULINGS,
            "agent": str(self.agent),
            "agent_sha256": s3.sha256(self.agent),
            "source": str(self.source),
            "source_label": SOURCE_LABEL,
            "harness_sha256": s3.sha256(Path(__file__).resolve()),
            "host": os.uname().nodename,
            "steps": {},
        }

    def save(self, record: dict) -> None:
        tmp = self.record_path.with_suffix(".tmp")
        tmp.write_text(json.dumps(record, indent=1, sort_keys=True) + "\n")
        tmp.replace(self.record_path)

    def step(self, key: str, value: dict) -> None:
        record = self.load()
        value["harness_sha256"] = s3.sha256(Path(__file__).resolve())
        record["steps"][key] = value
        self.save(record)

    # -- read-only git in the source --

    def read_git(self, *argv: str) -> subprocess.CompletedProcess:
        env = dict(self.env)
        env["GIT_OPTIONAL_LOCKS"] = "0"
        return subprocess.run(
            ["git", "-C", str(self.source), *argv],
            env=env,
            capture_output=True,
            check=False,
        )

    # -- census --

    def census(self, label: str) -> dict:
        root = self.source / ".git"
        rows: dict[str, list[int]] = {}
        stack = [root]
        start = time.monotonic_ns()

        def row(path: Path) -> None:
            info = os.lstat(path)
            rel = os.fsdecode(path.relative_to(self.source))
            rows[rel] = [
                info.st_mode,
                info.st_size,
                info.st_mtime_ns,
                info.st_ctime_ns,
                info.st_ino,
                info.st_nlink,
            ]

        row(root)
        while stack:
            directory = stack.pop()
            with os.scandir(directory) as entries:
                for entry in entries:
                    path = Path(entry.path)
                    row(path)
                    if entry.is_dir(follow_symlinks=False):
                        stack.append(path)
        canonical = json.dumps(sorted(rows.items()), separators=(",", ":")).encode()
        out = self.work / "census" / f"{label}.json"
        s3.private_dir(out.parent)
        out.write_text(json.dumps(rows, sort_keys=True) + "\n")
        summary = {
            "taken": now(),
            "entries": len(rows),
            "sha256": sha256_bytes(canonical),
            "file": str(out),
            "seconds": round((time.monotonic_ns() - start) / 1e9, 3),
            "fields": ["mode", "size", "mtime_ns", "ctime_ns", "ino", "nlink"],
        }
        self.step(f"census/{label}", summary)
        return summary

    def census_diff(self, a: str, b: str) -> dict:
        left = json.loads((self.work / "census" / f"{a}.json").read_text())
        right = json.loads((self.work / "census" / f"{b}.json").read_text())
        fields = ["mode", "size", "mtime_ns", "ctime_ns", "ino", "nlink"]
        added = sorted(set(right) - set(left))
        removed = sorted(set(left) - set(right))
        changed = {}
        for key in sorted(set(left) & set(right)):
            if left[key] != right[key]:
                changed[key] = {
                    f: [lv, rv]
                    for f, lv, rv in zip(fields, left[key], right[key])
                    if lv != rv
                }
        out = {
            "a": a,
            "b": b,
            "identical": not (added or removed or changed),
            "added": added,
            "removed": removed,
            "changed": changed,
        }
        self.step(f"census-diff/{a}..{b}", out)
        return out

    # -- baseline --

    def baseline(self, label: str) -> dict:
        directory = s3.private_dir(self.work / "baseline" / label)
        out: dict = {"taken": now()}
        commands = {
            "for-each-ref": [
                "for-each-ref",
                "--format=%(objectname) %(refname) %(symref)",
            ],
            "head": ["rev-parse", "--verify", "HEAD"],
            "head-symbolic": ["symbolic-ref", "-q", "HEAD"],
            "worktree-list": ["worktree", "list", "--porcelain"],
            "stash-list": ["stash", "list", "--format=%H"],
            "object-format": ["rev-parse", "--show-object-format"],
            "shallow-file": ["rev-parse", "--path-format=absolute", "--git-path", "shallow"],
        }
        for name, argv in commands.items():
            done = self.read_git(*argv)
            (directory / f"{name}.out").write_bytes(done.stdout)
            out[name] = {
                "argv": ["git", "-C", str(self.source), *argv],
                "exit": done.returncode,
                "bytes": len(done.stdout),
                "lines": done.stdout.count(b"\n"),
                "sha256": sha256_bytes(done.stdout),
            }
            if name in ("head", "head-symbolic", "object-format"):
                out[name]["value"] = done.stdout.decode().strip()
        refs = self.baseline_refs(label)
        out["refs"] = len(refs)
        out["symrefs"] = {n: s for n, (_, s) in refs.items() if s}
        out["distinct_oids"] = len({o for o, _ in refs.values()})
        out["canonical_carry_refs"] = sum(1 for n in refs if self.canonical(n))
        out["native_refs"] = len(refs) - out["canonical_carry_refs"]
        sorted_lines = "".join(f"{o} {n} {s}\n" for n, (o, s) in sorted(refs.items()))
        out["refset_sha256"] = sha256_bytes(sorted_lines.encode())
        out["old_format_header_model_bytes"] = self.old_header_model(refs)
        stashes = (directory / "stash-list.out").read_text().split()
        out["stashes"] = stashes
        shallow = Path((directory / "shallow-file.out").read_text().strip())
        out["shallow_frontier_lines"] = (
            len(shallow.read_text().split()) if shallow.exists() else 0
        )
        out["worktrees"] = self.worktrees(label)
        self.step(f"baseline/{label}", out)
        return out

    def baseline_refs(self, label: str) -> dict[str, tuple[str, str]]:
        text = (self.work / "baseline" / label / "for-each-ref.out").read_text()
        refs = {}
        for line in text.splitlines():
            oid, name, symref = (line.split(" ", 2) + [""])[:3]
            if name in refs:
                raise SystemExit(f"duplicate ref {name}")
            refs[name] = (oid, symref)
        return refs

    def worktrees(self, label: str) -> dict:
        text = (self.work / "baseline" / label / "worktree-list.out").read_text()
        rows, cur = [], {}
        for line in text.splitlines():
            if not line:
                if cur:
                    rows.append(cur)
                cur = {}
                continue
            key, _, val = line.partition(" ")
            cur[key] = val
        if cur:
            rows.append(cur)
        root = str(self.source) + "/"
        nested = {
            r["worktree"][len(root) :]: r.get("HEAD", "")
            for r in rows[1:]
            if r["worktree"].startswith(root)
        }
        return {
            "total": len(rows),
            "main": rows[0] if rows else {},
            "nested_inside_checkout": len(nested),
            "outside_checkout": len(rows) - 1 - len(nested),
            "nested": nested,
        }

    @staticmethod
    def canonical(name: str) -> bool:
        if not name.startswith(CARRY):
            return False
        parts = name[len(CARRY) :].split("/", 2)
        if len(parts) != 3:
            return False
        slug, snap, suffix = parts
        ok_slug = 0 < len(slug) <= 64 and all(c.isalnum() or c in "-_" for c in slug)
        ok_snap = len(snap) == 64 and all(c in "0123456789abcdefABCDEF" for c in snap)
        return ok_slug and ok_snap and bool(suffix)

    def old_header_model(self, refs: dict[str, tuple[str, str]]) -> int:
        """The pre-#182 header's ref term: one `<oid> <exported name>\\n`
        line per carried ref, as git_carry::exported_refs names it."""
        total = 0
        for name, (oid, _) in refs.items():
            if self.canonical(name):
                exported = "refs/carry-export/union/v1/" + name[len(CARRY) :]
            else:
                exported = "refs/carry-export/" + name
            total += len(oid) + 1 + len(exported) + 1
        return total

    # -- the agent --

    def run_child(self, step: str, argv: list[str]) -> dict:
        before = {"load1": load1(), "at": now()}
        child = self.runner.run(step, argv)
        after = {"load1": load1(), "at": now()}
        out = s3.compact(child)
        out["before"] = before
        out["after"] = after
        out["counters"] = s3.counters(child["stderr"]) or s3.counters(child["stdout"])
        out["receipts"] = s3.receipts(child["stdout"])
        out["final_refusal"] = s3.final_refusal(child["stderr"])
        out["stderr_tail"] = child["stderr"].splitlines()[-5:]
        return out

    def plan_items(self) -> dict:
        s3.private_dir(self.plan.parent)
        out = self.run_child(
            "plan/estate-add-batch",
            [
                str(self.agent),
                "estate-add-batch",
                str(self.plan),
                str(self.source),
                str(self.dest),
                "-",
            ],
        )
        self.step("plan", out)
        return out

    def corpus_files(self) -> dict[str, int]:
        if not self.corpus.exists():
            return {}
        return {
            e.name: e.stat(follow_symlinks=False).st_size
            for e in os.scandir(self.corpus)
        }

    def capture(self, label: str) -> dict:
        s3.private_dir(self.state)
        s3.private_dir(self.corpus)
        seen = self.corpus_files()
        envelopes_before = {str(p) for p in self.state.glob("*/shallow-envelope.git")}
        out = self.run_child(
            f"{label}/estate-capture",
            [
                str(self.agent),
                "estate-capture",
                str(self.plan),
                str(self.state),
                str(self.corpus),
                "2",
            ],
        )
        after = self.corpus_files()
        new = {k: v for k, v in after.items() if seen.get(k) != v}
        out["corpus_new_or_changed"] = new
        out["corpus_total_bytes"] = sum(after.values())
        out["new_bundle_bytes"] = sum(
            v for k, v in new.items() if k.endswith(".bundle")
        )
        out["source_bytes_read"] = sum(
            s3.number(r, "source_bytes_read") for r in out["receipts"]
        )
        out["headers"] = {
            k: self.header(self.corpus / k) for k in new if k.endswith(".bundle")
        }
        out["envelopes"] = {
            p: self.envelope(Path(p))
            for p in sorted(
                {str(p) for p in self.state.glob("*/shallow-envelope.git")}
                - envelopes_before
            )
        }
        out["private_repositories"] = {
            str(p): self.private_refs(p) for p in sorted(self.state.glob("*/repository.git"))
        }
        out["state_bytes"] = du(self.state)
        self.step(f"capture/{label}", out)
        return out

    @staticmethod
    def header(bundle: Path) -> dict:
        data = b""
        with bundle.open("rb") as handle:
            while b"\n\n" not in data and len(data) < (64 << 20):
                block = handle.read(1 << 20)
                if not block:
                    break
                data += block
        end = data.find(b"\n\n")
        head = data[: end + 2] if end >= 0 else data
        lines = head.decode("utf-8", "replace").splitlines()
        refs = [ln for ln in lines[1:] if ln and not ln.startswith(("-", "@", "#"))]
        return {
            "header_bytes": len(head),
            "signature": lines[0] if lines else "",
            "ref_lines": len(refs),
            "prerequisite_lines": sum(1 for ln in lines if ln.startswith("-")),
            "capability_lines": sum(1 for ln in lines if ln.startswith("@")),
            "ref_names": sorted(ln.split(" ", 1)[1] for ln in refs if " " in ln)
            if len(refs) <= 16
            else None,
            "bundle_bytes": bundle.stat().st_size,
        }

    def envelope(self, envelope: Path) -> dict:
        def git(*argv: str) -> bytes:
            return subprocess.run(
                ["git", "--git-dir", str(envelope), *argv],
                env=self.env,
                capture_output=True,
                check=True,
            ).stdout

        custody = git("rev-parse", "refs/carry-export/shallow-custody-v1").decode().strip()
        manifest = git("cat-file", "blob", f"{custody}:value")
        pack_size = int(git("cat-file", "-s", f"{custody}:objects.pack").decode())
        boundary, rest = postcard_bytes(manifest)
        inventory, rest = postcard_bytes(rest)
        pack_oid, rest = postcard_bytes(rest)
        lines = inventory.decode().splitlines()
        names = [ln.split(" ", 1)[1] for ln in lines]
        return {
            "custody": custody,
            "manifest_bytes": len(manifest),
            "manifest_cap": 16 * 1024 * 1024,
            "frontier_bytes": len(boundary),
            "inner_inventory_bytes": len(inventory),
            "inner_inventory_lines": len(lines),
            "ref_tip_lines": sum(1 for n in names if n.startswith("refs/carry-export/ref-tip-v1/")),
            "ref_table_lines": sum(1 for n in names if n == "refs/carry-ref-table/v1"),
            "metadata_lines": sorted(
                n
                for n in names
                if n != "refs/carry-ref-table/v1"
                and not n.startswith("refs/carry-export/ref-tip-v1/")
            ),
            "inner_pack_bytes": pack_size,
            "inner_pack_oid": pack_oid.decode(),
            "trailing_bytes": len(rest),
        }

    def private_refs(self, private: Path) -> dict:
        done = subprocess.run(
            ["git", "--git-dir", str(private), "for-each-ref", "--format=%(refname)"],
            env=self.env,
            capture_output=True,
            check=False,
        )
        names = done.stdout.decode().splitlines()
        table = subprocess.run(
            [
                "git",
                "--git-dir",
                str(private),
                "ls-tree",
                "-r",
                "-l",
                "refs/carry-ref-table/v1",
            ],
            env=self.env,
            capture_output=True,
            check=False,
        ).stdout.decode()
        sizes = [int(ln.split()[3]) for ln in table.splitlines() if ln.split()[3].isdigit()]
        return {
            "refs": len(names),
            "ref_tips": sum(1 for n in names if n.startswith("refs/carry-export/ref-tip-v1/")),
            "ref_table": "refs/carry-ref-table/v1" in names,
            "table_blobs": len(sizes),
            "table_blob_bytes": sum(sizes),
        }

    def apply(self) -> dict:
        s3.private_dir(self.dest.parent)
        init = None
        if not self.dest.exists():
            init = subprocess.run(
                ["git", "init", "--bare", "--quiet", "--template=", str(self.dest)],
                env=self.env,
                capture_output=True,
                check=False,
            ).returncode
        out = self.run_child(
            "apply/estate-apply",
            [
                str(self.agent),
                "estate-apply",
                str(self.plan),
                str(self.corpus),
                str(self.apply_state),
                SOURCE_LABEL,
                "1",
            ],
        )
        out["dest_init_exit"] = init
        out["dest_bytes"] = du(self.dest)
        out["apply_state_bytes"] = du(self.apply_state)
        self.step("apply", out)
        return out

    # -- compare --

    def compare(self, base_label: str) -> dict:
        baseline = self.baseline_refs(base_label)
        record = self.load()
        base = record["steps"][f"baseline/{base_label}"]
        done = subprocess.run(
            [
                "git",
                "--git-dir",
                str(self.dest),
                "for-each-ref",
                "--format=%(objectname) %(refname) %(symref)",
            ],
            env=self.env,
            capture_output=True,
            check=True,
        )
        (self.work / "dest-for-each-ref.out").write_bytes(done.stdout)
        restored: dict[str, tuple[str, str]] = {}
        for line in done.stdout.decode().splitlines():
            oid, name, symref = (line.split(" ", 2) + [""])[:3]
            restored[name] = (oid, symref)
        own = f"{CARRY}{SOURCE_LABEL}/"
        digests = {n[len(own) :].split("/", 1)[0] for n in restored if n.startswith(own)}
        unexpected = sorted(n for n in restored if not n.startswith(CARRY))
        native: dict[str, str] = {}
        union: dict[str, str] = {}
        stashes: dict[str, str] = {}
        metadata: dict[str, str] = {}
        for name, (oid, _) in restored.items():
            if not name.startswith(CARRY):
                continue
            if name.startswith(own):
                tail = name[len(own) :].split("/", 1)[1]
                if tail.startswith("refs/"):
                    native[tail] = oid
                elif tail.startswith("stashes/"):
                    stashes[tail[len("stashes/") :]] = oid
                else:
                    metadata[tail] = oid
            else:
                union[name] = oid
        mapped = {**union, **native}
        names_missing = sorted(set(baseline) - set(mapped))
        names_extra = sorted(set(mapped) - set(baseline))
        oid_diff = sorted(
            n for n in set(baseline) & set(mapped) if baseline[n][0] != mapped[n]
        )
        restored_symrefs = sorted(n for n, (_, s) in restored.items() if s)
        symref_rows = {
            n: {
                "source_symref": s,
                "source_resolved_oid": baseline[n][0],
                "restored_oid": mapped.get(n),
                "restored_as": "plain ref at the resolved oid"
                if mapped.get(n) == baseline[n][0]
                else "MISMATCH",
            }
            for n, (_, s) in baseline.items()
            if s
        }
        head_oid = metadata.get("head")
        head_symbolic = None
        if "head-symbolic" in metadata:
            head_symbolic = self.dest_blob(f"{metadata['head-symbolic']}:value").decode()
        nested = None
        if "nested-worktrees-v1" in metadata:
            nested = decode_nested_worktrees(
                self.dest_blob(f"{metadata['nested-worktrees-v1']}:value")
            )
        base_nested = base["worktrees"]["nested"]
        nested_cmp = None
        if nested is not None:
            carried = {r[0]: r[2] for r in nested}
            nested_cmp = {
                "carried": len(carried),
                "baseline": len(base_nested),
                "equal": carried == base_nested,
                "missing": sorted(set(base_nested) - set(carried)),
                "extra": sorted(set(carried) - set(base_nested)),
                "head_diff": sorted(
                    k for k in set(carried) & set(base_nested) if carried[k] != base_nested[k]
                ),
            }
        shallow_dest = (self.dest / "shallow").read_text().split() if (
            self.dest / "shallow"
        ).exists() else []
        source_shallow = Path(
            (self.work / "baseline" / base_label / "shallow-file.out").read_text().strip()
        )
        shallow_src = source_shallow.read_text().split() if source_shallow.exists() else []
        out = {
            "baseline": base_label,
            "restored_refs": len(restored),
            "snapshot_digests": sorted(digests),
            "unexpected_outside_carry": unexpected,
            "union_refs": len(union),
            "native_refs": len(native),
            "stash_refs": len(stashes),
            "metadata_refs": sorted(metadata),
            "ref_tip_refs_imported": sum(1 for n in restored if "ref-tip-v1" in n),
            "baseline_refs": len(baseline),
            "names_missing": names_missing[:50],
            "names_missing_count": len(names_missing),
            "names_extra": names_extra[:50],
            "names_extra_count": len(names_extra),
            "oid_diff": oid_diff[:50],
            "oid_diff_count": len(oid_diff),
            "restored_symrefs": restored_symrefs,
            "source_symrefs": symref_rows,
            "head": {
                "source_oid": base["head"]["value"],
                "carried_oid": head_oid,
                "equal": head_oid == base["head"]["value"],
                "source_symbolic": base["head-symbolic"]["value"],
                "carried_symbolic": head_symbolic,
                "symbolic_equal": head_symbolic == base["head-symbolic"]["value"],
            },
            "stashes": {
                "source": base["stashes"],
                "carried": sorted(stashes),
                "equal": sorted(stashes) == sorted(base["stashes"])
                and all(k == v for k, v in stashes.items()),
            },
            "nested_worktrees": nested_cmp,
            "shallow": {
                "source": shallow_src,
                "dest": shallow_dest,
                "equal": shallow_src == shallow_dest,
            },
        }
        out["exact_names_oids"] = not (names_missing or names_extra or oid_diff)
        self.step(f"compare/{base_label}", out)
        return out

    def dest_blob(self, rev: str) -> bytes:
        return subprocess.run(
            ["git", "--git-dir", str(self.dest), "cat-file", "blob", rev],
            env=self.env,
            capture_output=True,
            check=True,
        ).stdout


def postcard_varint(data: bytes) -> tuple[int, bytes]:
    value, shift = 0, 0
    for index, byte in enumerate(data):
        value |= (byte & 0x7F) << shift
        if not byte & 0x80:
            return value, data[index + 1 :]
        shift += 7
    raise ValueError("truncated varint")


def postcard_bytes(data: bytes) -> tuple[bytes, bytes]:
    length, rest = postcard_varint(data)
    return rest[:length], rest[length:]


def decode_nested_worktrees(data: bytes) -> list[tuple[str, str, str]]:
    count, rest = postcard_varint(data)
    rows = []
    for _ in range(count):
        rel, rest = postcard_bytes(rest)
        name, rest = postcard_bytes(rest)
        head, rest = postcard_bytes(rest)
        rows.append((os.fsdecode(rel), name.decode(), head.decode()))
    if rest:
        raise ValueError("trailing bytes in nested-worktrees-v1")
    return rows


def du(path: Path) -> int:
    total = 0
    if not path.exists():
        return 0
    for root, dirs, files in os.walk(path):
        for name in files + dirs:
            info = os.lstat(os.path.join(root, name))
            if stat.S_ISREG(info.st_mode):
                total += info.st_size
    return total


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--agent", required=True)
    parser.add_argument("--work", required=True)
    parser.add_argument("--source", default="/srv/fast-local/jess/git/blahaj")
    parser.add_argument("command")
    parser.add_argument("rest", nargs="*")
    args = parser.parse_args()
    os.umask(0o077)
    work = Work(args)
    s3.private_dir(work.work)
    if args.command == "census":
        result = work.census(args.rest[0])
    elif args.command == "census-diff":
        result = work.census_diff(args.rest[0], args.rest[1])
    elif args.command == "baseline":
        result = work.baseline(args.rest[0])
    elif args.command == "plan":
        result = work.plan_items()
    elif args.command == "capture":
        result = work.capture(args.rest[0])
    elif args.command == "apply":
        result = work.apply()
    elif args.command == "compare":
        result = work.compare(args.rest[0])
    else:
        parser.error(f"unknown command {args.command}")
    brief = {
        k: v
        for k, v in result.items()
        if k not in ("changed", "nested", "receipts", "worktrees", "symrefs", "envelopes")
    }
    print(json.dumps(brief, indent=1, sort_keys=True, default=str)[:6000])
    return 0


if __name__ == "__main__":
    sys.exit(main())
