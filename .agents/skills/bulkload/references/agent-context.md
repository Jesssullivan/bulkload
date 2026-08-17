# Agent context migration

## General procedure

1. Inventory provider state without printing credential contents.
2. Classify each path as transcript, durable memory, config, generated index,
   cache, live database, or credential.
3. Recreate repo-managed configuration on the destination first.
4. Re-authenticate on the destination.
5. Copy only explicitly allowlisted, quiesced transcript or memory artifacts.
6. Preserve source files and use create-if-absent or append/superset checks.
7. Validate file format and owner-only modes.
8. Create one fresh non-secret nonce dialog and intentionally resume it.
9. Treat historical recall and fresh continuity as separate proofs.

## Codex

Portable candidates are closed rollout JSONL files and reviewed durable memory.
Do not copy `auth.json`, `config.toml`, rules directories wholesale, session
indexes, `history.jsonl`, SQLite databases, WAL/SHM, logs, goals databases,
caches, installation IDs, shell snapshots, or active rollouts.

Before copying a rollout:

- prove it is a regular non-symlink file;
- prove no process has it open;
- hash and size it twice around the copy;
- require the destination path to be absent;
- create with mode `0600` under `0700` directories; and
- validate every JSONL record afterward.

When two same-name rollouts exist, preserve both until prefix/superset evidence
establishes which is authoritative. Never overwrite the longer destination
with a shorter source.

Compaction is a semantic boundary: raw historical bytes may remain in JSONL
while the resumed model receives only the newest replacement history. A
successful session lookup therefore does not prove exact old checklist recall.
Do not use an “ephemeral” flag as an immutability guarantee without measuring
the source file before and after.

Quarantine AppleDouble `._*` files outside interpreted rules/skill/config
directories. Record path, size, mode, type, and SHA-256; do not silently delete
evidence.

## Claude

Treat project transcript paths, plans/tasks, file history, and auto-memory as
potentially portable only after exact path mapping. Project directory slugs may
encode the physical cwd and may collide. Regenerate settings containing Nix
store paths, plugins with platform binaries, caches, daemons, shell snapshots,
and indexes. Re-authenticate; never copy keychain or credential files.

Do not blanket-rewrite binary or SQLite content. Use provider-supported
re-indexing or a verified text/JSON transformation with a source backup.

## Pi

Pi discovers portable skills under `~/.agents/skills` and
`~/.pi/agent/skills`. Treat conversation/provider state according to its own
current format and authentication documentation; skill discovery does not make
session or credential stores portable.

## Continuity claim

Claim `SESSION_NATIVE` only after the destination creates a fresh thread under
fresh auth, stores it owner-only, intentionally resumes its exact identifier,
and reproduces a non-secret nonce or handoff payload without reading files or
credentials during the proof.
