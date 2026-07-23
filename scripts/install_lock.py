#!/usr/bin/env python3
"""Serialize user-scope bulkload installs without platform-specific flock CLI."""

from __future__ import annotations

import fcntl
import os
from pathlib import Path
import stat
import subprocess
import sys


def main() -> int:
    if len(sys.argv) < 2:
        print("install-lock: installer path is required", file=sys.stderr)
        return 2
    home = Path(os.environ["HOME"]).expanduser()
    try:
        home_info = home.lstat()
    except FileNotFoundError:
        print("install-lock: HOME does not exist", file=sys.stderr)
        return 1
    if not stat.S_ISDIR(home_info.st_mode) or stat.S_ISLNK(home_info.st_mode):
        print("install-lock: HOME must be a real directory", file=sys.stderr)
        return 1
    parent = home / ".agents"
    try:
        os.mkdir(parent, 0o700)
    except FileExistsError:
        pass
    try:
        parent_info = parent.lstat()
    except FileNotFoundError:
        print("install-lock: .agents disappeared during preflight", file=sys.stderr)
        return 1
    if not stat.S_ISDIR(parent_info.st_mode) or stat.S_ISLNK(parent_info.st_mode):
        print("install-lock: .agents must be a real directory", file=sys.stderr)
        return 1
    directory_descriptor = os.open(
        parent,
        os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0),
    )
    try:
        if not stat.S_ISDIR(os.fstat(directory_descriptor).st_mode):
            print("install-lock: .agents changed during preflight", file=sys.stderr)
            return 1
        os.fchmod(directory_descriptor, 0o700)
        lock_name = ".bulkload-install.lock"
        nofollow = getattr(os, "O_NOFOLLOW", 0)
        try:
            descriptor = os.open(
                lock_name,
                os.O_RDWR | os.O_CREAT | os.O_EXCL | nofollow,
                0o600,
                dir_fd=directory_descriptor,
            )
        except FileExistsError:
            descriptor = os.open(
                lock_name,
                os.O_RDWR | nofollow,
                dir_fd=directory_descriptor,
            )
        try:
            if not stat.S_ISREG(os.fstat(descriptor).st_mode):
                print("install-lock: lock path is not a regular file", file=sys.stderr)
                return 1
            os.fchmod(descriptor, 0o600)
            fcntl.flock(descriptor, fcntl.LOCK_EX)
            environment = {**os.environ, "BULKLOAD_INSTALL_LOCKED": "1"}
            process = subprocess.run(sys.argv[1:], env=environment, check=False)
            return process.returncode
        finally:
            os.close(descriptor)
    finally:
        os.close(directory_descriptor)


if __name__ == "__main__":
    raise SystemExit(main())
