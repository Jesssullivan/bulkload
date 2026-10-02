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
import stat
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
echo 'benchmark revision=r rclone_version="rclone v1.75.0" sealed_corpus_blake3=abc'
echo "sample sequence=0 arm=Native phase=initial elapsed_ms=100.0 workload_bytes=1000 transferred_content_bytes=900 source_bytes_read=1000 power=ac load1=1.00 gated=true"
echo "native_timing sequence=0 phase=initial scope=s walk_ns=50 walk_ahead_wait_ns=20 queue_wait_ns=1"
echo "native_counters sequence=0 phase=initial scope=s flush_barrier_ns=10000000 flush_full_ns=5000000 flush_dir_ns=0 files_materialized=10"
echo "sample sequence=1 arm=Rclone phase=initial elapsed_ms=80.0 workload_bytes=1000 transferred_content_bytes=unknown source_bytes_read=unknown power=ac load1=1.00 gated=true"
echo "verdict status=fail r23_initial_win=false"
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


class HarnessTests(unittest.TestCase):
    def run_main(self, tmp: Path, extra: list[str], ok: bool = True) -> int:
        bench = stub(tmp / "bench")
        info = {"rev": "r", "sha": "f" * 40, "binary": str(bench), "sha256": "0" * 64}
        good = {"utc": "t", "load1": 1.0, "power": "ac", "ok": ok}
        with (
            mock.patch.object(ab, "build", return_value=info),
            mock.patch.object(ab, "conditions", return_value=good),
            mock.patch.object(ab, "resolve_rclone", return_value=tmp / "rclone"),
            contextlib.redirect_stdout(io.StringIO()),
        ):
            return ab.main(
                ["--repo", str(tmp), "--work-root", str(tmp / "work"), *extra]
            )

    def test_dry_run_end_to_end_is_marked_not_a_gate_sample(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            tmp = Path(raw)
            self.assertEqual(self.run_main(tmp, ["--dry-run"]), 0)
            report = json.loads((tmp / "work" / "r23-ab.json").read_text())
            self.assertEqual(report["status"], "dry-run-complete-not-a-gate-sample")
            self.assertEqual(
                [r["label"] for r in report["reps"]], list("ABABA") + ["V4"]
            )
            self.assertAlmostEqual(report["dedup"]["b_dup_share"], 0.0)
            md = next((tmp / "work").glob("r23-dryrun-*.md")).read_text()
            self.assertIn(ab.NOT_GATE, md)

    def test_gated_refused_off_darwin_or_without_quiet(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            tmp = Path(raw)
            with mock.patch.object(ab.platform, "system", return_value="Linux"):
                self.assertEqual(self.run_main(tmp, ["--corpus", raw]), 2)
            with mock.patch.object(ab.platform, "system", return_value="Darwin"):
                self.assertEqual(self.run_main(tmp, ["--corpus", raw]), 2)
            self.assertFalse((tmp / "work").exists())

    def test_gated_aborts_when_conditions_fail_between_reps(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            tmp = Path(raw)
            corpus = tmp / "corpus"
            corpus.mkdir()
            (corpus / "f").write_bytes(b"x" * 7)
            states = iter([True, True, True, False])

            def flip() -> dict[str, object]:
                ok = next(states, False)
                return {
                    "utc": "t",
                    "load1": 1.0 if ok else 9.0,
                    "power": "ac",
                    "ok": ok,
                }

            bench = stub(tmp / "bench")
            info = {
                "rev": "r",
                "sha": "f" * 40,
                "binary": str(bench),
                "sha256": "0" * 64,
            }
            with (
                mock.patch.object(ab.platform, "system", return_value="Darwin"),
                mock.patch.object(ab, "build", return_value=info),
                mock.patch.object(ab, "conditions", side_effect=flip),
                mock.patch.object(ab, "resolve_rclone", return_value=tmp / "rclone"),
                contextlib.redirect_stdout(io.StringIO()),
            ):
                code = ab.main(
                    [
                        "--repo",
                        raw,
                        "--work-root",
                        str(tmp / "work"),
                        "--corpus",
                        str(corpus),
                        "--expect-files",
                        "1",
                        "--expect-bytes",
                        "7",
                        "--coordinator-quiet",
                        "--evidence",
                        str(tmp / "ev.md"),
                    ]
                )
            self.assertEqual(code, 3)
            report = json.loads((tmp / "work" / "r23-ab.json").read_text())
            self.assertEqual(report["status"], "aborted")
            self.assertEqual(len(report["reps"]), 1)
            self.assertIn("precondition failed", report["reason"])


if __name__ == "__main__":
    unittest.main()
