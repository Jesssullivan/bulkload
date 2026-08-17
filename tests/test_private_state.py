from __future__ import annotations

from contextlib import redirect_stderr
import io
import json
import os
from pathlib import Path
import sqlite3
import stat
import tempfile
import unittest
from unittest import mock

from bulkload_lib.cli import main as cli_main
from bulkload_lib.model import BulkloadError, read_json
from bulkload_lib import private_state
from bulkload_lib.private_state import (
    PRIVATE_CAPTURE_SCHEMA,
    PRIVATE_MANIFEST,
    PRIVATE_PLAN_SCHEMA,
    capture_codex_private_state,
    compile_codex_private_state_plan,
    validate_codex_private_capture,
    write_private_json_noreplace,
)


SOURCE_AUTHORITY = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
DESTINATION_AUTHORITY = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
FAMILIES = (
    "goals_1.sqlite",
    "logs_2.sqlite",
    "memories_1.sqlite",
    "state_5.sqlite",
)


def create_family(
    path: Path,
    *,
    role: str,
    state_extra_column: bool = False,
    keep_open: bool = False,
) -> sqlite3.Connection | None:
    connection = sqlite3.connect(path)
    connection.execute("PRAGMA journal_mode=WAL")
    connection.execute(
        """
        CREATE TABLE _sqlx_migrations(
            version INTEGER PRIMARY KEY,
            description TEXT NOT NULL,
            installed_on TEXT NOT NULL,
            success INTEGER NOT NULL,
            checksum BLOB NOT NULL,
            execution_time INTEGER NOT NULL
        )
        """
    )
    connection.execute(
        "INSERT INTO _sqlx_migrations VALUES(1, 'fixture', 'now', 1, x'00', 1)"
    )
    if path.name == "state_5.sqlite":
        extra = ", name TEXT" if state_extra_column else ""
        connection.execute(
            f"""
            CREATE TABLE threads(
                id TEXT PRIMARY KEY,
                rollout_path TEXT NOT NULL
                {extra}
            )
            """
        )
        connection.execute(
            """
            CREATE TABLE thread_spawn_edges(
                parent_thread_id TEXT NOT NULL,
                child_thread_id TEXT PRIMARY KEY,
                status TEXT NOT NULL
            )
            """
        )
        columns = (
            "(id, rollout_path, name)" if state_extra_column else ("(id, rollout_path)")
        )
        placeholders = "(?, ?, ?)" if state_extra_column else "(?, ?)"
        values = (
            (f"{role}-thread", f"/{role}/sessions/rollout.jsonl", role)
            if state_extra_column
            else (f"{role}-thread", f"/{role}/sessions/rollout.jsonl")
        )
        connection.execute(
            f"INSERT INTO threads {columns} VALUES {placeholders}",
            values,
        )
    elif path.name == "goals_1.sqlite":
        connection.execute(
            "CREATE TABLE thread_goals(thread_id TEXT PRIMARY KEY, objective TEXT)"
        )
    elif path.name == "memories_1.sqlite":
        connection.execute(
            "CREATE TABLE stage1_outputs(thread_id TEXT PRIMARY KEY, raw_memory TEXT)"
        )
    else:
        connection.execute(
            "CREATE TABLE logs(id INTEGER PRIMARY KEY, feedback_log_body TEXT)"
        )
    connection.commit()
    if keep_open:
        return connection
    connection.close()
    return None


def create_home(
    root: Path,
    role: str,
    *,
    auth: str,
    state_extra_column: bool = False,
    live_state_writer: bool = False,
) -> tuple[Path, sqlite3.Connection | None]:
    home = root / role
    home.mkdir(mode=0o700)
    auth_path = home / "auth.json"
    auth_path.write_text(json.dumps({"fixture_secret": auth}) + "\n")
    auth_path.chmod(0o600)
    live: sqlite3.Connection | None = None
    for family in FAMILIES:
        opened = create_family(
            home / family,
            role=role,
            state_extra_column=state_extra_column,
            keep_open=live_state_writer and family == "state_5.sqlite",
        )
        if opened is not None:
            live = opened
    return home, live


def capture(root: Path, role: str, home: Path) -> tuple[dict, Path]:
    output = root / f"{role}-bundle"
    value = capture_codex_private_state(
        home,
        output,
        role=role,
        host_authority_id=(
            SOURCE_AUTHORITY if role == "source" else DESTINATION_AUTHORITY
        ),
        codex_version="0.145.0",
        sqlite_home=home,
        include_auth=True,
        include_sqlite=True,
        acknowledge_private_capture=True,
    )
    return value, output


class CodexPrivateStateTest(unittest.TestCase):
    def test_capture_uses_online_backup_and_keeps_values_out_of_manifest(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            secret = "DO_NOT_PUBLISH_THIS_FIXTURE"
            home, writer = create_home(
                root,
                "source",
                auth=secret,
                live_state_writer=True,
            )
            assert writer is not None
            try:
                value, output = capture(root, "source", home)
            finally:
                writer.close()

            validate_codex_private_capture(value)
            self.assertEqual(value["schema"], PRIVATE_CAPTURE_SCHEMA)
            self.assertFalse(value["ready_for_apply"])
            self.assertEqual(
                [item["basename"] for item in value["sqlite_families"]],
                list(FAMILIES),
            )
            self.assertEqual(stat.S_IMODE(output.stat().st_mode), 0o700)
            self.assertEqual(
                stat.S_IMODE((output / PRIVATE_MANIFEST).stat().st_mode),
                0o600,
            )
            self.assertNotIn(
                secret.encode(),
                (output / PRIVATE_MANIFEST).read_bytes(),
            )
            self.assertEqual(
                (output / "auth.json").read_text(),
                json.dumps({"fixture_secret": secret}) + "\n",
            )
            for family in FAMILIES:
                snapshot = output / "sqlite" / family
                self.assertTrue(snapshot.is_file())
                self.assertEqual(stat.S_IMODE(snapshot.stat().st_mode), 0o600)
                connection = sqlite3.connect(
                    f"file:{snapshot}?mode=ro",
                    uri=True,
                )
                try:
                    self.assertEqual(
                        connection.execute("PRAGMA quick_check(1)").fetchone()[0],
                        "ok",
                    )
                finally:
                    connection.close()
                self.assertFalse((output / "sqlite" / f"{family}-wal").exists())
                self.assertFalse((output / "sqlite" / f"{family}-shm").exists())

    def test_capture_rejects_hardlinked_auth_and_retains_private_staging(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            home, _ = create_home(root, "source", auth="fixture")
            os.link(home / "auth.json", home / "auth.second-link")
            output = root / "blocked-bundle"

            with self.assertRaisesRegex(BulkloadError, "preserved owner-private"):
                capture_codex_private_state(
                    home,
                    output,
                    role="source",
                    host_authority_id=SOURCE_AUTHORITY,
                    codex_version="0.145.0",
                    include_auth=True,
                    include_sqlite=False,
                    acknowledge_private_capture=True,
                )

            self.assertFalse(output.exists())
            retained = list(root.glob(".blocked-bundle.bulkload-private-*"))
            self.assertEqual(len(retained), 1)
            self.assertEqual(stat.S_IMODE(retained[0].stat().st_mode), 0o700)

    def test_capture_rejects_invalid_auth_json(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            home, _ = create_home(root, "source", auth="fixture")
            (home / "auth.json").write_bytes(b"not-json\n")
            (home / "auth.json").chmod(0o600)

            with self.assertRaisesRegex(BulkloadError, "strict JSON object"):
                capture_codex_private_state(
                    home,
                    root / "blocked-bundle",
                    role="source",
                    host_authority_id=SOURCE_AUTHORITY,
                    codex_version="0.145.0",
                    sqlite_home=home,
                    include_auth=True,
                    include_sqlite=False,
                    acknowledge_private_capture=True,
                )

    def test_capture_rejects_sqlite_family_set_change(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            home, _ = create_home(root, "source", auth="fixture")
            original = private_state._online_backup
            calls = 0

            def add_family(*args, **kwargs):
                nonlocal calls
                result = original(*args, **kwargs)
                calls += 1
                if calls == 1:
                    connection = sqlite3.connect(home / "late.sqlite")
                    connection.execute("CREATE TABLE late(id INTEGER PRIMARY KEY)")
                    connection.commit()
                    connection.close()
                return result

            with (
                mock.patch.object(
                    private_state,
                    "_online_backup",
                    side_effect=add_family,
                ),
                self.assertRaisesRegex(
                    BulkloadError,
                    "family authority changed",
                ),
            ):
                capture_codex_private_state(
                    home,
                    root / "blocked-bundle",
                    role="source",
                    host_authority_id=SOURCE_AUTHORITY,
                    codex_version="0.145.0",
                    sqlite_home=home,
                    include_auth=False,
                    include_sqlite=True,
                    acknowledge_private_capture=True,
                )

    def test_capture_enforces_budget_against_wal_backed_logical_size(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            home = root / "source"
            home.mkdir(mode=0o700)
            connection = sqlite3.connect(home / "logs_1.sqlite")
            connection.execute("PRAGMA journal_mode=WAL")
            connection.execute("PRAGMA wal_autocheckpoint=0")
            connection.execute("CREATE TABLE logs(payload BLOB)")
            connection.execute("INSERT INTO logs(payload) VALUES(zeroblob(1048576))")
            connection.commit()
            try:
                self.assertGreater(
                    (home / "logs_1.sqlite-wal").stat().st_size,
                    64 * 1024,
                )
                with self.assertRaisesRegex(
                    BulkloadError,
                    "snapshot exceeds the configured byte budget",
                ):
                    capture_codex_private_state(
                        home,
                        root / "blocked-bundle",
                        role="source",
                        host_authority_id=SOURCE_AUTHORITY,
                        codex_version="0.145.0",
                        sqlite_home=home,
                        include_auth=False,
                        include_sqlite=True,
                        acknowledge_private_capture=True,
                        max_total_sqlite_bytes=64 * 1024,
                    )
            finally:
                connection.close()

    def test_capture_enforces_thread_index_budget(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            home, _ = create_home(root, "source", auth="fixture")
            connection = sqlite3.connect(home / "state_5.sqlite")
            connection.execute(
                "INSERT INTO threads(id, rollout_path) VALUES(?, ?)",
                ("second-thread", "/source/sessions/second.jsonl"),
            )
            connection.commit()
            connection.close()

            with self.assertRaisesRegex(
                BulkloadError,
                "thread index exceeds its capture budget",
            ):
                capture_codex_private_state(
                    home,
                    root / "blocked-bundle",
                    role="source",
                    host_authority_id=SOURCE_AUTHORITY,
                    codex_version="0.145.0",
                    sqlite_home=home,
                    include_auth=False,
                    include_sqlite=True,
                    acknowledge_private_capture=True,
                    max_thread_entries=1,
                )

    def test_capture_enforces_schema_metadata_budget(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            home, _ = create_home(root, "source", auth="fixture")
            with self.assertRaisesRegex(
                BulkloadError,
                "metadata exceeds its capture budget",
            ):
                capture_codex_private_state(
                    home,
                    root / "blocked-bundle",
                    role="source",
                    host_authority_id=SOURCE_AUTHORITY,
                    codex_version="0.145.0",
                    sqlite_home=home,
                    include_auth=False,
                    include_sqlite=True,
                    acknowledge_private_capture=True,
                    max_metadata_entries=1,
                )

    def test_capture_requires_explicit_effective_sqlite_authority(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            codex_home = root / "codex"
            sqlite_home = root / "sqlite"
            codex_home.mkdir(mode=0o700)
            sqlite_home.mkdir(mode=0o700)
            create_family(sqlite_home / "state_5.sqlite", role="source")
            with (
                mock.patch.dict(
                    os.environ,
                    {"CODEX_SQLITE_HOME": str(sqlite_home)},
                ),
                self.assertRaisesRegex(
                    BulkloadError,
                    "explicit effective SQLite home",
                ),
            ):
                capture_codex_private_state(
                    codex_home,
                    root / "bundle",
                    role="source",
                    host_authority_id=SOURCE_AUTHORITY,
                    codex_version="0.145.0",
                    include_auth=False,
                    include_sqlite=True,
                    acknowledge_private_capture=True,
                )

    def test_auth_only_capture_ignores_stale_sqlite_environment(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            home = root / "source"
            home.mkdir(mode=0o700)
            auth = home / "auth.json"
            auth.write_text('{"fixture":"auth-only"}\n')
            auth.chmod(0o600)
            with mock.patch.dict(
                os.environ,
                {"CODEX_SQLITE_HOME": str(root / "missing")},
            ):
                value = capture_codex_private_state(
                    home,
                    root / "bundle",
                    role="source",
                    host_authority_id=SOURCE_AUTHORITY,
                    codex_version="0.145.0",
                    include_auth=True,
                    include_sqlite=False,
                    acknowledge_private_capture=True,
                )
            self.assertEqual(value["selected_state_classes"], ["auth"])
            self.assertIsNone(value["sqlite_home"])
            self.assertEqual(value["sqlite_families"], [])

    def test_capture_rejects_manifest_over_configured_cap(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            home = root / "source"
            home.mkdir(mode=0o700)
            auth = home / "auth.json"
            auth.write_text('{"fixture":"bounded"}\n')
            auth.chmod(0o600)
            with (
                mock.patch.object(
                    private_state,
                    "MAX_PRIVATE_MANIFEST_BYTES",
                    1,
                ),
                self.assertRaisesRegex(
                    BulkloadError,
                    "manifest exceeds the configured byte budget",
                ),
            ):
                capture_codex_private_state(
                    home,
                    root / "blocked-bundle",
                    role="source",
                    host_authority_id=SOURCE_AUTHORITY,
                    codex_version="0.145.0",
                    include_auth=True,
                    include_sqlite=False,
                    acknowledge_private_capture=True,
                )
            self.assertFalse((root / "blocked-bundle").exists())

    def test_capture_revalidates_auth_after_publication(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            home, _ = create_home(root, "source", auth="source")
            output = root / "bundle"
            original_rename = private_state._rename_noreplace_at

            def mutate_after_rename(*args, **kwargs):
                original_rename(*args, **kwargs)
                with (output / "auth.json").open("ab") as stream:
                    stream.write(b"tamper")

            with (
                mock.patch.object(
                    private_state,
                    "_rename_noreplace_at",
                    side_effect=mutate_after_rename,
                ),
                self.assertRaisesRegex(
                    BulkloadError,
                    "preserved owner-private artifact",
                ),
            ):
                capture_codex_private_state(
                    home,
                    output,
                    role="source",
                    host_authority_id=SOURCE_AUTHORITY,
                    codex_version="0.145.0",
                    sqlite_home=home,
                    include_auth=True,
                    include_sqlite=True,
                    acknowledge_private_capture=True,
                )
            self.assertTrue(output.is_dir())

    def test_capture_revalidates_sqlite_after_publication(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            home, _ = create_home(root, "source", auth="source")
            output = root / "bundle"
            original_rename = private_state._rename_noreplace_at

            def mutate_after_rename(*args, **kwargs):
                original_rename(*args, **kwargs)
                with (output / "sqlite" / "state_5.sqlite").open("ab") as stream:
                    stream.write(b"tamper")

            with (
                mock.patch.object(
                    private_state,
                    "_rename_noreplace_at",
                    side_effect=mutate_after_rename,
                ),
                self.assertRaisesRegex(
                    BulkloadError,
                    "preserved owner-private artifact",
                ),
            ):
                capture_codex_private_state(
                    home,
                    output,
                    role="source",
                    host_authority_id=SOURCE_AUTHORITY,
                    codex_version="0.145.0",
                    sqlite_home=home,
                    include_auth=True,
                    include_sqlite=True,
                    acknowledge_private_capture=True,
                )
            self.assertTrue(output.is_dir())

    def test_capture_rejects_unexpected_published_sqlite_family(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            home, _ = create_home(root, "source", auth="source")
            output = root / "bundle"
            original_rename = private_state._rename_noreplace_at

            def add_family_after_rename(*args, **kwargs):
                original_rename(*args, **kwargs)
                connection = sqlite3.connect(output / "sqlite" / "goals_999.sqlite")
                connection.execute("CREATE TABLE unexpected(id INTEGER)")
                connection.commit()
                connection.close()
                (output / "sqlite" / "goals_999.sqlite").chmod(0o600)

            with (
                mock.patch.object(
                    private_state,
                    "_rename_noreplace_at",
                    side_effect=add_family_after_rename,
                ),
                self.assertRaisesRegex(
                    BulkloadError,
                    "preserved owner-private artifact",
                ),
            ):
                capture_codex_private_state(
                    home,
                    output,
                    role="source",
                    host_authority_id=SOURCE_AUTHORITY,
                    codex_version="0.145.0",
                    sqlite_home=home,
                    include_auth=True,
                    include_sqlite=True,
                    acknowledge_private_capture=True,
                )
            self.assertTrue(output.is_dir())

    def test_plan_records_schema_and_composer_blockers(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source_home, _ = create_home(root, "source", auth="source")
            destination_home, _ = create_home(
                root,
                "destination",
                auth="destination",
                state_extra_column=True,
            )
            _, source = capture(root, "source", source_home)
            _, destination = capture(root, "destination", destination_home)

            plan = compile_codex_private_state_plan(source, destination)

            self.assertEqual(plan["schema"], PRIVATE_PLAN_SCHEMA)
            self.assertFalse(plan["ready_for_apply"])
            codes = [item["code"] for item in plan["blockers"]]
            self.assertIn("auth-installer-not-implemented", codes)
            self.assertIn("sqlite-schema-mismatch", codes)
            self.assertEqual(codes.count("sqlite-composer-not-implemented"), 4)
            state = next(
                item
                for item in plan["sqlite_families"]
                if item["basename"] == "state_5.sqlite"
            )
            self.assertFalse(state["schema_compatible"])
            self.assertEqual(state["thread_relation"]["source_only_threads"], 1)
            self.assertEqual(
                state["thread_relation"]["destination_only_threads"],
                1,
            )

    def test_plan_rejects_sqlite_header_mismatch_as_incompatible(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source_home, _ = create_home(root, "source", auth="same")
            destination_home, _ = create_home(root, "destination", auth="same")
            source_connection = sqlite3.connect(source_home / "state_5.sqlite")
            source_connection.execute("PRAGMA user_version=1")
            source_connection.close()
            destination_connection = sqlite3.connect(
                destination_home / "state_5.sqlite"
            )
            destination_connection.execute("PRAGMA user_version=2")
            destination_connection.close()
            _, source = capture(root, "source", source_home)
            _, destination = capture(root, "destination", destination_home)

            plan = compile_codex_private_state_plan(source, destination)

            codes = [item["code"] for item in plan["blockers"]]
            self.assertIn("sqlite-header-mismatch", codes)
            state = next(
                item
                for item in plan["sqlite_families"]
                if item["basename"] == "state_5.sqlite"
            )
            self.assertFalse(state["header_compatible"])
            self.assertFalse(state["compatible"])

    def test_plan_rejects_tampered_private_artifact(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source_home, _ = create_home(root, "source", auth="same")
            destination_home, _ = create_home(root, "destination", auth="same")
            _, source = capture(root, "source", source_home)
            _, destination = capture(root, "destination", destination_home)
            with (source / "auth.json").open("ab") as stream:
                stream.write(b"tamper")

            with self.assertRaisesRegex(BulkloadError, "artifact digest mismatch"):
                compile_codex_private_state_plan(source, destination)

    def test_sqlite_only_plan_does_not_require_auth(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            bundles = []
            for role, authority in (
                ("source", SOURCE_AUTHORITY),
                ("destination", DESTINATION_AUTHORITY),
            ):
                home = root / role
                home.mkdir(mode=0o700)
                create_family(home / "state_5.sqlite", role=role)
                bundle = root / f"{role}-bundle"
                capture_codex_private_state(
                    home,
                    bundle,
                    role=role,
                    host_authority_id=authority,
                    codex_version="0.145.0",
                    sqlite_home=home,
                    include_auth=False,
                    include_sqlite=True,
                    acknowledge_private_capture=True,
                )
                bundles.append(bundle)
            plan = compile_codex_private_state_plan(*bundles)
            self.assertEqual(plan["auth"], {"action": "not-selected"})
            self.assertNotIn(
                "source-auth-not-captured",
                [item["code"] for item in plan["blockers"]],
            )

    def test_private_cli_is_capture_and_plan_only(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source_home, _ = create_home(root, "source", auth="source")
            destination_home, _ = create_home(
                root,
                "destination",
                auth="destination",
            )
            source_bundle = root / "source-bundle"
            destination_bundle = root / "destination-bundle"
            common = [
                "--codex-version",
                "0.145.0",
                "--include-auth",
                "--include-sqlite",
                "--acknowledge-private-capture",
            ]
            for role, home, authority, output in (
                ("source", source_home, SOURCE_AUTHORITY, source_bundle),
                (
                    "destination",
                    destination_home,
                    DESTINATION_AUTHORITY,
                    destination_bundle,
                ),
            ):
                with mock.patch("sys.stdout", new=io.StringIO()):
                    result = cli_main(
                        [
                            "codex-private-capture",
                            "--codex-home",
                            str(home),
                            "--sqlite-home",
                            str(home),
                            "--output-directory",
                            str(output),
                            "--role",
                            role,
                            "--host-authority-id",
                            authority,
                            *common,
                        ]
                    )
                self.assertEqual(result, 0)

            plan_path = root / "private-plan.json"
            with mock.patch("sys.stdout", new=io.StringIO()):
                result = cli_main(
                    [
                        "codex-private-plan",
                        "--source-bundle",
                        str(source_bundle),
                        "--destination-bundle",
                        str(destination_bundle),
                        "--output",
                        str(plan_path),
                    ]
                )
            self.assertEqual(result, 4)
            plan = read_json(plan_path)
            self.assertFalse(plan["ready_for_apply"])
            self.assertEqual(
                plan["implementation"],
                "capture-and-compatibility-plan",
            )

            with (
                redirect_stderr(io.StringIO()),
                mock.patch("sys.stdout", new=io.StringIO()),
            ):
                with self.assertRaises(SystemExit) as raised:
                    cli_main(["codex-private-apply"])
            self.assertEqual(raised.exception.code, 2)

    def test_private_plan_cannot_write_into_recorded_live_roots(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            captures: dict[str, tuple[Path, Path, Path]] = {}
            for role, authority in (
                ("source", SOURCE_AUTHORITY),
                ("destination", DESTINATION_AUTHORITY),
            ):
                codex_home = root / f"{role}-codex"
                sqlite_home = root / f"{role}-sqlite"
                codex_home.mkdir(mode=0o700)
                sqlite_home.mkdir(mode=0o700)
                auth = codex_home / "auth.json"
                auth.write_text(json.dumps({"fixture_secret": role}) + "\n")
                auth.chmod(0o600)
                create_family(sqlite_home / "state_5.sqlite", role=role)
                bundle = root / f"{role}-bundle"
                capture_codex_private_state(
                    codex_home,
                    bundle,
                    role=role,
                    host_authority_id=authority,
                    codex_version="0.145.0",
                    sqlite_home=sqlite_home,
                    include_auth=True,
                    include_sqlite=True,
                    acknowledge_private_capture=True,
                )
                captures[role] = (codex_home, sqlite_home, bundle)

            source_bundle = captures["source"][2]
            destination_bundle = captures["destination"][2]
            protected = (
                captures["source"][0],
                captures["source"][1],
                captures["destination"][0],
                captures["destination"][1],
            )
            for index, live_root in enumerate(protected):
                output = live_root / f"forbidden-plan-{index}.json"
                with (
                    redirect_stderr(io.StringIO()),
                    mock.patch("sys.stdout", new=io.StringIO()),
                ):
                    result = cli_main(
                        [
                            "codex-private-plan",
                            "--source-bundle",
                            str(source_bundle),
                            "--destination-bundle",
                            str(destination_bundle),
                            "--output",
                            str(output),
                        ]
                    )
                self.assertEqual(result, 2)
                self.assertFalse(output.exists())

    def test_private_plan_allows_missing_remote_recorded_roots(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source_home, _ = create_home(root, "source", auth="source")
            destination_home, _ = create_home(
                root,
                "destination",
                auth="destination",
            )
            _, source_bundle = capture(root, "source", source_home)
            _, destination_bundle = capture(
                root,
                "destination",
                destination_home,
            )
            source_home.rename(root / "source-remote-now")
            destination_home.rename(root / "destination-remote-now")
            output = root / "private-plan.json"

            with mock.patch("sys.stdout", new=io.StringIO()):
                result = cli_main(
                    [
                        "codex-private-plan",
                        "--source-bundle",
                        str(source_bundle),
                        "--destination-bundle",
                        str(destination_bundle),
                        "--output",
                        str(output),
                    ]
                )

            self.assertEqual(result, 4)
            self.assertTrue(output.is_file())

    def test_private_plan_publication_is_fail_held_on_short_write(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            output = root / "private-plan.json"
            original_write = os.write
            writes = 0

            def interrupted_write(descriptor, payload):
                nonlocal writes
                writes += 1
                if writes == 1:
                    return original_write(descriptor, payload[:5])
                raise OSError("injected write interruption")

            with (
                mock.patch.object(
                    private_state.os,
                    "write",
                    side_effect=interrupted_write,
                ),
                self.assertRaisesRegex(
                    BulkloadError,
                    "preserved owner-private artifact",
                ),
            ):
                write_private_json_noreplace(
                    output,
                    {"schema": PRIVATE_PLAN_SCHEMA, "payload": "bounded"},
                )

            self.assertFalse(output.exists())
            retained = list(root.glob(".private-plan.json.bulkload-private-*"))
            self.assertEqual(len(retained), 1)
            self.assertEqual(stat.S_IMODE(retained[0].stat().st_mode), 0o600)

    def test_private_plan_revalidates_inputs_after_publication(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source_home, _ = create_home(root, "source", auth="source")
            destination_home, _ = create_home(
                root,
                "destination",
                auth="destination",
            )
            _, source_bundle = capture(root, "source", source_home)
            _, destination_bundle = capture(
                root,
                "destination",
                destination_home,
            )
            output = root / "private-plan.json"
            original_writer = write_private_json_noreplace

            def mutate_after_write(*args, **kwargs):
                original_writer(*args, **kwargs)
                with (source_bundle / "auth.json").open("ab") as stream:
                    stream.write(b"tamper")

            errors = io.StringIO()
            with (
                mock.patch(
                    "bulkload_lib.cli.write_private_json_noreplace",
                    side_effect=mutate_after_write,
                ),
                redirect_stderr(errors),
                mock.patch("sys.stdout", new=io.StringIO()),
            ):
                result = cli_main(
                    [
                        "codex-private-plan",
                        "--source-bundle",
                        str(source_bundle),
                        "--destination-bundle",
                        str(destination_bundle),
                        "--output",
                        str(output),
                    ]
                )

            self.assertEqual(result, 2)
            self.assertTrue(output.is_file())
            self.assertIn("fail-held evidence", errors.getvalue())

    def test_generic_apply_rejects_private_plan_before_root_creation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            plan = root / "private-plan.json"
            plan.write_text(json.dumps({"schema": PRIVATE_PLAN_SCHEMA}) + "\n")
            plan.chmod(0o600)
            destination = root / "destination"
            state = root / "state"
            receipt = root / "receipt.json"

            with (
                redirect_stderr(io.StringIO()),
                mock.patch("sys.stdout", new=io.StringIO()),
            ):
                result = cli_main(
                    [
                        "apply",
                        "--plan",
                        str(plan),
                        "--accept-plan",
                        "not-a-private-digest",
                        "--source-root",
                        str(root / "source"),
                        "--destination-root",
                        str(destination),
                        "--state-root",
                        str(state),
                        "--receipt",
                        str(receipt),
                    ]
                )

            self.assertEqual(result, 2)
            self.assertFalse(destination.exists())
            self.assertFalse(state.exists())
            self.assertFalse(receipt.exists())


if __name__ == "__main__":
    unittest.main()
