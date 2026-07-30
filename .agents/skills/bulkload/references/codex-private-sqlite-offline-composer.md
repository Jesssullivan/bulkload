# Codex private SQLite offline composition contract v7

## Status and authority boundary

This document is the contract-first boundary for TIN-3268's internal offline
SQLite composer and independent verifier. It is source design, not execution
authority. A v7 source slice may pin only the strict protocol and read-only
verifier oracle described below; the exact v6 runtime remains immutable
producer authority.

This slice does not add a CLI command, writer, verification-receipt publisher,
reservation, bundle publisher, installer, picker, recovery command, activation,
provider session, or live-state operation. Its read-only oracle accepts only
synthetic or already present offline bundle-shaped directories and returns an
in-memory report. It
does not change any public readiness claim. In particular, all of these remain
false:

- `composer_implemented`;
- `ready_for_internal_offline_compose`;
- `ready_for_offline_compose`;
- `sqlite_compose`;
- `sqlite_publish`;
- `sqlite_union_ready`;
- `combined`;
- `ready_for_apply`;
- `provider_runtime_acceptance_verified`; and
- `provider_writer_proof`.

The implementation must land in three independently reviewable source
boundaries:

1. this immutable contract, strict protocol schemas, and a read-only verifier
   oracle exercised against independently hand-built bundles;
2. an internal writer with no public command and no independent-verification
   claim; and
3. integration that runs the already reviewed verifier and publishes its
   separate create-only receipt.

Production command enablement is a later fourth boundary. The verifier lands
before the writer so that writer behavior cannot become its unstated oracle. A
green writer test does not collapse these boundaries.

## Immutable authority lineage

A future writer may consume only this exact chain:

1. the exact repaired v5 action-plan producer:
   - source commit
     `3daf764660f45c0d1e71383f1269237a0cfe991b`;
   - policy SHA-256
     `13fa05eecb0eefb2f697735a4ad7351834695e26a7dd0a910be2e87b035ca82d`;
   - runtime closure SHA-256
     `8193d692be87174c6618450fa84cd44a0755661f2cf50cab791e50725dabcad7`;
2. one exact v5 action plan whose complete v4 opening and v5 close inputs
   recompute from their original evidence;
3. the exact v6 request/observation producer:
   - source commit
     `96db5c46eda9774c3bf6983eb04281cb249675fb`;
   - policy SHA-256
     `b0fb835934a1caac48fb72190f072b5ec35737c52ce42c7bf6a30088d5bf8548`;
   - runtime closure SHA-256
     `b221bb77ebd23e58ba2ea0788b35bfe52f7f1c6073b9e5e6fd2a4498daeb3650`;
4. one exact v6 compose request and one unexpired, sufficient v6 capacity
   observation bound to that request, host authority, runtime, and workspace;
   and
5. a distinct future runtime authority whose exact source inventory contains
   the internal writer.

The v6 request deliberately records
`required_composer_runtime_authority=null`. A later runtime must not reinterpret
that null as implicit permission. The future internal entrypoint must bind the
exact v6 artifacts to the exact reviewed writer runtime in a single-use,
process-local writer ticket and in the composition receipt.

The verifier oracle lands first with its own exact source digest. A later
writer may retain that frozen verifier source, but it must have a different
writer source digest and may not import the verifier. Verification receipts
bind the exact verifier source separately from the containing runtime closure.
The later final verifier independently binds the same immutable v5 and v6
inputs plus the final bundle. The first oracle validates only its supplied
v5/v6 artifacts and explicitly cannot claim the complete original chain.
Neither side may substitute a self-redigested historical policy or producer.

## Opaque writer ticket

No serializable artifact may claim compose authorization. Before the first
mutation, a future internal entrypoint must:

- hold the exact writer runtime and every v4-to-v6 input open;
- deeply recompute the complete v5 action plan and v6 request;
- validate the exact v6 observation;
- bind the operator-supplied expected host-authority UUID;
- bind the request's workspace descriptor, mount, lineage, protected
  namespaces, staging prefix, and final leaf;
- bind one cryptographically random attempt UUID and ticket nonce;
- acquire a nonblocking cooperating-process `flock` on the pinned workspace;
  and
- verify that the entire matching staging namespace and final leaf are absent.

It then creates and pins exactly one cryptographically randomized `0700`
staging sibling with descriptor-relative `mkdirat`. Only after that atomic
claim does it issue an opaque, single-use process-local ticket. The ticket
carries pinned descriptors, complete-body digests, writer runtime authority,
host authority, clock/deadline readings, staging identity, final leaf, and a
nonce. It has no JSON encoder, pathname-only constructor, copy/deepcopy
support, or reusable success state.

The receipt records only the ticket nonce's SHA-256 and the exact authorities;
it never serializes the ticket. The ticket is consumed before the first SQLite
writer opens. A changed descriptor, input, runtime, clock, host, capacity, or
workspace invalidates it.

## Time, host, and replay closure

The mutating writer must use a fresh wall-clock reading and a monotonic
deadline. It must reject:

- a wall clock before `created_at` beyond the fixed skew allowance;
- a capacity observation at or past its expiry;
- `claims.capacity_sufficient_observed` other than true;
- a host-authority UUID differing from the capacity observation;
- a request, observation, workspace, action, or runtime body that differs;
- an occupied staging namespace or final leaf; and
- a replay already represented by any fail-held staging, final bundle, or
  verifier evidence.

The nonblocking `flock` excludes cooperating writers. Randomized
descriptor-relative `mkdirat` is the durable claim. Immediately after creation,
the writer must prove that the namespace contains exactly its one owned staging
directory and the final leaf remains absent. Any concurrent or attacker-owned
entry blocks. A failed staging attempt remains visibly consumed until a
descriptor-bound recovery/quarantine workflow classifies it. That recovery
state machine, durable attempt identity, and idempotent retry contract must be
implemented and reviewed before any writer slice is allowed to mutate a
workspace.

## Capacity is observation, not reservation

The writer must not upgrade the v6 observation into a reservation claim. It
reopens the pinned workspace and obtains caller-available
`f_bavail * (f_frsize || f_bsize)` and `f_favail`:

1. before staging;
2. after staging and before the writer ticket;
3. before each family baseline;
4. before each family transaction;
5. before manifest and receipt creation; and
6. after the complete pre-seal revalidation and immediately before metadata
   durability.

Each observation uses checked 63-bit arithmetic and repeats the complete v6
`required_bytes` and `required_inodes` as its conservative remaining
requirement. The requirement never decreases based on claimed progress. Every
observation must independently satisfy that full requirement. The writer also
enforces SQLite page, length, row, byte, inode, and elapsed-time limits.
`SQLITE_FULL`, `ENOSPC`, `EDQUOT`, overflow, timeout, a zeroed or reduced
requirement, or a short observation fails the attempt and retains any staging
state as fail-held.

Repeated observation narrows the race but does not prove reservation, quota,
or future capacity. A production reservation or quota authority remains a
separate later design.

## Internal writer state machine

The writer has no CLI registration. Its internal API accepts typed objects and
pinned descriptors, never repository- or database-supplied executable code.
It performs these transitions in order:

1. `PRE_MUTATION_VALIDATED`
   - pin the exact writer runtime before import;
   - deeply recompute the complete v4-to-v6 chain;
   - validate time, host, capacity, workspace, protected roots, absence, and
     acquire the nonblocking cooperating lock;
2. `STAGING_CLAIMED`
   - create randomized same-parent `0700` staging;
   - prove it is the only matching staging entry and issue the opaque ticket;
3. `STAGING_DURABLE`
   - create the `sqlite/` directory;
   - bind their device, inode, owner, mode, mount, and lineage;
   - fsync both new directory entries;
4. `BASELINES_COMPLETE`
   - process families in canonical basename order;
   - create each output family as a new `0600` file;
   - use SQLite's backup API to copy the immutable destination snapshot;
5. `UNIONS_COMPLETE`
   - for each family, insert source-only rows in one bounded transaction;
   - preserve destination-only and shared-equal rows exactly once;
   - perform only the allowlisted source rollout-path translation;
6. `WRITER_SELF_CHECK_COMPLETE`
   - rederive schema, header, migration, table, edge, count, and semantic
     digest facts;
   - run full `integrity_check` and `foreign_key_check`;
   - reject every unexpected file or SQLite sidecar;
7. `PRESEAL_REVALIDATED`
   - recompute the complete input chain again;
   - revalidate the writer runtime, ticket, workspace, protected roots, clock,
     capacity, every staged inode, exclusive staging namespace, and final-leaf
     absence;
   - record the final full-requirement capacity observation before creating
     either metadata document;
8. `BUNDLE_METADATA_DURABLE`
   - create, write, and fsync the canonical manifest;
   - create, write, and fsync the canonical pre-seal composition receipt whose
     evidence includes the already completed pre-seal transition;
   - fsync every family, `sqlite/`, and staging directory;
   - revalidate both metadata descriptors and the exact staged tree, after
     which no staged entry may change;
9. `SEALED`
   - use an OS-backed same-directory no-replace rename from staging to the
     action-derived final leaf; and
   - fsync the workspace directory.

The no-replace rename is the only seal commit point. Unsupported no-replace
semantics, `EXDEV`, NFS ambiguity, parent drift, or an occupied target block
the operation. No ordinary replacing rename, copy-then-delete fallback, or
success-by-path-observation is permitted.

The receipt is serialized during transition 8, so its stored transition
history ends at `PRESEAL_REVALIDATED`. It never records
`BUNDLE_METADATA_DURABLE` or `SEALED`: both describe facts established only
after the receipt body already exists.

No file inside the sealed bundle is modified after the rename. The writer may
return an in-memory outcome, but it does not publish a post-seal receipt or an
independent-verification claim.

## SQLite family algorithm

The writer starts from the immutable destination snapshot because destination
is the preservation baseline. It never starts from a blank schema and never
mutates either input bundle.

For each exact-schema, exact-migration family:

1. validate source and destination snapshot digests, sizes, headers, schemas,
   migrations, application IDs, and user versions against the action plan;
2. backup the destination snapshot into the new staged family;
3. require `journal_mode=DELETE`, `synchronous=FULL`, foreign keys enabled,
   extension loading disabled, and no attached database;
4. start one bounded write transaction for the family;
5. stream source and destination identities in the action plan's canonical
   typed order;
6. insert only source-only complete typed rows through parameterized SQL;
7. rewrite only a registered source rollout-path column whose session UUID and
   relative path bind the accepted session union;
8. defer foreign-key enforcement through the complete family transaction so
   registered cycles can close, then require commit-time success;
9. reject shared divergence, missing or extra identities, duplicate
   identities, secondary-UNIQUE conflicts, triggers, views, virtual/shadow
   objects, unknown tables, unsafe collations, schema changes, attachments,
   or extension loading;
10. commit, run full integrity and foreign-key checks, normalize and close the
    family, reject sidecars, chmod `0600`, and fsync the database and directory.

All identifiers come from the deeply validated registry and schema contract
and are quoted by one fixed implementation. Values are always parameters.
Neither a registry nor a database may name SQL text, Python, a module,
callable, extension, pragma, or executable adapter. Registered-prefix
migrations remain blocked until a separately reviewed in-closure adapter
slice.

## Sealed bundle contract

The sealed final directory contains exactly:

```text
sqlite/
  <canonical-family-basename>.sqlite
manifest.json
composition-receipt.json
```

Directories are `0700`; regular files are `0600`; every entry is current-user,
single-link, non-symlink, on the bound mount, and reachable beneath the pinned
final directory. No WAL, SHM, journal, temporary, lock, cache, or
unlisted entry is allowed inside the bundle.

The canonical manifest schema is
`dev.tinyland.bulkload.codex-private-sqlite-composed-bundle-manifest.v7`.
Its exact top-level keys are:

- `schema`;
- `created_at`;
- `composition_id`;
- `producer_lineage`;
- `sqlite_engine_authority`;
- `accepted_inputs`;
- `workspace`;
- `families`;
- `payload_inventory`;
- `claims`;
- `implementation`; and
- `manifest_sha256`.

`producer_lineage` binds the exact v5 action-plan, v6 request/observation, and
current writer runtime authorities plus the writer source digest.
`accepted_inputs` binds each action plan, compose request, and capacity
observation by schema, accepted self-digest, and complete canonical-file
digest. `workspace` binds the parent/mount/lineage, randomized staging leaf and
identity, and intended final leaf.

Every family record binds its basename, relative path, mode, size, physical
SHA-256, journal mode, application/user versions, raw and structured schema,
migrations, edges, table facts, and absent sidecars. `payload_inventory`
contains only the SQLite payloads as canonical
`{relative_path,type,mode,size,sha256}` entries plus their aggregate digest, so
the manifest never self-references.

The manifest may positively claim only that the destination baseline was
streamed, source-only rows were inserted, the payload is complete, and the
writer's self-check passed. It keeps existing-destination mutation, identity
remap, deduplication, deletion, final-leaf commit observation, independent
verification, reservation, publication, installation, SQLite-union readiness,
provider acceptance, and provider-writer proof false.

The canonical pre-seal receipt schema is
`dev.tinyland.bulkload.codex-private-sqlite-composition-receipt.v7`. Its exact
top-level keys are:

- `schema`;
- `completed_at`;
- `composition_id`;
- `producer_lineage`;
- `sqlite_engine_authority`;
- `accepted_inputs`;
- `capacity_admission`;
- `workspace_claim`;
- `operation_graph`;
- `manifest`;
- `claims`;
- `implementation`; and
- `receipt_sha256`.

It binds the manifest schema/self-digest/complete-file digest, exact v5
operation-graph digest and the sorted graph prefix completed before receipt
serialization, writer runtime, capacity admission, lock and staging claim,
ticket nonce SHA-256, state transitions, self-checks, intended final leaf, and
failure policy. That completed prefix ends at `write-manifest`;
`write-receipt`, `fsync-bundle`, and `seal-bundle` are necessarily excluded.

Because the receipt is written before the commit-point rename, it may claim
only complete against-input recomputation, capacity admission, ticket
consumption, composition completion, and writer self-verification. It must
record `seal_preconditions_complete=false` because receipt persistence, full
bundle fsync, and final staged-tree revalidation are still pending. It must
also record `final_leaf_commit_observed=false`, `independently_verified=false`,
`sqlite_compose=false`, `sqlite_publish=false`, and
`provider_runtime_acceptance_verified=false`. Renaming the containing
directory does not retroactively make a stored false claim true.

The manifest does not digest the receipt if doing so would create a digest
cycle. The receipt binds the manifest; the later verifier binds both complete
bodies and the sealed directory identity.

Stored chronology is exact and non-circular. Capacity observations are
monotonic in their canonical phase order, the final `pre-seal` observation is
not later than `manifest.created_at`, the manifest is not later than
`receipt.completed_at`, and the receipt is not later than the oracle's fresh
`observed_at`. The receipt therefore never claims a transition or observation
that occurs after the receipt was made durable.

## Independent verifier boundary

The final verifier is read-only and lands from a source inventory distinct
from the writer. Build and source-contract tests must prove that the final
verifier:

- does not import the writer module;
- does not reuse the writer's row streamer, typed-row encoder, manifest
  compiler, receipt compiler, self-check result, or state-transition result;
- opens the sealed final directory and every child no-follow under pinned
  descriptors;
- independently recomputes the complete v4-to-v6 input chain;
- independently enumerates and validates the exact bundle tree;
- independently scans every output schema, migration, header, edge, identity,
  typed row, count, and semantic digest;
- runs its own `integrity_check` and `foreign_key_check`;
- compares the observed output directly with the action plan and immutable
  source/destination inputs; and
- treats manifest and writer receipt fields as claims to test, not facts.

It may share only reviewed canonical schema constants, generic strict-JSON
parsing, bounded hashing primitives, and immutable producer validators. Any
shared semantic row implementation defeats the independence claim.

The first v7 slice does not implement that final verifier. Its internal
read-only oracle has no writer, publisher, receipt publisher, public CLI, or
readiness surface; its callable is an internal library function only. It
accepts hand-built test bundles and already present offline
bundles, validates the supplied v5/v6 artifacts structurally, and independently
observes bundle custody, tree, SQLite integrity, foreign keys, schema,
migrations, edges, and semantic output against the supplied action. It does not
receive every original v4/v5 input and therefore cannot claim complete
against-input recomputation, a witnessed writer commit, a sealed bundle, final
independent verification, or offline readiness.
Its public wrapper accepts neither an observation identifier nor an observation
timestamp from the caller; both are generated from the active process at the
start of the pinned observation. Deterministic injection is confined to the
private fixture worker, whose only active-runtime caller is that wrapper.

The oracle's schema is
`dev.tinyland.bulkload.codex-private-sqlite-verifier-oracle-report.v7`. It is a
strict in-memory diagnostic report, not an independent-verification receipt.
Its exact top-level keys are:

- `schema`;
- `observed_at`;
- `observation_id`;
- `verifier_runtime_authority`;
- `accepted_artifacts`;
- `bundle_observation`;
- `manifest_observation`;
- `composition_receipt_observation`;
- `families`;
- `failures`;
- `observed_checks`;
- `claims`;
- `implementation`; and
- `oracle_report_sha256`.

Each family contains exactly `observed`,
`manifest_matches_observed`, `action_plan_matches_observed`,
`integrity_check_observed`, `foreign_key_check_observed`,
`schema_matches_action_observed`, `migration_matches_action_observed`,
`edge_matches_action_observed`, and
`semantic_output_matches_action_observed`. Observed checks cover artifact
bindings, descriptor custody, exact tree inventory, strict manifest/receipt
structure, claimed-versus-observed SQLite engine equality, integrity, foreign
keys, schema, migrations, edges, and semantic rows.

Schema hashes have one canonical derivation. `raw_schema_sha256` hashes the
canonical ordered raw-schema record array.
`schema_contract_sha256` hashes the complete schema contract before that field
is inserted. `structured_schema_sha256` hashes the resulting complete schema
contract including `schema_contract_sha256`. Each table's `schema_sha256`
hashes its complete table contract, and `foreign_keys_sha256` hashes exactly
that table contract's ordered `foreign_keys` array. Migration and observed-edge
hashes cover their complete canonical arrays. Implementations and fixtures may
not substitute an abbreviated object with a matching-looking digest.

The oracle retains the 64 MiB per-value semantic bound and caps each
materialized SQLite result record at that bound plus 1 MiB of record metadata
allowance. It issues no SQLite `ORDER BY`: every schema collection is charged
before in-process canonical sorting, and semantic ordering retains at most
100,000 rows and 16 MiB of identity/sort payload before sorting. This prevents
SQLite from creating a transient B-tree or spilling a supposedly read-only
observation into an out-of-tree temporary file. The metadata allowance is not
a larger per-value authority; every value is independently rejected above
64 MiB, and the historical aggregate row/byte bounds still apply. Text
identity ordering reads `PRAGMA encoding` and reproduces the frozen producer's
database-encoding `CAST(TEXT AS BLOB)` bytes in process.

The 600-second progress-handler and monotonic checks are cooperative SQL and
Python deadlines. They cannot preempt blocked filesystem I/O and are not a
hard wall-clock authority. The internal-only oracle therefore excludes
unresponsive or adversarial filesystems; a later live-facing integration must
place it behind a parent-enforced process timeout with bounded cleanup before
claiming an enforceable deadline.

Even with no oracle failures, every final or public authority claim remains
false, including `final_leaf_commit_observed`, `bundle_sealed`,
`full_against_inputs_recomputed`, `independent_verification_complete`,
`offline_bundle_verified`, publication, installation, session execution,
SQLite-union readiness, provider acceptance, combination, and apply authority.

The later final-verifier integration publishes one create-only owner-private
receipt outside the sealed bundle and outside all live roots. Its schema is
`dev.tinyland.bulkload.codex-private-sqlite-independent-verification-receipt.v7`.
Its exact top-level keys are:

- `schema`;
- `verified_at`;
- `verification_id`;
- `verifier_runtime_authority`;
- `sqlite_engine_authority`;
- `accepted_inputs`;
- `bundle`;
- `manifest`;
- `composition_receipt`;
- `families`;
- `failures`;
- `claims`;
- `implementation`; and
- `verification_sha256`.

Only that later final verifier, after it has recomputed the complete original
input chain and produced a zero-failure receipt, may first claim
`bundle_sealed=true` and `offline_verified=true`. Even then,
`sqlite_compose`, `sqlite_publish`, `sqlite_union_ready`, install, activation,
session execution, and provider acceptance remain false until their later
policy slices.

## Failure and crash semantics

No writer error triggers pathname cleanup. The exact outcomes are:

- before staging: no workspace mutation is promised;
- after staging and before rename: staging is fail-held;
- rename returned success but later parent fsync or revalidation failed: the
  final leaf is fail-held and must not be treated as sealed authority;
- verifier failure: final leaf, manifest, writer receipt, and verifier failure
  evidence are fail-held; and
- verifier success: the bundle is offline evidence only.

The writer and recovery state machine must share one durable, fsynced attempt
identity and an explicit transition record. Recovery classifies staging,
final, manifest, receipt, directory-sync, and verifier states before it can
offer quarantine or bounded forward completion. Repeating recovery against the
same observed state must produce the same decision and may not invert a prior
committed result. Until that reviewed recovery implementation exists, writer
mutation is forbidden and the only permitted response to a fixture or foreign
artifact is attended inventory and preservation. Never auto-delete, overwrite,
reinterpret, install, or activate a fail-held artifact.

## Required regression matrix

The writer source slice must fail closed under tests for:

- every input, policy, runtime, request, observation, host, clock, workspace,
  mount, lineage, ticket, staging, and final-leaf drift;
- cooperating-lock and concurrent randomized-staging races;
- replay after staging, final, or verification evidence exists;
- expired, future-dated, insufficient, overflowed, or cross-host capacity;
- capacity loss before every allocation boundary and any zeroed or reduced
  repeated conservative requirement;
- nonmonotonic phase time, a manifest before final capacity, a manifest after
  receipt completion, or an oracle observation before receipt completion;
- `SQLITE_FULL`, `ENOSPC`, `EDQUOT`, interruption, and timeout;
- failures before and after every file fsync, directory fsync, and rename;
- deterministic repeated recovery around every file fsync, directory fsync,
  no-replace rename, and uncertain-return boundary;
- values larger than 8 MiB, the exact 64 MiB boundary, and an over-boundary
  value;
- aggregate streaming beyond 5 GiB without whole-database or whole-table
  materialization;
- NULL, INTEGER extremes, REAL values outside identities, UTF-8 TEXT, empty and
  large BLOBs, and type-changing identity attacks;
- generated columns, composite identities, foreign-key cycles, and deferred
  commit failure;
- duplicate identity, shared divergence, unsafe collation, unknown object,
  trigger/view, virtual/shadow table, internal `sqlite_%`, and secondary
  UNIQUE attacks;
- path traversal, casefold/prefix near miss, wrong UUID, stale Neo root, and
  unregistered TEXT rewrite;
- symlink, hardlink, parent swap, descriptor/path alias, cross-device, and
  no-replace-unavailable attacks; and
- injected, omitted, extra, malformed, or self-redigested manifest/receipt
  fields.

The verifier source slice repeats the semantic corpus with independent fixture
construction and must additionally reject a deliberately wrong bundle that
the writer self-check is mocked to accept. Forward review must try to produce a
P0/P1 false positive without importing writer code.

## Research basis

- SQLite Online Backup API: the destination stays in one write transaction
  during backup,
  <https://sqlite.org/c3ref/backup_finish.html>.
- SQLite atomic commit and rollback-journal durability assumptions,
  <https://www.sqlite.org/atomiccommit.html>.
- SQLite `integrity_check` and the separate `foreign_key_check`,
  <https://www.sqlite.org/pragma.html>.
- SQLite authorizer and connection limits,
  <https://www.sqlite.org/c3ref/set_authorizer.html> and
  <https://www.sqlite.org/c3ref/limit.html>.
- Python `sqlite3` backup, authorizer, progress-handler, and runtime-limit
  interfaces,
  <https://docs.python.org/3/library/sqlite3.html>.
- Descriptor-relative atomic `RENAME_NOREPLACE` semantics and their filesystem
  limitations,
  <https://man7.org/linux/man-pages/man2/renameat2.2.html>.

## Later slices

1. **V7 protocol and read-only oracle:** this contract, strict schemas, exact
   v6 freeze, and independent verification of hand-built bundles; no writer,
   CLI, or receipt publication.
2. **Recovery and attempt-state runtime:** descriptor-bound classification,
   durable/fsynced attempt identity, deterministic repeated outcomes, and
   quarantine/forward rules over fixture states; no writer mutation or CLI.
3. **Internal writer runtime:** enabled only with the reviewed recovery state
   machine; no CLI and no sealed/verified positive claim.
4. **Independent receipt integration:** run the already-reviewed verifier and
   publish its separate create-only receipt.
5. **Production adapters and command enablement:** exact observed families,
   explicit digest acceptance, and only then a narrowly scoped compose claim.
6. **Publisher:** monotonic version/catalog publication; activation remains
   separate.
7. **Session execution proof:** execute the accepted session union and verify
   every SQLite-to-rollout binding before `sqlite_union_ready`.
8. **Attended provider acceptance and cutover:** picker, historical/current/new
   resume-dialog proofs, auth, rollback, and an independently authorized live
   transition.
