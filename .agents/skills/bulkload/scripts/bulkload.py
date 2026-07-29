#!/usr/bin/env python3
"""Self-contained bulkload command entrypoint with pre-import runtime pinning."""

# ruff: noqa: E402

from __future__ import annotations

# The direct launcher is deliberately limited to built-in modules. It re-execs
# this exact open file descriptor under isolated Python before any importable
# stdlib name can be resolved from the mutable scripts directory.
import posix as _bootstrap_posix
import sys as _bootstrap_sys


_BULKLOAD_PINNED_BOOTSTRAP = globals().get("_BULKLOAD_PINNED_BOOTSTRAP")
if __name__ == "__main__" and _BULKLOAD_PINNED_BOOTSTRAP is None:
    _bootstrap_maximum = 16 * 1024 * 1024
    _bootstrap_entry_before = _bootstrap_posix.lstat(__file__)

    def _bootstrap_entry_identity(value):
        return (
            value.st_dev,
            value.st_ino,
            value.st_mode,
            value.st_nlink,
            value.st_size,
            value.st_mtime_ns,
            value.st_ctime_ns,
        )

    _bootstrap_is_symlink = _bootstrap_entry_before.st_mode & 0o170000 == 0o120000
    _bootstrap_link_before = (
        _bootstrap_posix.readlink(__file__) if _bootstrap_is_symlink else None
    )
    _bootstrap_target_before = _bootstrap_posix.stat(__file__)
    try:
        _bootstrap_descriptor = _bootstrap_posix.open(
            __file__,
            _bootstrap_posix.O_RDONLY
            | getattr(_bootstrap_posix, "O_NOFOLLOW", 0)
            | getattr(_bootstrap_posix, "O_CLOEXEC", 0),
        )
    except OSError:
        if not _bootstrap_is_symlink:
            raise
        _bootstrap_descriptor = _bootstrap_posix.open(
            __file__,
            _bootstrap_posix.O_RDONLY | getattr(_bootstrap_posix, "O_CLOEXEC", 0),
        )
    try:
        _bootstrap_entry_after = _bootstrap_posix.lstat(__file__)
        if _bootstrap_entry_identity(
            _bootstrap_entry_before
        ) != _bootstrap_entry_identity(_bootstrap_entry_after) or (
            _bootstrap_is_symlink
            and _bootstrap_posix.readlink(__file__) != _bootstrap_link_before
        ):
            raise RuntimeError("Bulkload launcher entry changed while pinning")
        _bootstrap_before = _bootstrap_posix.fstat(_bootstrap_descriptor)
        _bootstrap_target_after = _bootstrap_posix.stat(__file__)
        if _bootstrap_entry_identity(
            _bootstrap_target_before
        ) != _bootstrap_entry_identity(
            _bootstrap_target_after
        ) or _bootstrap_entry_identity(
            _bootstrap_target_after
        ) != _bootstrap_entry_identity(_bootstrap_before):
            raise RuntimeError(
                "Bulkload launcher descriptor differs from requested target"
            )
        if (
            _bootstrap_before.st_mode & 0o170000 != 0o100000
            or _bootstrap_before.st_size < 1
            or _bootstrap_before.st_size > _bootstrap_maximum
        ):
            raise RuntimeError("Bulkload launcher custody or size is invalid")
        _bootstrap_payload = b""
        while len(_bootstrap_payload) <= _bootstrap_maximum:
            _bootstrap_block = _bootstrap_posix.read(
                _bootstrap_descriptor,
                min(
                    65536,
                    _bootstrap_maximum + 1 - len(_bootstrap_payload),
                ),
            )
            if not _bootstrap_block:
                break
            _bootstrap_payload += _bootstrap_block
        _bootstrap_after = _bootstrap_posix.fstat(_bootstrap_descriptor)
        if len(_bootstrap_payload) != _bootstrap_before.st_size or (
            _bootstrap_before.st_dev,
            _bootstrap_before.st_ino,
            _bootstrap_before.st_mode,
            _bootstrap_before.st_nlink,
            _bootstrap_before.st_size,
            _bootstrap_before.st_mtime_ns,
            _bootstrap_before.st_ctime_ns,
        ) != (
            _bootstrap_after.st_dev,
            _bootstrap_after.st_ino,
            _bootstrap_after.st_mode,
            _bootstrap_after.st_nlink,
            _bootstrap_after.st_size,
            _bootstrap_after.st_mtime_ns,
            _bootstrap_after.st_ctime_ns,
        ):
            raise RuntimeError("Bulkload launcher changed while pinning")
        _bootstrap_posix.lseek(_bootstrap_descriptor, 0, 0)
        _bootstrap_posix.set_inheritable(_bootstrap_descriptor, True)
        _bootstrap_stub = (
            "import os,posix,sys\n"
            "fd=int(sys.argv[1]); requested=sys.argv[2];"
            "direct=sys.argv[3]=='1'; maximum=16*1024*1024\n"
            "before=posix.fstat(fd); payload=b''\n"
            "while len(payload)<=maximum:\n"
            " block=posix.read(fd,min(65536,maximum+1-len(payload)))\n"
            " if not block: break\n"
            " payload+=block\n"
            "after=posix.fstat(fd)\n"
            "identity=lambda value:(value.st_dev,value.st_ino,value.st_mode,"
            "value.st_nlink,value.st_size,value.st_mtime_ns,value.st_ctime_ns)\n"
            "if len(payload)!=before.st_size or identity(before)!=identity(after):"
            " raise RuntimeError('Bulkload pinned launcher changed')\n"
            "if sys.platform.startswith('linux'):\n"
            " source=posix.readlink('/proc/self/fd/'+str(fd))\n"
            " if source.endswith(' (deleted)'):"
            "  raise RuntimeError('Bulkload pinned launcher was deleted')\n"
            "elif sys.platform=='darwin':\n"
            " import fcntl\n"
            " source=fcntl.fcntl(fd,50,b'\\0'*1024).split(b'\\0',1)[0].decode()\n"
            "elif direct:\n"
            " source=os.path.realpath(requested)\n"
            "else:\n"
            " raise RuntimeError('Bulkload symlink launcher lacks FD path authority')\n"
            "source=os.path.realpath(source)\n"
            "if not os.path.isabs(source):"
            " raise RuntimeError('Bulkload pinned launcher path is not absolute')\n"
            "entry_before=os.stat(source,follow_symlinks=False)\n"
            "if identity(entry_before)!=identity(before):"
            " raise RuntimeError('Bulkload pinned launcher path differs')\n"
            "entry_after=os.stat(source,follow_symlinks=False)\n"
            "if identity(entry_before)!=identity(entry_after)"
            " or identity(entry_after)!=identity(posix.fstat(fd)):"
            " raise RuntimeError('Bulkload pinned launcher path changed')\n"
            "posix.lseek(fd,0,0); arguments=sys.argv[4:];"
            "sys.argv=[source,*arguments]\n"
            "scope={'__name__':'__main__','__file__':source,"
            "'__package__':None,'__spec__':None,"
            "'_BULKLOAD_PINNED_BOOTSTRAP':(fd,payload)}\n"
            "exec(compile(payload,source,'exec',dont_inherit=True),scope)\n"
        )
        _bootstrap_posix.execv(
            _bootstrap_sys.executable,
            [
                _bootstrap_sys.executable,
                "-I",
                "-c",
                _bootstrap_stub,
                str(_bootstrap_descriptor),
                __file__,
                "0" if _bootstrap_is_symlink else "1",
                *_bootstrap_sys.argv[1:],
            ],
        )
    except BaseException:
        _bootstrap_posix.close(_bootstrap_descriptor)
        raise

import hashlib
import importlib.abc
import importlib.util
import json
import os
from pathlib import Path
import stat
import sys
from types import ModuleType
from typing import Any


sys.dont_write_bytecode = True

POLICY_NAME = "codex-private-state-policy.v6.json"
POLICY_SCHEMA = "dev.tinyland.bulkload.codex-private-state-policy.v6"
RUNTIME_SCHEMA = "dev.tinyland.bulkload.codex-private-runtime-authority.v1"
IMPLEMENTATION = "auth-atomic-replace-sqlite-compose-request-v6"
MAX_POLICY_BYTES = 1024 * 1024
MAX_SOURCE_BYTES = 16 * 1024 * 1024
CORE_SOURCE_KEYS = {
    "scripts/bulkload_lib/cli.py",
    "scripts/bulkload_lib/private_apply.py",
    "scripts/bulkload_lib/private_quiescence.py",
    "scripts/bulkload_lib/private_sqlite_action_plan.py",
    "scripts/bulkload_lib/private_sqlite_close.py",
    "scripts/bulkload_lib/private_sqlite_plan.py",
    "scripts/bulkload_lib/private_sqlite_request.py",
    "scripts/bulkload_lib/private_state.py",
    "scripts/bulkload_lib/sessions.py",
}
ALLOWED_CODEX_COMMANDS = [
    "codex-capture",
    "codex-close-capture",
    "codex-close-request",
    "codex-plan",
    "codex-prefix-proof",
    "codex-prefix-request",
    "codex-prefix-requests",
    "codex-private-apply",
    "codex-private-capture",
    "codex-private-install-plan",
    "codex-private-plan",
    "codex-private-quiescence-attest",
    "codex-private-recover",
    "codex-private-rollback",
    "codex-private-sqlite-capacity-observe",
    "codex-private-sqlite-compose-request",
    "codex-private-verify",
]
FORBIDDEN_COMMANDS = [
    "codex-private-combined-apply",
    "codex-private-sqlite-compose",
    "codex-state-apply",
]
STATE_CLASSES = {
    "auth_file": {
        "authority": "CODEX_HOME/auth.json",
        "classification": "copy-eligible",
        "copy_method": "pinned-private-file-atomic-replace",
        "default": "opt-in",
        "destination_evidence": "fresh-auth-and-sqlite-preservation-capture",
        "accepted_input_matrices": [
            {
                "destination": ["auth", "sqlite"],
                "source": ["auth"],
            },
            {
                "destination": ["auth", "sqlite"],
                "source": ["auth", "sqlite"],
            },
        ],
        "installer_implemented": True,
        "preferred_source_state_classes": ["auth"],
        "quiescence": "operator-attested-plus-cooperating-bulkload-lock",
        "reader_implemented": True,
        "runtime_acceptance_required": True,
        "source_input": "auth-only-capture-allowed",
    },
    "sqlite_families": {
        "auth_install_action": "preserve-destination-exact",
        "authority": "explicit-effective-sqlite-home",
        "classification": "copy-eligible",
        "composer_implemented": False,
        "copy_method": "sqlite-immutable-backup-api",
        "default": "opt-in",
        "preservation_implemented": True,
        "live_sidecars": "reject-wal-shm-journal",
        "post_plan_close_required": True,
        "provider_writer_proof": False,
        "publisher_implemented": False,
        "raw_database_wal_shm_copy": False,
        "reader_implemented": True,
        "runtime_acceptance_required": True,
        "session_union_execution_verified": False,
        "source_sqlite_required_for_auth_install": False,
        "legacy_sqlite_opening_validator_implemented": True,
        "legacy_sqlite_close_action_validator_implemented": True,
        "sqlite_compose_action_plan_implemented": False,
        "sqlite_compose_request_implemented": True,
        "capacity_observation_implemented": True,
        "workspace_reservation_implemented": False,
        "sqlite_compose_plan_implemented": False,
        "sqlite_compose_request_scope": "exact-repaired-v5-input-only",
        "post_plan_close_implemented": True,
        "wal_aware_capture": False,
    },
}


class BootstrapError(RuntimeError):
    """The installed private runtime does not match its exact policy."""


def _canonical_bytes(value: Any) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode("utf-8")


def _sha256(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def _strict_object(payload: bytes) -> dict[str, Any]:
    def unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        value: dict[str, Any] = {}
        for key, item in pairs:
            if key in value:
                raise ValueError(f"duplicate JSON key {key!r} is forbidden")
            value[key] = item
        return value

    def reject_nonfinite(value: str) -> None:
        raise ValueError(f"non-finite JSON number {value} is forbidden")

    try:
        value = json.loads(
            payload.decode("utf-8"),
            object_pairs_hook=unique_object,
            parse_constant=reject_nonfinite,
        )
    except (UnicodeDecodeError, json.JSONDecodeError, ValueError) as error:
        raise BootstrapError("private-state policy is not strict JSON") from error
    if not isinstance(value, dict):
        raise BootstrapError("private-state policy must be an object")
    if _canonical_bytes(value) + b"\n" != payload:
        raise BootstrapError("private-state policy is not canonical JSON")
    return value


def _runtime_paths(skill_root: Path) -> list[Path]:
    scripts_root = skill_root / "scripts"
    return sorted(scripts_root.rglob("*.py"))


def _relative(skill_root: Path, path: Path) -> str:
    try:
        return path.relative_to(skill_root).as_posix()
    except ValueError as error:
        raise BootstrapError("private runtime source escapes skill root") from error


def _read_descriptor(
    descriptor: int,
    *,
    maximum: int,
    label: str,
) -> bytes:
    try:
        before = os.fstat(descriptor)
        if (
            not stat.S_ISREG(before.st_mode)
            or before.st_size < 1
            or before.st_size > maximum
        ):
            raise BootstrapError(f"{label} custody or size is invalid")
        os.lseek(descriptor, 0, os.SEEK_SET)
        payload = b""
        while len(payload) <= maximum:
            block = os.read(
                descriptor,
                min(65536, maximum + 1 - len(payload)),
            )
            if not block:
                break
            payload += block
        after = os.fstat(descriptor)
    except OSError as error:
        raise BootstrapError(f"cannot read {label}") from error
    before_identity = (
        before.st_dev,
        before.st_ino,
        before.st_mode,
        before.st_nlink,
        before.st_size,
        before.st_mtime_ns,
        before.st_ctime_ns,
    )
    after_identity = (
        after.st_dev,
        after.st_ino,
        after.st_mode,
        after.st_nlink,
        after.st_size,
        after.st_mtime_ns,
        after.st_ctime_ns,
    )
    if len(payload) != before.st_size or before_identity != after_identity:
        raise BootstrapError(f"{label} changed while reading")
    return payload


def _entry_identity(value: os.stat_result) -> tuple[int, int, int, int, int, int, int]:
    return (
        value.st_dev,
        value.st_ino,
        value.st_mode,
        value.st_nlink,
        value.st_size,
        value.st_mtime_ns,
        value.st_ctime_ns,
    )


def _snapshot_entry(path: Path, *, label: str) -> tuple[tuple[int, ...], str | None]:
    try:
        entry = os.stat(path, follow_symlinks=False)
        link = os.readlink(path) if stat.S_ISLNK(entry.st_mode) else None
    except OSError as error:
        raise BootstrapError(f"cannot inspect {label}") from error
    if not (stat.S_ISREG(entry.st_mode) or stat.S_ISLNK(entry.st_mode)):
        raise BootstrapError(f"{label} path entry is not a regular file or symlink")
    return _entry_identity(entry), link


def _snapshot_path_binding(
    path: Path,
    *,
    label: str,
) -> tuple[tuple[int, ...], str | None, Path, tuple[int, ...]]:
    entry_snapshot = _snapshot_entry(path, label=label)
    link = entry_snapshot[1]
    requested_target = (
        Path(link)
        if link is not None and Path(link).is_absolute()
        else path.parent / link
        if link is not None
        else path
    )
    try:
        target = requested_target.resolve(strict=True)
        target_entry = os.stat(target, follow_symlinks=False)
    except OSError as error:
        raise BootstrapError(f"cannot resolve {label} requested target") from error
    if _snapshot_entry(path, label=label) != entry_snapshot:
        raise BootstrapError(f"{label} path entry changed while resolving")
    if not stat.S_ISREG(target_entry.st_mode):
        raise BootstrapError(f"{label} requested target is not a regular file")
    return (*entry_snapshot, target, _entry_identity(target_entry))


def _descriptor_path(
    descriptor: int,
    *,
    requested: Path,
    symlink: bool,
    label: str,
) -> Path:
    try:
        if sys.platform.startswith("linux"):
            raw = os.readlink(f"/proc/self/fd/{descriptor}")
            if raw.endswith(" (deleted)"):
                raise BootstrapError(f"{label} descriptor target was deleted")
            target = Path(raw)
        elif sys.platform == "darwin":
            import fcntl

            raw = fcntl.fcntl(descriptor, 50, b"\0" * 1024)
            target = Path(raw.split(b"\0", 1)[0].decode())
        elif symlink:
            raise BootstrapError(f"{label} symlink lacks descriptor path authority")
        else:
            target = requested
        target = target.resolve(strict=True)
    except (OSError, UnicodeDecodeError) as error:
        raise BootstrapError(f"cannot resolve {label} descriptor path") from error
    if not target.is_absolute():
        raise BootstrapError(f"{label} descriptor path is not absolute")
    return target


def _open_path_binding(
    path: Path,
    descriptor: int,
    *,
    expected: tuple[tuple[int, ...], str | None, Path, tuple[int, ...]],
    label: str,
) -> tuple[tuple[int, ...], str | None, Path, tuple[int, ...]]:
    observed_entry = _snapshot_entry(path, label=label)
    if observed_entry != expected[:2]:
        raise BootstrapError(f"{label} path entry changed while pinning")
    target = _descriptor_path(
        descriptor,
        requested=path,
        symlink=expected[1] is not None,
        label=label,
    )
    if target != expected[2]:
        raise BootstrapError(f"{label} descriptor target differs from requested target")
    try:
        target_entry = os.stat(target, follow_symlinks=False)
        opened = os.fstat(descriptor)
    except OSError as error:
        raise BootstrapError(f"cannot bind {label} descriptor path") from error
    target_identity = _entry_identity(target_entry)
    if (
        not stat.S_ISREG(target_entry.st_mode)
        or target_identity != expected[3]
        or target_identity != _entry_identity(opened)
    ):
        raise BootstrapError(f"{label} descriptor path differs")
    return expected


def _open_file(
    path: Path,
    *,
    maximum: int,
    label: str,
) -> tuple[
    int,
    bytes,
    tuple[tuple[int, ...], str | None, Path, tuple[int, ...]],
]:
    expected = _snapshot_path_binding(path, label=label)
    try:
        descriptor = os.open(
            path,
            os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0),
        )
    except OSError:
        if expected[1] is None:
            raise BootstrapError(f"cannot open {label}") from None
        try:
            descriptor = os.open(
                path,
                os.O_RDONLY | getattr(os, "O_CLOEXEC", 0),
            )
        except OSError as error:
            raise BootstrapError(f"cannot open {label}") from error
    try:
        payload = _read_descriptor(
            descriptor,
            maximum=maximum,
            label=label,
        )
        binding = _open_path_binding(
            path,
            descriptor,
            expected=expected,
            label=label,
        )
        return descriptor, payload, binding
    except BaseException:
        os.close(descriptor)
        raise


def _revalidate_path_binding(
    path: Path,
    descriptor: int,
    binding: tuple[tuple[int, ...], str | None, Path, tuple[int, ...]],
    *,
    label: str,
) -> None:
    entry_identity, link, target, target_identity = binding
    if _snapshot_entry(path, label=label) != (entry_identity, link):
        raise BootstrapError(f"{label} path entry changed")
    try:
        target_entry = os.stat(target, follow_symlinks=False)
        opened = os.fstat(descriptor)
    except OSError as error:
        raise BootstrapError(f"cannot revalidate {label} path binding") from error
    if (
        _entry_identity(target_entry) != target_identity
        or _entry_identity(opened) != target_identity
    ):
        raise BootstrapError(f"{label} descriptor path binding changed")


def _runtime_digest(payloads: dict[str, bytes]) -> str:
    inventory = {
        relative: _sha256(payload) for relative, payload in sorted(payloads.items())
    }
    return _sha256(_canonical_bytes(inventory))


def _validate_policy(
    policy: dict[str, Any],
    runtime_payloads: dict[str, bytes],
) -> str:
    expected_keys = {
        "schema",
        "implementation",
        "readiness",
        "source_digests",
        "runtime_source_sha256",
        "allowed_codex_cli_commands",
        "state_classes",
        "forbidden_commands",
    }
    if set(policy) != expected_keys:
        raise BootstrapError("private-state policy keys differ")
    if (
        policy["schema"] != POLICY_SCHEMA
        or policy["implementation"] != IMPLEMENTATION
        or policy["readiness"]
        != {
            "auth_install": True,
            "sqlite_compose_action_plan": False,
            "sqlite_compose_request": True,
            "sqlite_capacity_observation": True,
            "sqlite_compose_plan": False,
            "sqlite_compose": False,
            "sqlite_publish": False,
            "combined": False,
        }
        or policy["allowed_codex_cli_commands"] != ALLOWED_CODEX_COMMANDS
        or policy["forbidden_commands"] != FORBIDDEN_COMMANDS
        or policy["state_classes"] != STATE_CLASSES
    ):
        raise BootstrapError("private-state policy semantics differ")
    source_digests = policy["source_digests"]
    if not isinstance(source_digests, dict) or set(source_digests) != CORE_SOURCE_KEYS:
        raise BootstrapError("private-state policy source inventory differs")
    for relative, expected in source_digests.items():
        if (
            not isinstance(expected, str)
            or len(expected) != 64
            or _sha256(runtime_payloads[relative]) != expected
        ):
            raise BootstrapError(
                f"private-state policy source digest differs: {relative}"
            )
    runtime_sha256 = _runtime_digest(runtime_payloads)
    if policy["runtime_source_sha256"] != runtime_sha256:
        raise BootstrapError("private-state policy runtime closure differs")
    return runtime_sha256


class PinnedBootstrapRuntimeAuthority:
    """Pre-import source authority held through the complete command."""

    def __init__(
        self,
        *,
        skill_root: Path,
        paths: dict[str, Path],
        descriptors: dict[str, int],
        payloads: dict[str, bytes],
        bindings: dict[
            str,
            tuple[tuple[int, ...], str | None, Path, tuple[int, ...]],
        ],
        record: dict[str, Any],
    ) -> None:
        self._skill_root = skill_root
        self._paths = paths
        self._descriptors = descriptors
        self._payloads = payloads
        self._bindings = bindings
        self._record = record
        self._closed = False

    @property
    def record(self) -> dict[str, Any]:
        if self._closed:
            raise BootstrapError("private runtime authority is closed")
        return json.loads(json.dumps(self._record))

    def module_sources(self) -> dict[str, tuple[Path, bytes, bool]]:
        if self._closed:
            raise BootstrapError("private runtime authority is closed")
        prefix = "scripts/bulkload_lib/"
        modules: dict[str, tuple[Path, bytes, bool]] = {}
        for relative, payload in self._payloads.items():
            if not relative.startswith(prefix) or not relative.endswith(".py"):
                continue
            suffix = relative[len(prefix) :]
            parts = suffix.split("/")
            if parts[-1] == "__init__.py":
                module_parts = parts[:-1]
                is_package = True
            else:
                module_parts = [*parts[:-1], parts[-1][:-3]]
                is_package = False
            if not module_parts:
                module_parts = ["bulkload_lib"]
            else:
                module_parts.insert(0, "bulkload_lib")
            name = ".".join(module_parts)
            if name in modules:
                raise BootstrapError(f"duplicate private runtime module: {name}")
            modules[name] = (self._paths[relative], payload, is_package)
        if "bulkload_lib" not in modules or "bulkload_lib.cli" not in modules:
            raise BootstrapError("private runtime module closure is incomplete")
        return modules

    def revalidate(self) -> None:
        if self._closed:
            raise BootstrapError("private runtime authority is closed")
        observed_paths = {
            _relative(self._skill_root, path): path
            for path in _runtime_paths(self._skill_root)
        }
        policy_relative = f"references/{POLICY_NAME}"
        observed_paths[policy_relative] = self._skill_root / "references" / POLICY_NAME
        if observed_paths != self._paths:
            raise BootstrapError("private runtime source inventory changed")
        for relative, descriptor in self._descriptors.items():
            payload = _read_descriptor(
                descriptor,
                maximum=(
                    MAX_POLICY_BYTES
                    if relative == policy_relative
                    else MAX_SOURCE_BYTES
                ),
                label=f"private runtime artifact {relative}",
            )
            _revalidate_path_binding(
                self._paths[relative],
                descriptor,
                self._bindings[relative],
                label=f"private runtime artifact {relative}",
            )
            if payload != self._payloads[relative]:
                raise BootstrapError(
                    f"private runtime artifact content changed: {relative}"
                )

    def close(self) -> None:
        if self._closed:
            return
        self._closed = True
        for descriptor in reversed(tuple(self._descriptors.values())):
            try:
                os.close(descriptor)
            except OSError:
                pass


def _open_runtime_authority(script_directory: Path) -> PinnedBootstrapRuntimeAuthority:
    if _BULKLOAD_PINNED_BOOTSTRAP is None:
        raise BootstrapError(
            "private runtime authority requires the pinned isolated launcher"
        )
    skill_root = script_directory.parent
    paths = {_relative(skill_root, path): path for path in _runtime_paths(skill_root)}
    policy_relative = f"references/{POLICY_NAME}"
    paths[policy_relative] = skill_root / "references" / POLICY_NAME
    descriptors: dict[str, int] = {}
    payloads: dict[str, bytes] = {}
    bindings: dict[
        str,
        tuple[tuple[int, ...], str | None, Path, tuple[int, ...]],
    ] = {}
    try:
        for relative, path in sorted(paths.items()):
            if relative == "scripts/bulkload.py":
                descriptor, executed_payload = _BULKLOAD_PINNED_BOOTSTRAP
                payload = _read_descriptor(
                    descriptor,
                    maximum=MAX_SOURCE_BYTES,
                    label="executing Bulkload launcher",
                )
                if payload != executed_payload:
                    raise BootstrapError(
                        "executing Bulkload launcher differs from its pinned bytes"
                    )
                expected = _snapshot_path_binding(
                    path,
                    label="executing Bulkload launcher",
                )
                binding = _open_path_binding(
                    path,
                    descriptor,
                    expected=expected,
                    label="executing Bulkload launcher",
                )
            else:
                descriptor, payload, binding = _open_file(
                    path,
                    maximum=(
                        MAX_POLICY_BYTES
                        if relative == policy_relative
                        else MAX_SOURCE_BYTES
                    ),
                    label=f"private runtime artifact {relative}",
                )
            descriptors[relative] = descriptor
            payloads[relative] = payload
            bindings[relative] = binding
        policy_payload = payloads[policy_relative]
        runtime_payloads = {
            relative: payload
            for relative, payload in payloads.items()
            if relative != policy_relative
        }
        policy = _strict_object(policy_payload)
        runtime_sha256 = _validate_policy(policy, runtime_payloads)
        record = {
            "schema": RUNTIME_SCHEMA,
            "policy_schema": policy["schema"],
            "policy_sha256": _sha256(policy_payload),
            "runtime_source_sha256": runtime_sha256,
            "source_digests": policy["source_digests"],
        }
        authority = PinnedBootstrapRuntimeAuthority(
            skill_root=skill_root,
            paths=paths,
            descriptors=descriptors,
            payloads=payloads,
            bindings=bindings,
            record=record,
        )
        authority.revalidate()
        return authority
    except BaseException:
        for descriptor in reversed(tuple(descriptors.values())):
            try:
                os.close(descriptor)
            except OSError:
                pass
        raise


class PinnedSourceLoader(importlib.abc.Loader):
    def __init__(self, path: Path, payload: bytes, is_package: bool) -> None:
        self._path = path
        self._payload = payload
        self._is_package = is_package

    def create_module(self, spec: Any) -> ModuleType | None:
        return None

    def exec_module(self, module: ModuleType) -> None:
        try:
            source = self._payload.decode("utf-8")
        except UnicodeDecodeError as error:
            raise BootstrapError(
                f"private runtime module is not UTF-8: {self._path}"
            ) from error
        module.__file__ = os.fspath(self._path)
        if self._is_package:
            module.__path__ = ["<bulkload-pinned-runtime>"]
        code = compile(source, os.fspath(self._path), "exec", dont_inherit=True)
        exec(code, module.__dict__)


class PinnedSourceFinder(importlib.abc.MetaPathFinder):
    def __init__(self, modules: dict[str, tuple[Path, bytes, bool]]) -> None:
        self._modules = modules

    def find_spec(
        self,
        fullname: str,
        path: Any = None,
        target: ModuleType | None = None,
    ) -> Any:
        del path, target
        entry = self._modules.get(fullname)
        if entry is None:
            if fullname == "bulkload_lib" or fullname.startswith("bulkload_lib."):
                raise ModuleNotFoundError(
                    f"module is outside the pinned Bulkload closure: {fullname}"
                )
            return None
        source_path, payload, is_package = entry
        loader = PinnedSourceLoader(source_path, payload, is_package)
        return importlib.util.spec_from_loader(
            fullname,
            loader,
            origin=os.fspath(source_path),
            is_package=is_package,
        )


def _run() -> int:
    script_directory = Path(__file__).resolve().parent
    authority = _open_runtime_authority(script_directory)
    try:
        finder = PinnedSourceFinder(authority.module_sources())
    except BaseException:
        authority.close()
        raise
    if any(
        name == "bulkload_lib" or name.startswith("bulkload_lib.")
        for name in sys.modules
    ):
        authority.close()
        raise BootstrapError("bulkload runtime was imported before authority pinning")
    sys.meta_path.insert(0, finder)
    original_sys_path = list(sys.path)
    blocked_import_roots = {
        script_directory,
        script_directory.parent,
    }
    sys.path[:] = [
        entry
        for entry in sys.path
        if Path(entry or os.curdir).resolve() not in blocked_import_roots
    ]
    try:
        from bulkload_lib.cli import main

        authority.revalidate()
        result = int(main(process_runtime_authority=authority))
        authority.revalidate()
        return result
    finally:
        sys.path[:] = original_sys_path
        try:
            sys.meta_path.remove(finder)
        except ValueError:
            pass
        authority.close()


if __name__ == "__main__":
    try:
        raise SystemExit(_run())
    except BootstrapError as error:
        print(f"bulkload: bootstrap error: {error}", file=sys.stderr)
        raise SystemExit(2) from error
