#!/usr/bin/env python3
"""Validate Bulkload's compact, self-contained Agent Skill."""

from __future__ import annotations

import argparse
import ast
from pathlib import Path, PurePosixPath
import re
import sys


RUNTIME_FILES = {
    "scripts/bulkload.py",
    "scripts/bulkload_lib/__init__.py",
    "scripts/bulkload_lib/cli.py",
    "scripts/bulkload_lib/executor.py",
    "scripts/bulkload_lib/model.py",
    "scripts/bulkload_lib/planner.py",
    "scripts/bulkload_lib/scanner.py",
}
PUBLIC_COMMANDS = {
    "agent-capture",
    "agent-plan",
    "agent-stage",
    "agent-apply",
    "agent-verify",
    "agent-rollback",
    "agent-recover",
}
APPROVED_SCHEMA_IDS = {
    "dev.tinyland.bulkload.agent-apply-receipt.v4",
    "dev.tinyland.bulkload.agent-capture.v4",
    "dev.tinyland.bulkload.agent-journal.v4",
    "dev.tinyland.bulkload.agent-plan.v4",
    "dev.tinyland.bulkload.agent-recover-receipt.v4",
    "dev.tinyland.bulkload.agent-rollback-receipt.v4",
    "dev.tinyland.bulkload.agent-stage-receipt.v4",
    "dev.tinyland.bulkload.agent-verify-receipt.v4",
    "dev.tinyland.bulkload.git-workspace.v2",
}
SUPERSEDED_NAMES = {
    "private_apply.py",
    "private_quiescence.py",
    "private_runtime.py",
    "private_sqlite_action_plan.py",
    "private_sqlite_close.py",
    "private_sqlite_plan.py",
    "private_sqlite_protocol.py",
    "private_sqlite_request.py",
    "private_sqlite_verifier.py",
    "private_state.py",
    "sessions.py",
}
FORBIDDEN_RUNTIME_PATTERNS = (
    r"\bos\.kill\b",
    r"\bsignal\.SIG",
    r"\.send_signal\s*\(",
    r"\bkillpg\s*\(",
    r"\bpkill\b",
    r"\bkillall\b",
    r"\bSIGSTOP\b",
    r"\bSIGCONT\b",
    r"\bSIGTERM\b",
    r"\bSIGKILL\b",
    r"\bSIGHUP\b",
    r"\bkill\b",
    r"\blaunchctl\b",
    r"\bsystemctl\b",
    r"force[-_]quiesce",
    r"skip[-_]quiescence",
    r"assume[-_]quiet",
)
LINK = re.compile(r"\[[^]]+\]\(([^)]+)\)")


class SkillContractError(ValueError):
    pass


def _relative_files(root: Path) -> set[str]:
    return {
        path.relative_to(root).as_posix() for path in root.rglob("*") if path.is_file()
    }


def _literal_commands(cli_source: str) -> set[str]:
    tree = ast.parse(cli_source)
    commands: set[str] = set()
    for node in ast.walk(tree):
        if not isinstance(node, ast.Call) or not isinstance(node.func, ast.Attribute):
            continue
        if node.func.attr != "add_parser" or not node.args:
            continue
        first = node.args[0]
        if not isinstance(first, ast.Constant) or not isinstance(first.value, str):
            raise SkillContractError("CLI command names must be string literals")
        commands.add(first.value)
    return commands


def validate(skill_root: Path, *, allow_runfiles_symlinks: bool = False) -> None:
    try:
        root = skill_root.resolve(strict=True)
    except (OSError, RuntimeError) as error:
        raise SkillContractError("skill root does not resolve") from error
    if not root.is_dir():
        raise SkillContractError("skill root is not a directory")
    entries = list(root.rglob("*"))
    if not allow_runfiles_symlinks and any(path.is_symlink() for path in entries):
        raise SkillContractError("skill bundle must not contain symlinks")
    if any(
        "__pycache__" in path.relative_to(root).parts or path.suffix in {".pyc", ".pyo"}
        for path in entries
    ):
        raise SkillContractError("skill bundle contains generated Python cache")
    files = _relative_files(root)
    if not RUNTIME_FILES <= files:
        raise SkillContractError(
            f"runtime closure is incomplete: {sorted(RUNTIME_FILES - files)}"
        )
    leaves = {PurePosixPath(path).name for path in files}
    if leaves & SUPERSEDED_NAMES:
        raise SkillContractError("superseded private planner/runtime source remains")
    if any("codex-private-state-policy.v" in path for path in files):
        raise SkillContractError("superseded private-state policy remains")
    for relative in RUNTIME_FILES:
        path = root / relative
        if path.is_symlink() and not allow_runfiles_symlinks:
            raise SkillContractError(
                f"runtime source must not be a symlink: {relative}"
            )
        try:
            payload = path.read_bytes()
            source = payload.decode("utf-8")
            ast.parse(source)
        except (OSError, UnicodeDecodeError, SyntaxError) as error:
            raise SkillContractError(
                f"runtime source is invalid: {relative}"
            ) from error
        if not payload or len(payload) > 4 * 1024 * 1024:
            raise SkillContractError(f"runtime source size is invalid: {relative}")
    launcher = (root / "scripts/bulkload.py").read_text(encoding="utf-8")
    if not launcher.startswith("#!/usr/bin/env -S python3 -I -S\n"):
        raise SkillContractError(
            "direct launcher must enter isolated Python with -I -S"
        )
    cli_source = (root / "scripts/bulkload_lib/cli.py").read_text(encoding="utf-8")
    observed_commands = _literal_commands(cli_source)
    if observed_commands != PUBLIC_COMMANDS:
        raise SkillContractError(
            f"public command surface differs: {sorted(observed_commands ^ PUBLIC_COMMANDS)}"
        )
    skill = root / "SKILL.md"
    text = skill.read_text(encoding="utf-8")
    lines = text.splitlines()
    if not lines or lines[0] != "---" or lines.count("---") < 2:
        raise SkillContractError("SKILL.md must have YAML frontmatter")
    if len(lines) > 500:
        raise SkillContractError("SKILL.md exceeds the progressive-disclosure limit")
    frontmatter_end = lines[1:].index("---") + 1
    frontmatter = "\n".join(lines[1:frontmatter_end])
    if not re.search(r"^name:\s*bulkload\s*$", frontmatter, re.MULTILINE):
        raise SkillContractError("SKILL.md name must be bulkload")
    if not re.search(r"^description:\s*\S", frontmatter, re.MULTILINE):
        raise SkillContractError("SKILL.md description is required")
    for match in LINK.finditer(text):
        target = match.group(1).split("#", 1)[0]
        if not target or "://" in target or target.startswith("#"):
            continue
        if not (root / target).resolve().is_file():
            raise SkillContractError(f"SKILL.md link target is missing: {target}")
    combined = "\n".join(
        (root / relative).read_text(encoding="utf-8")
        for relative in sorted(RUNTIME_FILES)
    )
    observed_schemas = set(
        re.findall(r"dev\.tinyland\.bulkload\.[a-z0-9.-]+", combined)
    )
    if observed_schemas != APPROVED_SCHEMA_IDS:
        raise SkillContractError(
            f"runtime schema surface differs: {sorted(observed_schemas ^ APPROVED_SCHEMA_IDS)}"
        )
    for forbidden in (
        "codex-private-plan",
        "sqlite_union_ready=false",
        "composer_implemented = False",
    ):
        if forbidden in combined:
            raise SkillContractError(
                f"superseded fail-held contract remains: {forbidden}"
            )
    for pattern in FORBIDDEN_RUNTIME_PATTERNS:
        if re.search(pattern, combined):
            raise SkillContractError(
                f"runtime must not control unowned processes: {pattern}"
            )


def self_test() -> None:
    if (
        _literal_commands(
            "\n".join(
                ["commands = parser.add_subparsers()"]
                + [f"commands.add_parser({name!r})" for name in sorted(PUBLIC_COMMANDS)]
            )
        )
        != PUBLIC_COMMANDS
    ):
        raise AssertionError("literal CLI command scanner failed")
    try:
        _literal_commands("commands.add_parser(variable)")
    except SkillContractError:
        pass
    else:
        raise AssertionError("dynamic CLI command escaped validation")
    for sample in (
        "os.kill(pid, 0)",
        "parser.add_argument('--assume-quiet')",
        'subprocess.run(["kill", "-TERM", pid])',
        'subprocess.run(["launchctl", "bootout", label])',
    ):
        if not any(
            re.search(pattern, sample) for pattern in FORBIDDEN_RUNTIME_PATTERNS
        ):
            raise AssertionError("process-control scanner failed")
    if any(
        re.search(pattern, "subprocess.run(arguments, check=False)\nskill_root = root")
        for pattern in FORBIDDEN_RUNTIME_PATTERNS
    ):
        raise AssertionError("process-control scanner is overbroad")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--allow-runfiles-symlinks", action="store_true")
    parser.add_argument("skill_root", nargs="?", default=".agents/skills/bulkload")
    arguments = parser.parse_args()
    try:
        if arguments.self_test:
            self_test()
        validate(
            Path(arguments.skill_root),
            allow_runfiles_symlinks=arguments.allow_runfiles_symlinks,
        )
    except (OSError, SkillContractError, AssertionError, ValueError) as error:
        print(f"skill-contract: FAIL: {error}", file=sys.stderr)
        return 1
    print("skill-contract: PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
