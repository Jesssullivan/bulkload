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
):
    """A deterministic trace: n evenly spaced samples per window. Latency
    cycles through 20 levels (1.00 to 1.95 x base), so every window of one
    base has the same p95 (1.9 x base at n=300) and zero noise. An evidence
    trace follows the evidence protocol (settle 60 s, every window on AC)."""
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
                }
            ]
        windows.append(record)
        for k in range(n):
            ts = t + k * window / n
            spread = 1.0 + (k % 20) / 20.0
            step = 0.0
            for op in s2.OPS:
                factor = (on_factor * op_factor.get(op, 1.0)) if on else 1.0
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
        self.assertEqual(p95s, {19.0})
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
        by_step = s2.analyze(trace)
        self.assertEqual(by_step["status"], "PASS", reasons(by_step))
        self.assertAlmostEqual(by_step["delta_p95"], 0.15, places=6)
        by_op = s2.analyze(trace, {"gate_metric": "each-op"})
        self.assertEqual(by_op["status"], "FAIL")
        self.assertEqual(by_op["worst_metric"], "jsonl")
        self.assertAlmostEqual(by_op["delta_p95"], 0.6, places=6)
        self.assertFalse(by_op["evidence"])

    def test_settle_drops_the_switch_transient(self) -> None:
        trace = synth()
        for window in trace["windows"]:
            if window["state"] == "ON":
                for row in trace["latency"]:
                    if window["start"] <= row["t"] < window["start"] + 20.0:
                        row["ms"] *= 5.0
        self.assertEqual(s2.analyze(trace)["status"], "FAIL")
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

        with mock.patch.object(s2.time, "monotonic", lambda: clock["now"]):
            skipped = s2.cadence(0.0, 0.0, Stop(), 6.0, body)
        self.assertEqual(skipped, 2)
        self.assertEqual([round(c, 6) for c in calls], [0.0, 1.0, 4.0, 5.0])


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
                        'echo "counters priority=$1 priority_from=flag"',
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
            self.assertEqual(run["argv"][-1], "background")
        ops = {row["op"] for row in trace["latency"]}
        self.assertEqual(ops, {"jsonl", "sqlite", "git", "search", "step"})
        self.assertTrue(all(row["ok"] for row in trace["latency"]), trace["latency"])
        self.assertGreaterEqual(len(trace["load"]), 4)
        self.assertIsNone(trace["cut_short"])
        self.assertEqual(trace["errors"], [])
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


if __name__ == "__main__":
    unittest.main()
