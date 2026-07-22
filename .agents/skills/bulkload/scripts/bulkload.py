#!/usr/bin/env python3
"""Self-contained bulkload command entrypoint."""

import sys
from pathlib import Path

sys.dont_write_bytecode = True
SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

from bulkload_lib.cli import main  # noqa: E402


if __name__ == "__main__":
    raise SystemExit(main())
