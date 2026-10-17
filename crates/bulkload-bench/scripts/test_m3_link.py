#!/usr/bin/env python3
"""Stub tests for m3_link.py: census, generator and verdict logic, no network."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
import m3_link as m3  # noqa: E402

GIT_ENV = {
    **os.environ,
    "GIT_CONFIG_GLOBAL": os.devnull,
    "GIT_CONFIG_NOSYSTEM": "1",
    "GIT_AUTHOR_NAME": "t",
    "GIT_AUTHOR_EMAIL": "t@example.invalid",
    "GIT_COMMITTER_NAME": "t",
    "GIT_COMMITTER_EMAIL": "t@example.invalid",
}


def git(cwd: Path, *args: str) -> None:
    subprocess.run(
        ["git", *args], cwd=cwd, env=GIT_ENV, check=True, capture_output=True
    )


class CensusTests(unittest.TestCase):
    def test_state_census_counts_and_keeps_private_names_out(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "projects" / "secret-repo-name").mkdir(parents=True)
            (root / "projects" / "secret-repo-name" / "a.jsonl").write_bytes(b"x" * 600)
            (root / "private-dir").mkdir()
            (root / "private-dir" / "b.db").write_bytes(b"SQLite format 3\0")
            (root / "private-dir" / "b.db-wal").write_bytes(b"w")
            (root / "link").symlink_to("projects")
            out = m3.local("census", {"root": tmp, "label": "s", "kind": "state"})
            self.assertEqual(out["files"], 3)
            self.assertEqual(out["symlinks"], 1)
            self.assertEqual(out["classes"]["jsonl"]["files"], 1)
            self.assertEqual(out["classes"]["sqlite"]["files"], 1)
            self.assertEqual(out["classes"]["sqlite-sidecar"]["files"], 1)
            self.assertEqual(sum(out["size_hist"]["count"]), 3)
            top = m3.public_top(out["top"])
            self.assertEqual(set(top), {"projects", "(other)"})
            self.assertNotIn("secret-repo-name", json.dumps(top))

    def test_git_census_finds_checkouts_worktrees_and_objects(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            repo = root / "r1"
            repo.mkdir()
            git(repo, "init", "-q", "-b", "main")
            (repo / "f.txt").write_text("hello\n")
            git(repo, "add", ".")
            git(repo, "commit", "-qm", "c")
            git(repo, "worktree", "add", "-q", str(root / "r1.worktrees" / "w"))
            out = m3.local("census", {"root": tmp, "label": "g", "kind": "git"})
            self.assertEqual(out["repos"]["checkouts"], 1)
            self.assertEqual(out["repos"]["linked_worktrees"], 1)
            self.assertGreaterEqual(out["classes"]["git-loose"]["files"], 3)
            self.assertEqual(out["per_repo"]["worktree_admins"]["sum"], 1)
            self.assertNotIn("top", out)
            rows = m3.local("repos", {"root": tmp})["rows"]
            self.assertEqual(
                sorted((r[0], r[1], r[2]) for r in rows),
                [("r1", "main", None), ("r1.worktrees/w", "linked", "r1")],
            )


def profile() -> dict:
    return {
        "label": "area",
        "kind": "state",
        "files": 1000,
        "bytes": 1000 * 3000 + 10 * (8 << 20),
        "dirs": 50,
        "dir_depth_hist": [0, 10, 20, 20],
        "classes": {
            "jsonl": {
                "files": 1000,
                "bytes": 1000 * 3000,
                "size_hist": {"count": [0, 0, 1000], "bytes": [0, 0, 3_000_000]},
            },
            "compressed": {
                "files": 10,
                "bytes": 10 * (8 << 20),
                "size_hist": {
                    "count": [0, 0, 0, 10],
                    "bytes": [0, 0, 0, 10 * (8 << 20)],
                },
            },
        },
    }


class GeneratorTests(unittest.TestCase):
    def test_scaled_counts_keep_class_counts_and_bucket_means(self) -> None:
        rows = m3.scaled_counts(profile(), 0.1)
        by = {cls: (count, mean) for cls, count, mean in rows}
        self.assertEqual(by["jsonl"], (100, 3000))
        self.assertEqual(by["compressed"], (1, 8 << 20))

    def test_scaled_counts_do_not_round_in_a_bucket_too_large_for_the_scale(
        self,
    ) -> None:
        rows = m3.scaled_counts(profile(), 0.001)
        self.assertNotIn("compressed", {cls for cls, _c, _m in rows if _c})

    def test_generate_is_deterministic_and_verifies(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            prof = Path(tmp) / "p.json"
            prof.write_text(json.dumps(profile()))
            ids = []
            for name in ("a", "b"):
                dest = Path(tmp) / name
                args = m3.parser().parse_args(
                    ["generate", str(dest), "--profile", str(prof), "--bytes", "400000"]
                )
                self.assertEqual(m3.cmd_generate(args), 0)
                seal = json.loads((dest / "SEAL.json").read_text())
                ids.append(seal["identity"])
                verify = m3.parser().parse_args(["verify", str(dest)])
                self.assertEqual(m3.cmd_verify(verify), 0)
                for path in dest.rglob("*"):
                    if path.is_dir():
                        path.chmod(0o755)
            self.assertEqual(ids[0], ids[1])


class ArgvTests(unittest.TestCase):
    def test_rsync_pattern_escapes_only_wildcard_paths(self) -> None:
        self.assertEqual(m3.rsync_pattern("a/b.db"), "/a/b.db")
        self.assertEqual(m3.rsync_pattern("odd [x]*?.db"), "/odd \\[x\\]\\*\\?.db")


class VerdictTests(unittest.TestCase):
    def sample(self, arm: str, phase: str, ms: float, **kw: object) -> dict:
        native = {"max_rss_bytes": 1 << 20, "serve_max_rss_bytes": 1 << 20}
        return {
            "arm": arm,
            "phase": phase,
            "elapsed_ms": ms,
            **(native if arm == "B" else {}),
            **kw,
        }

    def test_gate_b_rule_needs_wins_over_plain_rclone_s3_and_rss(self) -> None:
        zero = {"bytes_received": 0, "source_bytes_read": 0}
        samples = [
            self.sample("B", "initial", 100.0, sequence=0),
            self.sample("P", "initial", 200.0, sequence=1),
            self.sample("R", "initial", 90.0, sequence=3),
            self.sample("B", "rerun", 5.0, sequence=0, **zero),
            self.sample("P", "rerun", 50.0, sequence=1),
            self.sample("B", "delta", 10.0, sequence=0, source_bytes_read=40),
            self.sample("P", "delta", 20.0, sequence=1),
        ]
        rep = {"samples": samples, "delta": {"changed_seat_bytes": 40}}
        v = m3.rep_verdict(rep)
        self.assertTrue(v["gate_b_rule"])
        self.assertFalse(v["initial_win_vs_R"])  # tuned rclone is informational
        self.assertEqual(v["s3_rerun_ratio"], 0.05)
        samples[5]["source_bytes_read"] = 41
        self.assertFalse(m3.rep_verdict(rep)["gate_b_rule"])
        samples[5]["source_bytes_read"] = 40
        samples[0]["serve_max_rss_bytes"] = 3 << 30
        self.assertFalse(m3.rep_verdict(rep)["gate_b_rule"])
        samples[0]["serve_max_rss_bytes"] = 1 << 20
        samples[3]["source_bytes_read"] = 1
        self.assertFalse(m3.rep_verdict(rep)["s3_rerun_zero"])


class BesideTests(unittest.TestCase):
    def test_b_against_p_uses_only_the_b_slots_beside_p(self) -> None:
        times = {0: 100.0, 1: 300.0, 2: 110.0, 3: 300.0, 4: 120.0, 5: 50.0, 6: 10.0}
        order = "BPBPBRB"
        samples = [
            {"arm": order[q], "phase": "initial", "elapsed_ms": t, "sequence": q}
            for q, t in times.items()
        ]
        self.assertEqual(m3.med_beside(samples, "P", "initial"), 110.0)
        self.assertEqual(m3.med_beside(samples, "R", "initial"), 65.0)


class DeltaTests(unittest.TestCase):
    def test_delta_changes_about_one_percent_of_files_in_place(self) -> None:
        rows = [
            [f"f{i:04d}", "f", 0o644, 10 if i % 2 else 1 << 20, "h"] for i in range(250)
        ]
        rows += [["d", "d", 0o755, 0, ""], ["db", "f", 0o644, 5000, "h"]]
        plan = m3.delta_plan_files(rows, {"db": "sqlite-magic"})
        self.assertEqual(len(plan), 3)  # ceil(1 % of 250 comparable files)
        self.assertTrue(all(rel != "db" for rel, _ in plan))
        sizes = {r[0]: r[3] for r in rows}
        for rel, length in plan:
            self.assertEqual(length, min(sizes[rel], 64 << 10))
        self.assertEqual(plan, m3.delta_plan_files(rows, {"db": "sqlite-magic"}))


class SubsetTests(unittest.TestCase):
    def test_work_root_inside_a_source_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            src = Path(tmp) / "src"
            src.mkdir()
            (src / "f").write_text("x")
            with self.assertRaisesRegex(RuntimeError, "overlaps"):
                m3.local(
                    "subset",
                    {
                        "sources": [["a", str(src)]],
                        "work": str(src / "work"),
                        "python": sys.executable,
                        "agent": "/bin/true",
                    },
                )
            self.assertFalse((src / "work").exists())

    def test_linked_worktrees_are_relinked_and_escapes_refused(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            gitdir = root / "real" / "git"
            repo = gitdir / "r1"
            repo.mkdir(parents=True)
            git(repo, "init", "-q", "-b", "main")
            (repo / "f.txt").write_text("hello\n")
            git(repo, "add", ".")
            git(repo, "commit", "-qm", "c")
            git(repo, "worktree", "add", "-q", str(gitdir / "r1.worktrees" / "w"))
            alias = root / "alias"
            alias.symlink_to(gitdir)  # a source named through a symlink
            out = m3.local(
                "subset",
                {
                    "sources": [["git", str(alias)]],
                    "work": str(root / "work"),
                    "python": sys.executable,
                    "agent": "/bin/true",
                },
            )
            self.assertEqual(out["skipped"]["worktree_links_rewritten"], 2)
            rows = m3.local("repos", {"root": out["corpus"] + "/git"})["rows"]
            self.assertEqual(
                sorted((r[0], r[1], r[2]) for r in rows),
                [("r1", "main", None), ("r1.worktrees/w", "linked", "r1")],
            )
            lone = root / "lone"
            lone.mkdir()
            (lone / "w").mkdir()
            (lone / "w" / ".git").write_text(f"gitdir: {repo}/.git/worktrees/w\n")
            with self.assertRaisesRegex(RuntimeError, "outside"):
                m3.local(
                    "subset",
                    {
                        "sources": [["lone", str(lone)]],
                        "work": str(root / "work2"),
                        "python": sys.executable,
                        "agent": "/bin/true",
                    },
                )


class IdentityTests(unittest.TestCase):
    def test_working_copy_manifest_reproduces_the_seal_identity(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            prof = Path(tmp) / "p.json"
            prof.write_text(json.dumps({**profile(), "symlinks": 300}))
            dest = Path(tmp) / "c"
            args = m3.parser().parse_args(
                ["generate", str(dest), "--profile", str(prof), "--bytes", "400000"]
            )
            self.assertEqual(m3.cmd_generate(args), 0)
            seal = json.loads((dest / "SEAL.json").read_text())
            rows = m3.local("manifest", {"root": str(dest / "corpus")})["rows"]
            self.assertEqual(m3.rows_identity(rows), seal["identity"])
            for path in dest.rglob("*"):
                if path.is_dir() and not path.is_symlink():
                    path.chmod(0o755)

    def test_duplicate_profile_labels_are_refused(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            prof = Path(tmp) / "p.json"
            prof.write_text(json.dumps(profile()))
            args = m3.parser().parse_args(
                ["generate", str(Path(tmp) / "c"), "--profile", str(prof)]
                + ["--profile", str(prof), "--bytes", "1000"]
            )
            self.assertEqual(m3.cmd_generate(args), 2)


if __name__ == "__main__":
    unittest.main()
