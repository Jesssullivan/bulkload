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
    parent = home / ".agents"
    parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    lock_path = parent / ".bulkload-install.lock"
    descriptor = os.open(
        lock_path,
        os.O_RDWR | os.O_CREAT | getattr(os, "O_NOFOLLOW", 0),
        0o600,
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


if __name__ == "__main__":
    raise SystemExit(main())
