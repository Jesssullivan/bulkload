# Codex private SQLite compose request and capacity observation v6

> Historical exact producer contract. Active policy v7 retains these artifacts
> only as validator-bound legacy evidence and registers neither producer
> command. The command examples below require the exact reviewed v6 checkout;
> they are not available from the active v7 CLI.

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

The request accepts only an action plan produced by the exact accepted-H6 v5
closure:

- source commit:
  `4d949a846690b265b0bf775ec81ff4b9e5a52ddc`;
- policy schema:
  `dev.tinyland.bulkload.codex-private-state-policy.v5`;
- policy SHA-256:
  `78318633ef06ca12d6dc7e72199c07dc6f5cfdf0e9cf13bc5bd3f3e7c2ddbcf0`;
- runtime closure SHA-256:
  `cc9f96adb8189e0a41133244231edf51dd837fa75459728355b93eeb981c7392`;
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

Keep every input `0600`, same-UID, single-link, and non-symlink in an exact
`0700` evidence directory outside live state and the proposed workspace. The
frozen v6 validator compares every private owner UID to the process UID, so
cross-UID validation is unsupported even when filesystem ACLs would permit the
read. Pass the complete original chain:

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
same-UID directory with exact mode `0700`. The request command does not create
it.

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

## Historical producer provenance

The request and capacity producers exist only at reviewed v6 commit
`7bd06a05f7a4710e42fac6be08b477b801493c95`. Active v7 deliberately removes
their CLI handlers, writer helpers, workspace mutator, and public constructor
functions. This document records the frozen artifact semantics for validation;
it is not an operator recipe for minting new v6 evidence.

V7 accepts an existing artifact only after structural validation and exact
relational binding to its v5 action plan, v6 runtime authority, workspace,
capacity requirement, and request digest. Self-redigested drift remains
invalid. A new producer requires a later policy version and a separately
reviewed transaction; do not recover the historical commands by copying them
from Git history into the active runtime.

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
