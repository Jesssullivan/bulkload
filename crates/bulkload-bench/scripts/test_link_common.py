#!/usr/bin/env python3
"""Unit tests for link_common.py, the shared pieces of m3_link.py (#246).

The tests of what survived gate_b.py's retirement (N03, TIN-4543 63feb3cf,
2026-10-17), moved from test_gate_b.py; the gate (b) runner's tests went
with it.

Run: python3 crates/bulkload-bench/scripts/test_link_common.py
Everything runs on this host with fakes: stub agents, a loopback ssh shim
and the source helper executed locally. Nothing here reaches another host.
"""

from __future__ import annotations

import importlib.util
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
SPEC = importlib.util.spec_from_file_location("link_common", HERE / "link_common.py")
gb = importlib.util.module_from_spec(SPEC)
sys.modules["link_common"] = gb  # dataclasses resolve annotations through it
SPEC.loader.exec_module(gb)
# The source helper, executed locally (the same text the source host runs).
HELPER: dict[str, object] = {"__name__": "link_common_helper"}
exec(compile(gb.REMOTE_HELPER, "link_common_helper", "exec"), HELPER)  # noqa: S102


def executable(path: Path, text: str) -> Path:
    path.write_text(text)
    path.chmod(path.stat().st_mode | stat.S_IXUSR)
    return path


def conditions(load1: float = 1.0, power: str = "ac") -> dict[str, object]:
    return {"utc": "t", "load1": load1, "power": power, "system": "Darwin"}


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
        # A raised destination limit passes host_problems, but the sample is
        # still judged against the fixed limit.
        self.assertEqual(
            gb.host_problems(conditions(1.0), conditions(64.0), "gated", 1000.0), []
        )

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


class VerdictTests(unittest.TestCase):
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
        self.assertFalse(gb.disk_budget(0, 0, 1, 1, 1)["ok"])

    def test_volume_space_reads_this_filesystem(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            total, available = gb.volume_space(Path(tmp))
        self.assertGreater(total, 0)
        self.assertLessEqual(available, total)


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

    def test_scrub_redacts_nested_strings(self) -> None:
        value = {"a": ["pass = hunter2", 3], "b": ("RCLONE_SFTP_PASS=hunter2",)}
        self.assertNotIn("hunter2", repr(gb.scrub(value)))
        self.assertEqual(gb.scrub({"n": 1, "s": "ok"}), {"n": 1, "s": "ok"})

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
            "bulkload-agent: refused: ITEMS_REFUSED\n"
        )
        parsed = gb.parse_native(stdout, stderr)
        odd = os.fsdecode(b"state/odd \xff.db")
        self.assertEqual(parsed["refusals"], [[odd, "SQLITE_STATE_CHANGED"]])
        self.assertEqual(parsed["serve_max_rss_bytes"], 4096)
        self.assertEqual(parsed["serve"]["priority"], "background")
        self.assertEqual(parsed["transfer"]["bytes_received"], 100)
        self.assertEqual(parsed["final_refusal"], "ITEMS_REFUSED")
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
            out = HELPER["op_manifest"]({"root": str(root)})
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
            HELPER["op_xor"]({"root": str(root), "plan": plan})
            mutated = HELPER["op_manifest"]({"root": str(root)})
            self.assertEqual(
                gb.changed_paths(out["rows"], mutated["rows"]), {"d/a.txt"}
            )
            self.assertEqual(
                (root / "d" / "a.txt").read_bytes()[:1], bytes([ord("x") ^ 0xA5])
            )
            HELPER["op_xor"]({"root": str(root), "plan": plan})
            self.assertEqual(
                HELPER["op_manifest"]({"root": str(root)})["digest"], out["digest"]
            )

    def test_measure_agrees_with_the_manifest_without_hashing(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            sealed = Path(tmp) / "sealed"
            self.tree(sealed / "corpus")
            measured = HELPER["op_measure"]({"corpus": str(sealed)})
            out = HELPER["op_manifest"]({"root": str(sealed / "corpus")})
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
                HELPER["op_measure"]({"corpus": str(sealed / "absent")})

    def test_xor_refuses_a_target_outside_the_copy(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "c"
            self.tree(root)
            with self.assertRaises(RuntimeError):
                HELPER["op_xor"]({"root": str(root), "plan": [["d/link", 1]]})

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
                out = HELPER["op_prepare"](
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
                    HELPER["op_prepare"]({"corpus": str(sealed), "work": str(work)})
            finally:
                for dirpath, _dirs, _files in os.walk(sealed):
                    Path(dirpath).chmod(0o755)

    @unittest.skipIf(os.geteuid() == 0, "root reads a mode-000 directory")
    def test_the_helper_walks_fail_on_an_unreadable_directory(self) -> None:
        """Review, 2026-10-15: os.walk skips an unreadable directory unless told."""
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "t"
            (root / "hidden").mkdir(parents=True)
            (root / "hidden" / "x").write_bytes(b"x")
            (root / "hidden").chmod(0)
            try:
                with mock.patch.dict(HELPER, {"shared_extents": lambda _p: 1}):
                    with self.assertRaises(PermissionError):
                        HELPER["tree_shared"](str(root))
                with self.assertRaises(PermissionError):
                    HELPER["plain_copy"](str(root), str(Path(tmp) / "copy"))
            finally:
                (root / "hidden").chmod(0o755)

    def test_helper_fiemap_is_trusted_only_where_sharing_is_reported(self) -> None:
        """Review, 2026-10-15: CIFS answers FIEMAP but never flags sharing."""
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "f"
            path.write_bytes(b"f" * 4096)
            with mock.patch.dict(HELPER, {"fs_magic": lambda _fd: 0xFF534D42}):
                self.assertIsNone(HELPER["shared_extents"](str(path)))
                if sys.platform.startswith("linux"):
                    self.assertEqual(HELPER["tree_shared"](tmp), "unsupported")
            if not sys.platform.startswith("linux"):
                return
            with mock.patch.dict(HELPER, {"fs_magic": lambda _fd: 0x01021994}):
                self.assertEqual(HELPER["shared_extents"](str(path)), 0)

    def test_prepare_copies_by_read_and_write_and_refuses_a_shared_copy(self) -> None:
        """OI-1003-Q165 (b): no shutil, and a shared extent refuses."""

        class NoShutil:
            def __getattr__(self, name: str) -> object:
                raise AssertionError(f"shutil.{name} used (OI-1003-Q165)")

        with tempfile.TemporaryDirectory() as tmp:
            sealed = Path(tmp) / "sealed"
            self.tree(sealed / "corpus")
            agent = executable(Path(tmp) / "agent", "#!/bin/sh\nexit 0\n")
            args = {
                "corpus": str(sealed),
                "python": sys.executable,
                "agent": str(agent),
                "flags": [],
            }
            with mock.patch.dict(HELPER, {"shutil": NoShutil()}):
                out = HELPER["op_prepare"]({**args, "work": str(Path(tmp) / "w1")})
            copy = Path(out["corpus"])
            self.assertEqual((copy / "d" / "a.txt").read_bytes(), b"x" * 5000)
            self.assertEqual(os.readlink(copy / "d" / "link"), "a.txt")
            self.assertTrue((copy / "empty").is_dir())
            self.assertIn(out["shared_extents"], (0, "unsupported", "n/a"))
            with mock.patch.dict(HELPER, {"tree_shared": lambda _root: 2}):
                with self.assertRaisesRegex(RuntimeError, "shares 2 extent"):
                    HELPER["op_prepare"]({**args, "work": str(Path(tmp) / "w2")})
            # Real FIEMAP where the directory can clone: a reflinked sealed
            # file is shared, its read-and-write copy is not.
            twin = Path(tmp) / "twin"
            try:
                with (
                    open(sealed / "corpus" / "d" / "a.txt", "rb") as inp,
                    open(twin, "xb") as dst,
                ):
                    import fcntl

                    fcntl.ioctl(dst.fileno(), 0x40049409, inp.fileno())
            except OSError as error:
                if os.environ.get("BULKLOAD_P84_REQUIRE_REFLINK"):
                    self.fail(f"required by BULKLOAD_P84_REQUIRE_REFLINK: {error}")
                print(
                    f"link-common no-clone: reflinked half not run ({error})",
                    file=sys.stderr,
                )
                return
            self.assertEqual(
                HELPER["shared_extents"](str(sealed / "corpus" / "d" / "a.txt")), 1
            )
            out = HELPER["op_prepare"]({**args, "work": str(Path(tmp) / "w3")})
            self.assertEqual(out["shared_extents"], 0)

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


if __name__ == "__main__":
    unittest.main()
