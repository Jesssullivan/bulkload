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
_RUNTIME_STAGE_ENV = "BULKLOAD_TEST_RUNTIME_STAGE"


def _unprivileged_account() -> tuple[str, int, int]:
    import pwd

    for name in ("nobody", "daemon"):
        try:
            account = pwd.getpwnam(name)
        except KeyError:
            continue
        if account.pw_uid > 0 and account.pw_gid > 0:
            return account.pw_name, account.pw_uid, account.pw_gid
    for account in pwd.getpwall():
        if account.pw_uid > 0 and account.pw_gid > 0:
            return account.pw_name, account.pw_uid, account.pw_gid
    raise RuntimeError("root test worker has no unprivileged account")


def _prepare_owned_directory(path: Path, uid: int, gid: int) -> None:
    path.mkdir(mode=0o700)
    path.chmod(0o700)
    os.chown(path, uid, gid)


def _assert_regular_source(skill_root: Path, path: Path) -> Path:
    relative = path.relative_to(skill_root)
    cursor = skill_root
    for component in relative.parts:
        cursor = cursor / component
        if cursor.is_symlink():
            raise RuntimeError(f"private runtime source contains a symlink: {relative}")
    if not stat.S_ISREG(path.stat(follow_symlinks=False).st_mode):
        raise RuntimeError(f"private runtime source is not regular: {relative}")
    return relative


def _prepare_private_runtime_stage() -> tuple[Path, Path]:
    """Copy only the declared runtime authority to a root-owned read-only stage."""

    from bulkload_lib import private_runtime

    skill_root = Path(private_runtime.__file__).resolve().parents[2]
    sources = list(private_runtime._runtime_paths(skill_root))
    sources.append(
        skill_root / "references" / private_runtime.PRIVATE_STATE_POLICY_NAME
    )
    stage_root = Path(tempfile.mkdtemp(prefix="bulkload-runtime-stage-", dir="/tmp"))
    try:
        for source in sorted(sources):
            relative = _assert_regular_source(skill_root, source)
            destination = stage_root / relative
            destination.parent.mkdir(mode=0o755, parents=True, exist_ok=True)
            destination.write_bytes(source.read_bytes())
            destination.chmod(0o444)
        directories = [
            stage_root,
            *(path for path in stage_root.rglob("*") if path.is_dir()),
        ]
        for directory in sorted(
            directories, key=lambda path: len(path.parts), reverse=True
        ):
            directory.chmod(0o555)
        staged_module = stage_root / "scripts" / "bulkload_lib" / "private_runtime.py"
        if not staged_module.is_file():
            raise RuntimeError("private runtime stage omitted its authority module")
        return stage_root, staged_module
    except BaseException:
        _remove_readonly_tree(stage_root)
        raise


def _remove_readonly_tree(root: Path) -> None:
    if root.is_symlink():
        root.unlink()
        return
    if not root.exists():
        return
    for path in sorted(root.rglob("*"), key=lambda item: len(item.parts), reverse=True):
        if path.is_symlink():
            path.unlink()
        elif path.is_dir():
            path.chmod(0o700)
        else:
            path.chmod(0o600)
    root.chmod(0o700)
    shutil.rmtree(root)


def run_unittest_main() -> None:
    """Fork a same-image child and irreversibly drop root before discovery."""

    if not hasattr(os, "geteuid") or os.geteuid() != 0:
        unittest.main()
        return
    if not all(hasattr(os, name) for name in ("fork", "setgroups", "setgid", "setuid")):
        raise RuntimeError("root test worker cannot drop privileges")

    username, uid, gid = _unprivileged_account()
    for encoding in _LAZY_CODECS:
        codecs.lookup(encoding)
    runtime_stage, staged_runtime_module = _prepare_private_runtime_stage()
    test_root: Path | None = None
    try:
        test_root = Path(tempfile.mkdtemp(prefix="bulkload-unprivileged-", dir="/tmp"))
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

        child = os.fork()
        if child == 0:
            status = 1
            try:
                from bulkload_lib import private_runtime

                private_runtime.__file__ = os.fspath(staged_runtime_module)
                os.environ[_RUNTIME_STAGE_ENV] = os.fspath(runtime_stage)
                os.setgroups([])
                os.setgid(gid)
                os.setuid(uid)
                for variable, path in directories.items():
                    os.environ[variable] = os.fspath(path)
                os.environ["USER"] = username
                os.environ["LOGNAME"] = username
                os.environ["BULKLOAD_TEST_PRIVILEGE_DROP"] = "1"
                tempfile.tempdir = os.fspath(directories["TMPDIR"])
                program = unittest.main(exit=False)
                status = 0 if program.result.wasSuccessful() else 1
            except BaseException:
                traceback.print_exc()
            finally:
                sys.stdout.flush()
                sys.stderr.flush()
                os._exit(status)

        waited, status = os.waitpid(child, 0)
        if waited != child:
            raise RuntimeError("unprivileged test child wait returned a different pid")
    finally:
        if test_root is not None:
            shutil.rmtree(test_root)
        _remove_readonly_tree(runtime_stage)
    if os.WIFEXITED(status):
        raise SystemExit(os.WEXITSTATUS(status))
    if os.WIFSIGNALED(status):
        raise SystemExit(128 + os.WTERMSIG(status))
    raise RuntimeError("unprivileged test child ended in an unknown state")
