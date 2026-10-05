#!/usr/bin/env python3
"""Tests for q42_probes.py (OI-1003-Q42..Q45). Stdlib only.

Fast: the header rules and ref mapping are checked on synthetic bytes, and one
tiny corpus (a few dozen refs) is built under a private temporary directory to
check the corpus invariants, the inventory shape, the walk replay over every
ref storage, the spawn micro-measure and the child-attribution shim end to
end. Run with `python3 q42_probes.py selftest`.
"""

from __future__ import annotations

import contextlib
import io
import json
import shutil
import tempfile
import unittest
from pathlib import Path

import q42_probes as q

OID = "a" * 40
DIGEST = "b" * 64


class Mapping(unittest.TestCase):
    def test_canonical_carry_ref_maps_into_the_union_namespace(self) -> None:
        name = f"refs/carry/v1/neo/{DIGEST}/refs/heads/main"
        self.assertEqual(
            q.exported_name(name),
            f"refs/carry-export/union/v1/neo/{DIGEST}/refs/heads/main",
        )

    def test_non_canonical_refs_keep_their_name_under_the_export_prefix(self) -> None:
        for name in (
            "refs/heads/main",
            f"refs/carry/v1/neo/{DIGEST[:63]}/head",  # short digest
            f"refs/carry/v1/neo/{DIGEST}/",  # empty suffix
            f"refs/carry/v1/n.o/{DIGEST}/head",  # bad slug
        ):
            self.assertEqual(q.exported_name(name), "refs/carry-export/" + name, name)

    def test_line_arithmetic(self) -> None:
        self.assertEqual(q.ref_line_bytes(OID, "refs/heads/x"), 40 + 1 + 12 + 1)
        self.assertEqual(q.prereq_line_bytes(OID), 54)
        # A canonical carry ref's line is 135 B + slug + suffix (SHA-1).
        for slug in ("neo", "neo-20260922-c2a"):
            name = f"refs/carry/v1/{slug}/{DIGEST}/refs/heads/main"
            self.assertEqual(
                q.ref_line_bytes(OID, q.exported_name(name)),
                q.UNION_LINE_FIXED + len(slug) + len("refs/heads/main"),
            )
        self.assertEqual(q.UNION_LINE_FIXED, 135)
        self.assertEqual(q.refs_at_cap(100.0, 0), q.HEADER_CAP // 100)
        with self.assertRaises(ValueError):
            q.refs_at_cap(0.0, 0)


class Header(unittest.TestCase):
    def setUp(self) -> None:
        self.dir = Path(tempfile.mkdtemp(prefix="q42-selftest-"))

    def tearDown(self) -> None:
        shutil.rmtree(self.dir)

    def bundle(self, body: bytes) -> Path:
        path = self.dir / "b.bundle"
        path.write_bytes(body)
        return path

    def test_under_cap_header_reads_ok_with_terms(self) -> None:
        body = (
            b"# v2 git bundle\n"
            + f"-{OID} shared base\n".encode()
            + f"{OID} refs/carry-export/head\n".encode()
            + b"\nPACK"
        )
        got = q.read_header(self.bundle(body))
        self.assertEqual(got["agent_reader"], "ok")
        self.assertEqual(got["header_bytes"], len(body) - 4)
        self.assertEqual((got["prerequisite_lines"], got["ref_lines"]), (1, 1))
        self.assertEqual(got["prerequisite_bytes"], 54)

    def test_over_cap_header_is_the_typed_refusal_and_fully_counted(self) -> None:
        line = f"{OID} refs/carry-export/{'x' * 200}\n".encode()
        count = q.HEADER_CAP // len(line) + 10
        body = b"# v2 git bundle\n" + line * count + b"\n"
        got = q.read_header(self.bundle(body))
        self.assertEqual(got["agent_reader"], "GIT_INVENTORY_MALFORMED")
        self.assertEqual(got["agent_reader_cause"], "header_over_16MiB")
        self.assertEqual(got["header_bytes"], len(body))
        self.assertEqual(got["ref_lines"], count)
        self.assertTrue(got["over_cap"])

    def test_header_exactly_at_cap_is_accepted_and_one_byte_more_is_not(self) -> None:
        signature = b"# v2 git bundle\n"
        room = q.HEADER_CAP - len(signature) - 1  # less the blank line
        count, rest = divmod(room, 100)
        line = OID.encode() + b" " + b"r" * 58 + b"\n"  # 100 bytes
        last = OID.encode() + b" " + b"s" * (rest - 42) + b"\n"
        body = signature + line * count + last + b"\n"
        self.assertEqual(len(body), q.HEADER_CAP)
        got = q.read_header(self.bundle(body))
        self.assertEqual((got["agent_reader"], got["headroom_bytes"]), ("ok", 0))
        longer = signature + line * count + last[:-1] + b"t\n" + b"\n"
        got = q.read_header(self.bundle(longer))
        self.assertEqual(got["agent_reader_cause"], "header_over_16MiB")
        self.assertEqual(got["headroom_bytes"], -1)

    def test_a_line_over_one_mebibyte_refuses_at_the_line_bound(self) -> None:
        body = b"# v2 git bundle\n" + OID.encode() + b" " + b"r" * q.LINE_CAP + b"\n\n"
        got = q.read_header(self.bundle(body))
        self.assertEqual(got["agent_reader_cause"], "line_over_1MiB")

    def test_bad_signature_and_missing_blank_line(self) -> None:
        self.assertEqual(
            q.read_header(self.bundle(b"# v9 git bundle\n\n"))["agent_reader_cause"],
            "signature",
        )
        self.assertEqual(
            q.read_header(self.bundle(b"# v2 git bundle\nabc"))["agent_reader_cause"],
            "line_over_1MiB",
        )
        self.assertEqual(
            q.read_header(self.bundle(b"# v2 git bundle\n"))["agent_reader_cause"],
            "eof",
        )

    def test_header_terms_split_source_and_metadata_lines(self) -> None:
        source = "refs/carry-export/refs/heads/main"
        body = (
            b"# v2 git bundle\n"
            + f"-{OID} shared base\n".encode()
            + f"{OID} {source}\n".encode()
            + f"{OID} refs/carry-export/worktree\n".encode()
            + b"\nPACK"
        )
        got = q.header_terms(self.bundle(body), {source})
        self.assertEqual((got["source_ref_lines"], got["metadata_ref_lines"]), (1, 1))
        self.assertEqual(got["source_ref_bytes"], 40 + 1 + len(source) + 1)
        self.assertEqual(got["metadata_ref_names"], ["refs/carry-export/worktree"])
        self.assertEqual(got["header_bytes"], len(body) - 4)
        self.assertEqual(got["prerequisite_bytes"], 54)


class Classify(unittest.TestCase):
    def test_kinds(self) -> None:
        typed = q.classify(1, "", "bulkload-agent: refused: GIT_INVENTORY_MALFORMED\n")
        self.assertEqual(
            (typed["kind"], typed["codes"]), ("typed", ["GIT_INVENTORY_MALFORMED"])
        )
        receipt = 'item=x source="/s" outcome=refused reason=Some("GIT_INVENTORY_MALFORMED")\n'
        self.assertEqual(
            q.classify(0, receipt, "")["codes"], ["GIT_INVENTORY_MALFORMED"]
        )
        self.assertEqual(
            q.classify(1, "", "bulkload-agent: refused: IO (errno 2)\n")["kind"],
            "untyped",
        )
        self.assertEqual(
            q.classify(101, "", "thread 'main' panicked at x\n")["kind"], "panic"
        )
        self.assertEqual(q.classify(0, "", "")["kind"], "ok")

    def test_counters_line(self) -> None:
        got = q.parse_counters(
            "noise\ncounters verb=x elapsed_ns=12 priority=background\n"
        )
        self.assertEqual(got, {"verb": "x", "elapsed_ns": 12, "priority": "background"})


class Allowance(unittest.TestCase):
    def measured_case(self, modelled: int) -> tuple[dict, dict]:
        corpus = {
            "model": {"ref_line_bytes": modelled, "refs": 10},
            "tracked_files": 30,
            "dirs": 2,
        }
        decomposed = {
            "bundle_bytes": 16 + 108 + 1_000 + 100 + 1 + 32 + 60 + 400 + 300 + 9,
            "pack_bytes": 32 + 60 + 400 + 300 + 9,
            "pack_fixed_bytes": 32,
            "b_bytes_per_changed_tree": 200.0,
            "b_bytes_per_tree_entry": 25.0,
            "header": {
                "header_bytes": 16 + 108 + 1_000 + 100 + 1,
                "prerequisite_bytes": 108,
                "prerequisite_lines": 2,
                "ref_bytes": 1_100,
                "ref_lines": 11,
            },
            "header_terms": {
                "signature_bytes": 16,
                "capability_bytes": 0,
                "prerequisite_bytes": 108,
                "source_ref_lines": 10,
                "source_ref_bytes": 1_000,
                "metadata_ref_lines": 1,
                "metadata_ref_bytes": 100,
                "metadata_ref_names": ["refs/carry-export/worktree"],
                "blank_bytes": 1,
                "header_bytes": 16 + 108 + 1_000 + 100 + 1,
            },
            "by_role": {
                "staged:commit": {"objects": 1, "in_pack_bytes": 60, "raw_bytes": 50},
                "worktree:tree": {
                    "objects": 2,
                    "in_pack_bytes": 400,
                    "raw_bytes": 500,
                    "entries": 16,
                },
                "filesystem-v1:blob": {
                    "objects": 1,
                    "in_pack_bytes": 300,
                    "raw_bytes": 1_600,
                },
                "worktree:blob": {"objects": 1, "in_pack_bytes": 9, "raw_bytes": 9},
            },
        }
        return corpus, decomposed

    def test_measured_a_refs_and_the_check_is_only_an_identity(self) -> None:
        corpus, decomposed = self.measured_case(1_000)
        got = q.allowance_terms(corpus, decomposed)
        self.assertEqual(got["a_source"], "measured")
        self.assertEqual(
            (got["check"], got["pack_check"], got["header_check"]), (0, 0, 0)
        )
        self.assertEqual((got["a_model_delta"], got["metadata_ref_lines"]), (0, 1))
        self.assertEqual(got["terms"]["c"], 16 + 1 + 32 + 60 + 100)
        self.assertEqual(got["units"]["tree_entries"], 16)
        # A wrong model leaves `check` at 0; only `a_model_delta` sees it.
        corpus, decomposed = self.measured_case(1_040)
        got = q.allowance_terms(corpus, decomposed)
        self.assertEqual((got["check"], got["a_model_delta"]), (0, -40))
        self.assertEqual(got["terms"]["a_refs"], 1_000)

    def test_terms_partition_the_bundle_exactly(self) -> None:
        corpus = {
            "model": {"ref_line_bytes": 1_000, "refs": 10},
            "tracked_files": 30,
            "dirs": 2,
        }
        decomposed = {
            "bundle_bytes": 17 + 54 * 2 + 1_100 + 32 + 60 + 400 + 300 + 9,
            "pack_fixed_bytes": 32,
            "b_bytes_per_changed_tree": 200.0,
            "b_bytes_per_tree_entry": 25.0,
            "header": {
                "header_bytes": 17 + 54 * 2 + 1_100,
                "prerequisite_bytes": 108,
                "prerequisite_lines": 2,
                "ref_bytes": 1_100,
            },
            "by_role": {
                "staged:commit": {"objects": 1, "in_pack_bytes": 60, "raw_bytes": 50},
                "worktree:tree": {
                    "objects": 2,
                    "in_pack_bytes": 400,
                    "raw_bytes": 500,
                    "entries": 16,
                },
                "filesystem-v1:blob": {
                    "objects": 1,
                    "in_pack_bytes": 300,
                    "raw_bytes": 1_600,
                },
                "worktree:blob": {"objects": 1, "in_pack_bytes": 9, "raw_bytes": 9},
            },
        }
        decomposed["pack_bytes"] = 32 + 60 + 400 + 300 + 9
        decomposed["header"]["ref_lines"] = 11
        got = q.allowance_terms(corpus, decomposed)
        # Run A's records: no header_terms, so a*refs is the model.
        self.assertEqual(got["a_source"], "modelled")
        self.assertEqual(got["check"], 0)
        self.assertEqual(got["terms"]["c"], 17 + 32 + 60 + 100)
        self.assertEqual(
            (got["terms"]["a_refs"], got["terms"]["p_tips"], got["terms"]["s_seats"]),
            (1_000, 108, 300),
        )
        self.assertEqual(got["units"]["s_bytes_per_seat_raw"], 50.0)


class Fits(unittest.TestCase):
    def test_linear_fit_and_spread(self) -> None:
        fit = q.linear_fit([(0, 1.0), (1, 3.0), (2, 5.0)])
        self.assertAlmostEqual(fit["slope"], 2.0)
        self.assertAlmostEqual(fit["intercept"], 1.0)
        self.assertAlmostEqual(fit["r2"], 1.0)
        self.assertIsNone(q.linear_fit([(1, 1.0), (1, 2.0)]))
        self.assertEqual(
            q.spread([3.0, 1.0, 2.0]), {"n": 3, "median": 2.0, "min": 1.0, "max": 3.0}
        )

    def test_git_subcommand_skips_global_flags(self) -> None:
        line = "/usr/bin/git --no-optional-locks -c gc.auto=0 -C /x update-ref --stdin"
        self.assertEqual(q.git_subcommand(line), "update-ref")
        self.assertEqual(q.git_detail(line), "update-ref --stdin")

    def test_review_suite_holds_the_slope_family_fixed(self) -> None:
        plans = {plan["name"]: plan for plan in q.suite_plans("review")}
        self.assertIn("compact-119761", plans)
        for refs in q.SLOPE_REFS:
            plan = plans[f"slope-compact-{refs}"]
            split = q.split_refs(refs, 1_032, 157, cover=True)
            self.assertEqual(split["native_commits"], 1_032 - 3 * 157)
            self.assertEqual(plan["reps"], 3)
        self.assertEqual(plans["carry-window"]["spawn_pairs"], 400)
        self.assertEqual([p["name"] for p in q.suite_plans("v1")][0], "small-compact")


class Corpus(unittest.TestCase):
    def setUp(self) -> None:
        self.dir = Path(tempfile.mkdtemp(prefix="q42-selftest-"))

    def tearDown(self) -> None:
        shutil.rmtree(self.dir)

    def test_split_and_plan_are_exact(self) -> None:
        split = q.split_refs(119_761, 1_032, 157)
        self.assertEqual(
            split["live_native_refs"] + 157 * (split["native_refs_per_namespace"] + 4),
            119_761,
        )
        self.assertEqual(split["native_commits"], 561)
        rows = q.plan_refs(split, "carry", 42)
        self.assertEqual(len(rows), 119_761)
        self.assertEqual(len({name for name, _ in rows}), 119_761)
        with self.assertRaises(ValueError):
            q.split_refs(100, 10, 20)
        # `cover` leaves a count that splits unchanged and lifts one that
        # does not, holding tips and namespaces fixed.
        self.assertEqual(q.split_refs(119_761, 1_032, 157, cover=True), split)
        with self.assertRaises(ValueError):
            q.split_refs(2_000, 1_032, 157)
        small = q.split_refs(2_000, 1_032, 157, cover=True)
        self.assertGreaterEqual(small["live_native_refs"] - 1, 561)
        self.assertEqual(
            small["live_native_refs"] + 157 * (small["native_refs_per_namespace"] + 4),
            2_000,
        )
        self.assertEqual(len(q.plan_refs(small, "compact", 42)), 2_000)

    def test_tiny_corpora_hold_their_counts_and_replay_the_walk(self) -> None:
        for shape in ("carry", "compact"):
            with self.subTest(shape=shape):
                corpus = q.build_corpus(
                    self.dir / shape / "source",
                    shape=shape,
                    refs=60,
                    oids=16,
                    namespaces=2,
                    dirs=3,
                    files_per_dir=4,
                )
                model = corpus["model"]
                self.assertEqual((model["refs"], model["distinct_oids"]), (60, 16))
                if shape == "carry":
                    self.assertEqual(
                        model["union_mapped_refs"],
                        60 - corpus["split"]["live_native_refs"],
                    )
                shape_stats = model["shape"]
                self.assertEqual(shape_stats["union_formula_check"], 0)
                self.assertEqual(shape_stats["target_types"], {"commit": 60})
                if shape == "carry":
                    self.assertEqual(
                        shape_stats["slugs"],
                        {"neo": {"refs": model["union_mapped_refs"], "slug_bytes": 3}},
                    )
                walk = q.replay_walk(
                    Path(corpus["path"]),
                    self.dir / shape / "walk",
                    reps=1,
                    storages=q.REF_STORAGES,
                    spawn_pairs=2,
                )
                self.assertEqual(walk["excluded_tips"], 16)
                for storage, entry in walk["storages"].items():
                    aggressive = entry["objects-edge-aggressive"]
                    # The new worktree commit, its root tree, one directory
                    # tree and the changed blob; everything else is excluded.
                    self.assertEqual(aggressive["objects_listed"], 4, storage)
                    self.assertGreater(aggressive["pack_bytes"], 32)
                    self.assertGreaterEqual(entry["objects-edge"]["objects_listed"], 4)
                    self.assertEqual(entry["create_refs"]["exit"], 0)
                    self.assertEqual(entry["spawn_micro"]["pairs"], 2)
                # The agent's storage: one loose file per private ref.
                self.assertEqual(
                    walk["storages"]["loose"]["loose_ref_files"],
                    walk["storages"]["loose"]["private_refs"],
                )
                self.assertEqual(walk["storages"]["packed"]["loose_ref_files"], 0)
                self.assertEqual(walk["spawn_micro_source"]["pairs"], 2)
                self.assertFalse(any((self.dir / shape / "walk").glob("private-*")))

    def test_child_shim_attributes_each_git_child(self) -> None:
        shim = q.child_shim(self.dir / "shim")
        if shim is None:
            self.skipTest("needs git and bash")
        log = q.private_dir(self.dir / "children")
        env = q.shim_env(shim, log)
        for args in (
            ["version"],
            ["-c", "gc.auto=0", "-C", str(self.dir), "version"],
            ["-C", str(self.dir), "rev-parse", "--verify", "no-such-ref"],
        ):
            run = q.measured(["git", *args], env=env)
            self.assertEqual(run["exit"], 128 if "rev-parse" in args else 0)
        got = q.parse_child_log(log)
        self.assertEqual(got["children"], 3)
        self.assertEqual(got["by_subcommand"]["version"]["count"], 2)
        self.assertEqual(got["by_subcommand"]["rev-parse"]["failed"], 1)
        self.assertEqual(q.bash_seconds("1m2.500s"), 62.5)

    def test_header_output_records_the_script_revision(self) -> None:
        corpus = q.build_corpus(
            self.dir / "source",
            shape="carry",
            refs=60,
            oids=16,
            namespaces=2,
            dirs=3,
            files_per_dir=4,
        )
        printed = io.StringIO()
        with contextlib.redirect_stdout(printed):
            self.assertEqual(q.main(["header", corpus["path"]]), 0)
        got = json.loads(printed.getvalue())
        self.assertEqual(got["refs"], 60)
        self.assertEqual(got["script"]["sha256"], q.script_provenance()["sha256"])

    def test_header_target_search_steps_over_counts_too_small_to_split(self) -> None:
        # 20 namespaces over 200 oids need about 3,000 refs before the live
        # native set covers every native commit; the first bisection midpoint
        # lands below that and must move the search up, not down.
        target = 600_000
        refs = q.fit_refs_to_header(target, 200, 20, "carry", 42)
        rows = q.plan_refs(q.split_refs(refs, 200, 20), "carry", 42)
        self.assertLessEqual(q.planned_header_bytes(rows), target)
        # Within a few ref lines (about 184 B each) of the target.
        self.assertGreater(q.planned_header_bytes(rows), target - 2_000)

    def test_header_target_sizes_the_inventory(self) -> None:
        target = 40_000
        refs = q.fit_refs_to_header(target, 16, 2, "carry", 42)
        rows = q.plan_refs(q.split_refs(refs, 16, 2), "carry", 42)
        self.assertLessEqual(q.planned_header_bytes(rows), target)
        self.assertGreater(q.planned_header_bytes(rows), target - 1_000)


if __name__ == "__main__":
    unittest.main()
