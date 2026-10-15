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

No clone (S2, OI-1003-Q165 (b)): a rig copy of the corpus is an explicit
read and write (`plain_copy_tree`: `os.read`/`os.write`, never `cp`, whose
default is `--reflink=auto`, and never `shutil`, whose Linux fast copy is
`copy_file_range` from Python 3.14), checked with FIEMAP (`tree_sharing`)
to share no extent with any other file now. FIEMAP's flag is trusted on
xfs, btrfs, bcachefs and ocfs2; ext4 and tmpfs cannot reflink (0 by
construction); anything else is `unsupported`. `copy` makes such a copy of a
corpus and refuses one that shares; `seal` refuses to seal a copy that
shares; `fiemap` prints a tree's count. `verify` checks content only.

Usage:
  r23_corpus.py generate OUT   # OUT new; writes OUT/corpus, OUT/README.md, OUT/MANIFEST.tsv
  r23_corpus.py verify CORPUS  # exit 0 only if CORPUS matches the committed manifest
  r23_corpus.py manifest CORPUS
  r23_corpus.py seal OUT       # verify, check unshared, then make OUT read-only (0444/0555)
  r23_corpus.py copy CORPUS OUT  # OUT new; read/write copy of CORPUS to OUT/corpus, verified and unshared
  r23_corpus.py fiemap ROOT    # files, extents and shared extents under ROOT
BLAKE3 comes from `b3sum`: $R23_B3SUM, then PATH, then nixpkgs#b3sum
resolved through the repo flake's inputs.
"""

from __future__ import annotations

import errno
import hashlib
import os
import shutil
import struct
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


# FIEMAP (linux/fiemap.h): FS_IOC_FIEMAP is _IOWR('f', 11, struct fiemap),
# whose header is 32 bytes; each struct fiemap_extent is 56 bytes, its
# fe_flags at offset 40.
FS_IOC_FIEMAP = 0xC020660B
FIEMAP_BATCH = 64
FIEMAP_EXTENT_LAST = 0x1
FIEMAP_EXTENT_SHARED = 0x2000
COPY_CHUNK = 1 << 20
# fstatfs f_type magics (linux/magic.h). FIEMAP's SHARED flag is trusted only
# where the file system both reflinks and reports it; ext2/3/4 and tmpfs
# cannot reflink, so a file there is unshared by construction; any other file
# system (CIFS/SMB3 clones but never flags sharing, NFS, overlayfs, ZFS) is
# unsupported, never a silent 0 (review, 2026-10-15).
FIEMAP_TRUSTED = {
    0x58465342: "xfs",
    0x9123683E: "btrfs",
    0xCA451A4E: "bcachefs",
    0x7461636F: "ocfs2",
}
EXT_MAGIC = 0xEF53
TMPFS_MAGIC = 0x01021994
CANNOT_REFLINK = {EXT_MAGIC: "ext2/3/4", TMPFS_MAGIC: "tmpfs"}


def _raise(error: OSError) -> None:
    """os.walk's onerror: an unreadable directory fails the walk, never hides."""
    raise error


def fs_magic(fd: int) -> int | None:
    """fstatfs(fd).f_type (low 32 bits), or None where it cannot be read.

    Python has no fstatfs, so this calls libc's through ctypes; f_type is
    the first field of struct statfs, a C long on 64-bit Linux.
    """
    import ctypes

    try:
        libc = ctypes.CDLL(None, use_errno=True)
    except OSError:
        return None
    buffer = ctypes.create_string_buffer(256)
    if libc.fstatfs(fd, buffer) != 0:
        return None
    return ctypes.c_long.from_buffer(buffer).value & 0xFFFFFFFF


def file_sharing(path: Path) -> tuple[int, int, str] | None:
    """(extents, shared extents, basis) of the regular file at `path`.

    Read-only: the file is opened O_RDONLY without following a final
    symlink, never read, and no FIEMAP_FLAG_SYNC is passed, so no write-back
    starts. The basis is `fiemap` (xfs, btrfs, bcachefs, ocfs2: the flag is
    reported) or `cannot-reflink` (ext2/3/4, tmpfs: shared is 0 by
    construction). None where sharing cannot be known: off Linux, or any
    other file system. FIEMAP sees sharing that exists now: a clone whose
    target was later overwritten in full or freed is not seen.
    """
    if sys.platform != "linux":
        return None
    import fcntl

    flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC
    fd = os.open(path, flags)
    try:
        magic = fs_magic(fd)
        if magic in FIEMAP_TRUSTED:
            basis = "fiemap"
        elif magic in CANNOT_REFLINK:
            basis = "cannot-reflink"
        else:
            return None
        start = extents = shared = 0
        while True:
            request = bytearray(
                struct.pack("=QQIIII", start, 2**64 - 1, 0, 0, FIEMAP_BATCH, 0)
                + bytes(56 * FIEMAP_BATCH)
            )
            try:
                fcntl.ioctl(fd, FS_IOC_FIEMAP, request, True)
            except OSError as error:
                if error.errno in (errno.EOPNOTSUPP, errno.ENOTTY):
                    # tmpfs: no extent map, and no reflink either.
                    return (0, 0, basis) if basis == "cannot-reflink" else None
                raise
            mapped = struct.unpack_from("=I", request, 20)[0]
            last = mapped == 0
            following = start
            for index in range(min(mapped, FIEMAP_BATCH)):
                logical, _physical, length = struct.unpack_from(
                    "=QQQ", request, 32 + 56 * index
                )
                flags_word = struct.unpack_from("=I", request, 32 + 56 * index + 40)[0]
                extents += 1
                if flags_word & FIEMAP_EXTENT_SHARED:
                    shared += 1
                if flags_word & FIEMAP_EXTENT_LAST:
                    last = True
                following = logical + length
            if last or mapped < FIEMAP_BATCH or following <= start:
                return extents, shared, basis
            start = following
    finally:
        os.close(fd)


def tree_sharing(root: Path) -> dict[str, object] | None:
    """FIEMAP over every regular file under `root` (symlinks not followed).

    Returns files, extents, shared (extents), shared_files and
    no_reflink_files (files on a file system that cannot reflink), or None
    when sharing cannot be known there (see `file_sharing`). An unreadable
    directory raises; it is never skipped.
    """
    out: dict[str, object] = {
        "files": 0,
        "extents": 0,
        "shared": 0,
        "shared_files": [],
        "no_reflink_files": 0,
    }
    for dirpath, dirs, names in os.walk(root, onerror=_raise):
        dirs.sort()
        for name in sorted(names):
            path = Path(dirpath) / name
            if path.is_symlink() or not path.is_file():
                continue
            sharing = file_sharing(path)
            if sharing is None:
                return None
            out["files"] += 1
            out["extents"] += sharing[0]
            out["shared"] += sharing[1]
            if sharing[2] == "cannot-reflink":
                out["no_reflink_files"] += 1
            if sharing[1]:
                out["shared_files"].append(path.relative_to(root).as_posix())
    return out


def plain_copy_file(source: Path, target: Path, mode: int, fsync: bool) -> int:
    """Copy one regular file by `os.read` and `os.write`: never a clone.

    No `shutil`, `copy_file_range`, `sendfile` or `cp`: a read into user
    memory and a write of it cannot share blocks with the source. The target
    is new (O_EXCL), gets `mode` and the source's mtime. Returns the bytes
    copied.
    """
    flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC
    inp = os.open(source, flags)
    try:
        before = os.fstat(inp)
        out = os.open(
            target,
            os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC,
            0o600,
        )
        try:
            copied = 0
            while True:
                block = os.read(inp, COPY_CHUNK)
                if not block:
                    break
                view = memoryview(block)
                while view:
                    written = os.write(out, view)
                    view = view[written:]
                copied += len(block)
            if copied != before.st_size:
                raise OSError(
                    f"{source}: read {copied} bytes, stat said {before.st_size}"
                )
            os.fchmod(out, mode)
            if fsync:
                os.fsync(out)
        finally:
            os.close(out)
    finally:
        os.close(inp)
    os.utime(target, ns=(before.st_atime_ns, before.st_mtime_ns), follow_symlinks=False)
    return copied


def plain_copy_tree(
    source: Path,
    target: Path,
    file_mode: int = 0o644,
    dir_mode: int = 0o755,
    fsync: bool = False,
) -> int:
    """Copy the tree at `source` to the new directory `target` by read and write.

    Regular files go through `plain_copy_file`; symlinks are recreated as
    links; directories get `dir_mode` and keep their mtimes; any other node
    refuses. With `fsync`, every file and directory is synced. Returns the
    regular files copied.
    """
    os.mkdir(target, 0o700)
    files = 0
    walked = []
    for dirpath, dirs, names in os.walk(source, onerror=_raise):
        dirs.sort()
        rel = Path(dirpath).relative_to(source)
        here = target / rel
        for name in sorted(dirs):
            path = Path(dirpath) / name
            if path.is_symlink():
                os.symlink(os.readlink(path), here / name)
            else:
                os.mkdir(here / name, 0o700)
        dirs[:] = [name for name in dirs if not (Path(dirpath) / name).is_symlink()]
        for name in sorted(names):
            path = Path(dirpath) / name
            info = path.lstat()
            if path.is_symlink():
                os.symlink(os.readlink(path), here / name)
            elif path.is_file():
                plain_copy_file(path, here / name, file_mode, fsync)
                files += 1
            else:
                raise OSError(
                    f"not a regular file, directory or symlink: {path} ({info.st_mode:o})"
                )
        walked.append(rel)
    for rel in reversed(walked):
        info = (source / rel).lstat()
        (target / rel).chmod(dir_mode)
        os.utime(target / rel, ns=(info.st_atime_ns, info.st_mtime_ns))
        if fsync:
            fd = os.open(target / rel, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
            try:
                os.fsync(fd)
            finally:
                os.close(fd)
    return files


def require_unshared(root: Path, what: str, required: bool) -> tuple[str, str | None]:
    """OI-1003-Q165 (b): `what` at `root` shares no extent with any other file.

    Returns (record, problem). The record is the shared-extent count,
    `unsupported` (Linux, a file system whose sharing FIEMAP cannot show) or
    `n/a` (off Linux). A shared extent is always a problem; `unsupported` is
    one when `required`. "0" means no extent is shared now, not that no
    clone was ever made.
    """
    sharing = tree_sharing(root)
    if sharing is None:
        if sys.platform != "linux":
            return "n/a", None
        problem = (
            f"{what} cannot be verified unshared: no FIEMAP at {root} (OI-1003-Q165)"
            if required
            else None
        )
        return "unsupported", problem
    if sharing["shared"]:
        listed = ", ".join(sharing["shared_files"][:5])
        return str(sharing["shared"]), (
            f"{what} shares {sharing['shared']} extent(s) with another file"
            f" ({listed}): a clone, refused (S2, OI-1003-Q165)"
        )
    return "0", None


def copy(corpus: Path, out: Path) -> int:
    """A new, unshared copy of `corpus` at OUT/corpus, laid out as `generate` lays it out.

    The copy is `plain_copy_tree` with every file and directory synced,
    files 0644 and directories 0755, mtimes kept. It must verify to the
    committed identity and share no extent (OI-1003-Q165, OI-1003-Q166).
    `seal OUT` then makes it read-only.
    """
    if out.exists() or not out.parent.is_dir():
        print(f"r23-corpus refused: OUT must be new under an existing parent: {out}")
        return 2
    if verify(corpus) != 0:
        print(f"r23-corpus refused: the source corpus does not verify: {corpus}")
        return 1
    out.mkdir(0o755)
    files = plain_copy_tree(corpus, out / "corpus", fsync=True)
    text, problems = manifest_text(out / "corpus")
    identity = identity_of(text)
    (out / "MANIFEST.tsv").write_text(text)
    (out / "README.md").write_text(readme(identity, text))
    record, problem = require_unshared(out / "corpus", "the copy", required=True)
    print(
        f"r23-corpus copied={out / 'corpus'} from={corpus} files={files}"
        f" method=read-write shared_extents={record} identity={identity}"
    )
    if problem or problems:
        print(f"r23-corpus refused: {problem or problems}")
        return 1
    return verify(out / "corpus")


def seal(out: Path) -> int:
    """Make a generated copy (OUT from `generate`) read-only, after verifying it.

    Files become 0444 and directories 0555, including OUT itself, so the
    copy cannot be changed without an explicit chmod. Run only on copies this
    script generated.
    """
    if verify(out / "corpus") != 0:
        return 1
    record, problem = require_unshared(
        out / "corpus", "the copy to seal", required=False
    )
    if problem:
        print(f"r23-corpus refused: {problem}")
        return 1
    print(f"r23-corpus seal shared_extents={record}")
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
    if len(argv) == 3 and argv[0] == "copy":
        return copy(Path(argv[1]).resolve(), Path(argv[2]).resolve())
    if len(argv) != 2 or argv[0] not in (
        "generate",
        "verify",
        "manifest",
        "seal",
        "fiemap",
    ):
        print(__doc__)
        return 2
    target = Path(argv[1]).resolve()
    if argv[0] == "fiemap":
        sharing = tree_sharing(target)
        if sharing is None:
            print(f"r23-corpus fiemap root={target} supported=False")
            return 1
        print(
            f"r23-corpus fiemap root={target} files={sharing['files']}"
            f" extents={sharing['extents']} shared={sharing['shared']}"
            f" no_reflink_files={sharing['no_reflink_files']}"
        )
        for rel in sharing["shared_files"]:
            print(f"r23-corpus fiemap shared_file={rel}")
        return 0
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
