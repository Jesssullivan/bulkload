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

Use `bulkload codex-capture` only after both Codex writers are quiescent. It
requires an explicit `source` or `destination` role plus the
`--acknowledge-writers-quiesced` assertion and a canonical, non-secret
`--host-authority-id`. Reuse one authority ID for every capture that can address
the same physical filesystem namespace, including a renamed host or shared
storage; use a different ID for an independent host-local namespace. Source
evidence may record legacy group/world-readable files and directories, but
nothing writable by group/other. Destination evidence requires exact `0600`
files under exact `0700` directories. Regular files must be current-user,
single-link, and non-symlink.

Capture uses strict UTF-8 JSON parsing: duplicate keys and non-finite values are
invalid, the first record must be `session_meta`, exactly one such record may
exist, and its canonical UUID must match the filename. It fails closed on
unexpected files, duplicate or portable-colliding paths, special files,
hardlinks, foreign authority, budget exhaustion, or an observed file,
directory, subtree, or root-identity change. Every traversal, content, error,
catalog, and output surface is bounded. Candidate files and their stable sizes
are charged before JSON parsing, so malformed rollouts cannot multiply the
configured file or byte allowance.

Use `bulkload codex-plan` only with distinct source A/B and destination A/B
captures. Each pair must have identical catalog bodies, hosts, roots, roles,
filesystem identities, typed directory claims, root lineage, authority IDs,
and budgets; all writers must be acknowledged quiescent. Within one host
authority, equal or nested source and destination roots are rejected through
their resolved paths and recorded lineage. Its only positive proposal is
`copy-if-absent` for a UUID missing from the destination.
Same-UUID/same-hash is a no-op; destination-only is preserved;
same-UUID/different-hash and cross-input portable file/file, file/directory, or
ancestor-prefix collisions are blockers. One blocker suppresses every copy
candidate. Snapshot and plan artifacts use the Codex v2 schemas. Inputs must be
exact `0600`, current-user, single-link regular files; the CLI pins them and a
private output parent until the atomic rename. The adapter remains
dry-run-only and has no copy/apply operation. Keep its UUID-bearing evidence
owner-private and outside both session roots.

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
