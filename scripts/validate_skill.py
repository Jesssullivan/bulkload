#!/usr/bin/env python3
"""Validate bulkload's portable Agent Skill contract without dependencies."""

from __future__ import annotations

import argparse
import ast
import copy
import hashlib
import json
from pathlib import Path
from pathlib import PurePosixPath
import re
import sys
from typing import Any
from typing import Final


ALLOWED_FRONTMATTER: Final = {
    "name",
    "description",
    "license",
    "metadata",
}
NAME_PATTERN: Final = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")
MARKDOWN_LINK: Final = re.compile(r"\[[^\]]+\]\(([^)]+)\)")
PRIVATE_STATE_POLICY_NAME: Final = "codex-private-state-policy.v2.json"
EXPECTED_PRIVATE_STATE_POLICY: Final[dict[str, Any]] = {
    "schema": "dev.tinyland.bulkload.codex-private-state-policy.v2",
    "implementation": "capture-and-compatibility-plan",
    "ready_for_apply": False,
    "cli_source_sha256": (
        "7ac31d3b5d63eedcf2d47e51f361418153935a8cf3d30eae8add8b68b72476b5"
    ),
    "private_state_source_sha256": (
        "418e4c52e360bf09639726888673ca4c79f534edf04d84d43d93267e67171cb8"
    ),
    "runtime_source_sha256": (
        "adde391bba7c7d3c5d932bcaced63e93027eac9a03cf70f1a1f05eed2c3fcd70"
    ),
    "allowed_codex_cli_commands": [
        "codex-capture",
        "codex-close-capture",
        "codex-close-request",
        "codex-plan",
        "codex-prefix-proof",
        "codex-prefix-request",
        "codex-prefix-requests",
        "codex-private-capture",
        "codex-private-plan",
    ],
    "state_classes": {
        "auth_file": {
            "classification": "copy-eligible",
            "default": "opt-in",
            "authority": "CODEX_HOME/auth.json",
            "reader_implemented": True,
            "executor_implemented": False,
            "copy_method": "pinned-private-file-capture",
            "requirements": [
                "explicit-source-and-destination",
                "no-value-logging",
                "regular-single-link-current-user-file",
                "owner-private-no-replace-capture",
                "source-and-destination-backup",
                "exact-owner-and-mode",
                "same-directory-atomic-install",
                "file-and-directory-fsync",
                "rollback-retained",
            ],
            "acceptance": [
                "destination-file-private",
                "codex-authentication-succeeds",
                "fresh-provider-turn-succeeds",
            ],
        },
        "sqlite_families": {
            "classification": "copy-eligible",
            "default": "opt-in",
            "authority_resolution": [
                "sqlite_home",
                "CODEX_SQLITE_HOME",
                "CODEX_HOME",
            ],
            "discovery": "enumerate-provider-owned-sqlite-families",
            "reader_implemented": True,
            "executor_implemented": False,
            "copy_method": "sqlite-online-backup-api",
            "raw_database_wal_shm_copy": False,
            "requirements": [
                "explicit-effective-sqlite-home",
                "enumerate-all-provider-owned-families",
                "repeat-family-enumeration",
                "matching-codex-version",
                "compatible-migration-and-schema",
                "compatible-user-version-and-application-id",
                "online-backup-with-bounded-time",
                "bounded-schema-migration-metadata",
                "bounded-thread-index",
                "snapshot-quick-check",
                "snapshot-delete-journal-mode",
                "owner-private-no-replace-capture",
                "destination-backup",
                "canonical-destination-rollout-paths",
                "same-directory-atomic-install",
                "file-and-directory-fsync",
                "rollback-retained",
            ],
            "acceptance": [
                "destination-quick-check",
                "no-stale-source-rollout-paths",
                "thread-and-spawn-edge-parity",
                "history-picker-parity",
                "representative-historical-resume",
                "fresh-turn-persists",
            ],
        },
    },
    "forbidden_commands": ["codex-private-apply", "codex-state-apply"],
}
EXPECTED_CLI_COMMAND_HANDLERS: Final[dict[str, str]] = {
    "apply": "_apply",
    "capture": "_capture",
    "codex-capture": "_codex_capture",
    "codex-close-capture": "_codex_close_capture",
    "codex-close-request": "_codex_close_request",
    "codex-plan": "_codex_plan",
    "codex-prefix-proof": "_codex_prefix_proof",
    "codex-prefix-request": "_codex_prefix_request",
    "codex-prefix-requests": "_codex_prefix_request",
    "codex-private-capture": "_codex_private_capture",
    "codex-private-plan": "_codex_private_plan",
    "doctor": "_doctor",
    "files": "_files",
    "plan": "_plan",
    "verify": "_verify",
}


class SkillContractError(ValueError):
    """Raised when a skill violates its portable contract."""


def runtime_source_digest_from_texts(sources: dict[str, str]) -> str:
    inventory = {
        path: hashlib.sha256(text.encode("utf-8")).hexdigest()
        for path, text in sorted(sources.items())
    }
    payload = json.dumps(
        inventory,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode("utf-8")
    return hashlib.sha256(payload).hexdigest()


def runtime_source_digest(skill_root: Path) -> str:
    scripts_root = skill_root / "scripts"
    paths = [scripts_root / "bulkload.py"]
    paths.extend(sorted((scripts_root / "bulkload_lib").rglob("*.py")))
    sources: dict[str, str] = {}
    for path in paths:
        try:
            relative = path.relative_to(skill_root).as_posix()
            sources[relative] = path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError, ValueError) as error:
            raise SkillContractError(
                f"cannot read Bulkload runtime source {path}: {error}"
            ) from error
    return runtime_source_digest_from_texts(sources)


def parse_strict_json_object(text: str) -> dict[str, Any]:
    def unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        value: dict[str, Any] = {}
        for key, item in pairs:
            if key in value:
                raise ValueError(f"duplicate JSON key {key!r} is forbidden")
            value[key] = item
        return value

    def reject_nonfinite(value: str) -> None:
        raise ValueError(f"non-finite JSON number {value} is forbidden")

    value = json.loads(
        text,
        object_pairs_hook=unique_object,
        parse_constant=reject_nonfinite,
    )
    if not isinstance(value, dict):
        raise ValueError("policy must contain one JSON object")
    return value


def cli_command_handlers(cli_text: str) -> dict[str, str]:
    try:
        tree = ast.parse(cli_text)
    except SyntaxError as error:
        raise SkillContractError(f"cannot parse bulkload CLI: {error}") from error
    builders = [
        node
        for node in tree.body
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
        and node.name == "build_parser"
    ]
    if len(builders) != 1:
        raise SkillContractError("bulkload CLI must define one build_parser function")
    all_parser_calls = {
        id(node)
        for node in ast.walk(builders[0])
        if isinstance(node, ast.Call)
        and isinstance(node.func, ast.Attribute)
        and node.func.attr == "add_parser"
    }
    consumed_parser_calls: set[int] = set()
    parser_names: dict[str, tuple[str, ...]] = {}
    for node in ast.walk(builders[0]):
        if not (
            isinstance(node, ast.Assign)
            and len(node.targets) == 1
            and isinstance(node.targets[0], ast.Name)
            and isinstance(node.value, ast.Call)
            and isinstance(node.value.func, ast.Attribute)
            and node.value.func.attr == "add_parser"
        ):
            continue
        parser_variable = node.targets[0].id
        if parser_variable in parser_names:
            raise SkillContractError(
                f"bulkload CLI parser variable is reused: {parser_variable}"
            )
        call = node.value
        consumed_parser_calls.add(id(call))
        if (
            not call.args
            or not isinstance(call.args[0], ast.Constant)
            or not isinstance(call.args[0].value, str)
        ):
            raise SkillContractError("bulkload CLI command names must be literal")
        names = [call.args[0].value]
        aliases = next(
            (keyword.value for keyword in call.keywords if keyword.arg == "aliases"),
            None,
        )
        if aliases is not None:
            if not isinstance(aliases, (ast.List, ast.Tuple)):
                raise SkillContractError("bulkload CLI aliases must be literal")
            for alias in aliases.elts:
                if not isinstance(alias, ast.Constant) or not isinstance(
                    alias.value,
                    str,
                ):
                    raise SkillContractError("bulkload CLI aliases must be strings")
                names.append(alias.value)
        parser_names[parser_variable] = tuple(names)
    if consumed_parser_calls != all_parser_calls:
        raise SkillContractError(
            "every bulkload CLI add_parser call must use one direct assignment"
        )
    all_handler_calls = {
        id(node)
        for node in ast.walk(builders[0])
        if isinstance(node, ast.Call)
        and isinstance(node.func, ast.Attribute)
        and node.func.attr == "set_defaults"
    }
    consumed_handler_calls: set[int] = set()
    handlers_by_parser: dict[str, str] = {}
    for node in ast.walk(builders[0]):
        if not (
            isinstance(node, ast.Call)
            and isinstance(node.func, ast.Attribute)
            and node.func.attr == "set_defaults"
            and isinstance(node.func.value, ast.Name)
            and node.func.value.id in parser_names
        ):
            continue
        parser_variable = node.func.value.id
        consumed_handler_calls.add(id(node))
        handler_values = [
            keyword.value for keyword in node.keywords if keyword.arg == "handler"
        ]
        if (
            len(handler_values) != 1
            or not isinstance(handler_values[0], ast.Name)
            or parser_variable in handlers_by_parser
        ):
            raise SkillContractError(
                f"bulkload CLI handler binding is invalid: {parser_variable}"
            )
        handlers_by_parser[parser_variable] = handler_values[0].id
    if consumed_handler_calls != all_handler_calls:
        raise SkillContractError(
            "every bulkload CLI set_defaults call must bind one assigned parser"
        )
    if set(handlers_by_parser) != set(parser_names):
        raise SkillContractError("every bulkload CLI parser must bind one handler")
    command_handlers: dict[str, str] = {}
    for parser_variable, names in parser_names.items():
        for name in names:
            if name in command_handlers:
                raise SkillContractError(
                    f"bulkload CLI command or alias is duplicated: {name}"
                )
            command_handlers[name] = handlers_by_parser[parser_variable]
    return command_handlers


def validate_private_state_policy(
    policy: object,
    *,
    cli_text: str,
    private_state_text: str,
    runtime_sha256: str,
    expected_cli_sha256: str | None = None,
    expected_private_state_sha256: str | None = None,
    expected_runtime_sha256: str | None = None,
) -> None:
    if policy != EXPECTED_PRIVATE_STATE_POLICY:
        raise SkillContractError(
            "Codex private-state policy does not match the exact v2 contract"
        )
    required_cli_sha256 = (
        EXPECTED_PRIVATE_STATE_POLICY["cli_source_sha256"]
        if expected_cli_sha256 is None
        else expected_cli_sha256
    )
    observed_cli_sha256 = hashlib.sha256(cli_text.encode("utf-8")).hexdigest()
    if observed_cli_sha256 != required_cli_sha256:
        raise SkillContractError(
            "bulkload CLI source differs from the exact private-state policy digest: "
            f"expected={required_cli_sha256} observed={observed_cli_sha256}"
        )
    required_private_state_sha256 = (
        EXPECTED_PRIVATE_STATE_POLICY["private_state_source_sha256"]
        if expected_private_state_sha256 is None
        else expected_private_state_sha256
    )
    observed_private_state_sha256 = hashlib.sha256(
        private_state_text.encode("utf-8")
    ).hexdigest()
    if observed_private_state_sha256 != required_private_state_sha256:
        raise SkillContractError(
            "Bulkload private-state implementation differs from the exact policy "
            f"digest: expected={required_private_state_sha256} "
            f"observed={observed_private_state_sha256}"
        )
    required_runtime_sha256 = (
        EXPECTED_PRIVATE_STATE_POLICY["runtime_source_sha256"]
        if expected_runtime_sha256 is None
        else expected_runtime_sha256
    )
    if runtime_sha256 != required_runtime_sha256:
        raise SkillContractError(
            "Bulkload runtime dependency closure differs from the exact policy "
            f"digest: expected={required_runtime_sha256} observed={runtime_sha256}"
        )
    observed_handlers = cli_command_handlers(cli_text)
    if observed_handlers != EXPECTED_CLI_COMMAND_HANDLERS:
        raise SkillContractError(
            "bulkload CLI command/alias/handler topology differs from the exact "
            f"private-state contract: expected={EXPECTED_CLI_COMMAND_HANDLERS} "
            f"observed={observed_handlers}"
        )
    allowed_commands = set(EXPECTED_PRIVATE_STATE_POLICY["allowed_codex_cli_commands"])
    observed_commands = {
        name for name in observed_handlers if name.startswith("codex-")
    }
    if observed_commands != allowed_commands:
        raise SkillContractError(
            "bulkload Codex CLI surface differs from the exact policy allowlist: "
            f"expected={sorted(allowed_commands)} observed={sorted(observed_commands)}"
        )


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

    policy_file = skill_root / "references" / PRIVATE_STATE_POLICY_NAME
    try:
        policy = parse_strict_json_object(policy_file.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError, ValueError) as error:
        raise SkillContractError(
            f"cannot read exact Codex private-state policy: {error}"
        ) from error
    cli_file = skill_root / "scripts" / "bulkload_lib" / "cli.py"
    private_state_file = skill_root / "scripts" / "bulkload_lib" / "private_state.py"
    try:
        cli_text = cli_file.read_text(encoding="utf-8")
        private_state_text = private_state_file.read_text(encoding="utf-8")
    except OSError as error:
        raise SkillContractError(
            f"cannot read private-state implementation: {error}"
        ) from error
    validate_private_state_policy(
        policy,
        cli_text=cli_text,
        private_state_text=private_state_text,
        runtime_sha256=runtime_source_digest(skill_root),
    )

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
    valid_private_state_text = "def capture_and_plan_only():\n    return False\n"
    valid_cli_text = """
def build_parser():
    capture = commands.add_parser("capture")
    capture.set_defaults(handler=_capture)
    plan = commands.add_parser("plan")
    plan.set_defaults(handler=_plan)
    apply = commands.add_parser("apply")
    apply.set_defaults(handler=_apply)
    verify = commands.add_parser("verify")
    verify.set_defaults(handler=_verify)
    files = commands.add_parser("files")
    files.set_defaults(handler=_files)
    codex_capture = commands.add_parser("codex-capture")
    codex_capture.set_defaults(handler=_codex_capture)
    codex_close_capture = commands.add_parser("codex-close-capture")
    codex_close_capture.set_defaults(handler=_codex_close_capture)
    codex_close_request = commands.add_parser("codex-close-request")
    codex_close_request.set_defaults(handler=_codex_close_request)
    codex_plan = commands.add_parser("codex-plan")
    codex_plan.set_defaults(handler=_codex_plan)
    codex_prefix_proof = commands.add_parser("codex-prefix-proof")
    codex_prefix_proof.set_defaults(handler=_codex_prefix_proof)
    codex_prefix_request = commands.add_parser(
        "codex-prefix-request",
        aliases=("codex-prefix-requests",),
    )
    codex_prefix_request.set_defaults(handler=_codex_prefix_request)
    codex_private_capture = commands.add_parser("codex-private-capture")
    codex_private_capture.set_defaults(handler=_codex_private_capture)
    codex_private_plan = commands.add_parser("codex-private-plan")
    codex_private_plan.set_defaults(handler=_codex_private_plan)
    doctor = commands.add_parser("doctor")
    doctor.set_defaults(handler=_doctor)
"""
    valid_runtime_sources = {
        "scripts/bulkload.py": "from bulkload_lib.cli import main\n",
        "scripts/bulkload_lib/cli.py": valid_cli_text,
        "scripts/bulkload_lib/private_state.py": valid_private_state_text,
    }
    valid_runtime_sha256 = runtime_source_digest_from_texts(valid_runtime_sources)
    validate_private_state_policy(
        copy.deepcopy(EXPECTED_PRIVATE_STATE_POLICY),
        cli_text=valid_cli_text,
        private_state_text=valid_private_state_text,
        runtime_sha256=valid_runtime_sha256,
        expected_cli_sha256=hashlib.sha256(valid_cli_text.encode("utf-8")).hexdigest(),
        expected_private_state_sha256=hashlib.sha256(
            valid_private_state_text.encode("utf-8")
        ).hexdigest(),
        expected_runtime_sha256=valid_runtime_sha256,
    )
    invalid_policy = copy.deepcopy(EXPECTED_PRIVATE_STATE_POLICY)
    invalid_policy["ready_for_apply"] = True
    try:
        validate_private_state_policy(
            invalid_policy,
            cli_text=valid_cli_text,
            private_state_text=valid_private_state_text,
            runtime_sha256=valid_runtime_sha256,
            expected_cli_sha256=hashlib.sha256(
                valid_cli_text.encode("utf-8")
            ).hexdigest(),
            expected_private_state_sha256=hashlib.sha256(
                valid_private_state_text.encode("utf-8")
            ).hexdigest(),
            expected_runtime_sha256=valid_runtime_sha256,
        )
    except SkillContractError:
        pass
    else:
        raise AssertionError("apply-ready private-state policy was accepted")
    forbidden_surfaces = (
        valid_cli_text
        + """
    state_apply = commands.add_parser("state-apply")
    state_apply.set_defaults(handler=_codex_state_apply)
""",
        valid_cli_text.replace(
            'apply = commands.add_parser("apply")',
            'apply = commands.add_parser("apply", aliases=("state-apply",))',
        ),
        valid_cli_text.replace(
            "apply.set_defaults(handler=_apply)",
            "apply.set_defaults(handler=_codex_state_apply)",
        ),
        valid_cli_text
        + """
    codex_state_apply = commands.add_parser("codex-state-apply")
    codex_state_apply.set_defaults(handler=_codex_state_apply)
""",
        valid_cli_text
        + """
    dynamic = commands.add_parser(command_name)
    dynamic.set_defaults(handler=_dynamic)
""",
        valid_cli_text
        + """
    commands.add_parser("state-apply").set_defaults(
        handler=_codex_state_apply,
    )
""",
        valid_cli_text
        + """
    state_apply: object = commands.add_parser("state-apply")
    state_apply.set_defaults(handler=_codex_state_apply)
""",
        valid_cli_text
        + """
    (state_apply := commands.add_parser("state-apply")).set_defaults(
        handler=_codex_state_apply,
    )
""",
    )
    for forbidden_cli_text in forbidden_surfaces:
        try:
            validate_private_state_policy(
                copy.deepcopy(EXPECTED_PRIVATE_STATE_POLICY),
                cli_text=forbidden_cli_text,
                private_state_text=valid_private_state_text,
                runtime_sha256=valid_runtime_sha256,
                expected_cli_sha256=hashlib.sha256(
                    valid_cli_text.encode("utf-8")
                ).hexdigest(),
                expected_private_state_sha256=hashlib.sha256(
                    valid_private_state_text.encode("utf-8")
                ).hexdigest(),
                expected_runtime_sha256=valid_runtime_sha256,
            )
        except SkillContractError:
            continue
        raise AssertionError(
            "private-state CLI command/alias/handler topology was accepted"
        )
    try:
        validate_private_state_policy(
            copy.deepcopy(EXPECTED_PRIVATE_STATE_POLICY),
            cli_text=valid_cli_text,
            private_state_text=valid_private_state_text + "# mutation\n",
            runtime_sha256=valid_runtime_sha256,
            expected_cli_sha256=hashlib.sha256(
                valid_cli_text.encode("utf-8")
            ).hexdigest(),
            expected_private_state_sha256=hashlib.sha256(
                valid_private_state_text.encode("utf-8")
            ).hexdigest(),
            expected_runtime_sha256=valid_runtime_sha256,
        )
    except SkillContractError:
        pass
    else:
        raise AssertionError("mutated private-state implementation was accepted")
    mutated_runtime_sources = dict(valid_runtime_sources)
    mutated_runtime_sources["scripts/bulkload_lib/model.py"] = "def mutate(): pass\n"
    try:
        validate_private_state_policy(
            copy.deepcopy(EXPECTED_PRIVATE_STATE_POLICY),
            cli_text=valid_cli_text,
            private_state_text=valid_private_state_text,
            runtime_sha256=runtime_source_digest_from_texts(mutated_runtime_sources),
            expected_cli_sha256=hashlib.sha256(
                valid_cli_text.encode("utf-8")
            ).hexdigest(),
            expected_private_state_sha256=hashlib.sha256(
                valid_private_state_text.encode("utf-8")
            ).hexdigest(),
            expected_runtime_sha256=valid_runtime_sha256,
        )
    except SkillContractError:
        pass
    else:
        raise AssertionError("mutated runtime dependency closure was accepted")
    for invalid_json in (
        '{"schema":"first","schema":"second"}',
        '{"value":NaN}',
        '["not-an-object"]',
    ):
        try:
            parse_strict_json_object(invalid_json)
        except (json.JSONDecodeError, ValueError):
            continue
        raise AssertionError(f"non-strict policy JSON was accepted: {invalid_json}")


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
