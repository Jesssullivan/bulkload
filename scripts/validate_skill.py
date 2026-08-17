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
PRIVATE_STATE_POLICY_NAME: Final = "codex-private-state-policy.v5.json"
PRIVATE_SOURCE_PATHS: Final = (
    "scripts/bulkload_lib/cli.py",
    "scripts/bulkload_lib/private_apply.py",
    "scripts/bulkload_lib/private_quiescence.py",
    "scripts/bulkload_lib/private_sqlite_action_plan.py",
    "scripts/bulkload_lib/private_sqlite_close.py",
    "scripts/bulkload_lib/private_sqlite_plan.py",
    "scripts/bulkload_lib/private_state.py",
    "scripts/bulkload_lib/sessions.py",
)
EXPECTED_PRIVATE_STATE_POLICY: Final[dict[str, Any]] = {
    "schema": "dev.tinyland.bulkload.codex-private-state-policy.v5",
    "implementation": "auth-atomic-replace-sqlite-action-plan-v5",
    "readiness": {
        "auth_install": True,
        "combined": False,
        "sqlite_compose_action_plan": True,
        "sqlite_compose_plan": True,
        "sqlite_compose": False,
        "sqlite_publish": False,
    },
    "source_digests": {
        "scripts/bulkload_lib/cli.py": (
            "5d5aa2b9d9a8f04413a49dff92a8c4eb545d54a2970d9b5371b7b52bac9e4238"
        ),
        "scripts/bulkload_lib/private_apply.py": (
            "4cfbc1173468884c2098da2397bf9b5c1f8c51251bc85ed66ed3170524a507de"
        ),
        "scripts/bulkload_lib/private_quiescence.py": (
            "7b7e3e110f030a536f2b74bfc0079d59681b5af740f68859e96596f22d3d0423"
        ),
        "scripts/bulkload_lib/private_sqlite_action_plan.py": (
            "7da422b4fd63b8c9fbf78797f9a27684648cdf165ac93c4dca26bb3ddf4ba2e3"
        ),
        "scripts/bulkload_lib/private_sqlite_close.py": (
            "676f71c01fc1151a1019bbef2f7b42d4cf0e0b933ce3487f2529efe4e9a1d864"
        ),
        "scripts/bulkload_lib/private_sqlite_plan.py": (
            "e45dc732fe1a208bbbea1455d25435754d49397d9a0bea471d5bd5265321398c"
        ),
        "scripts/bulkload_lib/private_state.py": (
            "67a764a4ca8783517c94bd98d5f41af7f42e35a40755361225d316fc9419ba01"
        ),
        "scripts/bulkload_lib/sessions.py": (
            "35982ce66e2bcfc2b5c82ffc47560f43665aa5fe680681be57df994b8503b735"
        ),
    },
    "runtime_source_sha256": (
        "cc9f96adb8189e0a41133244231edf51dd837fa75459728355b93eeb981c7392"
    ),
    "allowed_codex_cli_commands": [
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
        "codex-private-sqlite-close-request",
        "codex-private-sqlite-compose-action-plan",
        "codex-private-sqlite-compose-plan",
        "codex-private-sqlite-private-reclose",
        "codex-private-sqlite-session-reclose",
        "codex-private-verify",
    ],
    "state_classes": {
        "auth_file": {
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
            "authority": "CODEX_HOME/auth.json",
            "classification": "copy-eligible",
            "copy_method": "pinned-private-file-atomic-replace",
            "default": "opt-in",
            "destination_evidence": "fresh-auth-and-sqlite-preservation-capture",
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
            "live_sidecars": "reject-wal-shm-journal",
            "post_plan_close_required": True,
            "preservation_implemented": True,
            "provider_writer_proof": False,
            "publisher_implemented": False,
            "raw_database_wal_shm_copy": False,
            "reader_implemented": True,
            "runtime_acceptance_required": True,
            "session_union_execution_verified": False,
            "source_sqlite_required_for_auth_install": False,
            "sqlite_compose_action_plan_implemented": True,
            "sqlite_compose_plan_implemented": True,
            "sqlite_compose_plan_scope": "opening-v4-close-action-plan-v5",
            "post_plan_close_implemented": True,
            "wal_aware_capture": False,
        },
    },
    "forbidden_commands": [
        "codex-private-combined-apply",
        "codex-private-sqlite-compose",
        "codex-state-apply",
    ],
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
    "codex-private-quiescence-attest": "_codex_private_quiescence_attest",
    "codex-private-plan": "_codex_private_plan",
    "codex-private-install-plan": "_codex_private_install_plan",
    "codex-private-apply": "_codex_private_apply",
    "codex-private-verify": "_codex_private_verify",
    "codex-private-rollback": "_codex_private_rollback",
    "codex-private-recover": "_codex_private_recover",
    "codex-private-sqlite-close-request": ("_codex_private_sqlite_close_request"),
    "codex-private-sqlite-compose-action-plan": (
        "_codex_private_sqlite_compose_action_plan"
    ),
    "codex-private-sqlite-compose-plan": ("_codex_private_sqlite_compose_plan"),
    "codex-private-sqlite-private-reclose": ("_codex_private_sqlite_private_reclose"),
    "codex-private-sqlite-session-reclose": ("_codex_private_sqlite_session_reclose"),
    "doctor": "_doctor",
    "files": "_files",
    "plan": "_plan",
    "verify": "_verify",
}


class SkillContractError(ValueError):
    """Raised when a skill violates its portable contract."""


def runtime_source_digest_from_payloads(sources: dict[str, bytes]) -> str:
    inventory = {
        path: hashlib.sha256(payload).hexdigest()
        for path, payload in sorted(sources.items())
    }
    payload = json.dumps(
        inventory,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode("utf-8")
    return hashlib.sha256(payload).hexdigest()


def runtime_source_digest_from_texts(sources: dict[str, str]) -> str:
    return runtime_source_digest_from_payloads(
        {path: text.encode("utf-8") for path, text in sources.items()}
    )


def runtime_source_digest(skill_root: Path) -> str:
    scripts_root = skill_root / "scripts"
    paths = sorted(scripts_root.rglob("*.py"))
    sources: dict[str, bytes] = {}
    for path in paths:
        try:
            relative = path.relative_to(skill_root).as_posix()
            sources[relative] = path.read_bytes()
            sources[relative].decode("utf-8")
        except (OSError, UnicodeDecodeError, ValueError) as error:
            raise SkillContractError(
                f"cannot read Bulkload runtime source {path}: {error}"
            ) from error
    return runtime_source_digest_from_payloads(sources)


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
    source_payloads: dict[str, bytes] | None = None,
    runtime_sha256: str,
    expected_policy: dict[str, Any] = EXPECTED_PRIVATE_STATE_POLICY,
    private_state_text: str | None = None,
    expected_cli_sha256: str | None = None,
    expected_private_state_sha256: str | None = None,
    expected_runtime_sha256: str | None = None,
) -> None:
    del (
        expected_cli_sha256,
        expected_private_state_sha256,
        expected_runtime_sha256,
    )
    if source_payloads is None:
        source_payloads = {
            "scripts/bulkload_lib/cli.py": cli_text.encode("utf-8"),
            "scripts/bulkload_lib/private_apply.py": b"legacy-self-test-only\n",
            "scripts/bulkload_lib/private_quiescence.py": b"legacy-self-test-only\n",
            "scripts/bulkload_lib/private_sqlite_action_plan.py": (
                b"legacy-self-test-only\n"
            ),
            "scripts/bulkload_lib/private_sqlite_close.py": (
                b"legacy-self-test-only\n"
            ),
            "scripts/bulkload_lib/private_sqlite_plan.py": (b"legacy-self-test-only\n"),
            "scripts/bulkload_lib/private_state.py": (
                (private_state_text or "").encode("utf-8")
            ),
            "scripts/bulkload_lib/sessions.py": b"legacy-self-test-only\n",
        }
    if policy != expected_policy:
        raise SkillContractError(
            "Codex private-state policy does not match the exact v5 contract"
        )
    if set(source_payloads) != set(PRIVATE_SOURCE_PATHS):
        raise SkillContractError(
            "private-state core source inventory differs from the v5 contract"
        )
    for relative in PRIVATE_SOURCE_PATHS:
        observed = hashlib.sha256(source_payloads[relative]).hexdigest()
        expected = policy["source_digests"][relative]
        if observed != expected:
            raise SkillContractError(
                "Bulkload private-state source differs from the exact policy "
                f"digest: path={relative} expected={expected} observed={observed}"
            )
    if runtime_sha256 != policy["runtime_source_sha256"]:
        raise SkillContractError(
            "Bulkload runtime dependency closure differs from the exact policy "
            f"digest: expected={policy['runtime_source_sha256']} "
            f"observed={runtime_sha256}"
        )
    observed_handlers = cli_command_handlers(cli_text)
    if observed_handlers != EXPECTED_CLI_COMMAND_HANDLERS:
        raise SkillContractError(
            "bulkload CLI command/alias/handler topology differs from the exact "
            f"private-state contract: expected={EXPECTED_CLI_COMMAND_HANDLERS} "
            f"observed={observed_handlers}"
        )
    allowed_commands = set(policy["allowed_codex_cli_commands"])
    observed_commands = {
        name for name in observed_handlers if name.startswith("codex-")
    }
    if observed_commands != allowed_commands:
        raise SkillContractError(
            "bulkload Codex CLI surface differs from the exact policy allowlist: "
            f"expected={sorted(allowed_commands)} observed={sorted(observed_commands)}"
        )
    if observed_commands & set(policy["forbidden_commands"]):
        raise SkillContractError(
            "bulkload CLI exposes a command forbidden by private-state policy"
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
    source_payloads: dict[str, bytes] = {}
    try:
        for relative in PRIVATE_SOURCE_PATHS:
            payload = (skill_root / relative).read_bytes()
            payload.decode("utf-8")
            source_payloads[relative] = payload
        cli_text = source_payloads["scripts/bulkload_lib/cli.py"].decode("utf-8")
    except (OSError, UnicodeDecodeError) as error:
        raise SkillContractError(
            f"cannot read private-state implementation: {error}"
        ) from error
    validate_private_state_policy(
        policy,
        cli_text=cli_text,
        source_payloads=source_payloads,
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

    command_lines = [
        "def build_parser():",
        *[
            (
                f'    parser_{index} = commands.add_parser("{name}")\n'
                f"    parser_{index}.set_defaults(handler={handler})"
            )
            for index, (name, handler) in enumerate(
                (item for item in EXPECTED_CLI_COMMAND_HANDLERS.items())
            )
            if name != "codex-prefix-requests"
        ],
    ]
    valid_cli_text = "\n".join(command_lines).replace(
        'commands.add_parser("codex-prefix-request")',
        'commands.add_parser("codex-prefix-request", '
        'aliases=("codex-prefix-requests",))',
    )
    source_texts = {
        "scripts/bulkload_lib/cli.py": valid_cli_text,
        "scripts/bulkload_lib/private_apply.py": "def apply(): return None\n",
        "scripts/bulkload_lib/private_quiescence.py": "def attest(): return None\n",
        "scripts/bulkload_lib/private_sqlite_action_plan.py": (
            "def compile_action_plan(): return None\n"
        ),
        "scripts/bulkload_lib/private_sqlite_close.py": (
            "def compile_close_request(): return None\n"
        ),
        "scripts/bulkload_lib/private_sqlite_plan.py": (
            "def compile_plan(): return None\n"
        ),
        "scripts/bulkload_lib/private_state.py": "def capture(): return None\n",
        "scripts/bulkload_lib/sessions.py": "def sessions(): return None\n",
    }
    source_payloads = {
        path: text.encode("utf-8") for path, text in source_texts.items()
    }
    runtime_sources = {
        "scripts/bulkload.py": b"print('fixture')\n",
        "scripts/bulkload_lib/__init__.py": b'"""fixture"""\n',
        **source_payloads,
        "scripts/bulkload_lib/model.py": b"VALUE = 1\n",
    }
    fixture_policy = copy.deepcopy(EXPECTED_PRIVATE_STATE_POLICY)
    fixture_policy["source_digests"] = {
        path: hashlib.sha256(payload).hexdigest()
        for path, payload in source_payloads.items()
    }
    fixture_policy["runtime_source_sha256"] = runtime_source_digest_from_payloads(
        runtime_sources
    )
    validate_private_state_policy(
        fixture_policy,
        cli_text=valid_cli_text,
        source_payloads=source_payloads,
        runtime_sha256=fixture_policy["runtime_source_sha256"],
        expected_policy=fixture_policy,
    )

    policy_mutations = (
        lambda value: value["readiness"].__setitem__("combined", True),
        lambda value: value["allowed_codex_cli_commands"].append(
            "codex-private-combined-apply"
        ),
        lambda value: value["forbidden_commands"].pop(),
        lambda value: value["state_classes"]["sqlite_families"].__setitem__(
            "composer_implemented",
            True,
        ),
    )
    for mutate in policy_mutations:
        changed = copy.deepcopy(fixture_policy)
        mutate(changed)
        try:
            validate_private_state_policy(
                changed,
                cli_text=valid_cli_text,
                source_payloads=source_payloads,
                runtime_sha256=fixture_policy["runtime_source_sha256"],
                expected_policy=fixture_policy,
            )
        except SkillContractError:
            continue
        raise AssertionError("private-state policy semantic drift was accepted")

    mutated_cli = valid_cli_text + (
        '\n    extra = commands.add_parser("codex-private-combined-apply")\n'
        "    extra.set_defaults(handler=_forbidden)\n"
    )
    mutated_payloads = dict(source_payloads)
    mutated_payloads["scripts/bulkload_lib/cli.py"] = mutated_cli.encode("utf-8")
    mutated_policy = copy.deepcopy(fixture_policy)
    mutated_policy["source_digests"]["scripts/bulkload_lib/cli.py"] = hashlib.sha256(
        mutated_cli.encode("utf-8")
    ).hexdigest()
    mutated_runtime = dict(runtime_sources)
    mutated_runtime["scripts/bulkload_lib/cli.py"] = mutated_cli.encode("utf-8")
    mutated_policy["runtime_source_sha256"] = runtime_source_digest_from_payloads(
        mutated_runtime
    )
    try:
        validate_private_state_policy(
            mutated_policy,
            cli_text=mutated_cli,
            source_payloads=mutated_payloads,
            runtime_sha256=mutated_policy["runtime_source_sha256"],
            expected_policy=mutated_policy,
        )
    except SkillContractError:
        pass
    else:
        raise AssertionError("forbidden private CLI surface was accepted")

    changed_runtime = dict(runtime_sources)
    changed_runtime["scripts/bulkload_lib/model.py"] += b"# drift\n"
    try:
        validate_private_state_policy(
            fixture_policy,
            cli_text=valid_cli_text,
            source_payloads=source_payloads,
            runtime_sha256=runtime_source_digest_from_payloads(changed_runtime),
            expected_policy=fixture_policy,
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
