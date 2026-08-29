from __future__ import annotations

import argparse
import copy
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import sqlite3
import stat
import subprocess
import tempfile
import threading
import unittest
from unittest import mock

from bulkload_lib.cli import _agent_plan, _protect_output, build_parser
from bulkload_lib.executor import (
    _atomic_install_blob,
    _current_record,
    _git,
    _git_worktree_state,
    _rollback_steps,
    _same_record,
    _snapshot_allowlist_stream,
    _StageSourceChanged,
    _verify_record,
    _write_object_bytes,
    apply_agent_plan,
    push_agent_transport,
    recover_agent_apply,
    rollback_agent_apply,
    stage_agent_plan,
    verify_agent_plan,
)
from bulkload_lib.model import (
    AGENT_CAPTURE_SCHEMA,
    AGENT_PLAN_SCHEMA,
    GIT_WORKSPACE_SCHEMA,
    BulkloadError,
    canonical_bytes,
    fsync_directory,
    git_environment,
    reflink_clone,
    require_capacity,
    require_digest,
    sha256_bytes,
    sha256_symlink,
    translate_path,
)
from bulkload_lib.planner import (
    _fingerprint,
    compile_agent_plan_authorities,
    materialize_plan_operation,
)
from bulkload_lib.scanner import (
    _capture_provider,
    canonical_path_map,
    canonical_provider_policy,
    capture_agent_state,
    inspect_rsync,
    stable_capture_pair,
    validate_agent_capture,
)
import bulkload_lib.scanner as scanner


def git(path: Path, *arguments: str) -> bytes:
    result = subprocess.run(
        ["git", "-C", str(path), *arguments],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env={
            **os.environ,
            "GIT_CONFIG_GLOBAL": os.devnull,
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_TERMINAL_PROMPT": "0",
        },
    )
    if result.returncode:
        raise AssertionError(
            f"git {' '.join(arguments)} failed: {result.stderr.decode(errors='replace')}"
        )
    return result.stdout


def gnu_rsync_path() -> Path:
    candidates = [shutil.which("rsync")]
    candidates.extend(
        os.fspath(path) for path in Path("/nix/store").glob("*-rsync-*/bin/rsync")
    )
    for candidate in candidates:
        if not candidate:
            continue
        result = subprocess.run(
            [candidate, "--help"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        if result.returncode == 0 and b"--from0" in result.stdout:
            return Path(candidate).resolve()
    raise AssertionError("tests require GNU rsync with --from0")


def initialize_repository(path: Path) -> None:
    path.mkdir(parents=True)
    git(path, "init")
    git(path, "config", "user.email", "bulkload@example.invalid")
    git(path, "config", "user.name", "Bulkload Test")
    git(path, "config", "commit.gpgsign", "false")
    (path / "tracked.txt").write_text("base\n", encoding="utf-8")
    (path / "deleted.txt").write_text("delete me\n", encoding="utf-8")
    git(path, "add", "tracked.txt", "deleted.txt")
    git(path, "commit", "-m", "base")


def create_database(
    path: Path, rows: list[tuple[int, str]], *, wal: bool = False
) -> None:
    connection = sqlite3.connect(path)
    try:
        if wal:
            connection.execute("PRAGMA journal_mode=WAL")
        connection.execute(
            "CREATE TABLE state(id INTEGER PRIMARY KEY, value TEXT NOT NULL)"
        )
        connection.executemany("INSERT INTO state VALUES (?,?)", rows)
        connection.commit()
        if wal:
            connection.execute("PRAGMA wal_checkpoint(PASSIVE)")
    finally:
        connection.close()


def create_bag_database(path: Path, values: list[str]) -> None:
    connection = sqlite3.connect(path)
    try:
        connection.execute("CREATE TABLE state(value TEXT NOT NULL)")
        connection.executemany(
            "INSERT INTO state VALUES (?)", [(value,) for value in values]
        )
        connection.commit()
    finally:
        connection.close()


def ref_inventory(path: Path) -> bytes:
    return git(path, "for-each-ref", "--format=%(refname)%09%(objectname)%09%(symref)")


def object_inventory(path: Path) -> dict[str, str]:
    common = Path(
        git(path, "rev-parse", "--path-format=absolute", "--git-common-dir")
        .decode()
        .strip()
    )
    objects = common / "objects"
    return {
        item.relative_to(objects).as_posix(): hashlib.sha256(
            item.read_bytes()
        ).hexdigest()
        for item in objects.rglob("*")
        if item.is_file()
    }


def reflog_inventory(path: Path) -> dict[str, bytes]:
    common = Path(
        git(path, "rev-parse", "--path-format=absolute", "--git-common-dir")
        .decode()
        .strip()
    )
    return {
        item.relative_to(common).as_posix(): item.read_bytes()
        for item in common.rglob("*")
        if item.is_file() and "logs" in item.relative_to(common).parts
    }


def worktree_inventory(path: Path) -> bytes:
    return git(path, "worktree", "list", "--porcelain", "-z")


def git_transaction_inventory(path: Path) -> tuple[bytes, dict[str, bytes], bytes]:
    return ref_inventory(path), reflog_inventory(path), worktree_inventory(path)


def add_linked_worktree(repository: Path, root: Path, *, locked: bool = False) -> Path:
    git(repository, "branch", "linked-cutover")
    target = root / "linked"
    git(
        repository,
        "worktree",
        "add",
        "--no-checkout",
        str(target),
        "linked-cutover",
    )
    git(target, "reset", "--hard", "HEAD")
    if locked:
        git(repository, "worktree", "lock", str(target))
    return target


def compile_agent_plan(
    source_a: dict,
    source_b: dict,
    destination_a: dict,
    destination_b: dict,
) -> dict:
    """Validate both live pairs, then compile — the two-capture test driver.

    This lived in `planner.py` with no production caller: `cli.py` reaches
    `compile_agent_plan_authorities` directly. It is test scaffolding, so it
    lives with the tests now. The engine behaviour it exercises —
    `stable_capture_pair` on each role, then the authorities compile — is
    unchanged, and every existing call site is the pin.
    """
    stable_capture_pair(source_a, source_b, role="source")
    stable_capture_pair(destination_a, destination_b, role="destination")
    return compile_agent_plan_authorities(
        source_b,
        (source_a["capture_id"], source_b["capture_id"]),
        destination_b,
        (destination_a["capture_id"], destination_b["capture_id"]),
    )


class CutoverFixture:
    def __init__(self, root: Path, *, sqlite_union: bool = True) -> None:
        self.root = root.resolve()
        self.source_home = self.root / "source"
        self.destination_home = self.root / "destination"
        self.source_git = self.source_home / "git"
        self.destination_git = self.destination_home / "git"
        self.source_repo = self.source_git / "repo"
        self.destination_repo = self.destination_git / "repo"
        self.stage = self.root / "stage"
        self.rollback = self.root / "rollback"
        self.journal = self.root / "apply-journal.json"
        self.rsync_path = gnu_rsync_path()
        self.source_seats: list[tuple[str, Path] | tuple[str, Path, str]] = []
        self.destination_seats: list[tuple[str, Path] | tuple[str, Path, str]] = []
        self.managed_exclusions: list[tuple[str, str]] = []
        for home in (self.source_home, self.destination_home):
            for relative in ("git", ".codex", ".claude", ".pi/agent"):
                (home / relative).mkdir(parents=True, exist_ok=True)
        initialize_repository(self.source_repo)
        git(
            self.source_home, "clone", str(self.source_repo), str(self.destination_repo)
        )
        (self.source_repo / "tracked.txt").write_text("source dirt\n", encoding="utf-8")
        (self.source_repo / "untracked.txt").write_text(
            "source only\n", encoding="utf-8"
        )
        (self.source_repo / "deleted.txt").unlink()
        (self.source_repo / "intent.txt").write_text("intent\n", encoding="utf-8")
        git(self.source_repo, "add", "-N", "intent.txt")
        os.symlink("tracked.txt", self.source_repo / "link")
        (self.source_home / "git" / "fleet-note.txt").write_text(
            "non-git fleet state\n", encoding="utf-8"
        )
        (self.source_home / ".codex" / "history.jsonl").write_text(
            '{"session_id":"one","text":"source"}\n', encoding="utf-8"
        )
        (self.destination_home / ".codex" / "history.jsonl").write_text(
            '{"session_id":"one","text":"source"}\n', encoding="utf-8"
        )
        source_auth = self.source_home / ".codex" / "auth.json"
        source_auth.write_text('{"token":"SOURCE-SECRET"}', encoding="utf-8")
        source_auth.chmod(0o600)
        destination_auth = self.destination_home / ".codex" / "auth.json"
        destination_auth.write_text('{"token":"DESTINATION-SECRET"}', encoding="utf-8")
        destination_auth.chmod(0o600)
        if sqlite_union:
            create_database(
                self.source_home / ".codex" / "state.sqlite",
                [(1, "shared"), (2, "source")],
            )
            create_database(
                self.destination_home / ".codex" / "state.sqlite",
                [(1, "shared"), (3, "destination")],
            )
        self.path_map = canonical_path_map(
            [
                (str(self.source_home), str(self.destination_home)),
                (str(self.source_git), str(self.destination_git)),
            ]
        )

    def capture(self, role: str) -> dict:
        home = self.source_home if role == "source" else self.destination_home
        git_root = self.source_git if role == "source" else self.destination_git
        return capture_agent_state(
            role=role,
            home=home,
            git_root=git_root,
            codex_root=None,
            claude_root=None,
            pi_root=None,
            seats=self.source_seats if role == "source" else self.destination_seats,
            path_map=self.path_map,
            writers_quiesced=True,
            managed_exclusions=self.managed_exclusions,
            rsync_path=self.rsync_path,
            max_files=50_000,
            max_bytes=4 * 1024**3,
            max_sqlite_rows=100_000,
        )

    def plan(self) -> dict:
        return compile_agent_plan(
            self.capture("source"),
            self.capture("source"),
            self.capture("destination"),
            self.capture("destination"),
        )

    def stage_both(self, plan: dict) -> tuple[dict, dict]:
        preseed = stage_agent_plan(
            plan,
            accepted_plan_sha256=plan["plan_sha256"],
            phase="preseed",
            stage_root=self.stage,
            allow_accounted_copy=True,
            reserve_bytes=0,
        )
        final = stage_agent_plan(
            plan,
            accepted_plan_sha256=plan["plan_sha256"],
            phase="final",
            stage_root=self.stage,
            allow_accounted_copy=True,
            reserve_bytes=0,
        )
        return preseed, final

    def apply(self, plan: dict, final: dict) -> dict:
        return apply_agent_plan(
            plan,
            final,
            accepted_plan_sha256=plan["plan_sha256"],
            journal_path=self.journal,
            rollback_root=self.rollback,
            reserve_bytes=0,
        )

    def staged(self) -> tuple[dict, dict]:
        plan = self.plan()
        return plan, self.stage_both(plan)[1]

    def recover(self, plan: dict, final: dict, strategy: str) -> dict:
        return recover_agent_apply(
            plan, final, journal_path=self.journal, strategy=strategy
        )

    @staticmethod
    def roll_back(applied: dict) -> dict:
        return rollback_agent_apply(
            applied, accepted_receipt_sha256=applied["receipt_sha256"]
        )


class SchemaAndCaptureTests(unittest.TestCase):
    def test_live_snapshot_preserves_opaque_nested_false_git_discovery(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            opaque = fixture.source_repo / ".cache" / "uv" / "archive"
            (opaque / ".git").mkdir(parents=True)
            (opaque / ".git" / "opaque").write_bytes(b"not Git authority\n")
            (opaque / "payload.whl").write_bytes(b"opaque payload\n")
            snapshot_root = fixture.root / "evidence" / "source-a.snapshot"
            real_git = scanner._git

            def reject_false_discovery(
                path: Path, arguments: list[str], **kwargs: object
            ) -> bytes:
                if (
                    tuple(Path(path).parts[-3:]) == (".cache", "uv", "archive")
                    and arguments[0] == "rev-parse"
                ):
                    raise BulkloadError("Git inspection command failed (rev-parse)")
                return real_git(path, arguments, **kwargs)

            with mock.patch.object(scanner, "_git", side_effect=reject_false_discovery):
                capture = capture_agent_state(
                    role="source",
                    home=fixture.source_home,
                    git_root=fixture.source_git,
                    codex_root=None,
                    claude_root=None,
                    pi_root=None,
                    seats=fixture.source_seats,
                    path_map=fixture.path_map,
                    writers_quiesced=False,
                    snapshot_root=snapshot_root,
                    managed_exclusions=fixture.managed_exclusions,
                    rsync_path=fixture.rsync_path,
                    max_files=50_000,
                    max_bytes=4 * 1024**3,
                    max_sqlite_rows=100_000,
                    snapshot_reserve_bytes=0,
                )

            validate_agent_capture(capture, expected_role="source")
            preserved = (
                snapshot_root
                / "roots"
                / "git"
                / "repo"
                / ".cache"
                / "uv"
                / "archive"
                / "payload.whl"
            )
            self.assertEqual(preserved.read_bytes(), b"opaque payload\n")

    def test_live_snapshot_refuses_failed_declared_gitfile_authority(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            git_root = Path(temporary).resolve() / "git"
            repository = git_root / "declared"
            repository.mkdir(parents=True)
            (repository / ".git").write_text(
                "gitdir: /missing/declared-authority\n", encoding="utf-8"
            )

            for inspect in (
                scanner._git_snapshot_authorities,
                scanner._git_live_generation,
            ):
                with self.subTest(inspect=inspect.__name__):
                    with self.assertRaisesRegex(
                        BulkloadError, "Git inspection command failed \\(rev-parse\\)"
                    ):
                        inspect(git_root)

    def test_live_snapshot_git_authority_accepts_unborn_absent_index_only(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            git_root = Path(temporary).resolve() / "git"
            repository = git_root / "demo"
            repository.mkdir(parents=True)
            git(repository, "init")
            self.assertFalse((repository / ".git" / "index").exists())

            repositories, _controls = scanner._git_snapshot_authorities(git_root)
            self.assertIn(repository.resolve(), repositories)

            real_git = scanner._git
            external_index = Path(temporary) / "external" / "index"

            def report_external_index(
                path: Path, arguments: list[str], **kwargs: object
            ) -> bytes:
                if arguments[-2:] == ["--git-path", "index"]:
                    return os.fsencode(external_index) + b"\n"
                return real_git(path, arguments, **kwargs)

            with mock.patch.object(scanner, "_git", side_effect=report_external_index):
                with self.assertRaisesRegex(BulkloadError, "outside live snapshot"):
                    scanner._git_snapshot_authorities(git_root)

    def test_live_capture_seals_immutable_snapshot_without_quiescence(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=True)
            snapshot_root = fixture.root / "evidence" / "source-a.snapshot"
            (fixture.source_home / ".claude" / "foo").mkdir()
            (fixture.source_home / ".claude" / "foo" / "z").write_text("z")
            (fixture.source_home / ".claude" / "foo-bar").write_text("bar")
            capture = capture_agent_state(
                role="source",
                home=fixture.source_home,
                git_root=fixture.source_git,
                codex_root=None,
                claude_root=None,
                pi_root=None,
                seats=fixture.source_seats,
                path_map=fixture.path_map,
                writers_quiesced=False,
                snapshot_root=snapshot_root,
                managed_exclusions=fixture.managed_exclusions,
                rsync_path=fixture.rsync_path,
                max_files=50_000,
                max_bytes=4 * 1024**3,
                max_sqlite_rows=100_000,
                snapshot_reserve_bytes=0,
            )
            validate_agent_capture(capture, expected_role="source")
            self.assertFalse(capture["writers_quiesced"])
            snapshot = capture["catalog"]["snapshot"]
            self.assertEqual(snapshot["mode"], "immutable-live")
            self.assertEqual(snapshot["snapshot_id"], capture["capture_id"])
            self.assertTrue(Path(snapshot["seal_path"]).is_file())
            self.assertIn("sqlite-online-backup", snapshot["methods"])
            extra = snapshot_root / "undeclared"
            extra.write_text("extra")
            with self.assertRaisesRegex(BulkloadError, "top-level"):
                validate_agent_capture(capture, expected_role="source")

    def test_live_pair_plans_from_b_and_stages_after_live_source_moves(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=True)
            evidence = fixture.root / "evidence"

            def live(role: str, name: str, base: dict | None = None) -> dict:
                home = (
                    fixture.source_home
                    if role == "source"
                    else fixture.destination_home
                )
                return capture_agent_state(
                    role=role,
                    home=home,
                    git_root=(
                        fixture.source_git
                        if role == "source"
                        else fixture.destination_git
                    ),
                    codex_root=None,
                    claude_root=None,
                    pi_root=None,
                    seats=(
                        fixture.source_seats
                        if role == "source"
                        else fixture.destination_seats
                    ),
                    path_map=fixture.path_map,
                    writers_quiesced=False,
                    snapshot_root=evidence / f"{name}.snapshot",
                    snapshot_base_seal=Path(base["catalog"]["snapshot"]["seal_path"])
                    if base is not None
                    else None,
                    managed_exclusions=fixture.managed_exclusions,
                    rsync_path=fixture.rsync_path,
                    max_files=50_000,
                    max_bytes=4 * 1024**3,
                    max_sqlite_rows=100_000,
                    snapshot_reserve_bytes=0,
                )

            source_a = live("source", "source-a")
            source_b = live("source", "source-b", source_a)
            source_history = fixture.source_home / ".codex" / "history.jsonl"
            source_history_before = source_history.read_bytes()
            self.assertIn("base-reflink", source_b["catalog"]["snapshot"]["methods"])
            destination_a = live("destination", "destination-a")
            destination_b = live("destination", "destination-b", destination_a)
            plan = compile_agent_plan(source_a, source_b, destination_a, destination_b)
            self.assertEqual(
                plan["source"]["catalog_sha256"], source_b["catalog_sha256"]
            )
            plan_path = evidence / "final-plan.json"
            plan_path.write_bytes(canonical_bytes(plan) + b"\n")
            protected_output = (
                Path(source_b["catalog"]["snapshot"]["seal_path"]).parent
                / "cutover-release.json"
            )
            release_arguments = build_parser().parse_args(
                [
                    "agent-verify",
                    "--plan",
                    str(plan_path),
                    "--stage-receipt",
                    str(evidence / "final-stage.json"),
                    "--apply-receipt",
                    str(evidence / "apply.json"),
                    "--destination-verify-receipt",
                    str(evidence / "destination-verify.json"),
                    "--output",
                    str(protected_output),
                ]
            )
            with self.assertRaisesRegex(BulkloadError, "evidence output overlaps"):
                _protect_output(release_arguments)
            fake_ssh = fixture.root / "fake-ssh-live"
            fake_ssh.write_text(
                "#!/usr/bin/env python3\n"
                "import os, shlex, sys\n"
                "arguments=sys.argv[1:]\n"
                "while arguments and arguments[0].startswith('-o'): arguments.pop(0)\n"
                "if arguments and arguments[0] == '--': arguments.pop(0)\n"
                "arguments.pop(0)\n"
                "if len(arguments) == 1: arguments=shlex.split(arguments[0])\n"
                "os.execv(arguments[0], arguments)\n",
                encoding="utf-8",
            )
            fake_ssh.chmod(0o700)
            transport_stage = fixture.root / "transport-stage"
            preseed_prepare = stage_agent_plan(
                plan,
                accepted_plan_sha256=plan["plan_sha256"],
                phase="preseed",
                stage_root=transport_stage,
                allow_accounted_copy=True,
                reserve_bytes=0,
                transport_mode="prepare",
            )
            preseed_transport = push_agent_transport(
                preseed_prepare,
                transport_stage / ".transport-allowlist-preseed.nul",
                accepted_plan_sha256=plan["plan_sha256"],
                phase="preseed",
                stage_root=transport_stage,
                destination_ssh_host=socket.gethostname(),
                _ssh_binary=str(fake_ssh),
            )
            live_preseed = stage_agent_plan(
                plan,
                accepted_plan_sha256=plan["plan_sha256"],
                phase="preseed",
                stage_root=transport_stage,
                allow_accounted_copy=True,
                reserve_bytes=0,
                transport_mode="materialize",
                prepare_receipt=preseed_prepare,
                transport_receipt=preseed_transport,
            )
            self.assertFalse(live_preseed["ready_for_apply"])
            prepare = stage_agent_plan(
                plan,
                accepted_plan_sha256=plan["plan_sha256"],
                phase="final",
                stage_root=transport_stage,
                allow_accounted_copy=True,
                reserve_bytes=0,
                transport_mode="prepare",
            )

            def mutate_during_push(
                source: object, snapshot: object, **arguments: object
            ) -> None:
                _snapshot_allowlist_stream(source, snapshot, **arguments)
                (fixture.source_home / ".codex" / "history.jsonl").write_text(
                    '{"session_id":"during","text":"push"}\n', encoding="utf-8"
                )

            with mock.patch(
                "bulkload_lib.executor._snapshot_allowlist_stream",
                side_effect=mutate_during_push,
            ):
                with self.assertRaisesRegex(BulkloadError, "changed after immutable"):
                    push_agent_transport(
                        prepare,
                        transport_stage / ".transport-allowlist-final.nul",
                        accepted_plan_sha256=plan["plan_sha256"],
                        phase="final",
                        stage_root=transport_stage,
                        destination_ssh_host=socket.gethostname(),
                        _ssh_binary=str(fake_ssh),
                    )
            source_history.write_bytes(source_history_before)
            final_transport = push_agent_transport(
                prepare,
                transport_stage / ".transport-allowlist-final.nul",
                accepted_plan_sha256=plan["plan_sha256"],
                phase="final",
                stage_root=transport_stage,
                destination_ssh_host=socket.gethostname(),
                _ssh_binary=str(fake_ssh),
            )
            final = stage_agent_plan(
                plan,
                accepted_plan_sha256=plan["plan_sha256"],
                phase="final",
                stage_root=transport_stage,
                allow_accounted_copy=True,
                reserve_bytes=0,
                transport_mode="materialize",
                prepare_receipt=prepare,
                transport_receipt=final_transport,
            )
            applied = fixture.apply(plan, final)
            destination_verified = verify_agent_plan(plan, final, applied)
            self.assertTrue(destination_verified["verified"])
            unavailable_stage = transport_stage.with_name("sting-only-stage")
            shutil.move(transport_stage, unavailable_stage)
            self.assertFalse(Path(final["stage_root"]).exists())
            source_history.write_text(
                '{"session_id":"after","text":"transport"}\n', encoding="utf-8"
            )
            with self.assertRaisesRegex(BulkloadError, "changed after immutable"):
                verify_agent_plan(
                    plan,
                    final,
                    applied,
                    destination_verify_receipt=destination_verified,
                )
            source_history.write_bytes(source_history_before)
            # The #24 break-glass cannot buy this release. The receipt below
            # seals `independent_fresh_observation: True` over that fence and
            # `validate_verify_receipt` requires it, so a skipped observation
            # here would be laundered into a receipt byte-identical to an
            # honest one. The refusal fires on the note variable alone, before
            # the snapshot binding is even looked at, so a stale export in a
            # profile or a unit file is a hard stop rather than a quiet one.
            break_glass_note = fixture.root / "cutover-break-glass.jsonl"
            with mock.patch.dict(
                os.environ,
                {"BULKLOAD_BREAK_GLASS_LIVE_FENCE_NOTE": os.fspath(break_glass_note)},
            ):
                with self.assertRaisesRegex(
                    BulkloadError, "break-glass is refused at this call site"
                ):
                    verify_agent_plan(
                        plan,
                        final,
                        applied,
                        destination_verify_receipt=destination_verified,
                    )
            self.assertFalse(break_glass_note.exists())
            released = verify_agent_plan(
                plan,
                final,
                applied,
                destination_verify_receipt=destination_verified,
            )
            self.assertTrue(released["verified"])
            self.assertEqual(released["verification_role"], "cutover-release")
            self.assertEqual(
                released["destination_verify_receipt_sha256"],
                destination_verified["receipt_sha256"],
            )
            (fixture.source_home / ".codex" / "history.jsonl").write_text(
                '{"session_id":"two","text":"live moved"}\n', encoding="utf-8"
            )
            preseed = stage_agent_plan(
                plan,
                accepted_plan_sha256=plan["plan_sha256"],
                phase="preseed",
                stage_root=fixture.stage,
                allow_accounted_copy=True,
                reserve_bytes=0,
            )
            self.assertFalse(preseed["ready_for_apply"])
            with self.assertRaisesRegex(BulkloadError, "changed after immutable"):
                stage_agent_plan(
                    plan,
                    accepted_plan_sha256=plan["plan_sha256"],
                    phase="final",
                    stage_root=fixture.stage,
                    allow_accounted_copy=True,
                    reserve_bytes=0,
                )

    def test_rsync_binding_is_rehashed_on_every_check(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            executable = Path(temporary) / "rsync"
            executable.write_text(
                "#!/bin/sh\n"
                'if [ "$1" = --version ]; then '
                "echo 'rsync version 3.4.4 protocol version 32'; else "
                "echo '--from0 --files-from --ignore-missing-args'; fi\n",
                encoding="utf-8",
            )
            executable.chmod(0o700)
            first = inspect_rsync(str(executable))
            executable.write_text(
                executable.read_text(encoding="utf-8") + "# replacement\n",
                encoding="utf-8",
            )
            second = inspect_rsync(str(executable))
            self.assertNotEqual(first["sha256"], second["sha256"])

    def test_rsync_path_rejects_shell_syntax_before_subprocess(self) -> None:
        with mock.patch("bulkload_lib.scanner.subprocess.run") as invoked:
            with self.assertRaisesRegex(BulkloadError, "shell-safe"):
                inspect_rsync("/tmp/rsync;touch")
            invoked.assert_not_called()

    def test_schema_ids_and_exact_public_cli(self) -> None:
        self.assertEqual(AGENT_CAPTURE_SCHEMA, "dev.tinyland.bulkload.agent-capture.v4")
        self.assertEqual(GIT_WORKSPACE_SCHEMA, "dev.tinyland.bulkload.git-workspace.v2")
        self.assertEqual(AGENT_PLAN_SCHEMA, "dev.tinyland.bulkload.agent-plan.v4")
        parser = build_parser()
        subparsers = next(
            action
            for action in parser._actions
            if isinstance(action, __import__("argparse")._SubParsersAction)
        )
        self.assertEqual(
            set(subparsers.choices),
            {
                "agent-capture",
                "agent-plan",
                "agent-stage",
                "agent-apply",
                "agent-verify",
                "agent-rollback",
                "agent-recover",
            },
        )
        push = parser.parse_args(
            [
                "agent-stage",
                "--phase",
                "preseed",
                "--transport-mode",
                "push",
                "--accept-plan-sha256",
                "0" * 64,
                "--stage-root",
                "/srv/fast-local/jess/bulkload/stage",
                "--destination-ssh-host",
                "jess@sting",
                "--prepare-receipt",
                "/secure/preseed-prepare.json",
                "--transport-allowlist",
                "/secure/preseed-allowlist.nul",
                "--output",
                "/secure/preseed-push.json",
            ]
        )
        self.assertIsNone(push.plan)
        self.assertEqual(push.transport_mode, "push")

    def test_longest_path_prefix_wins(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            (root / "home/git").mkdir(parents=True)
            mapping = canonical_path_map(
                [
                    (str(root / "home"), "/home/jess"),
                    (str(root / "home/git"), "/srv/fast-local/jess/git"),
                ]
            )
            self.assertEqual(
                translate_path(root / "home/git/repo", mapping),
                "/srv/fast-local/jess/git/repo",
            )
            self.assertEqual(
                translate_path(root / "home/.codex", mapping),
                "/home/jess/.codex",
            )

    def test_claude_payload_rewrite_uses_longest_path_prefix(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            home = root / "home"
            git_root = home / "git"
            claude = home / ".claude"
            git_root.mkdir(parents=True)
            claude.mkdir()
            source_path = git_root / "repo"
            line = json.dumps({"cwd": str(source_path)}, separators=(",", ":")) + "\n"
            (claude / "history.jsonl").write_text(line, encoding="utf-8")
            mapping = canonical_path_map(
                [
                    (str(home), "/home/jess"),
                    (str(git_root), "/srv/fast-local/jess/git"),
                ]
            )
            capture = capture_agent_state(
                role="source",
                home=home,
                git_root=git_root,
                codex_root=None,
                claude_root=claude,
                pi_root=None,
                seats=[],
                path_map=mapping,
                writers_quiesced=True,
                rsync_path=gnu_rsync_path(),
            )
            item = next(
                item
                for provider in capture["catalog"]["providers"]
                if provider["name"] == "claude"
                for item in provider["items"]
            )
            expected = line.replace(str(git_root), "/srv/fast-local/jess/git").encode()
            self.assertEqual(item["translated_sha256"], sha256_bytes(expected))

    def test_git_workspace_v2_captures_refs_worktrees_index_and_non_git(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            git(fixture.source_repo, "tag", "cutover-tag")
            git(fixture.source_repo, "notes", "add", "-m", "note")
            linked = fixture.source_git / "repo-linked"
            git(fixture.source_repo, "worktree", "add", "--detach", str(linked), "HEAD")
            capture = fixture.capture("source")
            self.assertTrue(capture["complete"], capture["catalog"]["blockers"])
            workspace = capture["catalog"]["git_workspaces"][0]
            self.assertEqual(workspace["schema"], GIT_WORKSPACE_SCHEMA)
            refs = {entry["name"] for entry in workspace["refs"]}
            self.assertIn("refs/tags/cutover-tag", refs)
            self.assertIn("refs/notes/commits", refs)
            self.assertEqual(len(workspace["worktrees"]), 2)
            primary = next(
                item
                for item in workspace["worktrees"]
                if item["path"] == str(fixture.source_repo)
            )
            self.assertTrue(
                any(item["intent_to_add"] for item in primary["index"]["entries"])
            )
            dirt = {item["path"]: item for item in primary["dirt"]}
            self.assertTrue(dirt["tracked.txt"]["modified"])
            self.assertTrue(dirt["deleted.txt"]["missing"])
            self.assertTrue(dirt["untracked.txt"]["untracked"])
            self.assertTrue(
                any(
                    item["relative_path"] == "link" and item["kind"] == "symlink"
                    for item in primary["files"]
                )
            )
            self.assertTrue(
                any(
                    item["relative_path"] == "fleet-note.txt"
                    for item in capture["catalog"]["non_git"]
                )
            )
            self.assertTrue(workspace["recovery_anchors"])

            plan = fixture.plan()
            git_operation = next(
                item
                for item in plan["operations"]
                if item["kind"] == "git-workspace-union"
            )
            self.assertIn("source_ref", git_operation)
            self.assertNotIn("source", git_operation)
            self.assertNotIn("destination_before", git_operation)

    def test_git_blob_hashing_uses_one_batch_process_for_linked_worktrees(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            linked = fixture.source_git / "repo-linked"
            git(fixture.source_repo, "worktree", "add", "--detach", str(linked), "HEAD")

            def legacy_sha(batch, oid):
                if oid in scanner.ZERO_OIDS:
                    return None
                process = subprocess.Popen(
                    ["git", "-C", str(batch._repository), "cat-file", "blob", oid],
                    stdout=subprocess.PIPE,
                    stderr=subprocess.DEVNULL,
                    env=git_environment(),
                )
                digest = hashlib.sha256()
                assert process.stdout is not None
                with process.stdout:
                    while chunk := process.stdout.read(1024 * 1024):
                        digest.update(chunk)
                self.assertEqual(process.wait(), 0)
                return digest.hexdigest()

            with mock.patch.object(scanner._GitBlobBatch, "sha256", legacy_sha):
                legacy_capture = fixture.capture("source")
            with mock.patch(
                "bulkload_lib.scanner.subprocess.Popen", wraps=subprocess.Popen
            ) as popen:
                capture = fixture.capture("source")
            self.assertTrue(capture["complete"], capture["catalog"]["blockers"])
            self.assertEqual(capture["catalog"], legacy_capture["catalog"])
            self.assertEqual(
                capture["catalog_sha256"], legacy_capture["catalog_sha256"]
            )
            batch_commands = [
                invocation.args[0]
                for invocation in popen.call_args_list
                if invocation.args
                and invocation.args[0][-2:] == ["cat-file", "--batch"]
            ]
            self.assertEqual(len(batch_commands), 1)

    def test_independent_git_workspaces_capture_concurrently(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            initialize_repository(fixture.source_git / "second-repo")
            original = scanner._capture_workspace
            barrier = threading.Barrier(2, timeout=10)

            def synchronized(*args, **kwargs):
                barrier.wait()
                return original(*args, **kwargs)

            with mock.patch(
                "bulkload_lib.scanner._capture_workspace", side_effect=synchronized
            ):
                capture = fixture.capture("source")
            self.assertTrue(capture["complete"], capture["catalog"]["blockers"])
            self.assertEqual(len(capture["catalog"]["git_workspaces"]), 2)

    def test_invalid_git_candidate_falls_back_to_exact_non_git(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            invalid = fixture.source_git / "invalid-candidate"
            (invalid / ".git").mkdir(parents=True)
            marker = invalid / ".git/opaque.bin"
            marker.write_bytes(b"not a repository\x00")
            payload = invalid / "payload.bin"
            payload.write_bytes(b"opaque payload\xff")

            capture = fixture.capture("source")
            self.assertTrue(capture["complete"], capture["catalog"]["blockers"])
            non_git = {
                item["relative_path"]: item for item in capture["catalog"]["non_git"]
            }
            self.assertEqual(
                non_git["invalid-candidate/.git/opaque.bin"]["sha256"],
                hashlib.sha256(marker.read_bytes()).hexdigest(),
            )
            self.assertEqual(
                non_git["invalid-candidate/payload.bin"]["sha256"],
                hashlib.sha256(payload.read_bytes()).hexdigest(),
            )

            unreadable = fixture.source_git / "unreadable-candidate"
            unreadable.mkdir()
            unreadable_git = unreadable / ".git"
            unreadable_git.write_bytes(b"not git\n")
            path_open = Path.open

            def guarded_open(path: Path, *args, **kwargs):
                if path == unreadable_git:
                    raise PermissionError("unreadable test authority")
                return path_open(path, *args, **kwargs)

            with mock.patch.object(Path, "open", guarded_open):
                unreadable_capture = fixture.capture("source")
            self.assertFalse(unreadable_capture["complete"])
            self.assertIn(
                "git-discovery-failed",
                {item["code"] for item in unreadable_capture["catalog"]["blockers"]},
            )

            unsafe = fixture.source_git / "unsafe-candidate"
            unsafe.mkdir()
            os.symlink("../invalid-candidate/.git", unsafe / ".git")
            external = fixture.source_git / "external-authority"
            external.mkdir()
            (external / ".git").write_text(
                "gitdir: /unavailable/git/authority\n", encoding="utf-8"
            )
            blocked = fixture.capture("source")
            self.assertFalse(blocked["complete"])
            self.assertTrue(
                {"unsafe-git-authority", "git-discovery-failed"}.issubset(
                    {item["code"] for item in blocked["catalog"]["blockers"]}
                )
            )

    def test_alternates_and_fsck_failures_use_opaque_non_git_custody(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            alternate = fixture.source_git / "alternate-repo"
            initialize_repository(alternate)
            external_objects = fixture.root / "external-objects"
            external_objects.mkdir()
            alternates = alternate / ".git/objects/info/alternates"
            alternates.write_text(str(external_objects) + "\n", encoding="utf-8")

            corrupt = fixture.source_git / "corrupt-repo"
            initialize_repository(corrupt)
            corrupt_object = corrupt / ".git/objects/aa" / ("0" * 38)
            corrupt_object.parent.mkdir()
            corrupt_object.write_bytes(b"not a loose object")

            capture = fixture.capture("source")
            self.assertTrue(capture["complete"], capture["catalog"]["blockers"])
            workspace_paths = {
                item["path"] for item in capture["catalog"]["git_workspaces"]
            }
            self.assertNotIn(str(alternate.resolve()), workspace_paths)
            self.assertNotIn(str(corrupt.resolve()), workspace_paths)
            non_git = {
                item["relative_path"]: item for item in capture["catalog"]["non_git"]
            }
            self.assertEqual(
                non_git["alternate-repo/.git/objects/info/alternates"]["sha256"],
                hashlib.sha256(alternates.read_bytes()).hexdigest(),
            )
            self.assertEqual(
                non_git["corrupt-repo/.git/objects/aa/" + "0" * 38]["sha256"],
                hashlib.sha256(corrupt_object.read_bytes()).hexdigest(),
            )

            alternates.unlink()
            os.symlink(external_objects, alternates)
            unsafe_alternates = fixture.capture("source")
            self.assertFalse(unsafe_alternates["complete"])
            self.assertIn(
                "Git alternates authority is unsafe",
                {
                    item.get("detail")
                    for item in unsafe_alternates["catalog"]["blockers"]
                },
            )
            alternates.unlink()
            alternates.write_text(str(external_objects) + "\n", encoding="utf-8")

            linked = add_linked_worktree(
                alternate, fixture.source_git / "alternate-linked"
            )
            linked.rename(fixture.root / "moved-linked-worktree")
            blocked = fixture.capture("source")
            self.assertFalse(blocked["complete"])
            self.assertIn(
                "Git workspace has blockers in addition to opaque-only state",
                {item.get("detail") for item in blocked["catalog"]["blockers"]},
            )

    def test_missing_fetch_head_recovery_oid_is_non_authoritative(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            git_dir = fixture.source_repo / ".git"
            missing = "f" * 40
            (git_dir / "FETCH_HEAD").write_text(
                f"{missing}\tnot-for-merge\tbranch 'stale' of example.invalid\n",
                encoding="utf-8",
            )
            capture = fixture.capture("source")
            self.assertTrue(capture["complete"], capture["catalog"]["blockers"])
            anchors = capture["catalog"]["git_workspaces"][0]["recovery_anchors"]
            self.assertNotIn(missing, {item["oid"] for item in anchors})

            (git_dir / "FETCH_HEAD").unlink()
            (git_dir / "ORIG_HEAD").write_text(missing + "\n", encoding="ascii")
            blocked_pseudo = fixture.capture("source")
            self.assertFalse(blocked_pseudo["complete"])
            self.assertIn(
                "a Git recovery anchor object is missing",
                {item.get("detail") for item in blocked_pseudo["catalog"]["blockers"]},
            )

            (git_dir / "ORIG_HEAD").unlink()
            (git_dir / "logs/missing-anchor").write_text(
                f"{'0' * 40} {missing} actor <actor@example.invalid> 0 +0000\told\n",
                encoding="ascii",
            )
            blocked_reflog = fixture.capture("source")
            self.assertFalse(blocked_reflog["complete"])
            self.assertIn(
                "a Git recovery anchor object is missing",
                {item.get("detail") for item in blocked_reflog["catalog"]["blockers"]},
            )

    def test_capture_pair_rejects_motion_and_reuse(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            first = fixture.capture("source")
            reused = copy.deepcopy(first)
            with self.assertRaisesRegex(BulkloadError, "reuse"):
                stable_capture_pair(first, reused, role="source")
            (fixture.source_repo / "untracked.txt").write_text(
                "moved\n", encoding="utf-8"
            )
            second = fixture.capture("source")
            with self.assertRaisesRegex(BulkloadError, "not byte-stable"):
                stable_capture_pair(first, second, role="source")

    def test_declared_provider_root_defaults_to_portable_private(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            (fixture.source_home / ".codex" / "mystery.bin").write_bytes(b"unknown")
            capture = fixture.capture("source")
            self.assertTrue(capture["complete"], capture["catalog"]["blockers"])
            codex = next(
                item
                for item in capture["catalog"]["providers"]
                if item["name"] == "codex"
            )
            mystery = next(
                item
                for item in codex["items"]
                if item["relative_path"] == "mystery.bin"
            )
            self.assertEqual(mystery["classification"], "portable-private")

    def test_provider_walk_error_is_a_capture_blocker(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve() / "provider"
            root.mkdir()

            def denied_walk(*_arguments: object, **arguments: object) -> list[object]:
                callback = arguments["onerror"]
                assert callable(callback)
                callback(OSError(13, "denied", os.fspath(root / "unreadable")))
                return []

            with mock.patch("bulkload_lib.scanner.os.walk", side_effect=denied_walk):
                _provider, blockers = _capture_provider(
                    "claude",
                    root,
                    role="source",
                    path_map=canonical_path_map(
                        [(os.fspath(root.parent), "/destination")]
                    ),
                    exclusions=(),
                    max_files=10,
                    max_bytes=1024,
                    max_sqlite_rows=10,
                )
            self.assertEqual(
                blockers,
                [
                    {
                        "code": "unreadable-agent-state",
                        "path": os.fspath(root / "unreadable"),
                    }
                ],
            )

    def test_provider_regenerate_symlinks_are_pruned_before_admission(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            codex = fixture.source_home / ".codex"
            (codex / "plugins/cache/openai-bundled/chrome").mkdir(parents=True)
            os.symlink(
                "/private/rebuildable/chrome",
                codex / "plugins/cache/openai-bundled/chrome/latest",
            )
            (codex / ".tmp").mkdir()
            os.symlink("/nix/store/rebuildable", codex / ".tmp/arg0")
            (codex / "archived_sessions").mkdir()
            (codex / "archived_sessions/old.jsonl").write_text(
                '{"value":1}\n', encoding="utf-8"
            )
            (codex / "sessions").mkdir()
            os.symlink("../archived_sessions/old.jsonl", codex / "sessions/old.jsonl")
            capture = fixture.capture("source")
            self.assertTrue(capture["complete"], capture["catalog"]["blockers"])
            provider = next(
                item
                for item in capture["catalog"]["providers"]
                if item["name"] == "codex"
            )
            items = {item["relative_path"]: item for item in provider["items"]}
            self.assertFalse(
                any(
                    path == ".tmp"
                    or path.startswith(".tmp/")
                    or path == "plugins/cache"
                    or path.startswith("plugins/cache/")
                    for path in items
                )
            )
            self.assertEqual(
                items["sessions/old.jsonl"]["classification"], "portable-symlink"
            )
            os.symlink("/private/not-portable", codex / "absolute-private")
            blocked = fixture.capture("source")
            self.assertFalse(blocked["complete"])
            self.assertIn(
                "codex:absolute-private",
                {item["path"] for item in blocked["catalog"]["blockers"]},
            )

    def test_provider_regenerate_trees_prune_special_entries(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            roots = {
                "codex": fixture.source_home / ".codex",
                "claude": fixture.source_home / ".claude",
                "pi": fixture.source_home / ".pi/agent",
            }
            regenerate = {
                "codex": ("logs", "tmp", "shell_snapshots"),
                "claude": (
                    "cache",
                    "debug",
                    "logs",
                    "telemetry",
                    "security/agent-sdk-venv",
                    "agent-notes-rescue/archive/tmp",
                ),
                "pi": ("cache", "logs", "tmp"),
            }
            for provider, relatives in regenerate.items():
                for relative in relatives:
                    parent = roots[provider] / relative
                    parent.mkdir(parents=True)
                    os.symlink("/private/runtime-only", parent / "special-link")

            capture = fixture.capture("source")
            self.assertTrue(capture["complete"], capture["catalog"]["blockers"])
            providers = {
                provider["name"]: provider
                for provider in capture["catalog"]["providers"]
            }
            for provider, relatives in regenerate.items():
                captured = {
                    item["relative_path"] for item in providers[provider]["items"]
                }
                for relative in relatives:
                    self.assertFalse(
                        any(
                            path == relative or path.startswith(relative + "/")
                            for path in captured
                        ),
                        (provider, relative, captured),
                    )

    def test_claude_opaque_history_and_rescue_state_remain_exact(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            claude = fixture.source_home / ".claude"
            payloads = {
                "projects/repo/tool-results/result.sqlite": b"binary sqlite\x00\xff",
                "projects/repo/reports/history.jsonl": b"binary history\x00\xff",
                "agent-notes-rescue/auth.json": b"opaque auth-shaped state",
                "agent-notes-rescue/state.sqlite": b"opaque sqlite-shaped state",
                "history.jsonl": b'{"stable-but-incomplete":true}',
            }
            for relative, payload in payloads.items():
                path = claude / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(payload)

            capture = fixture.capture("source")
            self.assertTrue(capture["complete"], capture["catalog"]["blockers"])
            provider = next(
                item
                for item in capture["catalog"]["providers"]
                if item["name"] == "claude"
            )
            items = {item["relative_path"]: item for item in provider["items"]}
            for relative, payload in payloads.items():
                self.assertEqual(items[relative]["classification"], "portable-private")
                self.assertEqual(
                    items[relative]["sha256"], hashlib.sha256(payload).hexdigest()
                )
                self.assertNotIn("translated_sha256", items[relative])
                self.assertNotIn("records", items[relative])

    def test_mutable_seat_skips_socket_but_blocks_other_special_entries(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            seat = fixture.source_home / "runtime-seat"
            seat.mkdir()
            (seat / "state.bin").write_bytes(b"durable")
            socket_path = seat / "runtime.sock"
            listener = socket.socket(socket.AF_UNIX)
            try:
                listener.bind(str(socket_path))
                fixture.source_seats = [("runtime", seat)]
                capture = fixture.capture("source")
            finally:
                listener.close()
            self.assertTrue(capture["complete"], capture["catalog"]["blockers"])
            captured_seat = next(
                item
                for item in capture["catalog"]["seats"]
                if item["name"] == "runtime"
            )
            self.assertEqual(
                {item["relative_path"] for item in captured_seat["items"]},
                {"state.bin"},
            )

            os.mkfifo(seat / "runtime.pipe")
            blocked = fixture.capture("source")
            self.assertFalse(blocked["complete"])
            self.assertIn(
                str(seat / "runtime.pipe"),
                {item.get("path") for item in blocked["catalog"]["blockers"]},
            )

    def test_provider_policy_excludes_managed_links_and_retains_private_state(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            claude = fixture.source_home / ".claude"
            pi = fixture.source_home / ".pi/agent"
            managed = fixture.root / "managed"
            managed.mkdir()
            (managed / "skill.md").write_text("managed\n", encoding="utf-8")
            os.symlink(managed, claude / "skills")
            os.symlink(managed, pi / "tinyland")
            os.symlink(managed / "skill.md", pi / "AGENTS.md")
            os.symlink(managed / "skill.md", pi / "APPEND_SYSTEM.md")
            codex = fixture.source_home / ".codex"
            (codex / "AGENTS.md").write_text("managed\n", encoding="utf-8")
            (codex / "config.toml").write_text("managed=true\n", encoding="utf-8")
            os.symlink(managed / "skill.md", codex / "instructions.md")
            (claude / "rescue").mkdir()
            (claude / "rescue/state.weird").write_text("private\n", encoding="utf-8")
            os.symlink("rescue/state.weird", claude / "retained-link")
            (pi / "trust").mkdir()
            (pi / "trust/evidence.unknown").write_text("private\n", encoding="utf-8")
            fixture.managed_exclusions = [
                ("claude", "skills"),
                ("codex", "AGENTS.md"),
                ("codex", "config.toml"),
                ("codex", "instructions.md"),
                ("pi", "tinyland"),
                ("pi", "AGENTS.md"),
                ("pi", "APPEND_SYSTEM.md"),
            ]
            capture = fixture.capture("source")
            self.assertTrue(capture["complete"], capture["catalog"]["blockers"])
            providers = {item["name"]: item for item in capture["catalog"]["providers"]}
            claude_items = {
                item["relative_path"]: item for item in providers["claude"]["items"]
            }
            self.assertNotIn("skills", claude_items)
            self.assertEqual(
                claude_items["rescue/state.weird"]["classification"],
                "portable-private",
            )
            self.assertEqual(
                claude_items["retained-link"]["classification"], "portable-symlink"
            )
            self.assertNotIn(
                "tinyland",
                {item["relative_path"] for item in providers["pi"]["items"]},
            )
            self.assertTrue(
                {"AGENTS.md", "APPEND_SYSTEM.md"}.isdisjoint(
                    {item["relative_path"] for item in providers["pi"]["items"]}
                )
            )
            self.assertIn(
                "trust/evidence.unknown",
                {item["relative_path"] for item in providers["pi"]["items"]},
            )
            self.assertTrue(
                {"AGENTS.md", "config.toml", "instructions.md"}.isdisjoint(
                    {item["relative_path"] for item in providers["codex"]["items"]}
                )
            )
            with self.assertRaisesRegex(BulkloadError, "outside"):
                canonical_provider_policy([("claude", "projects/private")])
            with self.assertRaisesRegex(BulkloadError, "overlap"):
                canonical_provider_policy(
                    [("claude", "skills"), ("claude", "skills/private")]
                )

    def test_codex_claude_and_pi_typed_unions_include_wal_and_auth(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            codex = fixture.source_home / ".codex"
            claude = fixture.source_home / ".claude"
            pi = fixture.source_home / ".pi/agent"
            for relative in (
                "sessions/session.jsonl",
                "archived_sessions/archive.jsonl",
                "goals/goal.json",
                "memory/memory.json",
                "queue/item.json",
            ):
                path = codex / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text('{"value":1}\n', encoding="utf-8")
            for relative in (
                "projects/repo/session.jsonl",
                "memory/state.json",
            ):
                path = claude / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(
                    json.dumps({"cwd": str(fixture.source_repo)}) + "\n",
                    encoding="utf-8",
                )
            claude_auth = claude / ".credentials.json"
            claude_auth.write_text('{"token":"CLAUDE-SECRET"}', encoding="utf-8")
            claude_auth.chmod(0o600)
            for relative in (
                "sessions/session.jsonl",
                "state/state.json",
                "memory/memory.json",
                "queue/item.json",
            ):
                path = pi / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text('{"value":1}\n', encoding="utf-8")
            pi_auth = pi / "auth.json"
            pi_auth.write_text('{"token":"PI-SECRET"}', encoding="utf-8")
            pi_auth.chmod(0o600)
            database = codex / "state.sqlite"
            connection = sqlite3.connect(database)
            try:
                connection.execute("PRAGMA journal_mode=WAL")
                connection.execute("PRAGMA wal_autocheckpoint=0")
                connection.execute(
                    "CREATE TABLE state(id INTEGER PRIMARY KEY, value TEXT)"
                )
                connection.execute("INSERT INTO state VALUES (1, 'wal')")
                connection.commit()
                capture = fixture.capture("source")
            finally:
                connection.close()
            self.assertTrue(capture["complete"], capture["catalog"]["blockers"])
            providers = {
                provider["name"]: provider
                for provider in capture["catalog"]["providers"]
            }
            codex_classes = {
                item["relative_path"]: item["classification"]
                for item in providers["codex"]["items"]
            }
            self.assertEqual(codex_classes["auth.json"], "portable-auth")
            self.assertEqual(codex_classes["state.sqlite"], "sqlite")
            self.assertEqual(codex_classes["sessions/session.jsonl"], "append-jsonl")
            self.assertEqual(
                codex_classes["archived_sessions/archive.jsonl"], "append-jsonl"
            )
            for relative in (
                "goals/goal.json",
                "memory/memory.json",
                "queue/item.json",
            ):
                self.assertEqual(codex_classes[relative], "union-state")
            sqlite_item = next(
                item
                for item in providers["codex"]["items"]
                if item["classification"] == "sqlite"
            )
            self.assertIn("wal", {item["kind"] for item in sqlite_item["sidecars"]})
            claude_classes = {
                item["relative_path"]: item["classification"]
                for item in providers["claude"]["items"]
            }
            self.assertEqual(claude_classes[".credentials.json"], "nonportable-auth")
            self.assertEqual(
                claude_classes["projects/repo/session.jsonl"],
                "append-jsonl-rewrite",
            )
            pi_classes = {
                item["relative_path"]: item["classification"]
                for item in providers["pi"]["items"]
            }
            self.assertEqual(pi_classes["auth.json"], "portable-auth")
            self.assertEqual(pi_classes["sessions/session.jsonl"], "append-jsonl")
            for relative in (
                "state/state.json",
                "memory/memory.json",
                "queue/item.json",
            ):
                self.assertEqual(pi_classes[relative], "portable-state")


class PlannerTests(unittest.TestCase):
    def test_sequential_file_planner_binds_all_four_capture_ids(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = CutoverFixture(root, sqlite_union=False)
            captures = [
                fixture.capture("source"),
                fixture.capture("source"),
                fixture.capture("destination"),
                fixture.capture("destination"),
            ]
            paths = []
            for index, capture in enumerate(captures):
                path = root / f"capture-{index}.json"
                path.write_bytes(canonical_bytes(capture) + b"\n")
                paths.append(path)
            arguments = argparse.Namespace(
                source_a=str(paths[0]),
                source_b=str(paths[1]),
                destination_a=str(paths[2]),
                destination_b=str(paths[3]),
            )
            plan = _agent_plan(arguments)
            self.assertEqual(
                set(plan["source"]["capture_ids"] + plan["destination"]["capture_ids"]),
                {capture["capture_id"] for capture in captures},
            )

    def test_duplicate_provider_identity_returns_blocked_plan(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            sessions = fixture.source_home / ".codex/sessions"
            sessions.mkdir(parents=True)
            collision_id = "01234567-89ab-4cde-8fab-0123456789ab"
            for name in (f"{collision_id}-a.jsonl", f"copy-{collision_id}.jsonl"):
                (sessions / name).write_text(
                    json.dumps({"session_id": name}) + "\n", encoding="utf-8"
                )
            plan = fixture.plan()
            self.assertFalse(plan["ready"])
            self.assertIn(
                "agent-identity-collision",
                {item["code"] for item in plan["blockers"]},
            )

    def test_opaque_non_git_cannot_overwrite_typed_destination_git(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            source_opaque = fixture.source_git / "opaque"
            source_opaque.mkdir()
            (source_opaque / ".git").write_bytes(b"not git\n")
            (source_opaque / "payload.bin").write_bytes(b"source opaque")
            destination_git = fixture.destination_git / "opaque"
            initialize_repository(destination_git)

            plan = compile_agent_plan(
                fixture.capture("source"),
                fixture.capture("source"),
                fixture.capture("destination"),
                fixture.capture("destination"),
            )
            self.assertIn(
                {
                    "code": "non-git-git-workspace-collision",
                    "path": str(destination_git.resolve()),
                },
                plan["blockers"],
            )
            self.assertFalse(
                any(
                    Path(operation["destination_path"]).is_relative_to(destination_git)
                    for operation in plan["operations"]
                )
            )

    def test_plan_preserves_ref_divergence_with_recovery_ref(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            git(
                fixture.destination_repo,
                "config",
                "user.email",
                "bulkload@example.invalid",
            )
            git(fixture.destination_repo, "config", "user.name", "Bulkload Test")
            git(fixture.destination_repo, "config", "commit.gpgsign", "false")
            (fixture.destination_repo / "destination.txt").write_text(
                "destination\n", encoding="utf-8"
            )
            git(fixture.destination_repo, "add", "destination.txt")
            git(fixture.destination_repo, "commit", "-m", "destination divergence")
            plan = fixture.plan()
            self.assertTrue(plan["ready"], plan["blockers"])
            git_operation = next(
                item
                for item in plan["operations"]
                if item["kind"] == "git-workspace-union"
            )
            recovery = [
                item
                for item in git_operation["ref_actions"]
                if item["action"] == "anchor-destination-divergence"
            ]
            self.assertEqual(len(recovery), 1)
            self.assertTrue(
                recovery[0]["name"].startswith("refs/bulkload/recovery/destination/")
            )

    def test_sqlite_union_and_shared_conflict_are_source_authoritative(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary))
            plan = fixture.plan()
            self.assertTrue(plan["ready"], plan["blockers"])
            self.assertTrue(
                any(item["kind"] == "sqlite-union" for item in plan["operations"])
            )
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            create_database(
                fixture.source_home / ".codex/state.sqlite", [(1, "source")]
            )
            create_database(
                fixture.destination_home / ".codex/state.sqlite", [(1, "destination")]
            )
            plan = fixture.plan()
            self.assertTrue(plan["ready"], plan["blockers"])
            operation = next(
                item for item in plan["operations"] if item["kind"] == "sqlite-union"
            )
            self.assertIsNotNone(
                materialize_plan_operation(plan, operation)["destination_before"]
            )

    def test_append_divergence_is_source_authoritative(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            (fixture.destination_home / ".codex/history.jsonl").write_text(
                '{"session_id":"one","text":"fork"}\n', encoding="utf-8"
            )
            plan = fixture.plan()
            self.assertTrue(plan["ready"], plan["blockers"])
            operation = next(
                item
                for item in plan["operations"]
                if item["kind"] == "file-install"
                and item["destination_path"].endswith("history.jsonl")
            )
            self.assertIsNotNone(
                materialize_plan_operation(plan, operation)["destination_before"]
            )

    def test_claude_rewrite_and_nonportable_auth_hold(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            encoded_source = str(fixture.source_git).replace("/", "-").lstrip("-")
            project = fixture.source_home / ".claude/projects" / encoded_source
            project.mkdir(parents=True)
            (project / "session.jsonl").write_text(
                json.dumps({"cwd": str(fixture.source_repo)}) + "\n", encoding="utf-8"
            )
            claude_auth = fixture.source_home / ".claude/.credentials.json"
            claude_auth.write_text('{"token":"CLAUDE-SECRET"}', encoding="utf-8")
            claude_auth.chmod(0o600)
            plan = fixture.plan()
            self.assertTrue(plan["ready"], plan["blockers"])
            self.assertEqual(plan["holds"][0]["code"], "nonportable-auth")
            operation = next(
                item
                for item in plan["operations"]
                if item["kind"] == "file-install"
                and item["owner"]["name"] == "claude"
                and materialize_plan_operation(plan, item)["source"]["kind"]
                == "regular"
            )
            self.assertEqual(operation["transform"], "path-rewrite")
            self.assertNotIn("CLAUDE-SECRET", canonical_bytes(plan).decode())

    def test_destination_only_staged_index_is_source_authoritative(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            (fixture.source_repo / "tracked.txt").write_text("base\n", encoding="utf-8")
            (fixture.source_repo / "deleted.txt").write_text(
                "delete me\n", encoding="utf-8"
            )
            for name in ("untracked.txt", "intent.txt", "link"):
                (fixture.source_repo / name).unlink(missing_ok=True)
            git(fixture.source_repo, "read-tree", "HEAD")
            (fixture.destination_repo / "tracked.txt").write_text(
                "destination staged\n", encoding="utf-8"
            )
            git(fixture.destination_repo, "add", "tracked.txt")
            (fixture.destination_repo / "tracked.txt").write_text(
                "base\n", encoding="utf-8"
            )
            plan = fixture.plan()
            self.assertTrue(plan["ready"], plan["blockers"])
            operation = next(
                item
                for item in plan["operations"]
                if item["kind"] == "git-workspace-union"
            )
            self.assertIsNotNone(
                materialize_plan_operation(plan, operation)["destination_before"]
            )

    def test_structural_file_directory_collision_blocks_during_planning(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            (fixture.source_home / ".claude/collision").write_text(
                "source file\n", encoding="utf-8"
            )
            (fixture.destination_home / ".claude/collision").mkdir()
            plan = fixture.plan()
            self.assertFalse(plan["ready"])
            self.assertIn(
                "structural-type-collision",
                {item["code"] for item in plan["blockers"]},
            )

    def test_tampering_invalidates_plan(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            plan = CutoverFixture(Path(temporary), sqlite_union=False).plan()
            plan["holds"].append({"code": "invented", "path": "x"})
            with self.assertRaisesRegex(BulkloadError, "plan_sha256 mismatch"):
                require_digest(plan, "plan_sha256")


class TransactionTests(unittest.TestCase):
    def _require_reflink(self, root: Path) -> None:
        source = root / "reflink-source"
        destination = root / "reflink-destination"
        source.write_bytes(b"bulkload-reflink-probe")
        try:
            reflink_clone(source, destination)
        except BulkloadError as error:
            self.skipTest(f"test filesystem has no reflink support: {error}")

    def test_preseed_does_not_mutate_live_destination(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            self._require_reflink(fixture.root)
            plan = fixture.plan()
            before = {
                path.relative_to(fixture.destination_home).as_posix(): path.read_bytes()
                for path in fixture.destination_home.rglob("*")
                if path.is_file() and ".git/objects" not in path.as_posix()
            }
            receipt = stage_agent_plan(
                plan,
                accepted_plan_sha256=plan["plan_sha256"],
                phase="preseed",
                stage_root=fixture.stage,
                allow_accounted_copy=True,
                reserve_bytes=0,
            )
            after = {
                path.relative_to(fixture.destination_home).as_posix(): path.read_bytes()
                for path in fixture.destination_home.rglob("*")
                if path.is_file() and ".git/objects" not in path.as_posix()
            }
            self.assertEqual(before, after)
            self.assertFalse(receipt["ready_for_apply"])
            self.assertIn("manifest", receipt)
            self.assertNotIn("manifest_path", receipt)
            self.assertNotIn("schema", receipt["manifest"])

    def test_symlinked_install_roots_write_only_physical_backings(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            self._require_reflink(fixture.root)
            source_state = fixture.root / "source-state"
            destination_state = fixture.root / "destination-state"

            def relink(logical: Path, backing: Path) -> None:
                backing.parent.mkdir(parents=True, exist_ok=True)
                logical.rename(backing)
                os.symlink(backing, logical)

            source_codex = source_state / "codex"
            source_claude = source_state / "claude"
            destination_codex = destination_state / "codex"
            destination_claude = destination_state / "claude"
            relink(fixture.source_home / ".codex", source_codex)
            relink(fixture.source_home / ".claude", source_claude)
            relink(fixture.destination_home / ".codex", destination_codex)
            relink(fixture.destination_home / ".claude", destination_claude)
            binary = b"\x89PNG\r\n\x1a\n\xff\x00source"
            (source_claude / "agent-notes-rescue").mkdir()
            (source_claude / "agent-notes-rescue/image.bin").write_bytes(binary)
            destination_git_backing = fixture.root / "fast-local/git"
            relink(fixture.destination_git, destination_git_backing)
            source_gstack = source_state / "gstack"
            destination_gstack = destination_state / "gstack"
            source_gstack.mkdir(parents=True)
            destination_gstack.mkdir(parents=True)
            (source_gstack / "state.json").write_text("source\n", encoding="utf-8")
            os.symlink(source_gstack, fixture.source_home / ".gstack")
            os.symlink(destination_gstack, fixture.destination_home / ".gstack")
            fixture.source_seats.append(("gstack", fixture.source_home / ".gstack"))
            fixture.destination_seats.append(
                ("gstack", fixture.destination_home / ".gstack")
            )
            fixture.path_map = canonical_path_map(
                [
                    (str(fixture.source_home), str(fixture.destination_home)),
                    (str(fixture.source_git), str(destination_git_backing)),
                    (str(fixture.source_home / ".codex"), str(destination_codex)),
                    (str(fixture.source_home / ".claude"), str(destination_claude)),
                    (str(fixture.source_home / ".gstack"), str(destination_gstack)),
                ]
            )
            links_before = {
                path: os.readlink(path)
                for path in (
                    fixture.destination_git,
                    fixture.destination_home / ".codex",
                    fixture.destination_home / ".claude",
                    fixture.destination_home / ".gstack",
                )
            }
            plan = fixture.plan()
            self.assertTrue(plan["ready"], plan["blockers"])
            destination_catalog = plan["destination"]["catalog"]
            self.assertIsNotNone(destination_catalog["root_bindings"]["git_root_link"])
            codex = next(
                item
                for item in destination_catalog["providers"]
                if item["name"] == "codex"
            )
            self.assertEqual(
                codex["logical_path"], str(fixture.destination_home / ".codex")
            )
            self.assertEqual(codex["path"], str(destination_codex))
            _, final = fixture.stage_both(plan)
            applied = fixture.apply(plan, final)
            self.assertTrue(verify_agent_plan(plan, final, applied)["verified"])
            self.assertEqual(
                (destination_gstack / "state.json").read_text(), "source\n"
            )
            self.assertEqual(
                (destination_claude / "agent-notes-rescue/image.bin").read_bytes(),
                binary,
            )
            for path, target in links_before.items():
                self.assertTrue(path.is_symlink())
                self.assertEqual(os.readlink(path), target)

    def test_exact_file_seat_installs_and_absent_optional_is_receipted(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            self._require_reflink(fixture.root)
            bash_history = fixture.source_home / ".bash_history"
            bash_history.write_text("history\n", encoding="utf-8")
            fixture.source_seats.extend(
                [
                    ("bash-history", bash_history, "file"),
                    ("zsh-history", fixture.source_home / ".zsh_history", "file"),
                ]
            )
            fixture.destination_seats.extend(
                [
                    (
                        "bash-history",
                        fixture.destination_home / ".bash_history",
                        "file",
                    ),
                    (
                        "zsh-history",
                        fixture.destination_home / ".zsh_history",
                        "file",
                    ),
                ]
            )
            source = fixture.capture("source")
            seats = {item["name"]: item for item in source["catalog"]["seats"]}
            self.assertEqual(seats["bash-history"]["root_kind"], "file")
            self.assertFalse(seats["zsh-history"]["exists"])
            plan, final = fixture.staged()
            applied = fixture.apply(plan, final)
            self.assertTrue(verify_agent_plan(plan, final, applied)["verified"])
            self.assertEqual(
                (fixture.destination_home / ".bash_history").read_text(), "history\n"
            )
            with self.assertRaisesRegex(BulkloadError, "home root"):
                capture_agent_state(
                    role="source",
                    home=fixture.source_home,
                    git_root=fixture.source_git,
                    codex_root=None,
                    claude_root=None,
                    pi_root=None,
                    seats=[("whole-home", fixture.source_home)],
                    path_map=fixture.path_map,
                    writers_quiesced=True,
                    rsync_path=fixture.rsync_path,
                )

    def test_ssh_quarantine_crash_and_live_preseed_final_delta(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            self._require_reflink(fixture.root)
            preliminary = fixture.plan()
            fake_ssh = fixture.root / "fake-ssh"
            fake_ssh.write_text(
                """#!/usr/bin/env python3
import os
from pathlib import Path
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
marker = os.environ.get("BULKLOAD_TEST_TRANSPORT_FAIL_ONCE")
if marker and "--server" in arguments and not Path(marker).exists():
    Path(marker).write_text("failed", encoding="utf-8")
    print("SYNTHETIC-TRANSPORT-SECRET", file=sys.stderr)
    raise SystemExit(17)
os.execv(arguments[0], arguments)
""",
                encoding="utf-8",
            )
            fake_ssh.chmod(0o700)
            fixture.stage = fixture.root / "absent-parent/stage"
            prepare = stage_agent_plan(
                preliminary,
                accepted_plan_sha256=preliminary["plan_sha256"],
                phase="preseed",
                stage_root=fixture.stage,
                allow_accounted_copy=True,
                reserve_bytes=0,
                transport_mode="prepare",
            )
            self.assertEqual(stat.S_IMODE(fixture.stage.stat().st_mode), 0o700)
            self.assertEqual(
                stat.S_IMODE((fixture.stage / ".transport-quarantine").stat().st_mode),
                0o700,
            )
            tampered_prepare = copy.deepcopy(prepare)
            tampered_prepare["capacity"]["charged_bytes"] += 1
            with self.assertRaisesRegex(BulkloadError, "receipt_sha256 mismatch"):
                push_agent_transport(
                    tampered_prepare,
                    fixture.stage / ".transport-allowlist-preseed.nul",
                    accepted_plan_sha256=preliminary["plan_sha256"],
                    phase="preseed",
                    stage_root=fixture.stage,
                    destination_ssh_host=socket.gethostname(),
                    _ssh_binary=str(fake_ssh),
                )
            allowlist_path = fixture.stage / ".transport-allowlist-preseed.nul"
            allowlist_payload = allowlist_path.read_bytes()
            allowlist_path.write_bytes(allowlist_payload + b"tampered\0")
            with self.assertRaisesRegex(BulkloadError, "custody|digest"):
                push_agent_transport(
                    prepare,
                    allowlist_path,
                    accepted_plan_sha256=preliminary["plan_sha256"],
                    phase="preseed",
                    stage_root=fixture.stage,
                    destination_ssh_host=socket.gethostname(),
                    _ssh_binary=str(fake_ssh),
                )
            allowlist_path.write_bytes(allowlist_payload)
            allowlist_path.chmod(0o600)
            marker = fixture.root / "transport-failed"
            destination_before = (fixture.destination_repo / "tracked.txt").read_bytes()
            with mock.patch.dict(
                os.environ,
                {"BULKLOAD_TEST_TRANSPORT_FAIL_ONCE": str(marker)},
                clear=False,
            ):
                with self.assertRaises(BulkloadError) as failure:
                    push_agent_transport(
                        prepare,
                        fixture.stage / ".transport-allowlist-preseed.nul",
                        accepted_plan_sha256=preliminary["plan_sha256"],
                        phase="preseed",
                        stage_root=fixture.stage,
                        destination_ssh_host=socket.gethostname(),
                        _ssh_binary=str(fake_ssh),
                    )
            self.assertNotIn("SYNTHETIC-TRANSPORT-SECRET", str(failure.exception))
            self.assertFalse((fixture.stage / "receipt-preseed.json").exists())
            self.assertEqual(
                (fixture.destination_repo / "tracked.txt").read_bytes(),
                destination_before,
            )
            (fixture.source_repo / "tracked.txt").write_text(
                "late before preseed\n", encoding="utf-8"
            )
            unplanned = fixture.source_home / "not-in-accepted-plan.txt"
            unplanned.write_text("must not enter quarantine\n", encoding="utf-8")

            def mutate_after_snapshot(
                source: object, snapshot: object, **arguments: object
            ) -> None:
                _snapshot_allowlist_stream(source, snapshot, **arguments)
                relative = os.fspath(unplanned).removeprefix("/")
                allowlist_path.write_bytes(
                    allowlist_payload + os.fsencode(relative) + b"\0"
                )

            with mock.patch(
                "bulkload_lib.executor._snapshot_allowlist_stream",
                side_effect=mutate_after_snapshot,
            ):
                preseed_transport = push_agent_transport(
                    prepare,
                    allowlist_path,
                    accepted_plan_sha256=preliminary["plan_sha256"],
                    phase="preseed",
                    stage_root=fixture.stage,
                    destination_ssh_host=socket.gethostname(),
                    _ssh_binary=str(fake_ssh),
                )
            quarantine = fixture.stage / ".transport-quarantine"
            self.assertFalse(quarantine.joinpath(*unplanned.parts[1:]).exists())
            allowlist_path.write_bytes(allowlist_payload)
            allowlist_path.chmod(0o600)
            quarantine.chmod(0o755)
            with self.assertRaisesRegex(BulkloadError, "custody mode"):
                stage_agent_plan(
                    preliminary,
                    accepted_plan_sha256=preliminary["plan_sha256"],
                    phase="preseed",
                    stage_root=fixture.stage,
                    allow_accounted_copy=True,
                    reserve_bytes=0,
                    transport_mode="materialize",
                    prepare_receipt=prepare,
                    transport_receipt=preseed_transport,
                )
            quarantine.chmod(0o700)
            preseed = stage_agent_plan(
                preliminary,
                accepted_plan_sha256=preliminary["plan_sha256"],
                phase="preseed",
                stage_root=fixture.stage,
                allow_accounted_copy=True,
                reserve_bytes=0,
                transport_mode="materialize",
                prepare_receipt=prepare,
                transport_receipt=preseed_transport,
            )
            self.assertGreater(preseed["materialization"]["deferred_operations"], 0)
            (fixture.source_repo / "tracked.txt").write_text(
                "fresh final bytes\n", encoding="utf-8"
            )
            final_plan = fixture.plan()
            self.assertNotEqual(final_plan["plan_sha256"], preliminary["plan_sha256"])
            final_prepare = stage_agent_plan(
                final_plan,
                accepted_plan_sha256=final_plan["plan_sha256"],
                phase="final",
                stage_root=fixture.stage,
                allow_accounted_copy=True,
                reserve_bytes=0,
                transport_mode="prepare",
            )
            final_transport = push_agent_transport(
                final_prepare,
                fixture.stage / ".transport-allowlist-final.nul",
                accepted_plan_sha256=final_plan["plan_sha256"],
                phase="final",
                stage_root=fixture.stage,
                destination_ssh_host=socket.gethostname(),
                _ssh_binary=str(fake_ssh),
            )
            final = stage_agent_plan(
                final_plan,
                accepted_plan_sha256=final_plan["plan_sha256"],
                phase="final",
                stage_root=fixture.stage,
                allow_accounted_copy=True,
                reserve_bytes=0,
                transport_mode="materialize",
                prepare_receipt=final_prepare,
                transport_receipt=final_transport,
            )
            self.assertEqual(final["plan_sha256"], final_plan["plan_sha256"])
            self.assertEqual(final["transport"]["mode"], "ssh-rsync-quarantine")
            self.assertGreater(final["materialization"]["reused_object_bytes"], 0)
            applied = fixture.apply(final_plan, final)
            verified = verify_agent_plan(final_plan, final, applied)
            self.assertTrue(verified["verified"], verified["failures"])
            self.assertEqual(
                (fixture.destination_repo / "tracked.txt").read_text(),
                "fresh final bytes\n",
            )

    def test_transport_rejects_unsafe_stage_root_before_subprocess(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            plan = fixture.plan()
            prepare = stage_agent_plan(
                plan,
                accepted_plan_sha256=plan["plan_sha256"],
                phase="preseed",
                stage_root=fixture.stage,
                allow_accounted_copy=True,
                reserve_bytes=0,
                transport_mode="prepare",
            )
            with mock.patch("bulkload_lib.executor.subprocess.run") as invoked:
                for value in (
                    "/srv/fast-local/jess/bad path",
                    "/srv/fast-local/jess/bad;touch",
                    "/srv/fast-local/jess/../escape",
                    "/srv/fast-local/jess/$(touch)",
                    "/srv/fast-local/jess/bad\npath",
                    "/srv/fast-local/jess/bad:path",
                ):
                    with (
                        self.subTest(value=value),
                        self.assertRaisesRegex(BulkloadError, "remote-safe"),
                    ):
                        push_agent_transport(
                            prepare,
                            fixture.stage / ".transport-allowlist-preseed.nul",
                            accepted_plan_sha256=plan["plan_sha256"],
                            phase="preseed",
                            stage_root=Path(value),
                            destination_ssh_host="untrusted",
                        )
                invoked.assert_not_called()
            with mock.patch("bulkload_lib.executor.subprocess.run") as invoked:
                with self.assertRaisesRegex(BulkloadError, "shell-safe"):
                    push_agent_transport(
                        prepare,
                        fixture.stage / ".transport-allowlist-preseed.nul",
                        accepted_plan_sha256=plan["plan_sha256"],
                        phase="preseed",
                        stage_root=fixture.stage,
                        destination_ssh_host=socket.gethostname(),
                        _ssh_binary="/tmp/ssh`touch`",
                    )
                invoked.assert_not_called()

    def test_apply_verify_rollback_are_exact_and_idempotent(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary))
            self._require_reflink(fixture.root)
            plan, final = fixture.staged()
            apply_receipt = fixture.apply(plan, final)
            repeated = fixture.apply(plan, final)
            self.assertEqual(repeated, apply_receipt)
            verification = verify_agent_plan(plan, final, apply_receipt)
            self.assertTrue(verification["verified"], verification["failures"])
            self.assertEqual(
                (fixture.destination_repo / "tracked.txt").read_text(), "source dirt\n"
            )
            self.assertEqual(
                (fixture.destination_repo / "untracked.txt").read_text(),
                "source only\n",
            )
            self.assertFalse((fixture.destination_repo / "deleted.txt").exists())
            self.assertEqual(
                (fixture.destination_home / ".codex/auth.json").read_text(),
                '{"token":"SOURCE-SECRET"}',
            )
            connection = sqlite3.connect(
                fixture.destination_home / ".codex/state.sqlite"
            )
            try:
                self.assertEqual(
                    connection.execute("SELECT * FROM state ORDER BY id").fetchall(),
                    [(1, "shared"), (2, "source"), (3, "destination")],
                )
            finally:
                connection.close()
            serialized = canonical_bytes(
                {"plan": plan, "apply": apply_receipt, "verify": verification}
            ).decode()
            self.assertNotIn("SOURCE-SECRET", serialized)
            self.assertNotIn("DESTINATION-SECRET", serialized)
            rollback = fixture.roll_back(apply_receipt)
            repeated_rollback = fixture.roll_back(apply_receipt)
            self.assertEqual(rollback, repeated_rollback)
            self.assertEqual(
                (fixture.destination_home / ".codex/auth.json").read_text(),
                '{"token":"DESTINATION-SECRET"}',
            )
            self.assertFalse((fixture.destination_repo / "untracked.txt").exists())

    def test_source_authority_replaces_divergence_and_rollback_restores_it(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            self._require_reflink(fixture.root)
            destination_tracked = fixture.destination_repo / "tracked.txt"
            destination_tracked.write_text("destination dirt\n", encoding="utf-8")
            git(fixture.destination_repo, "add", "tracked.txt")
            destination_only = fixture.destination_repo / "destination-only.txt"
            destination_only.write_text("destination only\n", encoding="utf-8")
            destination_only_directories = [
                fixture.destination_repo / "destination-only-dir",
                fixture.destination_repo / "destination-only-dir/nested",
            ]
            destination_only_directories[-1].mkdir(parents=True)
            destination_only_directories[0].chmod(0o750)
            destination_only_directories[1].chmod(0o710)
            source_non_git = fixture.source_git / "fleet-note.txt"
            destination_non_git = fixture.destination_git / "fleet-note.txt"
            destination_non_git.write_text("destination fleet\n", encoding="utf-8")
            source_provider = fixture.source_home / ".claude/rescue/private.bin"
            destination_provider = (
                fixture.destination_home / ".claude/rescue/private.bin"
            )
            source_provider.parent.mkdir(parents=True)
            destination_provider.parent.mkdir(parents=True)
            source_provider.write_bytes(b"\x00source-private\xff")
            destination_provider.write_bytes(b"\x00destination-private\xfe")
            source_history = fixture.source_home / ".codex/history.jsonl"
            destination_history = fixture.destination_home / ".codex/history.jsonl"
            destination_history.write_text(
                '{"session_id":"one","text":"destination fork"}\n',
                encoding="utf-8",
            )
            source_mode_history = fixture.source_home / ".codex/sessions/mode.jsonl"
            destination_mode_history = (
                fixture.destination_home / ".codex/sessions/mode.jsonl"
            )
            source_mode_history.parent.mkdir()
            destination_mode_history.parent.mkdir()
            source_mode_history.write_text('{"value":1}\n', encoding="utf-8")
            destination_mode_history.write_text('{"value":1}\n', encoding="utf-8")
            source_mode_history.chmod(0o600)
            destination_mode_history.chmod(0o640)
            source_database = fixture.source_home / ".codex/state.sqlite"
            destination_database = fixture.destination_home / ".codex/state.sqlite"
            create_database(source_database, [(1, "source row")])
            create_database(destination_database, [(1, "destination row")])
            destination_index = fixture.destination_repo / ".git/index"
            before = {
                "tracked": destination_tracked.read_bytes(),
                "destination_only": destination_only.read_bytes(),
                "non_git": destination_non_git.read_bytes(),
                "provider": destination_provider.read_bytes(),
                "history": destination_history.read_bytes(),
                "database": destination_database.read_bytes(),
                "index": destination_index.read_bytes(),
                "mode_history": stat.S_IMODE(destination_mode_history.stat().st_mode),
                "directory_modes": [
                    stat.S_IMODE(path.stat().st_mode)
                    for path in destination_only_directories
                ],
            }
            plan = fixture.plan()
            self.assertTrue(plan["ready"], plan["blockers"])
            _, final = fixture.stage_both(plan)
            applied = fixture.apply(plan, final)
            verified = verify_agent_plan(plan, final, applied)
            self.assertTrue(verified["verified"], verified["failures"])
            self.assertEqual(destination_tracked.read_text(), "source dirt\n")
            self.assertFalse(destination_only.exists())
            self.assertFalse(destination_only_directories[0].exists())
            self.assertEqual(
                destination_non_git.read_bytes(), source_non_git.read_bytes()
            )
            self.assertEqual(
                destination_provider.read_bytes(), source_provider.read_bytes()
            )
            self.assertEqual(
                destination_history.read_bytes(), source_history.read_bytes()
            )
            self.assertEqual(
                stat.S_IMODE(destination_mode_history.stat().st_mode), 0o600
            )
            connection = sqlite3.connect(destination_database)
            try:
                self.assertEqual(
                    connection.execute("SELECT * FROM state").fetchall(),
                    [(1, "source row")],
                )
            finally:
                connection.close()
            fixture.roll_back(applied)
            self.assertEqual(destination_tracked.read_bytes(), before["tracked"])
            self.assertEqual(destination_only.read_bytes(), before["destination_only"])
            self.assertEqual(destination_non_git.read_bytes(), before["non_git"])
            self.assertEqual(destination_provider.read_bytes(), before["provider"])
            self.assertEqual(destination_history.read_bytes(), before["history"])
            self.assertEqual(destination_database.read_bytes(), before["database"])
            self.assertEqual(destination_index.read_bytes(), before["index"])
            self.assertEqual(
                stat.S_IMODE(destination_mode_history.stat().st_mode),
                before["mode_history"],
            )
            self.assertEqual(
                [
                    stat.S_IMODE(path.stat().st_mode)
                    for path in destination_only_directories
                ],
                before["directory_modes"],
            )

    def test_rollback_removes_transaction_created_provider_parents(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            self._require_reflink(fixture.root)
            destination_root = fixture.destination_home / ".claude"
            destination_root.rmdir()
            source = fixture.source_home / ".claude/new/deep/state.bin"
            source.parent.mkdir(parents=True)
            source.write_bytes(b"source-only")
            plan = fixture.plan()
            self.assertTrue(plan["ready"], plan["blockers"])
            _, final = fixture.stage_both(plan)
            applied = fixture.apply(plan, final)
            self.assertTrue(verify_agent_plan(plan, final, applied)["verified"])
            self.assertTrue(destination_root.is_dir())
            with mock.patch(
                "bulkload_lib.executor.fsync_directory", wraps=fsync_directory
            ) as fsynced:
                fixture.roll_back(applied)
            self.assertIn(mock.call(destination_root.parent), fsynced.call_args_list)
            self.assertFalse(destination_root.exists())

    def test_rollback_removes_created_git_root_before_its_parent(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            self._require_reflink(fixture.root)
            initialize_repository(fixture.source_git / "new-parent/new-repository")
            destination_parent = fixture.destination_git / "new-parent"
            plan = fixture.plan()
            self.assertTrue(plan["ready"], plan["blockers"])
            _, final = fixture.stage_both(plan)
            applied = fixture.apply(plan, final)
            self.assertTrue(verify_agent_plan(plan, final, applied)["verified"])
            self.assertTrue(destination_parent.is_dir())
            fixture.roll_back(applied)
            self.assertFalse(destination_parent.exists())

        for target_kind in ("primary", "worktree"):
            with (
                self.subTest(target_kind=target_kind),
                tempfile.TemporaryDirectory() as temporary,
            ):
                fixture = CutoverFixture(Path(temporary), sqlite_union=False)
                self._require_reflink(fixture.root)
                if target_kind == "primary":
                    initialize_repository(fixture.source_git / "new-repository")
                    target = fixture.destination_git / "new-repository"
                else:
                    add_linked_worktree(fixture.source_repo, fixture.source_git)
                    target = fixture.destination_git / "linked"
                plan, final = fixture.staged()
                before_refs = ref_inventory(fixture.destination_repo)
                target.mkdir()
                target.chmod(0o710)
                with self.assertRaisesRegex(BulkloadError, "already exists"):
                    fixture.apply(plan, final)
                self.assertEqual(list(target.iterdir()), [])
                self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o710)
                self.assertEqual(ref_inventory(fixture.destination_repo), before_refs)

    def test_nullable_primary_key_duplicates_use_source_snapshot(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            self._require_reflink(fixture.root)

            def create(path: Path, rows: list[tuple[None, str]]) -> None:
                connection = sqlite3.connect(path)
                try:
                    connection.execute(
                        "CREATE TABLE state(id TEXT PRIMARY KEY, value TEXT NOT NULL)"
                    )
                    connection.executemany("INSERT INTO state VALUES (?,?)", rows)
                    connection.commit()
                finally:
                    connection.close()

            source = fixture.source_home / ".codex/state.sqlite"
            destination = fixture.destination_home / ".codex/state.sqlite"
            create(source, [(None, "source-a"), (None, "source-b")])
            create(destination, [])
            plan = fixture.plan()
            self.assertTrue(plan["ready"], plan["blockers"])
            _, final = fixture.stage_both(plan)
            applied = fixture.apply(plan, final)
            self.assertTrue(verify_agent_plan(plan, final, applied)["verified"])
            connection = sqlite3.connect(destination)
            try:
                self.assertEqual(
                    connection.execute("SELECT * FROM state ORDER BY value").fetchall(),
                    [(None, "source-a"), (None, "source-b")],
                )
            finally:
                connection.close()

    def test_linked_detached_refs_anchors_and_objects_rollback_exactly(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            self._require_reflink(fixture.root)
            detached = fixture.source_git / "detached"
            linked = add_linked_worktree(
                fixture.source_repo, fixture.source_git, locked=True
            )
            (linked / "tracked.txt").write_text("stash payload\n", encoding="utf-8")
            git(linked, "stash", "push", "-m", "cutover-stash")
            (linked / "tracked.txt").write_text("linked dirt\n", encoding="utf-8")
            (linked / "linked-only.txt").write_text("linked only\n", encoding="utf-8")
            git(
                fixture.source_repo,
                "worktree",
                "add",
                "--detach",
                "--no-checkout",
                str(detached),
                "HEAD",
            )
            git(detached, "reset", "--hard", "HEAD")
            (detached / "tracked.txt").write_text("detached dirt\n", encoding="utf-8")
            git(fixture.source_repo, "tag", "cutover-tag")
            git(fixture.source_repo, "notes", "add", "-m", "cutover-note", "HEAD")
            refs_before = ref_inventory(fixture.destination_repo)
            objects_before = object_inventory(fixture.destination_repo)
            reflogs_before = reflog_inventory(fixture.destination_repo)
            plan = fixture.plan()
            self.assertTrue(plan["ready"], plan["blockers"])
            _, final = fixture.stage_both(plan)
            applied = fixture.apply(plan, final)
            verified = verify_agent_plan(plan, final, applied)
            self.assertTrue(verified["verified"], verified["failures"])
            destination_linked = fixture.destination_git / "linked"
            destination_detached = fixture.destination_git / "detached"
            self.assertTrue(destination_linked.exists())
            self.assertTrue(destination_detached.exists())
            destination_linked_git_dir = Path(
                git(
                    destination_linked,
                    "rev-parse",
                    "--path-format=absolute",
                    "--git-dir",
                )
                .decode()
                .strip()
            )
            self.assertTrue((destination_linked_git_dir / "locked").exists())
            for name in (
                "refs/stash",
                "refs/tags/cutover-tag",
                "refs/notes/commits",
            ):
                self.assertTrue(
                    git(fixture.destination_repo, "rev-parse", "--verify", name)
                )
            fixture.roll_back(applied)
            self.assertFalse(destination_linked.exists())
            self.assertFalse(destination_detached.exists())
            self.assertEqual(ref_inventory(fixture.destination_repo), refs_before)
            self.assertEqual(object_inventory(fixture.destination_repo), objects_before)
            self.assertEqual(reflog_inventory(fixture.destination_repo), reflogs_before)

    def test_sqlite_bag_union_preserves_max_duplicate_multiplicity(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            self._require_reflink(fixture.root)
            source_db = fixture.source_home / ".codex/state.sqlite"
            destination_db = fixture.destination_home / ".codex/state.sqlite"
            create_bag_database(source_db, ["shared", "shared", "source"])
            create_bag_database(destination_db, ["shared", "destination"])
            plan = fixture.plan()
            self.assertTrue(plan["ready"], plan["blockers"])
            _, final = fixture.stage_both(plan)
            applied = fixture.apply(plan, final)
            verified = verify_agent_plan(plan, final, applied)
            self.assertTrue(verified["verified"], verified["failures"])
            connection = sqlite3.connect(destination_db)
            try:
                rows = connection.execute(
                    "SELECT value FROM state ORDER BY value"
                ).fetchall()
            finally:
                connection.close()
            self.assertEqual(
                rows,
                [("destination",), ("shared",), ("shared",), ("source",)],
            )

    def test_mutable_seat_state_applies_and_rolls_back(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            self._require_reflink(fixture.root)
            source_seat = fixture.source_home / "seat-state"
            destination_seat = fixture.destination_home / "seat-state"
            source_seat.mkdir()
            destination_seat.mkdir()
            (source_seat / "window.json").write_text(
                '{"display":"source"}\n', encoding="utf-8"
            )
            (destination_seat / "window.json").write_text(
                '{"display":"destination"}\n', encoding="utf-8"
            )
            (destination_seat / "destination-only.json").write_text(
                '{"retain":true}\n', encoding="utf-8"
            )
            fixture.source_seats = [("desktop", source_seat)]
            fixture.destination_seats = [("desktop", destination_seat)]
            plan = fixture.plan()
            self.assertTrue(plan["ready"], plan["blockers"])
            _, final = fixture.stage_both(plan)
            applied = fixture.apply(plan, final)
            self.assertEqual(
                (destination_seat / "window.json").read_text(),
                '{"display":"source"}\n',
            )
            self.assertTrue((destination_seat / "destination-only.json").exists())
            fixture.roll_back(applied)
            self.assertEqual(
                (destination_seat / "window.json").read_text(),
                '{"display":"destination"}\n',
            )

    def test_crash_recovery_forward(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            self._require_reflink(fixture.root)
            plan, final = fixture.staged()
            with mock.patch.dict(os.environ, {"BULKLOAD_TEST_CRASH_AFTER": "1"}):
                with self.assertRaisesRegex(BulkloadError, "injected crash"):
                    fixture.apply(plan, final)
            recovery = fixture.recover(plan, final, "forward")
            self.assertEqual(recovery["transaction_state"], "applied")
            journal = json.loads(fixture.journal.read_text())
            self.assertEqual(journal["state"], "applied")

    def test_crash_recovery_rollback(self) -> None:
        for boundary in ("ref", "head", "lock", "head-lock"):
            with (
                self.subTest(boundary=boundary),
                tempfile.TemporaryDirectory() as temporary,
            ):
                fixture = CutoverFixture(Path(temporary), sqlite_union=False)
                self._require_reflink(fixture.root)
                hybrid_path = None
                if boundary == "ref":
                    git(fixture.source_repo, "add", "-A")
                    git(fixture.source_repo, "commit", "-m", "source ref")
                elif boundary == "head":
                    git(fixture.source_repo, "checkout", "--detach", "HEAD")
                elif boundary == "lock":
                    add_linked_worktree(
                        fixture.source_repo, fixture.source_git, locked=True
                    )
                    add_linked_worktree(
                        fixture.destination_repo, fixture.destination_git
                    )
                else:
                    source_linked = add_linked_worktree(
                        fixture.source_repo, fixture.source_git
                    )
                    hybrid_path = add_linked_worktree(
                        fixture.destination_repo, fixture.destination_git
                    )
                    git(source_linked, "checkout", "--detach", "HEAD")
                    git(
                        fixture.source_repo,
                        "worktree",
                        "lock",
                        str(source_linked),
                    )
                before = git_transaction_inventory(fixture.destination_repo)
                plan, final = fixture.staged()
                crash_boundary = "head" if boundary == "head-lock" else boundary
                with mock.patch.dict(
                    os.environ, {"BULKLOAD_TEST_CRASH_GIT_AFTER": crash_boundary}
                ):
                    with self.assertRaisesRegex(BulkloadError, f"Git {crash_boundary}"):
                        fixture.apply(plan, final)
                journal = json.loads(fixture.journal.read_text())
                self.assertFalse(journal["git_prepared"])
                self.assertTrue(journal["git_refs_after"])
                self.assertTrue(journal["git_worktrees_after"])
                if hybrid_path is not None:
                    repository = str(fixture.destination_repo)
                    path = str(hybrid_path)
                    before_state = journal["git_worktrees_before"][repository][path]
                    after_state = journal["git_worktrees_after"][repository][path]
                    expected = {
                        key: after_state[key]
                        if key in {"branch", "detached", "head"}
                        else value
                        for key, value in before_state.items()
                    }
                    current = _git_worktree_state(hybrid_path)
                    self.assertEqual(current, expected)
                    self.assertNotEqual(current, before_state)
                    self.assertNotEqual(current, after_state)
                fixture.recover(plan, final, "rollback")
                self.assertEqual(
                    git_transaction_inventory(fixture.destination_repo), before
                )
                git(fixture.destination_repo, "fsck", "--full", "--no-dangling")

        with (
            self.subTest(boundary="ref-delete-failure"),
            tempfile.TemporaryDirectory() as temporary,
        ):
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            self._require_reflink(fixture.root)
            git(fixture.source_repo, "add", "-A")
            git(fixture.source_repo, "commit", "-m", "source ref")
            before = git_transaction_inventory(fixture.destination_repo)
            plan, final = fixture.staged()
            applied = fixture.apply(plan, final)
            journal = json.loads(fixture.journal.read_text())
            repository = str(fixture.destination_repo)
            created_ref = next(
                name
                for name, state in journal["git_refs_before"][repository].items()
                if state is None
            )

            def keep_created_ref(
                repository_path: Path, arguments: list[str], *, check: bool = True
            ) -> bytes:
                if arguments[:3] == ["update-ref", "--no-deref", "-d"] and (
                    arguments[3] == created_ref
                ):
                    return b""
                return _git(repository_path, arguments, check=check)

            with mock.patch("bulkload_lib.executor._git", side_effect=keep_created_ref):
                with self.assertRaisesRegex(
                    BulkloadError, "Git ref rollback did not reach"
                ):
                    fixture.roll_back(applied)
            failed = json.loads(fixture.journal.read_text())
            self.assertEqual(failed["state"], "rolling-back")
            self.assertIsNone(failed.get("rollback_receipt"))
            self.assertLess(failed["rollback_progress"], len(_rollback_steps(failed)))
            self.assertTrue(
                git(fixture.destination_repo, "rev-parse", "--verify", created_ref)
            )
            fixture.roll_back(applied)
            self.assertEqual(
                git_transaction_inventory(fixture.destination_repo), before
            )
            git(fixture.destination_repo, "fsck", "--full", "--no-dangling")

    def test_snapshot_crash_resumes_forward(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            self._require_reflink(fixture.root)
            plan, final = fixture.staged()
            with mock.patch.dict(
                os.environ, {"BULKLOAD_TEST_CRASH_AFTER_SNAPSHOT": "1"}
            ):
                with self.assertRaisesRegex(BulkloadError, "rollback snapshot"):
                    fixture.apply(plan, final)
            recovered = fixture.recover(plan, final, "forward")
            self.assertEqual(recovered["transaction_state"], "applied")

    def test_rollback_crash_resumes_and_recovery_receipt_is_idempotent(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            fixture = CutoverFixture(Path(temporary), sqlite_union=False)
            self._require_reflink(fixture.root)
            plan, final = fixture.staged()
            applied = fixture.apply(plan, final)
            with mock.patch.dict(
                os.environ, {"BULKLOAD_TEST_CRASH_ROLLBACK_AFTER": "1"}
            ):
                with self.assertRaisesRegex(BulkloadError, "rollback boundary"):
                    fixture.roll_back(applied)
            recovered = fixture.recover(plan, final, "rollback")
            repeated = fixture.recover(plan, final, "rollback")
            self.assertEqual(recovered, repeated)
            self.assertEqual(recovered["transaction_state"], "rolled-back")
            self.assertEqual(
                (fixture.destination_home / ".codex/auth.json").read_text(),
                '{"token":"DESTINATION-SECRET"}',
            )

    def test_required_reflink_never_falls_back(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            source = root / "source"
            source.write_bytes(b"content")
            with (
                mock.patch(
                    "bulkload_lib.model._darwin_clonefile",
                    side_effect=OSError("no clone"),
                ),
                mock.patch(
                    "bulkload_lib.model._linux_reflink", side_effect=OSError("no clone")
                ),
            ):
                with self.assertRaisesRegex(
                    BulkloadError, "full-copy fallback is forbidden"
                ):
                    reflink_clone(source, root / "destination")
            self.assertFalse((root / "destination").exists())

    def test_capacity_gate_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            observation = os.statvfs(temporary)
            total = observation.f_blocks * observation.f_frsize
            with self.assertRaisesRegex(BulkloadError, "capacity gate failed"):
                require_capacity(
                    Path(temporary), charged_bytes=total + 1, reserve_bytes=0
                )

    def test_staged_symlink_mode_is_exempt_across_kernels(self) -> None:
        """A staged symlink verifies on target bytes, never on mode.

        darwin lstat reports the creating umask's bits while Linux fixes every
        symlink at 0777 and has no lchmod, so a recorded mode can never be
        reproduced on the other kernel. The exemption is narrow: it does not
        reach a regular file, and it does not weaken the target-bytes identity.
        """
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            link = root / "link"
            os.symlink("target.txt", link)
            live_mode = stat.S_IMODE(link.stat(follow_symlinks=False).st_mode)
            # The mode the *other* kernel would have recorded for this link.
            foreign_mode = 0o755 if live_mode == 0o777 else 0o777
            self.assertNotEqual(live_mode, foreign_mode)
            record = {
                "kind": "symlink",
                "sha256": sha256_symlink(link),
                "mode": f"{foreign_mode:04o}",
                "size": len(os.fsencode(os.readlink(link))),
            }
            _verify_record(link, record)

            # The target bytes still bind the link.
            link.unlink()
            os.symlink("other.txt", link)
            with self.assertRaisesRegex(
                _StageSourceChanged, "symbolic link changed before staging"
            ):
                _verify_record(link, record)

            # A regular file's mode is still enforced.
            regular = root / "regular.txt"
            regular.write_bytes(b"content")
            os.chmod(regular, 0o600)
            with self.assertRaisesRegex(
                _StageSourceChanged, "state mode changed before staging"
            ):
                _verify_record(
                    regular,
                    {
                        "kind": "regular",
                        "sha256": sha256_bytes(b"content"),
                        "mode": "0644",
                        "size": 7,
                    },
                )

    def test_staged_symlink_without_a_mode_still_refuses(self) -> None:
        """The exemption drops the comparison, never the required field.

        A record that declares no mode must still fail closed here rather than
        slip through and reach _materialize_file's record["mode"] as a bare
        KeyError instead of a protocol refusal.
        """
        with tempfile.TemporaryDirectory() as temporary:
            link = Path(temporary).resolve() / "link"
            os.symlink("target.txt", link)
            with self.assertRaisesRegex(
                _StageSourceChanged, "state mode changed before staging"
            ):
                _verify_record(
                    link,
                    {
                        "kind": "symlink",
                        "sha256": sha256_symlink(link),
                        "size": len(os.fsencode(os.readlink(link))),
                    },
                )

    def test_installed_symlink_verifies_against_a_foreign_recorded_mode(self) -> None:
        """The destination gates must accept the other kernel's symlink mode.

        _materialize_file carries the source record's mode into the manifest
        entry verbatim, and _atomic_install_blob installs a link with a bare
        os.symlink and never chmods it, so on a cross-kernel lane the live
        destination mode is *always* the destination kernel's and *never* the
        entry's. Both destination gates compare through _same_record: the file
        branch of verify_agent_apply, which emits "file-verification-failed",
        and the Git worktree file loop, which emits "git-worktree-byte-
        mismatch". This exercises the real install -> observe -> compare
        round-trip; it is red without the _same_record exemption.
        """
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            stage_root = root / "stage"
            stage_root.mkdir(mode=0o700)
            destination = root / "destination"
            destination.mkdir()

            payload = os.fsencode("target.txt")
            digest = sha256_bytes(payload)
            _write_object_bytes(stage_root, digest, payload, 0o600)

            probe = root / "probe"
            os.symlink("target.txt", probe)
            live_mode = stat.S_IMODE(probe.stat(follow_symlinks=False).st_mode)
            # The mode the *other* kernel would have recorded for this link.
            foreign_mode = 0o755 if live_mode == 0o777 else 0o777
            self.assertNotEqual(live_mode, foreign_mode)

            entry = {
                "blob_sha256": digest,
                "kind": "symlink",
                "mode": f"{foreign_mode:04o}",
                "payload_kind": "symlink",
                "size": len(payload),
            }
            target = destination / "link"
            after = _atomic_install_blob(stage_root, entry, target)

            # The installed link really does carry the destination kernel's
            # mode, not the entry's — otherwise this test proves nothing.
            self.assertEqual(after["kind"], "symlink")
            self.assertNotEqual(after["mode"], entry["mode"])

            expected = {
                "kind": entry["payload_kind"],
                "mode": entry["mode"],
                "sha256": entry["blob_sha256"],
                "size": entry["size"],
            }
            # verify_agent_apply's file branch.
            self.assertTrue(_same_record(_current_record(target), expected))
            # The Git worktree file loop builds the same shape from "kind".
            self.assertTrue(
                _same_record(
                    _current_record(target),
                    {
                        "kind": entry["kind"],
                        "mode": entry["mode"],
                        "sha256": entry["blob_sha256"],
                        "size": entry["size"],
                    },
                )
            )

            # The target bytes still bind the link.
            retargeted = destination / "retargeted"
            os.symlink("elsewhere.txt", retargeted)
            self.assertFalse(_same_record(_current_record(retargeted), expected))

            # A regular file's mode is still compared.
            regular = destination / "regular.txt"
            regular.write_bytes(b"content")
            os.chmod(regular, 0o600)
            self.assertFalse(
                _same_record(
                    _current_record(regular),
                    {
                        "kind": "regular",
                        "mode": "0644",
                        "sha256": sha256_bytes(b"content"),
                        "size": 7,
                    },
                )
            )
            # A kind flip is still caught: the exemption is two-sided.
            self.assertFalse(
                _same_record(
                    _current_record(regular),
                    {
                        "kind": "symlink",
                        "mode": "0644",
                        "sha256": sha256_bytes(b"content"),
                        "size": 7,
                    },
                )
            )

    def test_planner_fingerprint_converges_on_cross_kernel_symlinks(self) -> None:
        """A byte-identical symlink must not be re-planned every run.

        _fingerprint drives every source-vs-destination install decision in the
        planner. Including a symlink's mode would mark all ~16.9k links drifted
        on every cross-kernel run, so the lane could never reach a no-op even
        with the destination verify gates fixed.
        """
        digest = sha256_bytes(os.fsencode("target.txt"))
        source = {"kind": "symlink", "mode": "0755", "sha256": digest, "size": 10}
        destination = {**source, "mode": "0777"}
        self.assertEqual(_fingerprint(source), _fingerprint(destination))

        # Target bytes and size still drive a re-plan.
        self.assertNotEqual(
            _fingerprint(source),
            _fingerprint({**source, "sha256": sha256_bytes(b"elsewhere.txt")}),
        )
        self.assertNotEqual(_fingerprint(source), _fingerprint({**source, "size": 14}))

        # A regular file's mode drift is still a re-plan.
        regular = {"kind": "regular", "mode": "0644", "sha256": digest, "size": 10}
        self.assertNotEqual(
            _fingerprint(regular), _fingerprint({**regular, "mode": "0600"})
        )


if __name__ == "__main__":
    unittest.main()
