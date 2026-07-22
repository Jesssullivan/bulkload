#!/usr/bin/env python3
"""Validate the bounded bulkload repository contract without dependencies."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import sys
from typing import Any


class ContractError(ValueError):
    pass


def require_keys(value: dict[str, Any], keys: set[str], label: str) -> None:
    if set(value) != keys:
        raise ContractError(f"{label} keys differ: {sorted(set(value) ^ keys)}")


def validate(value: Any) -> None:
    if not isinstance(value, dict):
        raise ContractError("manifest root must be an object")
    require_keys(
        value, {"schema_version", "repo", "taxonomy", "contracts", "boundaries"}, "root"
    )
    if value["schema_version"] != 1:
        raise ContractError("schema_version must be 1")
    repo = value["repo"]
    require_keys(repo, {"name", "github", "description"}, "repo")
    if repo["name"] != "bulkload" or repo["github"] != "Jesssullivan/bulkload":
        raise ContractError("repository identity drift")
    taxonomy = value["taxonomy"]
    require_keys(taxonomy, {"primary_role", "layers"}, "taxonomy")
    if taxonomy["primary_role"] != "private-migration-tooling":
        raise ContractError("primary role drift")
    if not isinstance(taxonomy["layers"], list) or not taxonomy["layers"]:
        raise ContractError("taxonomy layers must be non-empty")
    contracts = value["contracts"]
    require_keys(
        contracts, {"agent", "operator", "build", "design", "skill"}, "contracts"
    )
    boundaries = value["boundaries"]
    required_boundaries = {
        "default_read_only": True,
        "deletes_destination_data": False,
        "copies_credentials": False,
        "invokes_terminal_multiplexers": False,
        "owns_tcfs_runtime": False,
        "owns_home_manager_activation": False,
    }
    if boundaries != required_boundaries:
        raise ContractError("safety boundary drift")


def self_test() -> None:
    valid = {
        "schema_version": 1,
        "repo": {
            "name": "bulkload",
            "github": "Jesssullivan/bulkload",
            "description": "x",
        },
        "taxonomy": {"primary_role": "private-migration-tooling", "layers": ["x"]},
        "contracts": {
            "agent": "a",
            "operator": "o",
            "build": "b",
            "design": "d",
            "skill": "s",
        },
        "boundaries": {
            "default_read_only": True,
            "deletes_destination_data": False,
            "copies_credentials": False,
            "invokes_terminal_multiplexers": False,
            "owns_tcfs_runtime": False,
            "owns_home_manager_activation": False,
        },
    }
    validate(valid)
    invalid = json.loads(json.dumps(valid))
    invalid["boundaries"]["copies_credentials"] = True
    try:
        validate(invalid)
    except ContractError:
        return
    raise AssertionError("invalid boundary was accepted")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("manifest", nargs="?", default="tinyland.repo.json")
    arguments = parser.parse_args()
    try:
        if arguments.self_test:
            self_test()
        value = json.loads(Path(arguments.manifest).read_text(encoding="utf-8"))
        validate(value)
    except (OSError, json.JSONDecodeError, ContractError, AssertionError) as error:
        print(f"repo-manifest: FAIL: {error}", file=sys.stderr)
        return 1
    print("repo-manifest: PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
