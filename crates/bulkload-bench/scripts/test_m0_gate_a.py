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
import itertools
import os
import re
import stat
import statistics
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
        text = self.run_gate([], host_ok=True, override=False)
        self.assertIn("preflight_gated=1", text.splitlines()[0])
        self.assertIn("all_rows_gated=1", text.splitlines()[-1])
        self.assertNotIn("ungated_override=1", text)

    def test_bad_host_without_override_refuses(self) -> None:
        with self.assertRaises(SystemExit):
            self.run_gate([], host_ok=False, override=False)
        self.assertFalse(self.work.exists())

    def test_reps_below_minimum_refuse_without_override(self) -> None:
        with self.assertRaises(SystemExit):
            self.run_gate(["4"], host_ok=True, override=False)
        self.assertFalse(self.work.exists())
        text = self.run_gate(["4"], host_ok=True, override=True)
        self.assertIn("min_reps_met=0", text.splitlines()[0])

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


# --- Round-3 review tests (#57 @ a6b19e1), ported with the weaknesses fixed.
# The reviewer's test_break_* versions passed while each weakness existed;
# these assert the fixed behaviour. The bench stub pops total_ms values from
# a queue file and can simulate the charger being pulled during an arm.

R3_BENCH_STUB = """#!/bin/sh
Q="$STUB_DIR/queue"; N="$STUB_DIR/calls"
case "$2" in
  residency) echo "micro name=residency source=x files=2 source_resident_fraction=0.5000" ;;
  durable-corpus)
    c=$(cat "$N" 2>/dev/null || echo 0); c=$((c+1)); echo $c > "$N"
    if [ -n "$UNPLUG_AT" ] && [ "$c" -ge "$UNPLUG_AT" ]; then : > "$STUB_DIR/battery"; fi
    v=$(head -n1 "$Q" 2>/dev/null); [ -z "$v" ] && v=5.000
    tail -n +2 "$Q" > "$Q.t" 2>/dev/null; mv "$Q.t" "$Q" 2>/dev/null
    echo "micro name=durable-corpus variant=stub rep=0 jobs=1 total_ms=$v total_timing=wall" ;;
  *) exit 3 ;;
esac
"""
R3_RCLONE_STUB = """#!/bin/sh
if [ -n "$UNPLUG_ON_RCLONE" ]; then : > "$STUB_DIR/battery"; fi
cp -R "$2" "$3"
"""


class RoundThreeTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.bench = executable(self.root / "bench", R3_BENCH_STUB)
        self.rclone = executable(self.root / "rclone", R3_RCLONE_STUB)
        self.corpus = self.root / "corpus"
        self.corpus.mkdir()
        (self.corpus / "a").write_bytes(b"a" * 300)
        (self.corpus / "b").write_bytes(b"b" * 5)
        self.work = self.root / "work"

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def conditions(self) -> tuple[float, str, bool]:
        if (self.root / "battery").exists():
            return (1.0, "battery", False)
        return (1.0, "ac", True)

    def run_gate(self, extra, env_extra=None, override=False) -> list[str]:
        env = {k: v for k, v in os.environ.items() if k != "M0_ALLOW_UNGATED"}
        env["STUB_DIR"] = str(self.root)
        if override:
            env["M0_ALLOW_UNGATED"] = "1"
        env.update(env_extra or {})
        out = io.StringIO()
        with (
            mock.patch.dict(os.environ, env, clear=True),
            mock.patch.object(gate, "conditions", side_effect=self.conditions),
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
        return out.getvalue().splitlines()

    def reset(self) -> None:
        self.tearDown()
        self.setUp()

    @staticmethod
    def samples(lines: list[str]) -> list[str]:
        return [line for line in lines if line.startswith("gate_a_sample")]

    def test_unplug_during_an_arm_ungates_that_arm(self) -> None:
        lines = self.run_gate(["1", "--warmup", "0"], override=True)
        self.assertIn("all_rows_gated=0", lines[-1])  # override: never gated
        self.reset()
        lines = self.run_gate([], {"UNPLUG_AT": "3"})
        rows = [row for row in self.samples(lines) if " warmup=0 " in row]
        first_bad = next(i for i, row in enumerate(rows) if " gated=0 " in row)
        self.assertIn("power_after=battery", rows[first_bad])
        self.assertIn(" power=ac ", rows[first_bad])
        self.assertIn("all_rows_gated=0", lines[-1])
        self.reset()
        lines = self.run_gate(["--floor-jobs4"], {"UNPLUG_ON_RCLONE": "1"})
        rows = self.samples(lines)
        shipped = next(row for row in rows if "arm=rclone-shipped " in row)
        self.assertIn(" gated=0 ", shipped)
        self.assertIn("all_rows_gated=0", lines[-1])

    def test_last_rclone_arm_unplug_is_detected(self) -> None:
        executable(
            self.rclone,
            """#!/bin/sh
N="$STUB_DIR/rcalls"; c=$(cat "$N" 2>/dev/null || echo 0); c=$((c+1)); echo $c > "$N"
if [ "$c" -eq 18 ]; then : > "$STUB_DIR/battery"; fi
cp -R "$2" "$3"
""",
        )
        # 8 reps x 2 rclone arms = 16 measured rclone calls, plus 2 in the
        # warm-up: the 18th call is the last rclone arm of the run. It is not
        # the run's final arm (see test_unplug_during_final_arm_of_final_rep).
        lines = self.run_gate([])
        rows = self.samples(lines)
        last_rclone = [row for row in rows if "arm=rclone" in row][-1]
        self.assertIn(" gated=0 ", last_rclone)
        self.assertIn("final_power=battery", lines[-1])
        self.assertIn("all_rows_gated=0", lines[-1])

    def test_power_lost_after_last_arm_fails_the_run(self) -> None:
        # 1 preflight check + 2 checks per arm (before, after) over
        # (1 warm-up + 8 reps) x 4 arms; the next call is the final check.
        budget = 1 + 2 * (1 + 8) * 4
        calls = {"n": 0}

        def conditions() -> tuple[float, str, bool]:
            calls["n"] += 1
            if calls["n"] > budget:
                return (1.0, "battery", False)
            return (1.0, "ac", True)

        env = {k: v for k, v in os.environ.items() if k != "M0_ALLOW_UNGATED"}
        env["STUB_DIR"] = str(self.root)
        out = io.StringIO()
        with (
            mock.patch.dict(os.environ, env, clear=True),
            mock.patch.object(gate, "conditions", side_effect=conditions),
            contextlib.redirect_stdout(out),
        ):
            gate.main(
                [str(self.bench), str(self.rclone), str(self.corpus), str(self.work)]
            )
        lines = out.getvalue().splitlines()
        self.assertEqual(calls["n"], budget + 1)
        self.assertTrue(
            all(
                " gated=1 " in row for row in self.samples(lines) if " warmup=0 " in row
            )
        )
        self.assertIn("final_power=battery", lines[-1])
        self.assertIn("all_rows_gated=0", lines[-1])

    def test_override_with_failed_preflight_never_gated(self) -> None:
        (self.root / "battery").touch()
        lines = self.run_gate(["4"], override=True)
        self.assertIn("preflight_gated=0", lines[0])
        for row in self.samples(lines):
            self.assertIn(" gated=0 ", row)
        self.assertIn("all_rows_gated=0", lines[-1])

    def test_override_values_other_than_1_do_not_override(self) -> None:
        (self.root / "battery").touch()
        with self.assertRaises(SystemExit):
            self.run_gate([], {"M0_ALLOW_UNGATED": "true"})

    def test_warmup_rows_print_gated_0(self) -> None:
        lines = self.run_gate([])
        warm = [row for row in self.samples(lines) if " warmup=1 " in row]
        self.assertTrue(warm)
        self.assertTrue(all(" gated=0 " in row for row in warm))
        measured = [row for row in self.samples(lines) if " warmup=0 " in row]
        self.assertTrue(all(" gated=1 " in row for row in measured))

    def test_summary_counts_and_uses_gated_rows_only(self) -> None:
        # Floor values: 2 warm-up calls, then 16 measured calls. The charger
        # is pulled on the 13th floor call, so later rows are gated=0 and
        # carry a huge value that must not reach any median.
        values = ["1.000", "1.000"] + ["10.000"] * 10 + ["99999.000"] * 6
        (self.root / "queue").write_text("\n".join(values) + "\n")
        lines = self.run_gate([], {"UNPLUG_AT": "13"})
        rows = self.samples(lines)
        self.assertTrue(any(" gated=0 " in row for row in rows))
        for line in lines:
            if line.startswith("gate_a_summary"):
                self.assertRegex(line, r"gated_reps=\d+ stats_over=gated")
                if "arm=floor" in line:
                    self.assertNotIn("99999", line)
        self.assertRegex(lines[-1], r"floor_gated_reps=\d+ rclone_gated_reps=\d+")
        self.assertIn("all_rows_gated=0", lines[-1])

    def test_reps_zero_is_refused_not_defaulted(self) -> None:
        with self.assertRaises(SystemExit):
            self.run_gate(["0", "--warmup", "0"], override=True)

    def test_small_reps_flagged(self) -> None:
        with self.assertRaises(SystemExit):
            self.run_gate(["4", "--warmup", "0"])
        self.reset()
        lines = self.run_gate(["4", "--warmup", "0"], override=True)
        self.assertIn("reps=4", lines[0])
        self.assertIn("min_reps_met=0", lines[0])
        self.assertIn("balance=within-row-only", lines[0])

    def test_warmup_excluded_and_even_median(self) -> None:
        values = ["99999.000", "99999.000"] + [
            f"{v:.3f}"
            for v in [10, 40, 20, 30, 11, 41, 21, 31, 12, 42, 22, 32, 13, 43, 23, 33]
        ]
        (self.root / "queue").write_text("\n".join(values) + "\n")
        lines = self.run_gate([])
        rows = [row for row in self.samples(lines) if " warmup=0 " in row]
        per_arm: dict[str, list[float]] = {}
        for row in rows:
            arm = re.search(r"arm=(\S+)", row).group(1)
            value = float(re.search(r"elapsed_ms=(\S+)", row).group(1))
            per_arm.setdefault(arm, []).append(value)
        for line in lines:
            if not line.startswith("gate_a_summary"):
                continue
            arm = re.search(r"arm=(\S+)", line).group(1)
            got = per_arm[arm]
            self.assertEqual(len(got), 8)
            self.assertNotIn(99999.0, got)
            for key, expect in (
                ("min_ms", min(got)),
                ("median_ms", statistics.median(got)),
                ("max_ms", max(got)),
            ):
                shown = float(re.search(rf"{key}=(\S+)", line).group(1))
                self.assertAlmostEqual(shown, expect, delta=0.0011)

    def test_residency_logged_for_every_arm_including_warmup(self) -> None:
        lines = self.run_gate(["--floor-jobs4"])
        rows = self.samples(lines)
        self.assertEqual(len(rows), 5 * 11)
        for row in rows:
            self.assertIn("source_resident_fraction_before=0.5000", row)

    def test_williams_within_row_balance_4_and_5(self) -> None:
        for n in (4, 5):
            rows = gate.williams(n)
            self.assertEqual(len(rows), n if n % 2 == 0 else 2 * n)
            k = len(rows) // n
            for col in range(n):
                counts = [sum(1 for r in rows if r[col] == a) for a in range(n)]
                self.assertEqual(counts, [k] * n, (n, col))
            pairs = dict.fromkeys(itertools.permutations(range(n), 2), 0)
            for r in rows:
                for a, b in zip(r, r[1:]):
                    pairs[(a, b)] += 1
            self.assertEqual(set(pairs.values()), {k}, (n, pairs))

    def test_stream_carryover_is_unbalanced_and_documented(self) -> None:
        # Not fixed in W2 (B3): pairs across row boundaries are unbalanced.
        # The docstring and header must say so; W3 may add a serially
        # balanced sequence or a washout arm.
        self.assertIn("across row", gate.__doc__)
        self.assertIn("washout", gate.__doc__)
        for flag in ([], ["--floor-jobs4"]):
            self.reset()
            lines = self.run_gate(flag)
            self.assertIn("balance=within-row-only", lines[0])
            seq = [re.search(r"arm=(\S+)", row).group(1) for row in self.samples(lines)]
            pairs: dict[tuple[str, str], int] = {}
            for a, b in zip(seq, seq[1:]):
                pairs[(a, b)] = pairs.get((a, b), 0) + 1
            self.assertGreater(len(set(pairs.values())), 1, pairs)


# --- Round-4 review tests (#57 @ 08fc8ad), ported. test_limit_* document
# residual limits of point sampling; they pass while the limit exists.


def gate_rows(lines: list[str]) -> list[str]:
    return [line for line in lines if line.startswith("gate_a_sample")]


class RoundFourTest(RoundThreeTest):
    """Round-4 cases; the inherited round-3 cases are not re-run here."""

    def test_unplug_during_final_arm_of_final_rep(self) -> None:
        # williams(4) row 3 = [3,0,2,1]: the last arm of rep 7 is
        # floor-barrier, the 16th durable-corpus call with no warm-up.
        lines = self.run_gate(["--warmup", "0"], {"UNPLUG_AT": "16"})
        rows = gate_rows(lines)
        self.assertEqual(len(rows), 32)
        self.assertTrue(rows[-1].startswith("gate_a_sample arm=floor-barrier "))
        self.assertIn(" power_after=battery gated=0 ", rows[-1])
        self.assertTrue(all(" gated=1 " in row for row in rows[:-1]))
        self.assertIn("final_power=battery", lines[-1])
        self.assertIn("all_rows_gated=0", lines[-1])
        summary = next(
            line
            for line in lines
            if line.startswith("gate_a_summary arm=floor-barrier")
        )
        self.assertIn("gated_reps=7 stats_over=gated", summary)

    def test_unplug_mid_run_then_replug(self) -> None:
        executable(
            self.bench,
            R3_BENCH_STUB.replace(
                'if [ -n "$UNPLUG_AT" ] && [ "$c" -ge "$UNPLUG_AT" ]; then '
                ': > "$STUB_DIR/battery"; fi',
                'if [ "$c" -eq 3 ]; then : > "$STUB_DIR/battery"; fi\n'
                '    if [ "$c" -eq 5 ]; then rm -f "$STUB_DIR/battery"; fi',
            ),
        )
        lines = self.run_gate(["--warmup", "0"])
        self.assertTrue(any(" gated=0 " in row for row in gate_rows(lines)))
        self.assertIn("final_power=ac", lines[-1])
        self.assertIn("all_rows_gated=0", lines[-1])

    def test_power_unknown_after_arm_is_not_gated(self) -> None:
        seq = {"n": 0}

        def conditions() -> tuple[float, str, bool]:
            seq["n"] += 1
            # 1 preflight, then (before, after) per arm: call 5 is arm 2's after.
            if seq["n"] == 5:
                return (1.0, "unknown", False)
            return (1.0, "ac", True)

        self.conditions = conditions
        lines = self.run_gate(["--warmup", "0"])
        self.assertIn(" power_after=unknown gated=0 ", gate_rows(lines)[1])
        self.assertIn("all_rows_gated=0", lines[-1])

    def test_limit_transient_mid_arm_unplug_is_invisible(self) -> None:
        # Documented limit: power is sampled just before and just after each
        # arm, so an unplug-and-replug inside one arm is not seen.
        executable(
            self.bench,
            R3_BENCH_STUB.replace(
                'echo "micro name=durable-corpus',
                ': > "$STUB_DIR/battery"; rm -f "$STUB_DIR/battery"\n'
                '    echo "micro name=durable-corpus',
            ),
        )
        lines = self.run_gate(["--warmup", "0"])
        self.assertIn("all_rows_gated=1", lines[-1])

    def test_limit_load_spike_during_arm_invisible(self) -> None:
        # Documented limit: load1 is a before-arm check only.
        seq = {"n": 0}

        def conditions() -> tuple[float, str, bool]:
            seq["n"] += 1
            if seq["n"] > 1 and seq["n"] % 2 == 1:
                return (9.0, "ac", False)
            return (1.0, "ac", True)

        self.conditions = conditions
        lines = self.run_gate(["--warmup", "0"])
        self.assertIn("all_rows_gated=1", lines[-1])

    def test_reps_below_min_with_override_header(self) -> None:
        lines = self.run_gate(["4", "--warmup", "0"], override=True)
        self.assertIn("min_reps_met=0", lines[0])
        self.assertIn("balanced=1 balance=within-row-only", lines[0])
        self.assertIn("all_rows_gated=0", lines[-1])

    def test_negative_warmup_and_reps_refused(self) -> None:
        with self.assertRaises(SystemExit):
            self.run_gate(["-3"])
        with self.assertRaises(SystemExit):
            self.run_gate(["--warmup", "-1"])


for _name in [n for n in dir(RoundThreeTest) if n.startswith("test_")]:
    if _name not in RoundFourTest.__dict__:
        setattr(RoundFourTest, _name, None)


if __name__ == "__main__":
    unittest.main()
