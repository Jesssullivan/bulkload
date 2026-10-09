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
# A build with the Linux preflight answers `preflight`; an older one refuses.
if [ "$1" = preflight ]; then
  [ -f "$0.native" ] || exit 2
  echo "preflight power=ac load1=0.10 power_probe=sysfs os=linux arch=x86_64"
  exit 0
fi
# Which pmset a rep would run (the shim directory, or nothing).
command -v pmset >> "$0.pmset" || echo none >> "$0.pmset"
# Like the real bench: with --informational (dry run, under load) and the host
# outside the R-N81 gate, rows say gated=false and the verdict is informational
# with no wins; --only runs one arm and gives a diagnostic verdict.
informational=0
only=0
for arg in "$@"; do
  case "$arg" in
    --informational) informational=1 ;;
    --only) only=1 ;;
  esac
done
gated=${STUB_GATED:-}
if [ -z "$gated" ]; then
  if [ "$informational" = 1 ]; then gated=false; else gated=true; fi
fi
power=${STUB_POWER:-ac}
sealed=$(cat "$2"/* 2>/dev/null | cksum | cut -d' ' -f1)
echo "benchmark revision=r durability=group seal_primitive=${STUB_SEAL:-fsync} rclone_version=\\"${STUB_RCLONE:-rclone v1.74.4}\\" sealed_corpus_blake3=s$sealed"
echo "sample sequence=0 arm=Native phase=initial elapsed_ms=100.0 workload_bytes=1000 transferred_content_bytes=900 source_bytes_read=1000 power=$power load1=1.00 gated=$gated"
echo "native_timing sequence=0 phase=initial scope=s walk_ns=50 walk_ahead_wait_ns=20 queue_wait_ns=1 recv_setup_ns=10000000 recv_stream_ns=80000000 recv_tail_ns=6000000 recv_drain_ns=30000000 recv_first_data_ns=4000000"
echo "native_counters sequence=0 phase=initial scope=s flush_barrier_ns=10000000 flush_full_ns=5000000 flush_dir_ns=0 files_materialized=10"
echo "sample sequence=1 arm=Rclone phase=initial elapsed_ms=80.0 workload_bytes=1000 transferred_content_bytes=unknown source_bytes_read=unknown power=$power load1=1.00 gated=$gated"
echo "rclone_sync sequence=1 phase=initial sync_ms=30.000 timed=false"
if [ "$only" = 1 ]; then
  echo "verdict status=diagnostic-only reason=single-arm-run"
  exit 0
fi
if [ "$gated" = false ]; then
  echo "median phase=initial native_ms=100.000 rclone_ms=80.000 gated=false"
  echo "median phase=delta native_ms=10.000 rclone_ms=20.000 gated=false"
  echo "verdict status=informational reason=r-n81-preflight ungated_samples=2 r25_warm_zero=true r25_interrupted_zero=true native_rss_below_2gib=true"
  exit 0
fi
echo "rclone_synced phase=initial native_ms=100.000 rclone_synced_ms=110.000 native_wins=true informational=true"
echo "median phase=initial native_ms=100.000 rclone_ms=80.000 native_wins=false"
echo "median phase=delta native_ms=10.000 rclone_ms=20.000 native_wins=true"
echo "verdict status=${STUB_VERDICT:-fail} r23_initial_win=false"
exit 1
"""


# The gated tests run on a fake field host named `rig-x`; gated evidence must
# carry that name.
EV = "r23-rig-x-ev.md"


def fake_identity(node: str = "rig-x", **over: object) -> dict[str, object]:
    """A complete host_identity for the platform the test patched in."""
    system = ab.platform.system()
    identity: dict[str, object] = {
        "node": node,
        "system": system,
        "kernel": "6.0",
        "machine": "x86_64",
        "product": "TestBox1,1",
        "cpu_model": "Test CPU",
        "cpu_count": 4,
        "cpu_logical": 4,
        "cpu_physical_cores": 2,
        "ram_bytes": 16 * 2**30,
        "work_root_parent": "/x",
        "work_root_fs_type": "xfs",
        "power_probe": {"Linux": "sysfs", "Darwin": "pmset"}.get(system, "none"),
        "power_supplies": [],
    }
    identity.update(over)
    return identity


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
        # S1 at equal durability (OI-1003-Q107): informational, beside the
        # unchanged medians the verdict reads.
        self.assertEqual(
            summary["bench_rclone_synced"],
            {"initial": {"native_ms": 100.0, "rclone_synced_ms": 110.0}},
        )
        self.assertEqual(summary["bench_medians"]["initial"]["rclone_ms"], 80.0)
        # The timeline and its drain and ramp (OI-1003-Q143 step 1a) are known
        # keys, and setup + stream + tail cover 96 of the 100 ms; the drain
        # and ramp lie inside the stream and are never added to the cover.
        for key in ("recv_drain_ns", "recv_first_data_ns"):
            self.assertNotIn(key, summary["new_timing_keys"])
        self.assertEqual(summary["new_timing_keys"], ["walk_ahead_wait_ns"])
        timeline = summary["receive_timeline"]["initial"]
        self.assertAlmostEqual(timeline["recv_drain_ms_median"], 30.0)
        self.assertAlmostEqual(timeline["recv_first_data_ms_median"], 4.0)
        self.assertAlmostEqual(timeline["recv_setup_ms_median"], 10.0)
        self.assertAlmostEqual(timeline["cover_share_of_wall_median"], 0.96)
        self.assertEqual(timeline["samples"], 1)
        self.assertNotIn("delta", summary["receive_timeline"])


MAINS_ON = {"name": "ADP1", "type": "Mains", "online": "1"}
MAINS_OFF = {"name": "ADP1", "type": "Mains", "online": "0"}
FULL = {"name": "BAT0", "type": "Battery", "status": "Full"}
DRAINING = {"name": "BAT0", "type": "Battery", "status": "Discharging"}
MOUSE = {
    "name": "hid-mouse",
    "type": "Battery",
    "status": "Discharging",
    "scope": "Device",
}
USB_ON = {"name": "ucsi0", "type": "USB", "online": "1"}
UPS = {"name": "ups0", "type": "UPS", "status": "Full"}
NO_TYPE = {"name": "odd", "online": "1"}


def fake_sysfs(root: Path, supplies: list[dict[str, str]]) -> Path:
    root.mkdir()
    for supply in supplies:
        directory = root / supply["name"]
        directory.mkdir()
        for key, value in supply.items():
            if key != "name":
                (directory / key).write_text(value + "\n")
    return root


class LinuxPowerTests(unittest.TestCase):
    """The R-N81 Linux power rule (OI-1003-Q96).

    The same table as `linux_power_rule_over_fake_sysfs_trees` in the Rust bench.
    """

    CASES = (
        ("laptop on its adapter", [MAINS_ON, FULL], "ac"),
        ("laptop off its adapter", [MAINS_OFF, DRAINING], "battery"),
        ("adapter online, battery draining", [MAINS_ON, DRAINING], "ac"),
        ("battery only, not discharging", [FULL], "battery"),
        ("usb supply never proves ac", [USB_ON, FULL], "battery"),
        ("desktop: empty class", [], "ac"),
        ("desktop: offline mains, no battery", [MAINS_OFF], "ac"),
        ("desktop with a peripheral battery", [MOUSE], "ac"),
        ("peripheral battery beside a real one", [MOUSE, DRAINING], "battery"),
        ("ups without mains", [UPS], "battery"),
        ("unreadable type, nothing else", [NO_TYPE], "unknown"),
        ("unreadable type beside online mains", [NO_TYPE, MAINS_ON], "ac"),
        ("unreadable type beside a battery", [NO_TYPE, FULL], "battery"),
    )

    def setUp(self) -> None:
        raw = tempfile.TemporaryDirectory()
        self.addCleanup(raw.cleanup)
        self.tmp = Path(raw.name)

    def test_rule_over_fake_sysfs_trees(self) -> None:
        for index, (name, supplies, expected) in enumerate(self.CASES):
            root = fake_sysfs(self.tmp / f"case{index}", supplies)
            state, read = ab.linux_power(root)
            self.assertEqual(state, expected, name)
            self.assertEqual(
                sorted(s["name"] for s in read), sorted(s["name"] for s in supplies)
            )

    def test_missing_class_directory_and_silent_mains(self) -> None:
        self.assertEqual(ab.linux_power(self.tmp / "absent"), ("unknown", []))
        silent = {"name": "ADP1", "type": "Mains"}
        root = fake_sysfs(self.tmp / "silent", [silent, FULL])
        self.assertEqual(ab.linux_power(root)[0], "battery")

    def test_power_source_by_platform(self) -> None:
        root = fake_sysfs(self.tmp / "sys", [MAINS_OFF, DRAINING])
        with (
            mock.patch.object(ab, "POWER_SUPPLY_ROOT", root),
            mock.patch.object(ab.platform, "system", return_value="Linux"),
        ):
            self.assertEqual(ab.power_source(), "battery")
            self.assertFalse(ab.conditions()["ok"])
        with mock.patch.object(ab.platform, "system", return_value="FreeBSD"):
            self.assertEqual(ab.power_source(), "unknown")

    def shim(self, supplies: list[dict[str, str]], name: str) -> tuple[int, str]:
        root = fake_sysfs(self.tmp / name, supplies)
        out = io.StringIO()
        with (
            mock.patch.object(ab, "POWER_SUPPLY_ROOT", root),
            mock.patch.object(ab.platform, "system", return_value="Linux"),
            contextlib.redirect_stdout(out),
        ):
            return ab.main(["--pmset-shim"]), out.getvalue()

    def test_pmset_shim_speaks_pmset_and_never_invents_ac(self) -> None:
        self.assertEqual(
            self.shim([MAINS_ON, FULL], "a"), (0, "Now drawing from 'AC Power'\n")
        )
        self.assertEqual(
            self.shim([MAINS_OFF, DRAINING], "b"),
            (0, "Now drawing from 'Battery Power'\n"),
        )
        self.assertEqual(self.shim([NO_TYPE], "c"), (1, ""))
        out = io.StringIO()
        with (
            mock.patch.object(ab.platform, "system", return_value="Darwin"),
            contextlib.redirect_stdout(out),
        ):
            self.assertEqual(ab.main(["--pmset-shim"]), 1)
        self.assertEqual(out.getvalue(), "")

    def test_pmset_shim_script_runs_this_harness(self) -> None:
        directory = ab.write_pmset_shim(self.tmp)
        script = directory / "pmset"
        self.assertTrue(os.access(script, os.X_OK))
        self.assertIn("--pmset-shim", script.read_text())
        result = ab.subprocess.run(
            [str(script), "-g", "batt"], capture_output=True, text=True, check=False
        )
        expected = {
            "ac": (0, ab.PMSET_AC + "\n"),
            "battery": (0, ab.PMSET_BATTERY + "\n"),
        }.get(ab.power_source() if ab.platform.system() == "Linux" else "", (1, ""))
        self.assertEqual((result.returncode, result.stdout), expected)


class HostIdentityTests(unittest.TestCase):
    MOUNTINFO = (
        "22 1 253:0 / / rw,relatime shared:1 - xfs /dev/mapper/rl-root rw\n"
        "90 22 253:2 / /home rw,relatime shared:40 - ext4 /dev/mapper/rl-home rw\n"
        "95 90 0:40 / /home/a\\040b rw - tmpfs tmpfs rw\n"
        "malformed line\n"
    )
    MOUNT = (
        "/dev/disk3s1s1 on / (apfs, sealed, local, read-only, journaled)\n"
        "/dev/disk3s5 on /System/Volumes/Data (apfs, local, journaled, nobrowse)\n"
        "/dev/disk5s1 on /Volumes/TinylandState (hfs, local, journaled)\n"
    )

    def test_fs_type_takes_the_longest_containing_mount(self) -> None:
        fs = ab.fs_type_from_mountinfo
        self.assertEqual(fs(self.MOUNTINFO, "/home/jess/git-bulkload"), "ext4")
        self.assertEqual(fs(self.MOUNTINFO, "/home"), "ext4")
        self.assertEqual(fs(self.MOUNTINFO, "/homestead"), "xfs")
        self.assertEqual(fs(self.MOUNTINFO, "/home/a b/x"), "tmpfs")
        self.assertIsNone(fs("", "/home"))
        darwin = ab.fs_type_from_mount
        self.assertEqual(darwin(self.MOUNT, "/Volumes/TinylandState/x"), "hfs")
        self.assertEqual(darwin(self.MOUNT, "/Users/jess"), "apfs")
        self.assertIsNone(darwin("", "/"))

    def test_physical_cores_are_not_logical_cpus(self) -> None:
        """mbp-13's shape: 2 cores, 4 threads. os.cpu_count() would say 4."""
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for cpu, siblings in enumerate(("0,2", "1,3", "0,2", "1,3")):
                topology = root / f"cpu{cpu}" / "topology"
                topology.mkdir(parents=True)
                (topology / "thread_siblings_list").write_text(siblings + "\n")
            (root / "cpufreq").mkdir()
            (root / "cpu9").mkdir()  # offline: no topology directory
            self.assertEqual(ab.linux_physical_cores(root), 2)
            (root / "cpu3" / "topology" / "thread_siblings_list").unlink()
            self.assertIsNone(ab.linux_physical_cores(root))
            self.assertIsNone(ab.linux_physical_cores(root / "absent"))
        with tempfile.TemporaryDirectory() as tmp:
            self.assertIsNone(ab.linux_physical_cores(Path(tmp)))

    def test_rig_role_comes_from_the_committed_list(self) -> None:
        self.assertEqual(sorted(ab.RIG_OF_RECORD), ["mbp-13"])
        record = {
            "node": "mbp-13",
            "system": "Linux",
            "machine": "x86_64",
            "product": "MacBookPro12,1",
        }
        self.assertEqual(ab.rig_role(record), ("record", None))
        self.assertEqual(ab.rig_role({**record, "node": "mbp-13.lan"})[0], "record")
        for node in ("neo", "sting", "yoga", "mbp-130", ""):
            self.assertEqual(ab.rig_role({**record, "node": node}), ("field", None))
        role, problem = ab.rig_role({**record, "product": "KVM"})
        self.assertEqual(role, "field")
        self.assertIn("identity differs", problem)
        self.assertEqual(ab.rig_name("a b/c.example"), "a-b-c")
        self.assertEqual(ab.rig_name(None), "unknown-host")

    def test_cpu_model(self) -> None:
        text = "processor\t: 0\nmodel name\t: Intel(R) Core(TM) i7\nflags\t: fpu\n"
        self.assertEqual(ab.cpu_model_from_cpuinfo(text), "Intel(R) Core(TM) i7")
        self.assertIsNone(ab.cpu_model_from_cpuinfo("processor: 0\n"))

    def test_identity_names_the_rig(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            identity = ab.host_identity(Path(tmp))
        self.assertEqual(identity["system"], ab.platform.system())
        self.assertEqual(identity["cpu_count"], os.cpu_count())
        self.assertEqual(identity["cpu_logical"], os.cpu_count())
        if identity["system"] == "Linux":
            self.assertGreaterEqual(identity["cpu_physical_cores"], 1)
            self.assertLessEqual(identity["cpu_physical_cores"], os.cpu_count())
        self.assertGreater(identity["ram_bytes"], 0)
        if identity["system"] == "Linux":
            self.assertEqual(identity["power_probe"], "sysfs")
            self.assertTrue(identity["cpu_model"])
            self.assertTrue(identity["work_root_fs_type"])


def cond(
    ok: bool = True, load: float | None = None, power: str = "ac"
) -> dict[str, object]:
    return {
        "utc": "t",
        "load1": load if load is not None else (1.0 if ok else 9.0),
        "power": power,
        "ok": ok and power == "ac",
    }


def refuse_call(n: int) -> contextlib.AbstractContextManager[object]:
    """Make the n-th parsed bench run look refused (no verdict line)."""
    real = ab.parse_bench
    calls = {"n": 0}

    def parse(stdout: str) -> dict[str, object]:
        calls["n"] += 1
        return {} if calls["n"] == n else real(stdout)

    return mock.patch.object(ab, "parse_bench", side_effect=parse)


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
        # A fake Nix store: the flake-pinned rclone, and another build.
        self.pinned = self.fake_rclone("pin0-rclone-1.74.4")
        self.other = self.fake_rclone("oth0-rclone-1.75.0")

    def fake_rclone(self, name: str) -> Path:
        binary = self.tmp / "store" / name / "bin" / "rclone"
        binary.parent.mkdir(parents=True)
        binary.write_text(name)
        return binary

    def main(self, extra: list[str], **patches: object) -> int:
        """Run main() with builds, rclone and (unless given) host checks patched."""
        targets = {
            "build": mock.patch.object(ab, "build", return_value=self.info),
            "flake_rclone": mock.patch.object(
                ab, "flake_rclone", return_value=self.pinned
            ),
            "rclone_version": mock.patch.object(
                ab, "rclone_version", return_value=ab.RCLONE_PIN["version"]
            ),
            "system_key": mock.patch.object(
                ab, "system_key", return_value="test-system"
            ),
            "rclone_pin": mock.patch.dict(
                ab.RCLONE_PIN["store_paths"],
                {"test-system": str(self.pinned.parent.parent)},
            ),
            # `git rev-parse` of --rev-a on a rig of record: the pinned A.
            "git": mock.patch.object(
                ab, "git", return_value=ab.RIG_BASELINE_A["mbp-13"]["sha"]
            ),
            "conditions": mock.patch.object(ab, "conditions", return_value=cond()),
            "corpus_verify": mock.patch.object(ab, "corpus_verify", return_value=0),
            "corpus_shape": mock.patch.object(
                ab, "corpus_shape", return_value=(ab.RECORD_FILES, ab.RECORD_BYTES)
            ),
            "system": mock.patch.object(ab.platform, "system", return_value="Darwin"),
            "host_identity": mock.patch.object(
                ab, "host_identity", side_effect=lambda _parent: fake_identity()
            ),
        }
        for name, value in patches.items():
            targets[name] = value
        with contextlib.ExitStack() as stack:
            for patcher in targets.values():
                stack.enter_context(patcher)
            self.out = io.StringIO()
            stack.enter_context(contextlib.redirect_stdout(self.out))
            return ab.main(
                ["--repo", str(self.tmp), "--work-root", str(self.tmp / "work"), *extra]
            )

    def gated(self) -> list[str]:
        return [
            "--corpus",
            str(self.corpus),
            "--coordinator-quiet",
            "--evidence",
            str(self.tmp / EV),
            "--post-settle-seconds",
            "0",
        ]

    def under_load(self) -> list[str]:
        """Under-load args: no --coordinator-quiet (OI-1003-Q39), *underload* evidence."""
        return [
            "--corpus",
            str(self.corpus),
            "--under-load",
            "--evidence",
            str(self.tmp / "r23-underload-test.md"),
            "--post-settle-seconds",
            "0",
            "--settle-seconds",
            "0",
        ]

    def evidence_md(self) -> str:
        return (self.tmp / "r23-underload-test.md").read_text()

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
        # The stub prints no rclone_synced rows ungated, so no table.
        self.assertNotIn("## Equal durability", md)

    def test_dry_run_refuses_docs_evidence(self) -> None:
        target = self.tmp / "docs" / "evidence" / "x.md"
        self.assertEqual(self.main(["--dry-run", "--evidence", str(target)]), 2)
        self.assertFalse((self.tmp / "work").exists())

    def test_gated_refusals(self) -> None:
        other = mock.patch.object(ab.platform, "system", return_value="FreeBSD")
        self.assertEqual(self.main(self.gated(), system=other), 2)
        linux = mock.patch.object(ab.platform, "system", return_value="Linux")
        no_corpus = [a for a in self.gated() if a not in ("--corpus", str(self.corpus))]
        self.assertEqual(self.main(no_corpus, system=linux), 2)
        no_quiet = [a for a in self.gated() if a != "--coordinator-quiet"]
        self.assertEqual(self.main(no_quiet), 2)
        self.assertEqual(self.main([*self.gated(), "--expect-files", "1"]), 2)
        bad = mock.patch.object(ab, "corpus_verify", return_value=1)
        self.assertEqual(self.main(self.gated(), corpus_verify=bad), 2)
        self.assertFalse((self.tmp / "work").exists())

    def linux_builds(self, b_native: bool) -> mock._patch:
        """Distinct stub binaries per label; B has the sysfs preflight or not."""
        infos = {}
        for label in ("B", "A", "V4"):
            binary = stub(self.tmp / f"bench-{label}")
            if label == "B" and b_native:
                Path(f"{binary}.native").write_text("")
            infos[label] = {
                "rev": label,
                "sha": {
                    "B": "b" * 40,
                    "A": ab.RIG_BASELINE_A["mbp-13"]["sha"],
                    "V4": "4" * 40,
                }[label],
                "binary": str(binary),
                "sha256": "0" * 64,
            }
        self.infos = infos
        order = iter(("B", "A", "V4"))
        return mock.patch.object(
            ab, "build", side_effect=lambda *_a, **_k: infos[next(order)]
        )

    def test_gated_linux_refuses_a_b_without_its_own_preflight(self) -> None:
        linux = mock.patch.object(ab.platform, "system", return_value="Linux")
        code = self.main(
            self.gated(), system=linux, build=self.linux_builds(b_native=False)
        )
        self.assertEqual(code, 2)
        self.assertFalse((self.tmp / "work" / "r23-ab.json").exists())
        self.assertFalse((self.tmp / EV).exists())
        self.assertFalse(Path(self.infos["B"]["binary"] + ".pmset").exists())

    def test_gated_linux_runs_b_native_and_baselines_behind_the_shim(self) -> None:
        linux = mock.patch.object(ab.platform, "system", return_value="Linux")
        with mock.patch.dict(os.environ, {"STUB_VERDICT": "pass"}):
            code = self.main(
                self.gated(), system=linux, build=self.linux_builds(b_native=True)
            )
        self.assertEqual(code, 0)
        report = self.report()
        self.assertEqual(
            {k: v["preflight"] for k, v in report["builds"].items()},
            {"B": "native", "A": "pmset-shim", "V4": "pmset-shim"},
        )
        self.assertEqual(
            [(r["label"], r["preflight"]) for r in report["reps"]],
            [
                ("B", "native"),
                ("A", "pmset-shim"),
                ("B", "native"),
                ("A", "pmset-shim"),
                ("B", "native"),
                ("V4", "pmset-shim"),
            ],
        )
        # The verdict rule is unchanged: every B rep passes, A decides nothing.
        self.assertEqual(report["gate"]["verdict"], "PASS")
        self.assertEqual(report["gate"]["b_reps"], 3)
        self.assertIn("OI-1002-Q30", report["gate"]["rule"])
        shim = str(self.tmp / "work" / "pmset-shim" / "pmset")
        seen = {
            label: Path(info["binary"] + ".pmset").read_text().split()
            for label, info in self.infos.items()
        }
        self.assertEqual(seen["A"], [shim, shim])
        self.assertEqual(seen["V4"], [shim])
        self.assertEqual(len(seen["B"]), 3)
        self.assertNotIn(shim, seen["B"])
        identity = report["host_identity"]
        for key in (
            "node",
            "system",
            "kernel",
            "machine",
            "cpu_model",
            "cpu_count",
            "cpu_logical",
            "cpu_physical_cores",
            "ram_bytes",
            "work_root_fs_type",
            "power_probe",
        ):
            self.assertIn(key, identity)
        self.assertEqual(identity["system"], "Linux")
        self.assertEqual(identity["power_probe"], "sysfs")
        md = (self.tmp / EV).read_text()
        a12 = ab.RIG_BASELINE_A["mbp-13"]["sha"][:12]
        self.assertIn(f"| A | `A` | `{a12}` | `0000000000000000` | pmset-shim |", md)
        self.assertIn("B never runs behind it", md)
        self.assertIn("- Rig: `", md)
        self.assertIn("OI-1003-Q96", md)

    def test_darwin_builds_are_never_shimmed(self) -> None:
        with mock.patch.dict(os.environ, {"STUB_VERDICT": "pass"}):
            self.assertEqual(self.main(self.gated()), 0)
        report = self.report()
        self.assertFalse((self.tmp / "work" / "pmset-shim").exists())
        self.assertTrue(all("preflight" not in b for b in report["builds"].values()))
        self.assertTrue(all(r["preflight"] == "native" for r in report["reps"]))
        self.assertNotIn("pmset-shim", (self.tmp / EV).read_text())

    def no_a(self, name: str = "r23-rig-x-no-a-control.md") -> list[str]:
        args = [a for a in self.gated() if a not in ("--evidence", str(self.tmp / EV))]
        return [
            *args,
            "--evidence",
            str(self.tmp / name),
            "--b-only-no-a-control",
            "--no-a-control-reason",
            "A fails on this rig",
        ]

    def test_b_only_runs_bbb_and_marks_the_verdict(self) -> None:
        for verdict, expected in (("pass", "PASS"), ("fail", "FAIL")):
            with self.subTest(verdict=verdict):
                self.setUp()
                revs: list[str] = []

                def record(_repo: Path, rev: str, *_a: object) -> dict[str, str]:
                    revs.append(rev)
                    return self.info

                build = mock.patch.object(ab, "build", side_effect=record)
                with mock.patch.dict(os.environ, {"STUB_VERDICT": verdict}):
                    self.assertEqual(self.main(self.no_a(), build=build), 0)
                self.assertEqual(revs, [ab.DEFAULT_B, ab.DEFAULT_V4])
                report = self.report()
                # B and V4 are built; A never is.
                self.assertEqual(sorted(report["builds"]), ["B", "V4"])
                self.assertEqual(
                    [r["label"] for r in report["reps"]], ["B", "B", "B", "V4"]
                )
                self.assertEqual(report["mode"], "gated")
                self.assertEqual(report["status"], "complete-draft-no-a-control")
                self.assertEqual(report["pattern"], "BBB")
                self.assertFalse(report["a_control"])
                self.assertEqual(report["a_control_reason"], "A fails on this rig")
                gate = report["gate"]
                self.assertEqual(gate["verdict"], f"{expected}-NO-A-CONTROL")
                self.assertNotIn(" ", gate["verdict"])
                self.assertFalse(gate["of_record"])
                self.assertFalse(gate["a_control"])
                self.assertEqual(gate["b_reps"], 3)
                self.assertIn("lacks the A control", gate["rule"])
                self.assertIn("OI-1002-Q30", gate["rule"])
                md = (self.tmp / "r23-rig-x-no-a-control.md").read_text()
                self.assertIn("(DRAFT, NO A CONTROL)", md.splitlines()[0])
                self.assertIn(
                    f"**R23 gate verdict for B: {expected}-NO-A-CONTROL, rig `rig-x`,"
                    " role `field`",
                    md,
                )
                self.assertIn("A fails on this rig", md)
                self.assertIn("operator's decision", md)

    def test_b_only_keeps_every_gated_check(self) -> None:
        busy = mock.patch.object(ab, "conditions", return_value=cond(False))
        self.assertEqual(
            self.main([*self.no_a(), "--settle-seconds", "0"], conditions=busy), 2
        )
        self.assertEqual(self.report()["gate"]["verdict"][:4], "NONE")
        self.setUp()
        with mock.patch.dict(os.environ, {"STUB_GATED": "false"}):
            self.assertEqual(self.main(self.no_a()), 3)
        self.setUp()
        no_quiet = [a for a in self.no_a() if a != "--coordinator-quiet"]
        self.assertEqual(self.main(no_quiet), 2)

    def test_b_only_refusals(self) -> None:
        self.assertEqual(self.main(self.no_a("r23-plain.md")), 2)
        self.assertEqual(self.main(self.no_a()[:-2]), 2)
        self.assertEqual(self.main([*self.no_a()[:-1], "  "]), 2)
        self.assertEqual(self.main([*self.no_a(), "--pattern", "BBB"]), 2)
        self.assertEqual(self.main([*self.no_a(), "--under-load"]), 2)
        self.assertEqual(self.main([*self.no_a(), "--dry-run"]), 2)
        self.assertEqual(self.main([*self.gated(), "--no-a-control-reason", "x"]), 2)
        self.assertFalse((self.tmp / "work").exists())

    def test_a_default_gated_sample_keeps_its_a_control(self) -> None:
        with mock.patch.dict(os.environ, {"STUB_VERDICT": "pass"}):
            self.assertEqual(self.main(self.gated()), 0)
        report = self.report()
        self.assertTrue(report["a_control"])
        self.assertTrue(report["gate"]["a_control"])
        self.assertEqual(report["gate"]["verdict"], "PASS")
        self.assertEqual(report["status"], "complete-draft")
        self.assertNotIn("NO A CONTROL", (self.tmp / EV).read_text())

    def record_rig(self, **over: object) -> mock._patch:
        """Pretend to be mbp-13, the rig of record (Linux)."""
        pinned = {"product": "MacBookPro12,1", **over}
        return mock.patch.object(
            ab,
            "host_identity",
            side_effect=lambda _parent: fake_identity("mbp-13", **pinned),
        )

    def run_on(self, argv: list[str], **patches: object) -> tuple[int, str]:
        """main() with its stdout: the status line is what a poller reads."""
        code = self.main(argv, **patches)
        return code, self.out.getvalue()

    def test_a_field_host_is_never_of_record(self) -> None:
        with mock.patch.dict(os.environ, {"STUB_VERDICT": "pass"}):
            code, out = self.run_on(self.gated())
        self.assertEqual(code, 0)
        report = self.report()
        self.assertEqual((report["rig"], report["rig_role"]), ("rig-x", "field"))
        self.assertEqual(report["gate"]["verdict"], "PASS")
        self.assertFalse(report["gate"]["of_record"])
        self.assertIn(" rig=rig-x rig_role=field of_record=false ", out)
        self.assertTrue(out.strip().endswith("gate=PASS"))
        md = (self.tmp / EV).read_text()
        self.assertIn("rig rig-x, role field", md.splitlines()[0])
        self.assertIn(
            "**R23 gate verdict for B: PASS, rig `rig-x`, role `field`: a field"
            " confirmation",
            md,
        )
        self.assertIn("never the S1 verdict", md)

    def test_the_rig_of_record_with_its_a_control_is_of_record(self) -> None:
        linux = mock.patch.object(ab.platform, "system", return_value="Linux")
        argv = [a for a in self.gated() if a not in ("--evidence", str(self.tmp / EV))]
        with mock.patch.dict(os.environ, {"STUB_VERDICT": "fail"}):
            code, out = self.run_on(
                argv,
                system=linux,
                build=self.linux_builds(b_native=True),
                host_identity=self.record_rig(),
            )
        self.assertEqual(code, 0)
        report = self.report()
        self.assertEqual((report["rig"], report["rig_role"]), ("mbp-13", "record"))
        self.assertEqual(report["gate"]["verdict"], "FAIL")
        self.assertTrue(report["gate"]["of_record"])
        self.assertIn(" rig=mbp-13 rig_role=record of_record=true ", out)
        written = sorted((self.tmp / "docs" / "evidence").iterdir())
        self.assertEqual(len(written), 1)
        self.assertRegex(
            written[0].name, r"^r23-\d{4}-\d{2}-\d{2}-\d{4}Z-mbp-13-record\.md$"
        )
        md = written[0].read_text()
        self.assertIn("rig mbp-13, role record", md.splitlines()[0])
        self.assertIn("role `record`: the rig of record; of_record=true**", md)

    def test_b_only_on_the_rig_of_record_is_not_of_record(self) -> None:
        linux = mock.patch.object(ab.platform, "system", return_value="Linux")
        argv = self.no_a()
        at = argv.index("--evidence")
        del argv[at : at + 2]
        Path(f"{self.info['binary']}.native").write_text("")
        with mock.patch.dict(os.environ, {"STUB_VERDICT": "pass"}):
            code, out = self.run_on(argv, system=linux, host_identity=self.record_rig())
        self.assertEqual(code, 0)
        gate = self.report()["gate"]
        self.assertEqual(gate["verdict"], "PASS-NO-A-CONTROL")
        self.assertFalse(gate["of_record"])
        self.assertIn(" rig_role=record of_record=false ", out)
        self.assertTrue(out.strip().endswith("gate=PASS-NO-A-CONTROL"))
        self.assertNotRegex(out, r"gate=PASS(\s|$)")
        written = sorted((self.tmp / "docs" / "evidence").iterdir())
        self.assertRegex(written[0].name, r"Z-mbp-13-record-no-a-control\.md$")
        self.assertIn(
            "not a verdict of record (of_record=false)", written[0].read_text()
        )

    def test_gated_refuses_an_unnamed_or_impostor_rig(self) -> None:
        for key in ab.IDENTITY_REQUIRED:
            for empty in (None, ""):
                blank = mock.patch.object(
                    ab,
                    "host_identity",
                    side_effect=lambda _p, k=key, e=empty: fake_identity(**{k: e}),
                )
                code, out = self.run_on(self.gated(), host_identity=blank)
                self.assertEqual(code, 2, key)
                self.assertIn(f"host_identity has no {key}", out)
        # Named like the rig of record, on the wrong platform or product.
        code, out = self.run_on(self.gated(), host_identity=self.record_rig())
        self.assertEqual(code, 2)
        self.assertIn("identity differs", out)
        # Evidence that does not carry the rig's name.
        argv = self.gated()
        argv[argv.index("--evidence") + 1] = str(self.tmp / "r23-ev.md")
        code, out = self.run_on(argv)
        self.assertEqual(code, 2)
        self.assertIn("lacks rig-x", out)
        self.assertFalse((self.tmp / "work").exists())

    def test_under_load_does_not_need_a_complete_identity(self) -> None:
        blank = mock.patch.object(
            ab, "host_identity", side_effect=lambda _p: fake_identity(product=None)
        )
        self.assertEqual(self.main(self.under_load(), host_identity=blank), 0)
        self.assertIn("rig rig-x, role field", self.evidence_md().splitlines()[0])

    def test_build_and_settle_are_recorded(self) -> None:
        """The evidence says what was compiled in the sample and the load after."""
        compiled = {**self.info, "compiled_in_this_run": True}
        prebuilt = {**self.info, "compiled_in_this_run": False}
        order = iter((compiled, prebuilt, prebuilt))
        build = mock.patch.object(ab, "build", side_effect=lambda *_a: next(order))
        after_build = mock.patch.object(
            ab.os, "getloadavg", return_value=(2.03, 1.0, 0.5)
        )
        quiet = mock.patch.object(ab, "conditions", return_value=cond(load=1.5))
        with mock.patch.dict(os.environ, {"STUB_VERDICT": "pass"}):
            code = self.main(
                self.gated(), build=build, conditions=quiet, loadavg=after_build
            )
        self.assertEqual(code, 0)
        report = self.report()
        self.assertEqual(report["post_build"]["load1"], 2.03)
        self.assertEqual(report["settle"]["admitted"]["load1"], 1.5)
        self.assertEqual(report["seal_primitive"], "fsync")
        md = (self.tmp / EV).read_text()
        self.assertIn("compiled inside this sample: **B**", md)
        self.assertIn("load1 right after the builds: 2.03", md)
        self.assertIn("not scaled to this rig's 2 physical cores", md)
        self.assertIn("2 physical cores, 4 logical CPUs", md)
        self.assertIn("per-file seal in this sample was `fsync`", md)
        self.setUp()
        with mock.patch.dict(os.environ, {"STUB_VERDICT": "pass"}):
            self.assertEqual(self.main(self.gated()), 0)
        self.assertIn("none (every binary was prebuilt)", (self.tmp / EV).read_text())

    def test_build_only_builds_and_runs_no_sample(self) -> None:
        revs: list[str] = []

        def record(_repo: Path, rev: str, *_a: object) -> dict[str, object]:
            revs.append(rev)
            return {**self.info, "compiled_in_this_run": True}

        build = mock.patch.object(ab, "build", side_effect=record)
        never = mock.patch.object(ab, "run_rep", side_effect=AssertionError)
        code, out = self.run_on(["--build-only"], build=build, run_rep=never)
        self.assertEqual(code, 0)
        self.assertEqual(revs, [ab.DEFAULT_B, ab.DEFAULT_A, ab.DEFAULT_V4])
        self.assertIn("status=built-not-a-sample", out)
        self.assertEqual(out.count("compiled=true"), 3)
        self.assertFalse((self.tmp / "work" / "r23-ab.json").exists())
        self.assertFalse((self.tmp / "docs").exists())
        self.setUp()
        self.assertEqual(self.main(["--build-only", "--dry-run"]), 2)
        self.assertEqual(self.main(["--build-only", "--under-load"]), 2)
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
            "**R23 gate verdict for B: PASS, rig `rig-x`", (self.tmp / EV).read_text()
        )

    def test_gated_rollup_fails_when_any_b_rep_fails(self) -> None:
        self.assertEqual(self.main(self.gated()), 0)
        self.assertEqual(self.report()["gate"]["verdict"], "FAIL")

    def test_under_load_and_dry_run_are_exclusive(self) -> None:
        self.assertEqual(self.main(["--dry-run", "--under-load"]), 2)
        self.assertFalse((self.tmp / "work").exists())

    def test_under_load_records_load_but_does_not_gate_on_it(self) -> None:
        high = mock.patch.object(ab, "conditions", return_value=cond(False))
        self.assertEqual(self.main(self.under_load(), conditions=high), 0)
        report = self.report()
        self.assertEqual(report["mode"], "under-load")
        self.assertEqual(report["status"], "complete-under-load-informational")
        self.assertFalse(report["load_gated"])
        self.assertTrue(
            all(r["conditions_before"]["load1"] == 9.0 for r in report["reps"])
        )
        md = self.evidence_md()
        self.assertIn(ab.UNDER_LOAD, md.splitlines()[0])
        self.assertIn("not an R23 gate", md)

    def test_under_load_rollup_reports_statuses_and_medians_not_a_pass_count(
        self,
    ) -> None:
        # The stub, like the real bench under load, prints gated=false rows,
        # status=informational and no r23_*_win keys.
        self.assertEqual(self.main(self.under_load()), 0)
        report = self.report()
        gate = report["gate"]
        self.assertEqual(gate["b_statuses"], ["informational"] * 3)
        self.assertEqual(gate["b_status_counts"], "informational x3")
        self.assertTrue(gate["verdict"].startswith(ab.UNDER_LOAD))
        self.assertIn("informational x3", gate["verdict"])
        self.assertIn("initial native 100.000 ms vs rclone 80.000 ms", gate["verdict"])
        self.assertIn("delta native 10.000 ms vs rclone 20.000 ms", gate["verdict"])
        self.assertNotIn("pass", gate["verdict"])
        self.assertEqual(
            gate["b_bench_medians"],
            {
                "initial": {"native_ms": 100.0, "rclone_ms": 80.0},
                "delta": {"native_ms": 10.0, "rclone_ms": 20.0},
            },
        )
        self.assertTrue(
            all(not r["summary"]["all_gated"] for r in report["reps"]),
            "the stub must print gated=false under load, like the real bench",
        )
        md = self.evidence_md()
        self.assertNotIn("B reps pass", md)
        self.assertNotIn("R23 gate verdict for B", md)
        self.assertIn(
            "not an R23 gate verdict:** INFORMATIONAL UNDER LOAD - NOT A GATE SAMPLE:"
            " B bench statuses informational x3;",
            md,
        )
        self.assertIn("## Bench medians (informational, under load)", md)
        self.assertIn("| B median | B | informational x3 | 100.000 | 80.000 |", md)

    def test_under_load_with_a_quiet_host_still_reports_statuses(self) -> None:
        with mock.patch.dict(
            os.environ, {"STUB_GATED": "true", "STUB_VERDICT": "pass"}
        ):
            self.assertEqual(self.main(self.under_load()), 0)
        gate = self.report()["gate"]
        self.assertEqual(gate["b_status_counts"], "pass x3")
        self.assertIn("pass x3", gate["verdict"])
        self.assertTrue(gate["verdict"].startswith(ab.UNDER_LOAD))

    def test_under_load_cites_its_rulings_and_does_not_need_coordinator_quiet(
        self,
    ) -> None:
        self.assertEqual(self.main(self.under_load()), 0)
        report = self.report()
        self.assertFalse(report["coordinator_quiet"])
        self.assertEqual(report["rulings"], ab.RULINGS_UNDER_LOAD)
        md = self.evidence_md()
        rulings = next(line for line in md.splitlines() if line.startswith("Rulings:"))
        self.assertIn("OI-1003-Q39", rulings)
        self.assertIn("OI-1003-Q50", rulings)
        self.assertNotIn("R-N91", rulings)
        self.assertNotIn("R-N81", rulings)
        self.assertIn("not required under load", md)
        self.assertNotIn("(R-N91).", md)

    def test_under_load_records_coordinator_quiet_as_acknowledged_only(self) -> None:
        self.assertEqual(self.main([*self.under_load(), "--coordinator-quiet"]), 0)
        report = self.report()
        self.assertTrue(report["coordinator_quiet"])
        self.assertIn("not R-N91 gated", report["coordinator_quiet_meaning"])
        md = self.evidence_md()
        self.assertIn("acknowledged only; an under-load sample is not R-N91 gated", md)
        self.assertNotIn("(R-N91).", md)

    def test_gated_evidence_still_cites_r_n91(self) -> None:
        with mock.patch.dict(os.environ, {"STUB_VERDICT": "pass"}):
            self.assertEqual(self.main(self.gated()), 0)
        md = (self.tmp / EV).read_text()
        self.assertIn(f"Rulings: {ab.RULINGS}.", md)
        self.assertIn("coordinator-quiet acknowledged: `True` (R-N91)", md)

    def test_under_load_default_evidence_name_is_underload(self) -> None:
        args = [a for a in self.under_load()]
        at = args.index("--evidence")
        del args[at : at + 2]
        self.assertEqual(self.main(args), 0)
        written = sorted((self.tmp / "docs" / "evidence").iterdir())
        self.assertEqual(len(written), 1)
        self.assertRegex(
            written[0].name, r"^r23-underload-\d{4}-\d{2}-\d{2}-\d{4}Z\.md$"
        )
        self.assertIn(ab.UNDER_LOAD, written[0].read_text().splitlines()[0])

    def test_under_load_refuses_a_gate_style_evidence_name(self) -> None:
        for name in (EV, "r23-2026-10-04-1842Z.md"):
            args = self.under_load()
            args[args.index("--evidence") + 1] = str(self.tmp / name)
            self.assertEqual(self.main(args), 2)
            self.assertFalse((self.tmp / "work").exists())
            self.assertFalse((self.tmp / name).exists())

    def test_informational_flag_only_in_under_load_rep_commands(self) -> None:
        self.assertEqual(self.main(self.under_load()), 0)
        under = self.report()["reps"]
        self.assertTrue(all("--informational" in r["command"] for r in under))
        (self.tmp / "work").rename(self.tmp / "work-under-load")
        with mock.patch.dict(os.environ, {"STUB_VERDICT": "pass"}):
            self.assertEqual(self.main(self.gated()), 0)
        gated = self.report()["reps"]
        self.assertEqual(len(gated), 6)
        self.assertTrue(all("--informational" not in r["command"] for r in gated))

    def test_under_load_refuses_to_start_off_ac_power(self) -> None:
        battery = mock.patch.object(
            ab, "conditions", return_value=cond(load=9.0, power="battery")
        )
        with mock.patch.object(ab.time, "sleep"):
            self.assertEqual(self.main(self.under_load(), conditions=battery), 2)
        report = self.report()
        self.assertEqual(report["status"], "refused")
        self.assertIn("never settled (AC power)", report["reason"])
        self.assertEqual(report["reps"], [])

    def test_under_load_waits_for_ac_before_the_first_rep(self) -> None:
        states = iter([cond(load=9.0, power="battery")])
        settle = mock.patch.object(
            ab, "conditions", side_effect=lambda: next(states, cond(load=9.0))
        )
        args = self.under_load()
        args[args.index("--settle-seconds") + 1] = "3600"
        with mock.patch.object(ab.time, "sleep") as sleep:
            self.assertEqual(self.main(args, conditions=settle), 0)
        self.assertEqual(sleep.call_count, 1)

    def test_under_load_aborts_when_power_fails_before_a_rep(self) -> None:
        # Calls: the opening check, rep 0 before and after (no settle wait
        # under load), then rep 1 before, on battery.
        states = iter([cond(False)] * 3 + [cond(load=9.0, power="battery")])
        flip = mock.patch.object(
            ab, "conditions", side_effect=lambda: next(states, cond(False))
        )
        self.assertEqual(self.main(self.under_load(), conditions=flip), 3)
        report = self.report()
        self.assertEqual(report["status"], "aborted")
        self.assertEqual(len(report["reps"]), 1)
        self.assertIn("rep1 A precondition failed", report["reason"])
        self.assertIn("'power': 'battery'", report["reason"])

    def test_under_load_aborts_when_power_fails_after_a_rep(self) -> None:
        states = iter([cond(False)] * 2 + [cond(load=9.0, power="battery")])
        flip = mock.patch.object(
            ab, "conditions", side_effect=lambda: next(states, cond(False))
        )
        self.assertEqual(self.main(self.under_load(), conditions=flip), 3)
        report = self.report()
        self.assertEqual(report["status"], "aborted")
        self.assertIn(
            "rep0 B post-check failed: power=battery/battery", report["reason"]
        )
        self.assertTrue(report["reps"][0]["aborted"])

    def test_under_load_aborts_when_a_bench_row_is_off_ac_power(self) -> None:
        # --informational lets the bench run an arm on battery; the harness
        # must refuse the rows even though its own checks saw AC.
        with mock.patch.dict(os.environ, {"STUB_POWER": "battery"}):
            self.assertEqual(self.main(self.under_load()), 3)
        report = self.report()
        self.assertEqual(report["status"], "aborted")
        self.assertIn("2 bench row(s) not on AC power", report["reason"])
        self.assertIn("Native/initial power=battery", report["reason"])

    def test_under_load_refused_informational_rep_off_ac_power_aborts(self) -> None:
        # Rep 1 (A) is refused, and its post-rep power check fails: that is a
        # power failure, not a recordable informational refusal.
        states = iter([cond(False)] * 4 + [cond(load=9.0, power="battery")])
        flip = mock.patch.object(
            ab, "conditions", side_effect=lambda: next(states, cond(False))
        )
        self.assertEqual(
            self.main(self.under_load(), conditions=flip, parse_bench=refuse_call(2)),
            3,
        )
        report = self.report()
        self.assertEqual(report["status"], "aborted")
        self.assertNotIn("refused_reps", report)
        self.assertIn("rep1 A bench refused", report["reason"])
        self.assertIn("power=battery/battery", report["reason"])

    def test_under_load_refused_informational_rep_with_battery_rows_aborts(
        self,
    ) -> None:
        real = ab.parse_bench
        calls = {"n": 0}

        def parse(stdout: str) -> dict[str, object]:
            calls["n"] += 1
            parsed = real(stdout)
            if calls["n"] == 2:
                del parsed["verdict"]
                parsed["samples"][0]["power"] = "battery"
            return parsed

        refuse_a = mock.patch.object(ab, "parse_bench", side_effect=parse)
        self.assertEqual(self.main(self.under_load(), parse_bench=refuse_a), 3)
        report = self.report()
        self.assertNotIn("refused_reps", report)
        self.assertIn("rep1 A bench refused", report["reason"])
        self.assertIn("1 bench row(s) not on AC power", report["reason"])

    def test_under_load_records_a_refused_informational_rep_and_continues(self) -> None:
        self.assertEqual(self.main(self.under_load(), parse_bench=refuse_call(2)), 0)
        report = self.report()
        self.assertEqual(report["status"], "complete-under-load-informational")
        self.assertEqual([r["label"] for r in report["refused_reps"]], ["A"])
        self.assertEqual(
            [r["label"] for r in report["reps"]], ["B", "B", "A", "B", "V4"]
        )
        self.assertIn("refused under load", self.evidence_md())

    def test_under_load_aborts_on_a_refused_b_rep(self) -> None:
        self.assertEqual(self.main(self.under_load(), parse_bench=refuse_call(1)), 3)
        report = self.report()
        self.assertEqual(report["status"], "aborted")
        self.assertIn("rep0 B bench refused", report["reason"])
        self.assertNotIn("refused_reps", report)
        self.assertTrue(report["gate"]["verdict"].startswith("NONE"))
        self.assertIn("(ABORTED)", self.evidence_md().splitlines()[0])

    def test_under_load_aborts_when_corpus_changes_between_reps(self) -> None:
        calls = {"n": 0}

        def tamper(_binary: Path, corpus: Path) -> float:
            calls["n"] += 1
            if calls["n"] == 2:
                (corpus / "f").write_bytes(b"y" * 7)
            return 1.0

        patch = mock.patch.object(ab, "residency", side_effect=tamper)
        self.assertEqual(self.main(self.under_load(), residency=patch), 3)
        report = self.report()
        self.assertEqual(report["status"], "aborted")
        self.assertIn("sealed_corpus_blake3", report["reason"])

    def test_under_load_aborts_when_corpus_fails_final_verify(self) -> None:
        results = iter([0, 0, 1, 0])
        late = mock.patch.object(
            ab, "corpus_verify", side_effect=lambda _c: next(results, 1)
        )
        self.assertEqual(self.main(self.under_load(), corpus_verify=late), 3)
        report = self.report()
        self.assertEqual(report["status"], "aborted")
        self.assertFalse(report["content_verified_after"])
        self.assertIn("no longer verifies", report["reason"])

    def test_under_load_refuses_a_sealed_corpus_that_does_not_verify(self) -> None:
        bad = mock.patch.object(ab, "corpus_verify", return_value=1)
        self.assertEqual(self.main(self.under_load(), corpus_verify=bad), 2)
        self.assertFalse((self.tmp / "work").exists())

    def test_under_load_skips_the_post_settle_wait(self) -> None:
        args = ab.argparse.Namespace(
            dry_run=False, under_load=True, post_settle_seconds=3600
        )
        with (
            mock.patch.object(ab, "conditions", return_value=cond(False)),
            mock.patch.object(ab.time, "sleep", side_effect=AssertionError("waited")),
        ):
            first, settled = ab.post_settle(args)
        self.assertEqual(first["load1"], 9.0)
        self.assertIs(first, settled)

    def test_gated_post_settle_waits_for_load(self) -> None:
        args = ab.argparse.Namespace(
            dry_run=False, under_load=False, post_settle_seconds=3600
        )
        states = iter([cond(False), cond()])
        with (
            mock.patch.object(ab, "conditions", side_effect=lambda: next(states)),
            mock.patch.object(ab.time, "sleep") as sleep,
        ):
            first, settled = ab.post_settle(args)
        self.assertEqual(sleep.call_count, 1)
        self.assertEqual((first["load1"], settled["load1"]), (9.0, 1.0))

    def test_gated_aborts_on_a_gated_false_row(self) -> None:
        with mock.patch.dict(os.environ, {"STUB_GATED": "false"}):
            self.assertEqual(self.main(self.gated()), 3)
        self.assertIn("a bench row was gated=false", self.report()["reason"])

    def test_gated_mode_still_aborts_on_a_refused_rep(self) -> None:
        self.assertEqual(self.main(self.gated(), parse_bench=refuse_call(2)), 3)
        self.assertIn("bench refused", self.report()["reason"])

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
        md = (self.tmp / EV).read_text()
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

    # --- OI-1003-Q105: the flake-pinned rclone is the rclone of every sample.

    def choose(
        self, given: Path | None, reason: str | None, mode: str, **patches: object
    ) -> tuple[dict[str, object], str | None]:
        targets = {
            "flake_rclone": mock.patch.object(
                ab, "flake_rclone", return_value=self.pinned
            ),
            "rclone_version": mock.patch.object(
                ab, "rclone_version", return_value=ab.RCLONE_PIN["version"]
            ),
            "system_key": mock.patch.object(
                ab, "system_key", return_value="test-system"
            ),
            "rclone_pin": mock.patch.dict(
                ab.RCLONE_PIN["store_paths"],
                {"test-system": str(self.pinned.parent.parent)},
            ),
        }
        targets.update(patches)
        with contextlib.ExitStack() as stack:
            for patcher in targets.values():
                stack.enter_context(patcher)
            return ab.choose_rclone(
                self.tmp, str(given) if given else None, reason, mode
            )

    def test_rclone_default_is_the_flake_pinned_build_and_is_recorded(self) -> None:
        (self.tmp / "flake.lock").write_text(
            json.dumps(
                {
                    "root": "root",
                    "nodes": {
                        "root": {"inputs": {"nixpkgs": "np"}},
                        "np": {"locked": {"rev": "abc123"}},
                    },
                }
            )
        )
        for mode in ("gated", "under-load", "dry-run"):
            record, problem = self.choose(None, None, mode)
            self.assertIsNone(problem, mode)
            self.assertEqual(record["binary"], str(self.pinned))
            self.assertEqual(record["store_path"], str(self.pinned.parent.parent))
            self.assertEqual(record["version"], "rclone v1.74.4")
            self.assertEqual(record["flake_nixpkgs_rev"], "abc123")
            self.assertEqual(len(record["sha256"]), 64)
            self.assertTrue(record["is_flake_pinned"])
            self.assertTrue(record["matches_committed_pin"])
            self.assertIsNone(record["override"])
            self.assertEqual(
                record["committed_pin"]["store_path"], str(self.pinned.parent.parent)
            )
        # Naming the pinned build by its path is the same build.
        record, problem = self.choose(self.pinned, None, "gated")
        self.assertIsNone(problem)
        self.assertTrue(record["is_flake_pinned"])

    def test_rclone_other_binary_is_refused_without_a_recorded_override(self) -> None:
        for mode in ("gated", "under-load"):
            record, problem = self.choose(self.other, None, mode)
            self.assertIn("is not the flake-pinned rclone", problem)
            self.assertIn("OI-1003-Q105", problem)
            self.assertFalse(record["is_flake_pinned"])
            for blank in ("", "   "):
                self.assertIsNotNone(self.choose(self.other, blank, mode)[1])
            record, problem = self.choose(self.other, "testing 1.75", mode)
            self.assertIsNone(problem)
            self.assertEqual(record["override"], {"reason": "testing 1.75"})
            self.assertEqual(record["binary"], str(self.other))
            self.assertEqual(record["flake_pinned_binary"], str(self.pinned))
            self.assertFalse(record["matches_committed_pin"])
            # A reason with the pinned build is a mistake, not an override.
            for given in (None, self.pinned):
                self.assertIn(
                    "only for a --rclone that is not",
                    self.choose(given, "why", mode)[1],
                )

    def test_rclone_a_moved_flake_is_refused_until_the_pin_records_it(self) -> None:
        moved = mock.patch.object(ab, "flake_rclone", return_value=self.other)
        record, problem = self.choose(None, None, "gated", flake_rclone=moved)
        self.assertIn("a deliberate, recorded change (OI-1003-Q105)", problem)
        self.assertIn("update RCLONE_PIN", problem)
        self.assertTrue(record["is_flake_pinned"])
        self.assertFalse(record["matches_committed_pin"])
        # Same store path, another version string: still not the pin.
        newer = mock.patch.object(ab, "rclone_version", return_value="rclone v9")
        self.assertIsNotNone(self.choose(None, None, "gated", rclone_version=newer)[1])
        # A system the pin does not list.
        unlisted = mock.patch.object(ab, "system_key", return_value="riscv64-linux")
        record, problem = self.choose(None, None, "gated", system_key=unlisted)
        self.assertIn("committed pin for riscv64-linux is None", problem)
        # Under load it is recorded, not refused; a dry run refuses nothing.
        record, problem = self.choose(None, None, "under-load", flake_rclone=moved)
        self.assertIsNone(problem)
        self.assertFalse(record["matches_committed_pin"])
        self.assertIsNone(self.choose(self.other, None, "dry-run")[1])

    def test_rclone_that_cannot_be_resolved_or_read_is_refused(self) -> None:
        broken = mock.patch.object(
            ab, "flake_rclone", side_effect=ab.subprocess.CalledProcessError(1, "nix")
        )
        unreadable = mock.patch.object(ab, "rclone_version", return_value=None)
        for mode in ("gated", "under-load"):
            _record, problem = self.choose(None, None, mode, flake_rclone=broken)
            self.assertIn("cannot resolve the flake-pinned rclone", problem)
            # Even an override must record the pinned build it replaces.
            self.assertIsNotNone(
                self.choose(self.other, "why", mode, flake_rclone=broken)[1]
            )
            _record, problem = self.choose(None, None, mode, rclone_version=unreadable)
            self.assertIn("cannot read", problem)
        # A dry run with --rclone never calls nix.
        never = mock.patch.object(ab, "flake_rclone", side_effect=AssertionError)
        record, problem = self.choose(self.other, None, "dry-run", flake_rclone=never)
        self.assertIsNone(problem)
        self.assertIsNone(record["flake_pinned_binary"])

    def test_flake_rclone_takes_the_output_that_has_the_binary(self) -> None:
        man = self.tmp / "store" / "aaa-rclone-1.74.4-man"
        man.mkdir()
        out = mock.Mock(stdout=f"{man}\n{self.pinned.parent.parent}\n")
        with mock.patch.object(ab.subprocess, "run", return_value=out) as run:
            self.assertEqual(ab.flake_rclone(self.tmp), self.pinned)
            self.assertEqual(
                run.call_args.args[0][-3:],
                ["--inputs-from", str(self.tmp), "nixpkgs#rclone"],
            )
        out.stdout = f"{self.pinned.parent.parent}\n{man}\n"
        with mock.patch.object(ab.subprocess, "run", return_value=out):
            self.assertEqual(ab.flake_rclone(self.tmp), self.pinned)
        out.stdout = f"{man}\n"
        with mock.patch.object(ab.subprocess, "run", return_value=out):
            self.assertRaises(FileNotFoundError, ab.flake_rclone, self.tmp)

    def test_rclone_version_and_system_key(self) -> None:
        fake = self.tmp / "rclone-fake"
        fake.write_text("#!/bin/sh\necho 'rclone v1.74.4'\necho '- os/version: x'\n")
        fake.chmod(0o755)
        self.assertEqual(ab.rclone_version(fake), "rclone v1.74.4")
        fake.write_text("#!/bin/sh\nexit 3\n")
        self.assertIsNone(ab.rclone_version(fake))
        self.assertIsNone(ab.rclone_version(self.tmp / "absent"))
        for machine, system, key in (
            ("x86_64", "Linux", "x86_64-linux"),
            ("arm64", "Darwin", "aarch64-darwin"),
            ("aarch64", "Linux", "aarch64-linux"),
        ):
            with (
                mock.patch.object(ab.platform, "machine", return_value=machine),
                mock.patch.object(ab.platform, "system", return_value=system),
            ):
                self.assertEqual(ab.system_key(), key)

    def test_the_committed_rclone_pin_follows_the_repo_flake_lock(self) -> None:
        """A lock update that moves nixpkgs must also record the rclone pin."""
        pin = ab.RCLONE_PIN
        self.assertEqual(ab.flake_nixpkgs_rev(HERE.parents[2]), pin["nixpkgs_rev"])
        self.assertRegex(pin["version"], r"^rclone v\d+\.\d+\.\d+$")
        self.assertRegex(pin["recorded"], r"^\d{4}-\d{2}-\d{2}$")
        number = pin["version"].removeprefix("rclone v")
        self.assertIn("x86_64-linux", pin["store_paths"])
        for system, path in pin["store_paths"].items():
            self.assertRegex(
                path, rf"^/nix/store/[0-9a-z]{{32}}-rclone-{number}$", system
            )
        self.assertIsNone(ab.flake_nixpkgs_rev(self.tmp / "absent"))

    def test_gated_refuses_another_rclone_before_anything_is_written(self) -> None:
        code, out = self.run_on([*self.gated(), "--rclone", str(self.other)])
        self.assertEqual(code, 2)
        self.assertIn("refused: --rclone", out)
        self.assertIn("OI-1003-Q105", out)
        code, out = self.run_on([*self.under_load(), "--rclone", str(self.other)])
        self.assertEqual(code, 2)
        moved = mock.patch.object(ab, "flake_rclone", return_value=self.other)
        code, out = self.run_on(self.gated(), flake_rclone=moved)
        self.assertEqual(code, 2)
        self.assertIn("update RCLONE_PIN", out)
        self.assertFalse((self.tmp / "work").exists())
        self.assertFalse((self.tmp / EV).exists())
        # Under load a moved flake is recorded and the sample runs.
        self.assertEqual(self.main(self.under_load(), flake_rclone=moved), 0)
        self.assertFalse(self.report()["rclone_record"]["matches_committed_pin"])
        self.assertIn("**not** the committed pin", self.evidence_md())

    def test_every_report_records_the_rclone_store_path_and_version(self) -> None:
        store = str(self.pinned.parent.parent)
        runs = (
            (self.gated(), self.tmp / EV),
            (self.under_load(), self.tmp / "r23-underload-test.md"),
            (["--dry-run"], None),
        )
        for argv, evidence in runs:
            self.setUp()
            store = str(self.pinned.parent.parent)
            with mock.patch.dict(os.environ, {"STUB_VERDICT": "pass"}):
                code, out = self.run_on(argv)
            self.assertEqual(code, 0, argv)
            report = self.report()
            self.assertEqual(report["rclone"], str(self.pinned))
            self.assertEqual(report["rclone_store_path"], store)
            self.assertEqual(report["rclone_version"], "rclone v1.74.4")
            self.assertEqual(report["rclone_version_bench_header"], "rclone v1.74.4")
            self.assertTrue(report["rclone_record"]["is_flake_pinned"])
            self.assertFalse(report["gate"]["rclone_override"])
            self.assertIn(" rclone_pinned=true ", out)
            # Each rep ran that binary.
            for rep in report["reps"]:
                at = rep["command"].index("--rclone")
                self.assertEqual(rep["command"][at + 1], str(self.pinned))
            md = (
                evidence or next((self.tmp / "work").glob("r23-dryrun-*Z.md"))
            ).read_text()
            self.assertIn(f"- rclone: store path `{store}`, version", md)
            self.assertIn("`rclone v1.74.4`", md)
            self.assertIn("OI-1003-Q105", md)
        # An aborted sample records it too.
        self.setUp()
        busy = mock.patch.object(ab, "conditions", return_value=cond(False))
        self.assertEqual(
            self.main([*self.gated(), "--settle-seconds", "0"], conditions=busy), 2
        )
        self.assertEqual(self.report()["rclone_version"], "rclone v1.74.4")
        self.assertIn("- rclone: store path `", (self.tmp / EV).read_text())

    def test_an_rclone_override_is_recorded_and_never_of_record(self) -> None:
        linux = mock.patch.object(ab.platform, "system", return_value="Linux")
        argv = [a for a in self.gated() if a not in ("--evidence", str(self.tmp / EV))]
        argv += ["--rclone", str(self.other), "--rclone-override-reason", "try 1.75"]
        with mock.patch.dict(os.environ, {"STUB_VERDICT": "pass"}):
            code, out = self.run_on(
                argv,
                system=linux,
                build=self.linux_builds(b_native=True),
                host_identity=self.record_rig(),
            )
        self.assertEqual(code, 0)
        report = self.report()
        gate = report["gate"]
        self.assertEqual(gate["verdict"], "PASS-RCLONE-OVERRIDE")
        self.assertTrue(gate["rclone_override"])
        self.assertFalse(gate["of_record"])
        self.assertEqual(report["rclone"], str(self.other))
        self.assertEqual(report["rclone_record"]["override"], {"reason": "try 1.75"})
        self.assertIn(" of_record=false rclone_pinned=false ", out)
        self.assertNotRegex(out, r"gate=PASS(\s|$)")
        md = next((self.tmp / "docs" / "evidence").iterdir()).read_text()
        self.assertIn("(DRAFT, RCLONE OVERRIDE)", md.splitlines()[0])
        self.assertIn("try 1.75", md)
        self.assertIn("**not the flake-pinned build**", md)
        self.assertIn("not a verdict of record", md)
        # The equal-durability medians ride beside the verdict (OI-1003-Q107).
        self.assertIn("## Equal durability (informational, OI-1003-Q107)", md)
        self.assertIn("| 100.000 | 110.000 |", md)
        # With no A control as well, both tokens are in the verdict.
        self.setUp()
        argv = [*self.no_a(), "--rclone", str(self.other)]
        argv += ["--rclone-override-reason", "try 1.75"]
        with mock.patch.dict(os.environ, {"STUB_VERDICT": "fail"}):
            self.assertEqual(self.main(argv), 0)
        self.assertEqual(
            self.report()["gate"]["verdict"], "FAIL-NO-A-CONTROL-RCLONE-OVERRIDE"
        )

    def test_a_rep_that_ran_another_rclone_version_aborts(self) -> None:
        with mock.patch.dict(os.environ, {"STUB_RCLONE": "rclone v1.75.0"}):
            self.assertEqual(self.main(self.gated()), 3)
        report = self.report()
        self.assertEqual(report["status"], "aborted")
        self.assertIn("bench header rclone_version", report["reason"])
        self.assertEqual(len(report["reps"]), 1)
        self.setUp()
        with mock.patch.dict(os.environ, {"STUB_RCLONE": "rclone v1.75.0"}):
            self.assertEqual(self.main(self.under_load()), 3)
        # A dry run records the version and does not check it.
        self.setUp()
        with mock.patch.dict(os.environ, {"STUB_RCLONE": "rclone v1.75.0"}):
            self.assertEqual(self.main(["--dry-run"]), 0)

    # --- OI-1003-Q103: the rig of record has its own pinned A control.

    def test_the_rig_baseline_pins_are_complete(self) -> None:
        self.assertEqual(sorted(ab.RIG_BASELINE_A), sorted(ab.RIG_OF_RECORD))
        for rig, pin in ab.RIG_BASELINE_A.items():
            self.assertRegex(pin["sha"], r"^[0-9a-f]{40}$", rig)
            self.assertRegex(pin["pinned"], r"^\d{4}-\d{2}-\d{2}$", rig)
            self.assertGreater(len(pin["reason"]), 40, rig)
            self.assertFalse(pin["sha"].startswith(ab.DEFAULT_A), rig)
        self.assertEqual(ab.DEFAULT_A, "7c3ecc7")
        self.assertIn("OI-1003-Q103", ab.RULINGS)
        self.assertIn("OI-1003-Q105", ab.RULINGS)
        self.assertIn("never decides the gate", ab.A_PURPOSE)

    def built_revs(self, argv: list[str], **patches: object) -> tuple[int, list[str]]:
        revs: list[str] = []

        def record(_repo: Path, rev: str, *_a: object) -> dict[str, object]:
            revs.append(rev)
            sha = rev if len(rev) == 40 else self.info["sha"]
            return {**self.info, "sha": sha, "compiled_in_this_run": False}

        Path(f"{self.info['binary']}.native").write_text("")
        build = mock.patch.object(ab, "build", side_effect=record)
        return self.main(argv, build=build, **patches), revs

    def test_the_rig_of_record_runs_its_pinned_a(self) -> None:
        pin = ab.RIG_BASELINE_A["mbp-13"]
        linux = mock.patch.object(ab.platform, "system", return_value="Linux")
        argv = [a for a in self.gated() if a not in ("--evidence", str(self.tmp / EV))]
        with mock.patch.dict(os.environ, {"STUB_VERDICT": "pass"}):
            code, revs = self.built_revs(
                argv, system=linux, host_identity=self.record_rig()
            )
        self.assertEqual(code, 0)
        self.assertEqual(revs, [ab.DEFAULT_B, pin["sha"], ab.DEFAULT_V4])
        report = self.report()
        self.assertEqual([r["label"] for r in report["reps"]], list("BABAB") + ["V4"])
        self.assertEqual(report["baseline_a"]["sha"], pin["sha"])
        self.assertEqual(report["baseline_a"]["pin"], pin)
        self.assertTrue(report["gate"]["of_record"])
        md = next((self.tmp / "docs" / "evidence").iterdir()).read_text()
        self.assertIn(f"- A control: `{pin['sha'][:12]}`, this rig's pinned", md)
        self.assertIn(f"OI-1003-Q103, pinned {pin['pinned']}", md)
        self.assertIn("drift of the rig", md)
        # --build-only on the rig prebuilds the same A.
        self.setUp()
        code, revs = self.built_revs(
            ["--build-only"], system=linux, host_identity=self.record_rig()
        )
        self.assertEqual(code, 0)
        self.assertEqual(revs, [ab.DEFAULT_B, pin["sha"], ab.DEFAULT_V4])

    def test_the_rig_of_record_refuses_any_other_a(self) -> None:
        linux = mock.patch.object(ab.platform, "system", return_value="Linux")
        other = mock.patch.object(ab, "git", return_value="c" * 40)
        unknown = mock.patch.object(
            ab, "git", side_effect=ab.subprocess.CalledProcessError(128, "git")
        )
        argv = [a for a in self.gated() if a not in ("--evidence", str(self.tmp / EV))]
        for git in (other, unknown):
            code, out = self.run_on(
                [*argv, "--rev-a", ab.DEFAULT_A],
                system=linux,
                host_identity=self.record_rig(),
                git=git,
            )
            self.assertEqual(code, 2)
            self.assertIn("the A control is pinned to", out)
            self.assertIn("OI-1003-Q103", out)
        self.assertFalse((self.tmp / "work").exists())
        self.assertFalse((self.tmp / "docs").exists())

    def test_a_field_host_keeps_the_default_a(self) -> None:
        with mock.patch.dict(os.environ, {"STUB_VERDICT": "pass"}):
            code, revs = self.built_revs(self.gated())
        self.assertEqual(code, 0)
        self.assertEqual(revs, [ab.DEFAULT_B, ab.DEFAULT_A, ab.DEFAULT_V4])
        report = self.report()
        self.assertIsNone(report["baseline_a"]["pin"])
        self.assertIn(
            "- A control: `ffffffffffff` (`7c3ecc7`).", (self.tmp / EV).read_text()
        )
        # A field host may name another A; nothing pins it there.
        self.setUp()
        code, revs = self.built_revs([*self.gated(), "--rev-a", "abc"])
        self.assertEqual((code, revs[1]), (0, "abc"))
        # A B/B/B sample has no A to record.
        self.setUp()
        self.assertEqual(self.main(self.no_a()), 0)
        self.assertIsNone(self.report()["baseline_a"])


if __name__ == "__main__":
    unittest.main()
