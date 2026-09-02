"""The custody required/seen fold: narrow, measured at the source, and fenced.

The sealed index spells every relative path as the SOURCE filesystem reported
it; the plan spells git worktree paths as git's pointer files recorded them.
Where the source holds those two spellings as ONE directory entry they name one
set of sealed bytes, and keeping them as two custody keys makes the
required/seen equality unclosable on a case-sensitive destination. That is the
sting cutover's 22,866 `GloriousFlywheel.worktrees/*` refusals.

Widening a custody fence is a fail-open by default, so the widening here is
held down on three sides, and every side has a test below:

* WHAT folds. `str.casefold()` is FULL Unicode case folding and merges pairs
  that are two directory entries on every filesystem in existence -- 'ß'/'ss',
  'ﬁ'/'fi', 'ſ'/'s'. The fold is a per-codepoint lowercase instead.
* WHEN it folds. Only when the SOURCE host measured the equivalence and
  recorded it in the seal. Absent, malformed, or false measurement means the
  comparison stays byte-exact, exactly as it is on `main`.
* WHETHER the licence covers this corpus. Even under a measurement, a fold that
  merges two required paths, or two sealed index entries, refuses.

These tests do not need a case-insensitive host. `required_paths` is only ever a
comparison set -- the bytes reopened and re-hashed come from the snapshot roots
on disk, never from a required path -- so re-spelling the required set is
exactly the shape the cutover hit, and the measurement is forced by re-sealing
rather than inherited from whatever filesystem the suite happens to run on.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
import tempfile
import unicodedata
import unittest

from bulkload_lib import scanner
from bulkload_lib.model import BulkloadError, atomic_write_json, seal
from bulkload_lib.scanner import (
    SOURCE_FOLDING_FIELDS,
    SOURCE_FOLDING_KEY,
    _custody_identity,
    _fold_required,
    _record_custody_identity,
    _simple_lower,
    measure_source_path_folding,
    source_path_folding,
)

from test_bulkload import CutoverFixture
from test_wave0_perf import live_capture


# Pairs that FULL case folding merges and no filesystem does. Each is a
# separate directory entry everywhere, APFS included.
OVERSHOOT_PAIRS = (
    ("maße.txt", "masse.txt"),
    ("ſtem.txt", "stem.txt"),
    ("ﬁle.txt", "file.txt"),
)
# Genuine one-to-one case pairs, which a case-insensitive source really does
# hold as one entry.
CASE_PAIRS = (
    ("GloriousFlywheel.worktrees", "gloriousflywheel.worktrees"),
    ("FLEET-NOTE.TXT", "fleet-note.txt"),
    ("K", "k"),  # KELVIN SIGN
)
CAFE_NFC = unicodedata.normalize("NFC", "café.txt")
CAFE_NFD = unicodedata.normalize("NFD", "café.txt")
# Files planted in the capture fixture's git root so the end-to-end tests have
# real sealed bytes under a name whose spelling is contestable.
PLANTED = (OVERSHOOT_PAIRS[0][0], CAFE_NFC)


def host_merges_case(directory: Path) -> bool:
    """Does THIS filesystem hold two case spellings as one directory entry?"""
    probe = directory / "case-probe"
    probe.mkdir()
    (probe / "Aa.txt").write_text("one\n", encoding="utf-8")
    (probe / "aA.txt").write_text("two\n", encoding="utf-8")
    return len(list(probe.iterdir())) == 1


def tree(root: Path) -> set[str]:
    return {
        os.path.relpath(os.path.join(parent, name), root)
        for parent, directories, files in os.walk(root)
        for name in (*directories, *files)
    }


class SimpleLowerTests(unittest.TestCase):
    """The fold must not merge names that are two entries on every filesystem."""

    def test_full_case_folding_merges_what_this_fold_must_not(self) -> None:
        for left, right in OVERSHOOT_PAIRS:
            with self.subTest(pair=(left, right)):
                # The refuted implementation used `str.casefold()`, which
                # certifies a required 'masse.txt' against a sealed 'maße.txt'.
                self.assertEqual(left.casefold(), right.casefold())
                self.assertNotEqual(_simple_lower(left), _simple_lower(right))

    def test_a_genuine_case_pair_still_folds_to_one_key(self) -> None:
        for left, right in CASE_PAIRS:
            with self.subTest(pair=(left, right)):
                self.assertNotEqual(left, right)
                self.assertEqual(_simple_lower(left), _simple_lower(right))

    def test_the_fold_never_expands_a_codepoint(self) -> None:
        # A length-preserving map cannot merge two names of different length,
        # which is the whole family of full-folding overshoots.
        for point in range(0x11000):
            character = chr(point)
            self.assertEqual(len(_simple_lower(character)), 1)

    def test_an_expanding_lowercase_is_left_exactly_as_spelled(self) -> None:
        dotted_capital_i = "İ"
        self.assertEqual(len(dotted_capital_i.lower()), 2)
        self.assertEqual(_simple_lower(dotted_capital_i), dotted_capital_i)

    def test_the_ascii_fast_path_agrees_with_the_general_one(self) -> None:
        for value in ("Foo.TXT", "a/B/c", "", "123"):
            with self.subTest(value=value):
                self.assertTrue(value.isascii())
                self.assertEqual(
                    _simple_lower(value),
                    "".join(_simple_lower(character) for character in value),
                )


class CustodyIdentityTests(unittest.TestCase):
    """The identity is exact unless a caller passes a measured licence."""

    def test_the_identity_is_byte_exact_by_default(self) -> None:
        self.assertNotEqual(
            _custody_identity("/a/Foo.txt"), _custody_identity("/a/foo.txt")
        )
        self.assertNotEqual(
            _custody_identity("/a/" + CAFE_NFC), _custody_identity("/a/" + CAFE_NFD)
        )

    def test_each_fold_widens_only_its_own_axis(self) -> None:
        self.assertEqual(
            _custody_identity("/a/Foo.txt", fold_case=True),
            _custody_identity("/a/foo.txt", fold_case=True),
        )
        self.assertNotEqual(
            _custody_identity("/a/" + CAFE_NFC, fold_case=True),
            _custody_identity("/a/" + CAFE_NFD, fold_case=True),
        )
        self.assertEqual(
            _custody_identity("/a/" + CAFE_NFC, fold_normalization=True),
            _custody_identity("/a/" + CAFE_NFD, fold_normalization=True),
        )
        self.assertNotEqual(
            _custody_identity("/a/Foo.txt", fold_normalization=True),
            _custody_identity("/a/foo.txt", fold_normalization=True),
        )

    def test_the_identity_accepts_both_str_and_path(self) -> None:
        raw = "/srv/x/GloriousFlywheel.worktrees/main"
        self.assertEqual(_custody_identity(raw), _custody_identity(Path(raw)))
        self.assertIsInstance(_custody_identity(Path(raw)), str)

    def test_the_identity_never_resolves_a_path_itself(self) -> None:
        # It is a pure string map: no separator collapsing, no `..` resolution,
        # no trailing-slash strip. Production never asks it to -- `Path()` has
        # already collapsed `//` and a trailing slash before either call site,
        # and the required side additionally runs `os.path.abspath`, which
        # resolves `..` lexically. Stating that asymmetry is the point of this
        # test; the identity itself adds nothing to either side.
        for raw in ("/a//b", "/a/../b", "/a/b/", "/a/b"):
            with self.subTest(raw=raw):
                self.assertEqual(_custody_identity(raw), raw)
        self.assertEqual(os.fspath(Path("/a//b")), "/a/b")
        self.assertEqual(os.fspath(Path("/a/b/")), "/a/b")
        self.assertEqual(os.path.abspath("/a/../b"), "/b")


class SourceFoldingMeasurementTests(unittest.TestCase):
    """The licence comes from the source host, never from the reader's host."""

    def test_an_absent_or_malformed_measurement_folds_nothing(self) -> None:
        for recorded in (
            {},
            {SOURCE_FOLDING_KEY: None},
            {SOURCE_FOLDING_KEY: "true"},
            {SOURCE_FOLDING_KEY: {}},
            {SOURCE_FOLDING_KEY: {field: "yes" for field in SOURCE_FOLDING_FIELDS}},
            {SOURCE_FOLDING_KEY: {field: 1 for field in SOURCE_FOLDING_FIELDS}},
        ):
            with self.subTest(recorded=recorded):
                self.assertEqual(
                    source_path_folding(recorded),
                    {field: False for field in SOURCE_FOLDING_FIELDS},
                )

    def test_the_measurement_reports_this_hosts_own_behaviour(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "root"
            root.mkdir()
            (root / "Fleet-Note.txt").write_text("x\n", encoding="utf-8")
            (root / CAFE_NFC).write_text("y\n", encoding="utf-8")
            measured = measure_source_path_folding([root])
            self.assertIs(
                measured["case_insensitive"],
                (root / "fleet-note.txt").exists(),
            )
            self.assertIs(
                measured["normalization_insensitive"],
                (root / CAFE_NFD).exists(),
            )

    def test_the_roots_own_entry_answers_without_walking(self) -> None:
        # An empty or huge root still costs one stat: a directory is itself a
        # directory entry on the filesystem being measured. Without this an
        # empty `.claude` would silently disable the fold for the whole seal.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "Root"
            root.mkdir()
            self.assertEqual(list(root.iterdir()), [])
            self.assertIs(
                measure_source_path_folding([root])["case_insensitive"],
                (Path(temporary) / "rOOT").exists(),
            )

    def test_a_root_that_cannot_answer_claims_no_equivalence(self) -> None:
        # "123" has no case variant and no other normal form, so neither the
        # root's own directory entry nor anything under it can settle either
        # question. No evidence means no fold.
        with tempfile.TemporaryDirectory() as temporary:
            empty = Path(temporary) / "123"
            empty.mkdir()
            caseless = Path(temporary) / "456"
            caseless.mkdir()
            (caseless / "789").write_text("x\n", encoding="utf-8")
            for root in (empty, caseless, Path(temporary) / "absent"):
                with self.subTest(root=root.name):
                    self.assertEqual(
                        measure_source_path_folding([root]),
                        {field: False for field in SOURCE_FOLDING_FIELDS},
                    )

    def test_no_roots_at_all_claims_no_equivalence(self) -> None:
        self.assertEqual(
            measure_source_path_folding([]),
            {field: False for field in SOURCE_FOLDING_FIELDS},
        )

    def test_a_silent_root_inherits_the_filesystem_a_sibling_measured(self) -> None:
        # Case behaviour is a property of a mount, not of a directory. Pooling
        # by st_dev is what keeps one all-numeric or empty root from disabling
        # the fold for a whole capture.
        with tempfile.TemporaryDirectory() as temporary:
            decisive = Path(temporary) / "decisive"
            decisive.mkdir()
            (decisive / "Fleet-Note.txt").write_text("x\n", encoding="utf-8")
            silent = Path(temporary) / "123"
            silent.mkdir()
            self.assertEqual(decisive.stat().st_dev, silent.stat().st_dev)
            self.assertIs(
                measure_source_path_folding([decisive, silent])["case_insensitive"],
                measure_source_path_folding([decisive])["case_insensitive"],
            )

    def test_a_root_on_an_unmeasured_filesystem_claims_no_equivalence(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            decisive = Path(temporary) / "decisive"
            decisive.mkdir()
            (decisive / "Fleet-Note.txt").write_text("x\n", encoding="utf-8")
            self.assertFalse(
                measure_source_path_folding([decisive, Path(temporary) / "absent"])[
                    "case_insensitive"
                ]
            )

    def test_a_root_holding_both_spellings_measures_case_sensitive(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            # "123" so the root's own directory entry cannot answer and the
            # walk has to reach the pair inside.
            root = Path(temporary) / "123"
            root.mkdir()
            if host_merges_case(Path(temporary)):
                self.skipTest("this filesystem cannot hold both spellings")
            (root / "Fleet-Note.txt").write_text("x\n", encoding="utf-8")
            (root / "fleet-note.txt").write_text("y\n", encoding="utf-8")
            self.assertFalse(measure_source_path_folding([root])["case_insensitive"])
            # And one contradicting root turns the whole device off, even
            # beside a root whose own entry answered the other way.
            agreeable = Path(temporary) / "Agreeable"
            agreeable.mkdir()
            self.assertFalse(
                measure_source_path_folding([agreeable, root])["case_insensitive"]
            )

    def test_the_probe_writes_nothing_to_the_source(self) -> None:
        # It runs against the LIVE tree during a capture, so anything it left
        # behind would be a mutation the generation fence has to explain.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "root"
            (root / "nested").mkdir(parents=True)
            (root / "Fleet-Note.txt").write_text("x\n", encoding="utf-8")
            (root / "nested" / CAFE_NFC).write_text("y\n", encoding="utf-8")
            before = tree(root)
            measure_source_path_folding([root])
            self.assertEqual(tree(root), before)


class CardinalityFenceTests(unittest.TestCase):
    """A fold that merges two members of either side is not licensed."""

    def test_a_required_set_that_shrinks_under_the_fold_is_refused(self) -> None:
        folded = lambda path: _custody_identity(path, fold_case=True)  # noqa: E731
        with self.assertRaisesRegex(
            BulkloadError, "custody required paths collide under the source fold"
        ):
            _fold_required(["/a/Foo.txt", "/a/foo.txt"], folded)

    def test_a_required_set_that_keeps_its_cardinality_is_folded(self) -> None:
        folded = lambda path: _custody_identity(path, fold_case=True)  # noqa: E731
        self.assertEqual(
            _fold_required(["/a/Foo.txt", "/a/bar.txt"], folded),
            {"/a/foo.txt", "/a/bar.txt"},
        )

    def test_duplicate_spellings_of_one_path_are_not_a_collision(self) -> None:
        exact = _custody_identity
        self.assertEqual(
            _fold_required(["/a/x", "/a/./x", Path("/a/x")], exact), {"/a/x"}
        )

    def test_one_folded_key_binds_to_one_exact_spelling(self) -> None:
        seen: dict[str, str] = {}
        _record_custody_identity(seen, "/a/foo.txt", "/a/Foo.txt")
        _record_custody_identity(seen, "/a/foo.txt", "/a/Foo.txt")
        self.assertEqual(seen, {"/a/foo.txt": "/a/Foo.txt"})
        with self.assertRaisesRegex(
            BulkloadError, "sealed index paths collide under the source fold"
        ):
            _record_custody_identity(seen, "/a/foo.txt", "/a/foo.txt")


class CustodyFoldGateTests(unittest.TestCase):
    """`validate_snapshot_custody` end to end, with the seal's licence forced."""

    @staticmethod
    def capture(temporary: Path, name: str, *, sqlite_union: bool = True) -> dict:
        fixture = CutoverFixture(temporary, sqlite_union=sqlite_union)
        for planted in PLANTED:
            (fixture.source_home / "git" / planted).write_text(
                f"planted {planted}\n", encoding="utf-8"
            )
        return live_capture(fixture, name)["catalog"]["snapshot"]

    @staticmethod
    def reseal(snapshot: dict, **fields) -> dict:
        updated = seal(
            {
                **{
                    key: value
                    for key, value in snapshot.items()
                    if key != "seal_sha256"
                },
                **fields,
            },
            "seal_sha256",
        )
        atomic_write_json(Path(updated["seal_path"]), updated)
        return updated

    @classmethod
    def licence(cls, snapshot: dict, **measured: bool) -> dict:
        """Re-seal the snapshot with a chosen source measurement.

        Forcing it here rather than inheriting the suite host's filesystem is
        what makes every gate assertion below read the same on APFS and ext4.
        """
        folding = {field: False for field in SOURCE_FOLDING_FIELDS}
        folding.update(measured)
        return cls.reseal(snapshot, **{SOURCE_FOLDING_KEY: folding})

    @classmethod
    def unmeasured(cls, snapshot: dict) -> dict:
        """A seal with no measurement at all -- the shape `main` produces."""
        stripped = {
            key: value for key, value in snapshot.items() if key != SOURCE_FOLDING_KEY
        }
        return cls.reseal(stripped)

    @staticmethod
    def index_identities(snapshot: dict) -> list[tuple[Path, str]]:
        """Every `(root snapshot dir, relative)` the sealed index declares."""
        roots = [Path(item["snapshot"]) for item in snapshot["roots"]]
        entries: list[tuple[Path, str]] = []
        with Path(snapshot["index_path"]).open("rb") as stream:
            for line in stream:
                record = json.loads(line)
                entries.append((roots[record["root_index"]], record["relative_path"]))
        return entries

    @classmethod
    def required(cls, entries) -> set[Path]:
        return {
            root if relative == "." else root / relative for root, relative in entries
        }

    @classmethod
    def substitute(cls, entries, target: str, spelling: str) -> set[Path]:
        """The exact required set with ONE entry respelled.

        This is the direction the fence actually opens, and the direction the
        refuted revision had no test for: its only regression test respelled
        every entry at once, and its negative tests only reached for a foreign
        root -- the safe neighbour.
        """
        required = set()
        substituted = 0
        for root, relative in entries:
            if relative.rsplit("/", 1)[-1] == target:
                relative = relative[: len(relative) - len(target)] + spelling
                substituted += 1
            required.add(root if relative == "." else root / relative)
        if substituted != 1:
            raise AssertionError(f"{target!r} is not a unique fixture entry")
        return required

    def test_an_unmeasured_seal_refuses_a_respelled_required_path(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            snapshot = self.capture(Path(temporary), "source-a")
            entries = self.index_identities(snapshot)
            self.assertGreater(len(entries), 0)
            exact = self.required(entries)
            respelled = self.substitute(entries, "fleet-note.txt", "FLEET-NOTE.TXT")
            self.assertNotIn(next(iter(respelled - exact)), exact)

            # No measurement recorded at all -- a seal an older engine wrote:
            # byte-exact, exactly as on `main`.
            unmeasured = self.unmeasured(snapshot)
            self.assertNotIn(SOURCE_FOLDING_KEY, unmeasured)
            self.assertEqual(
                source_path_folding(unmeasured),
                {field: False for field in SOURCE_FOLDING_FIELDS},
            )
            scanner.validate_snapshot_custody(unmeasured, required_paths=exact)
            with self.assertRaisesRegex(
                BulkloadError, "snapshot payload index count or digest differs"
            ):
                scanner.validate_snapshot_custody(unmeasured, required_paths=respelled)

            # Measured and false: still byte-exact.
            sensitive = self.licence(snapshot, case_insensitive=False)
            scanner.validate_snapshot_custody(sensitive, required_paths=exact)
            with self.assertRaisesRegex(
                BulkloadError, "snapshot payload index count or digest differs"
            ):
                scanner.validate_snapshot_custody(sensitive, required_paths=respelled)

    def test_a_case_insensitive_seal_accepts_the_worktree_respelling(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            snapshot = self.capture(Path(temporary), "source-a")
            entries = self.index_identities(snapshot)
            respelled = self.substitute(entries, "fleet-note.txt", "FLEET-NOTE.TXT")
            insensitive = self.licence(snapshot, case_insensitive=True)
            # THE WIDENING, stated: under a measured case-insensitive source a
            # required path that was never sealed under that spelling is
            # certified on the strength of its case twin. That is the sting
            # defect's fix and the whole risk surface of this change.
            records = scanner.validate_snapshot_custody(
                insensitive, required_paths=respelled, collect_records=True
            )
            self.assertEqual(len(records.materialize()), snapshot["index_entries"])
            records.close()

    def test_a_case_insensitive_seal_still_refuses_a_full_folding_merge(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            snapshot = self.capture(Path(temporary), "source-a")
            entries = self.index_identities(snapshot)
            sharp_s, expansion = OVERSHOOT_PAIRS[0]
            merged = self.substitute(entries, sharp_s, expansion)
            insensitive = self.licence(snapshot, case_insensitive=True)
            # `str.casefold()` folds 'maße.txt' onto 'masse.txt'. No filesystem
            # anywhere holds those as one entry, so no case measurement can
            # licence certifying one for the other.
            self.assertEqual(sharp_s.casefold(), expansion.casefold())
            with self.assertRaisesRegex(
                BulkloadError, "snapshot payload index count or digest differs"
            ):
                scanner.validate_snapshot_custody(insensitive, required_paths=merged)

    def test_a_case_measurement_does_not_licence_a_normalization_respelling(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            snapshot = self.capture(Path(temporary), "source-a")
            entries = self.index_identities(snapshot)
            sealed = {relative for _, relative in entries}
            if CAFE_NFC not in sealed:
                self.skipTest("this filesystem re-spelled the planted NFC name")
            other_form = self.substitute(entries, CAFE_NFC, CAFE_NFD)
            insensitive = self.licence(snapshot, case_insensitive=True)
            with self.assertRaisesRegex(
                BulkloadError, "snapshot payload index count or digest differs"
            ):
                scanner.validate_snapshot_custody(
                    insensitive, required_paths=other_form
                )

    def test_a_normalization_insensitive_seal_accepts_the_other_form(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            snapshot = self.capture(Path(temporary), "source-a")
            entries = self.index_identities(snapshot)
            sealed = {relative for _, relative in entries}
            if CAFE_NFC not in sealed:
                self.skipTest("this filesystem re-spelled the planted NFC name")
            other_form = self.substitute(entries, CAFE_NFC, CAFE_NFD)
            # APFS is normalization-INSENSITIVE as well as case-insensitive:
            # git pointer files carry committed bytes (typically NFC) while
            # readdir may report NFD. Closing case and leaving this open would
            # reproduce the cutover failure on any non-ASCII path.
            licensed = self.licence(snapshot, normalization_insensitive=True)
            records = scanner.validate_snapshot_custody(
                licensed, required_paths=other_form, collect_records=True
            )
            records.close()

    def test_two_required_paths_that_fold_together_are_refused(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            snapshot = self.capture(Path(temporary), "source-a")
            entries = self.index_identities(snapshot)
            root = next(
                root for root, relative in entries if relative == "fleet-note.txt"
            )
            colliding = self.required(entries) | {root / "FLEET-NOTE.TXT"}
            insensitive = self.licence(snapshot, case_insensitive=True)
            with self.assertRaisesRegex(
                BulkloadError, "custody required paths collide under the source fold"
            ):
                scanner.validate_snapshot_custody(insensitive, required_paths=colliding)

    def test_the_fold_does_not_skip_payload_re_derivation(self) -> None:
        # The refuted revision billed a record-map equality as its
        # "no-digest-changed claim", but `collected` is appended before the
        # `required` branch, so the two maps it compared could not differ; the
        # only real assertion was "does not raise". This is the assertion that
        # actually witnesses the claim: tamper one sealed payload byte and the
        # folded run must still catch it.
        with tempfile.TemporaryDirectory() as temporary:
            snapshot = self.capture(Path(temporary), "source-a")
            entries = self.index_identities(snapshot)
            respelled = self.substitute(entries, "fleet-note.txt", "FLEET-NOTE.TXT")
            insensitive = self.licence(snapshot, case_insensitive=True)
            scanner.validate_snapshot_custody(insensitive, required_paths=respelled)

            payload = next(
                root / relative
                for root, relative in entries
                if relative.endswith("history.jsonl")
            )
            payload.chmod(0o600)
            payload.write_bytes(payload.read_bytes().replace(b"source", b"sourcE"))
            with self.assertRaisesRegex(
                BulkloadError, "snapshot payload differs from sealed index"
            ):
                scanner.validate_snapshot_custody(insensitive, required_paths=respelled)

    def test_the_record_map_is_independent_of_the_required_set(self) -> None:
        # Stated rather than asserted as evidence: `collected.append` runs for
        # every index entry regardless of `required_paths`, so a record-map
        # equality across two required sets witnesses nothing about the fold.
        with tempfile.TemporaryDirectory() as temporary:
            snapshot = self.capture(Path(temporary), "source-a", sqlite_union=False)
            entries = self.index_identities(snapshot)
            everything = scanner.validate_snapshot_custody(
                snapshot, collect_records=True
            )
            subset = scanner.validate_snapshot_custody(
                snapshot,
                required_paths=self.required(entries[:1]),
                collect_records=True,
            )
            self.assertEqual(everything, subset)
            everything.close()
            subset.close()

    def test_an_absent_required_path_is_still_refused(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            snapshot = self.capture(Path(temporary), "source-b", sqlite_union=False)
            entries = self.index_identities(snapshot)
            root = entries[0][0]
            missing = self.required(entries) | {root / "never-sealed.txt"}
            insensitive = self.licence(snapshot, case_insensitive=True)
            with self.assertRaisesRegex(
                BulkloadError, "snapshot payload index count or digest differs"
            ):
                scanner.validate_snapshot_custody(insensitive, required_paths=missing)

    def test_folding_does_not_admit_a_path_from_another_root(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            snapshot = self.capture(Path(temporary), "source-b", sqlite_union=False)
            entries = self.index_identities(snapshot)
            root, relative = next(item for item in entries if item[1] not in {".", ""})
            foreign = self.required(entries) | {
                root.parent / "not-a-declared-root" / relative
            }
            insensitive = self.licence(snapshot, case_insensitive=True)
            with self.assertRaisesRegex(
                BulkloadError, "snapshot payload index count or digest differs"
            ):
                scanner.validate_snapshot_custody(insensitive, required_paths=foreign)

    @staticmethod
    def seal_twin_entry(snapshot: dict, relative: str, twin: str) -> dict:
        """Add a second sealed index entry spelling one payload two ways.

        On a case-sensitive host the fixture seals both spellings by itself and
        this only re-emits what is already there. On a case-insensitive host
        the two spellings are one inode, so the second entry has to be written
        in -- which is the same corpus a case-SENSITIVE source would hand a
        destination whose seal wrongly claimed case-insensitivity. Either way
        the sealed index ends up holding a pair the fold would merge, which is
        the state the fence exists to refuse.
        """
        index_path = Path(snapshot["index_path"])
        records = [json.loads(line) for line in index_path.read_bytes().splitlines()]
        by_identity = {
            (record["root_index"], record["relative_path"]): record
            for record in records
        }
        original = next(
            record for record in records if record["relative_path"] == relative
        )
        by_identity.setdefault(
            (original["root_index"], twin), {**original, "relative_path": twin}
        )
        payload = b"".join(
            scanner.canonical_bytes(by_identity[key]) + b"\n"
            for key in sorted(by_identity)
        )
        index_path.chmod(0o600)
        index_path.write_bytes(payload)
        return {
            **snapshot,
            "index_entries": len(by_identity),
            "index_sha256": scanner.sha256_file(index_path),
        }

    def test_two_sealed_index_entries_that_fold_together_are_refused(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            (fixture.source_home / "git" / "Twin.txt").write_text(
                "twin\n", encoding="utf-8"
            )
            if not host_merges_case(fixture.root):
                (fixture.source_home / "git" / "twin.txt").write_text(
                    "twin\n", encoding="utf-8"
                )
            snapshot = live_capture(fixture, "source-a")["catalog"]["snapshot"]
            twinned = self.seal_twin_entry(snapshot, "Twin.txt", "twin.txt")
            entries = self.index_identities(twinned)
            self.assertLessEqual(
                {"Twin.txt", "twin.txt"}, {relative for _, relative in entries}
            )
            # Ask for only ONE of the two spellings, so the refusal can only
            # come from the index side of the fence.
            required = self.required(
                [item for item in entries if item[1] != "Twin.txt"]
            )
            insensitive = self.licence(twinned, case_insensitive=True)
            with self.assertRaisesRegex(
                BulkloadError, "sealed index paths collide under the source fold"
            ):
                scanner.validate_snapshot_custody(insensitive, required_paths=required)
            # The same corpus without the licence never reaches the fence: the
            # two spellings simply stay two custody keys, as they do on `main`.
            scanner.validate_snapshot_custody(
                self.licence(twinned, case_insensitive=False),
                required_paths=required,
            )


if __name__ == "__main__":
    unittest.main()
