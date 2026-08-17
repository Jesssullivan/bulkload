"""Run mutation tests as an actual unprivileged user on root CI workers."""

from __future__ import annotations

import codecs
import os
from pathlib import Path
import shutil
import stat
import sys
import tempfile
import traceback
import unittest


_LAZY_CODECS = ("ascii", "utf-16-be")
RUNTIME_SKILL_ROOT_ENV = "BULKLOAD_TEST_RUNTIME_SKILL_ROOT"
_MAX_PROJECTED_RUNTIME_BYTES = 16 * 1024 * 1024


def _unprivileged_account() -> tuple[str, int, int]:
    import pwd

    for name in ("nobody", "daemon"):
        try:
            account = pwd.getpwnam(name)
        except KeyError:
            continue
        if account.pw_uid > 0 and account.pw_gid > 0:
            return account.pw_name, account.pw_uid, account.pw_gid
    raise RuntimeError("root test worker has no sanctioned unprivileged account")


def _prepare_owned_directory(path: Path, uid: int, gid: int) -> None:
    path.mkdir(mode=0o700)
    path.chmod(0o700)
    os.chown(path, uid, gid)


def _read_projected_source(path: Path) -> bytes:
    try:
        descriptor = os.open(
            path,
            os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0),
        )
    except OSError as error:
        raise RuntimeError(f"cannot open projected runtime source: {path}") from error
    try:
        before = os.fstat(descriptor)
        if (
            not stat.S_ISREG(before.st_mode)
            or before.st_size < 1
            or before.st_size > _MAX_PROJECTED_RUNTIME_BYTES
        ):
            raise RuntimeError(f"projected runtime source is invalid: {path}")
        payload = b""
        while len(payload) <= _MAX_PROJECTED_RUNTIME_BYTES:
            block = os.read(
                descriptor,
                min(65536, _MAX_PROJECTED_RUNTIME_BYTES + 1 - len(payload)),
            )
            if not block:
                break
            payload += block
        after = os.fstat(descriptor)

        def identity(value: os.stat_result) -> tuple[int, ...]:
            return (
                value.st_dev,
                value.st_ino,
                value.st_mode,
                value.st_nlink,
                value.st_size,
                value.st_mtime_ns,
                value.st_ctime_ns,
            )

        if len(payload) != before.st_size or identity(before) != identity(after):
            raise RuntimeError(f"projected runtime source changed: {path}")
        return payload
    finally:
        os.close(descriptor)


def _write_projected_file(
    path: Path,
    payload: bytes,
    uid: int,
    gid: int,
) -> None:
    descriptor = os.open(
        path,
        os.O_WRONLY
        | os.O_CREAT
        | os.O_EXCL
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_CLOEXEC", 0),
        0o600,
    )
    try:
        view = memoryview(payload)
        while view:
            written = os.write(descriptor, view)
            if written < 1:
                raise RuntimeError(f"cannot write projected runtime source: {path}")
            view = view[written:]
        os.fchmod(descriptor, 0o600)
        os.fchown(descriptor, uid, gid)
    finally:
        os.close(descriptor)


def _project_runtime_skill_root(
    source_root: Path,
    policy_name: str,
    test_root: Path,
    uid: int,
    gid: int,
) -> Path:
    try:
        source_root = source_root.resolve(strict=True)
    except OSError as error:
        raise RuntimeError("runtime skill root cannot be resolved") from error
    if not policy_name or Path(policy_name).name != policy_name:
        raise RuntimeError("runtime policy name is not one bounded filename")
    scripts_root = source_root / "scripts"
    policy_path = source_root / "references" / policy_name
    if not scripts_root.is_dir() or scripts_root.is_symlink():
        raise RuntimeError("runtime scripts root is not a bounded directory")

    python_sources = sorted(scripts_root.rglob("*.py"))
    if not python_sources:
        raise RuntimeError("runtime projection has no Python sources")
    sources = [*python_sources, policy_path]
    relative_sources: dict[Path, Path] = {}
    for source in sources:
        try:
            relative = source.relative_to(source_root)
        except ValueError as error:
            raise RuntimeError("runtime projection source escapes its root") from error
        if relative in relative_sources or source.is_symlink() or not source.is_file():
            raise RuntimeError(f"runtime projection source is invalid: {relative}")
        relative_sources[relative] = source

    projection_root = test_root / "private-runtime"
    _prepare_owned_directory(projection_root, uid, gid)
    prepared_directories = {Path()}
    for relative, source in sorted(relative_sources.items()):
        cursor = projection_root
        for part in relative.parent.parts:
            cursor = cursor / part
            projected_relative = cursor.relative_to(projection_root)
            if projected_relative not in prepared_directories:
                _prepare_owned_directory(cursor, uid, gid)
                prepared_directories.add(projected_relative)
        destination = projection_root / relative
        try:
            destination.relative_to(projection_root)
        except ValueError as error:
            raise RuntimeError("runtime projection destination escapes") from error
        _write_projected_file(
            destination,
            _read_projected_source(source),
            uid,
            gid,
        )
    return projection_root.resolve(strict=True)


def run_unittest_main(
    *,
    runtime_skill_root: Path | None = None,
    runtime_policy_name: str | None = None,
) -> None:
    """Fork a same-image child and irreversibly drop root before discovery."""

    if not hasattr(os, "geteuid") or os.geteuid() != 0:
        unittest.main()
        return
    if not all(hasattr(os, name) for name in ("fork", "setgroups", "setgid", "setuid")):
        raise RuntimeError("root test worker cannot drop privileges")
    if RUNTIME_SKILL_ROOT_ENV in os.environ:
        raise RuntimeError("runtime skill projection environment is already set")
    if (runtime_skill_root is None) != (runtime_policy_name is None):
        raise RuntimeError("runtime skill root and policy name must be paired")

    username, uid, gid = _unprivileged_account()
    for encoding in _LAZY_CODECS:
        codecs.lookup(encoding)

    test_root = Path(tempfile.mkdtemp(prefix="bulkload-unprivileged-", dir="/tmp"))
    try:
        test_root.chmod(0o700)
        os.chown(test_root, uid, gid)
        directories = {
            "HOME": test_root / "home",
            "TMPDIR": test_root / "tmp",
            "XDG_CACHE_HOME": test_root / "xdg-cache",
            "XDG_CONFIG_HOME": test_root / "xdg-config",
            "XDG_DATA_HOME": test_root / "xdg-data",
            "XDG_RUNTIME_DIR": test_root / "xdg-runtime",
            "XDG_STATE_HOME": test_root / "xdg-state",
        }
        for path in directories.values():
            _prepare_owned_directory(path, uid, gid)
        runtime_projection = (
            _project_runtime_skill_root(
                runtime_skill_root,
                runtime_policy_name if runtime_policy_name is not None else "",
                test_root,
                uid,
                gid,
            )
            if runtime_skill_root is not None
            else None
        )

        child = os.fork()
        if child == 0:
            child_status = 1
            try:
                os.setgroups([])
                os.setgid(gid)
                os.setuid(uid)
                for variable, path in directories.items():
                    os.environ[variable] = os.fspath(path)
                os.environ["USER"] = username
                os.environ["LOGNAME"] = username
                os.environ["BULKLOAD_TEST_PRIVILEGE_DROP"] = "1"
                if runtime_projection is not None:
                    os.environ[RUNTIME_SKILL_ROOT_ENV] = os.fspath(runtime_projection)
                tempfile.tempdir = os.fspath(directories["TMPDIR"])
                program = unittest.main(exit=False)
                child_status = 0 if program.result.wasSuccessful() else 1
            except BaseException:
                traceback.print_exc()
            finally:
                sys.stdout.flush()
                sys.stderr.flush()
                os._exit(child_status)

        waited, status = os.waitpid(child, 0)
        if waited != child:
            raise RuntimeError("unprivileged test child wait returned a different pid")
    finally:
        shutil.rmtree(test_root)

    if os.WIFEXITED(status):
        raise SystemExit(os.WEXITSTATUS(status))
    if os.WIFSIGNALED(status):
        raise SystemExit(128 + os.WTERMSIG(status))
    raise RuntimeError("unprivileged test child ended in an unknown state")
