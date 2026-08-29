"""Wave-2 memory-slice regressions.

Wave 2 buys residency, not minutes. Every slice must therefore be *output*
identical — the differential gate from Wave 0 applies again — while moving a
structure that was O(files) resident onto disk or into a bounded window.

Each slice here carries two kinds of test:

* a **differential** test that runs the same fixture with the slice's env-var
  rollback lever off and on and asserts the sealed digests are byte-identical;
* an **adversarial** test that would fail if the bounded form silently lost an
  entry, mis-ordered one, or answered a lookup with the wrong record.
"""

from __future__ import annotations

import ast
import contextlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

from bulkload_lib import model, scanner
from bulkload_lib.model import BulkloadError, canonical_bytes

from test_bulkload import CutoverFixture
from test_wave0_perf import live_capture


def index_line(root_index: int, relative: str) -> bytes:
    """One sealed index record in exactly the shape the validator writes."""
    return (
        canonical_bytes(
            {
                "destination_device": 1,
                "kind": "regular",
                "method": "capacity-accounted-copy",
                "mode": "0600",
                "relative_path": relative,
                "root_index": root_index,
                "sha256": f"{root_index:064x}",
                "size": len(relative),
                "source_device": 1,
            }
        )
        + b"\n"
    )


def build_map(
    labels: list[str], rows: list[tuple[int, str]], *, spill_dir: Path | None
) -> scanner._BaseRecordMap:
    store = scanner._BaseRecordMap(labels, spill_dir=spill_dir)
    for root_index, relative in rows:
        store.append(root_index, relative, index_line(root_index, relative))
    store.seal()
    return store


class BaseRecordMapTests(unittest.TestCase):
    """W2-1: the spilled chained base-record map."""

    # Sorted the way the sealed index is sorted: by (root_index, relative),
    # with `relative` compared as a Python str. `a.c` before `a/b` because
    # U+002E precedes U+002F — the case a naive path-aware sort gets wrong.
    LABELS = ["git", "provider-codex", "seat-claude"]
    ROWS = [
        (0, "."),
        (0, "a.c"),
        (0, "a/b"),
        (0, "a/b/c"),
        (0, "z"),
        (1, "."),
        (1, "sessions/z.jsonl"),
        (1, "sessions/été.jsonl"),
        (1, "sessions/\U0001f9ea.jsonl"),
        (2, "."),
    ]

    def test_spilled_answers_every_lookup_exactly_as_resident(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            resident = build_map(self.LABELS, self.ROWS, spill_dir=None)
            spilled = build_map(self.LABELS, self.ROWS, spill_dir=Path(temporary))
            try:
                self.assertEqual(resident, spilled)
                self.assertTrue(spilled.spilled)
                self.assertFalse(resident.spilled)
                for root_index, relative in self.ROWS:
                    label = self.LABELS[root_index]
                    with self.subTest(label=label, relative=relative):
                        expected = resident.by_label(label).get(relative)
                        self.assertEqual(
                            expected, json.loads(index_line(root_index, relative))
                        )
                        self.assertEqual(
                            spilled.by_label(label).get(relative), expected
                        )
            finally:
                spilled.close()

    def test_a_relative_from_another_root_is_not_answered(self) -> None:
        """The adversarial case a single global binary search would get wrong.

        `z` exists under `git` and `sessions/z.jsonl` under `provider-codex`.
        A view must never reach outside its own root's span, or a chained
        capture would reuse one root's base bytes for another root's file.
        """
        with tempfile.TemporaryDirectory() as temporary:
            for spill in (None, Path(temporary)):
                store = build_map(self.LABELS, self.ROWS, spill_dir=spill)
                try:
                    with self.subTest(spilled=store.spilled):
                        self.assertIsNone(store.by_label("provider-codex").get("z"))
                        self.assertIsNone(store.by_label("seat-claude").get("a/b"))
                        self.assertIsNone(store.by_label("git").get("missing"))
                        self.assertEqual(len(store.by_label("seat-claude")), 1)
                finally:
                    store.close()

    def test_a_missing_key_between_two_present_keys_is_not_answered(self) -> None:
        """A binary search that returns the neighbour on a miss is fail-open.

        `_base_regular_reusable` reads `sha256` straight out of whatever the
        lookup returns, so answering `a/b0` with `a/b`'s record would make the
        copy reuse the wrong base file's bytes.
        """
        with tempfile.TemporaryDirectory() as temporary:
            store = build_map(self.LABELS, self.ROWS, spill_dir=Path(temporary))
            try:
                view = store.by_label("git")
                for probe in ("a", "a/", "a/b0", "a/b/", "aa", "", "~"):
                    with self.subTest(probe=probe):
                        self.assertIsNone(view.get(probe))
            finally:
                store.close()

    def test_key_order_is_the_str_order_the_index_was_sealed_in(self) -> None:
        """UTF-8 is order-preserving, so the byte search is the str search.

        If it were not, the search would walk the wrong half on any tree with
        a non-ASCII name and start missing records that are present.
        """
        relatives = [relative for root_index, relative in self.ROWS if root_index == 1]
        self.assertEqual(relatives, sorted(relatives))
        self.assertEqual(
            [item.encode("utf-8", "surrogatepass") for item in relatives],
            sorted(item.encode("utf-8", "surrogatepass") for item in relatives),
        )

    def test_an_empty_root_view_is_falsy_so_the_or_fallback_still_fires(self) -> None:
        """`(base_records or {}).get(...)` is the extant call shape."""
        store = build_map(self.LABELS, [(0, ".")], spill_dir=None)
        self.assertFalse(store.by_label("seat-claude"))
        self.assertIsNone((store.by_label("seat-claude") or {}).get("."))

    def test_spill_dir_inside_the_live_snapshot_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "snapshot"
            inside = root / "roots"
            inside.mkdir(parents=True)
            with mock.patch.dict(os.environ, {"BULKLOAD_SPILL_DIR": str(inside)}):
                with self.assertRaisesRegex(BulkloadError, "spill dir is inside"):
                    scanner._base_record_spill_dir(root, Path(temporary) / "partial")

    def test_lever_off_reads_no_other_spill_variable_at_all(self) -> None:
        """X1: the off position must restore shipped behaviour *exactly*.

        The lever used to be read at the `_BaseRecordMap` construction, which
        is downstream of the refusal above — so with the lever off and
        `BULKLOAD_SPILL_DIR` pointed inside the capture, the engine still died
        on a refusal the shipped engine has no equivalent of. A rollback lever
        that can still fail the capture is not a rollback lever.
        """
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "snapshot"
            inside = root / "roots"
            inside.mkdir(parents=True)
            with mock.patch.dict(
                os.environ,
                {
                    "BULKLOAD_SPILL_DIR": str(inside),
                    "BULKLOAD_SPILL_BASE_RECORDS": "0",
                },
            ):
                self.assertIsNone(
                    scanner._base_record_spill_dir(root, Path(temporary) / "partial")
                )

    def test_lever_off_ignores_a_spill_dir_inside_the_capture(self) -> None:
        """The same counterexample end to end, through a real chained capture."""
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=True)
            first = live_capture(fixture, "source-a")
            with mock.patch.dict(
                os.environ,
                {
                    "BULKLOAD_SPILL_BASE_RECORDS": "0",
                    "BULKLOAD_SPILL_DIR": str(
                        fixture.root / "evidence" / "source-b.snapshot"
                    ),
                },
            ):
                chained = live_capture(fixture, "source-b", base=first)
            self.assertIn("capture_sha256", chained)

    def test_an_unusable_spill_dir_fails_closed(self) -> None:
        """`validate_snapshot_custody` promises `BulkloadError` on every refusal.

        The arena is constructed outside that promise's `try`, so an unwritable
        or full spill volume escaped as a raw `OSError` — past the fail-closed
        fence, and past `_capture_live_snapshot`'s `finally: base_index.close()`,
        which never saw an instance to close.
        """
        with tempfile.TemporaryDirectory() as temporary:
            missing = Path(temporary) / "no-such-volume"
            with self.assertRaisesRegex(BulkloadError, "arena cannot be opened"):
                scanner._BaseRecordMap(["git"], spill_dir=missing)

    def test_a_spill_volume_that_fills_refuses_rather_than_truncates(self) -> None:
        """A short write must never become a silently truncated arena."""
        with tempfile.TemporaryDirectory() as temporary:
            store = scanner._BaseRecordMap(["git"], spill_dir=Path(temporary))
            try:
                with mock.patch.object(
                    store._arena, "write", side_effect=OSError(28, "No space left")
                ):
                    with self.assertRaisesRegex(
                        BulkloadError, "arena cannot be written"
                    ):
                        store.append(0, "a", index_line(0, "a"))
                with mock.patch.object(
                    store._arena, "flush", side_effect=OSError(28, "No space left")
                ):
                    with self.assertRaisesRegex(
                        BulkloadError, "arena cannot be written"
                    ):
                        store.seal()
            finally:
                store.close()

    def test_close_never_raises_and_never_masks_the_real_failure(self) -> None:
        """`close()` runs from `finally` and from `seal`'s error path.

        Flushing a dirty buffer to the volume that just refused the write would
        raise a second time and hide whatever the capture actually died of.
        """
        with tempfile.TemporaryDirectory() as temporary:
            store = scanner._BaseRecordMap(["git"], spill_dir=Path(temporary))
            released = store._arena.close

            def close_then_fail() -> None:
                released()  # really release the descriptor, then fail like ENOSPC
                raise OSError(28, "No space left")

            with mock.patch.object(store._arena, "close", close_then_fail):
                store.close()
            self.assertFalse(store.spilled)

    def test_the_memory_backed_mount_parse_picks_the_longest_mount(self) -> None:
        """A tmpfs under a disk-backed parent still has to be reported.

        Parsed as a pure function of the `/proc/self/mountinfo` text so this
        runs on darwin, where the file does not exist and the check no-ops.
        """
        mountinfo = "\n".join(
            (
                "21 1 0:20 / / rw,relatime - apfs /dev/disk3s1 rw",
                "22 21 0:21 / /tmp rw,relatime - tmpfs tmpfs rw",
                "23 21 0:22 / /tmp/durable rw - ext4 /dev/sdb1 rw",
                "24 21 0:23 / /var/spill\\040dir rw - ramfs ramfs rw",
            )
        )
        for candidate, expected in (
            ("/var/tmp", False),
            ("/tmp", True),
            ("/tmp/runs/a", True),
            ("/tmp/durable", False),
            ("/tmp/durable/runs", False),
            ("/var/spill dir/x", True),
            ("/nowhere", False),
        ):
            with self.subTest(candidate=candidate):
                self.assertIs(
                    scanner._memory_backed_mount(Path(candidate), mountinfo), expected
                )

    def test_the_lever_is_off_only_for_the_documented_spellings(self) -> None:
        for value, expected in (
            ("0", False),
            ("false", False),
            ("OFF", False),
            ("no", False),
            ("1", True),
            ("", True),
            ("yes", True),
        ):
            with self.subTest(value=value):
                with mock.patch.dict(os.environ, {"BULKLOAD_LEVER_PROBE": value}):
                    self.assertIs(scanner._env_lever("BULKLOAD_LEVER_PROBE"), expected)


class ChainedCaptureDifferentialTests(unittest.TestCase):
    """The Wave-0 landing gate, re-applied: stock vs branch must agree byte-wise."""

    LEVERS = ("BULKLOAD_SPILL_BASE_RECORDS", "BULKLOAD_BOUND_NAMESPACE")

    @staticmethod
    def evidence(capture: dict) -> dict:
        """Everything in a sealed capture that a memory slice could move.

        `capture_sha256`, `catalog_sha256` and `snapshot_id` are excluded on
        purpose: they bind the capture's own identity and its absolute paths,
        which differ between two captures of the same tree by construction.
        Everything below is a pure function of the bytes that were captured
        and of which copy method each payload took.
        """
        snapshot = capture["catalog"]["snapshot"]
        return {
            "index_sha256": snapshot["index_sha256"],
            "index_entries": snapshot["index_entries"],
            "inventory_sha256": snapshot["inventory_sha256"],
            "contract_sha256": snapshot["contract_sha256"],
            "git_generation_sha256": snapshot["git_generation_sha256"],
            "methods": snapshot["methods"],
            "generations": [root["generation_sha256"] for root in snapshot["roots"]],
        }

    def test_every_lever_is_byte_identical_on_a_chained_capture(self) -> None:
        """One fixture, one base, the chained leg captured once per lever.

        Capturing into two different temporary roots would compare two
        different corpora — the fixture's own files embed their absolute
        paths — so the comparison has to reuse a single live tree.
        """
        for lever in self.LEVERS:
            with self.subTest(lever=lever):
                with tempfile.TemporaryDirectory() as temporary:
                    fixture = CutoverFixture(Path(temporary), sqlite_union=True)
                    first = live_capture(fixture, "source-a")
                    seen = {}
                    for setting in ("0", "1"):
                        with mock.patch.dict(os.environ, {lever: setting}):
                            seen[setting] = self.evidence(
                                live_capture(fixture, f"source-b-{setting}", base=first)
                            )
                    self.assertEqual(seen["0"], seen["1"])

    def test_a_chained_capture_reuses_base_bytes_under_the_spill(self) -> None:
        """Self-refutation guard: if the spilled map answered every lookup with
        `None` the capture would still seal identically — it would just copy
        everything cold. Pin that base reuse actually happens."""
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=True)
            first = live_capture(fixture, "source-a")
            second = live_capture(fixture, "source-b", base=first)
            methods = second["catalog"]["snapshot"]["methods"]
            self.assertTrue(
                any(name.startswith("base-") for name in methods),
                f"no base reuse happened at all: {sorted(methods)}",
            )


def reference_snapshot_namespace(root: Path) -> list[tuple[str, Path]]:
    """The pre-slice `_snapshot_namespace`, kept verbatim as the value oracle."""
    observed: list[tuple[str, Path]] = [(".", root)]
    if root.is_dir():
        for current, directories, files in os.walk(
            root, topdown=True, followlinks=False
        ):
            directories[:] = sorted(directories)
            current_path = Path(current)
            observed.extend(
                (
                    (current_path / name).relative_to(root).as_posix(),
                    current_path / name,
                )
                for name in directories
            )
            observed.extend(
                (
                    (current_path / name).relative_to(root).as_posix(),
                    current_path / name,
                )
                for name in sorted(files)
            )
    return sorted(observed)


def adversarial_tree(root: Path) -> None:
    """Names chosen so a path-aware or byte-naive sort orders them differently."""
    root.mkdir(parents=True)
    # `a.c` < `a/b` because U+002E < U+002F: a sort that compares path parts
    # instead of the posix string puts these the other way round.
    (root / "a.c").write_text("a.c")
    (root / "a").mkdir()
    (root / "a" / "b").write_text("b")
    (root / "a" / "b.d").write_text("b.d")
    (root / "a-b").write_text("a-b")
    (root / "a0").write_text("a0")
    # Non-ASCII and non-BMP, both of which must sort by code point, not by
    # UTF-8 byte length. (A name that is not valid UTF-8 cannot be created at
    # all on APFS — the filesystem refuses it — so the surrogateescape path is
    # covered by the encode/decode round-trip test instead.)
    (root / "été.txt").write_text("e")
    (root / "\U0001f9ea.txt").write_text("t")
    (root / "Z").write_text("Z")
    (root / "z").write_text("z")
    (root / "~tilde").write_text("~")
    nested = root / "deep" / "er" / "est"
    nested.mkdir(parents=True)
    (nested / "leaf").write_text("leaf")
    (root / "empty").mkdir()


class SnapshotNamespaceTests(unittest.TestCase):
    """W2-2: the bounded namespace walk. Its order is digest-load-bearing."""

    def emitted(self, root: Path, **environment: str) -> list[tuple[str, Path]]:
        with mock.patch.dict(os.environ, environment):
            return list(scanner._snapshot_namespace(root))

    def test_the_generator_matches_the_old_list_element_for_element(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "root"
            adversarial_tree(root)
            expected = reference_snapshot_namespace(root)
            # chunk=2 forces the external merge on a tree of ~16 entries, so
            # the merge path is what is actually being compared.
            for chunk in ("2", "3", "1000000"):
                with self.subTest(chunk=chunk):
                    self.assertEqual(
                        self.emitted(root, BULKLOAD_NAMESPACE_CHUNK=chunk), expected
                    )
            self.assertEqual(self.emitted(root, BULKLOAD_BOUND_NAMESPACE="0"), expected)

    def test_the_merge_really_ran(self) -> None:
        """Self-refutation guard: with a chunk larger than the tree the code
        never leaves the in-memory `sorted()` branch, so a broken merge would
        pass every equality test above. Pin that chunk=2 spills."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "root"
            adversarial_tree(root)
            with mock.patch.dict(os.environ, {"BULKLOAD_NAMESPACE_CHUNK": "2"}):
                with mock.patch.object(
                    scanner, "_namespace_run", wraps=scanner._namespace_run
                ) as spy:
                    entries = list(scanner._snapshot_namespace(root))
            self.assertGreater(spy.call_count, 1)
            self.assertEqual(len(entries), len(reference_snapshot_namespace(root)))

    def test_a_leaf_only_root_and_a_missing_root_still_answer(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            leaf = Path(temporary) / "leaf"
            leaf.write_text("leaf")
            absent = Path(temporary) / "absent"
            for chunk in ("1", "1000000"):
                with self.subTest(chunk=chunk):
                    self.assertEqual(
                        self.emitted(leaf, BULKLOAD_NAMESPACE_CHUNK=chunk),
                        [(".", leaf)],
                    )
                    self.assertEqual(
                        self.emitted(absent, BULKLOAD_NAMESPACE_CHUNK=chunk),
                        [(".", absent)],
                    )

    def test_the_reconstructed_path_is_the_path_the_walk_produced(self) -> None:
        """Every consumer stats the second element; a join that normalised the
        path differently would stat a different file."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "root"
            adversarial_tree(root)
            for relative, path in scanner._snapshot_namespace(root):
                with self.subTest(relative=relative):
                    self.assertTrue(path.exists() or path.is_symlink())
                    self.assertEqual(
                        path.relative_to(root).as_posix() if relative != "." else ".",
                        relative,
                    )


PEAK_PROBE = """
import json, os, resource, sys, tempfile
from pathlib import Path
from bulkload_lib import scanner
from bulkload_lib.model import canonical_bytes


def peak():
    value = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    return value if os.uname().sysname == "Darwin" else value * 1024


def line(index, relative):
    return canonical_bytes({
        "destination_device": 1, "kind": "regular", "method": "copy",
        "mode": "0600", "relative_path": relative, "root_index": 0,
        "sha256": f"{index:064x}", "size": 4096, "source_device": 1}) + b"\\n"


def relative_at(index):
    return f"sessions/2026/08/23/rollout-2026-08-23T04-11-{index:012d}.jsonl"


def base_records(count, spilled):
    with tempfile.TemporaryDirectory() as temporary:
        before = peak()
        store = scanner._BaseRecordMap(
            ["provider-codex"], spill_dir=Path(temporary) if spilled else None
        )
        for index in range(count):
            store.append(0, relative_at(index), line(index, relative_at(index)))
        store.seal()
        view = store.by_label("provider-codex")
        for index in range(0, count, max(1, count // 500)):
            assert view.get(relative_at(index))["sha256"] == f"{index:064x}"
        after = peak()
        store.close()
    return after - before


def sort_stream(count, bounded):
    def entries():
        # Interleaved so no run is already in global order: a merge that
        # forgot to merge would show up as a mis-ordered yield below.
        for index in range(count):
            spun = (index * 2654435761) % count
            yield f"sessions/2026/08/23/rollout-2026-08-23T04-11-{spun:012d}.jsonl"

    before = peak()
    if bounded:
        # Deliberately the shipped default chunk: this gate has to measure the
        # configuration that runs on the ceremony host, not a tuned one.
        stream = scanner._bounded_sorted(entries())
    else:
        stream = iter(sorted(entries()))
    seen = 0
    previous = None
    for relative in stream:
        assert previous is None or previous <= relative
        previous = relative
        seen += 1
    after = peak()
    assert seen == count, (seen, count)
    return after - before, seen


WHICH, SUBJECT, VARIANT = sys.argv[1], sys.argv[2], sys.argv[3] == "1"
if WHICH == "base-records":
    count = int(SUBJECT)
    print(json.dumps({"bytes": base_records(count, VARIANT), "entries": count}))
else:
    grew, seen = sort_stream(int(SUBJECT), VARIANT)
    print(json.dumps({"bytes": grew, "entries": seen}))
"""


def probe(which: str, subject: str, variant: bool) -> dict[str, int]:
    """Measure peak RSS in a fresh interpreter.

    `ru_maxrss` is a process-wide high-water mark, so measuring a delta inside
    the test runner reports whatever the *suite* peaked at, not what this
    structure cost — and reports zero once some earlier test has peaked higher.
    A child process is the only honest reading.
    """
    root = Path(scanner.__file__).parent.parent
    result = subprocess.run(
        [sys.executable, "-c", PEAK_PROBE, which, subject, "1" if variant else "0"],
        capture_output=True,
        text=True,
        check=True,
        env={
            **os.environ,
            "PYTHONPATH": os.fspath(root),
            "PYTHONDONTWRITEBYTECODE": "1",
        },
    )
    return json.loads(result.stdout.strip().splitlines()[-1])


def lowest_probe(
    which: str, subject: str, variant: bool, runs: int = 3
) -> dict[str, int]:
    """The smallest of several readings.

    A child's peak can be inflated by anything the machine was doing while it
    ran, and the inflation is one-sided: noise only ever pushes the high-water
    mark up, never down. So the minimum across a few runs is the honest
    estimate of what the structure actually costs, and the only statistic that
    does not turn this gate into a load-dependent coin flip.
    """
    readings = [probe(which, subject, variant) for _ in range(runs)]
    return min(readings, key=lambda reading: reading["bytes"])


class ResidencyTests(unittest.TestCase):
    """The point of the whole wave: neither structure may grow with the corpus.

    Both budgets are per-entry and deliberately loose. They assert the
    asymptote — that the bounded form is O(1)-ish per entry where the old one
    was O(record) — not a particular allocator's constant.
    """

    BASE_RECORDS = 200_000
    NAMESPACE = 600_000

    def test_the_base_record_map_no_longer_holds_the_records(self) -> None:
        spilled = lowest_probe("base-records", str(self.BASE_RECORDS), True)
        resident = lowest_probe("base-records", str(self.BASE_RECORDS), False)
        # 2 MiB of allocator/interpreter noise on top of the asymptote.
        budget = 2 * 1024**2 + 128 * self.BASE_RECORDS
        self.assertLess(
            spilled["bytes"],
            budget,
            f"spilled peak grew {spilled['bytes']} B for {self.BASE_RECORDS} "
            f"entries (budget {budget} B); resident grew {resident['bytes']} B",
        )
        self.assertLess(spilled["bytes"] * 2, resident["bytes"])

    def test_the_namespace_sort_no_longer_holds_the_namespace(self) -> None:
        """Measured over the sort itself, not over a tree.

        A filesystem fixture large enough to out-signal the interpreter's own
        allocation churn would take longer to build than the whole suite takes
        to run; `_bounded_sorted` is the part that held the memory, and it can
        be fed a stream directly.
        """
        bounded = lowest_probe("namespace", str(self.NAMESPACE), True)
        listed = lowest_probe("namespace", str(self.NAMESPACE), False)
        self.assertEqual(bounded["entries"], listed["entries"])
        budget = 2 * 1024**2 + 64 * bounded["entries"]
        self.assertLess(
            bounded["bytes"],
            budget,
            f"bounded sort grew {bounded['bytes']} B for {bounded['entries']} "
            f"entries (budget {budget} B); the list grew {listed['bytes']} B",
        )
        # Measured 3.1x here. This probe understates the shipped win on
        # purpose: it sorts bare strings, while the list `_snapshot_namespace`
        # used to build held a (str, Path) tuple per entry — 206 B/entry
        # measured on a real tree against 4 B/entry for the bounded walk.
        self.assertLess(bounded["bytes"] * 2, listed["bytes"])


class ReductivePurgeTests(unittest.TestCase):
    """W2-4: the pure-LOC purge. Zero minutes, zero bytes, zero behaviour."""

    LIB = Path(scanner.__file__).parent

    def private_defs(self) -> dict[str, str]:
        modules = sorted(self.LIB.glob("*.py"))
        defined: dict[str, str] = {}
        used: set[str] = set()
        for module in modules:
            tree = ast.parse(module.read_text())
            for node in tree.body:
                if isinstance(
                    node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)
                ) and node.name.startswith("_"):
                    if not node.name.startswith("__"):
                        defined[node.name] = module.name
            for node in ast.walk(tree):
                if isinstance(node, ast.Name) and isinstance(node.ctx, ast.Load):
                    used.add(node.id)
                elif isinstance(node, ast.Attribute):
                    used.add(node.attr)
                elif isinstance(node, ast.ImportFrom):
                    used.update(alias.name for alias in node.names)
        return {name: where for name, where in defined.items() if name not in used}

    def test_no_private_helper_in_the_engine_is_unreferenced(self) -> None:
        """The guard that found `_worktree_at`, kept so it finds the next one.

        Deleting a function because it "looks unused" is how a fence goes
        missing. This is the check that has to be green *before* a deletion,
        and it is the check that would have gone red if `_worktree_at` had
        still had a caller.
        """
        self.assertEqual(self.private_defs(), {})

    def test_the_prune_rule_has_exactly_one_definition(self) -> None:
        source = "".join((self.LIB / "scanner.py").read_text().split())
        # `_is_excluded` and `_is_regenerate_namespace` are now reached only
        # through `_is_pruned` and through their own definitions.
        self.assertEqual(source.count("_is_excluded("), 2)
        self.assertEqual(source.count("_is_regenerate_namespace("), 2)
        # One definition plus nine call sites: the eight the census, copy,
        # charge and `_capture_provider` hold, and the doctor's walk, which
        # exists so the preflight names exactly the namespace capture reads.
        self.assertEqual(source.count("_is_pruned("), 10)

    def test_the_two_capture_provider_sites_keep_their_unguarded_form(self) -> None:
        """The asymmetry the collapse must not erase.

        Seven sites guard the prune with `provider is not None`; the two
        inside `_capture_provider` do not. Folding the guard into `_is_pruned`
        would change `_capture_provider` for a None provider, so the call
        sites keep the difference.
        """
        # Whitespace-normalised so a reformat cannot silently pass this.
        source = "".join((self.LIB / "scanner.py").read_text().split())
        self.assertEqual(
            source.count("ifproviderisnotNoneand_is_pruned(provider,relative,"), 7
        )
        self.assertEqual(source.count("if_is_pruned(provider,relative,"), 2)

    def test_git_environment_is_defined_once(self) -> None:
        for name in ("scanner.py", "executor.py"):
            with self.subTest(module=name):
                self.assertNotIn("def _git_environment", (self.LIB / name).read_text())
        self.assertEqual(
            model.git_environment(),
            {
                **{
                    key: value
                    for key, value in os.environ.items()
                    if not key.startswith("GIT_")
                    and key not in {"SSH_ASKPASS", "GIT_ASKPASS"}
                },
                "GIT_CONFIG_GLOBAL": os.devnull,
                "GIT_CONFIG_NOSYSTEM": "1",
                "GIT_TERMINAL_PROMPT": "0",
                "GIT_NO_REPLACE_OBJECTS": "1",
                "LC_ALL": "C",
            },
        )

    def test_the_purge_left_every_sealed_digest_alone(self) -> None:
        """A prune-rule collapse changes which entries a census visits, so it
        changes `generation_sha256` and `index_sha256` if it is wrong. The
        fixture carries an excluded path, a regenerate namespace, a live-sqlite
        primary with its sidecars, a symlink and a nested tree — every prune
        rule at once, including the sqlite-sidecar skip that is deliberately
        *not* folded into `_is_pruned`."""
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=True)
            capture = live_capture(fixture, "source-a")
            snapshot = capture["catalog"]["snapshot"]
            labels = {root["label"] for root in snapshot["roots"]}
            self.assertIn("git", labels)
            # Nothing pruned may appear in the sealed index.
            index = Path(snapshot["index_path"]).read_text().splitlines()
            self.assertEqual(len(index), snapshot["index_entries"])
            for line in index:
                record = json.loads(line)
                self.assertNotIn(".tmp", Path(record["relative_path"]).parts)
            # And the fence that reads it back still passes.
            scanner.validate_snapshot_custody(snapshot)
            scanner.validate_live_snapshot_generation(snapshot, passes=1)


class WitnessEpochTests(unittest.TestCase):
    """W2-5: observation-only, default off, and it can never fail a capture."""

    def capture(self, temporary: Path, **environment: str) -> tuple[dict, str]:
        fixture = CutoverFixture(temporary, sqlite_union=True)
        stream = io.StringIO()
        with mock.patch.dict(os.environ, environment):
            with contextlib.redirect_stderr(stream):
                capture = live_capture(fixture, "source-a")
        return capture, stream.getvalue()

    def test_it_is_off_unless_asked_for(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            _, noise = self.capture(Path(temporary))
        self.assertNotIn("bulkload-witness", noise)

    def test_it_reports_and_seals_identically_when_asked_for(self) -> None:
        with tempfile.TemporaryDirectory() as off:
            quiet, _ = self.capture(Path(off))
        with tempfile.TemporaryDirectory() as on:
            loud, noise = self.capture(Path(on), BULKLOAD_WITNESS_EPOCH="1")
        self.assertIn("bulkload-witness divergence=none", noise)
        # Observation only: it must not touch a single sealed byte.
        self.assertEqual(
            ChainedCaptureDifferentialTests.evidence(quiet).keys(),
            ChainedCaptureDifferentialTests.evidence(loud).keys(),
        )
        self.assertEqual(
            quiet["catalog"]["snapshot"]["index_entries"],
            loud["catalog"]["snapshot"]["index_entries"],
        )

    def test_a_divergent_live_tree_is_reported_and_never_raises(self) -> None:
        """The whole contract: it observes, it does not fence.

        A live write after the copy makes the real fence raise. The witness
        must print that and let the capture finish, or a measurement lap would
        become an outage.
        """
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=True)
            stream = io.StringIO()
            real = scanner.validate_live_snapshot_generation

            def diverge(snapshot, *, passes=2):
                raise BulkloadError("live generation differs after snapshot")

            with mock.patch.dict(os.environ, {"BULKLOAD_WITNESS_EPOCH": "1"}):
                with mock.patch.object(
                    scanner, "validate_live_snapshot_generation", diverge
                ):
                    with contextlib.redirect_stderr(stream):
                        capture = live_capture(fixture, "source-a")
            # The exception *class* is part of the line now: a real divergence
            # (`BulkloadError`) and a live-tree race (`FileNotFoundError`) are
            # different findings and a measurement lap has to tell them apart.
            self.assertIn(
                "bulkload-witness divergence=BulkloadError: live generation",
                stream.getvalue(),
            )
            self.assertIn("capture_sha256", capture)
            # The real fence is untouched and still refuses.
            real(capture["catalog"]["snapshot"], passes=1)

    def test_witness_survives_a_live_root_that_vanishes(self) -> None:
        """The refuter's counterexample, mocked nowhere.

        `except BulkloadError` was too narrow. The fence walks the *live* tree
        and `_tree_census` reaches `root.stat()` and `sha256_file()` unwrapped,
        so a root that an external tool removes between the seal and the
        witness raises a bare `FileNotFoundError`. That escaped into
        `_capture_live_snapshot`'s `except BaseException`, which removes the
        partial and re-raises: an off-by-default observation that changes no
        digest could destroy a capture that had already sealed.
        """
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=True)
            capture = live_capture(fixture, "source-a")
            snapshot = capture["catalog"]["snapshot"]
            live = Path(snapshot["roots"][0]["live"])
            shutil.rmtree(live)
            self.assertFalse(live.exists())

            # The fence itself still raises, and raises something that is not
            # a BulkloadError — otherwise this test proves nothing.
            with self.assertRaises(OSError):
                scanner.validate_live_snapshot_generation(snapshot, passes=1)

            stream = io.StringIO()
            with mock.patch.dict(os.environ, {"BULKLOAD_WITNESS_EPOCH": "1"}):
                with contextlib.redirect_stderr(stream):
                    scanner._witness_epoch(snapshot)
            self.assertIn(
                "bulkload-witness divergence=FileNotFoundError", stream.getvalue()
            )

    def test_only_the_operator_can_stop_a_capture_through_the_witness(self) -> None:
        """`Exception` is broad on purpose; `BaseException` is not swallowed."""
        for raised, escapes in (
            (BulkloadError("divergence"), False),
            (FileNotFoundError("live root vanished"), False),
            (MemoryError(), False),
            (KeyboardInterrupt(), True),
            (SystemExit(1), True),
        ):
            with self.subTest(raised=type(raised).__name__):

                def boom(snapshot, *, passes=2, error=raised):
                    raise error

                with mock.patch.dict(os.environ, {"BULKLOAD_WITNESS_EPOCH": "1"}):
                    with mock.patch.object(
                        scanner, "validate_live_snapshot_generation", boom
                    ):
                        with contextlib.redirect_stderr(io.StringIO()):
                            if escapes:
                                with self.assertRaises(type(raised)):
                                    scanner._witness_epoch({})
                            else:
                                scanner._witness_epoch({})

    def test_the_witness_is_the_fence_the_engine_already_owns(self) -> None:
        """Pins the KILL-1 correction.

        The design asked for a `content=True` `_tree_census` compared to the
        sealed generation. Those digests are over different tuples and can
        never be equal, so the witness has to be the live-generation fence —
        which also folds `sqlite_catalog` and so is not blind to sqlite.
        """
        source = "".join((Path(scanner.__file__)).read_text().split())
        body = source[source.index("def_witness_epoch(") :]
        body = body[: body.index("def_snapshot_path(")]
        self.assertIn("validate_live_snapshot_generation(snapshot,passes=1)", body)
        self.assertNotIn("_tree_census(", body)


if __name__ == "__main__":
    unittest.main()
