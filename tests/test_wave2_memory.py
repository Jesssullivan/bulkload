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
import json
import os
from pathlib import Path
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


def namespace(root, bounded):
    before = peak()
    if bounded:
        os.environ["BULKLOAD_NAMESPACE_CHUNK"] = "4096"
        seen = 0
        previous = None
        for relative, _ in scanner._snapshot_namespace(root):
            assert previous is None or previous < relative
            previous = relative
            seen += 1
    else:
        observed = [(".", root)]
        for current, directories, files in os.walk(
            root, topdown=True, followlinks=False
        ):
            directories[:] = sorted(directories)
            current_path = Path(current)
            observed.extend(
                ((current_path / n).relative_to(root).as_posix(), current_path / n)
                for n in directories
            )
            observed.extend(
                ((current_path / n).relative_to(root).as_posix(), current_path / n)
                for n in sorted(files)
            )
        listed = sorted(observed)
        seen = len(listed)
    after = peak()
    return after - before, seen


WHICH, SUBJECT, VARIANT = sys.argv[1], sys.argv[2], sys.argv[3] == "1"
if WHICH == "base-records":
    count = int(SUBJECT)
    print(json.dumps({"bytes": base_records(count, VARIANT), "entries": count}))
else:
    grew, seen = namespace(Path(SUBJECT), VARIANT)
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


class ResidencyTests(unittest.TestCase):
    """The point of the whole wave: neither structure may grow with the corpus.

    Both budgets are per-entry and deliberately loose. They assert the
    asymptote — that the bounded form is O(1)-ish per entry where the old one
    was O(record) — not a particular allocator's constant.
    """

    BASE_RECORDS = 200_000
    NAMESPACE = 40_000

    def test_the_base_record_map_no_longer_holds_the_records(self) -> None:
        spilled = probe("base-records", str(self.BASE_RECORDS), True)
        resident = probe("base-records", str(self.BASE_RECORDS), False)
        budget = 128 * self.BASE_RECORDS
        self.assertLess(
            spilled["bytes"],
            budget,
            f"spilled peak grew {spilled['bytes']} B for {self.BASE_RECORDS} "
            f"entries (budget {budget} B); resident grew {resident['bytes']} B",
        )
        self.assertLess(spilled["bytes"] * 2, resident["bytes"])

    def test_the_namespace_walk_no_longer_holds_the_namespace(self) -> None:
        # The tree is built here, not in the probe: creating 40 k files peaks
        # higher than walking them, and ru_maxrss would report that instead.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "root"
            per_directory = 400
            for group in range(self.NAMESPACE // per_directory):
                directory = root / f"sessions/2026/08/{group:05d}"
                directory.mkdir(parents=True)
                for item in range(per_directory):
                    name = f"rollout-{group:05d}-{item:06d}.jsonl"
                    (directory / name).write_bytes(b"x")
            bounded = probe("namespace", os.fspath(root), True)
            listed = probe("namespace", os.fspath(root), False)
        self.assertEqual(bounded["entries"], listed["entries"])
        budget = 64 * bounded["entries"]
        self.assertLess(
            bounded["bytes"],
            budget,
            f"bounded walk grew {bounded['bytes']} B for {bounded['entries']} "
            f"entries (budget {budget} B); the list grew {listed['bytes']} B",
        )
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
        self.assertEqual(source.count("_is_pruned("), 9)

    def test_the_two_capture_provider_sites_keep_their_unguarded_form(self) -> None:
        """The asymmetry the collapse must not erase.

        Six sites guard the prune with `provider is not None`; the two inside
        `_capture_provider` do not. Folding the guard into `_is_pruned` would
        change `_capture_provider` for a None provider, so the call sites keep
        the difference.
        """
        # Whitespace-normalised so a reformat cannot silently pass this.
        source = "".join((self.LIB / "scanner.py").read_text().split())
        self.assertEqual(
            source.count("ifproviderisnotNoneand_is_pruned(provider,relative,"), 6
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


if __name__ == "__main__":
    unittest.main()
