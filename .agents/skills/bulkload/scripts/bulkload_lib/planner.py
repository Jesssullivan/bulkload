"""Deterministic AgentCaptureV4 union planning."""

from __future__ import annotations

from pathlib import Path
from typing import Any, Iterable

from .model import (
    AGENT_PLAN_SCHEMA,
    BulkloadError,
    canonical_bytes,
    new_id,
    require_digest,
    require_exact_keys,
    runtime_source_digest,
    seal,
    sha256_bytes,
    utc_now,
)
from .scanner import (
    expand_provider_item,
    provider_item_destination,
    provider_item_identity,
    validate_agent_capture,
)


def _fingerprint(item: dict[str, Any], *, translated: bool = False) -> tuple[Any, ...]:
    return (
        item.get("kind"),
        item.get("mode"),
        item.get("translated_size", item.get("size"))
        if translated
        else item.get("size"),
        item.get("translated_sha256", item.get("sha256"))
        if translated
        else item.get("sha256"),
    )


def _structural_collision(
    source: dict[str, Any], destination: dict[str, Any] | None
) -> bool:
    return destination is not None and (
        (source.get("kind") == "directory") != (destination.get("kind") == "directory")
    )


def _operation(kind: str, **fields: Any) -> dict[str, Any]:
    operation = {"kind": kind, **fields}
    operation["operation_id"] = sha256_bytes(canonical_bytes(operation))
    return operation


def _catalog_ref(
    owner_class: str,
    *,
    identity: str | None = None,
    name: str | None = None,
    workspace_id: str | None = None,
) -> dict[str, str]:
    value = {"class": owner_class}
    if identity is not None:
        value["identity"] = identity
    if name is not None:
        value["name"] = name
    if workspace_id is not None:
        value["workspace_id"] = workspace_id
    return value


def _reference_key(reference: dict[str, str]) -> tuple[str, str, str]:
    return (
        reference.get("class", ""),
        reference.get("name", ""),
        reference.get("workspace_id", reference.get("identity", "")),
    )


def _catalog_reference_index(
    catalog: dict[str, Any],
) -> dict[tuple[str, str, str], tuple[dict[str, Any], str | None]]:
    result: dict[tuple[str, str, str], tuple[dict[str, Any], str | None]] = {}

    def add(reference: dict[str, str], item: dict[str, Any], root: str | None) -> None:
        key = _reference_key(reference)
        if key in result:
            raise BulkloadError("AgentPlanV4 catalog reference is not unique")
        result[key] = (item, root)

    for item in catalog.get("git_workspaces", []):
        add(
            _catalog_ref("git-workspace", workspace_id=item["workspace_id"]),
            item,
            None,
        )
    for item in catalog.get("non_git", []):
        add(
            _catalog_ref("non-git", identity=item["identity"]),
            item,
            catalog["root_bindings"]["git_root"],
        )
    for provider in catalog.get("providers", []):
        for item in provider.get("items", []):
            add(
                _catalog_ref(
                    "provider",
                    name=provider["name"],
                    identity=provider_item_identity(item),
                ),
                item,
                provider["path"],
            )
    for seat in catalog.get("seats", []):
        for item in seat.get("items", []):
            add(
                _catalog_ref(
                    "mutable-seat", name=seat["name"], identity=item["identity"]
                ),
                item,
                seat["path"],
            )
    return result


class PlanOperationResolver:
    def __init__(self, plan: dict[str, Any]) -> None:
        self._source = _catalog_reference_index(plan["source"]["catalog"])
        self._destination = _catalog_reference_index(plan["destination"]["catalog"])

    @staticmethod
    def _resolve(
        index: dict[tuple[str, str, str], tuple[dict[str, Any], str | None]],
        reference: dict[str, str] | None,
    ) -> tuple[dict[str, Any] | None, str | None]:
        if reference is None:
            return None, None
        try:
            return index[_reference_key(reference)]
        except KeyError as error:
            raise BulkloadError(
                "AgentPlanV4 catalog reference is unresolved"
            ) from error

    def materialize(self, operation: dict[str, Any]) -> dict[str, Any]:
        source, source_root = self._resolve(self._source, operation.get("source_ref"))
        destination, _ = self._resolve(
            self._destination, operation.get("destination_ref")
        )
        if (operation.get("source_ref") or {}).get("class") == "provider":
            assert source is not None
            source = expand_provider_item(source)
        if (operation.get("destination_ref") or {}).get("class") == "provider":
            assert destination is not None
            destination = expand_provider_item(destination)
        result = {
            key: value
            for key, value in operation.items()
            if key not in {"source_ref", "destination_ref"}
        }
        result["source"] = source
        if source_root is not None:
            result["source_root"] = source_root
        result["destination_before"] = destination
        return result


def materialize_plan_operation(
    plan: dict[str, Any], operation: dict[str, Any]
) -> dict[str, Any]:
    """Resolve compact plan references without serializing catalog copies."""
    return PlanOperationResolver(plan).materialize(operation)


def _block(
    blockers: list[dict[str, str]], code: str, path: str, detail: str | None = None
) -> None:
    value = {"code": code, "path": path}
    if detail:
        value["detail"] = detail
    blockers.append(value)


def _recovery_ref(destination_path: str, ref_name: str, oid: str) -> str:
    token = sha256_bytes(
        canonical_bytes({"destination": destination_path, "oid": oid, "ref": ref_name})
    )[:24]
    return f"refs/bulkload/recovery/destination/{token}"


def _destination_worktree_at(
    workspaces: Iterable[dict[str, Any]], path: str
) -> tuple[dict[str, Any], dict[str, Any]] | None:
    for workspace in workspaces:
        for worktree in workspace.get("worktrees", []):
            if worktree["path"] == path:
                return workspace, worktree
    return None


def _non_git_collision(catalog: dict[str, Any], target: str) -> bool:
    root = Path(catalog["root_bindings"]["git_root"])
    candidate = Path(target)
    for item in catalog.get("non_git", []):
        occupied = root / item["relative_path"]
        try:
            occupied.relative_to(candidate)
        except ValueError:
            continue
        return True
    return False


def _containing_git_worktree(
    workspaces: Iterable[dict[str, Any]], target: str
) -> str | None:
    candidate = Path(target)
    for workspace in workspaces:
        roots = {workspace["destination_path"]}
        roots.update(worktree["path"] for worktree in workspace.get("worktrees", []))
        for root in sorted(roots):
            try:
                candidate.relative_to(root)
            except ValueError:
                continue
            return root
    return None


def _plan_git(
    source: dict[str, Any],
    destination: dict[str, Any],
    operations: list[dict[str, Any]],
    blockers: list[dict[str, str]],
) -> None:
    destination_workspaces = destination.get("git_workspaces", [])
    for source_workspace in source.get("git_workspaces", []):
        target_primary = source_workspace["destination_path"]
        destination_match = _destination_worktree_at(
            destination_workspaces, target_primary
        )
        destination_workspace = destination_match[0] if destination_match else None
        if destination_workspace is None and _non_git_collision(
            destination, target_primary
        ):
            _block(blockers, "git-workspace-non-git-collision", target_primary)
            continue
        if destination_workspace is not None and (
            destination_workspace["object_format"] != source_workspace["object_format"]
        ):
            _block(blockers, "git-object-format-divergence", target_primary)
            continue

        for source_worktree in source_workspace.get("worktrees", []):
            target = source_worktree["destination_path"]
            collision = _destination_worktree_at(destination_workspaces, target)
            if collision is None and _non_git_collision(destination, target):
                _block(blockers, "git-workspace-non-git-collision", target)
                continue
            if (
                collision is not None
                and destination_workspace is not None
                and (
                    collision[0]["workspace_id"]
                    != destination_workspace["workspace_id"]
                )
            ):
                _block(blockers, "git-worktree-owner-divergence", target)
                continue
            if collision is None:
                continue
            source_files = {
                item["relative_path"]: item for item in source_worktree.get("files", [])
            }
            destination_files = {
                item["relative_path"]: item for item in collision[1].get("files", [])
            }
            for relative in sorted(set(source_files) & set(destination_files)):
                if _structural_collision(
                    source_files[relative], destination_files[relative]
                ):
                    _block(
                        blockers,
                        "structural-type-collision",
                        f"{target}/{relative}",
                    )
        source_objects = {
            item["relative_path"]: item
            for item in source_workspace.get("object_files", [])
        }
        destination_objects = {
            item["relative_path"]: item
            for item in (destination_workspace or {}).get("object_files", [])
        }
        for relative in sorted(set(source_objects) & set(destination_objects)):
            if _fingerprint(source_objects[relative]) != _fingerprint(
                destination_objects[relative]
            ):
                _block(
                    blockers,
                    "git-object-file-divergence",
                    f"{target_primary}/.git/objects/{relative}",
                )

        ref_actions: list[dict[str, Any]] = []
        source_refs = {
            entry["name"]: entry for entry in source_workspace.get("refs", [])
        }
        destination_refs = (
            {entry["name"]: entry for entry in destination_workspace.get("refs", [])}
            if destination_workspace
            else {}
        )
        occupied_recovery = {
            name: entry["oid"]
            for name, entry in destination_refs.items()
            if name.startswith("refs/bulkload/recovery/")
        }
        for name, source_ref in sorted(source_refs.items()):
            destination_ref = destination_refs.get(name)
            if destination_ref is not None and destination_ref != source_ref:
                recovery = _recovery_ref(target_primary, name, destination_ref["oid"])
                if (
                    recovery in occupied_recovery
                    and occupied_recovery[recovery] != destination_ref["oid"]
                ):
                    _block(blockers, "git-recovery-ref-collision", recovery)
                    continue
                ref_actions.append(
                    {
                        "action": "anchor-destination-divergence",
                        "name": recovery,
                        "oid": destination_ref["oid"],
                        "source_ref": name,
                    }
                )
            if destination_ref != source_ref:
                ref_actions.append(
                    {
                        "action": "set-source-ref",
                        "name": name,
                        "oid": source_ref["oid"],
                        "symbolic_target": source_ref.get("symbolic_target"),
                    }
                )
        source_anchor_oids = {
            entry["oid"] for entry in source_workspace.get("recovery_anchors", [])
        }
        destination_anchor_oids = {
            entry["oid"]
            for entry in (destination_workspace or {}).get("recovery_anchors", [])
        }
        destination_anchor_oids.update(
            entry["oid"] for entry in destination_refs.values()
        )
        for oid in sorted(
            entry["oid"]
            for entry in (destination_workspace or {}).get("recovery_anchors", [])
        ):
            name = f"refs/bulkload/recovery/destination/anchor/{oid}"
            existing = destination_refs.get(name)
            if existing is not None and existing["oid"] != oid:
                _block(blockers, "git-recovery-ref-collision", name)
            elif existing is None:
                ref_actions.append(
                    {
                        "action": "retain-destination-recovery",
                        "name": name,
                        "oid": oid,
                    }
                )
        for oid in sorted(source_anchor_oids - destination_anchor_oids):
            name = f"refs/bulkload/recovery/source/{oid}"
            existing = destination_refs.get(name)
            if existing is not None and existing["oid"] != oid:
                _block(blockers, "git-recovery-ref-collision", name)
            elif existing is None:
                ref_actions.append(
                    {"action": "retain-source-recovery", "name": name, "oid": oid}
                )
        operations.append(
            _operation(
                "git-workspace-union",
                destination_ref=_catalog_ref(
                    "git-workspace",
                    workspace_id=destination_workspace["workspace_id"],
                )
                if destination_workspace is not None
                else None,
                destination_path=target_primary,
                ref_actions=ref_actions,
                source_ref=_catalog_ref(
                    "git-workspace", workspace_id=source_workspace["workspace_id"]
                ),
            )
        )


def _plan_non_git(
    source: dict[str, Any],
    destination: dict[str, Any],
    operations: list[dict[str, Any]],
    blockers: list[dict[str, str]],
) -> None:
    source_root = source["root_bindings"]["destination_git_root"]
    destination_root = destination["root_bindings"]["git_root"]
    if source_root != destination_root:
        _block(blockers, "git-root-binding-divergence", destination_root)
        return
    source_items = {item["identity"]: item for item in source.get("non_git", [])}
    destination_items = {
        item["identity"]: item for item in destination.get("non_git", [])
    }
    blocked_worktrees: set[str] = set()
    for identity, item in sorted(source_items.items()):
        target = str(Path(destination_root) / item["destination_relative_path"])
        git_collision = _containing_git_worktree(
            destination.get("git_workspaces", []), target
        )
        if git_collision is not None:
            if git_collision not in blocked_worktrees:
                _block(
                    blockers,
                    "non-git-git-workspace-collision",
                    git_collision,
                )
                blocked_worktrees.add(git_collision)
            continue
        before = destination_items.get(identity)
        if _structural_collision(item, before):
            _block(blockers, "structural-type-collision", target)
            continue
        if before is None or _fingerprint(item) != _fingerprint(before):
            operations.append(
                _operation(
                    "file-install",
                    destination_ref=_catalog_ref("non-git", identity=before["identity"])
                    if before is not None
                    else None,
                    destination_path=target,
                    owner={"class": "non-git", "name": "git-root"},
                    source_ref=_catalog_ref("non-git", identity=item["identity"]),
                )
            )


def _provider_map(catalog: dict[str, Any]) -> dict[str, dict[str, Any]]:
    return {provider["name"]: provider for provider in catalog.get("providers", [])}


def _items_by_identity(
    provider: dict[str, Any],
) -> tuple[dict[str, dict[str, Any]], list[str]]:
    result: dict[str, dict[str, Any]] = {}
    duplicates: list[str] = []
    for item in provider.get("items", []):
        identity = provider_item_identity(item)
        if identity in result:
            duplicates.append(identity)
        else:
            result[identity] = item
    return result, duplicates


def _append_relation(source: dict[str, Any], destination: dict[str, Any]) -> str:
    source_records = source.get("translated_records", source["records"])
    destination_records = destination["records"]
    if source_records == destination_records:
        return "equal"
    if destination_records == source_records[: len(destination_records)]:
        return "source-superset"
    if source_records == destination_records[: len(source_records)]:
        return "destination-superset"
    return "divergent"


def _plan_providers(
    source: dict[str, Any],
    destination: dict[str, Any],
    operations: list[dict[str, Any]],
    blockers: list[dict[str, str]],
    holds: list[dict[str, str]],
) -> None:
    source_providers = _provider_map(source)
    destination_providers = _provider_map(destination)
    for name, source_provider in sorted(source_providers.items()):
        destination_provider = destination_providers.get(
            name,
            {
                "destination_path": source_provider["destination_path"],
                "items": [],
            },
        )
        if source_provider.get("destination_path") != destination_provider.get(
            "destination_path"
        ):
            _block(blockers, "provider-root-binding-divergence", name)
            continue
        source_items, source_duplicates = _items_by_identity(source_provider)
        destination_items, destination_duplicates = _items_by_identity(
            destination_provider
        )
        destination_paths = {
            provider_item_destination(item): item
            for item in destination_provider.get("items", [])
        }
        for identity in sorted(set(source_duplicates + destination_duplicates)):
            _block(blockers, "agent-identity-collision", f"{name}:{identity}")
        for identity, source_item in sorted(source_items.items()):
            classification = source_item["classification"]
            destination_item = destination_items.get(identity)
            target = str(
                Path(source_provider["destination_path"])
                / provider_item_destination(source_item)
            )
            owner = {"class": "provider", "name": name}
            if _structural_collision(
                source_item,
                destination_paths.get(provider_item_destination(source_item)),
            ):
                _block(blockers, "structural-type-collision", target)
                continue
            if classification == "regenerate":
                continue
            if classification == "unknown":
                _block(blockers, "unknown-agent-state", f"{name}:{identity}")
                continue
            if classification == "nonportable-auth":
                holds.append(
                    {
                        "code": "nonportable-auth",
                        "path": f"{name}:{identity}",
                        "resolution": "preserve-destination-and-reauthenticate-attended",
                    }
                )
                continue
            if classification == "sqlite":
                operations.append(
                    _operation(
                        "sqlite-union",
                        destination_ref=_catalog_ref(
                            "provider", name=name, identity=identity
                        )
                        if destination_item is not None
                        else None,
                        destination_path=target,
                        owner=owner,
                        source_ref=_catalog_ref(
                            "provider", name=name, identity=identity
                        ),
                    )
                )
                continue
            if classification.startswith("append-jsonl"):
                if destination_item is None:
                    relation = "source-only"
                else:
                    relation = _append_relation(source_item, destination_item)
                mode_drift = relation == "equal" and _fingerprint(
                    source_item, translated=classification.endswith("rewrite")
                ) != _fingerprint(destination_item)
                if (
                    relation
                    in {
                        "source-only",
                        "source-superset",
                        "divergent",
                    }
                    or mode_drift
                ):
                    operations.append(
                        _operation(
                            "file-install",
                            destination_ref=_catalog_ref(
                                "provider", name=name, identity=identity
                            )
                            if destination_item is not None
                            else None,
                            destination_path=target,
                            owner=owner,
                            source_ref=_catalog_ref(
                                "provider", name=name, identity=identity
                            ),
                            transform="path-rewrite"
                            if classification.endswith("rewrite")
                            else None,
                        )
                    )
                continue
            if classification == "portable-auth":
                operations.append(
                    _operation(
                        "auth-install",
                        destination_ref=_catalog_ref(
                            "provider", name=name, identity=identity
                        )
                        if destination_item is not None
                        else None,
                        destination_path=target,
                        owner=owner,
                        source_ref=_catalog_ref(
                            "provider", name=name, identity=identity
                        ),
                    )
                )
                continue
            translated = classification.endswith("rewrite")
            if destination_item is None or _fingerprint(
                source_item, translated=translated
            ) != _fingerprint(destination_item):
                operations.append(
                    _operation(
                        "file-install",
                        destination_ref=_catalog_ref(
                            "provider", name=name, identity=identity
                        )
                        if destination_item is not None
                        else None,
                        destination_path=target,
                        owner=owner,
                        source_ref=_catalog_ref(
                            "provider", name=name, identity=identity
                        ),
                        transform="path-rewrite" if translated else None,
                    )
                )


def _seat_map(catalog: dict[str, Any]) -> dict[str, dict[str, Any]]:
    return {seat["name"]: seat for seat in catalog.get("seats", [])}


def _plan_seats(
    source: dict[str, Any],
    destination: dict[str, Any],
    operations: list[dict[str, Any]],
    blockers: list[dict[str, str]],
) -> None:
    destinations = _seat_map(destination)
    for name, source_seat in sorted(_seat_map(source).items()):
        destination_seat = destinations.get(
            name,
            {
                "destination_path": source_seat["destination_path"],
                "items": [],
                "root_kind": source_seat["root_kind"],
            },
        )
        if source_seat.get("destination_path") != destination_seat.get(
            "destination_path"
        ) or source_seat.get("root_kind") != destination_seat.get("root_kind"):
            _block(blockers, "seat-root-binding-divergence", name)
            continue
        destination_items = {
            item["identity"]: item for item in destination_seat.get("items", [])
        }
        for item in source_seat.get("items", []):
            before = destination_items.get(item["identity"])
            if _structural_collision(item, before):
                target = (
                    source_seat["destination_path"]
                    if source_seat["root_kind"] == "file"
                    else str(
                        Path(source_seat["destination_path"]) / item["relative_path"]
                    )
                )
                _block(blockers, "structural-type-collision", target)
                continue
            if before is not None and _fingerprint(item) == _fingerprint(before):
                continue
            operations.append(
                _operation(
                    "file-install",
                    destination_ref=_catalog_ref(
                        "mutable-seat", name=name, identity=before["identity"]
                    )
                    if before is not None
                    else None,
                    destination_path=(
                        source_seat["destination_path"]
                        if source_seat["root_kind"] == "file"
                        else str(
                            Path(source_seat["destination_path"])
                            / item["relative_path"]
                        )
                    ),
                    owner={"class": "mutable-seat", "name": name},
                    source_ref=_catalog_ref(
                        "mutable-seat", name=name, identity=item["identity"]
                    ),
                )
            )


def _all_destination_file_digests(catalog: dict[str, Any]) -> set[str]:
    digests: set[str] = set()

    def add(item: dict[str, Any]) -> None:
        value = item.get("sha256")
        if item.get("kind") == "regular" and value:
            digests.add(value)

    for item in catalog.get("non_git", []):
        add(item)
    for workspace in catalog.get("git_workspaces", []):
        for item in workspace.get("object_files", []):
            add(item)
        for worktree in workspace.get("worktrees", []):
            for item in worktree.get("files", []):
                add(item)
            index = worktree.get("index", {})
            if index.get("exists"):
                digests.add(index["sha256"])
    for provider in catalog.get("providers", []):
        for item in provider.get("items", []):
            add(item)
    for seat in catalog.get("seats", []):
        for item in seat.get("items", []):
            add(item)
    return digests


def _capacity_contract(
    source: dict[str, Any],
    destination: dict[str, Any],
    operations: Iterable[dict[str, Any]],
) -> dict[str, int | str]:
    destination_digests = _all_destination_file_digests(destination)
    unique: dict[str, int] = {}
    transformed_bytes = 0
    sqlite_bytes = 0
    rollback_bytes = 0

    def charge_source(item: dict[str, Any], *, transformed: bool = False) -> None:
        nonlocal transformed_bytes
        if item.get("kind") != "regular":
            return
        digest = (
            item.get("translated_sha256", item.get("sha256"))
            if transformed
            else item.get("sha256")
        )
        size = (
            item.get("translated_size", item.get("size", 0))
            if transformed
            else item.get("size", 0)
        )
        if digest:
            unique.setdefault(digest, size)
            if transformed:
                transformed_bytes += size

    for operation in operations:
        before = operation.get("destination_before")
        if isinstance(before, dict) and operation["kind"] != "git-workspace-union":
            rollback_bytes += int(before.get("size", 0))
        if operation["kind"] in {"file-install", "auth-install"}:
            charge_source(
                operation["source"],
                transformed=operation.get("transform") == "path-rewrite",
            )
        elif operation["kind"] == "sqlite-union":
            source_size = int(operation["source"].get("size", 0))
            destination_size = int((before or {}).get("size", 0))
            sqlite_bytes += source_size + destination_size
        elif operation["kind"] == "git-workspace-union":
            workspace = operation["source"]
            for item in workspace.get("object_files", []):
                charge_source(item)
            for worktree in workspace.get("worktrees", []):
                for item in worktree.get("files", []):
                    charge_source(item)
                index = worktree.get("index", {})
                if index.get("exists"):
                    unique.setdefault(index["sha256"], index["size"])
                destination_worktree = next(
                    (
                        item
                        for item in (before or {}).get("worktrees", [])
                        if item["path"] == worktree["destination_path"]
                    ),
                    None,
                )
                if destination_worktree is not None:
                    for item in destination_worktree.get("files", []):
                        if item["kind"] != "directory":
                            rollback_bytes += int(item.get("size", 0))
                    destination_index = destination_worktree.get("index", {})
                    if destination_index.get("exists"):
                        rollback_bytes += int(destination_index.get("size", 0))
    reflink_reusable = sum(
        size for digest, size in unique.items() if digest in destination_digests
    )
    incoming = sum(
        size for digest, size in unique.items() if digest not in destination_digests
    )
    return {
        "formula": "available >= incoming_unique + sqlite_compose + exact_overwritten + reserve",
        "incoming_unique_bytes": incoming,
        "reflink_reusable_bytes": reflink_reusable,
        "rollback_exact_overwritten_bytes": rollback_bytes,
        "sqlite_compose_bytes": sqlite_bytes,
        "transformed_bytes": transformed_bytes,
    }


def compile_agent_plan_authorities(
    source_capture: dict[str, Any],
    source_capture_ids: tuple[str, str],
    destination_capture: dict[str, Any],
    destination_capture_ids: tuple[str, str],
) -> dict[str, Any]:
    validate_agent_capture(source_capture, expected_role="source")
    validate_agent_capture(destination_capture, expected_role="destination")
    source = source_capture["catalog"]
    destination = destination_capture["catalog"]
    if source["path_map"] != destination["path_map"]:
        raise BulkloadError("source and destination path-map contracts differ")
    if source["provider_policy"] != destination["provider_policy"]:
        raise BulkloadError("source and destination managed-exclusion policies differ")
    capture_ids = {*source_capture_ids, *destination_capture_ids}
    if len(capture_ids) != 4:
        raise BulkloadError("all four capture IDs must be globally distinct")
    operations: list[dict[str, Any]] = []
    blockers: list[dict[str, str]] = []
    holds: list[dict[str, str]] = []
    _plan_git(source, destination, operations, blockers)
    _plan_non_git(source, destination, operations, blockers)
    _plan_providers(source, destination, operations, blockers, holds)
    _plan_seats(source, destination, operations, blockers)
    operations.sort(
        key=lambda item: (item["kind"], item["destination_path"], item["operation_id"])
    )
    blockers.sort(key=lambda item: (item["code"], item["path"], item.get("detail", "")))
    holds.sort(key=lambda item: (item["code"], item["path"]))
    authority = {
        "source": {"catalog": source},
        "destination": {"catalog": destination},
    }
    expanded_operations: Iterable[dict[str, Any]] = ()
    if not blockers:
        resolver = PlanOperationResolver(authority)
        expanded_operations = (
            resolver.materialize(operation) for operation in operations
        )
    plan = {
        "blockers": blockers,
        "capacity": _capacity_contract(source, destination, expanded_operations),
        "created_at": utc_now(),
        "destination": {
            "capture_ids": list(destination_capture_ids),
            "catalog": destination,
            "catalog_sha256": destination_capture["catalog_sha256"],
        },
        "holds": holds,
        "operations": operations if not blockers else [],
        "path_map": source["path_map"],
        "plan_id": new_id(),
        "ready": not blockers,
        "required_stage_phases": ["preseed", "final"],
        "schema": AGENT_PLAN_SCHEMA,
        "source": {
            "capture_ids": list(source_capture_ids),
            "catalog": source,
            "catalog_sha256": source_capture["catalog_sha256"],
        },
    }
    return seal(plan, "plan_sha256")


def validate_agent_plan(value: dict[str, Any], *, require_ready: bool = False) -> None:
    require_exact_keys(
        value,
        {
            "blockers",
            "capacity",
            "created_at",
            "destination",
            "holds",
            "operations",
            "path_map",
            "plan_id",
            "plan_sha256",
            "ready",
            "required_stage_phases",
            "schema",
            "source",
        },
        "AgentPlanV4",
    )
    if value.get("schema") != AGENT_PLAN_SCHEMA:
        raise BulkloadError("input is not an AgentPlanV4")
    require_digest(value, "plan_sha256")
    if value.get("ready") != (not value.get("blockers")):
        raise BulkloadError("AgentPlanV4 readiness disagrees with blockers")
    if require_ready and not value["ready"]:
        raise BulkloadError("AgentPlanV4 is blocked")
    if value.get("required_stage_phases") != ["preseed", "final"]:
        raise BulkloadError("AgentPlanV4 stage contract is invalid")
    require_exact_keys(
        value["capacity"],
        {
            "formula",
            "incoming_unique_bytes",
            "reflink_reusable_bytes",
            "rollback_exact_overwritten_bytes",
            "sqlite_compose_bytes",
            "transformed_bytes",
        },
        "AgentPlanV4 capacity",
    )
    for role in ("source", "destination"):
        require_exact_keys(
            value[role],
            {"capture_ids", "catalog", "catalog_sha256"},
            f"AgentPlanV4 {role}",
        )
    if value.get("path_map") != value.get("source", {}).get("catalog", {}).get(
        "path_map"
    ):
        raise BulkloadError("AgentPlanV4 path map is detached from source authority")
    if value.get("path_map") != value.get("destination", {}).get("catalog", {}).get(
        "path_map"
    ):
        raise BulkloadError(
            "AgentPlanV4 path map is detached from destination authority"
        )
    current_runtime = runtime_source_digest()
    if (
        value.get("source", {}).get("catalog", {}).get("runtime_source_sha256")
        != current_runtime
    ):
        raise BulkloadError(
            "AgentPlanV4 source runtime closure differs from this executable"
        )
    if (
        value.get("destination", {}).get("catalog", {}).get("runtime_source_sha256")
        != current_runtime
    ):
        raise BulkloadError(
            "AgentPlanV4 destination runtime closure differs from this executable"
        )
    resolver = PlanOperationResolver(value) if value.get("operations") else None
    for operation in value.get("operations", []):
        expected = sha256_bytes(
            canonical_bytes(
                {key: item for key, item in operation.items() if key != "operation_id"}
            )
        )
        if operation.get("operation_id") != expected:
            raise BulkloadError("AgentPlanV4 operation digest mismatch")
        assert resolver is not None
        resolver.materialize(operation)
