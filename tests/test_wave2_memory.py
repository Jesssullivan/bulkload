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

import json
import os
from pathlib import Path
import resource
import tempfile
import unittest
from unittest import mock

from bulkload_lib import scanner
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


def peak_rss_bytes() -> int:
    """Darwin reports ru_maxrss in bytes; Linux in kibibytes."""
    peak = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    return peak if os.uname().sysname == "Darwin" else peak * 1024


class BaseRecordResidencyTests(unittest.TestCase):
    """W2-1's whole point: the map must not grow with the corpus."""

    ENTRIES = 120_000

    def build(self, spill_dir: Path | None) -> tuple[int, scanner._BaseRecordMap]:
        before = peak_rss_bytes()
        store = scanner._BaseRecordMap(["provider-codex"], spill_dir=spill_dir)
        for index in range(self.ENTRIES):
            relative = f"sessions/2026/08/rollout-{index:012d}.jsonl"
            store.append(0, relative, index_line(0, relative))
        store.seal()
        return peak_rss_bytes() - before, store

    def test_the_spilled_map_costs_far_less_than_the_resident_one(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            spilled_cost, spilled = self.build(Path(temporary))
            try:
                view = spilled.by_label("provider-codex")
                # Not a no-op store: it still answers.
                self.assertEqual(
                    view.get("sessions/2026/08/rollout-000000099999.jsonl")["sha256"],
                    f"{0:064x}",
                )
                resident_cost, resident = self.build(None)
                resident.close()
            finally:
                spilled.close()
        # The resident map holds one parsed dict per entry (~690 B measured on
        # the real corpus); the spilled map holds the key blob and two offset
        # arrays only. The budget is deliberately loose — this asserts the
        # asymptote, not a particular allocator.
        budget = 128 * self.ENTRIES
        self.assertLess(
            spilled_cost,
            budget,
            f"spilled peak grew {spilled_cost} B for {self.ENTRIES} entries "
            f"(budget {budget} B); resident cost was {resident_cost} B",
        )
        self.assertLess(spilled_cost, resident_cost)


class NamespaceResidencyTests(unittest.TestCase):
    """W2-2's whole point: the walk must not grow with the namespace."""

    PER_DIR = 400
    DIRS = 60

    def tree(self, root: Path) -> int:
        entries = 1
        for index in range(self.DIRS):
            directory = root / f"sessions/2026/08/{index:05d}"
            directory.mkdir(parents=True)
            entries += 1 + len(directory.relative_to(root).parts) - 1
            for item in range(self.PER_DIR):
                (directory / f"rollout-{index:05d}-{item:06d}.jsonl").write_bytes(b"x")
                entries += 1
        return entries

    def test_the_bounded_walk_costs_far_less_than_the_list(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "root"
            root.mkdir()
            self.tree(root)
            with mock.patch.dict(os.environ, {"BULKLOAD_NAMESPACE_CHUNK": "2048"}):
                before = peak_rss_bytes()
                bounded = 0
                previous = None
                for relative, _ in scanner._snapshot_namespace(root):
                    self.assertTrue(previous is None or previous < relative)
                    previous = relative
                    bounded += 1
                bounded_cost = peak_rss_bytes() - before
            before = peak_rss_bytes()
            listed = reference_snapshot_namespace(root)
            listed_cost = peak_rss_bytes() - before
        self.assertEqual(bounded, len(listed))
        budget = 64 * bounded
        self.assertLess(
            bounded_cost,
            budget,
            f"bounded walk grew {bounded_cost} B for {bounded} entries "
            f"(budget {budget} B); the list cost {listed_cost} B",
        )


if __name__ == "__main__":
    unittest.main()
