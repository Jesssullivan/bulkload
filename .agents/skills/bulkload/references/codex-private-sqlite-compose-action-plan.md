# Codex private SQLite close and action plan v5

## Contents

1. [Status and non-authority](#status-and-non-authority)
2. [Why v4 cannot be executed](#why-v4-cannot-be-executed)
3. [Writer-stop close request](#writer-stop-close-request)
4. [Fresh cross-plane closing evidence](#fresh-cross-plane-closing-evidence)
5. [Pinned migration-adapter proof](#pinned-migration-adapter-proof)
6. [Expected union contract](#expected-union-contract)
7. [Action-plan contract](#action-plan-contract)
8. [Race and custody closure](#race-and-custody-closure)
9. [Failure conditions](#failure-conditions)
10. [Later slices](#later-slices)

## Status and non-authority

Policy v5 closes a reviewed v4 SQLite opening request and may emit a
descriptive, non-actionable **offline composition plan**. It still exposes no
composer, publisher, installer, combined private-state apply, provider
activation, or session copier.

The following claims remain false:

- `composer_implemented`;
- `sqlite_compose`;
- `sqlite_publish`;
- `combined`;
- `sqlite_union_ready`;
- `ready_for_apply`; and
- `provider_runtime_acceptance_verified`.

An action plan authorizes no live mutation. It is an exact input to the later
internal offline-composer library. The policy continues to forbid
`codex-private-sqlite-compose`.

## Why v4 cannot be executed

The v4 artifact is an immutable opening and classification request. It records
that post-plan close evidence is absent, and it deliberately leaves
registered-prefix schema skew blocked by
`sqlite-schema-adapter-execution-not-implemented`.

V4 also lacks:

- a writer-stop epoch shared by the private and session planes;
- post-opening private and session A/B evidence;
- post-adapter schema, migration, and header evidence;
- post-adapter row classification;
- the complete expected output row contract;
- a deterministic descriptive operation graph;
- output path, space, and byte bounds;
- a composition receipt or independent verifier contract; and
- a complete versioned-bundle manifest.

No v4 digest may be reinterpreted as an action plan. Every v5 consumer must
recompute v4 from all of its original private, session, registry, path-map, and
runtime inputs.

Accordingly, the action-plan command requires that complete original v4 input
set. It reruns the full opening validation before compilation and again after
create-only publication; the close request's
`opening_inputs_revalidated=true` field is never accepted as proof by itself.

## Writer-stop close request

The close request is immutable and contains no private values. It binds:

- the exact accepted v4 plan and its recomputed input digests;
- the exact accepted session-union v3 plan;
- source and destination host-authority UUIDs;
- both private opening stable projections and their digests;
- both session opening custody/catalog projections;
- one fresh canonical writer-stop epoch UUID and UTC timestamp;
- `provider_writer_proof=false`; and
- the exact required closing roles and pass count.

The operator acknowledgement means only that provider writers were
procedurally stopped before the epoch was declared. Bulkload cannot prove that
Codex or another provider honors this fence. The request therefore records an
operator claim, not a distributed lock or provider lease.

The close request is create-only, owner-private, self-digested, and accepted by
exact digest. It names no output database and contains no operation.

## Fresh cross-plane closing evidence

Both planes require source A/B and destination A/B after the close request.
Every pass must use a distinct capture ID. Private passes also require distinct
quiescence-attestation IDs. All private and session IDs are globally disjoint.

### Private close

Each private close pass:

1. uses a fresh short-lived quiescence attestation;
2. binds that attestation to the exact close-request digest and writer-stop
   epoch;
3. captures the live Codex and effective SQLite roots directly;
4. refuses every WAL, SHM, or rollback-journal sidecar;
5. publishes a new create-only owner-private bundle; and
6. embeds the close request and epoch binding in its capture authority.

The close command accepts no prior bundle to wrap. A copied or renamed v4 input
therefore cannot masquerade as post-plan evidence.

For each role, closing A and B stable projections must be byte-equal. Each must
also equal that role's v4 opening stable projection. Capture budgets, roots,
lineage, namespace, family inventory, family snapshots, schema, migration,
header, count, and typed metadata must remain exact.

### Session close

Each session close pass scans the live session root directly and emits a
wrapper around that newly captured snapshot. It accepts no pre-existing
snapshot. The wrapper binds:

- close-request digest;
- writer-stop epoch UUID;
- role;
- fresh snapshot and capture digest; and
- the exact opening role custody/catalog projection.

For each role, closing A and B catalogs and custody must be byte-equal and must
equal the accepted session-union opening. A session created, appended,
compacted, moved, or removed after the opening therefore invalidates the close.

The same epoch must appear in every private and session close artifact.
Provider-writer proof remains false everywhere.

## Pinned migration-adapter proof

Migration adapters are code, not data. A registry may select only an adapter
already present in the pinned Bulkload runtime closure. The registry binds its
stable adapter ID and exact source digest; no SQL, Python, shell, module path,
callable name, or other executable payload may be loaded from a registry or
database.

For an exact migration relation, no adapter runs.

V5 blocks every registered-prefix upgrade with
`pinned-migration-adapter-not-implemented`. A future adapter slice must do all
of the following:

1. create an owner-private disposable copy of the immutable destination
   snapshot outside all live roots;
2. verify the copy against the accepted destination snapshot digest;
3. invoke the exact in-closure adapter selected by the complete registry key;
4. run the adapter inside one bounded SQLite transaction;
5. refuse attachment, extension loading, unsafe pragmas, or a path outside the
   disposable family;
6. run `integrity_check` and `foreign_key_check`;
7. recapture raw schema, structured schema, migrations, headers, and tables;
8. require exact equality with the source target contract; and
9. destroy only the disposable planner-owned scratch artifact after its
   evidence has been sealed.

Any future adapter error, partial transaction, schema mismatch, migration
mismatch,
header mismatch, unexpected family/table/object, or timeout blocks the whole
plan. Production adapters enter only through a separately reviewed source
slice. Approximate fixtures never become production registry truth.

## Expected union contract

After adapter proof, the planner reclassifies every table from the original
source snapshot and the adapted destination snapshot. It does not trust v4 row
counts or classifications as output authority.

For a keyed union with zero conflicts:

```text
output = destination baseline
       + source-only identities
       + shared identities whose complete typed rows are equal
```

Destination-only rows are preserved. Shared-equal rows appear once.
Source-only rows are inserted. Shared-divergent rows block. No ID remap,
surrogate-key invention, deduplication, last-writer-wins rule, or delete exists.

For every table, the plan persists:

- exact ordered column and identity contracts;
- source, adapted-destination, shared-equal, source-only, destination-only, and
  output counts;
- typed semantic digests for every input partition;
- the expected complete output digest;
- charged semantic bytes;
- rollout-path rewrites limited to the registered allowlisted column; and
- every required foreign-key/application edge.

The planner computes the expected output digest by streaming the same canonical
typed rows and identity order required of the later composer. Counts alone are
not an output proof.

Destination-native rollout paths remain unchanged. Only allowlisted source
rollout paths are translated to the destination session root, and every
translated UUID/path must bind the accepted session union.

## Action-plan contract

A complete v5 descriptive action plan binds:

- v4 plus the close request's proof that all original v4 inputs were
  recomputed;
- the close request;
- eight fresh cross-plane close captures;
- pinned policy and runtime closure;
- adapter registry and path map;
- exact family/table/edge closure;
- expected output and shared/source-only/destination-only partition counts and
  digests; and
- a deterministic create-only operation graph;

It deliberately does not bind a concrete compose output, scratch directory,
free-space proof, publication parent, or manifest/receipt schema. Therefore
`descriptive_action_complete` may become true, but
`ready_for_offline_compose=false` remains invariant in v5. A later internal
composer request must add those authorities without changing this plan's
semantic union.

The operation graph is descriptive. It may name only these future internal
steps:

1. create a new versioned owner-private staging directory;
2. stream the destination family baseline;
3. reject any family that would require a migration adapter;
4. insert source-only rows in canonical identity order;
5. rewrite only registered source rollout paths;
6. verify every table, schema, migration, header, edge, and expected digest;
7. write the canonical manifest and composition receipt;
8. fsync files and directories; and
9. seal the complete versioned bundle with a no-replace rename.

The action plan contains no raw SQLite values. It remains non-applicable until
the internal composer and independent verifier exist and accept this exact
plan digest.

## Race and custody closure

Every JSON input is opened no-follow, owner-only, single-link, bounded, and
pinned through plan publication. Every bundle manifest, SQLite snapshot, and
session close artifact is revalidated before and after publication.

A future composer request must prove that its scratch and proposed output are
outside:

- every live Codex, SQLite, and session root;
- every opening or closing bundle;
- the runtime and policy roots; and
- each other.

That later proposed output must be create-only beneath an exact `0700` parent;
future files are `0600`. Symlinks, hardlinks, cross-device publication, parent
swaps, an existing target, and insufficient free space remain future
composition blockers, not claims made by this descriptive plan.

The action plan is accepted only after complete against-input recomputation
before and after publication. Editing and re-digesting a persisted opening,
close, or action artifact cannot create authority.

## Failure conditions

The complete plan remains blocked by any:

- opening or closing drift;
- reused capture, attestation, epoch, or request identity;
- live SQLite sidecar;
- unknown family, table, view, trigger, virtual/shadow object, or collation;
- internal `sqlite_%` state not explicitly modeled;
- failed, malformed, non-prefix, or unregistered migration;
- adapter absence, source-digest mismatch, failure, or partial transaction;
- raw/structured schema, migration, header, or table mismatch after adaptation;
- nullable, REAL/NUMERIC, unsafe-collation, expression, partial, or secondary
  UNIQUE identity authority;
- unsupported append-multiset semantics;
- duplicate identity or shared-row divergence;
- missing, extra, or changed foreign-key/application edge;
- stale, outside-root, case-folded, traversal, or wrong-UUID rollout path;
- session union that is not closed and recomputed from its original inputs;
- aggregate row, byte, value, time, or output-size budget exceedance;
- `SQLITE_FULL`, `ENOSPC`, interruption, crash-injection, or integrity failure;
  or
- input, parent, runtime, registry, or path-map drift.

Regressions must cover values greater than 8 MiB, the 64 MiB value boundary,
more-than-5-GiB streaming/sparse behavior, typed values, failure at every
migration/table/manifest/fsync boundary, parent and path attacks, migration
rollback, generated columns, foreign-key cycles, and independent full
against-input recomputation.

## Later slices

1. **V5 close/action plan:** this reference; no composer command.
2. **Internal offline composer:** stream into a complete sealed bundle and
   independently verify it; no public command or readiness flip.
3. **Production adapters and command enablement:** review every observed
   family/table adapter, then expose digest-accepted offline composition and
   set only `sqlite_compose=true`.
4. **Publisher and recovery:** no-replace same-filesystem publication with
   crash recovery; keep activation separate.
5. **Session execution and independent verification:** produce a bound session
   receipt before `sqlite_union_ready` may become true.
6. **Attended provider acceptance and cutover:** picker, resume, dialog, new
   thread, authentication, rollback, and an independently authorized live
   transition.
