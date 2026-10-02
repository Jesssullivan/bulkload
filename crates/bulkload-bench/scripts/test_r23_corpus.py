#!/usr/bin/env python3
"""Tests for r23_corpus.py (OI-1002-Q28).

Run: python3 -m unittest crates/bulkload-bench/scripts/test_r23_corpus.py
The full generate-and-verify round trip runs only when `b3sum` is
available ($R23_B3SUM or PATH). The structural checks always run.
"""

from __future__ import annotations

import contextlib
import hashlib
import importlib.util
import io
import os
import shutil
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("r23_corpus", HERE / "r23_corpus.py")
corpus = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(corpus)
AB_SPEC = importlib.util.spec_from_file_location("r23_ab", HERE / "r23_ab.py")
ab = importlib.util.module_from_spec(AB_SPEC)
AB_SPEC.loader.exec_module(ab)


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

    def test_mixed_compressibility(self) -> None:
        text = next(corpus.content("medium/m02.txt"))
        sparse = next(corpus.content("medium/m03.img"))
        self.assertTrue(set(text) <= set(corpus.TEXT_TABLE))
        self.assertEqual(sparse[: corpus.RUN], bytes(corpus.RUN))
        self.assertNotEqual(sparse[corpus.RUN : 2 * corpus.RUN], bytes(corpus.RUN))


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
                victim = out / "corpus" / "small" / "s01.bin"
                victim.chmod(0o644)
                victim.write_bytes(b"\x00")
                self.assertEqual(corpus.main(["verify", str(out / "corpus")]), 1)


if __name__ == "__main__":
    unittest.main()
