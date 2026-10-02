#!/usr/bin/env python3
"""Stub-binary tests for r23_ab.py (#88, OI-1002-Q27).

Run: python3 -m unittest crates/bulkload-bench/scripts/test_r23_ab.py
A shell stub stands in for bulkload-bench; builds and host conditions are
patched, not real.
"""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import os
import stat
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("r23_ab", HERE / "r23_ab.py")
ab = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ab)

BENCH_STUB = """#!/bin/sh
if [ "$1" = micro ]; then
  echo "micro name=residency source=x files=1 source_resident_fraction=1.0000"
  exit 0
fi
sealed=$(cat "$2"/* 2>/dev/null | cksum | cut -d' ' -f1)
echo "benchmark revision=r rclone_version=\\"rclone v1.75.0\\" sealed_corpus_blake3=s$sealed"
echo "sample sequence=0 arm=Native phase=initial elapsed_ms=100.0 workload_bytes=1000 transferred_content_bytes=900 source_bytes_read=1000 power=ac load1=1.00 gated=true"
echo "native_timing sequence=0 phase=initial scope=s walk_ns=50 walk_ahead_wait_ns=20 queue_wait_ns=1"
echo "native_counters sequence=0 phase=initial scope=s flush_barrier_ns=10000000 flush_full_ns=5000000 flush_dir_ns=0 files_materialized=10"
echo "sample sequence=1 arm=Rclone phase=initial elapsed_ms=80.0 workload_bytes=1000 transferred_content_bytes=unknown source_bytes_read=unknown power=ac load1=1.00 gated=true"
echo "verdict status=${STUB_VERDICT:-fail} r23_initial_win=false"
exit 1
"""


def stub(path: Path) -> Path:
    path.write_text(BENCH_STUB)
    path.chmod(path.stat().st_mode | stat.S_IXUSR)
    return path


class ParseTests(unittest.TestCase):
    def test_pairs_casts_and_quotes(self) -> None:
        row = ab.pairs('a=1 b=2.5 c=true d="x y" e=Native')
        self.assertEqual(row, {"a": 1, "b": 2.5, "c": True, "d": "x y", "e": "Native"})

    def test_summary_reads_new_walk_wait_counter_and_seal_proxy(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            out = ab.subprocess.run(
                [str(stub(Path(tmp) / "bench"))],
                capture_output=True,
                text=True,
                check=False,
            ).stdout
        summary = ab.summarize(ab.parse_bench(out))
        self.assertEqual(summary["flush_barrier_ns_total"], 10_000_000)
        self.assertEqual(summary["flush_full_ns_total"], 5_000_000)
        self.assertEqual(summary["walk_ahead_wait_keys"], ["walk_ahead_wait_ns"])
        self.assertTrue(summary["receive_stall_source"].startswith("proxy:"))
        self.assertAlmostEqual(summary["receive_stall_share_of_wall"], 0.15)
        self.assertAlmostEqual(summary["files_per_s_median"], 100.0)
        self.assertEqual(summary["bytes_received_median"], 900)
        self.assertTrue(summary["all_gated"])


def cond(ok: bool = True, load: float | None = None) -> dict[str, object]:
    return {
        "utc": "t",
        "load1": load if load is not None else (1.0 if ok else 9.0),
        "power": "ac",
        "ok": ok,
    }


class HarnessTests(unittest.TestCase):
    def setUp(self) -> None:
        self.raw = tempfile.TemporaryDirectory()
        self.tmp = Path(self.raw.name)
        self.addCleanup(self.raw.cleanup)
        self.corpus = self.tmp / "sealed"
        self.corpus.mkdir()
        (self.corpus / "f").write_bytes(b"x" * 7)
        bench = stub(self.tmp / "bench")
        self.info = {
            "rev": "r",
            "sha": "f" * 40,
            "binary": str(bench),
            "sha256": "0" * 64,
        }

    def main(self, extra: list[str], **patches: object) -> int:
        """Run main() with builds, rclone and (unless given) host checks patched."""
        targets = {
            "build": mock.patch.object(ab, "build", return_value=self.info),
            "resolve_rclone": mock.patch.object(
                ab, "resolve_rclone", return_value=self.tmp / "rclone"
            ),
            "conditions": mock.patch.object(ab, "conditions", return_value=cond()),
            "corpus_verify": mock.patch.object(ab, "corpus_verify", return_value=0),
            "corpus_shape": mock.patch.object(
                ab, "corpus_shape", return_value=(ab.RECORD_FILES, ab.RECORD_BYTES)
            ),
            "system": mock.patch.object(ab.platform, "system", return_value="Darwin"),
        }
        for name, value in patches.items():
            targets[name] = value
        with contextlib.ExitStack() as stack:
            for patcher in targets.values():
                stack.enter_context(patcher)
            stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
            return ab.main(
                ["--repo", str(self.tmp), "--work-root", str(self.tmp / "work"), *extra]
            )

    def gated(self) -> list[str]:
        return [
            "--corpus",
            str(self.corpus),
            "--coordinator-quiet",
            "--evidence",
            str(self.tmp / "ev.md"),
            "--post-settle-seconds",
            "0",
        ]

    def report(self) -> dict[str, object]:
        return json.loads((self.tmp / "work" / "r23-ab.json").read_text())

    def test_dry_run_end_to_end_is_marked_not_a_gate_sample(self) -> None:
        self.assertEqual(self.main(["--dry-run"]), 0)
        report = self.report()
        self.assertEqual(report["status"], "dry-run-complete-not-a-gate-sample")
        self.assertEqual([r["label"] for r in report["reps"]], list("BABAB") + ["V4"])
        self.assertEqual(report["gate"]["verdict"], "NOT A GATE SAMPLE")
        self.assertAlmostEqual(report["dedup"]["b_dup_share"], 0.0)
        md = next((self.tmp / "work").glob("r23-dryrun-*Z.md")).read_text()
        self.assertIn(ab.NOT_GATE, md)
        self.assertIn("## Per-rep bench verdicts", md)

    def test_dry_run_refuses_docs_evidence(self) -> None:
        target = self.tmp / "docs" / "evidence" / "x.md"
        self.assertEqual(self.main(["--dry-run", "--evidence", str(target)]), 2)
        self.assertFalse((self.tmp / "work").exists())

    def test_gated_refusals(self) -> None:
        linux = mock.patch.object(ab.platform, "system", return_value="Linux")
        self.assertEqual(self.main(self.gated(), system=linux), 2)
        no_quiet = [a for a in self.gated() if a != "--coordinator-quiet"]
        self.assertEqual(self.main(no_quiet), 2)
        self.assertEqual(self.main([*self.gated(), "--expect-files", "1"]), 2)
        bad = mock.patch.object(ab, "corpus_verify", return_value=1)
        self.assertEqual(self.main(self.gated(), corpus_verify=bad), 2)
        self.assertFalse((self.tmp / "work").exists())

    def test_gated_refuses_any_pattern_but_babab(self) -> None:
        for pattern in ("B", "BA", "BBBBB", "ABABA", "BABABA"):
            self.assertEqual(self.main([*self.gated(), "--pattern", pattern]), 2)
        self.assertFalse((self.tmp / "work").exists())

    def test_rollup_needs_exactly_three_b_reps(self) -> None:
        def report(b_reps: int) -> dict[str, object]:
            rep = {"label": "B", "summary": {"verdict": {"status": "pass"}}}
            return {"mode": "gated", "status": "complete-draft", "reps": [rep] * b_reps}

        self.assertTrue(ab.gate_rollup(report(1))["verdict"].startswith("NONE"))
        self.assertTrue(ab.gate_rollup(report(5))["verdict"].startswith("NONE"))
        self.assertEqual(ab.gate_rollup(report(3))["verdict"], "PASS")

    def test_cached_binary_is_reused_only_with_matching_sha256(self) -> None:
        build_root = self.tmp / "build"
        scratch = self.tmp / "scratch"
        scratch.mkdir()
        sha = "a" * 40
        binary = build_root / "bin" / f"bulkload-bench-{sha[:12]}"
        binary.parent.mkdir(parents=True)
        binary.write_bytes(b"old")
        record = binary.with_name(binary.name + ".sha256")
        record.write_text("0" * 64 + "\n")
        built = build_root / f"target-{sha[:12]}" / "release" / "bulkload-bench"

        def fake_git(_repo: Path, *args: str) -> str:
            if args[0] == "archive":
                with tarfile.open(args[args.index("-o") + 1], "w"):
                    pass
            return sha

        def fake_cargo(*_a: object, **_k: object) -> mock.Mock:
            built.parent.mkdir(parents=True, exist_ok=True)
            built.write_bytes(b"new")
            return mock.Mock(returncode=0)

        with (
            mock.patch.object(ab, "git", side_effect=fake_git),
            mock.patch.object(ab.subprocess, "run", side_effect=fake_cargo) as cargo,
            contextlib.redirect_stdout(io.StringIO()),
        ):
            info = ab.build(self.tmp, "rev", build_root, scratch, 1)
            self.assertEqual(cargo.call_count, 1)
            self.assertEqual(binary.read_bytes(), b"new")
            self.assertEqual(record.read_text().strip(), info["sha256"])
            ab.build(self.tmp, "rev", build_root, scratch / "again", 1)
            self.assertEqual(cargo.call_count, 1)

    def test_gated_rollup_needs_every_b_rep_to_pass(self) -> None:
        with mock.patch.dict(os.environ, {"STUB_VERDICT": "pass"}):
            self.assertEqual(self.main(self.gated()), 0)
        report = self.report()
        self.assertEqual(report["gate"]["verdict"], "PASS")
        self.assertEqual(report["gate"]["b_reps_pass"], 3)
        self.assertTrue(report["content_verified_after"])
        self.assertIn(
            "**R23 gate verdict for B: PASS**", (self.tmp / "ev.md").read_text()
        )

    def test_gated_rollup_fails_when_any_b_rep_fails(self) -> None:
        self.assertEqual(self.main(self.gated()), 0)
        self.assertEqual(self.report()["gate"]["verdict"], "FAIL")

    def test_aborts_when_conditions_fail_between_reps(self) -> None:
        states = iter([cond(), cond(), cond(), cond(False)])
        flip = mock.patch.object(
            ab, "conditions", side_effect=lambda: next(states, cond(False))
        )
        self.assertEqual(self.main(self.gated(), conditions=flip), 3)
        report = self.report()
        self.assertEqual(report["status"], "aborted")
        self.assertEqual(len(report["reps"]), 1)
        self.assertIn("precondition failed", report["reason"])
        md = (self.tmp / "ev.md").read_text()
        self.assertIn("(ABORTED)", md.splitlines()[0])
        self.assertNotIn("## Medians per revision", md)

    def test_aborts_when_load_stays_high_after_a_rep(self) -> None:
        states = iter([cond(), cond(), cond(False)])
        high = mock.patch.object(
            ab, "conditions", side_effect=lambda: next(states, cond(False))
        )
        self.assertEqual(self.main(self.gated(), conditions=high), 3)
        self.assertIn("load1=9.0 still >=", self.report()["reason"])

    def test_aborts_when_corpus_changes_between_reps(self) -> None:
        calls = {"n": 0}

        def tamper(_binary: Path, corpus: Path) -> float:
            calls["n"] += 1
            if calls["n"] == 2:
                (corpus / "f").write_bytes(b"y" * 7)
            return 1.0

        patch = mock.patch.object(ab, "residency", side_effect=tamper)
        self.assertEqual(self.main(self.gated(), residency=patch), 3)
        report = self.report()
        self.assertEqual(report["status"], "aborted")
        self.assertEqual(len(report["reps"]), 2)
        self.assertIn("sealed_corpus_blake3", report["reason"])

    def test_aborts_when_corpus_fails_final_verify(self) -> None:
        results = iter([0, 0, 1, 0])
        late = mock.patch.object(
            ab, "corpus_verify", side_effect=lambda _c: next(results, 1)
        )
        self.assertEqual(self.main(self.gated(), corpus_verify=late), 3)
        report = self.report()
        self.assertEqual(report["status"], "aborted")
        self.assertFalse(report["content_verified_after"])
        self.assertIn("no longer verifies", report["reason"])


if __name__ == "__main__":
    unittest.main()
