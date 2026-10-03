#!/usr/bin/env python3
"""Deterministic R23 corpus, v1 (OI-1002-Q28, R23, R-N13).

The corpus of record (`.bulkload-bench-real-ee15653/corpus`, bench identity
9b52260a..., 23 files / 242,605,606 bytes) was deleted on 2026-10-02 and
nothing could regenerate it. This script replaces it with a corpus that is
byte-identical on every run and every host.

Shape: 23 regular files, 239,819,837 bytes, in big/, copies/, medium/ and
small/. The sizes mix a few large files with medium and small ones, and the
compressibility is mixed:
  random  incompressible SHAKE-256 output;
  text    the same stream mapped through a letter-frequency table, about 4 bits/byte;
  sparse  alternating 64 KiB runs of zeros and random bytes.
It plants two duplicates, so dedup loss is measurable: copies/blob-b-copy.bin
is a whole-file copy of big/blob-b.bin, and medium/m04-prefix.bin starts
with blob-b's first 3 MiB.

Bytes come from SHAKE-256 in counter mode: the block for (seed, path,
index) is shake_256(f"{SEED}/{path}/{index}").digest(1 MiB). SHAKE-256 is a
fixed standard (FIPS 202), so unlike `random`, whose byte methods may change
between Python versions, the output never depends on Python, OS or host.
Files are mode 0644, directories 0755, and every mtime is fixed at
2026-10-02T00:00:00Z.

Identity: the bench's `corpus_identity` hashes postcard-encoded RowSchema
rows, which carry dev, ino, mtime_ns, ctime_ns and nlink next to the path,
kind, size, mode and content hash. It is a content-and-metadata hash: it
differs on every copy and every host, so it cannot be committed. The committed identity covers content and paths only. It is the
BLAKE3 of the manifest, one line per regular file sorted by path bytes:
`<path>\\t<size>\\t<blake3>\\n`. The manifest is committed beside this script
as r23_corpus.manifest.tsv. `verify` also checks that the tree has no
other files or directories, that files are 0644 (or 0444 when sealed) and
that directories are 0755 (or 0555). Those are
the remaining fields the bench's `same_payload` compares.

Usage:
  r23_corpus.py generate OUT   # OUT new; writes OUT/corpus, OUT/README.md, OUT/MANIFEST.tsv
  r23_corpus.py verify CORPUS  # exit 0 only if CORPUS matches the committed manifest
  r23_corpus.py manifest CORPUS
  r23_corpus.py seal OUT       # verify, then make OUT read-only (0444/0555)
BLAKE3 comes from `b3sum`: $R23_B3SUM, then PATH, then nixpkgs#b3sum
resolved through the repo flake's inputs.
"""

from __future__ import annotations

import hashlib
import os
import shutil
import subprocess
import sys
from collections.abc import Iterator
from pathlib import Path

SEED = "bulkload-r23-corpus-v1"
BLOCK = 1 << 20
RUN = 64 << 10
MTIME = 1_790_899_200  # 2026-10-02T00:00:00Z
EXPECTED_IDENTITY = "f4a7619f7b88f2e0e1eeadb5995c79a809eafa8b4a8cf3f82d1ff6f29b5b07c7"
HERE = Path(__file__).resolve().parent
MANIFEST = HERE / "r23_corpus.manifest.tsv"
REPO = HERE.parents[2]
MIB = 1 << 20
# Generated modes, and the read-only modes `seal` applies to an archive copy.
FILE_MODES = (0o644, 0o444)
DIR_MODES = (0o755, 0o555)

# (path, kind, size, source). Order is irrelevant; the manifest is sorted.
FILES: list[tuple[str, str, int, str | None]] = [
    ("big/blob-a.bin", "random", 88 * MIB + 12_345, None),
    ("big/disk.img", "sparse", 40 * MIB, None),
    ("big/archive.log", "text", 32 * MIB + 777, None),
    ("big/blob-b.bin", "random", 16 * MIB, None),
    ("copies/blob-b-copy.bin", "copy", 16 * MIB, "big/blob-b.bin"),
    ("medium/m01.bin", "random", 8 * MIB + 101, None),
    ("medium/m02.txt", "text", 6 * MIB + 33, None),
    ("medium/m03.img", "sparse", 5 * MIB, None),
    ("medium/m04-prefix.bin", "prefix", 4 * MIB + 3, "big/blob-b.bin"),
    ("medium/m05.txt", "text", 4_000_000, None),
    ("medium/m06.bin", "random", 3 * MIB + 7, None),
    ("medium/m07.img", "sparse", 2 * MIB, None),
    ("medium/m08.txt", "text", 2_000_003, None),
    ("small/s01.bin", "random", 1, None),
    ("small/s02.txt", "text", 4_096, None),
    ("small/s03.img", "sparse", 4_097, None),
    ("small/s04.bin", "random", 65_536, None),
    ("small/s05.txt", "text", 100_000, None),
    ("small/s06.img", "sparse", 262_144, None),
    ("small/s07.bin", "random", 333_333, None),
    ("small/s08.txt", "text", 524_288, None),
    ("small/s09.img", "sparse", 777_777, None),
    ("small/s10.bin", "random", 1_048_576, None),
]
PREFIX_BYTES = 3 * MIB

# Letter-frequency table: byte -> printable text, about 4 bits of entropy.
_ALPHABET = (
    b"eeeeeeeeeeeeettttttttttaaaaaaaaooooooooiiiiiiinnnnnnnsssssshhhhhhrrrrrr"
    b"ddddllllcccuuummwwffggyyppbbvk          \n\n,.0123456789"
)
TEXT_TABLE = bytes(_ALPHABET[i % len(_ALPHABET)] for i in range(256))


def stream(path: str, size: int) -> Iterator[bytes]:
    index = 0
    left = size
    while left > 0:
        block = hashlib.shake_256(f"{SEED}/{path}/{index}".encode()).digest(BLOCK)
        take = block[: min(left, BLOCK)]
        yield take
        left -= len(take)
        index += 1


def content(path: str) -> Iterator[bytes]:
    spec = {row[0]: row for row in FILES}[path]
    _, kind, size, source = spec
    if kind == "random":
        yield from stream(path, size)
    elif kind == "text":
        for block in stream(path, size):
            yield block.translate(TEXT_TABLE)
    elif kind == "sparse":
        offset = 0
        for block in stream(path, size):
            out = bytearray(block)
            for start in range(0, len(out), RUN):
                if ((offset + start) // RUN) % 2 == 0:
                    end = min(start + RUN, len(out))
                    out[start:end] = bytes(end - start)
            offset += len(block)
            yield bytes(out)
    elif kind == "copy" and source:
        yield from content(source)
    elif kind == "prefix" and source:
        left = PREFIX_BYTES
        for block in content(source):
            if left <= 0:
                break
            yield block[:left]
            left -= len(block[:left])
        yield from stream(path, size - PREFIX_BYTES)
    else:
        raise SystemExit(f"bad spec {spec}")


def b3sum() -> str:
    given = os.environ.get("R23_B3SUM") or shutil.which("b3sum")
    if given:
        return given
    out = subprocess.run(
        [
            "nix",
            "build",
            "--no-link",
            "--print-out-paths",
            "--inputs-from",
            str(REPO),
            "nixpkgs#b3sum",
        ],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.split()
    return str(Path(out[-1]) / "bin" / "b3sum")


def manifest_text(corpus: Path) -> tuple[str, list[str]]:
    """Return the manifest of `corpus` and any problems with its tree."""
    problems: list[str] = []
    files: list[str] = []
    expected_dirs = {
        parent.as_posix()
        for path, _k, _s, _src in FILES
        for parent in Path(path).parents
        if parent != Path(".")
    }
    for dirpath, dirs, names in os.walk(corpus):
        for name in dirs:
            path = Path(dirpath) / name
            rel = path.relative_to(corpus).as_posix()
            if path.is_symlink() or (path.lstat().st_mode & 0o777) not in DIR_MODES:
                problems.append(f"directory not 0755/0555: {rel}")
            elif rel not in expected_dirs:
                problems.append(f"extra directory: {rel}")
        for name in names:
            path = Path(dirpath) / name
            rel = path.relative_to(corpus).as_posix()
            if path.is_symlink() or not path.is_file():
                problems.append(f"not a regular file: {rel}")
            elif (path.lstat().st_mode & 0o777) not in FILE_MODES:
                problems.append(f"file not 0644/0444: {rel}")
            else:
                files.append(rel)
    files.sort(key=lambda rel: rel.encode())
    lines = []
    if files:
        out = subprocess.run(
            [b3sum(), "--no-names", *files],
            cwd=corpus,
            capture_output=True,
            text=True,
            check=True,
        ).stdout.split()
        for rel, digest in zip(files, out, strict=True):
            lines.append(f"{rel}\t{(corpus / rel).stat().st_size}\t{digest}\n")
    return "".join(lines), problems


def identity_of(text: str) -> str:
    return subprocess.run(
        [b3sum(), "--no-names"], input=text, capture_output=True, text=True, check=True
    ).stdout.strip()


def generate(out: Path) -> int:
    if out.exists() or not out.parent.is_dir():
        print(f"r23-corpus refused: OUT must be new under an existing parent: {out}")
        return 2
    corpus = out / "corpus"
    for path, _kind, size, _source in FILES:
        target = corpus / path
        target.parent.mkdir(parents=True, exist_ok=True)
        written = 0
        with target.open("wb") as handle:
            for block in content(path):
                handle.write(block)
                written += len(block)
        if written != size:
            raise SystemExit(f"{path}: wrote {written}, expected {size}")
        target.chmod(0o644)
    for dirpath, dirs, names in os.walk(corpus, topdown=False):
        for name in names:
            os.utime(Path(dirpath) / name, (MTIME, MTIME))
        for name in dirs:
            (Path(dirpath) / name).chmod(0o755)
            os.utime(Path(dirpath) / name, (MTIME, MTIME))
    text, problems = manifest_text(corpus)
    identity = identity_of(text)
    (out / "MANIFEST.tsv").write_text(text)
    (out / "README.md").write_text(readme(identity, text))
    print(
        f"r23-corpus generated={corpus} files={len(FILES)} bytes={total_bytes()} identity={identity}"
    )
    return verify(corpus) if not problems else 1


def total_bytes() -> int:
    return sum(size for _p, _k, size, _s in FILES)


def readme(identity: str, text: str) -> str:
    return (
        f"# PROTECTED: R23 gate (a) corpus v1 ({identity[:16]})\n\n"
        "Do not modify, move, prune or delete this directory. It is the sealed\n"
        "input for the R23 / gate (a) benchmark (#88, R23, R-N57). It replaces the\n"
        "corpus of record lost in the 2026-10-02 TinylandState cleanup\n"
        "(OI-1002-Q28, R-N13). Samples are only comparable on this identity.\n\n"
        f"- Content identity (BLAKE3 of MANIFEST.tsv): `{identity}`\n"
        f"- {len(FILES)} regular files, {total_bytes():,} bytes, under `corpus/`.\n"
        "- Generator: bulkload `crates/bulkload-bench/scripts/r23_corpus.py`\n"
        f"  (seed `{SEED}`). Regenerate anywhere with `r23_corpus.py generate`.\n"
        "- Check before use: `r23_corpus.py verify <this dir>/corpus`.\n"
        "- The bench's own `sealed_corpus_blake3` hashes content and stat metadata\n"
        "  (dev, ino, mtime, ctime), so it differs per copy. It is compared only\n"
        "  between reps of one session on one copy, never across copies.\n\n"
        "Manifest (path, size, blake3):\n\n```text\n" + text + "```\n"
    )


def seal(out: Path) -> int:
    """Make a generated copy (OUT from `generate`) read-only, after verifying it.

    Files become 0444 and directories 0555, including OUT itself, so the
    copy cannot be changed without an explicit chmod. Run only on copies this
    script generated.
    """
    if verify(out / "corpus") != 0:
        return 1
    for dirpath, dirs, names in os.walk(out, topdown=False):
        for name in names:
            (Path(dirpath) / name).chmod(0o444)
        for name in dirs:
            (Path(dirpath) / name).chmod(0o555)
    out.chmod(0o555)
    print(f"r23-corpus sealed={out} files=0444 dirs=0555")
    return verify(out / "corpus")


def verify(corpus: Path) -> int:
    if not corpus.is_dir():
        print(f"r23-corpus verify corpus={corpus} missing=1 ok=False")
        return 1
    text, problems = manifest_text(corpus)
    identity = identity_of(text)
    committed = MANIFEST.read_text() if MANIFEST.is_file() else None
    ok = not problems and text == committed and identity == EXPECTED_IDENTITY
    for problem in problems:
        print(f"r23-corpus problem: {problem}")
    print(
        f"r23-corpus verify corpus={corpus} identity={identity} expected={EXPECTED_IDENTITY} "
        f"manifest_match={text == committed} ok={ok}"
    )
    return 0 if ok else 1


def main(argv: list[str]) -> int:
    if len(argv) != 2 or argv[0] not in ("generate", "verify", "manifest", "seal"):
        print(__doc__)
        return 2
    target = Path(argv[1]).resolve()
    if argv[0] == "generate":
        return generate(target)
    if argv[0] == "verify":
        return verify(target)
    if argv[0] == "seal":
        return seal(target)
    text, problems = manifest_text(target)
    sys.stdout.write(text)
    print(f"identity={identity_of(text)} problems={problems}", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
