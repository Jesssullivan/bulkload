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
PRIVATE_STATE_POLICY_NAME: Final = "codex-private-state-policy.v7.json"
PRIVATE_SOURCE_PATHS: Final = (
    "scripts/bulkload.py",
    "scripts/bulkload_lib/__init__.py",
    "scripts/bulkload_lib/cli.py",
    "scripts/bulkload_lib/executor.py",
    "scripts/bulkload_lib/model.py",
    "scripts/bulkload_lib/planner.py",
    "scripts/bulkload_lib/private_apply.py",
    "scripts/bulkload_lib/private_quiescence.py",
    "scripts/bulkload_lib/private_runtime.py",
    "scripts/bulkload_lib/private_sqlite_action_plan.py",
    "scripts/bulkload_lib/private_sqlite_close.py",
    "scripts/bulkload_lib/private_sqlite_plan.py",
    "scripts/bulkload_lib/private_sqlite_protocol.py",
    "scripts/bulkload_lib/private_sqlite_request.py",
    "scripts/bulkload_lib/private_sqlite_verifier.py",
    "scripts/bulkload_lib/private_state.py",
    "scripts/bulkload_lib/scanner.py",
    "scripts/bulkload_lib/sessions.py",
)
EXPECTED_PRIVATE_STATE_POLICY: Final[dict[str, Any]] = {
    "schema": "dev.tinyland.bulkload.codex-private-state-policy.v7",
    "implementation": "auth-atomic-replace-sqlite-verifier-oracle-v7",
    "readiness": {
        "auth_install": True,
        "combined": False,
        "sqlite_compose_action_plan": False,
        "sqlite_compose_request": False,
        "sqlite_capacity_observation": False,
        "sqlite_compose_plan": False,
        "sqlite_verifier_oracle_internal_only": True,
        "sqlite_independent_verification": False,
        "sqlite_compose": False,
        "sqlite_publish": False,
    },
    "source_digests": {
        "scripts/bulkload.py": (
            "50f16b69a7b296f4e067c8f02ec9716ad7888286ebbf3c8da3af5f060cabce62"
        ),
        "scripts/bulkload_lib/__init__.py": (
            "de2a7f4d6ec3468db41cb3b643469ec445cbbaf386f97230e4f72b7ebc3dd89f"
        ),
        "scripts/bulkload_lib/cli.py": (
            "d408d9c57ae05fcea01b6a9378559bacf9d1c0906a686629fabc1c54b31b6299"
        ),
        "scripts/bulkload_lib/executor.py": (
            "f4c48bc4e02ec8eb8efe164bd2523855ff82a71819e155b93803542da0364201"
        ),
        "scripts/bulkload_lib/model.py": (
            "6099e031801ac56038d0e1d9a76baefab057551f885840a5b7c3eee0abb6ac92"
        ),
        "scripts/bulkload_lib/planner.py": (
            "24ea53940dce472521460734bf31236380ea632fbfd4afc948c105b5a2473eb3"
        ),
        "scripts/bulkload_lib/private_apply.py": (
            "4cfbc1173468884c2098da2397bf9b5c1f8c51251bc85ed66ed3170524a507de"
        ),
        "scripts/bulkload_lib/private_quiescence.py": (
            "7b7e3e110f030a536f2b74bfc0079d59681b5af740f68859e96596f22d3d0423"
        ),
        "scripts/bulkload_lib/private_runtime.py": (
            "0ae2926aaa4a3383875c2e7822feeae796a0c7b77ed4cf01ee1bff7b7f44b0ba"
        ),
        "scripts/bulkload_lib/private_sqlite_action_plan.py": (
            "7262972ec15668c9de3d0a6fb188a1e13c60a9e482fb83ac07fe96df7f35a8d6"
        ),
        "scripts/bulkload_lib/private_sqlite_close.py": (
            "e7619be6349cb3d47cc71a93cb175d290a5b1b42ff28380163cd07f8435cf642"
        ),
        "scripts/bulkload_lib/private_sqlite_plan.py": (
            "465e24b4d28bf2c296a292080d3c92a59ae622180921cc63fbce6e5c576e8f18"
        ),
        "scripts/bulkload_lib/private_sqlite_protocol.py": (
            "70cdc2d86138124aa33499567f8fd4e9456b1d6fbbb5bc2fea61fa16f9a448ea"
        ),
        "scripts/bulkload_lib/private_sqlite_request.py": (
            "1f4f30ce3b648b8cb0bcaa3ae76b43fa0ff50d26471c3a24c7d3c347621c67c3"
        ),
        "scripts/bulkload_lib/private_sqlite_verifier.py": (
            "18e5e82d66b9252804bd41e5688994be37f0ded6d2d68a1c7e7305b0e7f96bbf"
        ),
        "scripts/bulkload_lib/private_state.py": (
            "67a764a4ca8783517c94bd98d5f41af7f42e35a40755361225d316fc9419ba01"
        ),
        "scripts/bulkload_lib/scanner.py": (
            "813c49326f1ba3dfee82ef37de6671fe9fb46d771a8f19c45a604c55a4d4b9b5"
        ),
        "scripts/bulkload_lib/sessions.py": (
            "30342175e2aea9f3c08a801523529b99438e2447241918297c804124268279f7"
        ),
    },
    "runtime_source_sha256": (
        "4cb514730f08371cffc00e69d1607bb6b73f464b6fd0c5d0ca894ffdca31be4f"
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
            "legacy_sqlite_opening_validator_implemented": True,
            "legacy_sqlite_close_action_validator_implemented": True,
            "sqlite_compose_action_plan_implemented": False,
            "sqlite_compose_request_implemented": False,
            "capacity_observation_implemented": False,
            "sqlite_verifier_oracle_internal_only": True,
            "independent_verification_receipt_implemented": False,
            "offline_bundle_writer_implemented": False,
            "workspace_reservation_implemented": False,
            "sqlite_compose_plan_implemented": False,
            "sqlite_compose_request_scope": "frozen-v6-validator-only",
            "post_plan_close_implemented": False,
            "wal_aware_capture": False,
        },
    },
    "forbidden_commands": [
        "codex-private-combined-apply",
        "codex-private-sqlite-capacity-observe",
        "codex-private-sqlite-compose",
        "codex-private-sqlite-compose-request",
        "codex-private-sqlite-install",
        "codex-private-sqlite-oracle",
        "codex-private-sqlite-publish",
        "codex-private-sqlite-verifier-oracle",
        "codex-private-sqlite-verify",
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
    "doctor": "_doctor",
    "files": "_files",
    "plan": "_plan",
    "verify": "_verify",
}
EXPECTED_ORACLE_FALSE_CLAIMS: Final[set[str]] = {
    "final_leaf_commit_observed",
    "bundle_sealed",
    "full_against_inputs_recomputed",
    "independent_verification_complete",
    "offline_bundle_verified",
    "composer_implemented",
    "ready_for_internal_offline_compose",
    "ready_for_offline_compose",
    "sqlite_compose",
    "sqlite_publish",
    "published",
    "publication_authorized",
    "installed",
    "install_authorized",
    "session_union_executed",
    "sqlite_union_ready",
    "provider_runtime_acceptance",
    "provider_runtime_acceptance_verified",
    "provider_writer_proof",
    "combined",
    "combined_authorized",
    "ready_for_apply",
    "apply_authorized",
    "engine_diversity_verified",
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


def validate_runtime_source_path_inventory(observed: set[str]) -> None:
    expected = set(PRIVATE_SOURCE_PATHS)
    forbidden_leaves = {
        "private_sqlite_composer.py",
        "private_sqlite_final_verifier.py",
        "private_sqlite_publisher.py",
        "private_sqlite_writer.py",
    }
    if (
        observed != expected
        or {PurePosixPath(path).name for path in observed} & forbidden_leaves
    ):
        raise SkillContractError(
            "Bulkload runtime source path inventory differs from the exact "
            "writer-free v7 contract"
        )


def runtime_source_digest(skill_root: Path) -> str:
    scripts_root = skill_root / "scripts"
    paths = sorted(scripts_root.rglob("*.py"))
    observed = {path.relative_to(skill_root).as_posix() for path in paths}
    validate_runtime_source_path_inventory(observed)
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
        for node in ast.walk(tree)
        if isinstance(node, ast.Call)
        and isinstance(node.func, ast.Attribute)
        and node.func.attr == "add_parser"
    }
    all_parser_attributes = {
        id(node)
        for node in ast.walk(tree)
        if isinstance(node, ast.Attribute)
        and isinstance(node.ctx, ast.Load)
        and node.attr == "add_parser"
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
    consumed_parser_attributes = {
        id(node.value.func)
        for node in ast.walk(builders[0])
        if isinstance(node, ast.Assign)
        and len(node.targets) == 1
        and isinstance(node.targets[0], ast.Name)
        and isinstance(node.value, ast.Call)
        and isinstance(node.value.func, ast.Attribute)
        and node.value.func.attr == "add_parser"
    }
    if (
        consumed_parser_calls != all_parser_calls
        or consumed_parser_attributes != all_parser_attributes
    ):
        raise SkillContractError(
            "every bulkload CLI add_parser call must use one direct assignment"
        )
    all_handler_calls = {
        id(node)
        for node in ast.walk(tree)
        if isinstance(node, ast.Call)
        and isinstance(node.func, ast.Attribute)
        and node.func.attr == "set_defaults"
    }
    all_handler_attributes = {
        id(node)
        for node in ast.walk(tree)
        if isinstance(node, ast.Attribute)
        and isinstance(node.ctx, ast.Load)
        and node.attr == "set_defaults"
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
    consumed_handler_attributes = {
        id(node.func)
        for node in ast.walk(builders[0])
        if isinstance(node, ast.Call)
        and isinstance(node.func, ast.Attribute)
        and node.func.attr == "set_defaults"
        and isinstance(node.func.value, ast.Name)
        and node.func.value.id in parser_names
    }
    if (
        consumed_handler_calls != all_handler_calls
        or consumed_handler_attributes != all_handler_attributes
    ):
        raise SkillContractError(
            "every bulkload CLI set_defaults call must bind one assigned parser"
        )
    if any(
        isinstance(node, ast.Call)
        and isinstance(node.func, ast.Name)
        and node.func.id == "getattr"
        and len(node.args) >= 2
        and isinstance(node.args[1], ast.Constant)
        and node.args[1].value in {"add_parser", "set_defaults"}
        for node in ast.walk(tree)
    ):
        raise SkillContractError(
            "bulkload CLI parser and handler registration cannot use getattr"
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


def validate_retired_sqlite_producer_sources(
    source_texts: dict[str, str],
) -> None:
    """Keep frozen v4-v6 artifacts validator-only in the active v7 source."""
    retired_private_helpers = {
        "_observe_frozen_v5_session_reclose",
        "_recompute_frozen_v4_compose_plan",
        "_recompute_frozen_v5_action_plan",
        "_recompute_frozen_v5_close_request",
        "_recompute_frozen_v6_capacity_observation",
        "_recompute_frozen_v6_compose_request",
    }
    forbidden_public_producers = {
        "capture_codex_private_sqlite_session_reclose",
        "compile_codex_private_sqlite_action_plan",
        "compile_codex_private_sqlite_capacity_observation",
        "compile_codex_private_sqlite_close_request",
        "compile_codex_private_sqlite_compose_plan",
        "compile_codex_private_sqlite_compose_request",
    }
    forbidden_cli_handlers = {
        "_codex_private_sqlite_capacity_observe",
        "_codex_private_sqlite_close_request",
        "_codex_private_sqlite_compose_action_plan",
        "_codex_private_sqlite_compose_plan",
        "_codex_private_sqlite_compose_request",
        "_codex_private_sqlite_private_reclose",
        "_codex_private_sqlite_session_reclose",
    }

    trees: dict[str, ast.Module] = {}
    for relative, text in source_texts.items():
        try:
            trees[relative] = ast.parse(text)
        except SyntaxError as error:
            raise SkillContractError(
                f"cannot parse retired SQLite source {relative}: {error}"
            ) from error

    for relative, tree in trees.items():
        all_nodes = tuple(ast.walk(tree))
        definitions = {
            node.name
            for node in all_nodes
            if isinstance(node, (ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef))
        }
        imported_names = {
            name
            for node in all_nodes
            if isinstance(node, (ast.Import, ast.ImportFrom))
            for alias in node.names
            for name in (alias.name, alias.asname)
            if name is not None
        }
        loaded_names = {
            node.id
            for node in all_nodes
            if isinstance(node, ast.Name) and isinstance(node.ctx, ast.Load)
        }
        attribute_names = {
            node.attr for node in all_nodes if isinstance(node, ast.Attribute)
        }
        string_names = {
            node.value
            for node in all_nodes
            if isinstance(node, ast.Constant) and isinstance(node.value, str)
        }
        star_import = any(
            isinstance(node, ast.ImportFrom)
            and any(alias.name == "*" for alias in node.names)
            for node in all_nodes
        )
        library_dunder_dictionary_access = bool(
            relative.startswith("scripts/bulkload_lib/")
            and any(
                isinstance(node, ast.Attribute) and node.attr == "__dict__"
                for node in all_nodes
            )
        )
        forbidden_names = forbidden_public_producers | retired_private_helpers
        observed_names = (
            definitions | imported_names | loaded_names | attribute_names | string_names
        )
        if star_import or library_dunder_dictionary_access:
            raise SkillContractError(
                f"retired SQLite source authority is indirect: {relative}"
            )
        if observed_names & forbidden_names:
            raise SkillContractError(
                f"active v7 retains historical SQLite producer authority: {relative}"
            )
        if relative.endswith("/cli.py") and observed_names & forbidden_cli_handlers:
            raise SkillContractError(
                "active v7 CLI retains a direct historical SQLite producer handler"
            )
        if (
            relative != "scripts/bulkload_lib/private_sqlite_verifier.py"
            and "_observe_codex_private_sqlite_bundle" in observed_names
        ):
            raise SkillContractError(
                "SQLite verifier private worker escaped its exact source owner"
            )

    validator_owners = {
        "validate_codex_private_sqlite_compose_plan_against_inputs": (
            "scripts/bulkload_lib/private_sqlite_plan.py"
        ),
        "validate_codex_private_sqlite_close_request_against_inputs": (
            "scripts/bulkload_lib/private_sqlite_close.py"
        ),
        "validate_codex_private_sqlite_action_plan_against_close": (
            "scripts/bulkload_lib/private_sqlite_action_plan.py"
        ),
    }
    expected_validator_callers = {
        "validate_codex_private_sqlite_compose_plan_against_inputs": {
            (
                "scripts/bulkload_lib/private_sqlite_close.py",
                "validate_codex_private_sqlite_close_request_against_inputs",
            )
        },
        "validate_codex_private_sqlite_close_request_against_inputs": set(),
        "validate_codex_private_sqlite_action_plan_against_close": set(),
    }
    for validator_name, owner in validator_owners.items():
        definitions = [
            (relative, node)
            for relative, tree in trees.items()
            for node in ast.walk(tree)
            if isinstance(node, (ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef))
            and node.name == validator_name
        ]
        if (
            len(definitions) != 1
            or definitions[0][0] != owner
            or not isinstance(definitions[0][1], ast.FunctionDef)
            or definitions[0][1] not in trees[owner].body
        ):
            raise SkillContractError(
                f"active v7 deterministic validator ownership differs: {validator_name}"
            )

        imports = [
            (relative, node, alias)
            for relative, tree in trees.items()
            for node in ast.walk(tree)
            if isinstance(node, (ast.Import, ast.ImportFrom))
            for alias in node.names
            if alias.name == validator_name or alias.asname == validator_name
        ]
        expected_importers = {
            relative
            for relative, _ in expected_validator_callers[validator_name]
            if relative != owner
        }
        if (
            {relative for relative, _, _ in imports} != expected_importers
            or len(imports) != len(expected_importers)
            or any(
                not isinstance(node, ast.ImportFrom)
                or node.level != 1
                or node.module != PurePosixPath(owner).stem
                or alias.name != validator_name
                or alias.asname is not None
                or node not in trees[relative].body
                for relative, node, alias in imports
            )
        ):
            raise SkillContractError(
                "active v7 deterministic validator import topology differs: "
                f"{validator_name}"
            )

        observed_callers: set[tuple[str, str]] = set()
        call_count = 0
        for relative, tree in trees.items():
            parent_by_id = {
                id(child): parent
                for parent in ast.walk(tree)
                for child in ast.iter_child_nodes(parent)
            }

            def enclosing_function_name(node: ast.AST) -> str | None:
                current = parent_by_id.get(id(node))
                while current is not None:
                    if isinstance(
                        current,
                        (ast.FunctionDef, ast.AsyncFunctionDef),
                    ):
                        return current.name
                    current = parent_by_id.get(id(current))
                return None

            loads = [
                node
                for node in ast.walk(tree)
                if isinstance(node, ast.Name)
                and isinstance(node.ctx, ast.Load)
                and node.id == validator_name
            ]
            for load in loads:
                parent = parent_by_id.get(id(load))
                caller = enclosing_function_name(load)
                if (
                    not isinstance(parent, ast.Call)
                    or parent.func is not load
                    or caller is None
                ):
                    raise SkillContractError(
                        "active v7 deterministic validator escaped direct-call "
                        f"topology: {validator_name}"
                    )
                call_count += 1
                observed_callers.add((relative, caller))
            if any(
                (isinstance(node, ast.Attribute) and node.attr == validator_name)
                or (isinstance(node, ast.Constant) and node.value == validator_name)
                for node in ast.walk(tree)
            ):
                raise SkillContractError(
                    "active v7 deterministic validator gained indirect access: "
                    f"{validator_name}"
                )
        if observed_callers != expected_validator_callers[
            validator_name
        ] or call_count != len(expected_validator_callers[validator_name]):
            raise SkillContractError(
                "active v7 deterministic validator call topology differs: "
                f"{validator_name}"
            )


def validate_frozen_v6_sqlite_validator_source(request_text: str) -> None:
    """Keep active v7 request handling validator-only."""
    try:
        tree = ast.parse(request_text)
    except SyntaxError as error:
        raise SkillContractError(
            "frozen v6 SQLite validator source is invalid"
        ) from error

    public_functions = {
        node.name
        for node in tree.body
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
        and not node.name.startswith("_")
    }
    expected_public_functions = {
        "validate_codex_private_sqlite_capacity_observation",
        "validate_codex_private_sqlite_capacity_observation_against_request",
        "validate_codex_private_sqlite_compose_request",
        "validate_codex_private_sqlite_compose_request_against_action",
    }
    forbidden_definitions = {
        "PinnedComposeWorkspace",
        "compile_codex_private_sqlite_capacity_observation",
        "compile_codex_private_sqlite_compose_request",
        "_recompute_frozen_v6_capacity_observation",
        "_recompute_frozen_v6_compose_request",
    }
    definitions = {
        node.name
        for node in ast.walk(tree)
        if isinstance(
            node,
            (ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef),
        )
    }
    if (
        public_functions != expected_public_functions
        or definitions & forbidden_definitions
        or any(isinstance(node, ast.ClassDef) for node in ast.walk(tree))
    ):
        raise SkillContractError(
            "active v7 SQLite request handling gained producer authority"
        )


def validate_verifier_oracle_source(verifier_text: str) -> None:
    try:
        tree = ast.parse(verifier_text)
    except SyntaxError as error:
        raise SkillContractError("SQLite verifier oracle source is invalid") from error
    imported_modules: set[str] = set()
    imported_names: set[str] = set()
    loaded_names: set[str] = set()
    called_attributes: set[str] = set()
    public_functions: set[str] = set()
    parent_by_id = {
        id(child): parent
        for parent in ast.walk(tree)
        for child in ast.iter_child_nodes(parent)
    }

    def enclosing_function(node: ast.AST) -> ast.AST | None:
        current = parent_by_id.get(id(node))
        while current is not None:
            if isinstance(current, (ast.FunctionDef, ast.AsyncFunctionDef)):
                return current
            current = parent_by_id.get(id(current))
        return None

    def binding_name(node: ast.AST) -> str | None:
        if isinstance(node, ast.Name) and isinstance(
            node.ctx,
            (ast.Store, ast.Del),
        ):
            return node.id
        if isinstance(node, ast.arg):
            return node.arg
        if isinstance(node, (ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef)):
            return node.name
        if isinstance(node, ast.ExceptHandler):
            return node.name
        if isinstance(node, (ast.MatchAs, ast.MatchStar)):
            return node.name
        if isinstance(node, ast.MatchMapping):
            return node.rest
        if isinstance(node, ast.alias):
            parent = parent_by_id.get(id(node))
            if isinstance(parent, ast.Import):
                return node.asname or node.name.split(".", 1)[0]
            if isinstance(parent, ast.ImportFrom):
                return node.asname or node.name
        return None

    def scope_bindings(
        function: ast.FunctionDef | ast.AsyncFunctionDef,
        name: str,
    ) -> list[ast.AST]:
        return [
            node
            for node in ast.walk(function)
            if enclosing_function(node) is function and binding_name(node) == name
        ]

    def immutable_assignment_value(
        function: ast.FunctionDef | ast.AsyncFunctionDef,
        name: str,
    ) -> ast.AST | None:
        bindings = scope_bindings(function, name)
        if len(bindings) != 1 or not isinstance(bindings[0], ast.Name):
            return None
        assignment = parent_by_id.get(id(bindings[0]))
        if (
            not isinstance(assignment, ast.Assign)
            or len(assignment.targets) != 1
            or assignment.targets[0] is not bindings[0]
        ):
            return None
        return assignment.value

    allowed_imports = {
        (0, "__future__"): {"annotations"},
        (0, "contextlib"): {"contextmanager"},
        (0, "copy"): {"deepcopy"},
        (0, "datetime"): {"UTC", "datetime"},
        (0, "pathlib"): {"Path"},
        (0, "typing"): {"Any", "Iterator"},
        (0, "urllib.parse"): {"quote"},
        (1, ""): {"private_runtime"},
        (1, "model"): {
            "BulkloadError",
            "canonical_bytes",
            "object_digest",
            "sha256_bytes",
            "utc_now",
        },
        (1, "private_sqlite_action_plan"): {
            "PRIVATE_SQLITE_ACTION_PLAN_SCHEMA",
            "validate_codex_private_sqlite_action_plan",
        },
        (1, "private_sqlite_plan"): {
            "MAX_SQLITE_PLAN_BYTES",
            "MAX_SQLITE_PLAN_ROWS",
            "MAX_SQLITE_PLAN_ROW_BYTES",
            "MAX_SQLITE_PLAN_SECONDS",
            "MAX_SQLITE_VALUE_BYTES",
            "validate_codex_private_sqlite_compose_plan",
        },
        (1, "private_sqlite_protocol"): {
            "COMPOSED_BUNDLE_MANIFEST_SCHEMA",
            "COMPOSITION_RECEIPT_SCHEMA",
            "MAX_FAILURES",
            "MAX_PROTOCOL_BYTES",
            "MAX_TABLES",
            "MAX_TREE_ENTRIES",
            "PRESEAL_RECEIPT_EXCLUDED_NODE_IDS",
            "SQLITE_FAMILY_BASENAME",
            "VERIFIER_ORACLE_REPORT_IMPLEMENTATION",
            "VERIFIER_ORACLE_REPORT_SCHEMA",
            "validate_composed_bundle_manifest",
            "validate_composition_receipt",
            "validate_verifier_oracle_report",
        },
        (1, "private_sqlite_request"): {
            "PRIVATE_SQLITE_CAPACITY_OBSERVATION_SCHEMA",
            "PRIVATE_SQLITE_COMPOSE_REQUEST_SCHEMA",
            "validate_codex_private_sqlite_capacity_observation",
            "validate_codex_private_sqlite_capacity_observation_against_request",
            "validate_codex_private_sqlite_compose_request",
            "validate_codex_private_sqlite_compose_request_against_action",
        },
    }
    allowed_plain_imports = {
        "hashlib",
        "json",
        "os",
        "re",
        "sqlite3",
        "stat",
        "struct",
        "time",
        "uuid",
    }
    import_bindings: set[str] = set()
    imports_are_exact = True
    top_level_import_ids = {
        id(node) for node in tree.body if isinstance(node, (ast.Import, ast.ImportFrom))
    }
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            if id(node) not in top_level_import_ids:
                imports_are_exact = False
            for alias in node.names:
                import_bindings.add(alias.asname or alias.name.split(".", 1)[0])
                if alias.asname is not None or alias.name not in allowed_plain_imports:
                    imports_are_exact = False
        elif isinstance(node, ast.ImportFrom):
            if id(node) not in top_level_import_ids:
                imports_are_exact = False
            allowed_names = allowed_imports.get((node.level, node.module or ""))
            for alias in node.names:
                import_bindings.add(alias.asname or alias.name)
                if (
                    allowed_names is None
                    or alias.asname is not None
                    or alias.name not in allowed_names
                ):
                    imports_are_exact = False

    rebound_imports = {
        name
        for node in ast.walk(tree)
        if not isinstance(node, ast.alias)
        and (name := binding_name(node)) is not None
        and name in import_bindings
    }

    def qualified_name(node: ast.AST) -> str | None:
        if isinstance(node, ast.Name):
            return node.id
        if isinstance(node, ast.Attribute):
            parent = qualified_name(node.value)
            if parent is not None:
                return f"{parent}.{node.attr}"
        return None

    allowed_module_attributes = {
        "datetime.strptime",
        "hashlib.sha256",
        "json.JSONDecodeError",
        "json.loads",
        "os.O_RDONLY",
        "os.SEEK_SET",
        "os.close",
        "os.dup",
        "os.fspath",
        "os.fstat",
        "os.fstatvfs",
        "os.getuid",
        "os.lseek",
        "os.open",
        "os.path",
        "os.path.abspath",
        "os.path.exists",
        "os.pread",
        "os.read",
        "os.scandir",
        "os.stat",
        "os.stat_result",
        "private_runtime.LEGACY_PRIVATE_RUNTIME_AUTHORITY_V5_REPAIRED",
        "private_runtime.LEGACY_PRIVATE_RUNTIME_AUTHORITY_V6",
        "private_runtime.open_pinned_private_runtime_authority",
        "private_runtime.validate_private_runtime_authority",
        "re.compile",
        "re.fullmatch",
        "sqlite3.Connection",
        "sqlite3.Cursor",
        "sqlite3.DatabaseError",
        "sqlite3.Error",
        "sqlite3.NotSupportedError",
        "sqlite3.SQLITE_LIMIT_COLUMN",
        "sqlite3.SQLITE_LIMIT_LENGTH",
        "sqlite3.SQLITE_LIMIT_SQL_LENGTH",
        "sqlite3.connect",
        "sqlite3.sqlite_version",
        "sqlite3.sqlite_version_info",
        "stat.S_IMODE",
        "stat.S_ISDIR",
        "stat.S_ISREG",
        "struct.error",
        "struct.pack",
        "time.monotonic",
        "uuid.UUID",
        "uuid.uuid4",
    }
    module_roots = {item.split(".", 1)[0] for item in allowed_module_attributes}
    module_attributes_are_exact = all(
        name in allowed_module_attributes
        for node in ast.walk(tree)
        if isinstance(node, ast.Attribute)
        and isinstance(node.ctx, ast.Load)
        and (name := qualified_name(node)) is not None
        and name.split(".", 1)[0] in module_roots
    )
    allowed_module_calls = {
        "datetime.strptime",
        "hashlib.sha256",
        "json.loads",
        "os.close",
        "os.dup",
        "os.fspath",
        "os.fstat",
        "os.fstatvfs",
        "os.getuid",
        "os.lseek",
        "os.open",
        "os.path.abspath",
        "os.path.exists",
        "os.pread",
        "os.read",
        "os.scandir",
        "os.stat",
        "private_runtime.open_pinned_private_runtime_authority",
        "private_runtime.validate_private_runtime_authority",
        "re.compile",
        "re.fullmatch",
        "sqlite3.connect",
        "stat.S_IMODE",
        "stat.S_ISDIR",
        "stat.S_ISREG",
        "struct.pack",
        "time.monotonic",
        "uuid.UUID",
        "uuid.uuid4",
    }
    allowed_instance_methods = {
        "add",
        "append",
        "casefold",
        "clear",
        "close",
        "decode",
        "digest",
        "enable_load_extension",
        "encode",
        "execute",
        "extend",
        "fetchone",
        "find",
        "fullmatch",
        "get",
        "getlimit",
        "hexdigest",
        "is_absolute",
        "isalnum",
        "isalpha",
        "items",
        "join",
        "lower",
        "revalidate",
        "set_progress_handler",
        "setdefault",
        "setlimit",
        "sort",
        "split",
        "splitlines",
        "startswith",
        "strip",
        "to_bytes",
        "update",
        "upper",
        "values",
    }
    allowed_direct_calls = {
        "BulkloadError",
        "Path",
        "ValueError",
        "all",
        "any",
        "bool",
        "canonical_bytes",
        "deepcopy",
        "enumerate",
        "getattr",
        "int",
        "isinstance",
        "len",
        "list",
        "max",
        "min",
        "object_digest",
        "quote",
        "range",
        "reversed",
        "set",
        "sha256_bytes",
        "sorted",
        "str",
        "sum",
        "tuple",
        "type",
        "utc_now",
        "validate_codex_private_sqlite_action_plan",
        "validate_codex_private_sqlite_capacity_observation",
        "validate_codex_private_sqlite_capacity_observation_against_request",
        "validate_codex_private_sqlite_compose_plan",
        "validate_codex_private_sqlite_compose_request",
        "validate_codex_private_sqlite_compose_request_against_action",
        "validate_composed_bundle_manifest",
        "validate_composition_receipt",
        "validate_verifier_oracle_report",
        "zip",
    }
    top_level_function_nodes = [
        node
        for node in tree.body
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
    ]
    local_functions = {node.name for node in top_level_function_nodes}
    top_level_functions_are_unique = len(local_functions) == len(
        top_level_function_nodes
    )
    top_level_function_ids = {id(node) for node in top_level_function_nodes}
    protected_call_bindings = import_bindings | allowed_direct_calls | local_functions
    rebound_call_bindings: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.alias):
            continue
        name = binding_name(node)
        if name is None or name not in protected_call_bindings:
            continue
        if (
            isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
            and id(node) in top_level_function_ids
            and name not in import_bindings | allowed_direct_calls
        ):
            continue
        rebound_call_bindings.add(name)
    classes_present = any(isinstance(node, ast.ClassDef) for node in ast.walk(tree))
    dunder_attributes_present = any(
        isinstance(node, ast.Attribute)
        and (node.attr.startswith("__") or node.attr.endswith("__"))
        for node in ast.walk(tree)
    )

    def safe_getattr_call(node: ast.Call) -> bool:
        if (
            len(node.args) != 3
            or node.keywords
            or not isinstance(node.args[0], ast.Name)
            or not isinstance(node.args[1], ast.Constant)
        ):
            return False
        receiver = node.args[0].id
        attribute = node.args[1].value
        default = node.args[2]
        return bool(
            (
                receiver == "os"
                and attribute in {"O_CLOEXEC", "O_DIRECTORY", "O_NOFOLLOW"}
                and isinstance(default, ast.Constant)
                and default.value == 0
            )
            or (
                receiver == "filesystem"
                and attribute == "f_fsid"
                and isinstance(default, ast.Constant)
                and default.value is None
            )
        )

    def direct_path_reader(node: ast.Call) -> bool:
        return bool(
            isinstance(node.func, ast.Attribute)
            and node.func.attr in {"exists", "read_text"}
            and isinstance(node.func.value, ast.Call)
            and isinstance(node.func.value.func, ast.Name)
            and node.func.value.func.id == "Path"
            and (
                (node.func.attr == "exists" and not node.args and not node.keywords)
                or (
                    node.func.attr == "read_text"
                    and not node.args
                    and len(node.keywords) == 1
                    and node.keywords[0].arg == "encoding"
                    and isinstance(node.keywords[0].value, ast.Constant)
                    and node.keywords[0].value.value == "utf-8"
                )
            )
        )

    def datetime_timezone_binding(node: ast.Call) -> bool:
        return bool(
            isinstance(node.func, ast.Attribute)
            and node.func.attr == "replace"
            and isinstance(node.func.value, ast.Call)
            and qualified_name(node.func.value.func) == "datetime.strptime"
            and not node.args
            and len(node.keywords) == 1
            and node.keywords[0].arg == "tzinfo"
            and isinstance(node.keywords[0].value, ast.Name)
            and node.keywords[0].value.id == "UTC"
        )

    def schema_budget_label_replace(node: ast.Call) -> bool:
        return bool(
            isinstance(node.func, ast.Attribute)
            and node.func.attr == "replace"
            and isinstance(node.func.value, ast.Name)
            and node.func.value.id == "key"
            and len(node.args) == 2
            and not node.keywords
            and all(isinstance(argument, ast.Constant) for argument in node.args)
            and [argument.value for argument in node.args] == ["_", "-"]
            and isinstance(enclosing_function(node), ast.FunctionDef)
            and enclosing_function(node).name == "_charge_schema_budget"
        )

    safe_sensitive_attribute_ids: set[int] = set()
    call_capabilities_are_exact = True
    for node in ast.walk(tree):
        if not isinstance(node, ast.Call):
            continue
        if isinstance(node.func, ast.Name):
            if node.func.id == "getattr":
                call_capabilities_are_exact &= safe_getattr_call(node)
            elif node.func.id not in allowed_direct_calls | local_functions:
                call_capabilities_are_exact = False
            continue
        if not isinstance(node.func, ast.Attribute):
            call_capabilities_are_exact = False
            continue
        name = qualified_name(node.func)
        if name is not None and name.split(".", 1)[0] in module_roots:
            if name not in allowed_module_calls:
                call_capabilities_are_exact = False
            elif name == "os.open":
                safe_sensitive_attribute_ids.add(id(node.func))
            continue
        if (
            direct_path_reader(node)
            or datetime_timezone_binding(node)
            or schema_budget_label_replace(node)
        ):
            safe_sensitive_attribute_ids.add(id(node.func))
            continue
        if node.func.attr not in allowed_instance_methods:
            call_capabilities_are_exact = False

    dangerous_loaded_attributes = {
        "chmod",
        "chown",
        "creat",
        "fdopen",
        "hardlink_to",
        "lchmod",
        "lchown",
        "link",
        "makedirs",
        "mkdir",
        "mkfifo",
        "mknod",
        "open",
        "remove",
        "removedirs",
        "rename",
        "renames",
        "replace",
        "rmdir",
        "symlink",
        "symlink_to",
        "touch",
        "truncate",
        "unlink",
        "write",
        "write_bytes",
        "write_text",
        "writelines",
    }
    sensitive_attribute_loads_are_exact = all(
        node.attr not in dangerous_loaded_attributes
        or id(node) in safe_sensitive_attribute_ids
        for node in ast.walk(tree)
        if isinstance(node, ast.Attribute) and isinstance(node.ctx, ast.Load)
    )
    enable_extension_calls_are_safe = all(
        len(node.args) == 1
        and not node.keywords
        and isinstance(node.args[0], ast.Constant)
        and node.args[0].value is False
        for node in ast.walk(tree)
        if isinstance(node, ast.Call)
        and isinstance(node.func, ast.Attribute)
        and node.func.attr == "enable_load_extension"
    )

    def read_only_flag_expression(node: ast.AST) -> bool:
        if isinstance(node, ast.BinOp) and isinstance(node.op, ast.BitOr):
            return read_only_flag_expression(node.left) and read_only_flag_expression(
                node.right
            )
        if (
            isinstance(node, ast.Attribute)
            and isinstance(node.value, ast.Name)
            and node.value.id == "os"
            and node.attr == "O_RDONLY"
        ):
            return True
        return bool(
            isinstance(node, ast.Call)
            and isinstance(node.func, ast.Name)
            and node.func.id == "getattr"
            and len(node.args) == 3
            and isinstance(node.args[0], ast.Name)
            and node.args[0].id == "os"
            and isinstance(node.args[1], ast.Constant)
            and node.args[1].value in {"O_CLOEXEC", "O_DIRECTORY", "O_NOFOLLOW"}
            and isinstance(node.args[2], ast.Constant)
            and node.args[2].value == 0
        )

    def read_only_open_call(node: ast.Call) -> bool:
        if (
            not isinstance(node.func, ast.Attribute)
            or not isinstance(node.func.value, ast.Name)
            or node.func.value.id != "os"
            or node.func.attr != "open"
            or len(node.args) < 2
        ):
            return False
        flags = node.args[1]
        if read_only_flag_expression(flags):
            return True
        if not isinstance(flags, ast.Name):
            return False
        function = enclosing_function(node)
        if not isinstance(function, (ast.FunctionDef, ast.AsyncFunctionDef)):
            return False
        value = immutable_assignment_value(function, flags.id)
        return bool(value is not None and read_only_flag_expression(value))

    attribute_calls = [
        node
        for node in ast.walk(tree)
        if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute)
    ]
    open_calls = [node for node in attribute_calls if node.func.attr == "open"]
    open_attributes = {
        id(node)
        for node in ast.walk(tree)
        if isinstance(node, ast.Attribute)
        and isinstance(node.ctx, ast.Load)
        and node.attr == "open"
    }
    safe_open_attributes = {id(node.func) for node in open_calls}
    read_only_opens = open_attributes == safe_open_attributes and all(
        read_only_open_call(node) for node in open_calls
    )

    connect_calls = [node for node in attribute_calls if node.func.attr == "connect"]
    connect_attributes = {
        id(node)
        for node in ast.walk(tree)
        if isinstance(node, ast.Attribute)
        and isinstance(node.ctx, ast.Load)
        and node.attr == "connect"
    }
    safe_connect_attributes = {id(node.func) for node in connect_calls}
    uri_value: ast.AST | None = None
    if len(connect_calls) == 1:
        connect_function = enclosing_function(connect_calls[0])
        if isinstance(connect_function, (ast.FunctionDef, ast.AsyncFunctionDef)):
            uri_value = immutable_assignment_value(connect_function, "uri")
    read_only_connects = connect_attributes == safe_connect_attributes and (
        not connect_calls
        or (
            len(connect_calls) == 1
            and isinstance(connect_calls[0].func.value, ast.Name)
            and connect_calls[0].func.value.id == "sqlite3"
            and len(connect_calls[0].args) == 1
            and isinstance(connect_calls[0].args[0], ast.Name)
            and connect_calls[0].args[0].id == "uri"
            and len(connect_calls[0].keywords) == 1
            and connect_calls[0].keywords[0].arg == "uri"
            and isinstance(connect_calls[0].keywords[0].value, ast.Constant)
            and connect_calls[0].keywords[0].value.value is True
            and isinstance(uri_value, ast.JoinedStr)
            and ast.unparse(uri_value)
            == "f\"file:{quote(descriptor_path, safe='/')}?mode=ro&immutable=1\""
        )
    )

    execute_calls = [node for node in attribute_calls if node.func.attr == "execute"]
    execute_attributes = {
        id(node)
        for node in ast.walk(tree)
        if isinstance(node, ast.Attribute)
        and isinstance(node.ctx, ast.Load)
        and node.attr == "execute"
    }
    safe_execute_attributes = {id(node.func) for node in execute_calls}

    def read_only_sql_argument(node: ast.AST, call: ast.Call) -> bool:
        if isinstance(node, ast.Constant) and isinstance(node.value, str):
            statement = node.value.strip().upper()
            return statement.startswith("SELECT ") or statement in {
                "PRAGMA APPLICATION_ID",
                "PRAGMA COMPILE_OPTIONS",
                "PRAGMA ENCODING",
                "PRAGMA FOREIGN_KEY_CHECK",
                "PRAGMA INTEGRITY_CHECK",
                "PRAGMA INTEGRITY_CHECK(1)",
                "PRAGMA JOURNAL_MODE",
                "PRAGMA QUERY_ONLY=ON",
                "PRAGMA USER_VERSION",
            }
        if not isinstance(node, ast.Name) or node.id != "query":
            return False
        function = enclosing_function(call)
        if not isinstance(function, (ast.FunctionDef, ast.AsyncFunctionDef)):
            return False
        query_value = immutable_assignment_value(function, "query")
        return bool(
            isinstance(query_value, ast.JoinedStr)
            and ast.unparse(query_value).lstrip("f'\"").startswith("SELECT ")
        )

    read_only_executes = execute_attributes == safe_execute_attributes and all(
        isinstance(node.func.value, ast.Name)
        and node.func.value.id == "connection"
        and bool(node.args)
        and read_only_sql_argument(node.args[0], node)
        for node in execute_calls
    )
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            imported_modules.update(alias.name for alias in node.names)
        elif isinstance(node, ast.ImportFrom):
            imported_modules.add(node.module or "")
            imported_names.update(alias.name for alias in node.names)
        elif isinstance(node, ast.Attribute) and isinstance(node.ctx, ast.Load):
            called_attributes.add(node.attr)
        elif isinstance(node, ast.Name) and isinstance(node.ctx, ast.Load):
            loaded_names.add(node.id)
        elif (
            isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
            and node in tree.body
        ):
            if not node.name.startswith("_"):
                public_functions.add(node.name)

    def oracle_report_topology_is_exact() -> bool:
        public_nodes = [
            node
            for node in ast.walk(tree)
            if isinstance(node, ast.FunctionDef)
            and node.name == "observe_codex_private_sqlite_bundle"
        ]
        worker_nodes = [
            node
            for node in ast.walk(tree)
            if isinstance(node, ast.FunctionDef)
            and node.name == "_observe_codex_private_sqlite_bundle"
        ]
        if (
            len(public_nodes) != 1
            or len(worker_nodes) != 1
            or public_nodes[0] not in tree.body
            or worker_nodes[0] not in tree.body
        ):
            return False
        public = public_nodes[0]
        worker = worker_nodes[0]
        if (
            public.args.posonlyargs
            or [argument.arg for argument in public.args.args]
            != [
                "bundle_path",
                "action_plan",
                "opening_plan",
                "compose_request",
                "capacity_observation",
            ]
            or public.args.vararg is not None
            or [argument.arg for argument in public.args.kwonlyargs]
            != ["verifier_runtime_authority"]
            or public.args.kw_defaults != [None]
            or public.args.kwarg is not None
            or public.args.defaults
            or public.decorator_list
        ):
            return False

        worker_loads = [
            node
            for node in ast.walk(tree)
            if isinstance(node, ast.Name)
            and isinstance(node.ctx, ast.Load)
            and node.id == worker.name
        ]
        if len(worker_loads) != 1:
            return False
        worker_load = worker_loads[0]
        worker_call = parent_by_id.get(id(worker_load))
        if (
            not isinstance(worker_call, ast.Call)
            or worker_call.func is not worker_load
            or enclosing_function(worker_call) is not public
        ):
            return False
        if any(
            (isinstance(node, ast.Attribute) and node.attr == worker.name)
            or (isinstance(node, ast.Constant) and node.value == worker.name)
            for node in ast.walk(tree)
        ):
            return False
        if (
            len(worker_call.args) != 5
            or [
                argument.id if isinstance(argument, ast.Name) else None
                for argument in worker_call.args
            ]
            != [
                "bundle_path",
                "action_snapshot",
                "opening_snapshot",
                "request_snapshot",
                "capacity_snapshot",
            ]
            or any(keyword.arg is None for keyword in worker_call.keywords)
        ):
            return False
        worker_keywords = {
            keyword.arg: keyword.value for keyword in worker_call.keywords
        }
        if set(worker_keywords) != {
            "verifier_runtime_binding",
            "observation_id",
            "observed_at",
            "deadline",
        }:
            return False

        identifier_value = immutable_assignment_value(public, "identifier")
        timestamp_value = immutable_assignment_value(public, "timestamp")
        if (
            not isinstance(identifier_value, ast.Call)
            or not isinstance(identifier_value.func, ast.Name)
            or identifier_value.func.id != "str"
            or len(identifier_value.args) != 1
            or identifier_value.keywords
            or not isinstance(identifier_value.args[0], ast.Call)
            or qualified_name(identifier_value.args[0].func) != "uuid.uuid4"
            or identifier_value.args[0].args
            or identifier_value.args[0].keywords
            or not isinstance(timestamp_value, ast.Call)
            or not isinstance(timestamp_value.func, ast.Name)
            or timestamp_value.func.id != "utc_now"
            or timestamp_value.args
            or timestamp_value.keywords
            or not isinstance(worker_keywords["observation_id"], ast.Name)
            or worker_keywords["observation_id"].id != "identifier"
            or not isinstance(worker_keywords["observed_at"], ast.Name)
            or worker_keywords["observed_at"].id != "timestamp"
        ):
            return False
        identifier_loads = [
            node
            for node in ast.walk(public)
            if isinstance(node, ast.Name)
            and isinstance(node.ctx, ast.Load)
            and node.id == "identifier"
            and enclosing_function(node) is public
        ]
        timestamp_loads = [
            node
            for node in ast.walk(public)
            if isinstance(node, ast.Name)
            and isinstance(node.ctx, ast.Load)
            and node.id == "timestamp"
            and enclosing_function(node) is public
        ]
        identifier_validator_calls = [
            node
            for node in ast.walk(public)
            if isinstance(node, ast.Call)
            and isinstance(node.func, ast.Name)
            and node.func.id == "_require_uuid"
            and enclosing_function(node) is public
        ]
        timestamp_validator_calls = [
            node
            for node in ast.walk(public)
            if isinstance(node, ast.Call)
            and isinstance(node.func, ast.Name)
            and node.func.id == "_parse_utc"
            and enclosing_function(node) is public
        ]
        if (
            len(identifier_loads) != 2
            or len(timestamp_loads) != 2
            or len(identifier_validator_calls) != 1
            or len(timestamp_validator_calls) != 1
            or not identifier_validator_calls[0].args
            or identifier_validator_calls[0].args[0] is not identifier_loads[0]
            and identifier_validator_calls[0].args[0] is not identifier_loads[1]
            or not timestamp_validator_calls[0].args
            or timestamp_validator_calls[0].args[0] is not timestamp_loads[0]
            and timestamp_validator_calls[0].args[0] is not timestamp_loads[1]
            or id(worker_keywords["observation_id"])
            not in {id(node) for node in identifier_loads}
            or id(worker_keywords["observed_at"])
            not in {id(node) for node in timestamp_loads}
        ):
            return False

        public_report_bindings = scope_bindings(public, "report")
        if len(public_report_bindings) != 1 or not isinstance(
            public_report_bindings[0], ast.Name
        ):
            return False
        public_assignment = parent_by_id.get(id(public_report_bindings[0]))
        if (
            not isinstance(public_assignment, ast.Assign)
            or len(public_assignment.targets) != 1
            or public_assignment.targets[0] is not public_report_bindings[0]
            or public_assignment.value is not worker_call
        ):
            return False
        public_returns = [
            node
            for node in ast.walk(public)
            if isinstance(node, ast.Return) and enclosing_function(node) is public
        ]
        if (
            len(public_returns) != 1
            or not isinstance(public_returns[0].value, ast.Name)
            or public_returns[0].value.id != "report"
        ):
            return False
        public_report_loads = {
            id(node)
            for node in ast.walk(public)
            if isinstance(node, ast.Name)
            and isinstance(node.ctx, ast.Load)
            and node.id == "report"
            and enclosing_function(node) is public
        }
        if public_report_loads != {id(public_returns[0].value)}:
            return False

        all_claim_bindings = [
            node for node in ast.walk(tree) if binding_name(node) == "claims"
        ]
        worker_claim_bindings = scope_bindings(worker, "claims")
        if (
            len(all_claim_bindings) != 1
            or worker_claim_bindings != all_claim_bindings
            or not isinstance(worker_claim_bindings[0], ast.Name)
        ):
            return False
        claims_assignment = parent_by_id.get(id(worker_claim_bindings[0]))
        if (
            not isinstance(claims_assignment, ast.Assign)
            or len(claims_assignment.targets) != 1
            or claims_assignment.targets[0] is not worker_claim_bindings[0]
            or not isinstance(claims_assignment.value, ast.Dict)
        ):
            return False
        claims_dict = claims_assignment.value
        if (
            len(claims_dict.keys) != len(EXPECTED_ORACLE_FALSE_CLAIMS)
            or any(
                not isinstance(key, ast.Constant)
                or not isinstance(key.value, str)
                or not isinstance(value, ast.Constant)
                or value.value is not False
                for key, value in zip(claims_dict.keys, claims_dict.values)
            )
            or {
                key.value
                for key in claims_dict.keys
                if isinstance(key, ast.Constant) and isinstance(key.value, str)
            }
            != EXPECTED_ORACLE_FALSE_CLAIMS
        ):
            return False

        all_report_bindings = [
            node for node in ast.walk(tree) if binding_name(node) == "report"
        ]
        worker_report_bindings = scope_bindings(worker, "report")
        if (
            len(all_report_bindings) != 2
            or len(worker_report_bindings) != 1
            or not isinstance(worker_report_bindings[0], ast.Name)
        ):
            return False
        report_binding = worker_report_bindings[0]
        report_assignment = parent_by_id.get(id(report_binding))
        if isinstance(report_assignment, ast.Assign):
            report_value = report_assignment.value
            report_target_is_exact = (
                len(report_assignment.targets) == 1
                and report_assignment.targets[0] is report_binding
            )
        elif isinstance(report_assignment, ast.AnnAssign):
            report_value = report_assignment.value
            report_target_is_exact = report_assignment.target is report_binding
        else:
            return False
        if not report_target_is_exact or not isinstance(report_value, ast.Dict):
            return False
        claims_values = [
            value
            for key, value in zip(report_value.keys, report_value.values)
            if isinstance(key, ast.Constant) and key.value == "claims"
        ]
        if (
            len(claims_values) != 1
            or not isinstance(claims_values[0], ast.Name)
            or claims_values[0].id != "claims"
        ):
            return False
        claims_loads = {
            id(node)
            for node in ast.walk(tree)
            if isinstance(node, ast.Name)
            and isinstance(node.ctx, ast.Load)
            and node.id == "claims"
        }
        if claims_loads != {id(claims_values[0])}:
            return False

        report_subscript_stores = [
            node
            for node in ast.walk(worker)
            if isinstance(node, ast.Subscript)
            and isinstance(node.ctx, (ast.Store, ast.Del))
            and isinstance(node.value, ast.Name)
            and node.value.id == "report"
            and enclosing_function(node) is worker
        ]
        if len(report_subscript_stores) != 1:
            return False
        digest_target = report_subscript_stores[0]
        digest_assignment = parent_by_id.get(id(digest_target))
        if (
            not isinstance(digest_assignment, ast.Assign)
            or len(digest_assignment.targets) != 1
            or digest_assignment.targets[0] is not digest_target
            or not isinstance(digest_target.slice, ast.Constant)
            or digest_target.slice.value != "oracle_report_sha256"
            or not isinstance(digest_assignment.value, ast.Call)
            or not isinstance(digest_assignment.value.func, ast.Name)
            or digest_assignment.value.func.id != "object_digest"
            or len(digest_assignment.value.args) != 2
            or digest_assignment.value.keywords
            or not isinstance(digest_assignment.value.args[0], ast.Name)
            or digest_assignment.value.args[0].id != "report"
            or not isinstance(digest_assignment.value.args[1], ast.Constant)
            or digest_assignment.value.args[1].value != "oracle_report_sha256"
        ):
            return False

        validator_calls = [
            node
            for node in ast.walk(tree)
            if isinstance(node, ast.Call)
            and isinstance(node.func, ast.Name)
            and node.func.id == "validate_verifier_oracle_report"
        ]
        validator_loads = [
            node
            for node in ast.walk(tree)
            if isinstance(node, ast.Name)
            and isinstance(node.ctx, ast.Load)
            and node.id == "validate_verifier_oracle_report"
        ]
        if (
            len(validator_calls) != 1
            or len(validator_loads) != 1
            or validator_calls[0].func is not validator_loads[0]
            or enclosing_function(validator_calls[0]) is not worker
            or len(validator_calls[0].args) != 1
            or validator_calls[0].keywords
            or not isinstance(validator_calls[0].args[0], ast.Name)
            or validator_calls[0].args[0].id != "report"
        ):
            return False
        worker_returns = [
            node
            for node in ast.walk(worker)
            if isinstance(node, ast.Return) and enclosing_function(node) is worker
        ]
        if (
            len(worker_returns) != 1
            or not isinstance(worker_returns[0].value, ast.Name)
            or worker_returns[0].value.id != "report"
        ):
            return False
        report_loads = {
            id(node)
            for node in ast.walk(worker)
            if isinstance(node, ast.Name)
            and isinstance(node.ctx, ast.Load)
            and node.id == "report"
            and enclosing_function(node) is worker
        }
        expected_report_loads = {
            id(digest_target.value),
            id(digest_assignment.value.args[0]),
            id(validator_calls[0].args[0]),
            id(worker_returns[0].value),
        }
        return bool(
            report_loads == expected_report_loads
            and claims_assignment.lineno < report_assignment.lineno
            and report_assignment.lineno < digest_assignment.lineno
            and digest_assignment.lineno < validator_calls[0].lineno
            and validator_calls[0].lineno < worker_returns[0].lineno
            and public_assignment.lineno < public_returns[0].lineno
        )

    oracle_report_topology_exact = oracle_report_topology_is_exact()
    forbidden_attributes = {
        "atomic_write",
        "backup",
        "blobopen",
        "call",
        "check_call",
        "check_output",
        "chown",
        "chmod",
        "copy",
        "copy2",
        "copyfile",
        "copyfileobj",
        "copymode",
        "copystat",
        "fsync",
        "lchmod",
        "link",
        "mkfifo",
        "mkdir",
        "mknod",
        "move",
        "popen",
        "Popen",
        "rename",
        "rmdir",
        "run",
        "symlink",
        "system",
        "touch",
        "truncate",
        "unlink",
        "write",
        "write_bytes",
        "write_text",
        "writelines",
        "executemany",
        "executescript",
    }
    allowed_modules = {
        "",
        "__future__",
        "contextlib",
        "copy",
        "datetime",
        "hashlib",
        "json",
        "model",
        "os",
        "pathlib",
        "private_sqlite_action_plan",
        "private_sqlite_plan",
        "private_sqlite_protocol",
        "private_sqlite_request",
        "re",
        "sqlite3",
        "stat",
        "struct",
        "time",
        "typing",
        "urllib.parse",
        "uuid",
    }
    sql_mutation = re.compile(
        r"\b(?:ALTER|ATTACH|BEGIN|COMMIT|CREATE|DELETE|DETACH|DROP|"
        r"INSERT|REINDEX|RELEASE|REPLACE|ROLLBACK|SAVEPOINT|UPDATE|VACUUM)\b",
        re.IGNORECASE,
    )
    string_literals = {
        node.value
        for node in ast.walk(tree)
        if isinstance(node, ast.Constant) and isinstance(node.value, str)
    }
    has_mutating_sql = any(
        sql_mutation.search(value) is not None
        or (
            value.lstrip().upper().startswith("PRAGMA ")
            and "=" in value
            and value.strip().upper() != "PRAGMA QUERY_ONLY=ON"
        )
        for value in string_literals
    )
    forbidden_final_receipt_names = {
        "INDEPENDENT_VERIFICATION_RECEIPT_SCHEMA",
        "validate_independent_verification_receipt",
    }
    if (
        not imports_are_exact
        or bool(rebound_imports)
        or bool(rebound_call_bindings)
        or not top_level_functions_are_unique
        or classes_present
        or dunder_attributes_present
        or not module_attributes_are_exact
        or not call_capabilities_are_exact
        or not sensitive_attribute_loads_are_exact
        or not enable_extension_calls_are_safe
        or "bulkload_lib.private_sqlite_composer" in imported_modules
        or "private_sqlite_composer" in imported_modules
        or not imported_modules <= allowed_modules
        or forbidden_final_receipt_names
        & (imported_names | loaded_names | called_attributes)
        or "argparse" in imported_modules
        or forbidden_attributes & called_attributes
        or "open" in loaded_names
        or {"__import__", "compile", "eval", "exec"} & loaded_names
        or not read_only_opens
        or not read_only_connects
        or not read_only_executes
        or has_mutating_sql
        or "ORDER BY" in verifier_text.upper()
        or {"O_APPEND", "O_CREAT", "O_RDWR", "O_TRUNC", "O_WRONLY"} & loaded_names
        or "os.replace(" in verifier_text
        or "Path.replace(" in verifier_text
        or public_functions != {"observe_codex_private_sqlite_bundle"}
        or not oracle_report_topology_exact
    ):
        raise SkillContractError(
            "SQLite verifier oracle gained writer, final-receipt, CLI, "
            "or public verification authority"
        )


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
            relative: b"legacy-self-test-only\n" for relative in PRIVATE_SOURCE_PATHS
        }
        source_payloads["scripts/bulkload_lib/cli.py"] = cli_text.encode("utf-8")
        source_payloads["scripts/bulkload_lib/private_state.py"] = (
            private_state_text or ""
        ).encode("utf-8")
    if policy != expected_policy:
        raise SkillContractError(
            "Codex private-state policy does not match the exact v7 contract"
        )
    if set(source_payloads) != set(PRIVATE_SOURCE_PATHS):
        raise SkillContractError(
            "private-state runtime source inventory differs from the v7 contract"
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
            "Bulkload application-source closure differs from the exact policy "
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
        verifier_text = source_payloads[
            "scripts/bulkload_lib/private_sqlite_verifier.py"
        ].decode("utf-8")
        request_validator_text = source_payloads[
            "scripts/bulkload_lib/private_sqlite_request.py"
        ].decode("utf-8")
    except (OSError, UnicodeDecodeError) as error:
        raise SkillContractError(
            f"cannot read private-state implementation: {error}"
        ) from error
    source_texts = {
        relative: payload.decode("utf-8")
        for relative, payload in source_payloads.items()
    }
    validate_retired_sqlite_producer_sources(source_texts)
    validate_frozen_v6_sqlite_validator_source(request_validator_text)
    validate_verifier_oracle_source(verifier_text)
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
    validate_runtime_source_path_inventory(set(PRIVATE_SOURCE_PATHS))
    for extra in (
        "scripts/bulkload_lib/arbitrary.py",
        "scripts/bulkload_lib/private_sqlite_composer.py",
        "scripts/bulkload_lib/private_sqlite_writer.py",
    ):
        try:
            validate_runtime_source_path_inventory({*PRIVATE_SOURCE_PATHS, extra})
        except SkillContractError:
            continue
        raise AssertionError("unreviewed runtime source path was accepted")

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
        "scripts/bulkload.py": "print('fixture')\n",
        "scripts/bulkload_lib/__init__.py": '"""fixture"""\n',
        "scripts/bulkload_lib/cli.py": valid_cli_text,
        "scripts/bulkload_lib/executor.py": "def execute(): return None\n",
        "scripts/bulkload_lib/model.py": "VALUE = 1\n",
        "scripts/bulkload_lib/planner.py": "def plan(): return None\n",
        "scripts/bulkload_lib/private_apply.py": "def apply(): return None\n",
        "scripts/bulkload_lib/private_quiescence.py": "def attest(): return None\n",
        "scripts/bulkload_lib/private_runtime.py": (
            "def validate_runtime(): return None\n"
        ),
        "scripts/bulkload_lib/private_sqlite_action_plan.py": (
            "def validate_codex_private_sqlite_action_plan_against_close(): "
            "return None\n"
        ),
        "scripts/bulkload_lib/private_sqlite_close.py": (
            "from .private_sqlite_plan import "
            "validate_codex_private_sqlite_compose_plan_against_inputs\n"
            "def validate_codex_private_sqlite_close_request_against_inputs(): "
            "return validate_codex_private_sqlite_compose_plan_against_inputs()\n"
            "def validate_codex_private_sqlite_session_reclose_against_live(): "
            "return None\n"
        ),
        "scripts/bulkload_lib/private_sqlite_plan.py": (
            "def validate_codex_private_sqlite_compose_plan_against_inputs(): "
            "return None\n"
        ),
        "scripts/bulkload_lib/private_sqlite_protocol.py": (
            "def validate_protocol(): return None\n"
        ),
        "scripts/bulkload_lib/private_sqlite_request.py": (
            "def _helper(): return None\n"
            "def validate_codex_private_sqlite_capacity_observation(): "
            "return None\n"
            "def validate_codex_private_sqlite_capacity_observation_against_request(): "
            "return None\n"
            "def validate_codex_private_sqlite_compose_request(): return None\n"
            "def validate_codex_private_sqlite_compose_request_against_action(): "
            "return None\n"
        ),
        "scripts/bulkload_lib/private_sqlite_verifier.py": "",
        "scripts/bulkload_lib/private_state.py": "def capture(): return None\n",
        "scripts/bulkload_lib/scanner.py": "def scan(): return None\n",
        "scripts/bulkload_lib/sessions.py": "def sessions(): return None\n",
    }
    source_payloads = {
        path: text.encode("utf-8") for path, text in source_texts.items()
    }
    runtime_sources = dict(source_payloads)
    fixture_policy = copy.deepcopy(EXPECTED_PRIVATE_STATE_POLICY)
    fixture_policy["source_digests"] = {
        path: hashlib.sha256(payload).hexdigest()
        for path, payload in source_payloads.items()
    }
    fixture_policy["runtime_source_sha256"] = runtime_source_digest_from_payloads(
        runtime_sources
    )
    validate_retired_sqlite_producer_sources(source_texts)
    validate_private_state_policy(
        fixture_policy,
        cli_text=valid_cli_text,
        source_payloads=source_payloads,
        runtime_sha256=fixture_policy["runtime_source_sha256"],
        expected_policy=fixture_policy,
    )
    claims_fixture = ",".join(
        f"{name!r}: False" for name in sorted(EXPECTED_ORACLE_FALSE_CLAIMS)
    )
    valid_verifier_text = (
        "def _require_uuid(value, label): return value\n"
        "def _parse_utc(value, label): return value\n"
        "def observe_codex_private_sqlite_bundle("
        "bundle_path, action_plan, opening_plan, compose_request, "
        "capacity_observation, *, verifier_runtime_authority):\n"
        "    action_snapshot = action_plan\n"
        "    opening_snapshot = opening_plan\n"
        "    request_snapshot = compose_request\n"
        "    capacity_snapshot = capacity_observation\n"
        "    identifier = str(uuid.uuid4())\n"
        "    _require_uuid(identifier, 'oracle observation ID')\n"
        "    timestamp = utc_now()\n"
        "    _parse_utc(timestamp, 'oracle observation timestamp')\n"
        "    report = _observe_codex_private_sqlite_bundle("
        "bundle_path, action_snapshot, opening_snapshot, request_snapshot, "
        "capacity_snapshot, "
        "verifier_runtime_binding=verifier_runtime_authority, "
        "observation_id=identifier, observed_at=timestamp, deadline=None)\n"
        "    return report\n"
        "def _observe_codex_private_sqlite_bundle("
        "bundle_path, action_plan, opening_plan, compose_request, "
        "capacity_observation, *, verifier_runtime_binding, observation_id, "
        "observed_at, deadline):\n"
        f"    claims = {{{claims_fixture}}}\n"
        "    report = {'claims': claims}\n"
        "    report['oracle_report_sha256'] = object_digest("
        "report, 'oracle_report_sha256')\n"
        "    validate_verifier_oracle_report(report)\n"
        "    return report\n"
    )
    source_texts["scripts/bulkload_lib/private_sqlite_verifier.py"] = (
        valid_verifier_text
    )
    valid_request_validator_text = source_texts[
        "scripts/bulkload_lib/private_sqlite_request.py"
    ]
    validate_frozen_v6_sqlite_validator_source(valid_request_validator_text)
    validate_verifier_oracle_source(valid_verifier_text)
    installer_no_execute_sentinel = (
        'raise AssertionError("UNVERIFIED_CLI_EXECUTED")\n' + valid_cli_text
    )
    if (
        cli_command_handlers(installer_no_execute_sentinel)
        != EXPECTED_CLI_COMMAND_HANDLERS
    ):
        raise AssertionError("static installer CLI sentinel topology differed")

    for relative, addition in (
        (
            "scripts/bulkload_lib/private_sqlite_plan.py",
            "\ndef compile_codex_private_sqlite_compose_plan(): return None\n",
        ),
        (
            "scripts/bulkload_lib/cli.py",
            "\ndef _codex_private_sqlite_compose_request(): return None\n",
        ),
        (
            "scripts/bulkload_lib/private_sqlite_action_plan.py",
            "\ndef rogue(): return _recompute_frozen_v5_action_plan("
            "created_at='frozen')\n",
        ),
        (
            "scripts/bulkload_lib/private_sqlite_plan.py",
            "\ndef outer():\n"
            "    def compile_codex_private_sqlite_compose_plan(): return None\n"
            "    return compile_codex_private_sqlite_compose_plan\n",
        ),
        (
            "scripts/bulkload_lib/private_sqlite_plan.py",
            "\ndef outer():\n"
            "    class compile_codex_private_sqlite_compose_plan: pass\n"
            "    return compile_codex_private_sqlite_compose_plan\n",
        ),
        (
            "scripts/bulkload_lib/private_sqlite_plan.py",
            "\ndef rogue(owner):\n"
            "    return owner.compile_codex_private_sqlite_compose_plan\n",
        ),
        (
            "scripts/bulkload_lib/private_sqlite_plan.py",
            "\ndef rogue(owner):\n"
            "    return getattr(owner, "
            "'compile_codex_private_sqlite_compose_plan')\n",
        ),
        (
            "scripts/bulkload_lib/private_sqlite_plan.py",
            "\ndef outer():\n"
            "    def _recompute_frozen_v4_compose_plan(): return None\n"
            "    return _recompute_frozen_v4_compose_plan\n",
        ),
        (
            "scripts/bulkload_lib/private_sqlite_action_plan.py",
            "\nvalidator_alias = "
            "validate_codex_private_sqlite_action_plan_against_close\n",
        ),
        (
            "scripts/bulkload_lib/private_sqlite_action_plan.py",
            "\nprivate_worker = _observe_codex_private_sqlite_bundle\n",
        ),
    ):
        changed_sources = dict(source_texts)
        changed_sources[relative] += addition
        try:
            validate_retired_sqlite_producer_sources(changed_sources)
        except SkillContractError:
            continue
        raise AssertionError("retired SQLite producer authority was accepted")

    for forbidden_request_validator in (
        valid_request_validator_text
        + "def compile_codex_private_sqlite_compose_request(): return {}\n",
        valid_request_validator_text + "class PinnedComposeWorkspace: pass\n",
        valid_request_validator_text.replace(
            "def _helper():",
            "def producer_helper():",
        ),
        valid_request_validator_text
        + "def _outer():\n"
        + "    def compile_codex_private_sqlite_compose_request(): return {}\n"
        + "    return compile_codex_private_sqlite_compose_request\n",
        valid_request_validator_text
        + "def _outer():\n"
        + "    class PinnedComposeWorkspace: pass\n"
        + "    return PinnedComposeWorkspace\n",
    ):
        try:
            validate_frozen_v6_sqlite_validator_source(forbidden_request_validator)
        except SkillContractError:
            continue
        raise AssertionError("forbidden v6 SQLite producer surface was accepted")

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

    def assert_cli_rejected(mutated_cli: str) -> None:
        mutated_payloads = dict(source_payloads)
        mutated_payloads["scripts/bulkload_lib/cli.py"] = mutated_cli.encode("utf-8")
        mutated_policy = copy.deepcopy(fixture_policy)
        mutated_policy["source_digests"]["scripts/bulkload_lib/cli.py"] = (
            hashlib.sha256(mutated_cli.encode("utf-8")).hexdigest()
        )
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
            return
        raise AssertionError("forbidden private CLI surface was accepted")

    assert_cli_rejected(
        valid_cli_text
        + '\n    extra = commands.add_parser("codex-private-combined-apply")\n'
        + "    extra.set_defaults(handler=_forbidden)\n"
    )
    assert_cli_rejected(
        valid_cli_text
        + "\n\ndef _hidden_registration(commands):\n"
        + '    extra = commands.add_parser("codex-private-combined-apply")\n'
        + "    extra.set_defaults(handler=_forbidden)\n"
    )
    assert_cli_rejected(
        valid_cli_text
        + "\n\ndef _dynamic_registration(commands):\n"
        + '    register = getattr(commands, "add_parser")\n'
        + '    extra = register("codex-private-combined-apply")\n'
        + '    getattr(extra, "set_defaults")(handler=_forbidden)\n'
    )

    for forbidden_verifier in (
        (
            "from private_sqlite_protocol import "
            "INDEPENDENT_VERIFICATION_RECEIPT_SCHEMA\n"
            "def observe_codex_private_sqlite_bundle(): return None\n"
        ),
        (
            "from pathlib import Path\n"
            "def observe_codex_private_sqlite_bundle(path): "
            "return Path(path).write_bytes(b'writer')\n"
        ),
        valid_verifier_text.replace(
            "return report",
            "open('/tmp/oracle', 'wb').writelines([b'writer']); return report",
        ),
        (
            "from pathlib import Path\n"
            + valid_verifier_text.replace(
                "return report",
                "Path('/tmp/oracle').touch(); return report",
            )
        ),
        (
            "from pathlib import Path\n"
            + valid_verifier_text.replace(
                "return report",
                "Path('/tmp/oracle').open('wb').close(); return report",
            )
        ),
        (
            "import shutil\n"
            + valid_verifier_text.replace(
                "return report",
                "shutil.copyfile('/tmp/source', '/tmp/oracle'); return report",
            )
        ),
        (
            "import os\n"
            + valid_verifier_text.replace(
                "return report",
                "os.close(os.open('/tmp/oracle', 577, 0o600)); return report",
            )
        ),
        (
            "import io\n"
            + valid_verifier_text.replace(
                "return report",
                "io.FileIO('/tmp/oracle', 'w').close(); return report",
            )
        ),
        (
            "import sqlite3\n"
            + valid_verifier_text.replace(
                "return report",
                "sqlite3.connect('/tmp/oracle').close(); return report",
            )
        ),
        valid_verifier_text.replace(
            "return report",
            "connection.execute('DELETE FROM state'); return report",
        ),
        (
            "def verify_bundle(): return True\n"
            "def observe_codex_private_sqlite_bundle(): return None\n"
        ),
        valid_verifier_text.replace(
            "'apply_authorized': False",
            "'apply_authorized': True",
        ),
    ):
        try:
            validate_verifier_oracle_source(forbidden_verifier)
        except SkillContractError:
            continue
        raise AssertionError("forbidden verifier authority surface was accepted")

    def assert_verifier_rejected(label: str, verifier_source: str) -> None:
        try:
            validate_verifier_oracle_source(verifier_source)
        except SkillContractError:
            return
        raise AssertionError(f"verifier adversarial probe was accepted: {label}")

    validated_worker_return = (
        "    validate_verifier_oracle_report(report)\n    return report\n"
    )
    verifier_adversarial_probes = {
        "os-creat": (
            "import os\n"
            + valid_verifier_text
            + "def _writer(): return os.creat('/tmp/oracle', 0o600)\n"
        ),
        "os-remove": (
            "import os\n"
            + valid_verifier_text
            + "def _writer(): return os.remove('/tmp/oracle')\n"
        ),
        "os-makedirs": (
            "import os\n"
            + valid_verifier_text
            + "def _writer(): return os.makedirs('/tmp/oracle')\n"
        ),
        "path-replace": (
            "from pathlib import Path\n"
            + valid_verifier_text
            + "def _writer(): return Path('/tmp/a').replace('/tmp/b')\n"
        ),
        "path-symlink-to": (
            "from pathlib import Path\n"
            + valid_verifier_text
            + "def _writer(): return Path('/tmp/a').symlink_to('/tmp/b')\n"
        ),
        "path-dunder-unlink-alias": (
            "from pathlib import Path\n"
            + valid_verifier_text
            + "def _writer():\n"
            + "    writer = Path.__dict__['unlink']\n"
            + "    return writer(Path('/tmp/oracle'))\n"
        ),
        "flags-augassign": (
            "import os\n"
            + valid_verifier_text
            + "def _writer(path):\n"
            + "    flags = os.O_RDONLY\n"
            + "    flags |= 64\n"
            + "    descriptor = os.open(path, flags)\n"
            + "    os.close(descriptor)\n"
        ),
        "query-augassign": (
            valid_verifier_text
            + "def _writer(connection):\n"
            + "    query = f'SELECT value'\n"
            + "    query *= 0\n"
            + "    query += 'DE' + 'LETE FROM state'\n"
            + "    return connection.execute(query)\n"
        ),
        "uri-augassign": (
            "import sqlite3\n"
            + "from urllib.parse import quote\n"
            + valid_verifier_text
            + "def _writer(descriptor_path):\n"
            + "    uri = "
            + "f\"file:{quote(descriptor_path, safe='/')}?mode=ro&immutable=1\"\n"
            + "    uri += '&mode=rwc&immutable=0'\n"
            + "    return sqlite3.connect(uri, uri=True)\n"
        ),
        "class-import-shadow": (
            "from urllib.parse import quote\n"
            + "class quote: pass\n"
            + valid_verifier_text
        ),
        "function-import-shadow": (
            "from urllib.parse import quote\n"
            + "def quote(value, safe): return value\n"
            + valid_verifier_text
        ),
        "nested-import-shadow": (
            valid_verifier_text
            + "def _nested_import():\n"
            + "    from urllib.parse import quote\n"
            + "    return quote\n"
        ),
        "claims-post-validation-augassign": valid_verifier_text.replace(
            validated_worker_return,
            "    validate_verifier_oracle_report(report)\n"
            "    claims |= {'apply_authorized': True}\n"
            "    return report\n",
        ),
        "claims-post-validation-alias": valid_verifier_text.replace(
            validated_worker_return,
            "    validate_verifier_oracle_report(report)\n"
            "    claims_alias = claims\n"
            "    claims_alias['apply_authorized'] = True\n"
            "    return report\n",
        ),
        "report-post-validation-mutation": valid_verifier_text.replace(
            validated_worker_return,
            "    validate_verifier_oracle_report(report)\n"
            "    report['claims'] = {'apply_authorized': True}\n"
            "    return report\n",
        ),
        "public-observation-id-parameter": valid_verifier_text.replace(
            "capacity_observation, *, verifier_runtime_authority):",
            "capacity_observation, observation_id, *, verifier_runtime_authority):",
        ),
        "nonambient-observation-id": valid_verifier_text.replace(
            "identifier = str(uuid.uuid4())",
            "identifier = '00000000-0000-0000-0000-000000000000'",
        ),
        "nonambient-observed-at": valid_verifier_text.replace(
            "timestamp = utc_now()",
            "timestamp = '1970-01-01T00:00:00Z'",
        ),
        "private-worker-alias": (
            valid_verifier_text
            + "_private_worker_alias = _observe_codex_private_sqlite_bundle\n"
        ),
    }
    for label, verifier_source in verifier_adversarial_probes.items():
        assert_verifier_rejected(label, verifier_source)

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
        raise AssertionError("mutated application-source closure was accepted")

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
