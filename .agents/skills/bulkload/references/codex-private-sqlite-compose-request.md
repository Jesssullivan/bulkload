# Codex private SQLite compose request and capacity observation v6

## Status and authority boundary

Policy v6 consumes one exact, complete v5 SQLite action plan and emits two
owner-private evidence artifacts:

1. a semantic compose request that binds the intended create-only workspace
   and a deterministic conservative capacity requirement; and
2. a separate, short-lived observation of caller-available capacity on that
   workspace filesystem.

Neither command composes, reserves, publishes, installs, replaces, deletes, or
activates SQLite state. Both return exit `4`. These claims remain false:

- `composer_implemented`;
- `ready_for_internal_offline_compose`;
- `ready_for_offline_compose`;
- `compose_authorized`;
- `publication_authorized`;
- `install_authorized`;
- `combined_authorized`;
- `apply_authorized`;
- `provider_runtime_acceptance`; and
- `provider_writer_proof`.

The capacity observation is not a reservation, quota proof, future-write
guarantee, readiness proof, or execution lease. Its fixed lifetime is 300
seconds. A future composer must obtain and validate a fresh observation, then
establish its own write/reservation authority immediately before work.

## Immutable producer and current consumer

The request accepts only an action plan produced by the exact repaired v5
closure:

- source commit:
  `3daf764660f45c0d1e71383f1269237a0cfe991b`;
- policy schema:
  `dev.tinyland.bulkload.codex-private-state-policy.v5`;
- policy SHA-256:
  `13fa05eecb0eefb2f697735a4ad7351834695e26a7dd0a910be2e87b035ca82d`;
- runtime closure SHA-256:
  `8193d692be87174c6618450fa84cd44a0755661f2cf50cab791e50725dabcad7`;
  and
- the exact eight-file v5 source-digest inventory embedded in that policy.

Policy v6 is a distinct consumer authority. It cannot self-redigest a modified
v5 producer, reinterpret another policy version as v5, or substitute its own
runtime for the action-plan producer. The request records the current v6
runtime separately and deliberately leaves
`required_composer_runtime_authority=null`.

The v4 opening and v5 close/action artifacts remain immutable inputs. Produce
or repair them only from the exact reviewed v4/v5 source revisions. The v6
slice does not widen or regenerate their authority.

The active v6 CLI therefore does not register the legacy
`codex-private-sqlite-compose-plan`, close-request, reclose, or action-plan
producer commands. Their validators remain solely to authenticate the complete
immutable input chain. Use an exact reviewed v4/v5 checkout for historical
production; never make a v6-bound opening that no exact-v5 close can consume.

## Required evidence chain

Keep every input `0600`, current-user, single-link, and non-symlink in an exact
`0700` evidence directory outside live state and the proposed workspace. Pass
the complete original chain:

- v5 action plan and exact accepted digest;
- v4 opening plan;
- v5 close request;
- opening private compatibility plan;
- opening source A/B and destination A/B private bundles;
- opening adapter registry and path map;
- opening session-union plan;
- opening source A/B and destination A/B session captures;
- every optional prefix/close artifact required by that session plan;
- closing source A/B and destination A/B private bundles; and
- closing source A/B and destination A/B session wrappers.

The CLI pins every JSON file and reopens every private bundle. It recomputes
the complete v4 opening, v5 close, and v5 action plan before compilation,
before create-only publication, and after publication. A persisted
`opening_inputs_revalidated` field is never trusted by itself.

## Workspace contract

Create the workspace parent before the request. It must be an existing,
current-user directory with exact mode `0700`. The request command does not
create it.

The command opens every absolute path component with no-follow directory
descriptors, binds the parent identity, mount identity, and full directory
lineage, and requires:

- the final output leaf to be absent;
- the action-digest-derived final leaf
  `bulkload-sqlite-compose-<ACTION_PLAN_SHA256>`;
- an empty, action-specific
  `.bulkload-sqlite-compose-<ACTION_PLAN_SHA256>-staging-*` namespace;
- any future staging entry to be a randomized sibling of the final leaf;
- same-parent and same-mount future publication;
- future directory mode `0700` and file mode `0600`; and
- no lexical or descriptor-proved ancestor, descendant, or alias relation with
  any protected namespace.

Protected namespaces include every opening/closing private bundle, every
protocol-evidence parent, the current runtime root, and all recorded live
Codex, SQLite, and session roots. Existing namespaces are descriptor-pinned.
Recorded-but-absent live roots are rechecked component by component so a
symlink ancestor cannot conceal them.

The request records absence only. It creates no final leaf, staging entry,
lock, reservation, output database, receipt, or manifest.

## Capacity requirement

The request derives a bounded requirement from the exact v5 action plan using
checked 63-bit nonnegative arithmetic. It includes:

- all immutable source and destination SQLite snapshot bytes;
- twice the classified-row byte charge;
- three copies of the resulting regular-output upper bound for final,
  temporary-duplicate, and SQLite-journal contingencies;
- 64 KiB of page rounding per family;
- bounded manifest and receipt space;
- a conservative inode-derived metadata allowance;
- eight additional inodes; and
- a fixed 1 GiB safety margin.

Overflow, a missing expected-output record, an incomplete action plan, any
action-plan blocker, or zero families fails closed. This is a conservative
preflight bound, not an allocation.

## Compile the request

Use `umask 077`. Review the exact v5 action-plan digest and substitute only
reviewed absolute paths:

```bash
python3 scripts/bulkload.py codex-private-sqlite-compose-request \
  --action-plan /secure/evidence/sqlite-action-v5.json \
  --accept-action-plan ACTION_PLAN_SHA256 \
  --opening-plan /secure/evidence/sqlite-opening-v4.json \
  --close-request /secure/evidence/sqlite-close-request-v5.json \
  --opening-compatibility-plan /secure/evidence/private-opening-plan.json \
  --opening-source-a-bundle /secure/evidence/opening-source-a \
  --opening-source-b-bundle /secure/evidence/opening-source-b \
  --opening-destination-a-bundle /secure/evidence/opening-destination-a \
  --opening-destination-b-bundle /secure/evidence/opening-destination-b \
  --opening-adapter-registry /secure/evidence/sqlite-adapters.json \
  --opening-path-map /secure/evidence/sqlite-path-map.json \
  --opening-session-union-plan /secure/evidence/session-union.json \
  --opening-session-source-a /secure/evidence/session-opening-source-a.json \
  --opening-session-source-b /secure/evidence/session-opening-source-b.json \
  --opening-session-destination-a /secure/evidence/session-opening-destination-a.json \
  --opening-session-destination-b /secure/evidence/session-opening-destination-b.json \
  --source-close-a-bundle /secure/evidence/closing-source-a \
  --source-close-b-bundle /secure/evidence/closing-source-b \
  --destination-close-a-bundle /secure/evidence/closing-destination-a \
  --destination-close-b-bundle /secure/evidence/closing-destination-b \
  --session-source-close-a /secure/evidence/session-closing-source-a.json \
  --session-source-close-b /secure/evidence/session-closing-source-b.json \
  --session-destination-close-a /secure/evidence/session-closing-destination-a.json \
  --session-destination-close-b /secure/evidence/session-closing-destination-b.json \
  --workspace-parent /secure/sqlite-compose-workspace \
  --output /secure/evidence/sqlite-compose-request-v6.json
```

When the accepted session-union plan contains prefix or close evidence, pass
the matching optional `--opening-session-*-prefix-*` and
`--opening-session-*-close-*` arguments too. Omitting required evidence blocks
the complete-chain recomputation.

Exit `4` is expected. Review the exact request digest, both runtime
authorities, action binding, workspace identity and protected namespaces,
capacity derivation, and every false authority/readiness claim. Do not treat
the artifact as permission to create the proposed leaf.

## Observe caller-available capacity

The observation command takes the same complete action chain and workspace.
Its output must share the request's evidence parent. Supply a stable,
non-secret canonical UUID for the physical host/filesystem authority:

```bash
python3 scripts/bulkload.py codex-private-sqlite-capacity-observe \
  [THE SAME COMPLETE ACTION-CHAIN ARGUMENTS] \
  --request /secure/evidence/sqlite-compose-request-v6.json \
  --accept-request REQUEST_SHA256 \
  --workspace-parent /secure/sqlite-compose-workspace \
  --host-authority-id 22222222-2222-4222-8222-222222222222 \
  --output /secure/evidence/sqlite-capacity-v6.json
```

The command uses descriptor-relative `fstatvfs` on the pinned workspace and
records `f_bavail * f_frsize` plus `f_favail`. It deliberately does not use
privileged `f_bfree`. Checked overflow, insufficient caller-available bytes or
inodes, target/staging occupation, path or mount drift, runtime drift, or any
input-chain drift blocks publication. It repeats the complete chain,
workspace, and sufficiency checks before and after create-only publication.

Exit `4` is expected for a successfully written observation. Review
`created_at`, `expires_at`, request/body digests, host authority, runtime,
workspace, requirement, filesystem values, and the invariant false claims.
After expiry, capacity change, workspace change, or any input/runtime change,
discard the observation and create a fresh one.

## Fail-held outputs

Publication uses an owner-private pinned parent, exact temporary bytes,
single-link custody, and an OS-backed no-replace rename. It never
pathname-deletes on failure. If a command reports drift after publication,
the named final artifact is fail-held evidence, not authority. Quarantine it
attended; do not overwrite, reinterpret, or feed it to a later slice.

Never place evidence or the workspace inside a live Codex, SQLite, or session
root. Never inspect or print raw SQLite rows or credential values. Never copy
WAL, SHM, or rollback-journal sidecars.

## Later slices

A future separately reviewed composer must bind an exact runtime, consume an
unexpired observation, create randomized same-parent staging, stream only the
v5-described union, independently verify every schema/count/digest/edge,
fsync the complete versioned bundle, and seal it create-only. A separate
publisher, installer, picker, and attended provider-resume proof remain later
authorities. V6 supplies none of them.
