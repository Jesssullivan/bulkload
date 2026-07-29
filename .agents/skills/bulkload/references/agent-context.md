# Agent context migration

## General procedure

1. Inventory provider state without printing credential contents.
2. Classify each path as transcript, durable memory, config, generated index,
   cache, live database, or credential.
3. Recreate repo-managed configuration on the destination first.
4. Select an explicit provider policy for auth and database state.
   Reauthentication is a fallback, not a mandatory substitute for a safe copy.
5. Copy only explicitly allowlisted, quiesced or consistently snapshotted
   artifacts.
6. Preserve source files and use create-if-absent or append/superset checks.
7. Validate file format and owner-only modes.
8. Create one fresh non-secret nonce dialog and intentionally resume it.
9. Treat historical recall and fresh continuity as separate proofs.

## Codex

Portable candidates include closed rollout JSONL files, append-only
`history.jsonl`, reviewed durable memory, `auth.json`, and provider-owned
SQLite families. Keep `config.toml`, rules directories, caches, installation
IDs, shell snapshots, and active rollouts out of generic copying. Handle auth
and SQLite only through the exact policy in
`codex-private-state-policy.v2.json`; the current skill has a private capture
reader and compatibility planner but no installer.

Before copying a rollout:

- prove it is a regular non-symlink file;
- prove no process has it open;
- hash and size it twice around the copy;
- require the destination path to be absent;
- create with mode `0600` under `0700` directories; and
- validate every JSONL record afterward.

When two same-UUID rollouts differ, require targeted evidence bound to both
catalogs. Promote a longer source only when the complete destination bytes are
its exact prefix ending at a JSONL record boundary. Preserve a longer
destination under the symmetric proof. Equal-size differing bytes, a
mid-record cut, missing evidence, or true divergence blocks the whole plan.

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
their resolved paths and recorded lineage. Its positive proposals are
destination-absent copies and a proved source-superset promotion; a proved
destination superset is preserved. Same-UUID/same-hash is a no-op. Equal-size
differing bytes, unproved divergence, and cross-input portable file/file,
file/directory, or ancestor-prefix collisions are blockers. One blocker
suppresses every copy candidate. After all required prefix proofs, compile a
close request that binds the opening custody plus every proof capture ID and
proof digest. Only then capture fresh closing A/B catalogs for both roots with
`codex-close-capture`; each v1 wrapper binds the close-request digest and
contains a newly captured, unchanged v2 snapshot. The wrappers must exactly
match the opening catalog/custody bodies, and every opening, proof, and closing
snapshot must have a distinct capture ID. The plan binds the prefix request,
every proof, the close request, and every wrapper digest. Session snapshots
remain v2; prefix requests/proofs, close requests/wrappers, and the union plan
use their separately versioned schemas. Inputs must be exact `0600`,
current-user, single-link regular files; the CLI pins them through a
create-only atomic publish in an owner-private output parent, verifies exact
temporary bytes and single-link custody, uses an OS no-replace rename, and
revalidates inputs plus the requested directory/target after publication. It
refuses an existing target and never pathname-deletes on failure; treat any
staging or final artifact left by a nonzero command as fail-held until attended
quarantine. The adapter remains dry-run-only and has no copy/apply operation.
Keep its UUID-bearing evidence owner-private and outside both session roots.

Compaction is a semantic boundary: raw historical bytes may remain in JSONL
while the resumed model receives only the newest replacement history. A
successful session lookup therefore does not prove exact old checklist recall.
Do not use an “ephemeral” flag as an immutability guarantee without measuring
the source file before and after.

## Codex private state

Read `codex-private-state-policy.v2.json` before planning auth or SQLite.

- Resolve SQLite authority in this order: configured `sqlite_home`,
  `CODEX_SQLITE_HOME`, then `CODEX_HOME`. Pass that resolved absolute path
  explicitly as `--sqlite-home`; private capture does not infer it.
- Enumerate all provider-owned SQLite families. Do not chase one remembered
  filename; state, logs, goals, and memories have all carried durable runtime
  state.
- Capture each family with SQLite online backup or while writers are
  quiescent. Never raw-copy a live database/WAL/SHM triplet.
- Require matching Codex versions, compatible migration/schema fingerprints,
  source and destination backups, `quick_check`, canonical destination rollout
  paths, atomic install, directory sync, rollback, picker parity, historical
  resume, and a fresh persisted turn.
- Copy `auth.json` only as an explicit password-equivalent artifact with
  owner-only custody, no value logging, atomic replacement, rollback, and a
  fresh authenticated turn.

Capture source and destination separately:

```bash
python3 scripts/bulkload.py codex-private-capture \
  --codex-home /absolute/private/codex-home \
  --sqlite-home /absolute/effective/sqlite-home \
  --role source \
  --host-authority-id 11111111-1111-4111-8111-111111111111 \
  --codex-version 0.145.0 \
  --include-auth --include-sqlite \
  --acknowledge-private-capture \
  --output-directory /secure/evidence/source-private
```

Repeat with role `destination` and a distinct authority ID, then run
`codex-private-plan` against the two bundle directories. Capture requires an
existing `0700` output parent, publishes the bundle with an atomic no-replace
rename, stores artifacts and its manifest as `0600`, and retains any failed
staging directory for attended quarantine. SQLite capture enumerates every
top-level `*.sqlite` family, uses the online backup API, normalizes the snapshot
to delete-journal mode, and never transfers source WAL/SHM files. The manifest
binds the opt-in state classes, effective authorities, and count/byte/time
budgets; family discovery is repeated after backup.

The compatibility planner rehashes every private artifact and records
version/family/schema/migration/header mismatches plus bounded thread/path set
relations. It does not merge or install auth, state, logs, goals, or memories.
The validator
enforces `implementation=capture-and-compatibility-plan`,
`ready_for_apply=false`, and the absence of `codex-private-apply`.

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
