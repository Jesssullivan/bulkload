"""Wave-0 performance-slice regressions.

Every slice in this wave is required to be output-identical: no digest value
and no artifact field name may change. These tests pin the *behaviour* that
each redundant read was carrying, so the redundancy can be removed without
removing a fence.
"""

from __future__ import annotations

import contextlib
import copy
import hashlib
import io
import json
import os
from pathlib import Path
import socket
import stat
import tempfile
import time
import unittest
from unittest import mock

from bulkload_lib import cli
from bulkload_lib.cli import _agent_stage, build_parser
from bulkload_lib import executor
from bulkload_lib.executor import push_agent_transport, stage_agent_plan
from bulkload_lib.model import BulkloadError, canonical_bytes, sha256_bytes
from bulkload_lib import scanner
from bulkload_lib.scanner import (
    _jsonl_records,
    capture_agent_state,
    validate_agent_capture,
    validate_live_snapshot_generation,
)

from test_bulkload import CutoverFixture, compile_agent_plan


def reference_jsonl_records(path: Path, *, replacements=()) -> dict:
    """The pre-slice implementation, kept verbatim as the value oracle."""
    hashes: list[str] = []
    transformed_hashes: list[str] = []
    hasher = hashlib.sha256()
    transformed_hasher = hashlib.sha256()
    with path.open("rb") as stream:
        for line in stream:
            json.loads(line)
            transformed = line
            for source, destination in replacements:
                transformed = transformed.replace(source, destination)
            json.loads(transformed)
            hashes.append(sha256_bytes(line))
            transformed_hashes.append(sha256_bytes(transformed))
            hasher.update(line)
            transformed_hasher.update(transformed)
    return {
        "records": hashes,
        "records_sha256": sha256_bytes(canonical_bytes(hashes)),
        "sha256": hasher.hexdigest(),
        "translated_records": transformed_hashes,
        "translated_sha256": transformed_hasher.hexdigest(),
    }


def live_capture(
    fixture: CutoverFixture,
    name: str,
    *,
    role: str = "source",
    base: dict | None = None,
) -> dict:
    home = fixture.source_home if role == "source" else fixture.destination_home
    git_root = fixture.source_git if role == "source" else fixture.destination_git
    seats = fixture.source_seats if role == "source" else fixture.destination_seats
    return capture_agent_state(
        role=role,
        home=home,
        git_root=git_root,
        codex_root=None,
        claude_root=None,
        pi_root=None,
        seats=seats,
        path_map=fixture.path_map,
        writers_quiesced=False,
        snapshot_root=fixture.root / "evidence" / f"{name}.snapshot",
        snapshot_base_seal=Path(base["catalog"]["snapshot"]["seal_path"])
        if base is not None
        else None,
        managed_exclusions=fixture.managed_exclusions,
        rsync_path=fixture.rsync_path,
        max_files=50_000,
        max_bytes=4 * 1024**3,
        max_sqlite_rows=100_000,
        snapshot_reserve_bytes=0,
    )


class SingleEpochFenceTests(unittest.TestCase):
    """W0-1: the fence keeps both passes; only a dominated site drops one.

    The walk inside the fence is sequential, so one pass is not atomic. These
    tests pin the straggler window the second pass carries, and pin that the
    only site allowed to give it up is recovered by a later full fence.
    """

    def test_fence_fails_on_a_real_post_seal_divergence(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=True)
            capture = live_capture(fixture, "source-a")
            validate_agent_capture(capture, expected_role="source")
            snapshot = capture["catalog"]["snapshot"]

            # The sealed snapshot still matches the untouched live tree.
            validate_live_snapshot_generation(snapshot)

            # Mutating a live payload byte after the seal must still abort.
            tracked = fixture.source_repo / "untracked.txt"
            tracked.write_bytes(b"mutated after the immutable seal\n")
            with self.assertRaisesRegex(
                BulkloadError, "live source changed after immutable snapshot B"
            ):
                validate_live_snapshot_generation(snapshot)

    def test_same_size_mutation_is_still_caught(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=True)
            history = fixture.source_home / ".codex" / "history.jsonl"
            original = history.read_bytes()
            capture = live_capture(fixture, "source-a")
            snapshot = capture["catalog"]["snapshot"]
            replacement = bytearray(original)
            replacement[-2] = original[-2] ^ 0x01
            history.write_bytes(bytes(replacement))
            self.assertEqual(len(bytes(replacement)), len(original))
            with self.assertRaisesRegex(
                BulkloadError, "live source changed after immutable snapshot B"
            ):
                validate_live_snapshot_generation(snapshot)

    @staticmethod
    def straggler_behind_the_cursor(victim: Path):
        """Land a write immediately after the first root is digested.

        That is the exact window a single pass cannot see: the walk has
        already moved past the victim's root, so the pass in flight reports
        the pre-write generation and only the *next* pass diverges.
        """
        real = scanner._tree_generation
        state = {"digested": 0}

        def hooked(root, **keywords):
            result = real(root, **keywords)
            state["digested"] += 1
            if state["digested"] == 1:
                victim.write_bytes(b"straggler written behind the walk cursor\n")
            return result

        return mock.patch.object(scanner, "_tree_generation", side_effect=hooked)

    def test_default_fence_catches_a_straggler_written_behind_the_cursor(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=True)
            capture = live_capture(fixture, "source-a")
            snapshot = capture["catalog"]["snapshot"]
            victim = Path(snapshot["roots"][0]["live"]) / "straggler.txt"
            with self.straggler_behind_the_cursor(victim):
                with self.assertRaisesRegex(
                    BulkloadError, "live source changed after immutable snapshot B"
                ):
                    validate_live_snapshot_generation(snapshot)

    def test_a_dominated_single_pass_is_recovered_by_the_next_full_fence(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=True)
            capture = live_capture(fixture, "source-a")
            snapshot = capture["catalog"]["snapshot"]
            victim = Path(snapshot["roots"][0]["live"]) / "straggler.txt"
            with self.straggler_behind_the_cursor(victim):
                # The dominated pre-push fence genuinely misses this write.
                # That is the cost of `passes=1`, stated rather than hidden.
                validate_live_snapshot_generation(snapshot, passes=1)
                # The full-strength fence that follows it still aborts, which
                # is the whole reason the pre-push site may give up a pass.
                with self.assertRaisesRegex(
                    BulkloadError, "live source changed after immutable snapshot B"
                ):
                    validate_live_snapshot_generation(snapshot)

    def test_pass_count_is_two_by_default_and_one_when_dominated(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=True)
            capture = live_capture(fixture, "source-a")
            snapshot = capture["catalog"]["snapshot"]
            real = scanner._tree_generation
            calls: list[str] = []

            def counted(root, **kwargs):
                calls.append(str(root))
                return real(root, **kwargs)

            with mock.patch.object(scanner, "_tree_generation", side_effect=counted):
                validate_live_snapshot_generation(snapshot)
            self.assertEqual(len(calls), 2 * len(snapshot["roots"]))
            calls.clear()
            with mock.patch.object(scanner, "_tree_generation", side_effect=counted):
                validate_live_snapshot_generation(snapshot, passes=1)
            self.assertEqual(len(calls), len(snapshot["roots"]))

    def test_the_fence_cannot_be_reduced_to_no_passes(self) -> None:
        # The guard runs before the snapshot is read, so a would-be caller
        # cannot disable the fence outright by asking for zero passes.
        for rejected in (0, -1, True, 1.5, "2", None):
            with self.assertRaisesRegex(
                BulkloadError, "live generation fence requires at least one pass"
            ):
                validate_live_snapshot_generation({}, passes=rejected)


BREAK_GLASS = "BULKLOAD_BREAK_GLASS_LIVE_FENCE_NOTE"
BREAK_GLASS_EXPECT = "BULKLOAD_BREAK_GLASS_LIVE_FENCE_EXPECT"


@contextlib.contextmanager
def no_root_may_be_read():
    """Fail loudly if the break-glass path censuses anything at all.

    A pass is a full-content read of every root, which is what #24's interim
    is buying back. Raising here means "skipped" cannot quietly decay into
    "computed, then ignored".

    Patching `_tree_generation` alone was not enough to justify the claim:
    `epoch()` reaches `_declared_root` for the declarations and
    `sqlite_catalog` for a root's sqlite before it ever calls
    `_tree_generation`, so a bypass that still walked those would have gone
    unnoticed. All four entry points are closed here.
    """

    def forbidden(*arguments, **keywords):
        raise AssertionError(f"break-glass read a root: {arguments!r}")

    with contextlib.ExitStack() as stack:
        for name in (
            "_tree_generation",
            "sqlite_catalog",
            "_git_live_authority_rows",
            "_declared_root",
        ):
            stack.enter_context(mock.patch.object(scanner, name, side_effect=forbidden))
        yield


def sealed_expectation(case: unittest.TestCase, snapshot: dict) -> str:
    """The digest this snapshot's glass must be bound to.

    Taken from the refusal itself, which is the only place an operator can
    learn it, rather than re-implemented here -- a test that recomputed the
    fence's own derivation would pass even if the two drifted apart.
    """
    with mock.patch.dict(os.environ, {BREAK_GLASS: "/dev/null"}):
        os.environ.pop(BREAK_GLASS_EXPECT, None)
        with case.assertRaises(BulkloadError) as caught:
            validate_live_snapshot_generation(snapshot, passes=1)
    message = str(caught.exception)
    case.assertIn(f"{BREAK_GLASS_EXPECT}=", message)
    digest = message.rsplit(f"{BREAK_GLASS_EXPECT}=", 1)[1].split()[0]
    case.assertRegex(digest, r"^[0-9a-f]{64}$")
    return digest


class LiveFenceBreakGlassTests(unittest.TestCase):
    """The disclosed break-glass for Jesssullivan/bulkload#24.

    The variables remove the fence, so the tests that matter are the halves of
    that bargain: with them set no root is read and one deviation record is
    written; the record carries an actual observation of the live roots and
    not only the sealed expectation, which observes nothing; the glass is
    bound to one snapshot so a stale export cannot open a later one; the
    cutover release refuses it outright; and without them nothing about the
    fence changes.
    """

    def open_glass(self, snapshot: dict, note: Path) -> dict:
        return {
            BREAK_GLASS: os.fspath(note),
            BREAK_GLASS_EXPECT: sealed_expectation(self, snapshot),
        }

    def test_break_glass_skips_the_census_and_records_the_deviation(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=True)
            capture = live_capture(fixture, "source-a")
            snapshot = capture["catalog"]["snapshot"]

            # Diverge the live tree so an un-broken fence would certainly
            # abort: the skip below is then unambiguous.
            victim = fixture.source_repo / "untracked.txt"
            victim.write_bytes(b"mutated after the immutable seal\n")

            note = Path(temporary) / "deviations" / "live-fence.jsonl"
            note.parent.mkdir()
            self.assertFalse(note.exists())
            glass = self.open_glass(snapshot, note)

            stderr = io.StringIO()
            with mock.patch.dict(os.environ, glass):
                with no_root_may_be_read():
                    with contextlib.redirect_stderr(stderr):
                        # False is the whole contract for a caller that seals
                        # over this call: the fence did not run.
                        self.assertIs(
                            validate_live_snapshot_generation(snapshot), False
                        )

            lines = note.read_text(encoding="utf-8").splitlines()
            self.assertEqual(len(lines), 1)
            record = json.loads(lines[0])
            self.assertEqual(
                sorted(record),
                [
                    "break_glass",
                    "expected",
                    "observation",
                    "passes_requested",
                    "reason",
                    "snapshot_id",
                    "ts",
                ],
            )
            self.assertEqual(record["break_glass"], "live-fence-skipped")
            self.assertEqual(record["passes_requested"], 2)
            self.assertEqual(record["snapshot_id"], snapshot["snapshot_id"])
            self.assertEqual(record["expected"], glass[BREAK_GLASS_EXPECT])
            self.assertIn("bulkload#24", record["reason"])
            # The deviation is disclosed on the console too, not only on disk.
            self.assertIn("BREAK-GLASS live fence skipped", stderr.getvalue())
            self.assertIn(os.fspath(note), stderr.getvalue())

            # Every bypassed fence is its own ledger line, including the
            # dominated `passes=1` site, which records the weaker request.
            with mock.patch.dict(os.environ, glass):
                with no_root_may_be_read():
                    with contextlib.redirect_stderr(io.StringIO()):
                        validate_live_snapshot_generation(snapshot, passes=1)
            lines = note.read_text(encoding="utf-8").splitlines()
            self.assertEqual(len(lines), 2)
            self.assertEqual(json.loads(lines[1])["passes_requested"], 1)
            self.assertEqual(json.loads(lines[1])["expected"], record["expected"])

    def test_the_record_observes_the_roots_it_did_not_fence(self) -> None:
        """`expected` is a function of the snapshot; it observes nothing.

        The record has to say something about the live tree or it cannot
        support or refute the "the roots are frozen" precondition afterwards.
        `pruned_leaves` is the #24 signature: a root with none of them cannot
        be affected by the defect the glass exists for.
        """
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=True)
            capture = live_capture(fixture, "source-a")
            snapshot = capture["catalog"]["snapshot"]
            note = Path(temporary) / "observed.jsonl"

            with mock.patch.dict(os.environ, self.open_glass(snapshot, note)):
                with no_root_may_be_read():
                    with contextlib.redirect_stderr(io.StringIO()):
                        validate_live_snapshot_generation(snapshot, passes=1)

            observation = json.loads(note.read_text(encoding="utf-8"))["observation"]
            self.assertEqual(
                [row["live"] for row in observation],
                [root["live"] for root in snapshot["roots"]],
            )
            for row in observation:
                self.assertEqual(
                    sorted(row),
                    [
                        "dev",
                        "entries",
                        "exists",
                        "ino",
                        "label",
                        "live",
                        "mtime_ceiling_ns",
                        "pruned_leaves",
                        "unreadable",
                    ],
                )
                self.assertTrue(row["exists"])
                self.assertEqual(row["unreadable"], 0)
                self.assertGreater(row["entries"], 0)
                self.assertIsInstance(row["mtime_ceiling_ns"], int)
                self.assertEqual(row["ino"], Path(row["live"]).lstat().st_ino)

            # It is an observation, not a re-derivation: a write after the
            # record moves the ceiling the next record reports.
            before = max(row["mtime_ceiling_ns"] for row in observation)
            time.sleep(0.01)
            fixture.source_repo.joinpath("untracked.txt").write_bytes(b"later\n")
            with mock.patch.dict(os.environ, self.open_glass(snapshot, note)):
                with no_root_may_be_read():
                    with contextlib.redirect_stderr(io.StringIO()):
                        validate_live_snapshot_generation(snapshot, passes=1)
            second = json.loads(note.read_text(encoding="utf-8").splitlines()[1])
            self.assertGreater(
                max(row["mtime_ceiling_ns"] for row in second["observation"]), before
            )

    def test_a_stale_export_cannot_open_the_glass_for_another_snapshot(self) -> None:
        """`_transport_environment` forwards the environment to children and

        an export in a shell profile, a launchd unit or a CI job outlives the
        run it was meant for. Binding the glass to one sealed expectation
        turns a stale export into a hard refusal instead of a silent bypass.
        """
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=True)
            capture = live_capture(fixture, "source-a")
            snapshot = capture["catalog"]["snapshot"]
            note = Path(temporary) / "stale.jsonl"

            other = copy.deepcopy(snapshot)
            other["roots"][0]["generation_sha256"] = "0" * 64
            stale = sealed_expectation(self, other)
            self.assertNotEqual(stale, sealed_expectation(self, snapshot))

            for value in (None, "", "not-a-digest", "f" * 64, stale):
                with self.subTest(expect=value):
                    environment = {BREAK_GLASS: os.fspath(note)}
                    if value is not None:
                        environment[BREAK_GLASS_EXPECT] = value
                    with mock.patch.dict(os.environ, environment):
                        if value is None:
                            os.environ.pop(BREAK_GLASS_EXPECT, None)
                        with no_root_may_be_read():
                            with self.assertRaisesRegex(
                                BulkloadError,
                                "break-glass is not bound to this snapshot",
                            ):
                                validate_live_snapshot_generation(snapshot, passes=1)
            self.assertFalse(note.exists())

    def test_the_cutover_release_refuses_the_glass(self) -> None:
        """`allow_break_glass=False` is a refusal, not a quieter bypass.

        The cutover release seals `independent_fresh_observation: True` over
        this call and `validate_verify_receipt` requires it, so a skip there
        would be laundered into a receipt byte-identical to an honest one.
        """
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=True)
            capture = live_capture(fixture, "source-a")
            snapshot = capture["catalog"]["snapshot"]
            note = Path(temporary) / "refused.jsonl"

            with mock.patch.dict(os.environ, self.open_glass(snapshot, note)):
                with no_root_may_be_read():
                    with self.assertRaisesRegex(
                        BulkloadError, "break-glass is refused at this call site"
                    ):
                        validate_live_snapshot_generation(
                            snapshot, allow_break_glass=False
                        )
            # Refused, and it left no record claiming otherwise.
            self.assertFalse(note.exists())

    def test_the_witness_reports_unobserved_rather_than_no_divergence(self) -> None:
        """`_witness_epoch` is the seventh call site and it fails open.

        With the glass open the fence never looks, so a witness that printed
        `divergence=none` would report the absence of a divergence it never
        tested for -- and the Wave 3 cost/trade measurement the witness exists
        to produce would be silently corrupted.
        """
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=True)
            capture = live_capture(fixture, "source-a")
            snapshot = capture["catalog"]["snapshot"]
            fixture.source_repo.joinpath("untracked.txt").write_bytes(
                b"mutated after the immutable seal\n"
            )
            note = Path(temporary) / "witness.jsonl"

            # Without the glass the witness sees the real divergence.
            honest = io.StringIO()
            with mock.patch.dict(os.environ, {"BULKLOAD_WITNESS_EPOCH": "1"}):
                os.environ.pop(BREAK_GLASS, None)
                with contextlib.redirect_stderr(honest):
                    scanner._witness_epoch(snapshot)
            self.assertIn(
                "bulkload-witness divergence=BulkloadError", honest.getvalue()
            )

            environment = {"BULKLOAD_WITNESS_EPOCH": "1"}
            environment.update(self.open_glass(snapshot, note))
            skipped = io.StringIO()
            with mock.patch.dict(os.environ, environment):
                with no_root_may_be_read():
                    with contextlib.redirect_stderr(skipped):
                        scanner._witness_epoch(snapshot)
            self.assertIn("bulkload-witness divergence=unobserved", skipped.getvalue())
            self.assertNotIn("divergence=none", skipped.getvalue())

    def test_a_record_that_cannot_be_written_fails_closed(self) -> None:
        """The one non-BulkloadError exit this function had, mid-transport.

        A break-glass whose deviation cannot be recorded is an undisclosed
        bypass, so it is a refusal rather than a bare `OSError` escaping into
        a transport path that converts nothing.
        """
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=True)
            capture = live_capture(fixture, "source-a")
            snapshot = capture["catalog"]["snapshot"]
            unwritable = Path(temporary) / "absent-directory" / "note.jsonl"
            a_directory = Path(temporary) / "note-is-a-directory"
            a_directory.mkdir()

            for note in (unwritable, a_directory):
                with self.subTest(note=note.name):
                    with mock.patch.dict(os.environ, self.open_glass(snapshot, note)):
                        with no_root_may_be_read():
                            with self.assertRaisesRegex(
                                BulkloadError,
                                "break-glass cannot write its deviation record",
                            ):
                                validate_live_snapshot_generation(snapshot, passes=1)

    def test_without_the_variable_the_fence_is_unchanged(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=True)
            capture = live_capture(fixture, "source-a")
            snapshot = capture["catalog"]["snapshot"]
            note = Path(temporary) / "unwritten.jsonl"

            with mock.patch.dict(os.environ):
                os.environ.pop(BREAK_GLASS, None)
                # True is the other half of the contract: the fence ran.
                self.assertIs(validate_live_snapshot_generation(snapshot), True)
                fixture.source_repo.joinpath("untracked.txt").write_bytes(
                    b"mutated after the immutable seal\n"
                )
                with self.assertRaisesRegex(
                    BulkloadError, "live source changed after immutable snapshot B"
                ):
                    validate_live_snapshot_generation(snapshot)
            self.assertFalse(note.exists())

    def test_break_glass_cannot_defeat_the_pass_count_guard(self) -> None:
        # The break-glass is read after the guard, so it widens no other
        # hole: a caller still cannot ask the fence for zero passes.
        with tempfile.TemporaryDirectory() as temporary:
            note = Path(temporary) / "guard.jsonl"
            with mock.patch.dict(
                os.environ,
                {BREAK_GLASS: os.fspath(note), BREAK_GLASS_EXPECT: "0" * 64},
            ):
                for rejected in (0, -1, True, 1.5, "2", None):
                    with self.assertRaisesRegex(
                        BulkloadError,
                        "live generation fence requires at least one pass",
                    ):
                        validate_live_snapshot_generation({}, passes=rejected)
            self.assertFalse(note.exists())


class PrunedLeafGenerationTests(unittest.TestCase):
    """Jesssullivan/bulkload#24: the seal and the fence census two trees.

    `capture_agent_state` seals `roots[].generation_sha256` over the immutable
    snapshot copy, which omits the pruned managed leaves, while
    `validate_live_snapshot_generation` re-derives it over `roots[].live`.
    `_tree_census(portable=True)` puts a directory's `st_size` into its
    authority, so a directory holding a pruned leaf is a different census entry
    in the two trees and the final fence can never pass for that root. Because
    the fence only runs for the final phase, no preseed lap ever exercised it.

    This is the smallest tree with that shape. It is `expectedFailure` until
    #24 lands one of its candidate fixes -- census the live tree under the same
    pruning at seal time, drop directory `st_size` from portable authority in
    favour of hashing the pruned leaf names, or census the copy in the fence
    and keep a separate named live-drift check. When any of those lands this
    test passes, unittest reports an unexpected success, and the run fails
    until the marker below is removed.

    That retirement guarantee used to be APFS-only. On APFS a directory's
    `st_size` is its entry count; on ext4 and xfs it is block-granular (4096
    until the directory crosses a block), so a small tree showed no
    difference and the test called `skipTest`. A skip inside
    `@unittest.expectedFailure` is reported as a *skip* and leaves
    `wasSuccessful()` True (verified on CPython 3.12.14), so on a Linux runner
    the marker could outlive the bug. `directory_size_tracks_entry_count`
    replaces the skip: it asserts the mechanism -- the authority tuple the
    census folds for the one directory the two trees share -- against a
    directory `st_size` that tracks entry count on every kernel. What is under
    test is the census, not the filesystem.
    """

    provider = "claude"
    exclusions = ("state/excluded.json",)

    def build(self, root: Path, *, pruned: bool) -> Path:
        """Write the tree, with the managed leaf present only when live.

        The leaf is never created in the copy rather than created and removed,
        because a directory's `st_size` does not always shrink back.
        """
        root.joinpath("state").mkdir(parents=True)
        root.joinpath("keep.txt").write_bytes(b"kept payload\n")
        root.joinpath("state", "keep.txt").write_bytes(b"kept nested payload\n")
        if pruned:
            root.joinpath("state", "excluded.json").write_bytes(b'{"managed": true}\n')
        return root

    @staticmethod
    @contextlib.contextmanager
    def directory_size_tracks_entry_count():
        """Give every kernel the APFS property #24 needs to be visible."""
        real = scanner._stable_stat

        def stable_stat(path):
            observed = real(path)
            if observed[2] != stat.S_IFDIR:
                return observed
            return observed[:5] + (len(os.listdir(path)),) + observed[6:]

        with mock.patch.object(scanner, "_stable_stat", stable_stat):
            yield

    @staticmethod
    def authority(path: Path) -> list[int]:
        """Exactly what `_tree_census(portable=True)` folds for `path`.

        `scanner.py:2095-2098`: `[IFMT, IMODE]`, plus `st_size` for anything
        that is not a live sqlite primary -- directories included.
        """
        observed = scanner._stable_stat(path)
        return [observed[2], observed[3], observed[5]]

    @unittest.expectedFailure
    def test_sealed_copy_generation_equals_the_live_generation(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            live = self.build(Path(temporary) / "live", pruned=True)
            sealed_tree = self.build(Path(temporary) / "roots" / "claude", pruned=False)

            with self.directory_size_tracks_entry_count():
                # The mechanism, asserted directly: the pruned leaf is absent
                # from both censuses by name, but the directory that held it
                # hands the two censuses different authority.
                self.assertEqual(
                    self.authority(live / "state"),
                    self.authority(sealed_tree / "state"),
                )
                sealed = scanner._tree_generation(
                    sealed_tree, provider=self.provider, exclusions=self.exclusions
                )
                observed = scanner._tree_generation(
                    live, provider=self.provider, exclusions=self.exclusions
                )
                self.assertEqual(sealed, observed)

    def test_the_reproducer_is_not_vacuous(self) -> None:
        """Without the pruned leaf the two trees agree, on every kernel.

        This is the control for the `expectedFailure` above: it proves the
        divergence there comes from the pruned leaf and not from the two trees
        merely being two directories.
        """
        with tempfile.TemporaryDirectory() as temporary:
            live = self.build(Path(temporary) / "live", pruned=False)
            sealed_tree = self.build(Path(temporary) / "roots" / "claude", pruned=False)
            with self.directory_size_tracks_entry_count():
                self.assertEqual(
                    self.authority(live / "state"),
                    self.authority(sealed_tree / "state"),
                )
                self.assertEqual(
                    scanner._tree_generation(
                        sealed_tree, provider=self.provider, exclusions=self.exclusions
                    ),
                    scanner._tree_generation(
                        live, provider=self.provider, exclusions=self.exclusions
                    ),
                )


class IdenticalLineShortCircuitTests(unittest.TestCase):
    """W0-2: a no-op transform must reuse the original parse and hash."""

    @staticmethod
    def write_corpus(path: Path) -> None:
        path.write_bytes(
            b"".join(
                json.dumps(
                    {"cwd": f"/Users/one/git/repo/{index}", "n": index},
                    sort_keys=True,
                ).encode()
                + b"\n"
                for index in range(64)
            )
        )

    def test_no_replacements_produce_the_reference_record(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "history.jsonl"
            self.write_corpus(path)
            self.assertEqual(_jsonl_records(path), reference_jsonl_records(path))

    def test_non_matching_replacement_produces_the_reference_record(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "history.jsonl"
            self.write_corpus(path)
            replacements = ((b"/Users/absent", b"/Users/other"),)
            self.assertEqual(
                _jsonl_records(path, replacements=replacements),
                reference_jsonl_records(path, replacements=replacements),
            )

    def test_full_and_partial_rewrites_produce_the_reference_record(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "history.jsonl"
            self.write_corpus(path)
            for replacements in (
                ((b"/Users/one", b"/Users/two"),),
                # Only one line matches, so the lazy fork happens mid-file and
                # has to carry every earlier identical line with it.
                ((b"repo/7", b"repo/x"),),
                ((b"/Users/one", b"/Users/two"), (b"repo/1", b"repo/y")),
            ):
                with self.subTest(replacements=replacements):
                    self.assertEqual(
                        _jsonl_records(path, replacements=replacements),
                        reference_jsonl_records(path, replacements=replacements),
                    )

    def test_partial_rewrite_first_line_only(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "history.jsonl"
            path.write_bytes(b'{"a":"one"}\n{"a":"two"}\n{"a":"three"}\n')
            replacements = ((b"one", b"ONE"),)
            self.assertEqual(
                _jsonl_records(path, replacements=replacements),
                reference_jsonl_records(path, replacements=replacements),
            )

    def test_identical_lines_are_parsed_once(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "history.jsonl"
            self.write_corpus(path)
            real = scanner.json.loads
            calls: list[int] = []

            def counted(payload, *args, **kwargs):
                calls.append(len(payload))
                return real(payload, *args, **kwargs)

            with mock.patch.object(scanner.json, "loads", side_effect=counted):
                _jsonl_records(path)
            self.assertEqual(len(calls), 64)

    def test_rewrite_to_invalid_jsonl_still_raises(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "history.jsonl"
            path.write_bytes(b'{"a":"one"}\n')
            with self.assertRaisesRegex(
                BulkloadError, "path rewriting produced invalid JSONL"
            ):
                _jsonl_records(path, replacements=((b'"a"', b"a"),))

    def test_incomplete_final_record_still_raises(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "history.jsonl"
            path.write_bytes(b'{"a":"one"}\n{"a":"tw')
            with self.assertRaises(scanner._MalformedAppendState):
                _jsonl_records(path)


class DeferredAppendDigestTests(unittest.TestCase):
    """W0-3: the discarded whole-file hash on append-jsonl files."""

    @staticmethod
    def provider_records(fixture: CutoverFixture, role: str) -> dict:
        capture = fixture.capture(role)
        providers = {
            provider["name"]: provider for provider in capture["catalog"]["providers"]
        }
        return {
            item["relative_path"]: item
            for item in providers["codex"]["items"]
            if item["kind"] == "regular"
        }

    def test_append_jsonl_record_keeps_its_exact_digest_and_size(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            payload = fixture.source_home / ".codex" / "history.jsonl"
            records = self.provider_records(fixture, "source")
            record = records["history.jsonl"]
            self.assertEqual(record["classification"], "append-jsonl")
            self.assertEqual(
                record["sha256"],
                hashlib.sha256(payload.read_bytes()).hexdigest(),
            )
            self.assertEqual(record["size"], payload.stat().st_size)
            self.assertEqual(record["sha256"], record["translated_sha256"])

    def test_append_jsonl_payload_is_read_once(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            payload = fixture.source_home / ".codex" / "history.jsonl"
            real = scanner.sha256_file
            hashed: list[str] = []

            def counted(path):
                hashed.append(str(path))
                return real(path)

            with mock.patch.object(scanner, "sha256_file", side_effect=counted):
                self.provider_records(fixture, "source")
            self.assertNotIn(str(payload), hashed)

    def test_malformed_append_state_still_carries_a_full_digest(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            payload = fixture.source_home / ".codex" / "history.jsonl"
            # No trailing newline: _jsonl_records rejects it and the record is
            # rebuilt as portable-private, which must still hash the bytes.
            payload.write_bytes(b'{"session_id":"one","text":"source"}')
            record = self.provider_records(fixture, "source")["history.jsonl"]
            self.assertEqual(record["classification"], "portable-private")
            self.assertEqual(
                record["sha256"],
                hashlib.sha256(payload.read_bytes()).hexdigest(),
            )
            self.assertNotIn("records", record)

    def test_a_deferred_digest_can_never_reach_a_record(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            with mock.patch.object(
                scanner, "_jsonl_records", return_value={"records": []}
            ):
                capture = fixture.capture("source")
            # The guard fails the provider closed rather than emitting a
            # record whose digest was deferred and never restored.
            self.assertFalse(capture["complete"])
            self.assertEqual(
                [
                    blocker["code"]
                    for blocker in capture["catalog"]["blockers"]
                    if "lost its digest" in blocker.get("detail", "")
                ],
                ["provider-capture-failed"],
            )
            self.assertNotIn(
                "codex",
                {provider["name"] for provider in capture["catalog"]["providers"]},
            )

    def test_non_append_records_still_hash_in_file_record(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            auth = fixture.source_home / ".codex" / "auth.json"
            record = self.provider_records(fixture, "source")["auth.json"]
            self.assertEqual(
                record["sha256"], hashlib.sha256(auth.read_bytes()).hexdigest()
            )


FAKE_SSH = """#!/usr/bin/env python3
import os
import shlex
import sys

arguments = sys.argv[1:]
while arguments and arguments[0].startswith("-o"):
    arguments.pop(0)
if arguments and arguments[0] == "--":
    arguments.pop(0)
if not arguments:
    raise SystemExit(90)
arguments.pop(0)
if len(arguments) == 1:
    arguments = shlex.split(arguments[0])
os.execv(arguments[0], arguments)
"""


class TransportChecksumTests(unittest.TestCase):
    """W0-4: rsync --checksum is opt-in, not the transport default."""

    def push_argv(self, *, transport_checksum: bool) -> list[str]:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            plan = fixture.plan()
            fake_ssh = fixture.root / "fake-ssh"
            fake_ssh.write_text(FAKE_SSH, encoding="utf-8")
            fake_ssh.chmod(0o700)
            prepare = stage_agent_plan(
                plan,
                accepted_plan_sha256=plan["plan_sha256"],
                phase="preseed",
                stage_root=fixture.stage,
                allow_accounted_copy=True,
                reserve_bytes=0,
                transport_mode="prepare",
            )
            recorded: list[list[str]] = []
            real = executor.subprocess.run

            def recording(argv, **keywords):
                recorded.append([str(item) for item in argv])
                return real(argv, **keywords)

            with mock.patch.object(executor.subprocess, "run", side_effect=recording):
                push_agent_transport(
                    prepare,
                    fixture.stage / ".transport-allowlist-preseed.nul",
                    accepted_plan_sha256=plan["plan_sha256"],
                    phase="preseed",
                    stage_root=fixture.stage,
                    destination_ssh_host=socket.gethostname(),
                    transport_checksum=transport_checksum,
                    _ssh_binary=str(fake_ssh),
                )
            payload = [argv for argv in recorded if "--files-from=-" in argv]
            self.assertEqual(len(payload), 1)
            return payload[0]

    def test_payload_push_omits_checksum_by_default(self) -> None:
        argv = self.push_argv(transport_checksum=False)
        self.assertNotIn("--checksum", argv)
        self.assertIn("--delay-updates", argv)
        self.assertIn("-a", argv)

    def test_transport_checksum_flag_restores_the_whole_file_pass(self) -> None:
        argv = self.push_argv(transport_checksum=True)
        self.assertIn("--checksum", argv)
        self.assertLess(argv.index("--checksum"), argv.index("--delay-updates"))

    def test_parser_default_is_off_and_the_flag_is_push_only(self) -> None:
        base = [
            "agent-stage",
            "--phase",
            "preseed",
            "--accept-plan-sha256",
            "0" * 64,
            "--stage-root",
            "/tmp/stage",
            "--output",
            "-",
        ]
        parser = build_parser()
        self.assertFalse(parser.parse_args(base).transport_checksum)
        self.assertTrue(
            parser.parse_args([*base, "--transport-checksum"]).transport_checksum
        )
        arguments = parser.parse_args(
            [*base, "--transport-checksum", "--plan", "/tmp/plan.json"]
        )
        with self.assertRaisesRegex(
            BulkloadError, "transport checksum applies only to the push transport"
        ):
            _agent_stage(arguments)


class SnapshotIndexWriterTests(unittest.TestCase):
    """W0-5 rider: the index writer is buffered but still durable."""

    def test_index_writer_is_buffered_and_still_fsyncs(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=True)
            real_fdopen = scanner.os.fdopen
            buffering: list[object] = []
            synced: list[int] = []

            def recording_fdopen(descriptor, mode="r", *args, **keywords):
                if mode == "wb":
                    buffering.append(args[0] if args else keywords.get("buffering"))
                return real_fdopen(descriptor, mode, *args, **keywords)

            real_fsync = scanner.os.fsync

            def recording_fsync(descriptor):
                synced.append(descriptor)
                return real_fsync(descriptor)

            with (
                mock.patch.object(scanner.os, "fdopen", side_effect=recording_fdopen),
                mock.patch.object(scanner.os, "fsync", side_effect=recording_fsync),
            ):
                capture = live_capture(fixture, "source-a")

            self.assertNotIn(0, buffering)
            self.assertIn(scanner.SNAPSHOT_INDEX_BUFFER_BYTES, buffering)
            self.assertTrue(synced)
            # The seal still describes the bytes that landed on disk.
            snapshot = capture["catalog"]["snapshot"]
            index = Path(snapshot["index_path"])
            self.assertEqual(
                hashlib.sha256(index.read_bytes()).hexdigest(),
                snapshot["index_sha256"],
            )
            self.assertEqual(
                len(index.read_bytes().splitlines()), snapshot["index_entries"]
            )
            validate_agent_capture(capture, expected_role="source")

    def test_a_partial_index_write_is_never_published(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            real = scanner._snapshot_index_record
            seen: list[str] = []

            def failing(path, **keywords):
                seen.append(str(path))
                if len(seen) > 3:
                    raise BulkloadError("synthetic index-write failure")
                return real(path, **keywords)

            with mock.patch.object(
                scanner, "_snapshot_index_record", side_effect=failing
            ):
                with self.assertRaises(BulkloadError):
                    live_capture(fixture, "source-a")
            self.assertFalse((fixture.root / "evidence" / "source-a.snapshot").exists())


class PhaseTimingAndJobsTests(unittest.TestCase):
    """S5' (PH-S0): stderr-only phase timing and --jobs plumbing."""

    def test_default_job_count_is_the_shipped_constant(self) -> None:
        self.assertEqual(scanner.MAX_CAPTURE_WORKSPACE_WORKERS, 3)
        self.assertEqual(scanner.workspace_worker_count(None, 10), 3)
        self.assertEqual(scanner.workspace_worker_count(None, 2), 2)
        self.assertEqual(scanner.workspace_worker_count(None, 0), 0)

    def test_explicit_job_count_is_bounded_by_pending_work(self) -> None:
        self.assertEqual(scanner.workspace_worker_count(1, 10), 1)
        self.assertEqual(scanner.workspace_worker_count(8, 10), 8)
        self.assertEqual(scanner.workspace_worker_count(8, 2), 2)

    def test_out_of_range_job_counts_are_refused(self) -> None:
        for jobs in (0, -1, scanner.MAX_CAPTURE_JOBS + 1):
            with self.subTest(jobs=jobs):
                with self.assertRaisesRegex(
                    BulkloadError, "capture job count is out of range"
                ):
                    scanner.workspace_worker_count(jobs, 10)

    def test_capture_refuses_an_out_of_range_job_count(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            with self.assertRaisesRegex(
                BulkloadError, "capture job count is out of range"
            ):
                capture_agent_state(
                    role="source",
                    home=fixture.source_home,
                    git_root=fixture.source_git,
                    codex_root=None,
                    claude_root=None,
                    pi_root=None,
                    seats=fixture.source_seats,
                    path_map=fixture.path_map,
                    writers_quiesced=True,
                    rsync_path=fixture.rsync_path,
                    jobs=0,
                )

    def test_parser_jobs_default_is_unset(self) -> None:
        base = [
            "agent-capture",
            "--role",
            "source",
            "--home",
            "/home",
            "--git-root",
            "/home/git",
            "--rsync-path",
            "/usr/bin/rsync",
            "--path-map",
            "/a=/b",
            "--output",
            "/tmp/out.json",
        ]
        parser = build_parser()
        self.assertIsNone(parser.parse_args(base).jobs)
        self.assertEqual(parser.parse_args([*base, "--jobs", "8"]).jobs, 8)

    def test_timing_is_silent_unless_the_operator_asks(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            stream = io.StringIO()
            with mock.patch.dict(os.environ, {}, clear=False):
                os.environ.pop("BULKLOAD_PHASE_TIMING", None)
                with contextlib.redirect_stderr(stream):
                    capture = live_capture(fixture, "source-a")
            self.assertEqual(stream.getvalue(), "")
            validate_agent_capture(capture, expected_role="source")

    def test_timing_emits_one_stderr_line_per_phase(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            stream = io.StringIO()
            with mock.patch.dict(os.environ, {"BULKLOAD_PHASE_TIMING": "1"}):
                with contextlib.redirect_stderr(stream):
                    capture = live_capture(fixture, "source-a")
            lines = [
                line
                for line in stream.getvalue().splitlines()
                if line.startswith("bulkload-phase ")
            ]
            self.assertTrue(lines)
            phases = {
                dict(field.split("=", 1) for field in line.split(" ")[1:])["phase"]
                for line in lines
            }
            self.assertLessEqual(
                {"census", "charge", "copy", "digest", "catalog", "seal"}, phases
            )
            for line in lines:
                fields = dict(field.split("=", 1) for field in line.split(" ")[1:])
                self.assertEqual(
                    sorted(fields), ["bytes", "files", "phase", "root", "seconds"]
                )
                float(fields["seconds"])
            # Timing is diagnostic only: it never reaches an artifact.
            self.assertNotIn(b"bulkload-phase", canonical_bytes(capture))
            validate_agent_capture(capture, expected_role="source")

    def test_timing_does_not_change_the_sealed_generations(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            quiet = live_capture(fixture, "quiet")["catalog"]["snapshot"]
            stream = io.StringIO()
            with mock.patch.dict(os.environ, {"BULKLOAD_PHASE_TIMING": "1"}):
                with contextlib.redirect_stderr(stream):
                    loud = live_capture(fixture, "loud")["catalog"]["snapshot"]
            self.assertEqual(
                [root["generation_sha256"] for root in quiet["roots"]],
                [root["generation_sha256"] for root in loud["roots"]],
            )
            self.assertEqual(
                quiet["git_generation_sha256"], loud["git_generation_sha256"]
            )
            self.assertEqual(quiet["index_sha256"], loud["index_sha256"])


class EvidenceMemoryGateTests(unittest.TestCase):
    """S6': the release step must not hard-abort where memory is unmeasurable."""

    LARGE = cli.LARGE_PLAN_THRESHOLD_BYTES + 1

    def test_small_evidence_never_probes_memory(self) -> None:
        with mock.patch.object(cli, "_available_memory") as probe:
            cli._require_large_evidence_memory(cli.LARGE_PLAN_THRESHOLD_BYTES)
        probe.assert_not_called()

    def test_unmeasurable_memory_does_not_abort(self) -> None:
        with (
            mock.patch.object(cli, "_proc_meminfo_available", return_value=None),
            mock.patch.object(cli, "_sysconf_available", return_value=None),
        ):
            self.assertIsNone(cli._available_memory())
            cli._require_large_evidence_memory(self.LARGE)

    def test_absent_proc_meminfo_reports_unmeasurable(self) -> None:
        if Path("/proc/meminfo").exists():
            self.skipTest("/proc/meminfo exists on this host")
        self.assertIsNone(cli._proc_meminfo_available())

    def test_a_real_shortfall_still_aborts(self) -> None:
        with mock.patch.object(cli, "_available_memory", return_value=1):
            with self.assertRaisesRegex(
                BulkloadError, "below the bounded Bulkload evidence gate"
            ):
                cli._require_large_evidence_memory(self.LARGE)

    def test_the_multiplier_is_still_four(self) -> None:
        size = self.LARGE
        with mock.patch.object(
            cli, "_available_memory", return_value=size * 4 + 2 * 1024**3
        ):
            cli._require_large_evidence_memory(size)
        with mock.patch.object(
            cli, "_available_memory", return_value=size * 4 + 2 * 1024**3 - 1
        ):
            with self.assertRaises(BulkloadError):
                cli._require_large_evidence_memory(size)

    def test_the_planning_gate_shares_the_portable_probe(self) -> None:
        with mock.patch.object(cli, "_available_memory", return_value=1):
            with self.assertRaisesRegex(
                BulkloadError, "below the bounded AgentPlanV4 planning gate"
            ):
                cli._require_large_evidence_memory(
                    self.LARGE,
                    message=(
                        "destination memory is below the bounded AgentPlanV4 "
                        "planning gate"
                    ),
                )


def flip_byte(path: Path, offset: int = 0) -> None:
    """Change one byte in place without changing the file's size."""
    payload = bytearray(path.read_bytes())
    payload[offset] = payload[offset] ^ 0x01
    mode = path.stat().st_mode
    path.chmod(0o600)
    path.write_bytes(bytes(payload))
    path.chmod(mode & 0o777)


class BaseCustodyTests(unittest.TestCase):
    """S8' (II-S3): --base-custody=sealed, with the X3 git carve-out."""

    @staticmethod
    def base_snapshot(capture: dict) -> dict:
        return capture["catalog"]["snapshot"]

    @staticmethod
    def payload(snapshot: dict, label: str, relative: str) -> Path:
        root = next(item for item in snapshot["roots"] if item["label"] == label)
        return Path(root["snapshot"]) / relative

    def test_sealed_returns_the_identical_record_map(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=True)
            snapshot = self.base_snapshot(live_capture(fixture, "source-a"))
            self.assertEqual(
                scanner.validate_snapshot_custody(snapshot, collect_records=True),
                scanner.validate_snapshot_custody(
                    snapshot, collect_records=True, payload_custody="sealed"
                ),
            )

    def test_sealed_refuses_a_tampered_seal(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            snapshot = self.base_snapshot(live_capture(fixture, "source-a"))
            flip_byte(Path(snapshot["seal_path"]), 8)
            with self.assertRaisesRegex(BulkloadError, "custody seal or index"):
                scanner.validate_snapshot_custody(
                    snapshot, collect_records=True, payload_custody="sealed"
                )

    def test_sealed_refuses_a_tampered_index(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            snapshot = self.base_snapshot(live_capture(fixture, "source-a"))
            index = Path(snapshot["index_path"])
            flip_byte(index, len(index.read_bytes()) // 2)
            with self.assertRaisesRegex(BulkloadError, "custody seal or index"):
                scanner.validate_snapshot_custody(
                    snapshot, collect_records=True, payload_custody="sealed"
                )

    def test_sealed_always_re_verifies_the_git_root(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            snapshot = self.base_snapshot(live_capture(fixture, "source-a"))
            flip_byte(self.payload(snapshot, "git", "repo/tracked.txt"))
            for mode in ("full", "sealed"):
                with self.subTest(mode=mode):
                    with self.assertRaisesRegex(
                        BulkloadError, "snapshot payload differs from sealed index"
                    ):
                        scanner.validate_snapshot_custody(
                            snapshot, collect_records=True, payload_custody=mode
                        )

    def test_sealed_always_re_verifies_sqlite_payloads(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=True)
            snapshot = self.base_snapshot(live_capture(fixture, "source-a"))
            codex = next(
                root for root in snapshot["roots"] if root["label"] == "provider-codex"
            )
            self.assertTrue(codex["sqlite"])
            relative = codex["sqlite"][0]["relative_path"]
            flip_byte(self.payload(snapshot, "provider-codex", relative), 32)
            for mode in ("full", "sealed"):
                with self.subTest(mode=mode):
                    with self.assertRaisesRegex(
                        BulkloadError, "snapshot payload differs from sealed index"
                    ):
                        scanner.validate_snapshot_custody(
                            snapshot, collect_records=True, payload_custody=mode
                        )

    def test_sealed_elides_only_ordinary_provider_payloads(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            snapshot = self.base_snapshot(live_capture(fixture, "source-a"))
            target = self.payload(snapshot, "provider-codex", "history.jsonl")
            flip_byte(target, 2)
            with self.assertRaisesRegex(
                BulkloadError, "snapshot payload differs from sealed index"
            ):
                scanner.validate_snapshot_custody(snapshot, collect_records=True)
            # sealed trades exactly this re-read away, and nothing else: the
            # namespace, count, mode and index digests are all still enforced.
            scanner.validate_snapshot_custody(
                snapshot, collect_records=True, payload_custody="sealed"
            )

    def test_sealed_still_enforces_the_anti_planting_perimeter(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            snapshot = self.base_snapshot(live_capture(fixture, "source-a"))
            planted = Path(snapshot["seal_path"]).parent / "undeclared"
            planted.write_text("planted", encoding="utf-8")
            with self.assertRaisesRegex(BulkloadError, "top-level namespace"):
                scanner.validate_snapshot_custody(
                    snapshot, collect_records=True, payload_custody="sealed"
                )
            planted.unlink()
            extra = self.payload(snapshot, "provider-codex", "planted.json")
            extra.write_text("{}", encoding="utf-8")
            with self.assertRaisesRegex(BulkloadError, "index count or digest"):
                scanner.validate_snapshot_custody(
                    snapshot, collect_records=True, payload_custody="sealed"
                )

    def test_sealed_reads_fewer_payloads_than_full(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            snapshot = self.base_snapshot(live_capture(fixture, "source-a"))

            def count(mode: str) -> list[str]:
                seen: list[str] = []
                real = scanner._snapshot_index_record

                def counted(path, **keywords):
                    seen.append(str(path))
                    return real(path, **keywords)

                with mock.patch.object(
                    scanner, "_snapshot_index_record", side_effect=counted
                ):
                    scanner.validate_snapshot_custody(
                        snapshot, collect_records=True, payload_custody=mode
                    )
                return seen

            full = count("full")
            sealed = count("sealed")
            self.assertLess(len(sealed), len(full))
            git_root = next(
                item["snapshot"] for item in snapshot["roots"] if item["label"] == "git"
            )
            self.assertEqual(
                [path for path in full if path.startswith(git_root)],
                [path for path in sealed if path.startswith(git_root)],
            )

    def test_sealed_cannot_be_combined_with_required_paths(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            snapshot = self.base_snapshot(live_capture(fixture, "source-a"))
            with self.assertRaisesRegex(BulkloadError, "cannot subset required paths"):
                scanner.validate_snapshot_custody(
                    snapshot,
                    required_paths={Path(snapshot["index_path"])},
                    payload_custody="sealed",
                )

    def test_unsupported_custody_mode_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            snapshot = self.base_snapshot(live_capture(fixture, "source-a"))
            with self.assertRaisesRegex(
                BulkloadError, "payload custody mode is unsupported"
            ):
                scanner.validate_snapshot_custody(snapshot, payload_custody="stat")

    def test_chained_capture_under_sealed_matches_full(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            first = live_capture(fixture, "source-a")
            chained_full = live_capture(fixture, "source-b", base=first)
            validate_agent_capture(chained_full, expected_role="source")
            chained_sealed = capture_agent_state(
                role="source",
                home=fixture.source_home,
                git_root=fixture.source_git,
                codex_root=None,
                claude_root=None,
                pi_root=None,
                seats=fixture.source_seats,
                path_map=fixture.path_map,
                writers_quiesced=False,
                snapshot_root=fixture.root / "evidence" / "source-c.snapshot",
                snapshot_base_seal=Path(self.base_snapshot(first)["seal_path"]),
                managed_exclusions=fixture.managed_exclusions,
                rsync_path=fixture.rsync_path,
                max_files=50_000,
                max_bytes=4 * 1024**3,
                max_sqlite_rows=100_000,
                snapshot_reserve_bytes=0,
                base_custody="sealed",
            )
            validate_agent_capture(chained_sealed, expected_role="source")
            full = self.base_snapshot(chained_full)
            sealed = self.base_snapshot(chained_sealed)
            self.assertEqual(full["index_sha256"], sealed["index_sha256"])
            self.assertEqual(
                [root["generation_sha256"] for root in full["roots"]],
                [root["generation_sha256"] for root in sealed["roots"]],
            )
            self.assertIn("base-reflink", sealed["methods"])

    def test_base_custody_requires_a_base_seal(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            with self.assertRaisesRegex(
                BulkloadError, "base custody mode requires a base seal"
            ):
                capture_agent_state(
                    role="source",
                    home=fixture.source_home,
                    git_root=fixture.source_git,
                    codex_root=None,
                    claude_root=None,
                    pi_root=None,
                    seats=fixture.source_seats,
                    path_map=fixture.path_map,
                    writers_quiesced=False,
                    snapshot_root=fixture.root / "evidence" / "orphan.snapshot",
                    managed_exclusions=fixture.managed_exclusions,
                    rsync_path=fixture.rsync_path,
                    snapshot_reserve_bytes=0,
                    base_custody="sealed",
                )

    def test_parser_base_custody_defaults_to_full(self) -> None:
        base = [
            "agent-capture",
            "--role",
            "source",
            "--home",
            "/home",
            "--git-root",
            "/home/git",
            "--rsync-path",
            "/usr/bin/rsync",
            "--path-map",
            "/a=/b",
            "--output",
            "/tmp/out.json",
        ]
        parser = build_parser()
        self.assertEqual(parser.parse_args(base).base_custody, "full")
        self.assertEqual(
            parser.parse_args([*base, "--base-custody", "sealed"]).base_custody,
            "sealed",
        )


def prepared_final_push(fixture: CutoverFixture) -> dict:
    """Everything up to the final push, which two tests below both need.

    The fence only exists on a live (non-quiesced) capture, so this needs the
    chained live plan, not `CutoverFixture.plan()`.
    """
    source_a = live_capture(fixture, "source-a")
    source_b = live_capture(fixture, "source-b", base=source_a)
    destination_a = live_capture(fixture, "dest-a", role="destination")
    destination_b = live_capture(
        fixture, "dest-b", role="destination", base=destination_a
    )
    plan = compile_agent_plan(source_a, source_b, destination_a, destination_b)
    fake_ssh = fixture.root / "fake-ssh"
    fake_ssh.write_text(FAKE_SSH, encoding="utf-8")
    fake_ssh.chmod(0o700)
    stage = fixture.root / "transport-stage"
    common = {"accepted_plan_sha256": plan["plan_sha256"], "stage_root": stage}
    preseed_prepare = stage_agent_plan(
        plan,
        phase="preseed",
        allow_accounted_copy=True,
        reserve_bytes=0,
        transport_mode="prepare",
        **common,
    )
    preseed_transport = push_agent_transport(
        preseed_prepare,
        stage / ".transport-allowlist-preseed.nul",
        phase="preseed",
        destination_ssh_host=socket.gethostname(),
        _ssh_binary=str(fake_ssh),
        **common,
    )
    stage_agent_plan(
        plan,
        phase="preseed",
        allow_accounted_copy=True,
        reserve_bytes=0,
        transport_mode="materialize",
        prepare_receipt=preseed_prepare,
        transport_receipt=preseed_transport,
        **common,
    )
    prepare = stage_agent_plan(
        plan,
        phase="final",
        allow_accounted_copy=True,
        reserve_bytes=0,
        transport_mode="prepare",
        **common,
    )
    return {
        "allowlist": stage / ".transport-allowlist-final.nul",
        "common": common,
        "fake_ssh": fake_ssh,
        "plan": plan,
        "prepare": prepare,
        "snapshot": plan["source"]["catalog"]["snapshot"],
    }


class DominatedFenceWiringTests(unittest.TestCase):
    """W0-1: exactly one call site is allowed to drop a pass."""

    def test_push_fences_once_before_the_push_and_at_full_strength_after(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            prepared = prepared_final_push(fixture)

            real = executor.validate_live_snapshot_generation
            recorded: list[int] = []

            def recording(snapshot, *, passes=2):
                recorded.append(passes)
                return real(snapshot, passes=passes)

            with mock.patch.object(
                executor, "validate_live_snapshot_generation", side_effect=recording
            ):
                push_agent_transport(
                    prepared["prepare"],
                    prepared["allowlist"],
                    phase="final",
                    destination_ssh_host=socket.gethostname(),
                    _ssh_binary=str(prepared["fake_ssh"]),
                    **prepared["common"],
                )
            # Pre-push is dominated and may run one pass; post-push is the
            # last fence before the receipt exists and keeps both.
            self.assertEqual(recorded, [1, 2])

    def test_break_glass_bypasses_both_push_fences_and_records_each(self) -> None:
        """The break-glass at a real executor call site, not at the unit.

        `push_agent_transport` is the only path that runs the fence twice for
        one snapshot. Both bypasses have to appear in the ledger with their
        own `passes_requested`, including the dominated `passes=1` site at
        `executor.py:491`, or the record understates what was skipped.
        """
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            prepared = prepared_final_push(fixture)
            note = fixture.root / "push-deviations.jsonl"
            expected = sealed_expectation(self, prepared["snapshot"])

            environment = {
                BREAK_GLASS: os.fspath(note),
                BREAK_GLASS_EXPECT: expected,
            }
            with mock.patch.dict(os.environ, environment):
                with no_root_may_be_read():
                    with contextlib.redirect_stderr(io.StringIO()) as stderr:
                        push_agent_transport(
                            prepared["prepare"],
                            prepared["allowlist"],
                            phase="final",
                            destination_ssh_host=socket.gethostname(),
                            _ssh_binary=str(prepared["fake_ssh"]),
                            **prepared["common"],
                        )
            records = [
                json.loads(line)
                for line in note.read_text(encoding="utf-8").splitlines()
            ]
            self.assertEqual([record["passes_requested"] for record in records], [1, 2])
            self.assertEqual({record["expected"] for record in records}, {expected})
            self.assertEqual(
                stderr.getvalue().count("BREAK-GLASS live fence skipped"), 2
            )


if __name__ == "__main__":
    unittest.main()
