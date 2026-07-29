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
`codex-private-state-policy.v5.json`. Policy v5 retains the narrow, attended
typed auth install while preserving destination SQLite exactly. It accepts the
complete legacy v4 four-pass, session-bound SQLite opening as immutable
evidence, adds a fresh cross-plane close, and may compile a descriptive offline
composition action plan only after reopening every original v4 input before
and after publication. It does not implement the composer, publication, SQLite
installation, combined apply, activation, or cutover.

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

Read `codex-private-state-policy.v5.json`,
[`codex-private-sqlite-compose-plan.md`](codex-private-sqlite-compose-plan.md),
[`codex-private-sqlite-compose-action-plan.md`](codex-private-sqlite-compose-action-plan.md),
and
[`codex-private-auth-install.md`](codex-private-auth-install.md) before
capturing or installing auth.

- Prefer source auth-only plus destination auth-and-SQLite evidence. Full
  source and destination captures are accepted, but source SQLite is never
  consumed.
- Resolve destination SQLite authority in this order: configured
  `sqlite_home`, `CODEX_SQLITE_HOME`, then `CODEX_HOME`, and pass the effective
  absolute path explicitly. Never infer it during capture.
- Any `-wal`, `-shm`, or `-journal` sidecar hard-stops immutable SQLite
  capture. Do not raw-copy or bypass the sidecar fence.
- Preserve every destination SQLite family exactly and require zero SQLite
  auth-install mutations. The legacy v4 opening may classify exact structural
  and type-tagged row relations. V5 may close that opening with fresh source
  and destination private/session A/B evidence and compile expected output
  counts, semantic digests, and a descriptive operation graph for families
  whose schema and migration state are already exact. Registered-prefix skew
  remains blocked because no pinned migration adapter executes in this slice.
  SQLite
  compose/publish/install, combined apply, provider acceptance, and cutover
  remain false.
- Treat each quiescence attestation as an operator procedural fence with
  `provider_writer_proof=false`. The directory `flock` coordinates Bulkload
  only; independently stop provider writers for the complete operation.
- Keep private bundles, plans, journals, backups, and receipts owner-only and
  outside live roots. Never print credential values.
- Require the pre-import pinned runtime authority carried by each plan. A
  changed policy or runtime source fails closed.
- Treat apply and verify receipts as offline byte/custody evidence only. Prove
  working authentication with a fresh attended provider turn.

The compatibility plan records version/family/schema/migration/header and
bounded thread/path relations but remains non-actionable. The separately
digest-accepted install plan can authorize only atomic `auth.json` replacement.
Its apply path creates a complete destination-auth rollback artifact, writes a
durable journal, preserves destination SQLite, supports bounded recovery, and
publishes offline apply/verify/rollback receipts. It never activates Codex or
claims provider acceptance. A v5 SQLite action plan is separate evidence for a
future internal offline composer. `descriptive_action_complete=true` can record
an exact-schema semantic closure, but `ready_for_offline_compose=false` remains
invariant because output and scratch authority are absent. No composition
command exists, and the plan authorizes no live-root mutation, publication,
installation, session execution, or provider-readiness claim.

Quarantine AppleDouble `._*` files outside interpreted rules/skill/config
directories. Record path, size, mode, type, and SHA-256; do not silently delete
evidence.

## Claude

Treat project transcript paths, plans/tasks, file history, and auto-memory as
potentially portable only after exact path mapping. Project directory slugs may
encode the physical cwd and may collide. Regenerate settings containing Nix
store paths, plugins with platform binaries, caches, daemons, shell snapshots,
and indexes. Move credentials only through a provider-specific typed policy.
Policy v5 authorizes Codex `auth.json`, not Claude keychain or credential
state, so use attended reauthentication for Claude.

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
