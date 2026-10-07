#!/usr/bin/env python3
"""Tests for s3_estate.py (Sprint 2 lane A: OI-1003-Q18, OI-1003-Q35, R-N13).

Run: python3 -m unittest crates/bulkload-bench/scripts/test_s3_estate.py
Pure tests cover the tolerant parsers, the OI-1003-Q18 bounds and the
verdicts. One end-to-end test generates the small estate corpus and drives
all six passes with a shell stub in place of bulkload-agent, so it needs git
>= 2.45 and Python 3.12; it is skipped otherwise.
"""

from __future__ import annotations

import argparse
import contextlib
import importlib.util
import io
import json
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
SPEC = importlib.util.spec_from_file_location("s3_estate", HERE / "s3_estate.py")
s3 = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(s3)

COPY_OUT = (
    "completed=3 reused=2 bytes_received=4096 source_bytes_read=4096 refusals=2 "
    "source_engine_temporaries=0 capped_subtrees=0\n"
    "transfer_timing verb=copy side=both scope=process priority=background "
    "priority_from=default walk_ns=10\n"
    "counters verb=copy side=both scope=process priority=background "
    "priority_from=default elapsed_ns=99 read_source_file_bytes=4096 census_walks=0\n"
)
COPY_ERR = (
    "refused state.sqlite: SQLITE_STATE_CHANGED\n"
    "refused sessions/a b\\xc3\\xa9.jsonl: GIT_DESTINATION_OCCUPIED\n"
    "bulkload-agent: refused: CONTRACT_SELF_INCONSISTENT\n"
)
CAPTURE_OUT = (
    'item=0 source="/w/git/r00 x" outcome=captured reason=None source_bytes_read=10\n'
    'item=0 nested path="vendor/lib" carried-by=1\n'
    'item=1 source="/w/git/r01" outcome=capture-reused-after-census '
    'reason=Some("a \\"q\\" b") source_bytes_read=0 reuse_unavailable=shallow\n'
)


class ParseTests(unittest.TestCase):
    def test_tokens_keep_quoted_runs(self) -> None:
        self.assertEqual(
            s3.tokens('a=1 b="x y" c=Some("p q") d'),
            ["a=1", 'b="x y"', 'c=Some("p q")', "d"],
        )

    def test_pairs_casts_and_skips_malformed(self) -> None:
        row = s3.pairs('x=1 y=-2 z=0.5 w="a \\"b\\"" bare =v k=')
        self.assertEqual(row, {"x": 1, "y": -2, "z": 0.5, "w": 'a "b"', "k": ""})

    def test_copy_lines(self) -> None:
        self.assertEqual(s3.transfer_line(COPY_OUT)["bytes_received"], 4096)
        row = s3.counters(COPY_OUT)
        self.assertEqual(row["read_source_file_bytes"], 4096)
        self.assertEqual(row["priority"], "background")
        self.assertEqual(
            s3.refusals(COPY_ERR),
            [
                ("state.sqlite", "SQLITE_STATE_CHANGED"),
                ("sessions/a bé.jsonl", "GIT_DESTINATION_OCCUPIED"),
            ],
        )
        self.assertEqual(s3.final_refusal(COPY_ERR), "CONTRACT_SELF_INCONSISTENT")

    def test_unescape_ascii_invalid_utf8(self) -> None:
        self.assertEqual(s3.unescape_ascii("a\\xff\\t\\\\"), b"a\xff\t\\")
        path, _ = s3.refusals("refused n\\xff: IO (errno 2)\n")[0]
        self.assertEqual(path, "n\\xff")

    def test_receipts_attach_extra_lines(self) -> None:
        rows = s3.receipts(CAPTURE_OUT)
        self.assertEqual(len(rows), 2)
        self.assertEqual(rows[0]["source"], "/w/git/r00 x")
        self.assertEqual(
            rows[0]["extra"], ['item=0 nested path="vendor/lib" carried-by=1']
        )
        self.assertEqual(rows[1]["reason"], 'Some("a \\"q\\" b")')
        self.assertEqual(rows[1]["reuse_unavailable"], "shallow")

    def test_estimate_blocks(self) -> None:
        text = "source=/a\nmissing_thin_pack_bytes=5\n\nsource=/b\nrefused=GIT_X\n"
        blocks = s3.estimate_blocks(text)
        self.assertEqual([b["source"] for b in blocks], ["/a", "/b"])
        self.assertEqual(blocks[0]["missing_thin_pack_bytes"], 5)
        self.assertEqual(s3.number(blocks[1], "missing_thin_pack_bytes"), 0)


class BoundTests(unittest.TestCase):
    def test_absent_bound(self) -> None:
        m = 256
        self.assertEqual(s3.absent_bound(None, 900, [[0, 900]], m), 900)
        self.assertEqual(s3.absent_bound(100, None, [], m), 0)
        self.assertEqual(s3.absent_bound(5000, 5000, [], m), 5000)
        # 64 bytes at 2000: [1744, 2576)
        self.assertEqual(s3.absent_bound(5000, 5000, [[2000, 2064]], m), 832)
        # overlapping windows merge; the cap is the file
        self.assertEqual(s3.absent_bound(5000, 5000, [[10, 20], [100, 110]], m), 622)
        self.assertEqual(s3.absent_bound(300, 300, [[0, 300]], m), 300)
        # an append: the last chunk plus the new bytes
        self.assertEqual(s3.absent_bound(4000, 4100, [[4000, 4100]], m), 356)

    def test_delta_bounds_split_by_half(self) -> None:
        sidecar = {
            "cdc_bytes": {"max": 256},
            "reads_allowed_class": {
                "data/big": "walk",
                ".codex/s.jsonl": "walk",
                ".codex/state.sqlite": "walk",
                ".bashrc": "walk",
                "git/r00/a.rs": "worktree",
                "git/r00/.git/index": "git-admin",
                "git/r00/.git/objects/ab/cdef": "git-objects",
            },
            "reads_allowed_sizes": {
                "data/big": 10_000,
                ".codex/s.jsonl": 500,
                ".codex/state.sqlite": 8192,
                ".bashrc": 10,
                "git/r00/a.rs": 70,
                "git/r00/.git/objects/ab/cdef": 40,
            },
            "operations": [
                {
                    "path": "data/big",
                    "pre_size": 10_000,
                    "post_size": 10_000,
                    "ranges": [[5000, 5064]],
                },
                {
                    "path": ".codex/s.jsonl",
                    "pre_size": 400,
                    "post_size": 500,
                    "ranges": [[400, 500]],
                },
                {
                    "path": ".codex/state.sqlite",
                    "stores": [
                        {
                            "path": ".codex/state.sqlite",
                            "pre_size": 8192,
                            "post_size": 8192,
                            "ranges": [[4096, 8192]],
                        }
                    ],
                },
            ],
            "reads_by_class": {"worktree": {"items": {"git/r00": {"bytes": 70}}}},
            "git_objects": {"git/r00": {"tips": [{"new_objects": {"bytes": 300}}]}},
            "items": {
                "changed": [{"item": "git/r00"}, {"item": "git/m.git"}],
                "census_walks_expected": 5,
            },
        }
        items = {
            "git/r00": item("git/r00", "main"),
            "git/m.git": item("git/m.git", "mirror"),
        }
        b = s3.delta_bounds(sidecar, ["data", ".codex"], {".codex/state.sqlite"}, items)
        # The SQLite seat belongs to the SQLite half only: copy refuses it.
        self.assertEqual(b["file_read_by_area"], {"data": 10_000, ".codex": 500})
        self.assertEqual(b["file_wire_by_area"], {"data": 832, ".codex": 356})
        self.assertEqual(
            b["file_wire_by_seat"], {"data/big": 832, ".codex/s.jsonl": 356}
        )
        self.assertEqual(b["sqlite_changed"], {".codex/state.sqlite": 8192})
        self.assertEqual(b["sqlite_changed_range_bytes"], 4096)
        self.assertEqual(b["uncovered_walk_seats"], [".bashrc"])
        self.assertEqual(b["git_read_by_item"], {"git/r00": 70})
        self.assertEqual(b["git_worktree_chunk_bound_by_item"], {"git/r00": 70})
        self.assertEqual(b["git_new_object_bytes_by_changed_repo"], {"git/r00": 300})
        self.assertEqual(b["git_object_store_seat_bytes_by_item"], {"git/r00": 40})
        repos = s3.git_wire(b, False)
        self.assertEqual(repos["git/r00"]["bound_chunk"], 370)
        self.assertEqual(repos["git/r00"]["bound_object"], 370)

    def test_git_bound_granularity_and_repo_once(self) -> None:
        """A large edit is bounded by its absent chunks; a repository's new
        objects count once however many of its items changed."""
        sidecar = {
            "cdc_bytes": {"max": 256},
            "reads_allowed_class": {"git/r01/model.bin": "worktree"},
            "reads_allowed_sizes": {"git/r01/model.bin": 1_000_000},
            "operations": [
                {
                    "path": "git/r01/model.bin",
                    "pre_size": 1_000_000,
                    "post_size": 1_000_000,
                    "ranges": [[500_000, 500_064]],
                }
            ],
            "reads_by_class": {
                "worktree": {"items": {"git/r01": {"bytes": 1_000_000}}}
            },
            "git_objects": {"git/r01": {"tips": [{"new_objects": {"bytes": 300}}]}},
            "items": {
                "changed": [{"item": "git/r01"}, {"item": "git/r01.worktrees/tree"}]
            },
        }
        items = {
            "git/r01": item("git/r01", "main"),
            "git/r01.worktrees/tree": item(
                "git/r01.worktrees/tree", "branch", repo="git/r01"
            ),
        }
        b = s3.delta_bounds(sidecar, [], set(), items)
        self.assertEqual(
            b["git_worktree_chunk_bound_by_item"],
            {"git/r01": 832, "git/r01.worktrees/tree": 0},
        )
        repos = s3.git_wire(b, False)
        self.assertEqual(
            repos["git/r01"]["items"], ["git/r01", "git/r01.worktrees/tree"]
        )
        self.assertEqual(repos["git/r01"]["bound_chunk"], 1132)
        self.assertEqual(repos["git/r01"]["bound_object"], 1_000_300)

    def test_sqlite_companions(self) -> None:
        dbs = {".local/s.db"}
        self.assertTrue(s3.is_sqlite_seat(".local/s.db-wal", dbs))
        self.assertTrue(s3.is_sqlite_seat(".local/s.db-shm", dbs))
        self.assertFalse(s3.is_sqlite_seat(".local/t.db-wal", dbs))


def item(name: str, kind: str, repo: str | None = None) -> dict:
    repo = repo or name
    admin = name if kind == "mirror" else f"{repo}/.git"
    return {"item": name, "kind": kind, "repo": repo, "admin": admin}


def child(**extra: object) -> dict:
    row = {"exit": 0, "wall_ns": 1_000_000_000, "cpu_s": 0.5, "counters": {}}
    row.update(extra)
    return row


def entry(
    label: str,
    file_read: int,
    recv: int,
    git_read: int,
    pack: int,
    walks: int,
    sidecar=None,
    cpu: float = 0.5,
    blocked: bool = True,
    readback: int = 0,
) -> dict:
    return {
        "label": label,
        "mutation": {"sidecar": sidecar} if sidecar else None,
        "file": {
            "data": child(
                cpu_s=cpu,
                transfer={"source_bytes_read": file_read, "bytes_received": recv},
                counters={"priority": "background", "priority_from": "default"},
                refused=(
                    [["data/x", "GIT_DESTINATION_OCCUPIED"]]
                    if sidecar and blocked
                    else []
                ),
            ),
            ".codex": child(
                cpu_s=cpu,
                transfer={"source_bytes_read": 0, "bytes_received": 0},
                refused=[[".codex/state.sqlite", "SQLITE_STATE_CHANGED"]],
            ),
        },
        "sqlite": {
            ".codex/state.sqlite": child(
                cpu_s=cpu, db_bytes=8192, wal_bytes=0, output_bytes=8192
            )
        },
        "git": {
            "rest": child(
                cpu_s=cpu,
                counters={
                    "census_walks": walks,
                    "write_source_pack_bytes": pack,
                    "read_source_pack_readback_bytes": readback,
                },
                outcomes={"captured": 1},
                source_bytes_read=git_read,
            ),
            "history": child(
                cpu_s=cpu,
                counters={"census_walks": 0},
                outcomes={},
                source_bytes_read=0,
            ),
        },
        "v2": {
            "rest": child(
                missing_thin_pack_bytes=7,
                missing_objects=1,
                source_history_bytes=9,
                refused=[],
            ),
            "history": child(
                missing_thin_pack_bytes=0,
                missing_objects=0,
                source_history_bytes=0,
                refused=[],
            ),
        },
        "racy_seats": [],
        "s2_v1": {},
        "s2_v2": {},
        "sqlite_identical": {".codex/state.sqlite": True},
        "load1_before": 1.0,
        "load1_after": 1.5,
    }


class EvaluateTests(unittest.TestCase):
    def setUp(self) -> None:
        self.ctx = {
            "areas": ["data", ".codex"],
            "sqlite": [".codex/state.sqlite"],
            "items": [item("git/r00", "main")],
            "plans": {"rest": {"censused": ["git/r00"]}, "history": {"censused": []}},
        }
        self.sidecar = {
            "cdc_bytes": {"max": 256},
            "reads_allowed_class": {"data/x": "walk", "git/r00/a": "worktree"},
            "reads_allowed_sizes": {"data/x": 1000, "git/r00/a": 50},
            "operations": [
                {
                    "path": "data/x",
                    "pre_size": 1000,
                    "post_size": 1000,
                    "ranges": [[0, 1]],
                }
            ],
            "reads_by_class": {"worktree": {"items": {"git/r00": {"bytes": 50}}}},
            "git_objects": {},
            "items": {"changed": [{"item": "git/r00"}]},
            "changed": {"modified": ["data/x"]},
        }

    def test_unchanged_and_delta_verdicts(self) -> None:
        record = {
            "passes": [
                entry("first", 1000, 1000, 50, 900, 4),
                entry("rerun-1", 0, 0, 0, 0, 1, cpu=0.01),
                entry("rerun-2", 0, 0, 0, 0, 1),
                entry("mutate-1", 1000, 600, 50, 2000, 4, self.sidecar, readback=4096),
            ]
        }
        ev = s3.evaluate(record, self.ctx)
        un = ev["rerun-1"]["subsets"]["all"]["unchanged"]
        self.assertEqual(un["file_source_bytes_read"], "pass")
        self.assertEqual(un["git_pack_bytes_written"], "pass")
        self.assertEqual(un["sqlite_whole_database_reads"], "fail")
        self.assertEqual(un["cpu_ratio_le_10pct"], "pass")
        # The wall ratio is informational and never stands in for the CPU one.
        un2 = ev["rerun-2"]["subsets"]["all"]["unchanged"]
        self.assertEqual(un2["cpu_ratio_le_10pct"], "fail")
        self.assertEqual(un2["wall_ratio_le_10pct_informational"], "fail")
        self.assertEqual(
            ev["rerun-1"]["subsets"]["all"]["census_walks"]["verdict"], "pass"
        )
        self.assertEqual(ev["first"]["subsets"]["all"]["census_walks"]["expected"], 4)
        delta = ev["mutate-1"]["subsets"]["all"]["delta"]
        self.assertEqual(delta["file_ineq1"]["verdict"], "pass")
        self.assertEqual(delta["file_ineq1"]["sqlite_header_bytes"], 16)
        # data/x was refused GIT_DESTINATION_OCCUPIED: not carried, so it is
        # outside the bound, and the delta did not converge.
        self.assertEqual(delta["file_ineq2"]["bound"], 0)
        self.assertEqual(delta["file_ineq2"]["bound_with_blocked"], 513)
        self.assertEqual(delta["file_ineq2"]["blocked_seats"], ["data/x"])
        self.assertEqual(delta["file_ineq2"]["verdict"], "n/a: blocked by WP0(d)")
        self.assertEqual(delta["file_ineq2"]["carried_verdict"], "fail")
        self.assertEqual(delta["git_ineq1"]["verdict"], "pass")
        self.assertEqual(
            delta["git_ineq1"]["object_store"]["verdict_if_counted"], "fail"
        )
        self.assertEqual(delta["git_ineq2"]["excess"], 1950)
        self.assertEqual(delta["git_ineq2"]["excess_object"], 1950)
        refusals = ev["mutate-1"]["refusals"]
        self.assertEqual(refusals["blocked by WP0(d)"], ["data/x"])
        self.assertEqual(refusals["routed-to-snapshot"], [".codex/state.sqlite"])
        without = ev["mutate-1"]["subsets"]["without-data"]["delta"]
        self.assertEqual(
            (without["file_ineq1"]["measured"], without["file_ineq1"]["bound"]), (0, 0)
        )
        self.assertEqual(without["file_ineq2"]["verdict"], "pass")

    def test_carried_delta_is_judged(self) -> None:
        record = {
            "passes": [
                entry("first", 1000, 1000, 50, 900, 4),
                entry("mutate-1", 1000, 500, 50, 50, 4, self.sidecar, blocked=False),
            ]
        }
        delta = s3.evaluate(record, self.ctx)["mutate-1"]["subsets"]["all"]["delta"]
        self.assertEqual(delta["file_ineq2"]["bound"], 513)
        self.assertEqual(delta["file_ineq2"]["verdict"], "pass")
        self.assertEqual(
            delta["git_ineq1"]["object_store"]["verdict_if_counted"],
            "unknown (readback is a lower bound)",
        )

    def test_reevaluate_writes_a_new_record(self) -> None:
        record = {
            "context": self.ctx,
            "passes": [entry("first", 1, 1, 1, 1, 4), entry("rerun-1", 0, 0, 0, 0, 1)],
        }
        with tempfile.TemporaryDirectory() as tmp:
            src = Path(tmp) / "s3-estate.json"
            src.write_text(json.dumps(record))
            out = Path(tmp) / "again.json"
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(s3.reevaluate(src, out, None), 0)
                self.assertEqual(s3.reevaluate(src, out, None), 2)
                self.assertEqual(s3.reevaluate(src, src, None), 2)
            again = json.loads(out.read_text())
            self.assertEqual(json.loads(src.read_text()), record)
        self.assertIn("rerun-1", again["evaluation"])
        self.assertEqual(again["reevaluated"]["from"], str(src))

    def test_report_renders(self) -> None:
        record = {
            "scale": "small",
            "label": s3.LABEL,
            "agent_sha256": "0" * 64,
            "passes": [entry("first", 1, 1, 1, 1, 4), entry("rerun-1", 0, 0, 0, 0, 1)],
        }
        record["evaluation"] = s3.evaluate(record, self.ctx)
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "r.json"
            path.write_text(json.dumps(record))
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                self.assertEqual(s3.report(path), 0)
        self.assertIn("| rerun-1 |", out.getvalue())
        self.assertIn("informational, ungated", out.getvalue())


AGENT_STUB = r"""#!/bin/sh
verb="$1"; shift
case "$verb" in
  estate-add-batch) echo "counters verb=estate-add-batch side=local scope=process priority=normal priority_from=default census_walks=0" >&2 ;;
  copy)
    echo "completed=1 reused=0 bytes_received=0 source_bytes_read=0 refusals=0 source_engine_temporaries=0 capped_subtrees=0"
    echo "counters verb=copy side=both scope=process priority=background priority_from=default read_source_file_bytes=0" ;;
  snapshot) : > "$2"; echo "snapshot complete"
    echo "counters verb=snapshot side=local scope=process priority=background priority_from=default" >&2 ;;
  estate-capture)
    echo "item=0 source=\"$1\" outcome=capture-reused-after-census reason=None source_bytes_read=0"
    echo "counters verb=estate-capture side=local scope=process priority=background priority_from=default census_walks=1 write_source_pack_bytes=0" >&2 ;;
  git-carry-estimate)
    shift 2
    while [ $# -gt 1 ]; do echo "source=$1"; echo "missing_thin_pack_bytes=0"; echo; shift 2; done
    echo "counters verb=git-carry-estimate side=local scope=process priority=background priority_from=default" >&2 ;;
  *) exit 2 ;;
esac
"""


def git_ok() -> bool:
    git = shutil.which("git")
    if not git or sys.version_info < (3, 12):
        return False
    out = subprocess.run(
        [git, "--version"], capture_output=True, text=True
    ).stdout.split()
    try:
        major, minor = (int(x) for x in out[2].split(".")[:2])
    except (IndexError, ValueError):
        return False
    return (major, minor) >= (2, 45)


@unittest.skipUnless(git_ok(), "needs git >= 2.45 and Python 3.12")
class StubRunTests(unittest.TestCase):
    def test_six_passes_with_a_stub_agent(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            agent = Path(tmp) / "agent"
            agent.write_text(AGENT_STUB)
            agent.chmod(agent.stat().st_mode | stat.S_IXUSR)
            args = argparse.Namespace(
                agent=str(agent),
                work=str(Path(tmp) / "work"),
                scale="small",
                seed=s3.ec.DEFAULT_SEED,
                jobs=2,
                min_available_gib=6.0,
                build_json=None,
            )
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(s3.Measure(args).run(args), 0)
            record = json.loads((Path(tmp) / "work" / "s3-estate.json").read_text())
            self.assertEqual(
                [p["label"] for p in record["passes"]], [p for p, _ in s3.PASSES]
            )
            ev = record["evaluation"]
            self.assertEqual(ev["first"]["subsets"]["all"]["s3"], "baseline")
            self.assertIn("unchanged", ev["rerun-3"]["subsets"]["all"])
            self.assertIn("delta", ev["mutate-10"]["subsets"]["without-both"])
            self.assertIn("data", record["context"]["areas"])
            self.assertNotIn("git", record["context"]["areas"])
            self.assertIn(".bashrc", record["context"]["uncovered"])
            self.assertEqual(record["v2_models"]["m0"]["stash_entries"], 4)
            self.assertEqual(record["final_verify"]["child"]["exit"], 0)


if __name__ == "__main__":
    unittest.main()
