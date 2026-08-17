#!/usr/bin/env python3
"""Validate bulkload's portable Agent Skill contract without dependencies."""

from __future__ import annotations

import argparse
from pathlib import Path
from pathlib import PurePosixPath
import re
import sys
from typing import Final


ALLOWED_FRONTMATTER: Final = {
    "name",
    "description",
    "license",
    "metadata",
}
NAME_PATTERN: Final = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")
MARKDOWN_LINK: Final = re.compile(r"\[[^\]]+\]\(([^)]+)\)")


class SkillContractError(ValueError):
    """Raised when a skill violates its portable contract."""


def parse_frontmatter(text: str) -> tuple[dict[str, str], str]:
    lines = text.splitlines()
    if not lines or lines[0] != "---":
        raise SkillContractError("SKILL.md must start with YAML frontmatter")
    try:
        closing = lines.index("---", 1)
    except ValueError as error:
        raise SkillContractError("SKILL.md frontmatter is not closed") from error

    values: dict[str, str] = {}
    for number, line in enumerate(lines[1:closing], start=2):
        if not line.strip() or line.startswith((" ", "\t")):
            raise SkillContractError(
                f"frontmatter line {number} must be a non-empty scalar mapping"
            )
        key, separator, raw_value = line.partition(":")
        if not separator or not key or not raw_value.strip():
            raise SkillContractError(f"invalid frontmatter line {number}")
        if key in values:
            raise SkillContractError(f"duplicate frontmatter key: {key}")
        values[key] = raw_value.strip().strip("\"'")
    return values, "\n".join(lines[closing + 1 :]).strip()


def resolve_local_link(skill_root: Path, target: str) -> Path | None:
    target = target.split("#", 1)[0]
    if not target or "://" in target or target.startswith(("mailto:", "#")):
        return None
    relative = PurePosixPath(target)
    if relative.is_absolute() or ".." in relative.parts:
        raise SkillContractError(f"local link escapes skill root: {target}")
    return skill_root.joinpath(*relative.parts)


def validate(skill_root: Path, *, allow_runfiles_symlinks: bool = False) -> None:
    if skill_root.is_symlink() and not allow_runfiles_symlinks:
        raise SkillContractError(f"skill root must not be a symlink: {skill_root}")
    skill_root = skill_root.resolve()
    skill_file = skill_root / "SKILL.md"
    try:
        text = skill_file.read_text(encoding="utf-8")
    except OSError as error:
        raise SkillContractError(f"cannot read {skill_file}: {error}") from error

    frontmatter, body = parse_frontmatter(text)
    unknown = set(frontmatter) - ALLOWED_FRONTMATTER
    if unknown:
        raise SkillContractError(f"unsupported frontmatter keys: {sorted(unknown)}")
    if set(frontmatter) < {"name", "description"}:
        raise SkillContractError("frontmatter requires name and description")

    name = frontmatter["name"]
    if name != skill_root.name:
        raise SkillContractError("skill name must match its directory")
    if len(name) > 64 or not NAME_PATTERN.fullmatch(name):
        raise SkillContractError(
            "skill name must be lowercase hyphen-case, at most 64 chars"
        )

    description = frontmatter["description"]
    if len(description) > 1024 or any(character in description for character in "<>"):
        raise SkillContractError(
            "description must be at most 1024 chars without angle brackets"
        )
    if not body:
        raise SkillContractError("SKILL.md body must not be empty")
    if len(text.splitlines()) > 500:
        raise SkillContractError(
            "SKILL.md exceeds the 500-line progressive-disclosure limit"
        )
    if re.search(r"\|\s*rsync\b", body):
        raise SkillContractError(
            "SKILL.md must not pipe an allowlist producer directly to rsync"
        )

    for target in MARKDOWN_LINK.findall(body):
        resolved = resolve_local_link(skill_root, target)
        if resolved is not None and not resolved.is_file():
            raise SkillContractError(f"missing local skill reference: {target}")

    metadata_file = skill_root / "agents" / "openai.yaml"
    try:
        metadata = metadata_file.read_text(encoding="utf-8")
    except OSError as error:
        raise SkillContractError(f"cannot read {metadata_file}: {error}") from error
    for key in ("display_name:", "short_description:", "default_prompt:"):
        if key not in metadata:
            raise SkillContractError(f"openai.yaml missing {key[:-1]}")
    if f"${name}" not in metadata:
        raise SkillContractError(
            "openai.yaml default prompt must mention the skill by name"
        )

    for path in skill_root.rglob("*"):
        if "__pycache__" in path.parts or path.suffix in {".pyc", ".pyo"}:
            raise SkillContractError(
                f"skill bundle contains generated Python cache: {path}"
            )
        if path.is_symlink() and not allow_runfiles_symlinks:
            raise SkillContractError(f"skill bundle must not contain symlinks: {path}")
        if path.is_file():
            try:
                path.read_bytes().decode("utf-8")
            except UnicodeDecodeError as error:
                raise SkillContractError(
                    f"skill bundle file is not UTF-8: {path}"
                ) from error


def self_test() -> None:
    values, body = parse_frontmatter(
        "---\nname: bulkload\ndescription: migrate safely\n---\n# Bulkload\n"
    )
    if values["name"] != "bulkload" or body != "# Bulkload":
        raise AssertionError("valid frontmatter did not round trip")
    for invalid in ("name: no-boundary", "---\nname: bulkload"):
        try:
            parse_frontmatter(invalid)
        except SkillContractError:
            continue
        raise AssertionError("invalid frontmatter was accepted")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--allow-runfiles-symlinks", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("skill", nargs="?", default=".agents/skills/bulkload")
    arguments = parser.parse_args()
    try:
        if arguments.self_test:
            self_test()
        validate(
            Path(arguments.skill),
            allow_runfiles_symlinks=arguments.allow_runfiles_symlinks,
        )
    except (OSError, SkillContractError, AssertionError) as error:
        print(f"skill-contract: FAIL: {error}", file=sys.stderr)
        return 1
    print("skill-contract: PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
