"""Run mutation tests as an actual unprivileged user on root CI workers."""

from __future__ import annotations

import codecs
import os
from pathlib import Path
import shutil
import sys
import tempfile
import traceback
import unittest


_LAZY_CODECS = ("ascii", "utf-16-be")


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
