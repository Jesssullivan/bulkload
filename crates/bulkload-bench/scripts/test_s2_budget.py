#!/usr/bin/env python3
"""Tests for s2_budget.py (OI-1003-Q34). Stdlib only.

Run: python3 -m unittest crates/bulkload-bench/scripts/test_s2_budget.py
(or `just bench-s2-budget selftest`). Synthetic traces drive every verdict
branch: pass, p95 fail, load1 fail, too few samples, the noise floor, the
A/A mode and the ON-run checks. One short live run (about 7 s) checks that
the real loops sample, that every ON run is waited for, and that both loops
have ended when `run` returns.
"""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import random
import tempfile
import threading
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("s2_budget", HERE / "s2_budget.py")
s2 = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(s2)

FIVE = ("OFF", "ON", "OFF", "ON", "OFF")


def per_window(value, index):
    return value[index] if isinstance(value, (list, tuple)) else value


def synth(
    pattern=FIVE,
    window=300.0,
    n=300,
    off_ms=10.0,
    on_factor=1.0,
    op_factor=None,
    off_load=1.0,
    on_add=0.0,
    aa=False,
    seen="background",
    exit_code=0,
    evidence=False,
    busy=1.0,
    wal_index=0,
    wal_created=0,
):
    """A deterministic trace: n evenly spaced samples per window. Latency
    cycles through 30 levels (1.00 to 1.97 x base), one cycle per 30 s
    bootstrap block at n = window, so every window of one base, and every
    bootstrap replicate, has the same p95 (1 + 28/30 x base) and the bounds
    collapse onto the point. An evidence trace follows the evidence
    protocol (settle 60 s, every window on AC)."""
    op_factor = op_factor or {}
    windows, latency, load = [], [], []
    t = 0.0
    for index, state in enumerate(pattern):
        on = state == "ON"
        base = per_window(off_ms, index) if not on else per_window(off_ms, 0)
        record = {
            "index": index,
            "state": state,
            "start": t,
            "end": t + window,
            "planned_end": t + window,
            "overrun_s": 0.0,
            "power_start": "ac" if evidence else "unknown-no-supply-class",
            "power_end": "ac" if evidence else "unknown-no-supply-class",
        }
        if on and not aa:
            record["runs"] = [
                {
                    "run": index,
                    "start": t,
                    "end": t + busy * window,
                    "exit": exit_code,
                    "priority_observed": [seen] if seen else [],
                    "source_wal_index_touched": wal_index,
                    "source_wal_created": wal_created,
                }
            ]
        windows.append(record)
        for k in range(n):
            ts = t + k * window / n
            spread = 1.0 + (k % 30) / 30.0
            step = 0.0
            for op in s2.OPS:
                factor = (
                    (per_window(on_factor, index) * op_factor.get(op, 1.0))
                    if on
                    else 1.0
                )
                ms = base / 4.0 * spread * factor
                step += ms
                latency.append({"t": ts, "op": op, "ms": ms, "ok": True})
            latency.append({"t": ts, "op": "step", "ms": step, "ok": True})
            value = per_window(off_load, 0 if on else index) + (on_add if on else 0.0)
            load.append({"t": ts, "load1": value})
        t += window
    return {
        "schema": s2.SCHEMA,
        "config": {
            "aa": aa,
            "priority": "background",
            "evidence": evidence,
            "window_seconds": window,
            "pattern": list(pattern),
            "repeat": True,
            "max_runs": s2.DEFAULT_MAX_RUNS,
            "coordinator_quiet": evidence,
        },
        "gate": {**s2.DEFAULT_GATE, **({} if evidence else {"settle_seconds": 0.0})},
        "windows": windows,
        "latency": latency,
        "load": load,
        "errors": [],
        "cut_short": None,
    }


def kernel_load(trace, add, base=1.0, phase=2.5):
    """Replace a synthetic trace's load1 rows with what the kernel reports
    for a run queue of `base` in OFF windows and `base + add` in ON windows:
    the 5 s EWMA (e = exp(-5/60)), rounded to two places as printed."""
    windows = trace["windows"]
    starts = [w["start"] for w in windows]
    level, update = base, phase
    for row in trace["load"]:
        while update <= row["t"]:
            index = max(
                0, min(len(windows) - 1, sum(st <= update for st in starts) - 1)
            )
            n = base + (add if windows[index]["state"] == "ON" else 0.0)
            level = level * s2.LOAD1_DECAY + n * (1.0 - s2.LOAD1_DECAY)
            update += s2.LOAD1_UPDATE_S
        row["load1"] = round(level, 2)
    return trace


def reasons(verdict) -> str:
    return " | ".join(verdict["reasons"])


class StatisticsTests(unittest.TestCase):
    def test_p95_is_nearest_rank(self) -> None:
        self.assertEqual(s2.p95(list(range(1, 101))), 95)
        self.assertEqual(s2.p95(list(range(1, 21))), 19)
        self.assertEqual(s2.p95([7.0]), 7.0)
        self.assertIsNone(s2.p95([]))

    def test_synthetic_windows_share_one_p95(self) -> None:
        verdict = s2.analyze(synth())
        p95s = {w["p95_ms"]["step"] for w in verdict["windows"]}
        self.assertEqual(p95s, {19.3333})
        self.assertEqual(verdict["noise_p95"], 0.0)
        self.assertEqual(verdict["noise_load1"], 0.0)


class VerdictTests(unittest.TestCase):
    def test_pass(self) -> None:
        verdict = s2.analyze(synth(on_factor=1.10, on_add=0.5))
        self.assertEqual(verdict["status"], "PASS", reasons(verdict))
        self.assertAlmostEqual(verdict["delta_p95"], 0.10, places=6)
        self.assertAlmostEqual(verdict["delta_load1"], 0.5, places=6)
        self.assertEqual(verdict["reasons"], [])
        self.assertFalse(verdict["evidence"])

    def test_p95_fail(self) -> None:
        verdict = s2.analyze(synth(on_factor=1.5))
        self.assertEqual(verdict["status"], "FAIL")
        self.assertAlmostEqual(verdict["delta_p95"], 0.5, places=6)
        self.assertIn("p95 step +50.0% exceeds +25%", reasons(verdict))

    def test_load1_fail(self) -> None:
        verdict = s2.analyze(synth(on_add=2.5))
        self.assertEqual(verdict["status"], "FAIL")
        self.assertIn("load1 +2.50 exceeds +2.0", reasons(verdict))
        self.assertNotIn("p95", reasons(verdict))

    def test_budget_bounds_are_inclusive(self) -> None:
        verdict = s2.analyze(synth(on_factor=1.25, on_add=2.0))
        self.assertEqual(verdict["status"], "PASS", reasons(verdict))

    def test_too_few_samples_in_a_window(self) -> None:
        verdict = s2.analyze(synth(n=10, on_factor=3.0))
        self.assertEqual(verdict["status"], "INCONCLUSIVE")
        self.assertIn(
            "too few samples: window 0 (OFF) has 10 latency", reasons(verdict)
        )
        self.assertEqual(verdict["raw_comparison"], "FAIL")

    def test_too_few_samples_pooled(self) -> None:
        verdict = s2.analyze(synth(n=30))
        self.assertEqual(verdict["status"], "INCONCLUSIVE")
        self.assertIn("too few samples: pooled ON has 60 latency", reasons(verdict))
        self.assertNotIn("window", reasons(verdict))

    def test_too_few_windows(self) -> None:
        verdict = s2.analyze(synth(pattern=("OFF", "ON")))
        self.assertEqual(verdict["status"], "INCONCLUSIVE")
        self.assertIn("too few windows: 1 OFF and 1 ON", reasons(verdict))

    def test_latency_noise_floor_is_inconclusive(self) -> None:
        verdict = s2.analyze(synth(off_ms=[10.0, 10.0, 13.0, 10.0, 10.0]))
        self.assertEqual(verdict["status"], "INCONCLUSIVE")
        self.assertIn("noise floor: OFF-window p95 spread", reasons(verdict))
        self.assertGreater(verdict["noise_p95"], 0.125)
        self.assertEqual(verdict["raw_comparison"], "PASS")

    def test_load1_noise_floor_is_inconclusive(self) -> None:
        verdict = s2.analyze(synth(off_load=[1.0, 1.0, 2.5, 1.0, 1.0]))
        self.assertEqual(verdict["status"], "INCONCLUSIVE")
        self.assertIn("OFF-window load1 spread 1.50", reasons(verdict))

    def test_noise_within_half_the_budget_still_decides(self) -> None:
        verdict = s2.analyze(synth(off_load=[1.0, 1.0, 1.9, 1.0, 1.0], on_add=0.5))
        self.assertEqual(verdict["status"], "PASS", reasons(verdict))
        self.assertAlmostEqual(verdict["noise_load1"], 0.9, places=6)

    def test_noise_floor_outranks_a_fail(self) -> None:
        trace = synth(off_ms=[10.0, 10.0, 13.0, 10.0, 10.0], on_factor=2.0)
        verdict = s2.analyze(trace)
        self.assertEqual(verdict["status"], "INCONCLUSIVE")
        self.assertEqual(verdict["raw_comparison"], "FAIL")

    def test_each_op_gate_catches_one_slow_operation(self) -> None:
        trace = synth(op_factor={"jsonl": 1.6})
        by_step = s2.analyze(trace, {"gate_metric": "step"})
        self.assertEqual(by_step["status"], "PASS", reasons(by_step))
        self.assertAlmostEqual(by_step["delta_p95"], 0.15, places=6)
        by_op = s2.analyze(trace, {"gate_metric": "each-op"})
        self.assertEqual(by_op["status"], "FAIL")
        self.assertEqual(by_op["worst_metric"], "jsonl")
        self.assertAlmostEqual(by_op["delta_p95"], 0.6, places=6)
        self.assertFalse(by_op["evidence"])

    def test_the_default_gate_never_passes_a_hidden_operation(self) -> None:
        """`step` alone would PASS jsonl +60 % behind step +15 %; the default
        computes both gates and reports the disagreement, unruled."""
        verdict = s2.analyze(synth(evidence=True, op_factor={"jsonl": 1.6}))
        self.assertEqual(verdict["gate_metric"], "both")
        self.assertEqual(verdict["status"], "INCONCLUSIVE")
        self.assertEqual(verdict["gates"]["step"], "PASS")
        self.assertEqual(verdict["gates"]["each-op"], "FAIL")
        self.assertIn(
            "the latency gates disagree (step PASS, each-op FAIL)", reasons(verdict)
        )
        self.assertTrue(verdict["evidence"], verdict["evidence_problems"])
        self.assertEqual(verdict["worst_metric"], "jsonl")
        self.assertEqual(verdict["raw_comparison"], "FAIL")

    def test_the_default_gate_fails_when_both_gates_fail(self) -> None:
        verdict = s2.analyze(synth(evidence=True, op_factor={"git": 2.5}))
        self.assertEqual(verdict["gates"]["step"], "FAIL")
        self.assertEqual(verdict["gates"]["each-op"], "FAIL")
        self.assertEqual(verdict["status"], "FAIL")
        self.assertIn("p95 git +150.0% exceeds +25%", reasons(verdict))

    def test_settle_drops_the_switch_transient(self) -> None:
        trace = synth()
        for window in trace["windows"]:
            if window["state"] == "ON":
                for row in trace["latency"]:
                    if window["start"] <= row["t"] < window["start"] + 20.0:
                        row["ms"] *= 5.0
        unsettled = s2.analyze(trace)
        self.assertEqual(unsettled["raw_comparison"], "FAIL")
        self.assertNotEqual(unsettled["status"], "PASS")
        settled = s2.analyze(trace, {"settle_seconds": 25.0})
        self.assertEqual(settled["status"], "PASS", reasons(settled))

    def test_workload_errors_are_inconclusive(self) -> None:
        trace = synth()
        trace["latency"].append({"t": 5.0, "op": "git", "ms": 1.0, "ok": False})
        verdict = s2.analyze(trace)
        self.assertEqual(verdict["status"], "INCONCLUSIVE")
        self.assertIn("workload errors: 1 failed operations", reasons(verdict))

    def test_a_cut_short_run_is_inconclusive(self) -> None:
        trace = synth()
        trace["cut_short"] = "ON run 3 failed"
        self.assertIn("cut short: ON run 3 failed", reasons(s2.analyze(trace)))


class Load1LagTests(unittest.TestCase):
    """load1 lags each switch (a 60 s EWMA updated every 5 s). The plain
    pooled mean read a steady add X as about 0.856 X at 300 s windows and a
    60 s settle, so a true +2.3 PASSed; the lag-corrected level does not."""

    def test_a_true_add_over_the_budget_fails(self) -> None:
        verdict = s2.analyze(kernel_load(synth(evidence=True), 2.3))
        self.assertEqual(verdict["status"], "FAIL", reasons(verdict))
        self.assertTrue(verdict["evidence"], verdict["evidence_problems"])
        self.assertAlmostEqual(verdict["delta_load1"], 2.3, delta=0.03)
        self.assertAlmostEqual(verdict["delta_load1_plain"], 1.96, delta=0.03)
        self.assertRegex(reasons(verdict), r"load1 \+2\.(29|30|31) exceeds \+2\.0")

    def test_the_update_phase_moves_the_level_by_under_one_percent(self) -> None:
        for phase in (0.01, 1.0, 2.5, 4.0, 4.99):
            trace = kernel_load(synth(evidence=True), 2.3, phase=phase)
            verdict = s2.analyze(trace)
            self.assertAlmostEqual(verdict["delta_load1"], 2.3, delta=0.02, msg=phase)

    def test_a_true_add_under_the_budget_passes(self) -> None:
        verdict = s2.analyze(kernel_load(synth(evidence=True), 1.7))
        self.assertEqual(verdict["status"], "PASS", reasons(verdict))
        self.assertAlmostEqual(verdict["delta_load1"], 1.7, delta=0.03)

    def test_the_level_does_not_depend_on_the_settle(self) -> None:
        trace = kernel_load(synth(evidence=True), 2.3)
        for settle, tolerance in ((0.0, 0.06), (30.0, 0.03), (120.0, 0.03)):
            verdict = s2.analyze(trace, {"settle_seconds": settle})
            self.assertAlmostEqual(verdict["delta_load1"], 2.3, delta=tolerance)
            self.assertEqual(verdict["raw_comparison"], "FAIL", settle)

    def test_the_trace_records_the_plain_attenuation(self) -> None:
        verdict = s2.analyze(kernel_load(synth(evidence=True), 1.0))
        model = verdict["load1_model"]
        self.assertAlmostEqual(
            model["response_to_unit_add"]["plain"], 0.856, delta=0.01
        )
        self.assertAlmostEqual(
            model["response_to_unit_add"]["corrected"], 1.0, delta=0.01
        )
        self.assertAlmostEqual(model["tau_s"], 59.53, delta=0.01)

    def test_the_off_tail_is_not_host_noise(self) -> None:
        verdict = s2.analyze(kernel_load(synth(evidence=True), 2.0))
        self.assertLess(verdict["noise_load1"], 0.05)
        levels = [w["load1_level"] for w in verdict["windows"] if w["state"] == "OFF"]
        means = [w["load1_mean"] for w in verdict["windows"] if w["state"] == "OFF"]
        self.assertGreater(max(means) - min(means), 0.15)
        self.assertLess(max(levels) - min(levels), 0.05)


class CoordinatedOmissionTests(unittest.TestCase):
    def test_stalls_that_skip_ticks_are_back_filled(self) -> None:
        trace = stall(synth(evidence=True))
        verdict = s2.analyze(trace)
        on = [w for w in verdict["windows"] if w["state"] == "ON"]
        self.assertTrue(all(w["missed"] > 0 for w in on))
        self.assertTrue(all(w["n"]["step"] >= 20 for w in on))
        self.assertEqual(verdict["samples"]["OFF"]["missed_ticks"], 0)
        self.assertGreater(verdict["samples"]["ON"]["missed_ticks"], 30)
        self.assertEqual(verdict["status"], "FAIL", reasons(verdict))
        self.assertGreater(verdict["delta_p95"], 1.0)
        self.assertTrue(verdict["evidence"], verdict["evidence_problems"])

    def test_without_the_back_fill_the_same_stalls_hide(self) -> None:
        trace = stall(synth(evidence=True))
        trace["missed_ticks"] = {}
        verdict = s2.analyze(trace)
        self.assertEqual(verdict["raw_comparison"], "PASS", reasons(verdict))
        self.assertLess(verdict["delta_p95"], 0.05)

    def test_missed_ticks_in_the_settle_are_dropped(self) -> None:
        trace = synth(evidence=True)
        start = trace["windows"][1]["start"]
        trace["missed_ticks"] = {"workload": [{"t": start + 5.0, "lag_ms": 9e3}]}
        verdict = s2.analyze(trace)
        self.assertEqual(verdict["windows"][1]["missed"], 0)
        self.assertEqual(verdict["status"], "PASS", reasons(verdict))


class OnRunTests(unittest.TestCase):
    def test_a_failed_on_run_is_inconclusive(self) -> None:
        verdict = s2.analyze(synth(exit_code=3))
        self.assertEqual(verdict["status"], "INCONCLUSIVE")
        self.assertIn("ON run 1 failed: exit 3", reasons(verdict))

    def test_another_priority_class_is_inconclusive(self) -> None:
        verdict = s2.analyze(synth(seen="normal"))
        self.assertEqual(verdict["status"], "INCONCLUSIVE")
        self.assertIn("reported priority normal, recorded background", reasons(verdict))

    def test_evidence_needs_a_reported_priority(self) -> None:
        self.assertEqual(s2.analyze(synth(seen=None))["status"], "PASS")
        verdict = s2.analyze(synth(seen=None, evidence=True))
        self.assertEqual(verdict["status"], "INCONCLUSIVE")
        self.assertIn("did not report its priority class", reasons(verdict))

    def test_the_wal_index_counter_is_recorded(self) -> None:
        verdict = s2.analyze(synth(evidence=True, wal_index=2))
        self.assertTrue(verdict["evidence"], verdict["evidence_problems"])
        self.assertEqual(
            verdict["source_wal_index_touched"],
            {"total": 4, "runs_reported": 2, "runs_unreported": 0},
        )
        self.assertEqual(verdict["status"], "PASS")

    def test_evidence_needs_a_reported_wal_index_counter(self) -> None:
        self.assertEqual(s2.analyze(synth(wal_index=None))["status"], "PASS")
        verdict = s2.analyze(synth(wal_index=None, evidence=True))
        self.assertEqual(verdict["status"], "INCONCLUSIVE")
        self.assertIn(
            "did not report source_wal_index_touched (OI-1003-Q36)", reasons(verdict)
        )
        self.assertEqual(
            verdict["source_wal_index_touched"],
            {"total": 0, "runs_reported": 0, "runs_unreported": 2},
        )

    def test_the_wal_index_counter_is_summed_over_counters_lines(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            log = Path(tmp) / "r0001.log"
            log.write_text(
                "counters verb=snapshot priority=background "
                "source_wal_index_touched=1 census_walks=0\n"
                "snapshot complete\n"
                "note source_wal_index_touched=9\n"
                "counters verb=compose priority=background source_wal_index_touched=2\n"
            )
            self.assertEqual(s2.wal_index_in(log), 3)
            log.write_text("counters verb=copy priority=background census_walks=0\n")
            self.assertIsNone(s2.wal_index_in(log))
            self.assertIsNone(s2.wal_index_in(Path(tmp) / "absent.log"))

    def test_the_wal_created_counter_is_recorded(self) -> None:
        verdict = s2.analyze(synth(evidence=True, wal_index=2, wal_created=1))
        self.assertTrue(verdict["evidence"], verdict["evidence_problems"])
        self.assertEqual(
            verdict["source_wal_created"],
            {"total": 2, "runs_reported": 2, "runs_unreported": 0},
        )
        self.assertEqual(
            verdict["source_wal_index_touched"],
            {"total": 4, "runs_reported": 2, "runs_unreported": 0},
        )
        self.assertEqual(verdict["status"], "PASS")

    def test_evidence_needs_a_reported_wal_created_counter(self) -> None:
        self.assertEqual(s2.analyze(synth(wal_created=None))["status"], "PASS")
        verdict = s2.analyze(synth(wal_created=None, evidence=True))
        self.assertEqual(verdict["status"], "INCONCLUSIVE")
        self.assertIn(
            "did not report source_wal_created (OI-1003-Q72)", reasons(verdict)
        )
        self.assertNotIn("source_wal_index_touched", reasons(verdict))
        self.assertEqual(
            verdict["source_wal_created"],
            {"total": 0, "runs_reported": 0, "runs_unreported": 2},
        )

    def test_the_wal_created_counter_is_summed_over_counters_lines(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            log = Path(tmp) / "r0001.log"
            log.write_text(
                "counters verb=snapshot priority=background "
                "source_wal_index_touched=1 source_wal_created=1\n"
                "note source_wal_created=9\n"
                "counters verb=compose source_wal_index_touched=4 "
                "source_wal_created=2 blake3_total_bytes=0\n"
            )
            self.assertEqual(s2.wal_created_in(log), 3)
            self.assertEqual(s2.wal_index_in(log), 5)
            # A build with the Q36 counter only: Q72's is unreported.
            log.write_text("counters verb=snapshot source_wal_index_touched=1\n")
            self.assertEqual(s2.wal_index_in(log), 1)
            self.assertIsNone(s2.wal_created_in(log))
            self.assertIsNone(s2.wal_created_in(Path(tmp) / "absent.log"))

    def test_an_idle_on_window_is_inconclusive(self) -> None:
        verdict = s2.analyze(synth(busy=0.5))
        self.assertEqual(verdict["status"], "INCONCLUSIVE")
        self.assertIn("ON window 1 was busy 50% of the time", reasons(verdict))

    def test_evidence_only_with_the_slo_gate(self) -> None:
        verdict = s2.analyze(synth(evidence=True))
        self.assertTrue(verdict["evidence"], verdict["evidence_problems"])
        self.assertEqual(verdict["evidence_problems"], [])
        loose = s2.analyze(synth(evidence=True), {"budget_p95": 0.5})
        self.assertFalse(loose["evidence"])

    def test_a_run_cap_under_evidence_is_inconclusive(self) -> None:
        trace = synth(evidence=True)
        trace["windows"][1]["run_cap_reached"] = True
        verdict = s2.analyze(trace)
        self.assertEqual(verdict["status"], "INCONCLUSIVE")
        self.assertIn("ON window 1 reached the run cap", reasons(verdict))


class PowerAndBaselineTests(unittest.TestCase):
    """R-N81 under evidence: AC power at every window boundary, and every
    OFF window's load1 level under the limit; checked, not just recorded."""

    def test_battery_mid_run_is_inconclusive_and_not_evidence(self) -> None:
        trace = synth(evidence=True)
        trace["windows"][2]["power_start"] = "battery"
        trace["windows"][3]["power_end"] = "battery"
        verdict = s2.analyze(trace)
        self.assertEqual(verdict["status"], "INCONCLUSIVE")
        self.assertFalse(verdict["evidence"])
        self.assertIn("R-N81: window 2 power start was battery", reasons(verdict))
        self.assertIn("R-N81: window 3 power end was battery", reasons(verdict))
        self.assertIn(
            "R-N81: window 2 power start was battery",
            " | ".join(verdict["evidence_problems"]),
        )

    def test_every_window_ending_on_battery_is_inconclusive(self) -> None:
        trace = synth(evidence=True)
        for window in trace["windows"]:
            window["power_end"] = "battery"
        verdict = s2.analyze(trace)
        self.assertEqual(verdict["status"], "INCONCLUSIVE")
        self.assertFalse(verdict["evidence"])

    def test_an_unrecorded_power_state_is_not_ac(self) -> None:
        trace = synth(evidence=True)
        del trace["windows"][4]["power_end"]
        verdict = s2.analyze(trace)
        self.assertIn("window 4 power end was unrecorded", reasons(verdict))

    def test_a_busy_off_baseline_is_inconclusive(self) -> None:
        busy = s2.analyze(synth(evidence=True, off_load=[1.0, 1.0, 2.6, 1.0, 1.0]))
        self.assertEqual(busy["status"], "INCONCLUSIVE")
        self.assertIn("R-N81: OFF window 2 load1 level 2.60", reasons(busy))
        self.assertFalse(busy["evidence"])
        quiet = s2.analyze(synth(evidence=True, off_load=2.4))
        self.assertEqual(quiet["status"], "PASS", reasons(quiet))
        self.assertTrue(quiet["evidence"], quiet["evidence_problems"])

    def test_the_off_tail_does_not_trip_the_baseline(self) -> None:
        trace = kernel_load(synth(evidence=True), 1.9, base=2.4)
        verdict = s2.analyze(trace)
        self.assertGreaterEqual(verdict["windows"][2]["load1_mean"], 2.5)
        self.assertNotIn("R-N81", reasons(verdict))
        self.assertEqual(verdict["status"], "PASS", reasons(verdict))

    def test_power_is_not_checked_without_evidence(self) -> None:
        trace = synth()
        trace["windows"][1]["power_end"] = "battery"
        self.assertEqual(s2.analyze(trace)["status"], "PASS")


class EvidenceProtocolTests(unittest.TestCase):
    """The evidence label is tied to the whole measurement protocol, not
    just the budgets: no analyze override and no relaxed recorded gate."""

    def test_every_analyze_override_that_differs_drops_the_label(self) -> None:
        trace = synth(evidence=True, on_factor=1.1)
        for key, value in (
            ("settle_seconds", 99.0),
            ("settle_seconds", 0.0),
            ("min_on_busy", 0.0),
            ("min_samples", 1),
            ("min_window_samples", 1),
            ("min_samples", 500),
            ("budget_load1", 3.0),
        ):
            verdict = s2.analyze(trace, {key: value})
            self.assertFalse(verdict["evidence"], (key, value))
            self.assertIn(
                f"analyze override {key}=", " | ".join(verdict["evidence_problems"])
            )
        same = s2.analyze(trace, {"settle_seconds": 60.0, "min_on_busy": 0.8})
        self.assertTrue(same["evidence"], same["evidence_problems"])

    def test_an_idle_evidence_trace_stays_inconclusive_or_loses_the_label(
        self,
    ) -> None:
        trace = synth(evidence=True, busy=0.05)
        verdict = s2.analyze(trace)
        self.assertEqual(verdict["status"], "INCONCLUSIVE")
        self.assertTrue(verdict["evidence"])
        relaxed = s2.analyze(trace, {"min_on_busy": 0.0, "settle_seconds": 99.0})
        self.assertEqual(relaxed["status"], "PASS")
        self.assertFalse(relaxed["evidence"])
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "trace.json"
            path.write_text(json.dumps(trace))
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                s2.main(
                    ["analyze", str(path), "--min-on-busy", "0"]
                    + ["--settle-seconds", "99"]
                )
        self.assertIn("evidence=false", out.getvalue())
        self.assertIn("NOT EVIDENCE", out.getvalue())

    def test_a_relaxed_recorded_protocol_is_not_evidence(self) -> None:
        for change in (
            {"gate": {"settle_seconds": 0.0}},
            {"gate": {"settle_seconds": 120.0}},
            {"gate": {"min_on_busy": 0.0}},
            {"gate": {"min_samples": 1}},
            {"gate": {"min_window_samples": 1}},
            {"config": {"repeat": False}},
            {"config": {"max_runs": 1}},
            {"config": {"coordinator_quiet": False}},
            {"config": {"window_seconds": 60.0}},
            {"schema": "bulkload.s2-budget.v-old"},
        ):
            trace = synth(evidence=True)
            for part, values in change.items():
                if isinstance(values, dict):
                    trace[part].update(values)
                else:
                    trace[part] = values
            verdict = s2.analyze(trace)
            self.assertFalse(verdict["evidence"], change)
            self.assertTrue(verdict["evidence_problems"], change)

    def test_stricter_floors_keep_the_label(self) -> None:
        trace = synth(evidence=True)
        trace["gate"].update({"min_samples": 200, "min_on_busy": 0.9})
        verdict = s2.analyze(trace)
        self.assertTrue(verdict["evidence"], verdict["evidence_problems"])


class AaTests(unittest.TestCase):
    def test_quiet(self) -> None:
        verdict = s2.analyze(synth(aa=True, on_factor=1.02, on_add=0.1))
        self.assertEqual((verdict["mode"], verdict["status"]), ("aa", "QUIET"))
        self.assertFalse(verdict["evidence"])

    def test_noisy(self) -> None:
        verdict = s2.analyze(synth(aa=True, on_factor=1.2))
        self.assertEqual(verdict["status"], "NOISY")
        self.assertIn("A/A p95 step +20.0% beyond half the budget", reasons(verdict))

    def test_noisy_on_the_noise_floor(self) -> None:
        verdict = s2.analyze(synth(aa=True, off_load=[1.0, 1.0, 2.5, 1.0, 1.0]))
        self.assertEqual(verdict["status"], "NOISY")

    def test_too_few_samples(self) -> None:
        self.assertEqual(s2.analyze(synth(aa=True, n=5))["status"], "INCONCLUSIVE")


def lognormal_trace(on_factor, sigma, seed, evidence=True):
    """An evidence-shaped trace whose latencies are iid lognormal (no host
    drift): each op's ms = base x lognormal(0, sigma), x on_factor in ON."""
    rng = random.Random(seed)
    trace = synth(evidence=evidence)
    for row in trace["latency"]:
        if row["op"] != "step":
            row["ms"] = 2.5 * rng.lognormvariate(0.0, sigma)
    starts = {w["index"]: w for w in trace["windows"]}
    steps = {}
    for row in trace["latency"]:
        window = starts[int(row["t"] // 300)]
        if row["op"] != "step" and window["state"] == "ON":
            row["ms"] *= per_window(on_factor, window["index"])
        if row["op"] != "step":
            steps[row["t"]] = steps.get(row["t"], 0.0) + row["ms"]
    for row in trace["latency"]:
        if row["op"] == "step":
            row["ms"] = steps[row["t"]]
    return trace


class UncertaintyTests(unittest.TestCase):
    """PASS and FAIL come from bootstrap bounds, not a point estimate: whole
    windows are resampled within each state, then 30 s blocks within each
    drawn window."""

    def test_on_windows_that_disagree_are_inconclusive(self) -> None:
        trace = synth(evidence=True, on_factor=[1.0, 1.10, 1.0, 1.40, 1.0])
        verdict = s2.analyze(trace)
        self.assertEqual(verdict["status"], "INCONCLUSIVE", reasons(verdict))
        low, high = verdict["delta_p95_bounds"]["step"]
        self.assertAlmostEqual(low, 0.10, places=3)
        self.assertAlmostEqual(high, 0.40, places=3)
        self.assertIn("straddles +25%", reasons(verdict))
        agree = s2.analyze(synth(evidence=True, on_factor=[1.0, 1.10, 1.0, 1.15, 1.0]))
        self.assertEqual(agree["status"], "PASS", reasons(agree))

    def test_bounds_collapse_on_a_noiseless_trace(self) -> None:
        verdict = s2.analyze(synth(evidence=True, on_factor=1.2, on_add=1.0))
        self.assertEqual(verdict["delta_p95_bounds"]["step"], [0.2, 0.2])
        self.assertEqual(verdict["delta_load1_bounds"], [1.0, 1.0])
        self.assertEqual(verdict["status"], "PASS")

    def test_a_clear_regression_fails_through_the_noise(self) -> None:
        verdict = s2.analyze(lognormal_trace(1.6, 0.3, seed=2))
        self.assertEqual(verdict["status"], "FAIL", reasons(verdict))
        self.assertGreater(verdict["delta_p95_bounds"]["step"][0], 0.25)

    def test_a_quiet_host_with_no_effect_passes(self) -> None:
        verdict = s2.analyze(lognormal_trace(1.0, 0.2, seed=2))
        self.assertEqual(verdict["status"], "PASS", reasons(verdict))
        self.assertLess(verdict["delta_p95_bounds"]["step"][1], 0.25)

    def test_an_effect_near_the_budget_is_not_decided_by_its_point(self) -> None:
        verdict = s2.analyze(lognormal_trace(1.25, 0.3, seed=3))
        low, high = verdict["delta_p95_bounds"]["step"]
        self.assertLess(low, 0.25)
        self.assertGreater(high, 0.25)
        self.assertNotEqual(verdict["status"], "PASS")

    def test_the_bootstrap_is_seeded(self) -> None:
        trace = lognormal_trace(1.1, 0.3, seed=4)
        self.assertEqual(
            s2.analyze(trace)["delta_p95_bounds"], s2.analyze(trace)["delta_p95_bounds"]
        )

    def test_the_kept_values_shortcut_matches_a_full_sort(self) -> None:
        rng = random.Random(7)
        samples = []
        for index, state in enumerate(FIVE):
            blocks = []
            for _ in range(rng.randint(1, 9)):
                size = rng.randint(1, 30)
                blocks.append(
                    {
                        m: sorted(rng.lognormvariate(0, 0.5) for _ in range(size))
                        for m in s2.METRICS
                    }
                )
            samples.append(
                {"state": state, "blocks": blocks, "level": float(index), "load_n": 9}
            )

        def reference() -> dict:
            pools = {
                st: [w for w in samples if w["state"] == st] for st in ("OFF", "ON")
            }
            draw = random.Random(s2.BOOTSTRAP_SEED).random
            out = {m: [] for m in (*s2.METRICS, "load1")}
            for _ in range(s2.BOOTSTRAP_REPLICATES):
                q, lv = {}, {}
                for st in ("OFF", "ON"):
                    pool, picks, weight, level = pools[st], [], 0.0, 0.0
                    for _ in pool:
                        w = pool[int(draw() * len(pool))]
                        count = len(w["blocks"])
                        picks.extend(
                            w["blocks"][int(draw() * count)] for _ in range(count)
                        )
                        weight += w["load_n"]
                        level += w["load_n"] * w["level"]
                    for m in s2.METRICS:
                        q[st, m] = s2.p95([v for block in picks for v in block[m]])
                    lv[st] = level / weight
                for m in s2.METRICS:
                    out[m].append(q["ON", m] / q["OFF", m] - 1.0)
                out["load1"].append(lv["ON"] - lv["OFF"])
            return {k: sorted(v) for k, v in out.items()}

        with mock.patch.object(s2, "BOOTSTRAP_REPLICATES", 60):
            self.assertEqual(s2.bootstrap(samples), reference())


class CadenceTests(unittest.TestCase):
    def test_an_overrunning_tick_skips_the_ticks_it_missed(self) -> None:
        clock = {"now": 0.0}

        class Stop:
            def is_set(self) -> bool:
                return False

            def wait(self, seconds: float) -> bool:
                clock["now"] += seconds
                return False

        durations = iter([0.2, 2.5, 0.1, 0.1, 0.1])
        calls = []

        def body() -> None:
            calls.append(clock["now"])
            clock["now"] += next(durations)

        missed = []
        with mock.patch.object(s2.time, "monotonic", lambda: clock["now"]):
            skipped = s2.cadence(0.0, 0.0, Stop(), 6.0, body, missed)
        self.assertEqual(skipped, 2)
        self.assertEqual([round(c, 6) for c in calls], [0.0, 1.0, 4.0, 5.0])
        self.assertEqual(missed, [(2.0, 1500.0), (3.0, 500.0)])

    def test_ticks_past_the_bound_are_not_missed(self) -> None:
        clock = {"now": 0.0}

        class Stop:
            def is_set(self) -> bool:
                return False

            def wait(self, seconds: float) -> bool:
                clock["now"] += seconds
                return False

        def body() -> None:
            clock["now"] += 4.5

        missed = []
        with mock.patch.object(s2.time, "monotonic", lambda: clock["now"]):
            skipped = s2.cadence(0.0, 0.0, Stop(), 3.0, body, missed)
        self.assertEqual(skipped, 2)
        self.assertEqual([t for t, _ in missed], [1.0, 2.0])


def stall(trace, every=27, stall_ms=6000.0):
    """Make each ON window's workload stall: one step in `every` takes
    `stall_ms` and the ticks it overran are missed (recorded as the loop
    records them), as an open-loop agent's steps would have been delayed."""
    gone, lags = set(), []
    skip = int(stall_ms // 1000)
    for window in trace["windows"]:
        if window["state"] != "ON":
            continue
        start = window["start"]
        for k in range(0, int(window["end"] - start), every):
            for j in range(1, skip):
                gone.add(start + k + j)
                lags.append({"t": start + k + j, "lag_ms": stall_ms - 1000.0 * j})
            for row in trace["latency"]:
                if row["t"] == start + k:
                    row["ms"] = (
                        stall_ms if row["op"] in ("jsonl", "step") else row["ms"]
                    )
    trace["latency"] = [row for row in trace["latency"] if row["t"] not in gone]
    trace["missed_ticks"] = {"workload": lags, "sampler": []}
    return trace


class ArgumentTests(unittest.TestCase):
    def test_pattern(self) -> None:
        self.assertEqual(
            s2.parse_pattern("off,on,off,on,off")[0], ["OFF", "ON", "OFF", "ON", "OFF"]
        )
        for bad in ("on,off,on", "off,off,on,off", "off,on", "off,x,off"):
            states, refusal = s2.parse_pattern(bad)
            self.assertIsNone(states, bad)
            self.assertTrue(refusal, bad)

    def test_workdir_must_be_a_new_sibling_on_the_source_device(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            source = Path(tmp) / "source"
            source.mkdir()
            self.assertIsNone(s2.workdir_refusal(source, Path(tmp) / "work"))
            self.assertIn("inside the source", s2.workdir_refusal(source, source / "w"))
            self.assertIn("already exists", s2.workdir_refusal(source, Path(tmp)))
            other = s2.workdir_refusal(
                source,
                Path(tmp) / "work",
                device_of=lambda p: 1 if p == source.resolve() else 2,
            )
            self.assertIn("not on the source's device", other)

    def test_run_refusals(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            source = Path(tmp) / "source"
            source.mkdir()
            base = ["run", "--source", str(source), "--out", str(Path(tmp) / "out")]

            def refused(*extra: str) -> str:
                args = s2.parser().parse_args([*base, *extra])
                states, _ = s2.parse_pattern(args.pattern)
                return " | ".join(s2.run_refusals(args, states))

            self.assertEqual(refused("--", "true"), "")
            self.assertIn("need a command after --", refused())
            self.assertIn("drop the ON command", refused("--aa", "--", "true"))
            gated = refused(
                "--evidence", "--window-seconds", "60", "--budget-p95", "0.5", "--", "x"
            )
            self.assertIn("at least 300 s", gated)
            self.assertIn("budget_p95 is pinned", gated)
            self.assertIn("--coordinator-quiet", gated)
            self.assertEqual(refused("--evidence", "--coordinator-quiet", "x"), "")
            self.assertEqual(
                refused(
                    "--evidence",
                    "--coordinator-quiet",
                    "--min-samples",
                    "200",
                    "--min-on-busy",
                    "0.9",
                    "--",
                    "x",
                ),
                "",
            )
            relaxed = refused(
                "--evidence",
                "--coordinator-quiet",
                "--settle-seconds",
                "0",
                "--min-on-busy",
                "0",
                "--min-samples",
                "1",
                "--min-window-samples",
                "1",
                "--no-repeat",
                "--max-runs",
                "1",
                "--",
                "true",
            )
            for needle in (
                "settle_seconds is pinned",
                "min_on_busy of at least",
                "min_samples of at least",
                "min_window_samples of at least",
                "no --no-repeat",
                "--max-runs at 1000",
            ):
                self.assertIn(needle, relaxed)
            self.assertIn(
                "settle_seconds is pinned",
                refused(
                    "--evidence", "--coordinator-quiet", "--settle-seconds", "120", "x"
                ),
            )
            self.assertIn(
                "under half a window", refused("--settle-seconds", "150", "x")
            )
            self.assertIn(
                "inside the source",
                " | ".join(
                    s2.run_refusals(
                        s2.parser().parse_args(
                            ["run", "--source", str(source), "--out", str(source / "o")]
                            + ["x"]
                        ),
                        None,
                    )
                ),
            )

    def test_a_refused_run_writes_nothing(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                status = s2.main(
                    ["run", "--source", str(Path(tmp) / "absent"), "--out"]
                    + [str(Path(tmp) / "out"), "--", "true"]
                )
            self.assertEqual(status, 2)
            self.assertIn("refused reason=", out.getvalue())
            self.assertEqual(list(Path(tmp).iterdir()), [])

    def test_analyze_re_derives_the_verdict(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "trace.json"
            path.write_text(json.dumps(synth(on_factor=1.1)))
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                self.assertEqual(s2.main(["analyze", str(path)]), 0)
            self.assertIn("s2-budget verdict status=PASS", out.getvalue())
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                s2.main(["analyze", str(path), "--budget-p95", "0.05"])
            self.assertIn("status=FAIL", out.getvalue())
            self.assertIn("evidence=false", out.getvalue())


class GitIsolationTests(unittest.TestCase):
    def test_git_env_drops_every_inherited_git_variable(self) -> None:
        inherited = {
            "PATH": "/bin",
            "GIT_DIR": "/elsewhere/.git",
            "GIT_INDEX_FILE": "/elsewhere/.git/index",
            "GIT_WORK_TREE": "/elsewhere",
            "GIT_CONFIG_COUNT": "1",
            "GIT_CONFIG_KEY_0": "core.hooksPath",
            "GIT_CONFIG_VALUE_0": "/elsewhere",
            "GIT_TEMPLATE_DIR": "/elsewhere/templates",
        }
        env = s2.git_env(Path("/work"), inherited)
        self.assertEqual(env["PATH"], "/bin")
        self.assertEqual(
            {k: v for k, v in env.items() if k.startswith("GIT_")},
            {**s2.GIT_SET, "GIT_CEILING_DIRECTORIES": "/work"},
        )

    def test_fixtures_and_workload_never_touch_an_inherited_repository(self) -> None:
        git = s2.shutil.which("git")
        if git is None:
            self.skipTest("git is not on PATH")
        with tempfile.TemporaryDirectory() as tmp:
            decoy = Path(tmp) / "decoy"
            decoy.mkdir()
            s2.build_fixtures(decoy, git)
            dot = decoy / "repo" / ".git"
            before = {
                "head": (dot / "HEAD").read_bytes(),
                "main": (dot / "refs" / "heads" / "main").read_bytes(),
                "index": (dot / "index").read_bytes(),
            }
            hostile = {
                "GIT_DIR": str(dot),
                "GIT_INDEX_FILE": str(dot / "index"),
                "GIT_WORK_TREE": str(decoy / "repo"),
            }
            root = Path(tmp) / "work"
            root.mkdir()
            with mock.patch.dict(s2.os.environ, hostile):
                s2.build_fixtures(root, git)
                tool, search = s2.search_command(root / "tree")
                workload = s2.Workload(root, git, search)
                try:
                    results = workload.step()
                finally:
                    workload.close()
            after = {
                "head": (dot / "HEAD").read_bytes(),
                "main": (dot / "refs" / "heads" / "main").read_bytes(),
                "index": (dot / "index").read_bytes(),
            }
            own = root / "repo" / ".git"
            self.assertTrue((own / "refs" / "heads" / "main").is_file())
            self.assertTrue((own / "index").is_file())
        self.assertEqual(after, before)
        self.assertTrue(all(error is None for _, _, error in results), results)


class LiveRunTests(unittest.TestCase):
    def test_a_short_run_samples_waits_and_ends(self) -> None:
        before = threading.active_count()
        with tempfile.TemporaryDirectory() as tmp:
            source = Path(tmp) / "source"
            source.mkdir()
            (source / "file").write_text("corpus\n")
            out_dir = Path(tmp) / "out"
            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout):
                status = s2.main(
                    [
                        "run",
                        "--source",
                        str(source),
                        "--out",
                        str(out_dir),
                        "--pattern",
                        "off,on,off",
                        "--window-seconds",
                        "2",
                        "--settle-seconds",
                        "0",
                        "--min-samples",
                        "1",
                        "--min-window-samples",
                        "1",
                        "--min-on-busy",
                        "0",
                        "--max-runs",
                        "2",
                        "--label",
                        "unit test",
                        "--",
                        "sh",
                        "-c",
                        'echo "counters priority=$1 priority_from=flag '
                        'source_wal_index_touched=1 source_wal_created=1"',
                        "on",
                        "{priority}",
                    ]
                )
            self.assertEqual(status, 0, stdout.getvalue())
            self.assertEqual(threading.active_count(), before)
            trace = json.loads((out_dir / "trace.json").read_text())
            summary = (out_dir / "summary.md").read_text()
            leftovers = list(Path(tmp).glob("s2-budget-workload-*"))
        self.assertEqual(leftovers, [])
        self.assertEqual([w["state"] for w in trace["windows"]], ["OFF", "ON", "OFF"])
        on = trace["windows"][1]
        self.assertEqual([r["exit"] for r in on["runs"]], [0, 0])
        self.assertTrue(on["run_cap_reached"])
        for run in on["runs"]:
            self.assertEqual(run["priority_observed"], ["background"])
            self.assertEqual(run["source_wal_index_touched"], 1)
            self.assertEqual(run["source_wal_created"], 1)
            self.assertEqual(run["argv"][-1], "background")
        self.assertEqual(trace["verdict"]["source_wal_index_touched"]["total"], 2)
        self.assertIn("wal-index files created or touched (OI-1003-Q36): 2", summary)
        self.assertEqual(trace["verdict"]["source_wal_created"]["total"], 2)
        self.assertIn("Empty source `-wal` files created (OI-1003-Q72): 2", summary)
        ops = {row["op"] for row in trace["latency"]}
        self.assertEqual(ops, {"jsonl", "sqlite", "git", "search", "step"})
        self.assertTrue(all(row["ok"] for row in trace["latency"]), trace["latency"])
        self.assertGreaterEqual(len(trace["load"]), 4)
        self.assertIsNone(trace["cut_short"])
        self.assertEqual(trace["errors"], [])
        self.assertEqual(set(trace["missed_ticks"]), {"sampler", "workload"})
        self.assertEqual(
            len(trace["missed_ticks"]["workload"]), trace["skipped_ticks"]["workload"]
        )
        self.assertIn(trace["config"]["search_tool"], ("rg", "grep -r"))
        verdict = trace["verdict"]
        self.assertIn(verdict["status"], ("PASS", "FAIL", "INCONCLUSIVE"))
        self.assertFalse(verdict["evidence"])
        self.assertIn("NOT EVIDENCE", summary)
        self.assertIn("NOT EVIDENCE", stdout.getvalue())

    def test_a_live_aa_run_starts_nothing_and_never_gives_a_budget_verdict(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            source = Path(tmp) / "source"
            source.mkdir()
            out_dir = Path(tmp) / "out"
            with contextlib.redirect_stdout(io.StringIO()):
                status = s2.main(
                    ["run", "--source", str(source), "--out", str(out_dir), "--aa"]
                    + ["--pattern", "off,on,off", "--window-seconds", "2"]
                    + ["--settle-seconds", "0", "--min-samples", "1"]
                    + ["--min-window-samples", "1"]
                )
            trace = json.loads((out_dir / "trace.json").read_text())
            runs = list((out_dir / "on-runs").iterdir())
        self.assertEqual(status, 0)
        self.assertEqual(runs, [])
        self.assertNotIn("runs", trace["windows"][1])
        self.assertEqual(trace["verdict"]["mode"], "aa")
        self.assertIn(trace["verdict"]["status"], ("QUIET", "NOISY", "INCONCLUSIVE"))
        self.assertFalse(trace["verdict"]["evidence"])

    def test_an_overrunning_on_run_is_waited_for_then_the_run_is_cut_short(
        self,
    ) -> None:
        before = threading.active_count()
        with tempfile.TemporaryDirectory() as tmp:
            source = Path(tmp) / "source"
            source.mkdir()
            out_dir = Path(tmp) / "out"
            with contextlib.redirect_stdout(io.StringIO()):
                status = s2.main(
                    ["run", "--source", str(source), "--out", str(out_dir)]
                    + ["--pattern", "off,on,off", "--window-seconds", "1"]
                    + ["--settle-seconds", "0", "--max-overrun-seconds", "0"]
                    + ["--", "sleep", "2"]
                )
            trace = json.loads((out_dir / "trace.json").read_text())
        self.assertEqual(status, 0)
        self.assertEqual(threading.active_count(), before)
        on = trace["windows"][1]
        self.assertEqual(len(on["runs"]), 1)
        self.assertEqual(on["runs"][0]["exit"], 0)
        self.assertGreaterEqual(on["runs"][0]["seconds"], 2.0)
        self.assertGreater(on["overrun_s"], 0.9)
        self.assertEqual(len(trace["windows"]), 2)
        self.assertIn("session bound reached", trace["cut_short"])
        self.assertEqual(trace["verdict"]["status"], "INCONCLUSIVE")


ORIGIN = 1_760_000_000.0


def observation(trace, power="ac"):
    """An `observe` header and its rows, from a synthetic trace: the same
    samples, timestamped from ORIGIN, and the run's ON windows as an
    intervals file of epoch seconds."""
    span = trace["windows"][-1]["end"]
    header = {
        "schema": s2.ALONGSIDE_SCHEMA,
        "rulings": s2.ALONGSIDE_RULINGS,
        "label": "synthetic",
        "config": {
            "source": "/src",
            "workdir": "/work",
            "out": "/out",
            "search_tool": "rg",
            "workload": "v0",
        },
        "host": {
            "hostname": "neo",
            "system": "Darwin",
            "release": "x",
            "machine": "arm64",
            "cpus": 12,
            "git": "git",
            "search": "rg",
            "load_source": "sysctl -n vm.loadavg",
        },
        "start_utc": "2026-10-15T00:00:00Z",
        "end_utc": "2026-10-15T01:00:00Z",
        "origin_epoch": ORIGIN,
        "span_s": span,
        "end_reason": "stop file",
        "errors": [],
        "skipped_ticks": {"sampler": 0, "workload": 0},
    }
    rows = [{"k": "lat", **row} for row in trace["latency"]]
    # Sampler rows carry the wall clock beside the monotonic offset.
    rows += [{"k": "load", "e": ORIGIN + row["t"], **row} for row in trace["load"]]
    rows += [
        {"k": "power", "t": t, "power": power} for t in range(0, int(span) + 1, 60)
    ]
    rows.append({"k": "end", "t": span, "reason": "stop file"})
    intervals = "\n".join(
        f"{ORIGIN + w['start']:.3f} {ORIGIN + w['end']:.3f} pass{w['index']}"
        for w in trace["windows"]
        if w["state"] == "ON"
    )
    return header, rows, intervals


def alongside_verdict(
    trace, evidence=True, min_window=300.0, intervals=None, power="ac", edit=None
):
    header, rows, derived = observation(trace, power)
    if edit is not None:
        header, rows = edit(header, rows)
    built = s2.alongside_trace(
        header,
        rows,
        derived if intervals is None else intervals,
        "intervals.txt",
        min_window,
        evidence,
        dict(s2.DEFAULT_GATE),
    )
    return built, s2.analyze(built)


def long_synth(**kwargs):
    """A synthetic trace with 400 s windows at 1 Hz: each OFF gap keeps at
    least 300 s once the next pass's settle lead is taken from it."""
    return synth(window=400.0, n=400, **kwargs)


class AlongsideTests(unittest.TestCase):
    """#165 (S2 budget alongside a real run; OI-1003-Q152: no freeze window)."""

    def test_intervals_read_epochs_iso_and_marks_and_merge(self) -> None:
        text = "\n".join(
            [
                "# pass one",
                "100 400 pass1",
                "350 500",
                "2026-10-15T00:00:00Z 2026-10-15T00:05:00Z",
                "on 9000 pass3",
                "off 9600",
            ]
        )
        intervals, open_interval, problems = s2.parse_intervals(text)
        midnight = s2.parse_time("2026-10-15T00:00:00Z")
        self.assertEqual(problems, [])
        self.assertFalse(open_interval)
        self.assertEqual(
            intervals, [(100.0, 500.0), (9000.0, 9600.0), (midnight, midnight + 300)]
        )

    def test_an_open_mark_and_a_bad_line_are_kept_apart(self) -> None:
        intervals, open_interval, problems = s2.parse_intervals(
            "on 10\nbogus line\noff 5 early\n500 400\non 600"
        )
        self.assertTrue(open_interval)
        # The second `on` while one is open is a problem, never a restart.
        self.assertEqual(len(problems), 4, problems)
        self.assertIn("on after on", problems[-1])
        self.assertEqual(intervals[-1][1], float("inf"))

    def test_windows_alternate_and_short_ones_are_dropped(self) -> None:
        kept, dropped = s2.derive_windows([(400, 800), (850, 1300)], 2000, 300)
        self.assertEqual(
            [(w["state"], w["start"], w["end"]) for w in kept],
            [
                ("OFF", 0.0, 400.0),
                ("ON", 400.0, 800.0),
                ("ON", 850.0, 1300.0),
                ("OFF", 1300.0, 2000.0),
            ],
        )
        self.assertEqual([w["index"] for w in kept], [0, 1, 2, 3])
        self.assertEqual([(w["state"], w["start"]) for w in dropped], [("OFF", 800.0)])

    def test_a_real_runs_intervals_give_the_budget_verdict(self) -> None:
        built, verdict = alongside_verdict(
            long_synth(on_factor=1.10, on_add=0.5, evidence=True)
        )
        self.assertEqual(verdict["status"], "PASS", reasons(verdict))
        self.assertTrue(verdict["evidence"], verdict["evidence_problems"])
        self.assertEqual(built["config"]["pattern"], list(FIVE))
        # Nothing was started: no ON-run reason is ever raised.
        self.assertNotIn("ran no command", reasons(verdict))
        _, failing = alongside_verdict(long_synth(on_factor=1.5, evidence=True))
        self.assertEqual(failing["status"], "FAIL", reasons(failing))
        _, loaded = alongside_verdict(long_synth(on_add=2.5, evidence=True))
        self.assertEqual(loaded["status"], "FAIL", reasons(loaded))

    def test_short_windows_are_not_evidence(self) -> None:
        _, verdict = alongside_verdict(
            synth(window=120.0, n=120, evidence=True), min_window=100.0
        )
        self.assertFalse(verdict["evidence"])
        self.assertTrue(
            any("at least 300 s" in p for p in verdict["evidence_problems"]),
            verdict["evidence_problems"],
        )

    def test_an_interval_still_open_is_inconclusive_and_not_evidence(self) -> None:
        trace = long_synth(on_factor=1.10, evidence=True)
        start = trace["windows"][1]["start"]
        _, verdict = alongside_verdict(trace, intervals=f"on {ORIGIN + start:.3f}")
        self.assertEqual(verdict["status"], "INCONCLUSIVE")
        self.assertFalse(verdict["evidence"])
        self.assertIn("still open", reasons(verdict))

    def test_battery_is_not_evidence(self) -> None:
        _, verdict = alongside_verdict(long_synth(evidence=True), power="battery")
        self.assertEqual(verdict["status"], "INCONCLUSIVE")
        self.assertFalse(verdict["evidence"])

    def test_evidence_rows_carry_every_gated_metric_and_window(self) -> None:
        built, verdict = alongside_verdict(long_synth(on_factor=1.10, evidence=True))
        rows = s2.evidence_rows(built, verdict)
        kinds = [row[0] for row in rows[1:]]
        self.assertEqual(rows[0][0], "kind")
        self.assertEqual(kinds.count("delta_p95"), len(s2.METRICS))
        self.assertEqual(kinds.count("delta_load1"), 1)
        self.assertEqual(kinds.count("window"), len(FIVE))
        self.assertEqual(rows[1][:3], ["verdict", "S2", "PASS"])
        # OI-1003-Q160: alongside evidence is labelled as such, and carries
        # the live baseline's spread and every adjacent pair.
        self.assertTrue(all(row[-1] == s2.ALONGSIDE_EVIDENCE_LABEL for row in rows[1:]))
        self.assertEqual(kinds.count("baseline"), 1)
        self.assertEqual(kinds.count("pair"), FIVE.count("ON"))

    def test_observe_starts_nothing_and_alongside_reads_it(self) -> None:
        before = threading.active_count()
        with tempfile.TemporaryDirectory() as tmp:
            source = Path(tmp) / "source"
            source.mkdir()
            out_dir = Path(tmp) / "observed"
            stop = Path(tmp) / "stop"
            marks = Path(tmp) / "intervals"
            timer = threading.Timer(3.5, stop.touch)
            timer.start()
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(
                    s2.main(["mark", str(marks), "on", "--label", "pass"]), 0
                )
                status = s2.main(
                    [
                        "observe",
                        "--source",
                        str(source),
                        "--out",
                        str(out_dir),
                        "--stop-file",
                        str(stop),
                        "--max-seconds",
                        "60",
                    ]
                )
                self.assertEqual(s2.main(["mark", str(marks), "off"]), 0)
                along = s2.main(
                    [
                        "alongside",
                        str(out_dir),
                        "--intervals",
                        str(marks),
                        "--min-window-seconds",
                        "0",
                    ]
                )
            timer.join()
            header = json.loads((out_dir / "observe.json").read_text())
            rows = [
                json.loads(line)
                for line in (out_dir / "rows.jsonl").read_text().splitlines()
            ]
            evidence = (out_dir / "alongside" / "evidence.tsv").read_text()
            listing = sorted(path.name for path in out_dir.iterdir())
            leftovers = list(Path(tmp).glob("s2-observe-workload-*"))
        self.assertEqual((status, along), (0, 0))
        self.assertEqual(threading.active_count(), before)
        self.assertEqual(header["end_reason"], "stop file")
        self.assertEqual(header["errors"], [])
        kinds = {row["k"] for row in rows}
        self.assertTrue({"lat", "load", "power"} <= kinds, kinds)
        self.assertEqual(
            {row["op"] for row in rows if row["k"] == "lat"},
            {"jsonl", "sqlite", "git", "search", "step"},
        )
        # Its own files only: no ON command ran, nothing else was made.
        self.assertEqual(listing, ["alongside", "observe.json", "rows.jsonl"])
        self.assertEqual(leftovers, [])
        self.assertTrue(evidence.startswith("kind\tname\tvalue"))
        self.assertIn("not-evidence", evidence)

    def test_observe_refusals(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            source = Path(tmp) / "source"
            source.mkdir()
            (Path(tmp) / "exists").mkdir()
            with contextlib.redirect_stdout(io.StringIO()):
                taken = s2.main(
                    [
                        "observe",
                        "--source",
                        str(source),
                        "--out",
                        str(Path(tmp) / "exists"),
                    ]
                )
                inside = s2.main(
                    [
                        "observe",
                        "--source",
                        str(source),
                        "--out",
                        str(Path(tmp) / "o"),
                        "--stop-file",
                        str(source / "stop"),
                    ]
                )
            made = (Path(tmp) / "o").exists()
        self.assertEqual((taken, inside), (2, 2))
        self.assertFalse(made)


def shifted(rows, after, by):
    """The rows with the wall clock of every sampler row after `after`
    seconds moved by `by` seconds: a sleep or a clock step mid-run."""
    out = []
    for row in rows:
        if "e" in row and row["t"] > after:
            row = {**row, "e": row["e"] + by}
        out.append(row)
    return out


def raised_starts(trace, seconds, factor):
    """The trace with every latency sample in the first `seconds` of each ON
    window multiplied by `factor`: a pass slow only while it walks."""
    for window in trace["windows"]:
        if window["state"] != "ON":
            continue
        for row in trace["latency"]:
            if window["start"] <= row["t"] < window["start"] + seconds:
                row["ms"] *= factor
    return trace


class AlongsideReviewTests(unittest.TestCase):
    """The 2026-10-15 review of #165's alongside mode: each test is red on a
    mutant or on the code before the fix (the agent note's table)."""

    def test_an_observation_with_no_recorded_end_is_cut_short(self) -> None:
        def unended(header, rows):
            # The header never closed (its end row alone is no proof).
            header = {
                k: v for k, v in header.items() if k not in ("end_reason", "span_s")
            }
            return header, rows

        _, verdict = alongside_verdict(long_synth(on_factor=1.10), edit=unended)
        self.assertEqual(verdict["status"], "INCONCLUSIVE")
        self.assertIn("cut short", reasons(verdict))

        def no_end_row(header, rows):
            return header, [row for row in rows if row.get("k") != "end"]

        _, verdict = alongside_verdict(long_synth(on_factor=1.10), edit=no_end_row)
        self.assertEqual(verdict["status"], "INCONCLUSIVE")
        self.assertIn("cut short", reasons(verdict))

    def test_missed_ticks_are_backfilled_into_their_windows(self) -> None:
        trace = long_synth(on_factor=1.0)

        def missed(header, rows):
            extra = []
            for window in trace["windows"]:
                if window["state"] == "ON":
                    for k in range(100, 160):
                        extra.append(
                            {
                                "k": "missed",
                                "loop": "workload",
                                "t": window["start"] + k + 0.5,
                                "lag_ms": 5000.0,
                            }
                        )
            return header, rows + extra

        _, verdict = alongside_verdict(trace, edit=missed)
        # Back-filled at their 5 s wait, the step p95 is over the budget.
        self.assertEqual(verdict["gates"]["step"], "FAIL", reasons(verdict))
        self.assertEqual(verdict["samples"]["ON"]["missed_ticks"], 120)
        _, clean = alongside_verdict(long_synth(on_factor=1.0))
        self.assertEqual(clean["gates"]["step"], "PASS", reasons(clean))

    def test_one_on_window_among_five_is_not_evidence(self) -> None:
        trace = long_synth(pattern=("OFF", "OFF", "ON", "OFF", "OFF"), on_factor=1.10)
        intervals = "\n".join(
            f"{ORIGIN + a:.3f} {ORIGIN + b:.3f}"
            for a, b in ((390, 400), (800, 1200), (1590, 1600))
        )
        built, verdict = alongside_verdict(trace, intervals=intervals)
        self.assertEqual(built["config"]["pattern"], ["OFF", "OFF", "ON", "OFF", "OFF"])
        self.assertFalse(verdict["evidence"])
        self.assertTrue(
            any("2 ON" in p for p in verdict["evidence_problems"]),
            verdict["evidence_problems"],
        )

    def test_three_windows_are_not_evidence_by_the_five_window_rule(self) -> None:
        _, verdict = alongside_verdict(long_synth(pattern=("OFF", "ON", "OFF")))
        self.assertFalse(verdict["evidence"])
        self.assertTrue(
            any("at least 5 windows" in p for p in verdict["evidence_problems"]),
            verdict["evidence_problems"],
        )

    def test_the_start_of_every_pass_is_measured(self) -> None:
        trace = raised_starts(
            synth(window=600.0, n=600, on_factor=1.0, evidence=True), 60.0, 3.0
        )
        built, verdict = alongside_verdict(trace)
        # Before the fix the first minute was the settle: PASS at +0 %. Now
        # it is measured: the point is over the budget, and two raised
        # 30 s blocks in twenty leave the bounds straddling it.
        self.assertNotEqual(verdict["status"], "PASS", reasons(verdict))
        self.assertGreater(verdict["delta_p95_by_metric"]["step"], 0.25)
        on = [w for w in built["windows"] if w["state"] == "ON"]
        # Each ON window starts one settle before its pass.
        self.assertEqual(
            [(w["start"], w["lead_s"]) for w in on], [(540.0, 60.0), (1740.0, 60.0)]
        )

    def test_a_pass_with_no_room_for_its_settle_is_not_evidence(self) -> None:
        trace = long_synth(on_factor=1.10)
        # Two passes 30 s apart: the second has no 60 s gap before it.
        intervals = "\n".join(
            f"{ORIGIN + a:.3f} {ORIGIN + b:.3f}"
            for a, b in ((400, 800), (830, 1200), (1600, 2000))
        )
        _, verdict = alongside_verdict(trace, intervals=intervals, min_window=0.0)
        self.assertFalse(verdict["evidence"])
        self.assertTrue(
            any("settle" in p for p in verdict["evidence_problems"]),
            verdict["evidence_problems"],
        )

    def test_a_wall_clock_jump_is_an_instrument_error(self) -> None:
        def slept(header, rows):
            return header, shifted(rows, 1000.0, 600.0)

        _, verdict = alongside_verdict(long_synth(on_factor=1.10), edit=slept)
        self.assertEqual(verdict["status"], "INCONCLUSIVE")
        self.assertIn("instrument errors", reasons(verdict))

        def unclocked(header, rows):
            return header, [{k: v for k, v in row.items() if k != "e"} for row in rows]

        _, verdict = alongside_verdict(long_synth(on_factor=1.10), edit=unclocked)
        self.assertEqual(verdict["status"], "INCONCLUSIVE")

    def test_power_that_changes_inside_a_window_is_not_evidence(self) -> None:
        trace = long_synth(on_factor=1.10)

        def unplugged(header, rows):
            # Battery from the middle of window 2 on (sampled every 60 s).
            out = []
            for row in rows:
                if row.get("k") == "power" and row["t"] >= 1000:
                    row = {**row, "power": "battery"}
                out.append(row)
            return header, out

        built, verdict = alongside_verdict(trace, edit=unplugged)
        window = built["windows"][2]
        self.assertEqual(
            (window["power_start"], window["power_end"]), ("ac", "battery")
        )
        self.assertFalse(verdict["evidence"])
        self.assertEqual(verdict["status"], "INCONCLUSIVE")

    def test_a_bad_intervals_line_is_inconclusive(self) -> None:
        trace = long_synth(on_factor=1.10)
        _, _, derived = observation(trace)
        _, verdict = alongside_verdict(trace, intervals=derived + "\nnot a line\n")
        self.assertEqual(verdict["status"], "INCONCLUSIVE")
        self.assertIn("instrument errors", reasons(verdict))

    def test_a_torn_row_is_inconclusive(self) -> None:
        trace = long_synth(on_factor=1.10)
        header, rows, derived = observation(trace)
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            (directory / "observe.json").write_text(json.dumps(header))
            text = "".join(json.dumps(row) + "\n" for row in rows)
            (directory / "rows.jsonl").write_text(text + '{"k":"lat","t":1')
            read_header, read_rows = s2.read_observation(directory)
        built = s2.alongside_trace(
            read_header, read_rows, derived, "i", 300.0, True, dict(s2.DEFAULT_GATE)
        )
        verdict = s2.analyze(built)
        self.assertEqual(verdict["status"], "INCONCLUSIVE")
        self.assertIn("instrument errors", reasons(verdict))

    def test_an_open_interval_is_named_among_five_kept_windows(self) -> None:
        trace = long_synth(on_factor=1.10)
        _, _, derived = observation(trace)
        # A last pass started 50 s before the observation ended.
        opened = derived + f"\non {ORIGIN + 1950:.3f}"
        built, verdict = alongside_verdict(trace, intervals=opened, min_window=100.0)
        self.assertGreaterEqual(len(built["config"]["pattern"]), 5)
        self.assertTrue(
            any("still open" in p for p in verdict["evidence_problems"]),
            verdict["evidence_problems"],
        )

    def test_the_shortest_kept_window_decides_the_window_rule(self) -> None:
        trace = synth(
            pattern=("OFF", "ON", "OFF", "ON", "OFF"),
            window=400.0,
            n=400,
            evidence=True,
        )
        # The middle OFF gap is 200 s (+ lead): kept under --min-window 100.
        intervals = "\n".join(
            f"{ORIGIN + a:.3f} {ORIGIN + b:.3f}" for a, b in ((400, 800), (1000, 1400))
        )
        built, verdict = alongside_verdict(trace, intervals=intervals, min_window=100.0)
        self.assertEqual(len(built["windows"]), 5)
        self.assertLess(built["config"]["window_seconds"], 300.0)
        self.assertFalse(verdict["evidence"])

    def test_observe_streams_missed_ticks_and_ends_its_record_on_request(
        self,
    ) -> None:
        import signal

        original = s2.Workload.step
        calls = [0]

        def slow(workload):
            calls[0] += 1
            if calls[0] == 3:
                threading.Event().wait(2.6)
            return original(workload)

        before = (signal.getsignal(signal.SIGTERM), signal.getsignal(signal.SIGHUP))
        seen = {}
        with tempfile.TemporaryDirectory() as tmp:
            source = Path(tmp) / "source"
            source.mkdir()
            out_dir = Path(tmp) / "observed"

            def request_end() -> None:
                # Wait (bounded) for a missed tick on disk while the
                # observation still runs, then ask it to end.
                waited = threading.Event()
                for _ in range(600):
                    rows_file = out_dir / "rows.jsonl"
                    rows = (
                        rows_file.read_text().splitlines() if rows_file.exists() else []
                    )
                    seen["missed"] = sum('"k":"missed"' in row for row in rows)
                    if seen["missed"]:
                        break
                    waited.wait(0.1)
                handler = signal.getsignal(signal.SIGTERM)
                seen["handled"] = callable(handler)
                if callable(handler):
                    handler(signal.SIGTERM, None)

            timer = threading.Thread(target=request_end)
            timer.start()
            with (
                mock.patch.object(s2.Workload, "step", slow),
                mock.patch.object(s2, "POWER_EVERY_S", 1.0),
                contextlib.redirect_stdout(io.StringIO()),
            ):
                status = s2.main(
                    [
                        "observe",
                        "--source",
                        str(source),
                        "--out",
                        str(out_dir),
                        "--max-seconds",
                        "120",
                    ]
                )
            timer.join()
            header = json.loads((out_dir / "observe.json").read_text())
            rows = [
                json.loads(line)
                for line in (out_dir / "rows.jsonl").read_text().splitlines()
            ]
        after = (signal.getsignal(signal.SIGTERM), signal.getsignal(signal.SIGHUP))
        self.assertEqual(status, 0)
        # The missed ticks were on disk before the observation ended.
        self.assertGreaterEqual(seen.get("missed", 0), 1, seen)
        self.assertTrue(seen.get("handled"), seen)
        self.assertEqual(header["end_reason"], "SIGTERM")
        self.assertEqual(rows[-1]["k"], "end")
        # Power sampled on its cadence while observing, not only at the end.
        self.assertGreaterEqual(
            sum(
                1
                for row in rows
                if row["k"] == "power" and row["t"] < header["span_s"] - 0.5
            ),
            2,
        )
        self.assertEqual(after, before)


def relevel(trace, levels):
    """The trace with every load1 row of window i set to levels[i]."""
    for window in trace["windows"]:
        for row in trace["load"]:
            if window["start"] <= row["t"] < window["end"]:
                row["load1"] = levels[window["index"]]
    return trace


def stepped(n_on, step_after, before, after, add):
    """A long alternating observation (OFF first and last, n_on ON passes)
    whose live baseline steps from `before` to `after` once window
    `step_after` ends, each pass adding `add` to the baseline of its time."""
    pattern = ("OFF", "ON") * n_on + ("OFF",)
    levels = [
        (before if index <= step_after else after) + (add if state == "ON" else 0.0)
        for index, state in enumerate(pattern)
    ]
    return relevel(long_synth(pattern=pattern, on_factor=1.10, evidence=True), levels)


def rescaled(trace, step_after, factor):
    """The trace with every latency sample after window `step_after`
    multiplied by `factor`: the host's own latency stepping down."""
    for window in trace["windows"]:
        if window["index"] > step_after:
            for row in trace["latency"]:
                if window["start"] <= row["t"] < window["end"]:
                    row["ms"] *= factor
    return trace


class AlongsideLiveBaselineTests(unittest.TestCase):
    """OI-1003-Q160: an observation alongside a live run is judged against
    the host's own live load. Its OFF windows between passes are the
    baseline with no absolute load1 cap (R-N81's 2.5 is amended for
    alongside runs only); the budget, AC power, the window rules and the
    recorded load are unchanged; and every ON window is compared with the
    OFF windows on both sides of it, so linear drift in the host's own load
    is not read as bulkload's cost or credit."""

    def test_a_busy_live_baseline_is_alongside_evidence(self) -> None:
        # Red before Q160: "R-N81: OFF window 0 load1 level 6.00 is not
        # under 2.5", INCONCLUSIVE and not evidence.
        built, verdict = alongside_verdict(
            long_synth(off_load=6.0, on_factor=1.10, on_add=0.5, evidence=True)
        )
        self.assertEqual(verdict["status"], "PASS", reasons(verdict))
        self.assertTrue(verdict["evidence"], verdict["evidence_problems"])
        self.assertEqual(
            verdict["evidence_label"], "alongside, live baseline (OI-1003-Q160)"
        )
        self.assertNotIn("R-N81", reasons(verdict))
        summary = s2.render_summary(built, verdict)
        self.assertIn(
            "PASS (evidence: alongside, live baseline (OI-1003-Q160))", summary
        )
        self.assertIn("Live baseline (OI-1003-Q160)", summary)
        # The budget is unchanged: +2.0 load1 over the live baseline fails.
        _, loaded = alongside_verdict(
            long_synth(off_load=6.0, on_add=2.5, evidence=True)
        )
        self.assertEqual(loaded["status"], "FAIL", reasons(loaded))
        self.assertIn("load1 +2.", reasons(loaded))
        # AC power still applies to a busy live baseline.
        _, battery = alongside_verdict(
            long_synth(off_load=6.0, on_factor=1.10, evidence=True), power="battery"
        )
        self.assertEqual(battery["status"], "INCONCLUSIVE")
        self.assertFalse(battery["evidence"])
        self.assertIsNone(battery["evidence_label"])
        self.assertIn("not AC power", reasons(battery))

    def test_a_gated_run_keeps_r_n81s_load_cap(self) -> None:
        # Standalone gated runs are unchanged: an OFF level at 6.0 is not
        # evidence and the verdict is INCONCLUSIVE (R-N81).
        verdict = s2.analyze(
            synth(evidence=True, off_load=6.0, on_factor=1.10, on_add=0.5)
        )
        self.assertEqual(verdict["status"], "INCONCLUSIVE")
        self.assertIn("R-N81: OFF window 0 load1 level 6.00", reasons(verdict))
        self.assertFalse(verdict["evidence"])
        self.assertIsNone(verdict["live_baseline"])
        quiet = s2.analyze(synth(evidence=True, off_load=2.4, on_factor=1.10))
        self.assertTrue(quiet["evidence"], quiet["evidence_problems"])
        self.assertEqual(quiet["evidence_label"], "gated run (R-N81)")

    def test_the_baseline_spread_and_each_adjacent_pair_are_reported(self) -> None:
        # A baseline rising 1.0 -> 1.5 -> 2.0: its spread is reported, and
        # each pass against the OFF windows on both sides of it.
        built, verdict = alongside_verdict(
            long_synth(
                off_load=[1.0, 0.0, 1.5, 0.0, 2.0],
                on_factor=1.10,
                on_add=0.5,
                evidence=True,
            )
        )
        baseline = verdict["live_baseline"]
        self.assertEqual(baseline["off_load1_levels"], [1.0, 1.5, 2.0])
        self.assertEqual(baseline["off_load1_spread"], 1.0)
        pairs = [
            (p["on"], p["off_before"], p["off_after"], p["baseline_load1"])
            for p in baseline["pairs"]
        ]
        # Interpolated to each pass's time (OI-1003-Q160 review): OFF window
        # 4 is 400 s against window 2's 340 s (the lead), so pass 3 sits
        # nearer window 2 and its baseline is a little under the plain mean.
        self.assertEqual(pairs, [(1, 0, 2, 1.25), (3, 2, 4, 1.741)])
        self.assertEqual([p["drift_load1"] for p in baseline["pairs"]], [0.5, 0.5])
        for pair in baseline["pairs"]:
            self.assertAlmostEqual(pair["d_p95"]["step"], 0.10, places=6)
        # The two passes add the same, but the second reads about 0.5 lower
        # than the first (the first pass's step also tilts its own level):
        # the rising baseline, which the pairs make visible.
        first, second = (p["d_load1"] for p in baseline["pairs"])
        self.assertAlmostEqual(first - second, 0.5, delta=0.1)
        rows = s2.evidence_rows(built, verdict)
        spread = [row for row in rows if row[0] == "baseline"]
        self.assertEqual(spread[0][1:3], ["off_load1_spread", "1.0000"])
        self.assertEqual(
            [row[1] for row in rows if row[0] == "pair"],
            ["1:ON vs 0,2:OFF", "3:ON vs 2,4:OFF"],
        )

    def test_a_falling_baseline_does_not_pass_as_bulkload_credit(self) -> None:
        # 31 windows; the host's own load falls by 1.0 as pass 15 ends.
        # Pooled, the bounds say PASS (old code: PASS). Against its own
        # neighbours pass 15 reads +2.11, over the budget: the pooled PASS
        # does not hold pair by pair, so the verdict is INCONCLUSIVE.
        trace = stepped(15, 15, before=2.0, after=1.0, add=1.4)
        _, verdict = alongside_verdict(trace)
        self.assertEqual(verdict["gates"]["load1"], "PASS")
        self.assertEqual(verdict["status"], "INCONCLUSIVE", reasons(verdict))
        self.assertIn(
            "live baseline (OI-1003-Q160): ON window 15 against OFF windows 14 "
            "and 16 reads load1 +2.11 (baseline drift -1.00)",
            reasons(verdict),
        )
        # The same passes on a steady baseline pass.
        _, steady = alongside_verdict(stepped(15, 99, before=2.0, after=2.0, add=1.4))
        self.assertEqual(steady["status"], "PASS", reasons(steady))

    def test_a_rising_baseline_does_not_fail_as_bulkload_cost(self) -> None:
        # The host's own load rises by 1.0 as pass 15 ends. Pooled, the
        # bounds say FAIL (old code: FAIL); against its own neighbours pass
        # 15 reads +1.91, within the budget. A FAIL must show on every pass.
        _, verdict = alongside_verdict(stepped(15, 15, before=1.0, after=2.0, add=2.1))
        self.assertEqual(verdict["gates"]["load1"], "FAIL")
        self.assertEqual(verdict["status"], "INCONCLUSIVE", reasons(verdict))
        self.assertIn("reads load1 +1.91 (baseline drift +1.00)", reasons(verdict))
        self.assertIn("pooled load1 FAIL does not hold pair by pair", reasons(verdict))

    def test_a_latency_baseline_drift_is_seen_pair_by_pair(self) -> None:
        # The host's own latency falls 10 % as pass 27 of 31 windows ends;
        # every pass costs +20 % over the latency of its time. Pooled, the
        # bounds say PASS (old code: PASS); pass 27 against its neighbours
        # reads +26.3 %, so the pooled latency PASS does not hold.
        pattern = ("OFF", "ON") * 15 + ("OFF",)
        trace = rescaled(
            long_synth(pattern=pattern, on_factor=1.2, evidence=True), 27, 0.9
        )
        _, verdict = alongside_verdict(trace)
        self.assertEqual(verdict["gates"]["latency"], "PASS")
        self.assertEqual(verdict["status"], "INCONCLUSIVE", reasons(verdict))
        self.assertIn(
            "ON window 27 against OFF windows 26 and 28 reads p95 step +26.3% "
            "(latency FAIL), so the pooled latency PASS does not hold pair by pair",
            reasons(verdict),
        )
        # A smaller step (8 %) leaves that pass at +25.0 %, within budget.
        trace = rescaled(
            long_synth(pattern=pattern, on_factor=1.2, evidence=True), 27, 0.92
        )
        _, small = alongside_verdict(trace)
        self.assertEqual(small["status"], "PASS", reasons(small))

    def test_a_pass_with_no_off_window_after_it_is_not_judged(self) -> None:
        # The observation ended while the last pass ran: that pass has no
        # OFF window after it, so the host's drift across it is unknown and
        # the pooled PASS is not given (old code: PASS).
        trace = long_synth(pattern=("OFF", "ON", "OFF", "ON"), on_factor=1.10)
        _, verdict = alongside_verdict(trace, evidence=False)
        self.assertEqual(verdict["status"], "INCONCLUSIVE", reasons(verdict))
        self.assertIn(
            "ON window 3 against OFF windows 2 and None has no comparison",
            reasons(verdict),
        )

    def test_evidence_needs_load_recorded_and_every_pair_compared(self) -> None:
        trace = long_synth(on_factor=1.10, evidence=True)
        end = trace["windows"][2]["end"]
        start = trace["windows"][2]["start"]
        # The sampler recorded no load1 in the middle OFF window.
        trace["load"] = [row for row in trace["load"] if not start <= row["t"] < end]
        _, verdict = alongside_verdict(trace)
        self.assertFalse(verdict["evidence"])
        problems = " | ".join(verdict["evidence_problems"])
        self.assertIn("evidence needs recorded load1 in every window", problems)
        self.assertIn("ON window 1 has no complete comparison", problems)
        self.assertEqual(verdict["status"], "INCONCLUSIVE")


def timeline(span, passes, lat, load, ewma=False):
    """An `observe` header, rows and intervals over [0, span) at 1 Hz, with
    the run's passes as (start, end) seconds from ORIGIN (a pass may end
    before it: recorded before the observation began). `lat(t, on)` is the
    latency factor (or a {op: factor} dict), `load(t, on)` the run queue;
    with `ewma` load1 is what the kernel reports for it (the 5 s EWMA)."""

    def on_at(t):
        return any(a <= t < b for a, b in passes)

    rows = []
    level, update = load(0.0, on_at(0.0)), 2.5
    for k in range(int(span)):
        t = float(k)
        on = on_at(t)
        spread = 1.0 + (k % 30) / 30.0
        factor = lat(t, on)
        step = 0.0
        for op in s2.OPS:
            ms = (
                10.0
                / 4.0
                * spread
                * (factor[op] if isinstance(factor, dict) else factor)
            )
            step += ms
            rows.append({"k": "lat", "t": t, "op": op, "ms": ms, "ok": True})
        rows.append({"k": "lat", "t": t, "op": "step", "ms": step, "ok": True})
        if ewma:
            while update <= t:
                n = load(update, on_at(update))
                level = level * s2.LOAD1_DECAY + n * (1.0 - s2.LOAD1_DECAY)
                update += s2.LOAD1_UPDATE_S
            value = round(level, 2)
        else:
            value = load(t, on)
        rows.append({"k": "load", "t": t, "e": ORIGIN + t, "load1": value})
    rows += [{"k": "power", "t": t, "power": "ac"} for t in range(0, int(span) + 1, 60)]
    rows.append({"k": "end", "t": span, "reason": "stop file"})
    header, _, _ = observation(long_synth())
    header = {**header, "span_s": span}
    intervals = "\n".join(f"{ORIGIN + a:.3f} {ORIGIN + b:.3f}" for a, b in passes)
    return header, rows, intervals


def timeline_verdict(span, passes, lat, load, min_window=300.0, ewma=False):
    header, rows, intervals = timeline(span, passes, lat, load, ewma)
    built = s2.alongside_trace(
        header, rows, intervals, "i.txt", min_window, True, dict(s2.DEFAULT_GATE)
    )
    return built, s2.analyze(built)


def alternating(n, on_s, off_s, first=None):
    """n passes of on_s seconds, off_s apart, the first at `first` (else
    off_s); and the span that ends one off_s after the last."""
    passes, t = [], off_s if first is None else first
    for _ in range(n):
        passes.append((t, t + on_s))
        t += on_s + off_s
    return passes, t


class LiveBaselineReviewTests(unittest.TestCase):
    """The 2026-10-15 review of OI-1003-Q160's live baseline: each test is
    red on the code before its fix (the agent note's table)."""

    # Finding 1: pairs are judged gate by gate.

    def test_a_load_breach_on_every_pass_stands_when_one_also_overruns_p95(
        self,
    ) -> None:
        # Every pass adds +3.0 load1 (each pair FAILs load1) and pass 15 is
        # also +30 % p95. Before: INCONCLUSIVE, the load1 reason dropped.
        pattern = ("OFF", "ON") * 15 + ("OFF",)
        factors = [1.30 if index == 15 else 1.02 for index in range(len(pattern))]
        _, verdict = alongside_verdict(
            long_synth(pattern=pattern, on_factor=factors, on_add=3.0, evidence=True)
        )
        self.assertEqual(verdict["gates"]["load1"], "FAIL")
        self.assertEqual(verdict["gates"]["latency"], "PASS")
        self.assertEqual(verdict["status"], "FAIL", reasons(verdict))
        self.assertIn("load1 +3.", reasons(verdict))
        self.assertIn(
            "ON window 15 against OFF windows 14 and 16 also reads p95 step "
            "+30.0% (latency FAIL)",
            reasons(verdict),
        )

    def test_a_latency_breach_on_every_pass_stands_when_one_also_overruns_load(
        self,
    ) -> None:
        # Every pass is +40 % p95 and pass 7 also reads load1 +2.3. Before:
        # INCONCLUSIVE, the p95 reason dropped.
        pattern = ("OFF", "ON") * 15 + ("OFF",)
        trace = long_synth(pattern=pattern, on_factor=1.40, evidence=True)
        levels = [
            1.0 + (0.0 if state == "OFF" else 2.3 if index == 7 else 0.3)
            for index, state in enumerate(pattern)
        ]
        _, verdict = alongside_verdict(relevel(trace, levels))
        self.assertEqual(verdict["gates"]["latency"], "FAIL")
        self.assertEqual(verdict["status"], "FAIL", reasons(verdict))
        self.assertIn("p95 step +40.0% exceeds", reasons(verdict))
        self.assertIn(
            "ON window 7 against OFF windows 6 and 8 also reads load1 +2.",
            reasons(verdict),
        )

    def test_a_pooled_latency_fail_one_pass_contradicts_is_inconclusive(
        self,
    ) -> None:
        # Every pass +40 % but pass 9 +10 %: the pooled FAIL is not shown by
        # every pass, so it does not stand; its reason is kept beside the
        # pair's (before: replaced by it).
        pattern = ("OFF", "ON") * 15 + ("OFF",)
        factors = [1.10 if index == 9 else 1.40 for index in range(len(pattern))]
        _, verdict = alongside_verdict(
            long_synth(pattern=pattern, on_factor=factors, evidence=True)
        )
        self.assertEqual(verdict["gates"]["latency"], "FAIL")
        self.assertEqual(verdict["status"], "INCONCLUSIVE", reasons(verdict))
        self.assertIn("exceeds +25%", reasons(verdict))
        self.assertIn(
            "ON window 9 against OFF windows 8 and 10 reads p95 step +10.0% "
            "(latency PASS), so the pooled latency FAIL does not hold pair by pair",
            reasons(verdict),
        )

    # Finding 6: the pair reason names the gated metric over the budget.

    def test_one_operation_over_on_one_pass_is_seen_under_gate_both(self) -> None:
        # Pass 7 has git x1.32 alone (step +8 %): that pass's latency is
        # MIXED, so the pooled PASS does not hold, and the reason names git.
        pattern = ("OFF", "ON") * 15 + ("OFF",)
        factors = [1.03] * len(pattern)
        factors[7] = 1.0
        trace = long_synth(pattern=pattern, on_factor=factors, evidence=True)
        for row in trace["latency"]:
            window = trace["windows"][7]
            if window["start"] <= row["t"] < window["end"] and row["op"] in (
                "git",
                "step",
            ):
                if row["op"] == "git":
                    row["ms"] *= 1.32
        # The step is the sum of the operations.
        by_t = {}
        for row in trace["latency"]:
            if row["op"] != "step":
                by_t[row["t"]] = by_t.get(row["t"], 0.0) + row["ms"]
        for row in trace["latency"]:
            if row["op"] == "step":
                row["ms"] = by_t[row["t"]]
        _, verdict = alongside_verdict(trace)
        self.assertEqual(verdict["gates"]["latency"], "PASS")
        self.assertEqual(verdict["status"], "INCONCLUSIVE", reasons(verdict))
        self.assertIn(
            "ON window 7 against OFF windows 6 and 8 reads p95 git +32.0% "
            "(latency MIXED)",
            reasons(verdict),
        )

    def test_a_pair_missing_one_operation_is_not_evidence(self) -> None:
        trace = long_synth(on_factor=1.10, evidence=True)
        window = trace["windows"][2]
        trace["latency"] = [
            row
            for row in trace["latency"]
            if not (row["op"] == "git" and window["start"] <= row["t"] < window["end"])
        ]
        _, verdict = alongside_verdict(trace)
        self.assertFalse(verdict["evidence"])
        problems = " | ".join(verdict["evidence_problems"])
        self.assertIn("ON window 1 has no complete comparison", problems)

    # Finding 2: no pass escapes the verdict.

    def test_a_dropped_pass_is_not_evidence_and_is_reported(self) -> None:
        # A 100 s pass inside the first gap is under 300 s with its lead:
        # dropped, never compared. Before: evidence, nothing reported.
        trace = synth(window=600.0, n=600, on_factor=1.10, evidence=True)
        _, _, derived = observation(trace)
        intervals = f"{ORIGIN + 100:.3f} {ORIGIN + 200:.3f} short\n" + derived
        built, verdict = alongside_verdict(trace, intervals=intervals)
        self.assertEqual(built["config"]["pattern"], list(FIVE))
        self.assertFalse(verdict["evidence"])
        self.assertTrue(
            any(
                "1 pass (100 s of pass time) was dropped" in p
                for p in verdict["evidence_problems"]
            ),
            verdict["evidence_problems"],
        )
        self.assertEqual(verdict["dropped_on"], {"windows": 1, "pass_seconds": 100.0})
        self.assertIn("1 pass (100 s) dropped", s2.render_summary(built, verdict))
        rows = s2.evidence_rows(built, verdict)
        self.assertIn(
            [
                "dropped",
                "on_windows",
                "1",
                "-",
                "-",
                "-",
                "pass seconds 100",
                "not-evidence",
            ],
            rows,
        )

    def test_heavy_short_passes_do_not_hide_behind_gentle_long_ones(self) -> None:
        # The reviewer's E2: four gentle 600 s passes judged, four heavy
        # 200 s passes (a quarter of the pass time) dropped.
        passes, t, heavy = [], 600.0, []
        for _ in range(4):
            passes.append((t, t + 600))
            t += 600
            heavy.append((t + 900, t + 1100))
            passes.append(heavy[-1])
            t += 2000

        def is_heavy(t):
            return any(a <= t < b for a, b in heavy)

        def lat(t, on):
            return (2.0 if is_heavy(t) else 1.05) if on else 1.0

        def load(t, on):
            return 1.0 + ((5.0 if is_heavy(t) else 0.3) if on else 0.0)

        _, verdict = timeline_verdict(t + 600, passes, lat, load)
        self.assertFalse(verdict["evidence"])
        self.assertIsNone(verdict["evidence_label"])
        self.assertEqual(verdict["dropped_on"]["windows"], 4)

    def test_the_minimum_window_is_pinned_under_evidence(self) -> None:
        _, verdict = alongside_verdict(
            long_synth(on_factor=1.10, evidence=True), min_window=200.0
        )
        self.assertFalse(verdict["evidence"])
        self.assertTrue(
            any(
                "min_window_seconds is pinned at 300" in p
                for p in verdict["evidence_problems"]
            ),
            verdict["evidence_problems"],
        )

    # Finding 3: a recovery tail after each pass inflates the baseline.

    def test_a_recovery_tail_after_every_pass_is_not_a_pass(self) -> None:
        # The reviewer's E3: each pass x1.35 p95 and +2.2 load1, then a
        # 300 s tail of x1.25 and +1.6; the observation began as a pass
        # ended, so every OFF baseline holds a tail and their spread is
        # nil. Before: PASS, evidence, at +11.9 % and +1.83.
        passes, span = alternating(6, 600, 600)
        passes = [(-600.0, 0.0), *passes]

        def tail(t):
            ends = [b for _, b in passes if b <= t]
            return bool(ends) and t - max(ends) < 300

        def lat(t, on):
            return 1.35 if on else 1.25 if tail(t) else 1.0

        def load(t, on):
            return 1.0 + (2.2 if on else 1.6 if tail(t) else 0.0)

        _, verdict = timeline_verdict(span, passes, lat, load, ewma=True)
        self.assertEqual(verdict["noise_p95"], 0.0)
        self.assertEqual(verdict["status"], "INCONCLUSIVE", reasons(verdict))
        self.assertIn("recovery after a pass", reasons(verdict))
        self.assertFalse(verdict["evidence"])
        recovery = verdict["live_baseline"]["recovery"]
        self.assertGreater(recovery["d_p95"]["step"], 0.125)
        self.assertGreater(recovery["d_load1"], 1.0)

        # The same passes with no tail pass: the pass's own load1 decaying
        # (the kernel's EWMA) is not read as a recovery.
        def clean_lat(t, on):
            return 1.10 if on else 1.0

        def clean_load(t, on):
            return 1.0 + (1.9 if on else 0.0)

        _, clean = timeline_verdict(span, passes[1:], clean_lat, clean_load, ewma=True)
        self.assertEqual(clean["status"], "PASS", reasons(clean))
        self.assertTrue(clean["evidence"], clean["evidence_problems"])

    def test_a_latency_tail_or_a_load_tail_alone_is_seen(self) -> None:
        # Each half of the recovery rule on its own: a tail of x1.20 p95
        # alone (over half the budget, under the whole) and one of +1.5
        # load1 alone. Passes cost x1.10 and +0.5; the observation began as
        # a pass ended.
        passes, span = alternating(6, 600, 600)
        passes = [(-600.0, 0.0), *passes]

        def tail(t):
            ends = [b for _, b in passes if b <= t]
            return bool(ends) and t - max(ends) < 300

        cases = {
            "latency": (1.20, 0.0, "p95 step +20.0%"),
            "load1": (1.0, 1.5, "load1 +1."),
        }
        for name, (factor, add, named) in cases.items():
            with self.subTest(name):

                def lat(t, on, factor=factor):
                    return 1.10 if on else factor if tail(t) else 1.0

                def load(t, on, add=add):
                    return 1.0 + (0.5 if on else add if tail(t) else 0.0)

                _, verdict = timeline_verdict(span, passes, lat, load, ewma=True)
                self.assertEqual(verdict["status"], "INCONCLUSIVE", reasons(verdict))
                self.assertIn("recovery after a pass", reasons(verdict))
                self.assertIn(named, reasons(verdict))

    def test_a_pass_ending_just_before_the_observation_is_not_evidence(self) -> None:
        # The first OFF window is the only baseline with no pass before it:
        # a recorded pass ending 100 s before the origin leaves its tail in
        # it. Before: the interval was skipped silently.
        passes, span = alternating(3, 600, 600)
        _, verdict = timeline_verdict(
            span,
            [(-700.0, -100.0), *passes],
            lambda t, on: 1.10 if on else 1.0,
            lambda t, on: 1.0 + (0.5 if on else 0.0),
        )
        self.assertFalse(verdict["evidence"])
        self.assertTrue(
            any(
                "ended 100 s before the observation began" in p
                for p in verdict["evidence_problems"]
            ),
            verdict["evidence_problems"],
        )
        # A pass long enough before the origin is recorded, not a problem.
        built, early = timeline_verdict(
            span,
            [(-1600.0, -1000.0), *passes],
            lambda t, on: 1.10 if on else 1.0,
            lambda t, on: 1.0 + (0.5 if on else 0.0),
        )
        self.assertTrue(early["evidence"], early["evidence_problems"])
        self.assertEqual(built["config"]["pre_origin_intervals"], [[-1600.0, -1000.0]])

    # Finding 5: unequal OFF windows.

    def test_unequal_off_windows_cancel_a_linear_drift(self) -> None:
        # The reviewer's E4: gaps growing 360 to 4800 s and the host's load
        # rising linearly. Each pass adds 2.2; the plain mean of the two
        # OFF levels read the baseline up to 0.06 high.
        gaps, on_s = [360, 600, 1200, 2400, 4800], 400
        passes, t = [], gaps[0]
        for gap in gaps[1:]:
            passes.append((t, t + on_s))
            t += on_s + gap
        slope = 0.95 / t

        def load(time, on):
            return 1.0 + slope * time + (2.2 if on else 0.0)

        _, verdict = timeline_verdict(
            t, passes, lambda _t, on: 1.05 if on else 1.0, load, ewma=True
        )
        for pair in verdict["live_baseline"]["pairs"]:
            self.assertAlmostEqual(pair["d_load1"], 2.2, delta=0.01, msg=pair)


if __name__ == "__main__":
    unittest.main()
