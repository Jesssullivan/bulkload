# Codex private SQLite composition opening plan v4

## Status and boundary

Policy v4 implements a source-only, non-actionable classification request. It
does not implement a SQLite composer, publisher, installer, combined auth and
SQLite apply, provider activation, or session copier.

Every v4 plan records these terminal claims:

- `composer_implemented=false`;
- `sqlite_union_ready=false`;
- `sqlite_compose=false`;
- `sqlite_publish=false`;
- `combined=false`;
- `ready_for_apply=false`;
- `post_plan_close_required=true`; and
- `post_plan_close_proven=false`.

The existing policy-v4 auth installer remains auth-only. It consumes no source
SQLite and preserves destination SQLite byte-exactly.

## Why four private captures

One source and one destination capture are insufficient completeness evidence.
The opening request requires:

- source private pass A and pass B;
- destination private pass A and pass B;
- four distinct capture UUIDs;
- four distinct quiescence-attestation UUIDs; and
- byte-equal stable projections within each role.

All eight private evidence UUIDs are globally distinct and disjoint from every
session opening, prefix, and closing capture UUID. The four private capture
digests are also globally distinct. Capture identity and attestation identity
never share one evidence namespace entry.

The stable projection excludes only capture time, capture/output identity, and
fresh attestation identity. It includes role, host authority, host, Codex
version, Codex and SQLite root authorities, selected classes, budgets, auth
metadata when selected, the complete SQLite-family metadata, the live
namespace digest, copy methods, and readiness flags. Family, source identity,
snapshot, schema, migration, header, thread/path, namespace, or budget drift
therefore rejects the opening.

The plan validator accepts exactly that typed projection shape at every level;
undeclared top-level, auth, family, table, identity, budget, or copy-method
fields are rejected even after self-redigest. Projected `migration_count`,
`thread_count`, and every captured counted-table row count are nonnegative and
cross-bound to the corresponding classified relation state. A nonempty
migration projection must name an integer latest migration; an empty projection
must name none.

The validator reapplies the captured budgets rather than merely retaining
them as evidence. Family count cannot exceed `max_sqlite_families`; source and
immutable-snapshot byte totals independently cannot exceed
`max_total_sqlite_bytes`; and each family's projected thread and metadata
entry/byte counts cannot exceed their captured per-family limits.

An A/B match is an observational stability barrier. The attestation remains an
operator procedural fence with `provider_writer_proof=false`.

## Session closure input

The SQLite plan binds the exact Codex session-union v3 plan and recomputes it
from all opening, prefix-proof, close-request, and closing-capture artifacts.
A self-digested plan is not accepted as authority.

The session lane currently has no apply receipt or independent session
verification schema. The SQLite plan therefore records:

- `ready_for_attended_copy=true`;
- `executed=false`; and
- `verified=false`.

It never invents a receipt or reuses the generic repository receipt. A later
session executor/receipt/verify slice is required before a complete migration
claim.

Private and session evidence IDs are normalized to UUID hex and must be
globally disjoint. Source and destination host-authority UUIDs must match
across the two state planes; hostnames and lexical path strings are not
cross-host identity.

## Accepted command

The only new command is:

```text
codex-private-sqlite-compose-plan
```

It requires:

- the v3 compatibility plan and exact accepted digest;
- source private A/B and destination private A/B bundles;
- an exact adapter registry and accepted digest;
- an exact path map and accepted digest;
- a session-union plan and exact accepted digest;
- session source A/B and destination A/B;
- every optional prefix/close artifact used to derive that plan; and
- a create-only owner-private output.

The command returns nonzero even when structural classification is complete.
Nonzero output is intentional fail-held evidence, not an apply authority.

There is no `codex-private-sqlite-compose`, publish, install, or combined
command. Those names remain forbidden by policy.

## Adapter registry

The registry schema is:

```text
dev.tinyland.bulkload.codex-private-sqlite-adapter-registry.v1
```

The plan embeds the complete canonical registry body, including its UUID,
timestamp, exact digest, registered-family keys, adapter provenance, table
rules, path authorities, edges, and collations. Structural validation
revalidates that body and binds its digest to the accepted input. Complete
against-input recomputation still proves that the embedded body is the exact
pinned external registry rather than a self-redigested replacement.

Each family is keyed by the exact tuple:

- basename, role, and generation;
- source and destination Codex versions;
- source and destination structured-schema digests;
- source and destination raw-schema digests;
- source and destination migration digests;
- application IDs and user versions; and
- migration relation.

An exact migration relation names no adapter. A registered-prefix upgrade
must name an adapter ID and exact adapter-source SHA-256. Approximate 40→42
fixtures must never be entered as Neo/Sting production truth.

Every table relation persists one merge class and its exact ordered identity
columns:

- `exact`;
- `keyed-union`;
- `append-multiset`; or
- `unsupported`.

Append/multiset and unsupported tables remain blockers until multiplicity and
surrogate-ID semantics are separately ratified. No generic `rowid` authority
is inferred.

The registry also owns every rollout-path table/column rule, exact observed
foreign-key edge (ordered columns, referenced columns, update/delete actions,
and match behavior), and accepted collation. Missing, extra, or changed edges
block. The presence of edge-registry, unregistered-collation, and
unregistered-table-set blockers is derived exactly from the embedded registry
and both persisted schema contracts. Views and triggers remain unconditional
semantic blockers in v4 even when their SQL digests match; a later reviewed
adapter must classify their read and write effects before composition. An
unknown family, table, collation, virtual/shadow table, or malformed identifier
also blocks.

## Structured schema

The classifier deterministically records:

- `application_id` and `user_version`;
- the canonical typed `sqlite_schema` record catalog and its digest;
- `pragma_table_list` type, column count, WITHOUT ROWID, and STRICT facts;
- `pragma_table_xinfo` column identity, declared type, affinity, nullability,
  default SQL digest, primary-key ordinal, hidden code, and generated kind;
- index uniqueness, origin, partial flag, terms, sort direction, collation,
  key status, and CREATE SQL digest;
- foreign-key IDs, columns, referenced tables, update/delete actions, and
  match behavior;
- trigger and view names plus exact SQL digests; and
- observed index-term collations.

Each raw catalog record has exactly `type`, `name`, `table`, and a nullable
CREATE-SQL digest, is uniquely ordered by `(type,name)`, and binds every
representable table, index, trigger, and view fact. Unsupported virtual/shadow
objects remain in the catalog while their normalized table bodies are omitted
under an exact canonical `{name,type}` omission catalog and matching explicit
blockers; this preserves their authority without falsely classifying their
semantics.

Each schema contract also persists the exact canonical producer blocker list
before source/destination role decoration. That list is covered by the
registry-bound schema-contract digest. Family validation requires each role's
blockers to equal its persisted schema blockers plus independently reconstructed
foreign-key-shape blockers. Internal `sqlite_%` objects, failed or malformed
migrations, explicit column collations, and other readiness-gating facts
therefore cannot be removed from a self-redigested plan to manufacture
`classification_complete=true`.

SQLite does not expose a complete standalone column-collation pragma.
Accordingly, exact schema SQL remains authority. Any explicit table-column
`COLLATE` clause blocks v4 classification, including a custom collation that
does not appear in index metadata. The SQL classifier ignores quoted literals,
quoted identifiers, and comments rather than treating the word `COLLATE`
inside them as authority. Unregistered observed index collations also block.

A row identity must be an exact PRIMARY KEY or UNIQUE key whose equality is
safe for cross-input classification. V4 accepts an INTEGER rowid alias, or a
STRICT-table key with non-null INTEGER, TEXT, or BLOB columns and BINARY
collation. Nullable keys, REAL/NUMERIC identity semantics, NOCASE/RTRIM/custom
collations, expression or partial UNIQUE indexes, and every secondary UNIQUE
claim remain explicit blockers. This prevents two raw-distinct rows from being
reported union-compatible when SQLite would reject them as one namespace
claim.

Any `sqlite_%` internal table is also an explicit v4 blocker. In particular,
`sqlite_sequence` carries AUTOINCREMENT high-water authority that changes
future identifier allocation; excluding it from ordinary schema/table scans
must never be mistaken for a complete composition claim.

## Migration relation

Canonical migration records are:

```text
[version, description, checksum_hex, success]
```

Versions must be unique and strictly ordered, and every record must be
successful.

The plan emits:

- source and destination counts/digests;
- source and destination latest migration versions, or null for an empty list;
- exact common-prefix count/digest; and
- source-tail digest.

Every zero count names the SHA-256 of the empty canonical list, and every
nonzero count names a nonempty-list digest. When typed migration records are
complete, source and destination migration counts equal both the classified
`_sqlx_migrations` row counts and the opening capture projections.

For `exact`, both counts, both migration digests, and the full common-prefix
digest must agree, the common-prefix count must equal both full counts, the
source-tail digest must name the empty canonical list, and no adapter may be
present. For `registered-prefix-upgrade`, the source count is strictly larger,
the destination count and digest equal the common prefix, source and
destination full-list digests differ, the tail is nonempty, and an adapter is
present. A `blocked` relation cannot reuse either internally complete shape
and must carry the matching fail-held blocker.

`registered-prefix-upgrade` is possible only when the destination list is an
exact canonical prefix of the source list and the complete exact registry key
matches. PR #5 records the adapter but does not execute it.

## Typed semantic row digests

Rows are preflighted and then streamed in the exact SQLite sort-key order used
for the cross-input merge. Values use length-prefixed binary type tags:

- NULL;
- signed 64-bit INTEGER;
- IEEE-754 REAL bits;
- exact UTF-8 TEXT bytes; and
- exact BLOB bytes.

The row hash also binds family, table, ordered column contract, identity, and
typed values. This prevents collisions such as integer `1` versus text `"1"`,
NULL versus empty text, BLOB versus TEXT, and `-0.0` versus `0.0`.

Only counts and digests enter the plan. Raw values never enter plans, blockers,
logs, or receipts.

For each classifiable table, the plan reports:

- source and destination row counts, charged semantic bytes, and semantic
  digests;
- shared equal count;
- source-only count;
- destination-only count; and
- conflicting shared count.

Duplicate identities and shared non-path divergence block.
Equal rowsets require identical complete semantic states; any source-only,
destination-only, or conflicting row requires distinct stream digests.
`sqlite-shared-row-divergence` and `sqlite-exact-table-divergence` blockers are
derived exactly from those counts, and family completeness additionally
requires every table relation to be complete. Persisted charged bytes must be
zero for an empty stream and at least the deterministic typed-envelope minimum
for every nonempty row.
Every value is size-checked before Python materializes the semantic row, and
the SQLite fetch limit independently caps one returned row. V4 accepts the
greater-than-8-MiB regression fixture but rejects any individual value above
64 MiB. One shared ledger charges aggregate rows and typed semantic bytes
across every source and destination stream, table, and family; preflight does
not double-charge it. Schema, preflight, and row-classification statements are
interrupted by an enforceable SQLite progress handler when the one plan-wide
deadline expires.

An unrecognized but safely named top-level `*.sqlite` family is retained in
the opening plan as `family_role=unsupported` with an explicit
`sqlite-family-unsupported` blocker. It cannot make the plan actionable, but
it also cannot erase the rest of the family census by making plan generation
abort.

## Rollout path authority

The path-map schema is:

```text
dev.tinyland.bulkload.codex-private-sqlite-path-map.v1
```

It binds:

- mapping UUID;
- source and destination host-authority UUIDs;
- source and destination Codex versions;
- exact session-union plan digest;
- source and destination session-catalog digests;
- canonical source and destination session roots; and
- an exact allowlist of family/table/session-ID/path columns.

The plan embeds and revalidates the complete canonical path-map body. Its exact
digest is bound to the accepted input, and its role-bound host authorities,
Codex versions, session-plan digest, catalog digests, roots, and rules are
cross-bound to the private opening, embedded session evidence, Codex-home
authority, and matching registry path authorities. Complete against-input
recomputation remains the proof that this body is the pinned external path map.

Policy v4 requires exactly:

```text
state_5.sqlite / threads / id / rollout_path / session-rollout
```

Each path must be normalized, absolute, and boundary-contained beneath the
correct session root. Its session UUID must exist in the accepted union
closure at that UUID's exact accepted relative path. The classifier translates
only that allowlisted source path to the destination root for semantic
comparison.

Equal lexical roots are valid on distinct host-authority UUIDs; each role owns
its root independently. Destination-native paths are preserved. Cross-wired
host authorities, `..`, relative paths, prefix near misses, casefold aliases,
outside-authority paths, wrong UUID-to-relative-path bindings, stale source
paths in destination state, and arbitrary TEXT search or rewrite block.

## Publication and race closure

The CLI pins every JSON input and the pre-import runtime/policy closure. It
fully recomputes the plan from all accepted JSON and bundle inputs before
publication, uses owner-private create-only no-replace publication, and
recomputes it again afterward. A deep structural validator rejects malformed
or extra projection/blocker fields, cross-level blocker omissions, missing
table relations, unsafe or secondary UNIQUE claims, raw/structured schema and
edge digest drift, cross-wired role authorities, migration/count arithmetic
contradictions, undercharged semantic states, reused capture digests, and
aggregate persisted-budget claims. Family and plan blockers use exact
code-specific typed bodies; raw values cannot be smuggled through a generic
blocker or stable-projection object. It also rejects malformed schema, relation,
edge, blocker, or readiness bodies even when an attacker recomputes enclosing
self-digests. The complete against-input recomputation is still mandatory for
every future consumer. Plans share the private publisher's 8-MiB-minus-newline
bound, so any structurally valid plan is publishable by the typed command. An
input or runtime change after publication leaves the artifact fail-held and
non-authoritative.

The v4 plan is the immutable opening request. It does not claim that pre-plan
A/B evidence closes a later live race.

A future offline-composer slice must:

1. accept the exact v4 plan digest;
2. obtain fresh source closing A/B and destination closing A/B directly from
   the live authorities under distinct attestations and capture IDs;
3. bind those captures to the opening request;
4. require each closing stable projection to equal the corresponding opening
   projection;
5. bind the same attended writer-stop epoch to the session closing evidence;
6. compose only into a new complete versioned owner-private directory; and
7. keep publication and activation false.

Stale pre-plan bundles may not be wrapped as closing evidence.

## Later slices

The intended sequence is:

1. v4 opening/classification request — this slice;
2. offline composer into a complete versioned bundle;
3. no-replace versioned-directory publisher and crash recovery;
4. independent provider acceptance across picker, resume, dialog, new thread,
   rollback, and auth; and
5. a separately attended cutover.

WAL-aware provider backup remains a separate future reader. Raw live DB/WAL/
SHM copying is never an acceptable substitute.
