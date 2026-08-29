#!/usr/bin/env -S python3 -I -S
"""Isolated, pinned-source entrypoint for Bulkload."""

from __future__ import annotations

import hashlib
import importlib.abc
import importlib.util
import os
from pathlib import Path
import stat
import sys
from types import ModuleType
from typing import Any


RUNTIME_FILES = (
    "__init__.py",
    "model.py",
    "mover.py",
    "scanner.py",
    "planner.py",
    "executor.py",
    "cli.py",
)
MAX_SOURCE_BYTES = 4 * 1024 * 1024


class BootstrapError(RuntimeError):
    pass


def _stable(info: os.stat_result) -> tuple[int, ...]:
    return (
        info.st_dev,
        info.st_ino,
        info.st_mode,
        info.st_nlink,
        info.st_size,
        info.st_mtime_ns,
        info.st_ctime_ns,
    )


def _pin_sources(root: Path) -> dict[str, tuple[Path, bytes, bool]]:
    modules: dict[str, tuple[Path, bytes, bool]] = {}
    inventory: dict[str, str] = {}
    for filename in RUNTIME_FILES:
        try:
            path = (root / filename).resolve(strict=True)
        except (OSError, RuntimeError) as error:
            raise BootstrapError(f"cannot resolve runtime source {filename}") from error
        flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0)
        try:
            descriptor = os.open(path, flags)
        except OSError as error:
            raise BootstrapError(f"cannot pin runtime source {filename}") from error
        try:
            before = os.fstat(descriptor)
            if (
                not stat.S_ISREG(before.st_mode)
                or before.st_nlink != 1
                or before.st_size < 1
                or before.st_size > MAX_SOURCE_BYTES
            ):
                raise BootstrapError(f"runtime source custody is invalid: {filename}")
            payload = b""
            while chunk := os.read(
                descriptor, min(65536, MAX_SOURCE_BYTES + 1 - len(payload))
            ):
                payload += chunk
            after = os.fstat(descriptor)
            entry = os.stat(path, follow_symlinks=False)
            if (
                len(payload) != before.st_size
                or _stable(before) != _stable(after)
                or _stable(after) != _stable(entry)
            ):
                raise BootstrapError(
                    f"runtime source changed while pinning: {filename}"
                )
        finally:
            os.close(descriptor)
        module = (
            "bulkload_lib" if filename == "__init__.py" else f"bulkload_lib.{path.stem}"
        )
        modules[module] = (path, payload, filename == "__init__.py")
        inventory[f"scripts/bulkload_lib/{filename}"] = hashlib.sha256(
            payload
        ).hexdigest()
    closure = hashlib.sha256(
        (
            "{"
            + ",".join(f'"{key}":"{inventory[key]}"' for key in sorted(inventory))
            + "}"
        ).encode()
    ).hexdigest()
    # BULKLOAD_RUNTIME_SOURCE_PIN is the operator break-glass for a
    # verify-side engine fix mid-ceremony: when set, the launcher presents the
    # pinned closure instead of the computed one. The distinct variable name
    # keeps the launcher's own trust channel (BULKLOAD_RUNTIME_SOURCE_SHA256,
    # always overwritten below) from leaking a stale value across launcher
    # invocations inside one process tree — the defect a setdefault had.
    os.environ["BULKLOAD_RUNTIME_SOURCE_SHA256"] = os.environ.get(
        "BULKLOAD_RUNTIME_SOURCE_PIN", closure
    )
    return modules


class _Loader(importlib.abc.Loader):
    def __init__(self, path: Path, payload: bytes, package: bool) -> None:
        self.path = path
        self.payload = payload
        self.package = package

    def create_module(self, spec: Any) -> ModuleType | None:
        return None

    def exec_module(self, module: ModuleType) -> None:
        module.__file__ = os.fspath(self.path)
        if self.package:
            module.__path__ = [os.fspath(self.path.parent)]
        try:
            source = self.payload.decode("utf-8")
        except UnicodeDecodeError as error:
            raise BootstrapError("runtime source is not UTF-8") from error
        exec(
            compile(source, os.fspath(self.path), "exec", dont_inherit=True),
            module.__dict__,
        )


class _Finder(importlib.abc.MetaPathFinder):
    def __init__(self, modules: dict[str, tuple[Path, bytes, bool]]) -> None:
        self.modules = modules

    def find_spec(self, fullname: str, path: Any = None, target: Any = None) -> Any:
        del path, target
        entry = self.modules.get(fullname)
        if entry is None:
            if fullname == "bulkload_lib" or fullname.startswith("bulkload_lib."):
                raise ModuleNotFoundError(
                    f"module is outside pinned Bulkload closure: {fullname}"
                )
            return None
        source, payload, package = entry
        return importlib.util.spec_from_loader(
            fullname,
            _Loader(source, payload, package),
            origin=os.fspath(source),
            is_package=package,
        )


def _run() -> int:
    if not all(
        (
            sys.flags.isolated,
            sys.flags.no_site,
            sys.flags.ignore_environment,
            sys.flags.safe_path,
        )
    ):
        raise BootstrapError("the supported launcher requires Python -I -S")
    modules = _pin_sources(Path(__file__).resolve().parent / "bulkload_lib")
    finder = _Finder(modules)
    sys.meta_path.insert(0, finder)
    try:
        from bulkload_lib.cli import main

        return int(main())
    finally:
        sys.meta_path.remove(finder)


if __name__ == "__main__":
    try:
        raise SystemExit(_run())
    except BootstrapError as error:
        print(f"bulkload: bootstrap error: {error}", file=sys.stderr)
        raise SystemExit(2) from error
