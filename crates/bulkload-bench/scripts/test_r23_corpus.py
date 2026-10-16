#!/usr/bin/env python3
"""Tests for r23_corpus.py (OI-1002-Q28).

Run: python3 -m unittest crates/bulkload-bench/scripts/test_r23_corpus.py
The full generate-and-verify round trip runs only when `b3sum` is
available ($R23_B3SUM or PATH). The structural checks always run.
"""

from __future__ import annotations

import contextlib
import errno
import fcntl
import hashlib
import importlib.util
import io
import os
import shutil
import stat
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("r23_corpus", HERE / "r23_corpus.py")
corpus = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(corpus)
AB_SPEC = importlib.util.spec_from_file_location("r23_ab", HERE / "r23_ab.py")
ab = importlib.util.module_from_spec(AB_SPEC)
AB_SPEC.loader.exec_module(ab)


# FICLONE, _IOW(0x94, 9, int): the tests' own clone (OI-1003-Q165).
FICLONE = 0x40049409


def clone_file(source: Path, target: Path) -> None:
    with open(source, "rb") as inp, open(target, "xb") as out:
        fcntl.ioctl(out.fileno(), FICLONE, inp.fileno())


def not_run(case: unittest.TestCase, why: str) -> None:
    """Skip a reflink half, or fail it under `just p84-reflink` (R-N122)."""
    if os.environ.get("BULKLOAD_P84_REQUIRE_REFLINK"):
        case.fail(f"required by BULKLOAD_P84_REQUIRE_REFLINK: {why}")
    case.skipTest(why)


def cannot_clone(directory: Path) -> str | None:
    """None when `directory` can clone and FIEMAP maps it; else why not."""
    if sys.platform != "linux":
        return "not Linux"
    probe, twin = directory / "probe", directory / "probe-twin"
    probe.write_bytes(b"p" * 65536)
    try:
        if corpus.file_sharing(probe) is None:
            return f"no FIEMAP under {directory}"
        clone_file(probe, twin)
    except OSError as error:
        return f"{directory} cannot clone ({errno.errorcode.get(error.errno)})"
    finally:
        probe.unlink()
        if twin.exists():
            twin.unlink()
    return None


def tree(root: Path) -> None:
    (root / "d" / "e").mkdir(parents=True)
    (root / "a.bin").write_bytes(os.urandom(300_000))
    (root / "d" / "b.txt").write_bytes(b"b" * 5)
    (root / "d" / "e" / "empty").write_bytes(b"")
    os.symlink("a.bin", root / "link")
    os.symlink("d", root / "dlink")


def digest(path: str) -> str:
    sha = hashlib.sha256()
    for block in corpus.content(path):
        sha.update(block)
    return sha.hexdigest()


class StructureTests(unittest.TestCase):
    def test_manifest_matches_spec_and_harness_defaults(self) -> None:
        rows = [line.split("\t") for line in corpus.MANIFEST.read_text().splitlines()]
        self.assertEqual(len(rows), len(corpus.FILES))
        sizes = {path: size for path, _kind, size, _src in corpus.FILES}
        self.assertEqual({r[0]: int(r[1]) for r in rows}, sizes)
        self.assertEqual(
            [r[0] for r in rows], sorted((r[0] for r in rows), key=str.encode)
        )
        self.assertEqual(ab.RECORD_FILES, len(corpus.FILES))
        self.assertEqual(ab.RECORD_BYTES, corpus.total_bytes())
        self.assertIn(corpus.EXPECTED_IDENTITY[:12], ab.DEFAULT_CORPUS)

    def test_content_is_deterministic_and_sized(self) -> None:
        for path, _kind, size, _src in corpus.FILES:
            if size <= 1 << 20:
                first = b"".join(corpus.content(path))
                self.assertEqual(len(first), size, path)
                self.assertEqual(first, b"".join(corpus.content(path)), path)

    def test_planted_duplicates(self) -> None:
        self.assertEqual(digest("copies/blob-b-copy.bin"), digest("big/blob-b.bin"))
        prefix = next(corpus.content("medium/m04-prefix.bin"))
        source = next(corpus.content("big/blob-b.bin"))
        self.assertEqual(prefix, source)

    def test_verify_reports_a_missing_path(self) -> None:
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            self.assertEqual(corpus.main(["verify", "/nonexistent/r23-corpus"]), 1)
        self.assertIn("missing=1", out.getvalue())

    def test_mixed_compressibility(self) -> None:
        text = next(corpus.content("medium/m02.txt"))
        sparse = next(corpus.content("medium/m03.img"))
        self.assertTrue(set(text) <= set(corpus.TEXT_TABLE))
        self.assertEqual(sparse[: corpus.RUN], bytes(corpus.RUN))
        self.assertNotEqual(sparse[corpus.RUN : 2 * corpus.RUN], bytes(corpus.RUN))


class NoCloneTests(unittest.TestCase):
    """OI-1003-Q165 (b): rig copies are a read and a write, checked by FIEMAP."""

    def test_plain_copy_tree_reads_and_writes_and_keeps_the_shape(self) -> None:
        def refused(*_a: object, **_k: object) -> None:
            raise AssertionError("a cloning copy path was used (OI-1003-Q165)")

        with tempfile.TemporaryDirectory() as raw:
            source, target = Path(raw) / "s", Path(raw) / "t"
            source.mkdir()
            tree(source)
            (source / "a.bin").chmod(0o444)
            os.utime(source / "d" / "b.txt", ns=(1, 1_790_899_200_000_000_000))
            os.utime(source / "d", ns=(1, 1_790_899_200_000_000_000))
            cloning = [
                mock.patch.object(shutil, name, side_effect=refused)
                for name in ("copytree", "copy2", "copyfile", "copy", "copyfileobj")
            ]
            cloning += [
                mock.patch.object(os, name, side_effect=refused, create=True)
                for name in ("copy_file_range", "sendfile", "splice")
            ]
            with contextlib.ExitStack() as stack:
                for patcher in cloning:
                    stack.enter_context(patcher)
                self.assertEqual(corpus.plain_copy_tree(source, target), 3)
            self.assertEqual(
                (target / "a.bin").read_bytes(), (source / "a.bin").read_bytes()
            )
            self.assertEqual(stat.S_IMODE((target / "a.bin").stat().st_mode), 0o644)
            self.assertEqual(stat.S_IMODE((target / "d").stat().st_mode), 0o755)
            self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o755)
            self.assertEqual(os.readlink(target / "link"), "a.bin")
            self.assertEqual(os.readlink(target / "dlink"), "d")
            self.assertEqual((target / "d" / "e" / "empty").read_bytes(), b"")
            for rel in ("d/b.txt", "d"):
                self.assertEqual(
                    (target / rel).stat().st_mtime_ns, (source / rel).stat().st_mtime_ns
                )
            with self.assertRaises(FileExistsError):
                corpus.plain_copy_tree(source, target)

    def test_unsupported_and_off_linux_are_recorded_not_guessed(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            with mock.patch.object(corpus, "tree_sharing", return_value=None):
                with mock.patch.object(corpus.sys, "platform", "linux"):
                    record, problem = corpus.require_unshared(root, "x", required=True)
                    self.assertEqual(record, "unsupported")
                    self.assertIn("cannot be verified unshared", problem)
                    self.assertEqual(
                        corpus.require_unshared(root, "x", required=False),
                        ("unsupported", None),
                    )
                with mock.patch.object(corpus.sys, "platform", "darwin"):
                    self.assertEqual(
                        corpus.require_unshared(root, "x", required=True), ("n/a", None)
                    )
            shared = {"files": 2, "extents": 3, "shared": 2, "shared_files": ["a", "b"]}
            with mock.patch.object(corpus, "tree_sharing", return_value=shared):
                record, problem = corpus.require_unshared(
                    root, "the copy", required=False
                )
            self.assertEqual(record, "2")
            self.assertIn(
                "the copy shares 2 extent(s) with another file (a, b)", problem
            )

    def test_seal_and_copy_refuse_a_shared_tree(self) -> None:
        shared = {"files": 1, "extents": 1, "shared": 1, "shared_files": ["a.bin"]}
        unshared = {"files": 1, "extents": 1, "shared": 0, "shared_files": []}
        with tempfile.TemporaryDirectory() as raw:
            source = Path(raw) / "source"
            source.mkdir()
            (source / "a.bin").write_bytes(b"a" * 10)
            out = Path(raw) / "out"
            patches = [
                mock.patch.object(corpus, "verify", return_value=0),
                mock.patch.object(corpus, "manifest_text", return_value=("", [])),
                mock.patch.object(corpus, "identity_of", return_value="i" * 64),
            ]
            with contextlib.ExitStack() as stack:
                for patcher in patches:
                    stack.enter_context(patcher)
                printed = stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
                with mock.patch.object(corpus, "tree_sharing", return_value=shared):
                    self.assertEqual(corpus.main(["copy", str(source), str(out)]), 1)
                    self.assertIn(
                        "refused: the copy shares 1 extent(s)", printed.getvalue()
                    )
                    self.assertEqual(corpus.main(["seal", str(out)]), 1)
                    self.assertEqual(stat.S_IMODE(out.stat().st_mode), 0o755)
                fresh = Path(raw) / "fresh"
                with mock.patch.object(corpus, "tree_sharing", return_value=unshared):
                    self.assertEqual(corpus.main(["copy", str(source), str(fresh)]), 0)
                    self.assertEqual(corpus.main(["copy", str(source), str(fresh)]), 2)
                    self.assertEqual(corpus.main(["seal", str(fresh)]), 0)
            self.assertIn("method=read-write shared_extents=0", printed.getvalue())
            self.assertEqual((fresh / "corpus" / "a.bin").read_bytes(), b"a" * 10)
            self.assertEqual(
                stat.S_IMODE((fresh / "corpus" / "a.bin").stat().st_mode), 0o444
            )
            for dirpath, _dirs, _files in os.walk(fresh):
                Path(dirpath).chmod(0o755)

    @unittest.skipIf(os.geteuid() == 0, "root reads a mode-000 directory")
    def test_an_unreadable_directory_fails_the_walks(self) -> None:
        """Review, 2026-10-15: os.walk skips an unreadable directory unless told."""
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw) / "t"
            (root / "hidden").mkdir(parents=True)
            (root / "hidden" / "x").write_bytes(b"x")
            (root / "hidden").chmod(0)
            try:
                with mock.patch.object(
                    corpus, "file_sharing", return_value=(1, 1, "fiemap")
                ):
                    with self.assertRaises(PermissionError):
                        corpus.tree_sharing(root)
                    with self.assertRaises(PermissionError):
                        corpus.require_unshared(root, "t", True)
                with self.assertRaises(PermissionError):
                    corpus.plain_copy_tree(root, Path(raw) / "copy")
            finally:
                (root / "hidden").chmod(0o755)

    def test_fiemap_is_trusted_only_where_the_file_system_reports_sharing(
        self,
    ) -> None:
        """Review, 2026-10-15: CIFS answers FIEMAP but never flags sharing."""
        with tempfile.TemporaryDirectory() as raw:
            path = Path(raw) / "f"
            path.write_bytes(b"f" * 4096)
            with mock.patch.object(corpus, "fs_magic", return_value=0xFF534D42):
                self.assertIsNone(corpus.file_sharing(path))
                self.assertIsNone(corpus.tree_sharing(Path(raw)))
            if sys.platform != "linux":
                return
            for magic in (corpus.EXT_MAGIC, corpus.TMPFS_MAGIC):
                with mock.patch.object(corpus, "fs_magic", return_value=magic):
                    extents, shared, basis = corpus.file_sharing(path)
                    self.assertEqual((shared, basis), (0, "cannot-reflink"))
        if Path("/dev/shm").is_dir() and sys.platform == "linux":
            with tempfile.TemporaryDirectory(dir="/dev/shm") as raw:
                (Path(raw) / "a").write_bytes(b"a")
                census = corpus.tree_sharing(Path(raw))
                self.assertIsNotNone(census, "tmpfs cannot reflink: unshared")
                self.assertEqual((census["shared"], census["no_reflink_files"]), (0, 1))
                self.assertEqual(
                    corpus.require_unshared(Path(raw), "t", True), ("0", None)
                )

    def test_fiemap_sees_a_reflink_and_the_plain_copy_shares_nothing(self) -> None:
        """Real FIEMAP and real clones; needs TMPDIR on a reflink file system."""
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            why = cannot_clone(root)
            if why:
                not_run(self, f"{why}: set TMPDIR to a reflink file system")
            source = root / "sealed"
            source.mkdir()
            tree(source)
            os.sync()
            self.assertEqual(corpus.tree_sharing(source)["shared"], 0)
            # A cloning copy of the sealed tree (what cp and shutil may do).
            clone_file(source / "a.bin", root / "reflinked")
            sharing = corpus.tree_sharing(source)
            self.assertGreater(sharing["shared"], 0)
            self.assertEqual(sharing["shared_files"], ["a.bin"])
            record, problem = corpus.require_unshared(
                source, "the sealed corpus", False
            )
            self.assertIn("shares", problem)
            # copy_file_range, shutil's Linux fast copy from Python 3.14, clones.
            with open(source / "d" / "b.txt", "rb") as inp:
                fd = os.open(root / "cfr", os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
                try:
                    os.copy_file_range(inp.fileno(), fd, 1 << 20)
                finally:
                    os.close(fd)
            self.assertIn("d/b.txt", corpus.tree_sharing(source)["shared_files"])
            # The harness's read-and-write copy shares nothing, not even from a
            # shared source.
            copy = root / "copy"
            corpus.plain_copy_tree(source, copy)
            self.assertEqual(
                corpus.require_unshared(copy, "the copy", True), ("0", None)
            )
            self.assertEqual(
                (copy / "a.bin").read_bytes(), (source / "a.bin").read_bytes()
            )
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                self.assertEqual(corpus.main(["fiemap", str(source)]), 0)
            self.assertIn("shared_file=a.bin", out.getvalue())


@unittest.skipUnless(
    os.environ.get("R23_B3SUM") or shutil.which("b3sum"), "b3sum unavailable"
)
class RoundTripTests(unittest.TestCase):
    def test_generate_verifies_and_tamper_fails(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            out = Path(raw) / "copy"
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(corpus.main(["generate", str(out)]), 0)
                extra = out / "corpus" / "small" / "extra-dir"
                extra.mkdir(mode=0o755)
                self.assertEqual(corpus.main(["verify", str(out / "corpus")]), 1)
                extra.rmdir()
                self.assertEqual(corpus.main(["seal", str(out)]), 0)
                blob = out / "corpus" / "big" / "blob-b.bin"
                self.assertEqual(blob.stat().st_mode & 0o777, 0o444)
                self.assertEqual(out.stat().st_mode & 0o777, 0o555)
                self.assertEqual(corpus.main(["verify", str(out / "corpus")]), 0)
                for dirpath, dirs, _names in os.walk(out):
                    for name in dirs:
                        (Path(dirpath) / name).chmod(0o755)
                out.chmod(0o755)
                # OI-1003-Q165/Q166: a read-and-write copy of the sealed
                # corpus verifies to the same identity and seals.
                again = Path(raw) / "again"
                if corpus.tree_sharing(out / "corpus") is not None:
                    self.assertEqual(
                        corpus.main(["copy", str(out / "corpus"), str(again)]), 0
                    )
                    self.assertEqual(corpus.main(["seal", str(again)]), 0)
                    self.assertEqual(corpus.main(["verify", str(again / "corpus")]), 0)
                    for dirpath, dirs, _names in os.walk(again):
                        Path(dirpath).chmod(0o755)
                victim = out / "corpus" / "small" / "s01.bin"
                victim.chmod(0o644)
                victim.write_bytes(b"\x00")
                self.assertEqual(corpus.main(["verify", str(out / "corpus")]), 1)


if __name__ == "__main__":
    unittest.main()
