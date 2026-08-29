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
    DRY_RUN_NOT_EVALUATED,
    EXIT_CODES,
    Progress,
    build_parser,
    exit_code_for,
    failure_detail,
    progress_enabled,
)
from bulkload_lib.model import (
    EXIT_BOOTSTRAP,
    EXIT_EPOCH,
    EXIT_INTERNAL,
    EXIT_INTERRUPTED,
    EXIT_OK,
    EXIT_REFUSED,
    EXIT_STORAGE,
    EXIT_USAGE,
    BulkloadError,
    EpochRefusal,
    StorageRefusal,
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
                EXIT_STORAGE,
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
        self.assertEqual(exit_code_for(StorageRefusal("full")), EXIT_STORAGE)
        self.assertTrue(issubclass(EpochRefusal, BulkloadError))
        self.assertTrue(issubclass(StorageRefusal, BulkloadError))

    def test_a_full_filesystem_is_a_storage_refusal(self) -> None:
        self.assertEqual(
            exit_code_for(OSError(errno.ENOSPC, "No space left on device")),
            EXIT_STORAGE,
        )
        self.assertEqual(
            exit_code_for(OSError(errno.ENOENT, "No such file")), EXIT_REFUSED
        )
        self.assertEqual(exit_code_for(sqlite3.DatabaseError("locked")), EXIT_REFUSED)

    def test_the_storage_refusal_names_the_volume_not_the_role(self) -> None:
        """5 must not send a full *source* disk to the destination host.

        `require_capacity` guards the source's own live-snapshot custody
        (`scanner.py:3794`, inside `agent-capture`, a verb with no destination
        at all) as well as the destination stage and the rollback root, so the
        class, the table row, and the message are all volume-neutral and the
        message carries the path.
        """
        meaning = dict(EXIT_CODES)[EXIT_STORAGE]
        self.assertNotIn("destination", meaning)
        self.assertIn("names the path", meaning)
        self.assertIn("volume", StorageRefusal.__doc__ or "")
        carried = OSError(errno.ENOSPC, "No space left on device")
        carried.filename = "/Users/jess/.bulkload-evidence/source-a.json.snapshot"
        named = failure_detail(carried, EXIT_STORAGE)
        self.assertIn("/Users/jess/.bulkload-evidence/source-a.json.snapshot", named)
        # An OSError that already spells the path is not made to say it twice.
        spelled = failure_detail(
            OSError(errno.ENOSPC, "No space left on device", "/srv/stage"),
            EXIT_STORAGE,
        )
        self.assertEqual(spelled.count("/srv/stage"), 1)

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
    def test_malformed_evidence_exits_six_with_its_traceback(self) -> None:
        """6 is the one class with no curated message, so it keeps the trace.

        The one-liner names a type, not a cause: `KeyError: 'catalog'` cannot
        tell the operator whether the protector, the planner, or the executor
        raised it, and nobody re-runs an hour-4 verb to set
        `BULKLOAD_TRACEBACK=1`. The typed refusals (3/4/5) keep the one-line
        form, where the message *is* the diagnosis.
        """
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
        self.assertIn("bulkload: FAIL[6]", result.stderr)
        self.assertIn("Traceback (most recent call last)", result.stderr)
        self.assertIn("KeyError", result.stderr)
        self.assertIn("cli.py", result.stderr)

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
        # A typed refusal keeps the one-line form; only 6 prints a trace.
        self.assertNotIn("Traceback", result.stderr)


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
            self.assertEqual(
                report["not_evaluated"],
                list(DRY_RUN_NOT_EVALUATED["agent-capture"]),
            )
            self.assertTrue(report["complete"])
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
            self.assertFalse(report["complete"])
            self.assertTrue(report["not_evaluated"])
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
            # The refusal path must not publish "mutates nothing, skipped
            # nothing". Measured before the fix: this report carried
            # "live_destination_mutations":0,"mutations":[],"not_evaluated":[]
            # alongside "exit_code":6.
            self.assertFalse(report["complete"])
            self.assertEqual(
                report["not_evaluated"],
                list(DRY_RUN_NOT_EVALUATED["agent-plan"]),
            )


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
        self.assertFalse(report["complete"])
        self.assertTrue(report["not_evaluated"])
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
        # apply refuses at validate_stage_receipt, before report["mutations"]
        # is ever populated: the report must say so rather than publishing an
        # empty mutation list as an inventory.
        self.assertFalse(report["complete"])
        self.assertEqual(report["mutations"], [])
        self.assertEqual(
            report["not_evaluated"], list(DRY_RUN_NOT_EVALUATED["agent-apply"])
        )
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

    def test_progress_defaults_on_even_when_stderr_is_redirected(self) -> None:
        """The measured silence is a redirected one, so a TTY default misses it.

        `logs/preseed-push.log` is 0 bytes for a 2h32m / 84 GiB push. That is
        the non-TTY case; defaulting on only for a terminal would leave the
        exact evidence this flag answers unchanged.
        """
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
        self.assertTrue(progress_enabled(arguments))
        arguments.progress = False
        self.assertFalse(progress_enabled(arguments))
        arguments.progress = True
        self.assertTrue(progress_enabled(arguments))

    def test_a_redirected_run_still_reports_its_phases(self) -> None:
        """Measured end to end: stderr is a pipe, and it is not silent."""
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
        self.assertIn("bulkload: phase:", result.stderr)
        self.assertIn("command=agent-plan", result.stderr)
        self.assertNotIn("bulkload:", result.stdout)

    def test_no_progress_restores_byte_exact_stderr(self) -> None:
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
                "--no-progress",
            )
        self.assertEqual(result.returncode, EXIT_REFUSED, result.stderr)
        self.assertEqual(len(result.stderr.strip().splitlines()), 1)
        self.assertNotIn("bulkload: phase:", result.stderr)

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
    def test_every_dry_run_verb_declares_what_it_did_not_check(self) -> None:
        self.assertEqual(set(DRY_RUN_NOT_EVALUATED), set(DRY_RUN_BUILDERS))
        for command, gates in DRY_RUN_NOT_EVALUATED.items():
            self.assertTrue(gates, command)
            for gate in gates:
                self.assertTrue(gate.strip(), command)

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


class ReachableCodeTests(unittest.TestCase):
    """3 and 5 driven out of the real engine to a real process status.

    Every other assertion about these two codes in this file calls
    `exit_code_for()` on a synthesized exception, which proves the table and
    nothing about the engine. A future `except BulkloadError: ... raise
    BulkloadError(...)` anywhere in scanner or executor would collapse 3 or 5
    into 4 with a fully green suite. These two tests fail when that happens.
    """

    @classmethod
    def setUpClass(cls) -> None:
        cls._temporary = tempfile.TemporaryDirectory()
        root = Path(cls._temporary.name)
        cls.fixture = CutoverFixture(root / "cutover")
        cls.evidence = root / "evidence"
        cls.evidence.mkdir()
        cls.destination = cls.evidence / "destination-a.json"
        cls.destination.write_text(
            json.dumps(cls.fixture.capture("destination")), encoding="utf-8"
        )
        cls.destination_b = cls.evidence / "destination-b.json"
        cls.destination_b.write_text(
            json.dumps(cls.fixture.capture("destination")), encoding="utf-8"
        )

    @classmethod
    def tearDownClass(cls) -> None:
        cls._temporary.cleanup()

    def test_an_unstable_ab_pair_exits_three_from_the_real_engine(self) -> None:
        """The live source moved between A and B, so the pair is not stable.

        This is the epoch class as `stable_capture_pair` (scanner.py:4751-4786)
        types it, reached through the streaming re-implementation `agent-plan`
        actually runs. It exited 4 before this fix: that re-implementation
        collapsed every sub-condition into one plain `BulkloadError`, so no
        input to any verb could produce a 3.
        """
        source_a = self.evidence / "source-a.json"
        source_a.write_text(
            json.dumps(self.fixture.capture("source")), encoding="utf-8"
        )
        (self.fixture.source_repo / "tracked.txt").write_text(
            "the host moved under the capture\n", encoding="utf-8"
        )
        source_b = self.evidence / "source-b.json"
        source_b.write_text(
            json.dumps(self.fixture.capture("source")), encoding="utf-8"
        )
        result = launch(
            "agent-plan",
            "--source-a",
            str(source_a),
            "--source-b",
            str(source_b),
            "--destination-a",
            str(self.destination),
            "--destination-b",
            str(self.destination_b),
            "--output",
            str(self.evidence / "plan.json"),
        )
        self.assertEqual(result.returncode, EXIT_EPOCH, result.stderr)
        self.assertIn("bulkload: FAIL[3]", result.stderr)
        self.assertIn("not byte-stable", result.stderr)
        self.assertFalse((self.evidence / "plan.json").exists())

    def test_a_reused_capture_id_stays_the_general_refusal(self) -> None:
        """The other half of the split: a wrong document is not an epoch."""
        source = self.evidence / "same.json"
        source.write_text(json.dumps(self.fixture.capture("source")), encoding="utf-8")
        result = launch(
            "agent-plan",
            "--source-a",
            str(source),
            "--source-b",
            str(source),
            "--destination-a",
            str(self.destination),
            "--destination-b",
            str(self.destination_b),
            "--output",
            str(self.evidence / "plan-2.json"),
        )
        self.assertEqual(result.returncode, EXIT_REFUSED, result.stderr)
        self.assertIn("reuse one capture ID", result.stderr)

    def test_the_capacity_gate_exits_five_and_names_the_volume(self) -> None:
        """A reserve larger than the volume drives `require_capacity` for real."""
        plan = self.fixture.plan()
        plan_path = self.evidence / "capacity-plan.json"
        plan_path.write_text(json.dumps(plan), encoding="utf-8")
        stage_root = Path(self._temporary.name) / "capacity-stage"
        result = launch(
            "agent-stage",
            "--phase",
            "preseed",
            "--plan",
            str(plan_path),
            "--accept-plan-sha256",
            plan["plan_sha256"],
            "--stage-root",
            str(stage_root),
            "--capacity-reserve-bytes",
            str(1 << 62),
            "--output",
            str(self.evidence / "capacity-receipt.json"),
        )
        self.assertEqual(result.returncode, EXIT_STORAGE, result.stderr)
        self.assertIn("bulkload: FAIL[5]", result.stderr)
        self.assertIn("capacity gate failed", result.stderr)
        # The volume is named, so an operator knows which host to look at.
        self.assertIn(os.path.realpath(stage_root), result.stderr)
        self.assertFalse((self.evidence / "capacity-receipt.json").exists())


class BlockerNotEpochTests(unittest.TestCase):
    """Per-entry churn is a capture blocker, not exit 3 -- as documented.

    `_file_record` raises `EpochRefusal` for an entry that moved and
    `BulkloadError` for one it cannot type, and both directory-walk handlers
    (`scanner.py:547`, `:577`) catch `BulkloadError` and record a blocker
    instead. That downgrade is deliberate: aborting a 1.8M-entry live capture
    on one churned file would make a busy host uncapturable. This test pins
    the consequence the README now states -- exit `0`, `complete: false` -- so
    the doc claim and the code cannot drift apart again.
    """

    def test_an_untypable_entry_is_a_blocker_and_the_process_exits_zero(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            fixture = CutoverFixture(root / "cutover")
            os.mkfifo(fixture.source_git / "spool.fifo")
            evidence = root / "evidence"
            evidence.mkdir()
            output = evidence / "source-a.json"
            result = launch(
                "agent-capture",
                "--role",
                "source",
                "--home",
                str(fixture.source_home),
                "--git-root",
                str(fixture.source_git),
                "--rsync-path",
                str(gnu_rsync_path()),
                "--path-map",
                f"{fixture.source_home}={fixture.destination_home}",
                "--path-map",
                f"{fixture.source_git}={fixture.destination_git}",
                "--acknowledge-writers-quiesced",
                "--output",
                str(output),
            )
            self.assertEqual(result.returncode, EXIT_OK, result.stderr)
            capture = json.loads(output.read_text(encoding="utf-8"))
            self.assertFalse(capture["complete"])
            self.assertIn("unsupported-filesystem-entry", json.dumps(capture))
            self.assertIn("spool.fifo", json.dumps(capture))
            self.assertNotIn("bulkload: FAIL", result.stderr)


if __name__ == "__main__":
    unittest.main()
