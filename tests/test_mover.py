"""Native content-addressed mover.

Every test here drives the *real* receiver: a separate `python3 -I -S
mover.py --receive` process on the other end of a pipe, exactly as the ssh
channel would run it. Nothing calls `receive()` in-process, because the two
properties most worth pinning — that the module has no intra-package imports
and that `-I` cannot see its own directory — are only true of a real
subprocess.
"""

from __future__ import annotations

import hashlib
import os
from pathlib import Path
import socket
import stat
import subprocess
import sys
import tempfile
import unittest

from bulkload_lib import mover
from bulkload_lib.mover import (
    MoveObject,
    MoverError,
    local_channel_factory,
    plan_objects,
    push,
)


MODULE = Path(mover.__file__)


def _tree(root: Path, layout: dict[str, bytes | str], modes=None) -> list[str]:
    """Write ``layout`` under ``root`` and return its '/'-relative paths."""
    relatives = []
    for name, payload in layout.items():
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        if isinstance(payload, str):
            path.symlink_to(payload)
        else:
            path.write_bytes(payload)
            os.chmod(path, (modes or {}).get(name, 0o644))
        relatives.append(path.relative_to("/").as_posix())
    return relatives


class MoverTransferTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="bulkload-mover-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / "source"
        self.destination = self.root / "destination"
        self.source.mkdir()
        self.destination.mkdir()

    def factory(self, root: Path | None = None):
        return local_channel_factory(
            sys.executable, str(MODULE), os.fspath(root or self.destination)
        )

    def move(self, relatives, *, streams: int = 2, **kwargs):
        return push(
            source_root=Path("/"),
            objects=plan_objects(Path("/"), relatives, **kwargs),
            channel_factory=self.factory(),
            streams=streams,
        )

    def landed(self, relative: str) -> Path:
        return self.destination / relative

    def test_moves_regular_files_symlinks_and_modes(self) -> None:
        relatives = _tree(
            self.source,
            {
                "a.txt": b"alpha",
                "nested/b.bin": os.urandom(4096),
                "nested/deep/c.txt": b"gamma",
                "link": "a.txt",
            },
            modes={"a.txt": 0o600, "nested/b.bin": 0o640},
        )
        summary = self.move(relatives)
        self.assertEqual(summary.objects_sent, 3)
        for relative in relatives:
            landed = self.landed(relative)
            source = Path("/") / relative
            self.assertTrue(landed.exists() or landed.is_symlink(), relative)
            if source.is_symlink():
                self.assertEqual(os.readlink(landed), os.readlink(source))
            else:
                self.assertEqual(landed.read_bytes(), source.read_bytes())
                self.assertEqual(
                    stat.S_IMODE(landed.lstat().st_mode),
                    stat.S_IMODE(source.lstat().st_mode),
                )

    def test_identical_content_crosses_the_wire_once(self) -> None:
        payload = os.urandom(64 * 1024)
        relatives = _tree(
            self.source,
            {f"copy-{index}/same.bin": payload for index in range(6)},
        )
        summary = self.move(relatives)
        self.assertEqual(summary.objects, 1)
        self.assertEqual(summary.paths, 6)
        self.assertEqual(summary.bytes_sent, len(payload))
        self.assertEqual(summary.bytes_deduplicated, len(payload) * 5)
        for relative in relatives:
            self.assertEqual(self.landed(relative).read_bytes(), payload)
        # The aliases are links, not copies: one object, one inode.
        inodes = {self.landed(relative).stat().st_ino for relative in relatives}
        self.assertEqual(len(inodes), 1)

    def test_rerun_transfers_nothing_and_is_byte_identical(self) -> None:
        relatives = _tree(
            self.source,
            {"a.txt": b"alpha", "b.txt": b"beta", "dup.txt": b"alpha"},
        )
        first = self.move(relatives)
        self.assertEqual(first.objects_sent, 2)
        second = self.move(relatives)
        self.assertEqual(second.objects_sent, 0)
        self.assertEqual(second.objects_skipped, 2)
        self.assertEqual(second.bytes_sent, 0)

    def test_resume_after_a_killed_run_completes_the_move(self) -> None:
        """The destination stage is the checkpoint; there is no journal."""
        relatives = _tree(
            self.source,
            {f"file-{index:03d}.bin": os.urandom(8192) for index in range(24)},
        )
        # Simulate an interrupted run by moving only a prefix, then the whole set.
        partial = self.move(relatives[:9], streams=1)
        self.assertEqual(partial.objects_sent, 9)
        full = self.move(relatives, streams=4)
        self.assertEqual(full.objects_skipped, 9)
        self.assertEqual(full.objects_sent, 15)
        for relative in relatives:
            self.assertEqual(
                self.landed(relative).read_bytes(), (Path("/") / relative).read_bytes()
            )

    def test_partial_object_from_a_killed_run_is_reused_by_name_not_stranded(
        self,
    ) -> None:
        """A stray temp file per interrupted object would fail destination custody.

        `validate_snapshot_custody` re-derives the quarantine namespace against
        the sealed index and fails closed on an extra entry (`scanner.py`,
        `_snapshot_namespace` comparison). The receiver's temp name is
        therefore derived from the path, not randomised, so a resume truncates
        the previous partial instead of adding a second one.
        """
        relatives = _tree(self.source, {"only.bin": os.urandom(2048)})
        landing = self.landed(relatives[0])
        landing.parent.mkdir(parents=True, exist_ok=True)
        stale = landing.parent / f".{landing.name}.mover-part"
        stale.write_bytes(b"garbage from a killed run")
        self.move(relatives)
        self.assertEqual(landing.read_bytes(), (Path("/") / relatives[0]).read_bytes())
        self.assertFalse(stale.exists())
        self.assertEqual(
            {item.name for item in landing.parent.iterdir()}, {landing.name}
        )

    def test_parallel_streams_move_the_same_bytes_as_one(self) -> None:
        relatives = _tree(
            self.source,
            {f"n/{index:04d}.bin": os.urandom(1024) for index in range(120)},
        )
        summary = self.move(relatives, streams=8)
        self.assertEqual(summary.streams, 8)
        self.assertEqual(summary.objects_sent, 120)
        for relative in relatives:
            self.assertEqual(
                self.landed(relative).read_bytes(), (Path("/") / relative).read_bytes()
            )


class MoverRefusalTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="bulkload-mover-refuse-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / "source"
        self.destination = self.root / "destination"
        self.source.mkdir()
        self.destination.mkdir()

    def factory(self):
        return local_channel_factory(
            sys.executable, str(MODULE), os.fspath(self.destination)
        )

    def test_a_lying_digest_is_refused_and_nothing_is_published(self) -> None:
        payload = b"the bytes that are actually on disk"
        path = self.source / "a.bin"
        path.write_bytes(payload)
        relative = path.relative_to("/").as_posix()
        item = MoveObject(
            kind="regular",
            mode="0644",
            paths=(relative,),
            digest=hashlib.sha256(b"a digest for different bytes").hexdigest(),
            size=len(payload),
        )
        with self.assertRaises(MoverError):
            push(
                source_root=Path("/"),
                objects=[item],
                channel_factory=self.factory(),
                streams=1,
            )
        self.assertFalse((self.destination / relative).exists())

    def test_a_path_escaping_the_root_is_refused(self) -> None:
        for escape in ("../outside", "/absolute", "a/../../b", ""):
            with self.subTest(escape=escape), self.assertRaises(MoverError):
                push(
                    source_root=Path("/"),
                    objects=[
                        MoveObject(
                            kind="regular",
                            mode="0644",
                            paths=(escape,),
                            digest="0" * 64,
                            size=0,
                        )
                    ],
                    channel_factory=self.factory(),
                    streams=1,
                )
        self.assertEqual(list(self.destination.iterdir()), [])

    def test_a_receiver_with_different_source_is_refused_before_any_object(
        self,
    ) -> None:
        """The remote copy must be the bytes the sender shipped.

        `runtime_source_digest` (`model.py:88-101`) pins the engine's own
        closure into every capture and plan, but the receiver is a remote
        process and there is no remote hashing tool to lean on. The hello
        frame is the mover's answer.
        """
        path = self.source / "a.bin"
        path.write_bytes(b"payload")
        relative = path.relative_to("/").as_posix()
        with self.assertRaises(MoverError) as caught:
            push(
                source_root=Path("/"),
                objects=plan_objects(Path("/"), [relative]),
                channel_factory=self.factory(),
                streams=1,
                expected_receiver_sha256="f" * 64,
            )
        self.assertIn("source differs", str(caught.exception))
        self.assertFalse((self.destination / relative).exists())

    def test_the_matching_receiver_digest_is_accepted(self) -> None:
        path = self.source / "a.bin"
        path.write_bytes(b"payload")
        relative = path.relative_to("/").as_posix()
        summary = push(
            source_root=Path("/"),
            objects=plan_objects(Path("/"), [relative]),
            channel_factory=self.factory(),
            streams=1,
            expected_receiver_sha256=hashlib.sha256(MODULE.read_bytes()).hexdigest(),
        )
        self.assertEqual(summary.objects_sent, 1)

    def test_a_source_that_changes_under_the_stream_is_refused(self) -> None:
        path = self.source / "a.bin"
        path.write_bytes(b"a" * 4096)
        relative = path.relative_to("/").as_posix()
        planned = list(plan_objects(Path("/"), [relative]))
        path.write_bytes(b"b" * 4096)
        with self.assertRaises(MoverError) as caught:
            push(
                source_root=Path("/"),
                objects=planned,
                channel_factory=self.factory(),
                streams=1,
            )
        # Both ends catch this independently; the sender's re-hash of what it
        # actually read gets there first, which is the useful message because
        # it names the source path.
        self.assertIn("source bytes changed during transfer", str(caught.exception))
        self.assertFalse((self.destination / relative).exists())

    def test_a_special_entry_stops_the_plan(self) -> None:
        fifo = self.source / "pipe"
        os.mkfifo(fifo)
        with self.assertRaises(MoverError) as caught:
            list(plan_objects(Path("/"), [fifo.relative_to("/").as_posix()]))
        self.assertIn("special entry", str(caught.exception))

    def test_a_directory_without_owner_write_stops_the_plan(self) -> None:
        """Documented gap against `rsync -a`, refused before a byte moves."""
        directory = self.source / "locked"
        directory.mkdir(mode=0o500)
        self.addCleanup(os.chmod, directory, 0o700)
        with self.assertRaises(MoverError) as caught:
            list(plan_objects(Path("/"), [directory.relative_to("/").as_posix()]))
        self.assertIn("not owner-writable", str(caught.exception))

    def test_a_sealed_digest_whose_size_disagrees_stops_the_plan(self) -> None:
        path = self.source / "a.bin"
        path.write_bytes(b"12345")
        relative = path.relative_to("/").as_posix()
        with self.assertRaises(MoverError) as caught:
            list(
                plan_objects(
                    Path("/"), [relative], digests=lambda _relative: ("0" * 64, 9999)
                )
            )
        self.assertIn("sealed size", str(caught.exception))

    def test_a_sealed_digest_is_taken_without_reading_the_file(self) -> None:
        payload = b"sealed"
        path = self.source / "a.bin"
        path.write_bytes(payload)
        relative = path.relative_to("/").as_posix()
        sealed = hashlib.sha256(payload).hexdigest()
        calls: list[str] = []

        def digests(item: str) -> tuple[str, int]:
            calls.append(item)
            return sealed, len(payload)

        planned = list(plan_objects(Path("/"), [relative], digests=digests))
        self.assertEqual(calls, [relative])
        self.assertEqual(planned[0].digest, sealed)

    def test_stream_count_outside_the_contract_is_refused(self) -> None:
        for streams in (0, -1, mover.MAX_STREAMS + 1, True):
            with self.subTest(streams=streams), self.assertRaises(MoverError):
                push(
                    source_root=Path("/"),
                    objects=[],
                    channel_factory=self.factory(),
                    streams=streams,
                )


class MoverReceiverEntryPointTests(unittest.TestCase):
    def test_the_receiver_runs_under_isolated_python_with_no_package(self) -> None:
        """`-I -S` is the engine's launcher contract and the receiver honours it.

        Under `-I` the script's own directory is not on `sys.path`, so a
        receiver that imported anything from `bulkload_lib` could not start.
        This runs it out of a copied file with no package around it at all.
        """
        with tempfile.TemporaryDirectory(prefix="bulkload-mover-alone-") as temporary:
            alone = Path(temporary) / "mover.py"
            alone.write_bytes(MODULE.read_bytes())
            root = Path(temporary) / "root"
            root.mkdir()
            result = subprocess.run(
                [
                    sys.executable,
                    "-I",
                    "-S",
                    str(alone),
                    "--receive",
                    "--root",
                    str(root),
                ],
                input=b"",
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
            )
            self.assertEqual(result.returncode, 3, result.stderr)
            self.assertIn(b"without a done frame", result.stderr)
            # It still announced itself before failing.
            self.assertIn(b"hello", result.stdout)

    def test_the_receiver_refuses_a_root_that_is_not_a_directory(self) -> None:
        with tempfile.TemporaryDirectory(prefix="bulkload-mover-root-") as temporary:
            missing = Path(temporary) / "absent"
            result = subprocess.run(
                [
                    sys.executable,
                    "-I",
                    "-S",
                    str(MODULE),
                    "--receive",
                    "--root",
                    str(missing),
                ],
                input=b"",
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
            )
            self.assertEqual(result.returncode, 3)
            self.assertIn(b"not a directory", result.stderr)


FAKE_SSH = """#!/usr/bin/env python3
import os
import shlex
import sys

arguments = sys.argv[1:]
while arguments and arguments[0].startswith("-o"):
    arguments.pop(0)
if arguments and arguments[0] == "--":
    arguments.pop(0)
if not arguments:
    raise SystemExit(90)
arguments.pop(0)
if len(arguments) == 1:
    arguments = shlex.split(arguments[0])
os.execv(arguments[0], arguments)
"""


class TransportMoverSelectionTests(unittest.TestCase):
    """The `--mover` switch: rsync stays the default; native is opt-in."""

    def _push(self, *, transport_mover: str):
        from bulkload_lib import executor
        from bulkload_lib.executor import push_agent_transport
        from test_bulkload import CutoverFixture

        temporary = tempfile.TemporaryDirectory(prefix="bulkload-mover-stage-")
        self.addCleanup(temporary.cleanup)
        fixture = CutoverFixture(Path(temporary.name), sqlite_union=False)
        plan = fixture.plan()
        fake_ssh = fixture.root / "fake-ssh"
        fake_ssh.write_text(FAKE_SSH, encoding="utf-8")
        fake_ssh.chmod(0o700)
        prepare = executor.stage_agent_plan(
            plan,
            accepted_plan_sha256=plan["plan_sha256"],
            phase="preseed",
            stage_root=fixture.stage,
            allow_accounted_copy=True,
            reserve_bytes=0,
            transport_mode="prepare",
        )
        quarantine = Path(prepare["transport"]["quarantine_root"])
        channel = None
        if transport_mover == "native":
            channel = (
                hashlib.sha256(MODULE.read_bytes()).hexdigest(),
                local_channel_factory(
                    sys.executable, str(MODULE), os.fspath(quarantine)
                ),
            )
        receipt = push_agent_transport(
            prepare,
            fixture.stage / ".transport-allowlist-preseed.nul",
            accepted_plan_sha256=plan["plan_sha256"],
            phase="preseed",
            stage_root=fixture.stage,
            destination_ssh_host=socket.gethostname(),
            transport_mover=transport_mover,
            transport_streams=4,
            _ssh_binary=str(fake_ssh),
            _channel_factory=channel,
        )
        allowlist = (fixture.stage / ".transport-allowlist-preseed.nul").read_bytes()
        relatives = [os.fsdecode(item) for item in allowlist.split(b"\0") if item]
        return receipt, quarantine, relatives

    def test_native_lands_every_allowlist_path_byte_and_mode_identical(self) -> None:
        receipt, quarantine, relatives = self._push(transport_mover="native")
        self.assertEqual(receipt["transport"]["mode"], "ssh-native-push")
        self.assertTrue(relatives)
        for relative in relatives:
            source = Path("/") / relative
            landed = quarantine / relative
            self.assertTrue(landed.exists() or landed.is_symlink(), relative)
            if source.is_symlink():
                self.assertEqual(os.readlink(landed), os.readlink(source))
                continue
            self.assertEqual(landed.read_bytes(), source.read_bytes(), relative)
            self.assertEqual(
                stat.S_IMODE(landed.lstat().st_mode),
                stat.S_IMODE(source.lstat().st_mode),
                relative,
            )

    def test_native_leaves_no_extra_entry_for_the_custody_namespace(self) -> None:
        """`validate_snapshot_custody` fails closed on an unexpected entry."""
        _, quarantine, relatives = self._push(transport_mover="native")
        landed = {
            path.relative_to(quarantine).as_posix()
            for path in quarantine.rglob("*")
            if path.is_file() or path.is_symlink()
        }
        self.assertEqual(landed, set(relatives))

    def test_rsync_remains_the_default_and_records_its_own_mode(self) -> None:
        receipt, _, _ = self._push(transport_mover="rsync")
        self.assertEqual(receipt["transport"]["mode"], "ssh-rsync-push")

    def test_an_unsupported_mover_is_refused(self) -> None:
        from bulkload_lib.executor import push_agent_transport
        from bulkload_lib.model import BulkloadError

        with self.assertRaisesRegex(BulkloadError, "transport mover is unsupported"):
            push_agent_transport(
                {},
                Path("/nonexistent"),
                accepted_plan_sha256="0" * 64,
                phase="preseed",
                stage_root=Path("/tmp/stage"),
                destination_ssh_host=None,
                transport_mover="rclone",
            )

    def test_parser_default_is_rsync_and_the_flag_is_push_only(self) -> None:
        from bulkload_lib.cli import _agent_stage, build_parser
        from bulkload_lib.model import BulkloadError

        base = [
            "agent-stage",
            "--phase",
            "preseed",
            "--accept-plan-sha256",
            "0" * 64,
            "--stage-root",
            "/tmp/stage",
            "--output",
            "-",
        ]
        parser = build_parser()
        self.assertEqual(parser.parse_args(base).mover, "rsync")
        self.assertEqual(parser.parse_args(base).transport_streams, 8)
        self.assertEqual(
            parser.parse_args([*base, "--mover", "native"]).mover, "native"
        )
        arguments = parser.parse_args(
            [*base, "--mover", "native", "--plan", "/tmp/plan.json"]
        )
        with self.assertRaisesRegex(
            BulkloadError, "transport mover applies only to the push transport"
        ):
            _agent_stage(arguments)


if __name__ == "__main__":
    unittest.main()
