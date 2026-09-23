#!/usr/bin/env python3
"""Stub-binary tests for m0_gate_a.py (R-N87 review N1, N5, N6; TIN-4541).

Run: python3 -m unittest crates/bulkload-bench/scripts/test_m0_gate_a.py
Stubs stand in for bulkload-bench and rclone, the way the round-2 reviewer
reproduced the override bug. Host conditions are patched, not read.
"""

from __future__ import annotations

import contextlib
import importlib.util
import io
import os
import re
import stat
import tempfile
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("m0_gate_a", HERE / "m0_gate_a.py")
gate = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(gate)

BENCH_STUB = """#!/bin/sh
case "$2" in
  residency) echo "micro name=residency source=x files=2 source_resident_fraction=1.0000" ;;
  durable-corpus) echo "micro name=durable-corpus variant=stub rep=0 jobs=1 total_ms=5.000 total_timing=wall" ;;
  *) exit 3 ;;
esac
"""
RCLONE_STUB = """#!/bin/sh
cp -R "$2" "$3"
"""


def executable(path: Path, text: str) -> Path:
    path.write_text(text)
    path.chmod(path.stat().st_mode | stat.S_IXUSR)
    return path


class GateATest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        root = Path(self.tmp.name)
        self.bench = executable(root / "bench", BENCH_STUB)
        self.rclone = executable(root / "rclone", RCLONE_STUB)
        self.corpus = root / "corpus"
        self.corpus.mkdir()
        (self.corpus / "a").write_bytes(b"a" * 300)
        (self.corpus / "b").write_bytes(b"b" * 5)
        self.work = root / "work"

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def run_gate(self, extra: list[str], *, host_ok: bool, override: bool) -> str:
        env = dict(os.environ)
        env.pop("M0_ALLOW_UNGATED", None)
        if override:
            env["M0_ALLOW_UNGATED"] = "1"
        conditions = (1.0, "ac", True) if host_ok else (9.0, "battery", False)
        out = io.StringIO()
        with (
            mock.patch.dict(os.environ, env, clear=True),
            mock.patch.object(gate, "conditions", return_value=conditions),
            contextlib.redirect_stdout(out),
        ):
            gate.main(
                [
                    str(self.bench),
                    str(self.rclone),
                    str(self.corpus),
                    str(self.work),
                    *extra,
                ]
            )
        return out.getvalue()

    def test_override_rows_are_never_gated_even_on_a_good_host(self) -> None:
        text = self.run_gate(["4"], host_ok=True, override=True)
        lines = text.splitlines()
        self.assertTrue(lines)
        for line in lines:
            self.assertIn("ungated_override=1", line, line)
        samples = [line for line in lines if line.startswith("gate_a_sample")]
        self.assertTrue(samples)
        for line in samples:
            self.assertIn(" gated=0 ", line, line)
        self.assertIn("preflight_gated=0", lines[0])
        self.assertIn("all_rows_gated=0", lines[-1])

    def test_gated_host_without_override_is_gated(self) -> None:
        text = self.run_gate(["4"], host_ok=True, override=False)
        self.assertIn("preflight_gated=1", text.splitlines()[0])
        self.assertIn("all_rows_gated=1", text.splitlines()[-1])
        self.assertNotIn("ungated_override=1", text)

    def test_bad_host_without_override_refuses(self) -> None:
        with self.assertRaises(SystemExit):
            self.run_gate(["4"], host_ok=False, override=False)
        self.assertFalse(self.work.exists())

    def test_order_warmup_residency_jobs_and_caveat(self) -> None:
        text = self.run_gate(["--floor-jobs4"], host_ok=True, override=True)
        lines = text.splitlines()
        self.assertIn("reps=10", lines[0])
        self.assertIn("balanced=1", lines[0])
        samples = [line for line in lines if line.startswith("gate_a_sample")]
        warm = [line for line in samples if " warmup=1 " in line]
        measured = [line for line in samples if " warmup=0 " in line]
        self.assertEqual(len(warm), 5)
        self.assertEqual(len(measured), 50)
        for line in samples:
            self.assertRegex(line, r"source_resident_fraction_before=\d")
            self.assertRegex(line, r" jobs=(1|4) ")
        self.assertIn(
            " jobs=4 ", next(line for line in samples if "arm=floor-full-j4 " in line)
        )
        self.assertIn(
            " jobs=4 ", next(line for line in samples if "arm=rclone-shipped " in line)
        )
        for line in lines:
            if line.startswith("gate_a_summary"):
                self.assertIn("reps=10", line)
        self.assertIn("caveat=asymmetric-timing:", lines[-1])
        # Balance: each ordered pair of adjacent arms occurs equally often.
        pairs: dict[tuple[str, str], int] = {}
        for line in measured:
            if " order=0 " in line:
                seq = re.search(r"sequence=(\S+)", line).group(1).split(",")
                for left, right in zip(seq, seq[1:]):
                    pairs[(left, right)] = pairs.get((left, right), 0) + 1
        self.assertEqual(len(set(pairs.values())), 1, pairs)
        self.assertEqual(len(pairs), 5 * 4)

    def test_williams_even_design_is_balanced(self) -> None:
        rows = gate.williams(4)
        self.assertEqual(len(rows), 4)
        pairs = {(r[i], r[i + 1]) for r in rows for i in range(3)}
        self.assertEqual(len(pairs), 12)
        for column in range(4):
            self.assertEqual(sorted(r[column] for r in rows), [0, 1, 2, 3])


if __name__ == "__main__":
    unittest.main()
