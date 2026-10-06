#!/usr/bin/env python3
"""Unit tests for gate_b.py, the S1 gate (b) harness (OI-1003-Q3, OI-1003-Q66).

Run: python3 crates/bulkload-bench/scripts/test_gate_b.py
Everything runs on this host with fakes: a fake source transport, stub
agents and the source helper executed locally. Nothing here reaches neo
(OI-1003-Q66). The end-to-end loopback smoke runs only when GATE_B_AGENT
(a built bulkload-agent) and GATE_B_RCLONE (an rclone binary) are set.
"""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import os
import re
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
REPO = HERE.parents[2]
SPEC = importlib.util.spec_from_file_location("gate_b", HERE / "gate_b.py")
gb = importlib.util.module_from_spec(SPEC)
sys.modules["gate_b"] = gb  # dataclasses resolve annotations through it
SPEC.loader.exec_module(gb)

AGENT_USAGE = re.search(
    r'const USAGE: &str = "\\\n(.*?)";\n',
    (REPO / "crates/bulkload-agent/src/main.rs").read_text(),
    re.S,
).group(1)
NO_PULL_HELP = """bulkload-agent -- ordinary-file transport
SUBCOMMANDS:
    copy SOURCE DEST SOURCE_STATE DEST_STATE
    help        Print this message
"""


def executable(path: Path, text: str) -> Path:
    path.write_text(text)
    path.chmod(path.stat().st_mode | stat.S_IXUSR)
    return path


def conditions(load1: float = 1.0, power: str = "ac") -> dict[str, object]:
    return {"utc": "t", "load1": load1, "power": power, "system": "Darwin"}


def sample(
    arm: str,
    phase: str,
    ms: float,
    *,
    gated: bool = True,
    ok: bool = True,
    rss: int = 100 << 20,
    seq: int = 0,
) -> dict[str, object]:
    row: dict[str, object] = {
        "rep": 0,
        "sequence": seq,
        "arm": arm,
        "phase": phase,
        "elapsed_ms": ms,
        "workload_bytes": 1000,
        "max_rss_bytes": rss,
        "gated": gated,
        "verified": {"ok": ok},
        "conditions_before": {"source": conditions(), "destination": conditions()},
    }
    if arm == "native":
        row["rss_ok"] = rss < gb.RSS_CAP_BYTES
        row["bytes_received"] = 0 if phase == "warm-resume" else 1000
        row["source_bytes_read"] = 0 if phase == "warm-resume" else 1000
        row["content_bytes_read"] = row["source_bytes_read"]
    return row


def rep_samples(
    native: float, rclone: float, delta_native: float, delta_rclone: float, **kw
) -> list:
    out = []
    for seq, arm in enumerate(gb.arm_order(3)):
        out.append(
            sample(arm, "initial", native if arm == "native" else rclone, seq=seq, **kw)
        )
    out.append(sample("native", "warm-resume", 5.0, **kw))
    for seq, arm in enumerate(gb.arm_order(3)):
        out.append(
            sample(
                arm,
                "delta",
                delta_native if arm == "native" else delta_rclone,
                seq=seq,
                **kw,
            )
        )
    return out


def report_with(
    statuses: list[str], mode: str = "gated", status: str = "complete-draft"
) -> dict:
    reps = []
    for index, verdict in enumerate(statuses):
        samples = rep_samples(10, 20, 1, 2)
        v = gb.rep_verdict(samples, "gated")
        v["status"] = verdict
        reps.append(
            {
                "index": index,
                "label": "B",
                "samples": samples,
                "conditions_before": {
                    "source": conditions(),
                    "destination": conditions(),
                },
                "link": {
                    "single": {"bytes_per_s": 1e6},
                    "parallel": {"bytes_per_s": 2e6},
                },
                "verdict": v,
            }
        )
    return {
        "format": gb.FORMAT,
        "date": "2026-10-06",
        "stamp": "2026-10-06-0000Z",
        "mode": mode,
        "status": status,
        "rulings": gb.RULINGS,
        "source": {"host": "neo"},
        "destination": {"work_root": "/w"},
        "workload": {},
        "arms": {},
        "reps": reps,
        "w5_missing": list(gb.W5_MISSING),
    }


class NativeArmTests(unittest.TestCase):
    def test_real_agent_usage_has_the_native_remote_arm(self) -> None:
        self.assertIn(gb.PULL_USAGE, AGENT_USAGE)
        self.assertIsNone(gb.native_capability(AGENT_USAGE, 1))

    def test_missing_pull_is_a_typed_refusal(self) -> None:
        refusal = gb.native_capability(NO_PULL_HELP, 1)
        self.assertEqual(refusal.code, "NATIVE_REMOTE_ARM_MISSING")
        self.assertIn("native remote arm missing (#47)", refusal.reason)

    def test_w5_streams_are_refused_not_built(self) -> None:
        refusal = gb.native_capability(AGENT_USAGE, 4)
        self.assertEqual(refusal.code, "NATIVE_REMOTE_ARM_MISSING")
        self.assertIn("#47", refusal.reason)
        self.assertIn("one `ssh -T` stream", refusal.reason)

    def test_main_refuses_cleanly_when_the_native_arm_is_missing(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            agent = executable(
                Path(tmp) / "agent", f"#!/bin/sh\ncat <<'EOF'\n{NO_PULL_HELP}EOF\n"
            )
            work = Path(tmp) / "work"
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                code = gb.main(
                    ["--dry-run", "--work-root", str(work), "--agent", str(agent)]
                )
            self.assertEqual(code, 2)
            self.assertIn("refused code=NATIVE_REMOTE_ARM_MISSING", out.getvalue())
            report = json.loads((work / "gate-b.json").read_text())
            self.assertEqual(report["status"], "refused")
            self.assertEqual(report["refusal"]["code"], "NATIVE_REMOTE_ARM_MISSING")
            self.assertIn("native remote arm missing (#47)", report["reason"])
            self.assertEqual(report["gate"]["verdict"], "NOT A GATE SAMPLE")
            self.assertEqual(report["schema_problems"], [])
            self.assertFalse(list(work.glob("*.md")))

    def test_missing_agent_binary_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            text, refusal = gb.probe_agent(Path(tmp) / "absent", 1)
            self.assertEqual(text, "")
            self.assertEqual(refusal.code, "NATIVE_REMOTE_ARM_MISSING")

    def test_w5_gap_list_names_the_missing_pieces(self) -> None:
        joined = " ".join(gb.W5_MISSING)
        for piece in ("N parallel streams", "zstd", "`Ref`", "`NeedRanges`", "RSS"):
            self.assertIn(piece, joined)


class PreflightTests(unittest.TestCase):
    def args(self, tmp: str, *extra: str) -> object:
        return gb.parser().parse_args(
            ["--work-root", f"{tmp}/w", "--agent", "/a", *extra]
        )

    def full(self, tmp: str, *extra: str) -> object:
        return self.args(
            tmp,
            "--source-corpus", "/c",
            "--source-work", "/neo/work",
            "--remote-agent", "/neo/agent",
            "--source-repo", "/neo/repo",
            *extra,
        )  # fmt: skip

    def test_gated_needs_quiet_lanes_and_the_full_shape(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            self.assertEqual(gb.preflight(self.full(tmp), "gated").code, "QUIET")
            quiet = ("--coordinator-quiet",)
            self.assertIsNone(gb.preflight(self.full(tmp, *quiet), "gated"))
            self.assertEqual(
                gb.preflight(self.full(tmp, *quiet, "--reps", "2"), "gated").code,
                "SHAPE",
            )
            self.assertEqual(
                gb.preflight(self.full(tmp, *quiet, "--scale", "small"), "gated").code,
                "SCALE",
            )

    def test_gated_refuses_a_raised_destination_load_limit(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            quiet = ("--coordinator-quiet",)
            for value in ("1000", "2.51", "0", "-1", "nan"):
                raised = self.full(tmp, *quiet, "--dest-load-limit", value)
                refusal = gb.preflight(raised, "gated")
                self.assertEqual(refusal.code, "LOAD_LIMIT", value)
                self.assertIn("R-N81", refusal.reason)
            for value in ("2.5", "1.0"):
                tight = self.full(tmp, *quiet, "--dest-load-limit", value)
                self.assertIsNone(gb.preflight(tight, "gated"), value)
            lifted = self.full(tmp, "--under-load", "--dest-load-limit", "1000")
            self.assertIsNone(gb.preflight(lifted, "under-load"))

    def test_under_load_does_not_need_quiet_lanes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            self.assertIsNone(
                gb.preflight(self.full(tmp, "--under-load"), "under-load")
            )

    def test_source_paths_must_be_absolute_and_pull_safe(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            self.assertEqual(gb.preflight(self.args(tmp), "gated").code, "SOURCE_ARGS")
            spaced = self.args(
                tmp,
                "--source-corpus", "/c",
                "--source-work", "/neo/my work",
                "--remote-agent", "/a",
                "--source-repo", "/r",
            )  # fmt: skip
            self.assertEqual(gb.preflight(spaced, "gated").code, "SOURCE_ARGS")
            relative = self.full(tmp, "--ssh-config", "cfg")
            self.assertEqual(gb.preflight(relative, "gated").code, "SOURCE_ARGS")

    def test_work_root_must_be_new_and_modes_exclusive(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            args = gb.parser().parse_args(["--work-root", tmp, "--agent", "/a"])
            self.assertEqual(gb.preflight(args, "dry-run").code, "WORK_ROOT")
            both = self.args(tmp, "--dry-run", "--under-load")
            self.assertEqual(gb.preflight(both, "dry-run").code, "MODE")

    def test_evidence_targets(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo, work = Path(tmp), Path(tmp) / "w"
            args = self.args(tmp)
            path, refusal = gb.evidence_target(args, "gated", repo, work, "S")
            self.assertIsNone(refusal)
            self.assertEqual(path.name, "s1-gate-b-S.md")
            path, _ = gb.evidence_target(args, "under-load", repo, work, "S")
            self.assertIn("underload", path.name)
            path, _ = gb.evidence_target(args, "dry-run", repo, work, "S")
            self.assertEqual(path.parent, work)
            bad = self.args(tmp, "--evidence", f"{tmp}/docs/evidence/x.md")
            _, refusal = gb.evidence_target(bad, "dry-run", repo, work, "S")
            self.assertEqual(refusal.code, "EVIDENCE")
            gate_name = self.args(tmp, "--evidence", f"{tmp}/s1-gate-b.md")
            _, refusal = gb.evidence_target(gate_name, "under-load", repo, work, "S")
            self.assertEqual(refusal.code, "EVIDENCE")


class GatingTests(unittest.TestCase):
    def test_gated_needs_source_ac_and_both_loads(self) -> None:
        ok = gb.host_problems(
            conditions(1.0), conditions(1.0, "unknown-no-supply-class"), "gated", 2.5
        )
        self.assertEqual(ok, [])
        battery = gb.host_problems(
            conditions(1.0, "battery"), conditions(), "gated", 2.5
        )
        self.assertIn("source power=battery", battery)
        busy_source = gb.host_problems(conditions(2.5), conditions(), "gated", 2.5)
        self.assertTrue(any("source load1" in p for p in busy_source))
        busy_dest = gb.host_problems(conditions(), conditions(4.0), "gated", 8.0)
        self.assertEqual(busy_dest, [])
        busy_dest = gb.host_problems(conditions(), conditions(9.0), "gated", 8.0)
        self.assertTrue(any("destination load1" in p for p in busy_dest))
        dest_battery = gb.host_problems(
            conditions(), conditions(1.0, "battery"), "gated", 2.5
        )
        self.assertIn("destination on battery power", dest_battery)

    def test_a_sample_is_gated_only_under_the_fixed_limit_on_both_hosts(self) -> None:
        self.assertTrue(gb.sample_gated(conditions(1.0), conditions(2.49)))
        self.assertFalse(gb.sample_gated(conditions(1.0), conditions(64.0)))
        self.assertFalse(gb.sample_gated(conditions(1.0), conditions(2.5)))
        self.assertFalse(gb.sample_gated(conditions(2.5), conditions(1.0)))
        self.assertFalse(gb.sample_gated(conditions(1.0, "battery"), conditions(1.0)))
        # The scenario from review: a raised limit lets await_ready through,
        # but the samples it lets through are not gated, so no rep can pass.
        self.assertEqual(
            gb.host_problems(conditions(1.0), conditions(64.0), "gated", 1000.0), []
        )
        busy = rep_samples(10, 20, 1, 2)
        for row in busy:
            row["gated"] = gb.sample_gated(conditions(1.0), conditions(64.0))
        self.assertEqual(gb.rep_verdict(busy, "gated")["status"], "informational")

    def test_under_load_keeps_only_power(self) -> None:
        self.assertEqual(
            gb.host_problems(conditions(9.0), conditions(9.0), "under-load", 2.5), []
        )
        self.assertTrue(
            gb.host_problems(
                conditions(9.0, "battery"), conditions(), "under-load", 2.5
            )
        )

    def test_dry_run_is_not_gated(self) -> None:
        self.assertEqual(
            gb.host_problems(conditions(9, "battery"), conditions(9), "dry-run", 2.5),
            [],
        )

    def test_await_ready_aborts_when_the_source_never_settles(self) -> None:
        transport = mock.Mock()
        transport.helper.return_value = conditions(3.0)
        ctx = mock.Mock(transport=transport, mode="gated")
        ctx.args.dest_load_limit = 2.5
        clock = iter([0.0, 0.0, 100.0, 400.0])
        with (
            mock.patch.object(gb, "dest_conditions", return_value=conditions()),
            mock.patch.object(gb.time, "monotonic", side_effect=lambda: next(clock)),
            mock.patch.object(gb.time, "sleep"),
        ):
            with self.assertRaises(gb.Abort) as raised:
                gb.await_ready(ctx, 300, "rep0 start")
        self.assertIn("source load1=3.0", str(raised.exception))

    def test_await_ready_waits_for_a_settling_source(self) -> None:
        transport = mock.Mock()
        transport.helper.side_effect = [conditions(3.0), conditions(1.0)]
        ctx = mock.Mock(transport=transport, mode="gated")
        ctx.args.dest_load_limit = 2.5
        with (
            mock.patch.object(gb, "dest_conditions", return_value=conditions()),
            mock.patch.object(gb.time, "sleep") as slept,
        ):
            source, _ = gb.await_ready(ctx, 300, "arm")
        self.assertEqual(source["load1"], 1.0)
        slept.assert_called_once()


class VerdictTests(unittest.TestCase):
    def test_rep_passes_only_when_native_wins_both_phases(self) -> None:
        v = gb.rep_verdict(rep_samples(10, 20, 1, 2), "gated")
        self.assertEqual(v["status"], "pass")
        self.assertTrue(v["r25_warm_zero"])
        self.assertEqual(v["medians"]["initial"], {"native_ms": 10, "rclone_ms": 20})
        self.assertEqual(
            gb.rep_verdict(rep_samples(10, 20, 3, 2), "gated")["status"], "fail"
        )
        self.assertEqual(
            gb.rep_verdict(rep_samples(30, 20, 1, 2), "gated")["status"], "fail"
        )

    def test_a_warm_resume_that_moves_or_reads_bytes_fails_the_rep(self) -> None:
        for field in ("bytes_received", "content_bytes_read"):
            samples = rep_samples(10, 20, 1, 2)
            warm = next(s for s in samples if s["phase"] == "warm-resume")
            warm[field] = 10**9
            v = gb.rep_verdict(samples, "gated")
            self.assertFalse(v["r25_warm_zero"], field)
            self.assertEqual(v["status"], "fail", field)
        missing = [s for s in rep_samples(10, 20, 1, 2) if s["phase"] != "warm-resume"]
        self.assertEqual(gb.rep_verdict(missing, "gated")["status"], "fail")
        report = report_with(["pass"] * 3)
        for rep in report["reps"]:
            warm = next(s for s in rep["samples"] if s["phase"] == "warm-resume")
            warm["bytes_received"] = warm["content_bytes_read"] = 10**9
            rep["verdict"] = gb.rep_verdict(rep["samples"], "gated")
        self.assertEqual(gb.gate_rollup(report)["verdict"], "FAIL")

    def test_the_missing_interrupted_resume_is_stated_not_passed(self) -> None:
        v = gb.rep_verdict(rep_samples(10, 20, 1, 2), "gated")
        self.assertIsNone(v["r25_interrupted_zero"])
        self.assertTrue(v["r25_interrupted_resume"].startswith("not-run"))
        rollup = gb.gate_rollup(report_with(["pass"] * 3))
        self.assertIn("warm resume", rollup["rule"])
        self.assertIn("no interrupted-resume phase", rollup["rule"])
        self.assertIn("Unratified deviation from gate (a)", rollup["rule"])
        self.assertEqual(rollup["deviations_from_gate_a"], list(gb.DEVIATIONS))
        report = report_with(["pass"] * 3)
        report["gate"] = rollup
        text = gb.evidence(report)
        self.assertIn("Deviations from gate (a)", text)
        self.assertIn("Interrupted resume: not-run", text)

    def test_rss_cap_and_verification_decide_too(self) -> None:
        heavy = gb.rep_verdict(rep_samples(10, 20, 1, 2, rss=3 << 30), "gated")
        self.assertEqual(heavy["status"], "fail")
        self.assertFalse(heavy["native_rss_below_2gib"])
        broken = gb.rep_verdict(rep_samples(10, 20, 1, 2, ok=False), "gated")
        self.assertEqual(broken["status"], "fail")

    def test_ungated_samples_and_non_gated_modes_are_informational(self) -> None:
        self.assertEqual(
            gb.rep_verdict(rep_samples(10, 20, 1, 2, gated=False), "gated")["status"],
            "informational",
        )
        self.assertEqual(
            gb.rep_verdict(rep_samples(10, 20, 1, 2), "under-load")["status"],
            "informational",
        )

    def test_rollup_needs_three_passing_b_reps(self) -> None:
        self.assertEqual(gb.gate_rollup(report_with(["pass"] * 3))["verdict"], "PASS")
        self.assertEqual(
            gb.gate_rollup(report_with(["pass", "fail", "pass"]))["verdict"], "FAIL"
        )
        self.assertTrue(
            gb.gate_rollup(report_with(["pass"] * 2))["verdict"].startswith("NONE")
        )
        aborted = report_with(["pass"] * 3, status="aborted")
        self.assertTrue(gb.gate_rollup(aborted)["verdict"].startswith("NONE"))
        self.assertEqual(
            gb.gate_rollup(report_with(["informational"], mode="dry-run"))["verdict"],
            "NOT A GATE SAMPLE",
        )
        under = report_with(
            ["informational"] * 3, "under-load", "complete-under-load-informational"
        )
        rollup = gb.gate_rollup(under)
        self.assertIn("NOT A GATE SAMPLE", rollup["verdict"])
        self.assertIn("informational x3", rollup["verdict"])
        self.assertIn(
            "no wall-clock SLA", gb.gate_rollup(report_with(["pass"] * 3))["rule"]
        )

    def test_link_fraction(self) -> None:
        link = {"single": {"bytes_per_s": 1e6}, "parallel": {"bytes_per_s": 4e6}}
        self.assertEqual(gb.link_fraction(4_000_000, 2000.0, link), 0.5)
        self.assertIsNone(gb.link_fraction(1, 0.0, link))
        self.assertIsNone(gb.link_fraction(1, 1.0, {}))


class DiskBudgetTests(unittest.TestCase):
    GIB = 1 << 30

    def test_floor_matches_the_agents_default(self) -> None:
        source = (REPO / "crates/bulkload-agent/src/space.rs").read_text()
        found = re.search(r"DEFAULT_MIN_FREE_PERCENT: u8 = (\d+);", source)
        self.assertEqual(int(found.group(1)), gb.AGENT_MIN_FREE_PERCENT)

    def test_copies_held_at_once(self) -> None:
        self.assertEqual(gb.destination_copies("gated", 3, 3, False), 5)
        self.assertEqual(gb.destination_copies("gated", 3, 3, True), 15)
        self.assertEqual(gb.destination_copies("dry-run", 1, 3, False), 6)

    def test_exactly_at_the_floor_passes_and_one_byte_under_refuses(self) -> None:
        # space.rs: 100 GiB filesystem, 40 GiB available, 25 % floor = 25 GiB.
        slack = gb.BUDGET_SLACK_BYTES
        fits = gb.disk_budget(100 * self.GIB, 40 * self.GIB, 5, 3 * self.GIB, 0)
        self.assertEqual(fits["need_bytes"], 15 * self.GIB + slack)
        self.assertFalse(fits["ok"])
        room = 40 * self.GIB + slack
        self.assertTrue(gb.disk_budget(100 * self.GIB, room, 5, 3 * self.GIB, 0)["ok"])
        self.assertFalse(
            gb.disk_budget(100 * self.GIB, room - 1, 5, 3 * self.GIB, 0)["ok"]
        )
        entries = gb.disk_budget(100 * self.GIB, room, 5, 3 * self.GIB, 1)
        self.assertEqual(entries["per_copy_bytes"], 3 * self.GIB + gb.BUDGET_BLOCK)
        self.assertFalse(entries["ok"])

    def test_the_reviewed_volumes_are_refused(self) -> None:
        # /srv/cache at 20.6 % free: under the floor before a byte is written.
        low = gb.disk_budget(1000 * self.GIB, 206 * self.GIB, 6, 10_000_000, 60)
        self.assertFalse(low["ok"])
        self.assertEqual(low["free_ratio"], 0.206)
        # 15 kept copies of a 4.19 GB set on a volume with 22 GiB available.
        kept = gb.disk_budget(78 * self.GIB, 22 * self.GIB, 15, 4_190_000_000, 0)
        self.assertFalse(kept["ok"])
        self.assertLess(kept["free_ratio_after"], 0)
        refusal = gb.budget_refusal(kept, Path("/w"))
        self.assertEqual(refusal.code, "DEST_SPACE")
        self.assertIn("25 % free floor", refusal.reason)
        self.assertIsNone(gb.budget_refusal({"ok": True}, Path("/w")))
        self.assertFalse(gb.disk_budget(0, 0, 1, 1, 1)["ok"])

    def test_volume_space_reads_this_filesystem(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            total, available = gb.volume_space(Path(tmp))
        self.assertGreater(total, 0)
        self.assertLessEqual(available, total)

    def test_release_removes_only_the_reps_own_arm_directories(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            rep_dir = Path(tmp) / "reps" / "rep0"
            order = gb.arm_order(3)
            for seq, arm in enumerate(order):
                (rep_dir / f"{seq}-{arm}" / "destination" / "d").mkdir(parents=True)
                (rep_dir / f"{seq}-{arm}" / "destination" / "d" / "f").write_bytes(b"x")
            outside = Path(tmp) / "outside"
            outside.write_bytes(b"keep")
            (rep_dir / "0-native" / "destination" / "link").symlink_to(outside)
            (rep_dir / "note").write_text("kept")
            rep: dict[str, object] = {"index": 0}
            gb.release_destinations(rep_dir, order, rep)
            self.assertTrue(rep["destinations_released"])
            self.assertEqual([p.name for p in rep_dir.iterdir()], ["note"])
            self.assertEqual(outside.read_bytes(), b"keep")
            again: dict[str, object] = {"index": 0}
            with self.assertRaises(gb.Abort) as raised:
                gb.release_destinations(rep_dir, order, again)
            self.assertFalse(again["destinations_released"])
            self.assertIs(raised.exception.rep, again)

    def test_main_refuses_before_the_source_copy_when_the_volume_is_short(
        self,
    ) -> None:
        if not any(
            os.path.isfile(p)
            for p in (
                "/usr/libexec/openssh/sftp-server",
                "/usr/lib/openssh/sftp-server",
                "/usr/libexec/sftp-server",
            )
        ) and not gb.shutil.which("sftp-server"):
            self.skipTest("no local sftp-server for the loopback")
        with tempfile.TemporaryDirectory() as tmp:
            agent = executable(
                Path(tmp) / "agent", f"#!/bin/sh\ncat <<'EOF'\n{AGENT_USAGE}\nEOF\n"
            )
            work = Path(tmp) / "work"
            out = io.StringIO()
            with (
                contextlib.redirect_stdout(out),
                mock.patch.object(
                    gb, "volume_space", return_value=(1000 * self.GIB, 206 * self.GIB)
                ),
            ):
                code = gb.main(
                    ["--dry-run", "--work-root", str(work), "--agent", str(agent)]
                )
            self.assertEqual(code, 2, out.getvalue())
            self.assertIn("refused code=DEST_SPACE", out.getvalue())
            report = json.loads((work / "gate-b.json").read_text())
            self.assertEqual(report["refusal"]["code"], "DEST_SPACE")
            disk = report["destination"]["disk"]
            self.assertEqual((disk["free_ratio"], disk["ok"]), (0.206, False))
            self.assertEqual(disk["copies"], 6)
            self.assertGreater(disk["measured"]["comparable_bytes"], 0)
            self.assertEqual(disk["measured"]["excluded"], 3)
            # Refused before `prepare`: no working copy was made.
            self.assertFalse((work / "source").exists())
            self.assertEqual(report["schema_problems"], [])


class SchemaTests(unittest.TestCase):
    def test_a_complete_report_conforms(self) -> None:
        report = report_with(["pass"] * 3)
        report["gate"] = gb.gate_rollup(report)
        self.assertEqual(gb.validate_report(report), [])
        self.assertIn("W5 (#47)", gb.evidence(report))
        self.assertIn("PASS", gb.evidence(report))

    def test_missing_and_mistyped_fields_are_named(self) -> None:
        report = report_with(["pass"])
        report["gate"] = gb.gate_rollup(report)
        del report["rulings"]
        report["reps"][0]["samples"][0]["max_rss_bytes"] = True
        report["reps"][0]["samples"][1]["arm"] = "rsync"
        problems = gb.validate_report(report)
        self.assertIn("report.rulings is not <class 'str'>", problems)
        self.assertTrue(any("samples[0].max_rss_bytes" in p for p in problems))
        self.assertTrue(any("samples[1].arm invalid" in p for p in problems))

    def test_an_unredacted_secret_is_a_schema_problem(self) -> None:
        report = report_with(["pass"])
        report["gate"] = gb.gate_rollup(report)
        report["arms"] = {"note": "--sftp-pass hunter2"}
        self.assertIn("report holds an unredacted secret", gb.validate_report(report))
        self.assertEqual(gb.validate_report(gb.scrub(report)), [])

    def test_finish_scrubs_and_writes_json_and_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            report = report_with(["pass"] * 3)
            report["reason"] = "pass = hunter2"
            evidence = Path(tmp) / "e.md"
            with contextlib.redirect_stdout(io.StringIO()):
                gb.finish(report, Path(tmp), evidence)
            text = (Path(tmp) / "gate-b.json").read_text()
            self.assertNotIn("hunter2", text)
            self.assertEqual(json.loads(text)["schema_problems"], [])
            self.assertNotIn("hunter2", evidence.read_text())


class RedactionTests(unittest.TestCase):
    def test_redact_known_secret_shapes(self) -> None:
        pem = "-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----"
        cases = {
            f"x {pem} y": "hidden",
            "pass = hunter2": "hunter2",
            "  key_pem = abc": "abc",
            "--sftp-pass hunter2": "hunter2",
            "--sftp-key-file-pass=hunter2": "hunter2",
            "RCLONE_CONFIG_PASS=hunter2 rclone": "hunter2",
            "RCLONE_SFTP_PASS=hunter2": "hunter2",
            "sftp://jess:hunter2@neo/x": "hunter2",
        }
        for text, secret in cases.items():
            scrubbed = gb.redact(text)
            self.assertNotIn(secret if secret != "hidden" else "AAAA", scrubbed, text)
            self.assertIn("[REDACTED", scrubbed)

    def test_redact_leaves_ordinary_lines(self) -> None:
        for text in (
            "completed=3 reused=0 bytes_received=10 refusals=0",
            "ssh = /usr/bin/ssh -F /home/x/.ssh/config -oBatchMode=yes neo",
            "key_file = /home/x/.ssh/id_ed25519",
            "b_reps_pass=3",
        ):
            self.assertEqual(gb.redact(text), text)

    def test_redact_argv_hides_the_value_after_a_secret_flag(self) -> None:
        argv = ["rclone", "copy", "--sftp-pass", "hunter2", "a", "b"]
        self.assertEqual(
            gb.redact_argv(argv),
            ["rclone", "copy", "--sftp-pass", "[REDACTED]", "a", "b"],
        )

    def test_external_remote_holds_no_secret(self) -> None:
        transport = gb.Transport(
            "/usr/bin/ssh", "neo", "/home/x/.ssh/config", "/usr/bin/python3"
        )
        remote, refusal = gb.rclone_remote(transport, "external", None, {})
        self.assertIsNone(refusal)
        self.assertEqual(
            remote["ssh"], "/usr/bin/ssh -F /home/x/.ssh/config -oBatchMode=yes neo"
        )
        self.assertEqual(remote["shell_type"], "none")
        self.assertEqual(remote["skip_links"], "true")
        self.assertFalse(set(remote) & set(gb.SECRET_KEYS))
        text = gb.rclone_config_text(remote)
        self.assertEqual(gb.redact(text), text)
        self.assertTrue(text.startswith("[gateb-src]\ntype = sftp\n"))

    def test_internal_remote_from_ssh_g(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            known = Path(tmp) / "known_hosts"
            known.write_text("")
            key = Path(tmp) / "id_ed25519"
            key.write_text("")
            resolved = gb.parse_ssh_g(
                f"hostname neo.lan\nuser jess\nport 2222\n"
                f"userknownhostsfile {tmp}/absent {known}\nidentityfile {key}\n"
            )
            transport = gb.Transport("ssh", "neo", None, "python3")
            remote, refusal = gb.rclone_remote(transport, "internal", resolved, {})
            self.assertIsNone(refusal)
            self.assertEqual(
                remote,
                {
                    "type": "sftp",
                    "host": "neo.lan",
                    "user": "jess",
                    "port": "2222",
                    "known_hosts_file": str(known),
                    "skip_links": "true",
                    "key_file": str(key),
                },
            )
            agent, _ = gb.rclone_remote(
                transport, "internal", resolved, {"SSH_AUTH_SOCK": "/s"}
            )
            self.assertEqual(agent["key_use_agent"], "true")
            self.assertNotIn("key_file", agent)
            no_known = gb.parse_ssh_g(
                f"hostname neo\nuserknownhostsfile {tmp}/absent\n"
            )
            _, refusal = gb.rclone_remote(transport, "internal", no_known, {})
            self.assertEqual(refusal.code, "HOST_KEY_UNVERIFIED")

    def test_rclone_env_drops_inherited_rclone_variables(self) -> None:
        with mock.patch.dict(os.environ, {"RCLONE_CONFIG_PASS": "x", "HOME": "/h"}):
            env = gb.rclone_env()
        self.assertNotIn("RCLONE_CONFIG_PASS", env)
        self.assertEqual(env["HOME"], "/h")

    def test_private_files_are_0600(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "rclone.conf"
            gb.write_private(path, "x")
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)


class ArmTests(unittest.TestCase):
    def test_rclone_glob_escapes_and_anchors(self) -> None:
        self.assertEqual(gb.rclone_glob("a/b.db"), "/a/b.db")
        self.assertEqual(
            gb.rclone_glob("s/odd [x]{y}*?.db"), r"/s/odd \[x\]\{y\}\*\?.db"
        )
        self.assertEqual(gb.rclone_glob("back\\slash"), "/back\\\\slash")

    def test_argv_shapes(self) -> None:
        native = gb.native_argv(
            "/a", "neo", "/src", Path("/d"), "/ss", Path("/ds"), "/w", "/cfg"
        )
        self.assertEqual(
            native, ["/a", "pull", "neo", "/src", "/d", "/ss", "/ds", "/w", "/cfg"]
        )
        rclone = gb.rclone_argv("rclone", Path("/c"), "/src", Path("/d"), Path("/x"))
        self.assertEqual(rclone[:4], ["rclone", "copy", "gateb-src:/src", "/d"])
        self.assertIn("--exclude-from", rclone)
        self.assertEqual(rclone[-len(gb.RCLONE_FLAGS) :], list(gb.RCLONE_FLAGS))

    def test_transport_argv_quotes_the_remote_command(self) -> None:
        t = gb.Transport("ssh", "neo", "/cfg", "/usr/bin/python3")
        self.assertEqual(
            t.argv(["dd", "if=/dev/zero"], extra=("-oCompression=no",)),
            ["ssh", "-F", "/cfg", "-T", "-oBatchMode=yes", "-oConnectTimeout=15",
             "-oCompression=no", "--", "neo", "dd if=/dev/zero"],
        )  # fmt: skip

    def test_parse_native_and_expected_refusals(self) -> None:
        stdout = (
            "completed=4 reused=0 bytes_received=100 source_bytes_read=120 refusals=1 "
            "source_engine_temporaries=0 capped_subtrees=0\n"
            "counters verb=pull side=destination scope=process priority=normal x=1\n"
        )
        stderr = (
            "transfer_timing verb=serve side=source scope=process priority=background walk_ns=5\n"
            "counters verb=serve side=source scope=process priority=background priority_from=default\n"
            "gate_b_serve max_rss_bytes=4096 exit=0\n"
            "refused state/odd\\x20\\xff.db: SQLITE_STATE_CHANGED\n"
            "bulkload-agent: refused: CONTRACT_SELF_INCONSISTENT\n"
        )
        parsed = gb.parse_native(stdout, stderr)
        odd = os.fsdecode(b"state/odd \xff.db")
        self.assertEqual(parsed["refusals"], [[odd, "SQLITE_STATE_CHANGED"]])
        self.assertEqual(parsed["serve_max_rss_bytes"], 4096)
        self.assertEqual(parsed["serve"]["priority"], "background")
        self.assertEqual(parsed["transfer"]["bytes_received"], 100)
        self.assertEqual(parsed["final_refusal"], "CONTRACT_SELF_INCONSISTENT")
        run = {"exit": 1}
        self.assertEqual(gb.native_problems(run, parsed, {odd: "sqlite-magic"}), [])
        problems = gb.native_problems(run, parsed, {})
        self.assertTrue(any("unexpected refusals" in p for p in problems))
        problems = gb.native_problems(
            run, parsed, {odd: "x", "other.db": "sqlite-magic"}
        )
        self.assertTrue(any("the sets disagree" in p for p in problems))
        clean = gb.parse_native(stdout.replace("refusals=1", "refusals=0"), "")
        self.assertEqual(gb.native_problems({"exit": 0}, clean, {}), [])
        self.assertTrue(gb.native_problems({"exit": 1}, clean, {}))
        self.assertTrue(gb.native_problems({"exit": 0}, gb.parse_native("", ""), {}))

    def test_unescape_ascii(self) -> None:
        self.assertEqual(gb.unescape_ascii(r"a\tb\\c\x41\'"), "a\tb\\cA'")

    def test_verify_tree(self) -> None:
        expected = [
            ["d", "d", 0o755, 0, ""],
            ["d/a", "f", 0o644, 3, "h1"],
            ["d/b.db", "f", 0o644, 9, "h2"],
            ["d/l", "l", 0o777, 0, "a"],
        ]
        excl = {"d/b.db": "sqlite-magic"}
        same = [row for row in expected if row[0] != "d/b.db"]
        self.assertTrue(gb.verify_tree(expected, excl, same)["ok"])
        wrong = [["d", "d", 0o755, 0, ""], ["d/a", "f", 0o644, 3, "XX"]]
        result = gb.verify_tree(expected, excl, wrong)
        self.assertFalse(result["ok"])
        self.assertEqual(result["wrong_content"], 1)
        self.assertEqual(result["symlink_mismatch"], 1)
        self.assertFalse(
            gb.verify_tree(expected, excl, [["d/a", "f", 0o644, 3, "h1"]])["ok"]
        )
        extra = [*same, ["d/l.rclonelink", "f", 0o644, 1, "z"]]
        self.assertEqual(gb.verify_tree(expected, excl, extra)["extra_entries"], 1)

    def test_delta_plan_is_one_percent_and_deterministic(self) -> None:
        rows = [[f"f{i:03}", "f", 0o644, 1000 + i, "h"] for i in range(200)]
        rows.append(["db", "f", 0o644, 10**6, "h"])
        excl = {"db": "sqlite-magic"}
        total = gb.comparable_bytes(rows, excl)
        plan = gb.delta_plan(rows, excl)
        self.assertEqual(sum(p[1] for p in plan), -(-total // 100))
        self.assertNotIn("db", [p[0] for p in plan])
        self.assertEqual(plan, gb.delta_plan(list(reversed(rows)), excl))
        self.assertNotEqual(plan, gb.delta_plan(rows, excl, seed="other"))

    def test_sqlite_probe_reads_are_netted_out_of_r25(self) -> None:
        rows = [
            ["a.db", "f", 0o644, 4096, "h"],
            ["tiny.db", "f", 0o644, 4, "h"],
            ["a.db-wal", "f", 0o644, 4096, "h"],
            ["x", "f", 0o644, 10, "h"],
        ]
        excl = {
            "a.db": "sqlite-magic",
            "tiny.db": "sqlite-magic",
            "a.db-wal": "sqlite-companion-name",
        }
        self.assertEqual(gb.probe_bytes(rows, excl), 20)
        self.assertEqual(gb.content_bytes_read(20, 20), 0)
        self.assertEqual(gb.content_bytes_read(52, 20), 32)
        self.assertIsNone(gb.content_bytes_read(None, 20))

    def test_arm_order(self) -> None:
        self.assertEqual(
            gb.arm_order(3), ["native", "rclone", "native", "rclone", "native"]
        )


class HelperTests(unittest.TestCase):
    """The source helper, executed locally (the same text neo would run)."""

    def tree(self, root: Path) -> None:
        (root / "d").mkdir(parents=True)
        (root / "d" / "a.txt").write_bytes(b"x" * 5000)
        (root / "d" / "s.db").write_bytes(b"SQLite format 3\0" + b"\0" * 100)
        (root / "d" / "s.db-journal").write_bytes(b"")
        (root / "d" / "w").write_bytes(b"\x37\x7f\x06\x83rest")
        (root / "d" / "link").symlink_to("a.txt")
        (root / "empty").mkdir()

    def test_manifest_exclusions_and_xor_round_trip(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "c"
            self.tree(root)
            out = gb.HELPER["op_manifest"]({"root": str(root)})
            kinds = {row[0]: row[1] for row in out["rows"]}
            self.assertEqual(kinds["d/link"], "l")
            self.assertEqual(kinds["empty"], "d")
            self.assertEqual(
                out["exclusions"],
                {
                    "d/s.db": "sqlite-magic",
                    "d/s.db-journal": "sqlite-companion-name",
                    "d/w": "sqlite-magic",
                },
            )
            plan = [["d/a.txt", 50]]
            gb.HELPER["op_xor"]({"root": str(root), "plan": plan})
            mutated = gb.HELPER["op_manifest"]({"root": str(root)})
            self.assertEqual(
                gb.changed_paths(out["rows"], mutated["rows"]), {"d/a.txt"}
            )
            self.assertEqual(
                (root / "d" / "a.txt").read_bytes()[:1], bytes([ord("x") ^ 0xA5])
            )
            gb.HELPER["op_xor"]({"root": str(root), "plan": plan})
            self.assertEqual(
                gb.HELPER["op_manifest"]({"root": str(root)})["digest"], out["digest"]
            )

    def test_measure_agrees_with_the_manifest_without_hashing(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            sealed = Path(tmp) / "sealed"
            self.tree(sealed / "corpus")
            measured = gb.HELPER["op_measure"]({"corpus": str(sealed)})
            out = gb.HELPER["op_manifest"]({"root": str(sealed / "corpus")})
            self.assertEqual(
                measured["comparable_bytes"],
                gb.comparable_bytes(out["rows"], out["exclusions"]),
            )
            self.assertEqual(
                measured,
                {
                    "comparable_files": 1,
                    "comparable_bytes": 5000,
                    "directories": 2,
                    "others": 1,
                    "excluded": 3,
                    "excluded_bytes": 116 + 0 + 8,
                },
            )
            with self.assertRaises(RuntimeError):
                gb.HELPER["op_measure"]({"corpus": str(sealed / "absent")})

    def test_xor_refuses_a_target_outside_the_copy(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "c"
            self.tree(root)
            with self.assertRaises(RuntimeError):
                gb.HELPER["op_xor"]({"root": str(root), "plan": [["d/link", 1]]})

    def test_prepare_makes_a_writable_copy_and_a_measuring_wrapper(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            sealed = Path(tmp) / "sealed"
            self.tree(sealed / "corpus")
            for dirpath, dirs, files in os.walk(sealed / "corpus", topdown=False):
                for name in files:
                    p = Path(dirpath) / name
                    if not p.is_symlink():
                        p.chmod(0o444)
                Path(dirpath).chmod(0o555)
            agent = executable(
                Path(tmp) / "agent", '#!/bin/sh\necho "agent $*"\nexit 3\n'
            )
            work = Path(tmp) / "work"
            try:
                out = gb.HELPER["op_prepare"](
                    {
                        "corpus": str(sealed),
                        "work": str(work),
                        "python": sys.executable,
                        "agent": str(agent),
                        "flags": ["--priority=normal"],
                    }
                )
                copy = Path(out["corpus"])
                self.assertEqual(
                    stat.S_IMODE((copy / "d" / "a.txt").stat().st_mode), 0o644
                )
                self.assertTrue((copy / "d" / "link").is_symlink())
                result = subprocess.run(
                    [out["wrapper"], "serve"],
                    capture_output=True,
                    text=True,
                    check=False,
                )
                self.assertEqual(result.returncode, 3)
                self.assertEqual(result.stdout.strip(), "agent --priority=normal serve")
                rss = gb.ab.pairs(result.stderr.strip().splitlines()[-1])
                self.assertGreater(rss["max_rss_bytes"], 0)
                with self.assertRaises(RuntimeError):
                    gb.HELPER["op_prepare"]({"corpus": str(sealed), "work": str(work)})
            finally:
                for dirpath, _dirs, _files in os.walk(sealed):
                    Path(dirpath).chmod(0o755)

    def test_transport_helper_through_a_loopback_shim(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            shim = executable(
                Path(tmp) / "ssh", gb.LOOPBACK_SHIM.format(sftp_server="/bin/false")
            )
            transport = gb.Transport(str(shim), "loop", None, sys.executable)
            out = transport.helper("conditions", {})
            self.assertIn("load1", out)
            with self.assertRaises(gb.Abort) as raised:
                transport.helper("manifest", {"root": f"{tmp}/absent/pass = hunter2"})
            self.assertNotIn("hunter2", str(raised.exception))
            link = transport.calibrate(1, 2)
            self.assertTrue(link["complete"])
            self.assertEqual(link["bytes"], 2 * gb.MIB)

    def test_helper_result_rejects_missing_or_failed_output(self) -> None:
        with self.assertRaises(gb.Abort):
            gb.helper_result("probe", 255, "", "ssh: connect failed")
        with self.assertRaises(gb.Abort):
            gb.helper_result("probe", 1, 'gate-b-helper {"error": "E"}', "")
        self.assertEqual(
            gb.helper_result("probe", 0, 'gate-b-helper {"a": 1}', ""), {"a": 1}
        )


@unittest.skipUnless(
    os.environ.get("GATE_B_AGENT") and os.environ.get("GATE_B_RCLONE"),
    "set GATE_B_AGENT and GATE_B_RCLONE for the loopback smoke",
)
class LoopbackSmoke(unittest.TestCase):
    def test_dry_run_end_to_end(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp) / "work"
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                code = gb.main(
                    [
                        "--dry-run",
                        "--work-root", str(work),
                        "--agent", os.environ["GATE_B_AGENT"],
                        "--rclone", os.environ["GATE_B_RCLONE"],
                        "--reps", "1",
                    ]
                )  # fmt: skip
            report = json.loads((work / "gate-b.json").read_text())
            if (report.get("refusal") or {}).get("code") == "DEST_SPACE":
                # The temporary directory's volume is under the agent's floor:
                # the native arm would refuse every file. Set TMPDIR elsewhere.
                self.skipTest(report["reason"])
            self.assertEqual(code, 0, report.get("reason"))
            self.assertEqual(report["status"], "dry-run-complete-not-a-gate-sample")
            self.assertTrue(report["destination"]["disk"]["ok"])
            self.assertTrue(report["reps"][0]["destinations_released"])
            self.assertFalse(list((work / "reps" / "rep0").iterdir()))
            self.assertEqual(report["schema_problems"], [])
            self.assertEqual(report["workload"]["excluded"], 3)
            samples = report["reps"][0]["samples"]
            self.assertEqual(len(samples), 11)
            self.assertTrue(all(s["verified"]["ok"] for s in samples))
            warm = [s for s in samples if s["phase"] == "warm-resume"][0]
            self.assertEqual(
                (warm["bytes_received"], warm["content_bytes_read"]), (0, 0)
            )
            self.assertTrue(report["reps"][0]["verdict"]["r25_warm_zero"])
            self.assertEqual(report["arms"]["rclone_remote"]["skip_links"], "true")
            self.assertTrue(report["workload"]["restored_after"])
            self.assertIn(
                gb.NOT_GATE, next(work.glob("s1-gate-b-dryrun-*.md")).read_text()
            )


if __name__ == "__main__":
    unittest.main()
