"""Custody required/seen comparison is case-folded, and only that.

The sealed index spells every relative path as the SOURCE filesystem reported
it; the plan spells git worktree paths as git's pointer files recorded them.
On a case-insensitive source (APFS default) those two spellings are one
directory entry holding one set of sealed bytes, so `validate_snapshot_custody`
must not treat them as two custody keys.

These tests do not need a case-insensitive host to reproduce the defect.
`required_paths` is only ever a comparison set: the bytes actually reopened and
re-hashed come from the snapshot roots on disk, never from a required path. So
re-spelling the required set alone is exactly the shape the sting cutover hit,
and it is filesystem-independent.
"""

from __future__ import annotations

import json
from pathlib import Path
import tempfile
import unittest

from bulkload_lib import scanner
from bulkload_lib.model import BulkloadError
from bulkload_lib.scanner import _custody_identity

from test_bulkload import CutoverFixture
from test_wave0_perf import live_capture


class CustodyIdentityTests(unittest.TestCase):
    """The identity itself: one function, both spellings, one key."""

    def test_the_apfs_worktree_pair_folds_to_one_key(self) -> None:
        # The live pair from the sting cutover: 22,866 required paths spelled
        # GloriousFlywheel.worktrees/* had no index twin under
        # gloriousflywheel.worktrees/*, on a source where they are one entry.
        plan_spelling = "/srv/x/GloriousFlywheel.worktrees/main/BUILD.bazel"
        index_spelling = "/srv/x/gloriousflywheel.worktrees/main/BUILD.bazel"
        self.assertNotEqual(plan_spelling, index_spelling)
        self.assertEqual(
            _custody_identity(plan_spelling), _custody_identity(index_spelling)
        )

    def test_the_identity_accepts_both_str_and_path(self) -> None:
        raw = "/srv/x/GloriousFlywheel.worktrees/main"
        self.assertEqual(_custody_identity(raw), _custody_identity(Path(raw)))
        self.assertIsInstance(_custody_identity(Path(raw)), str)

    def test_the_identity_does_not_otherwise_normalise(self) -> None:
        # Folding case is the whole change. It must not collapse separators,
        # resolve `..`, or strip a trailing slash -- those would widen the
        # custody perimeter rather than restore the source's own identity.
        for raw in ("/a//b", "/a/../b", "/a/b/", "/a/b"):
            self.assertEqual(_custody_identity(raw), raw.casefold())
        self.assertNotEqual(_custody_identity("/a/b"), _custody_identity("/a/c"))


class CustodyCaseFoldTests(unittest.TestCase):
    """`validate_snapshot_custody` compares required/seen by folded identity."""

    @staticmethod
    def index_identities(snapshot: dict) -> list[tuple[Path, str]]:
        """Every `(root snapshot dir, relative)` the sealed index declares."""
        roots = [Path(item["snapshot"]) for item in snapshot["roots"]]
        entries: list[tuple[Path, str]] = []
        with Path(snapshot["index_path"]).open("rb") as stream:
            for line in stream:
                record = json.loads(line)
                entries.append(
                    (roots[record["root_index"]], record["relative_path"])
                )
        return entries

    @staticmethod
    def required(entries, *, respell=False) -> set[Path]:
        return {
            root if relative == "." else root / (
                relative.swapcase() if respell else relative
            )
            for root, relative in entries
        }

    def test_a_case_variant_required_set_still_satisfies_custody(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=True)
            snapshot = live_capture(fixture, "source-a")["catalog"]["snapshot"]
            entries = self.index_identities(snapshot)
            self.assertGreater(len(entries), 0)

            exact = self.required(entries)
            respelled = self.required(entries, respell=True)
            # Self-check: the fixture holds no two entries that already differ
            # only by case, so a fold cannot silently shrink either side.
            self.assertEqual(len(exact), len(entries))
            self.assertEqual(len(respelled), len(exact))
            self.assertNotEqual(respelled, exact)

            # Output-identical: same record map, same digests, from the same
            # sealed bytes -- only the comparison key was folded.
            self.assertEqual(
                scanner.validate_snapshot_custody(
                    snapshot, required_paths=respelled, collect_records=True
                ),
                scanner.validate_snapshot_custody(
                    snapshot, required_paths=exact, collect_records=True
                ),
            )

    def test_an_absent_required_path_is_still_refused(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            snapshot = live_capture(fixture, "source-b")["catalog"]["snapshot"]
            entries = self.index_identities(snapshot)
            root = entries[0][0]
            missing = self.required(entries) | {root / "never-sealed.txt"}
            with self.assertRaisesRegex(
                BulkloadError, "snapshot payload index count or digest differs"
            ):
                scanner.validate_snapshot_custody(
                    snapshot, required_paths=missing
                )

    def test_folding_does_not_admit_a_path_from_another_root(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            snapshot = live_capture(fixture, "source-b")["catalog"]["snapshot"]
            entries = self.index_identities(snapshot)
            root, relative = next(
                item for item in entries if item[1] not in {".", ""}
            )
            foreign = self.required(entries) | {
                root.parent / "not-a-declared-root" / relative
            }
            with self.assertRaisesRegex(
                BulkloadError, "snapshot payload index count or digest differs"
            ):
                scanner.validate_snapshot_custody(
                    snapshot, required_paths=foreign
                )


if __name__ == "__main__":
    unittest.main()
