"""The documented exit-code table, --dry-run's no-mutation claim, and --progress.

Every assertion here is about the *contract a caller can rely on*: the number
the process returns, the fact that a dry run wrote nothing, and the fact that
progress never reaches stdout. The review item these pin is [P1/L] "No
dry-run, no progress, no resume, 34 required arguments" -- the three cheap
halves of it.
"""

from __future__ import annotations

import argparse
import errno
import io
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import threading
import unittest
from unittest import mock

from bulkload_lib.cli import (
    DRY_RUN_BUILDERS,
    EXIT_CODES,
    Progress,
    build_parser,
    exit_code_for,
    progress_enabled,
)
from bulkload_lib.model import (
    EXIT_BOOTSTRAP,
    EXIT_DESTINATION,
    EXIT_EPOCH,
    EXIT_INTERNAL,
    EXIT_INTERRUPTED,
    EXIT_OK,
    EXIT_REFUSED,
    EXIT_USAGE,
    BulkloadError,
    DestinationRefusal,
    EpochRefusal,
)

from test_bulkload import CutoverFixture, compile_agent_plan, gnu_rsync_path


ROOT = Path(__file__).parents[1]
LAUNCHER = ROOT / ".agents/skills/bulkload/scripts/bulkload.py"


def launch(*arguments: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(LAUNCHER), *arguments],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env={**os.environ, "PYTHONDONTWRITEBYTECODE": "1"},
    )


class ExitCodeTableTests(unittest.TestCase):
    def test_the_table_is_complete_and_unique(self) -> None:
        codes = [code for code, _ in EXIT_CODES]
        self.assertEqual(len(codes), len(set(codes)))
        self.assertEqual(
            set(codes),
            {
                EXIT_OK,
                EXIT_USAGE,
                EXIT_BOOTSTRAP,
                EXIT_EPOCH,
                EXIT_REFUSED,
                EXIT_DESTINATION,
                EXIT_INTERNAL,
                EXIT_INTERRUPTED,
            },
        )
        self.assertEqual(codes, sorted(codes))
        for _, meaning in EXIT_CODES:
            self.assertTrue(meaning.strip())

    def test_every_code_is_documented_in_readme_and_skill(self) -> None:
        readme = (ROOT / "README.md").read_text(encoding="utf-8")
        skill = (ROOT / ".agents/skills/bulkload/SKILL.md").read_text(encoding="utf-8")
        for code, _ in EXIT_CODES:
            for document, name in ((readme, "README.md"), (skill, "SKILL.md")):
                self.assertIn(f"| `{code}` |", document, name)

    def test_refusal_classes_carry_their_documented_code(self) -> None:
        self.assertEqual(exit_code_for(BulkloadError("custody")), EXIT_REFUSED)
        self.assertEqual(exit_code_for(EpochRefusal("moved")), EXIT_EPOCH)
        self.assertEqual(exit_code_for(DestinationRefusal("full")), EXIT_DESTINATION)
        self.assertTrue(issubclass(EpochRefusal, BulkloadError))
        self.assertTrue(issubclass(DestinationRefusal, BulkloadError))

    def test_a_full_filesystem_is_a_destination_refusal(self) -> None:
        self.assertEqual(
            exit_code_for(OSError(errno.ENOSPC, "No space left on device")),
            EXIT_DESTINATION,
        )
        self.assertEqual(
            exit_code_for(OSError(errno.ENOENT, "No such file")), EXIT_REFUSED
        )
        self.assertEqual(exit_code_for(sqlite3.DatabaseError("locked")), EXIT_REFUSED)

    def test_an_unexpected_error_is_internal_not_a_refusal(self) -> None:
        self.assertEqual(exit_code_for(KeyError("catalog")), EXIT_INTERNAL)
        self.assertEqual(exit_code_for(TypeError("bad")), EXIT_INTERNAL)


class UsageExitTests(unittest.TestCase):
    """Usage failures exit 1, so exit 2 stays the launcher's bootstrap code."""

    def _usage(self, *arguments: str) -> None:
        parser = build_parser()
        with mock.patch.object(sys, "stderr", io.StringIO()):
            with self.assertRaises(SystemExit) as raised:
                parser.parse_args(list(arguments))
        self.assertEqual(raised.exception.code, EXIT_USAGE)

    def test_missing_command_is_usage(self) -> None:
        self._usage()

    def test_unknown_option_is_usage(self) -> None:
        self._usage("agent-plan", "--nope")

    def test_bad_argument_type_is_usage(self) -> None:
        self._usage(
            "agent-capture",
            "--role",
            "source",
            "--home",
            "/tmp",
            "--git-root",
            "/tmp/git",
            "--rsync-path",
            "/bin/rsync",
            "--path-map",
            "/tmp=/dest",
            "--max-files",
            "not-an-integer",
            "--output",
            "/tmp/out.json",
        )

    def test_malformed_mapping_is_usage(self) -> None:
        self._usage(
            "agent-capture",
            "--role",
            "source",
            "--home",
            "/tmp",
            "--git-root",
            "/tmp/git",
            "--rsync-path",
            "/bin/rsync",
            "--path-map",
            "no-equals-sign",
            "--output",
            "/tmp/out.json",
        )

    def test_help_and_version_still_exit_zero(self) -> None:
        for arguments in (["--help"], ["--version"], ["agent-apply", "--help"]):
            with mock.patch.object(sys, "stdout", io.StringIO()):
                with self.assertRaises(SystemExit) as raised:
                    build_parser().parse_args(arguments)
            self.assertEqual(raised.exception.code, EXIT_OK, arguments)

    def test_the_launcher_reports_usage_as_one_and_bootstrap_as_two(self) -> None:
        usage = launch("agent-plan")
        self.assertEqual(usage.returncode, EXIT_USAGE, usage.stderr)
        self.assertIn("usage error", usage.stderr)
        bootstrap = subprocess.run(
            [sys.executable, str(LAUNCHER), "--version"],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.assertEqual(bootstrap.returncode, EXIT_BOOTSTRAP, bootstrap.stderr)


class HelpSurfaceTests(unittest.TestCase):
    def test_every_option_documents_itself(self) -> None:
        """The AX complaint was 34 required arguments with no help at all."""
        parser = build_parser()
        subparsers = [
            action
            for action in parser._actions
            if isinstance(action, argparse._SubParsersAction)
        ]
        self.assertEqual(len(subparsers), 1)
        undocumented: list[str] = []
        for name, command in subparsers[0].choices.items():
            for action in command._actions:
                if action.dest == "help":
                    continue
                if not (action.help or "").strip():
                    undocumented.append(f"{name} {action.dest}")
        for action in parser._actions:
            if (
                action.dest not in {"help", "command"}
                and not (action.help or "").strip()
            ):
                undocumented.append(f"bulkload {action.dest}")
        self.assertEqual(undocumented, [])

    def test_the_exit_table_is_in_the_epilog(self) -> None:
        epilog = build_parser().epilog or ""
        for code, _ in EXIT_CODES:
            self.assertIn(str(code), epilog)


class InternalErrorTests(unittest.TestCase):
    def test_malformed_evidence_exits_six_without_a_traceback(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            bad = root / "bad.json"
            bad.write_text(json.dumps({"not": "a capture"}), encoding="utf-8")
            result = launch(
                "agent-plan",
                "--source-a",
                str(bad),
                "--source-b",
                str(bad),
                "--destination-a",
                str(bad),
                "--destination-b",
                str(bad),
                "--output",
                str(root / "plan.json"),
            )
        self.assertEqual(result.returncode, EXIT_INTERNAL, result.stderr)
        self.assertNotIn("Traceback", result.stderr)
        self.assertIn("bulkload: FAIL[6]", result.stderr)
        self.assertEqual(len(result.stderr.strip().splitlines()), 1)

    def test_a_missing_input_is_a_refusal_not_an_internal_error(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            result = launch(
                "agent-rollback",
                "--apply-receipt",
                str(root / "absent.json"),
                "--accept-receipt-sha256",
                "0" * 64,
                "--output",
                str(root / "receipt.json"),
            )
        self.assertEqual(result.returncode, EXIT_REFUSED, result.stderr)
        self.assertIn("bulkload: FAIL[4]", result.stderr)


class DryRunTests(unittest.TestCase):
    maxDiff = None

    def _report(self, *arguments: str) -> tuple[dict, subprocess.CompletedProcess[str]]:
        result = launch(*arguments)
        return json.loads(result.stdout), result

    def test_capture_dry_run_writes_nothing_and_names_its_mutations(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            home = root / "home"
            (home / "git").mkdir(parents=True)
            evidence = root / "evidence"
            evidence.mkdir()
            report, result = self._report(
                "agent-capture",
                "--role",
                "source",
                "--home",
                str(home),
                "--git-root",
                str(home / "git"),
                "--rsync-path",
                str(gnu_rsync_path()),
                "--path-map",
                f"{home}=/destination/home",
                "--output",
                str(evidence / "source-a.json"),
                "--dry-run",
            )
            self.assertEqual(result.returncode, EXIT_OK, result.stderr)
            self.assertEqual(report["report"], "bulkload-dry-run")
            self.assertTrue(report["dry_run"])
            self.assertEqual(report["refusals"], [])
            self.assertEqual(report["live_destination_mutations"], 0)
            kinds = {item["kind"] for item in report["mutations"]}
            self.assertEqual(kinds, {"evidence-write", "live-snapshot-custody-create"})
            self.assertTrue(report["not_evaluated"])
            self.assertEqual(list(evidence.iterdir()), [])

    def test_capture_dry_run_reaches_the_real_refusal(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            home = root / "home"
            (home / "git").mkdir(parents=True)
            report, result = self._report(
                "agent-capture",
                "--role",
                "source",
                "--home",
                str(home),
                "--git-root",
                str(home / "git"),
                "--rsync-path",
                str(gnu_rsync_path()),
                "--path-map",
                f"{home}=/destination/home",
                "--output",
                str(home / "inside.json"),
                "--dry-run",
            )
            self.assertEqual(result.returncode, EXIT_REFUSED, result.stdout)
            self.assertEqual(report["exit_code"], EXIT_REFUSED)
            self.assertEqual(len(report["refusals"]), 1)
            self.assertIn("overlaps live root", report["refusals"][0]["message"])
            self.assertFalse((home / "inside.json").exists())

    def test_a_dry_run_reports_malformed_evidence_instead_of_raising(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            bad = root / "bad.json"
            bad.write_text(json.dumps({"not": "a capture"}), encoding="utf-8")
            report, result = self._report(
                "agent-plan",
                "--source-a",
                str(bad),
                "--source-b",
                str(bad),
                "--destination-a",
                str(bad),
                "--destination-b",
                str(bad),
                "--output",
                str(root / "plan.json"),
                "--dry-run",
            )
            self.assertEqual(result.returncode, EXIT_INTERNAL, result.stderr)
            self.assertEqual(report["refusals"][0]["class"], "KeyError")
            self.assertFalse((root / "plan.json").exists())


class DryRunPlanBoundTests(unittest.TestCase):
    """Stage and apply dry runs against a real compiled plan."""

    @classmethod
    def setUpClass(cls) -> None:
        cls._temporary = tempfile.TemporaryDirectory()
        root = Path(cls._temporary.name)
        fixture = CutoverFixture(root / "cutover")
        plan = compile_agent_plan(
            fixture.capture("source"),
            fixture.capture("source"),
            fixture.capture("destination"),
            fixture.capture("destination"),
        )
        cls.plan_path = root / "plan.json"
        cls.plan_path.write_text(json.dumps(plan), encoding="utf-8")
        cls.plan = plan
        cls.evidence = root / "evidence"
        cls.evidence.mkdir()
        cls.stage_root = root / "stage-that-must-not-exist"

    @classmethod
    def tearDownClass(cls) -> None:
        cls._temporary.cleanup()

    def test_stage_dry_run_names_the_sidecars_and_creates_nothing(self) -> None:
        result = launch(
            "agent-stage",
            "--phase",
            "preseed",
            "--transport-mode",
            "prepare",
            "--plan",
            str(self.plan_path),
            "--accept-plan-sha256",
            self.plan["plan_sha256"],
            "--stage-root",
            str(self.stage_root),
            "--output",
            str(self.evidence / "prepare.json"),
            "--dry-run",
        )
        self.assertEqual(result.returncode, EXIT_OK, result.stderr)
        report = json.loads(result.stdout)
        targets = {item["kind"]: item["target"] for item in report["mutations"]}
        self.assertEqual(
            set(targets),
            {
                "evidence-write",
                "stage-root-create",
                "quarantine-create",
                "allowlist-write",
                "prepare-receipt-write",
            },
        )
        self.assertTrue(
            targets["allowlist-write"].endswith(".transport-allowlist-preseed.nul")
        )
        self.assertTrue(
            targets["prepare-receipt-write"].endswith(".prepare-receipt-preseed.json")
        )
        self.assertEqual(report["plan"]["plan_sha256"], self.plan["plan_sha256"])
        self.assertTrue(report["plan"]["ready"])
        self.assertFalse(self.stage_root.exists())
        self.assertFalse((self.evidence / "prepare.json").exists())

    def test_stage_dry_run_refuses_the_wrong_accepted_digest(self) -> None:
        result = launch(
            "agent-stage",
            "--phase",
            "final",
            "--plan",
            str(self.plan_path),
            "--accept-plan-sha256",
            "0" * 64,
            "--stage-root",
            str(self.stage_root),
            "--output",
            str(self.evidence / "final.json"),
            "--dry-run",
        )
        self.assertEqual(result.returncode, EXIT_REFUSED, result.stderr)
        report = json.loads(result.stdout)
        self.assertIn(
            "accepted plan digest does not match",
            report["refusals"][0]["message"],
        )
        self.assertFalse(self.stage_root.exists())

    def test_stage_dry_run_refuses_a_push_without_its_receipts(self) -> None:
        result = launch(
            "agent-stage",
            "--phase",
            "preseed",
            "--transport-mode",
            "push",
            "--accept-plan-sha256",
            self.plan["plan_sha256"],
            "--stage-root",
            str(self.stage_root),
            "--output",
            str(self.evidence / "push.json"),
            "--dry-run",
        )
        self.assertEqual(result.returncode, EXIT_REFUSED, result.stderr)
        report = json.loads(result.stdout)
        self.assertIn("transport push requires", report["refusals"][0]["message"])

    def test_apply_dry_run_counts_live_mutations_and_writes_no_journal(self) -> None:
        journal = self.evidence / "journal.json"
        rollback = self.evidence / "rollback-root"
        result = launch(
            "agent-apply",
            "--plan",
            str(self.plan_path),
            "--stage-receipt",
            str(self.plan_path),
            "--accept-plan-sha256",
            self.plan["plan_sha256"],
            "--journal",
            str(journal),
            "--rollback-root",
            str(rollback),
            "--output",
            str(self.evidence / "apply.json"),
            "--dry-run",
        )
        report = json.loads(result.stdout)
        # The stage receipt here is deliberately the plan: apply must refuse it
        # in the dry run, from `validate_stage_receipt`, before any journal.
        self.assertNotEqual(result.returncode, EXIT_OK)
        self.assertTrue(report["refusals"])
        self.assertFalse(journal.exists())
        self.assertFalse(rollback.exists())


class ProgressTests(unittest.TestCase):
    def test_progress_is_silent_when_disabled(self) -> None:
        stream = io.StringIO()
        progress = Progress(False, command="agent-plan", stream=stream, interval=0)
        with progress:
            progress.phase("preflight")
        self.assertEqual(stream.getvalue(), "")

    def test_progress_reports_phases_on_its_stream(self) -> None:
        stream = io.StringIO()
        progress = Progress(True, command="agent-plan", stream=stream, interval=0)
        with progress:
            progress.phase("preflight")
            progress.phase("agent-plan")
        lines = stream.getvalue().strip().splitlines()
        self.assertEqual(len(lines), 2)
        self.assertTrue(all(line.startswith("bulkload: phase:") for line in lines))
        self.assertIn("command=agent-plan", lines[0])
        self.assertIn("phase=preflight", lines[0])
        self.assertIn("elapsed=", lines[0])

    def test_the_heartbeat_proves_a_long_verb_is_alive(self) -> None:
        stream = io.StringIO()
        seen = threading.Event()

        class Watched(io.StringIO):
            def write(self, value: str) -> int:
                if "alive" in value:
                    seen.set()
                return stream.write(value)

        progress = Progress(
            True, command="agent-capture", stream=Watched(), interval=0.01
        )
        with progress:
            self.assertTrue(seen.wait(5.0))
        self.assertIn("bulkload: alive:", stream.getvalue())

    def test_progress_defaults_to_an_interactive_stderr(self) -> None:
        arguments = build_parser().parse_args(
            [
                "agent-verify",
                "--plan",
                "/tmp/plan.json",
                "--stage-receipt",
                "/tmp/stage.json",
                "--apply-receipt",
                "/tmp/apply.json",
                "--output",
                "/tmp/out.json",
            ]
        )
        self.assertIsNone(arguments.progress)
        with mock.patch("bulkload_lib.cli._stderr_is_tty", return_value=True):
            self.assertTrue(progress_enabled(arguments))
        with mock.patch("bulkload_lib.cli._stderr_is_tty", return_value=False):
            self.assertFalse(progress_enabled(arguments))
        arguments.progress = True
        with mock.patch("bulkload_lib.cli._stderr_is_tty", return_value=False):
            self.assertTrue(progress_enabled(arguments))

    def test_progress_never_reaches_stdout(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            home = root / "home"
            (home / "git").mkdir(parents=True)
            result = launch(
                "agent-capture",
                "--role",
                "source",
                "--home",
                str(home),
                "--git-root",
                str(home / "git"),
                "--rsync-path",
                str(gnu_rsync_path()),
                "--path-map",
                f"{home}=/destination/home",
                "--output",
                str(root / "source-a.json"),
                "--dry-run",
                "--progress",
            )
        self.assertEqual(result.returncode, EXIT_OK, result.stderr)
        self.assertIn("bulkload: phase:", result.stderr)
        self.assertNotIn("bulkload:", result.stdout)
        json.loads(result.stdout)


class DryRunSurfaceTests(unittest.TestCase):
    def test_dry_run_is_offered_exactly_where_it_is_implemented(self) -> None:
        parser = build_parser()
        subparsers = next(
            action
            for action in parser._actions
            if isinstance(action, argparse._SubParsersAction)
        )
        offered = {
            name
            for name, command in subparsers.choices.items()
            if any(action.dest == "dry_run" for action in command._actions)
        }
        self.assertEqual(
            offered, {"agent-capture", "agent-plan", "agent-stage", "agent-apply"}
        )
        self.assertEqual(offered, set(DRY_RUN_BUILDERS))

    def test_every_verb_offers_progress(self) -> None:
        parser = build_parser()
        subparsers = next(
            action
            for action in parser._actions
            if isinstance(action, argparse._SubParsersAction)
        )
        for name, command in subparsers.choices.items():
            self.assertTrue(
                any(action.dest == "progress" for action in command._actions), name
            )


if __name__ == "__main__":
    unittest.main()
