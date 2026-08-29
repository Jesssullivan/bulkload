# Bulkload AgentCaptureV4 design

Status: implemented cutover product, 2026-08-22.

## Decision

Bulkload performs a reviewed, one-way union of a `~/git` fleet and typed agent
state. Its public surface is exactly:

```text
doctor (read-only, per role)
preliminary capture/plan -> agent-stage preseed
fresh final capture/plan -> agent-stage final
              -> agent-apply -> agent-verify
                              -> agent-rollback | agent-recover
```

`doctor` is the read-only preflight for both roles: it reports the contract
and cross-kernel defects that are discoverable before capture reads a byte,
writes its report, and exits non-zero when a check fails. It writes nothing
inside a declared root and signals no process on either host; the only bytes
it writes anywhere are its own report and, under
`BULKLOAD_BOUND_NAMESPACE`, the run files `_bounded_sorted` spills to
`BULKLOAD_SPILL_DIR`. Every peer probe is a simple read-only command.

Every check carries an explicit status, so an absent verdict is never mistaken
for a clean one: `pass`, `warn`, `fail`, plus `partial` when the walk stopped
at `--max-entries` before the check could see the whole namespace and
`skipped` when the check needed a peer that did not answer. Only `fail` sets
`ok` to false, and a truncated walk fails `scan-complete` rather than
reporting a partial namespace as green.

Capture, plan, stage, and verify do not mutate live destination paths. Apply is
the only forward live mutation. Every operation is bound to an exact plan
digest, a sealed final stage, an exact-overwrite capacity observation, a
complete reflinked rollback snapshot, a durable journal, and an independently
generated receipt.

Each stage phase uses destination `prepare`, source-to-destination `push`, then
destination `materialize`; these are modes of `agent-stage`, not extra public
commands. Preliminary writers resume during preseed. Fresh final A/B captures
and their new plan digest are the only apply authority.

The former repository v1, Codex session v3, and private policy v4-v7
planner/oracle chain are superseded and removed. There is one implementation,
one CLI, and one Bazel test family.

## Schemas

- `dev.tinyland.bulkload.doctor-report.v1`
- `dev.tinyland.bulkload.agent-capture.v4`
- `dev.tinyland.bulkload.git-workspace.v2`
- `dev.tinyland.bulkload.agent-plan.v4`
- `dev.tinyland.bulkload.agent-stage-receipt.v4`
- `dev.tinyland.bulkload.agent-apply-receipt.v4`
- `dev.tinyland.bulkload.agent-verify-receipt.v4`
- `dev.tinyland.bulkload.agent-rollback-receipt.v4`
- `dev.tinyland.bulkload.agent-recover-receipt.v4`
- `dev.tinyland.bulkload.agent-journal.v4`

All artifacts use sorted, compact UTF-8 JSON and a SHA-256 over the complete
body excluding its own digest field. Capture and plan bind the pinned Bulkload
application-source closure. Provider values, authentication contents, SQLite
rows, symlink payloads, and unsanitized remote URLs never appear in evidence or
errors.

## Capture barrier

Planning requires two distinct source captures and two distinct destination
captures. All four IDs differ. Each immutable-live B explicitly binds A's seal,
retains every A identity, and shares A's static contract; B is plan authority.
Sessions remain alive. Final authority instead requires an attended
no-interaction interval plus two matching complete live epochs before and after
source transport. The interval remains active through Sting verification; Neo
then binds that exact verify/apply/plan identity and runs two matching live
epochs before emitting the sealed cutover-release receipt. That receipt is an
observational release cut, not atomicity after interaction resumes. No process
is signaled.

Capture evaluates at most three independent common Git authorities in parallel.
Each authority uses one persistent `git cat-file --batch` reader and one object
digest cache shared by all of its linked worktrees; `git fsck --full` still runs
once per common authority. Results are sorted before sealing, so scheduling does
not affect canonical evidence bytes.

Declared provider descendants default to portable-private after exact managed
exclusions and regenerate pruning. Readable invalid Git candidates and
workspaces whose only typed failure is alternates or `git fsck` retain exact
non-Git byte custody; malformed stable append JSONL retains exact private bytes.
A scan failure, special file, unsafe Git authority, active Git operation,
unsupported typed state, or budget exceedance still makes capture incomplete.

## GitWorkspaceV2

Each Git workspace records:

- logical, source, translated destination, and common Git paths;
- object format, HEAD, branch, and sanitized remote backing;
- every ref, including tags, notes, stash, custom refs, and symbolic targets;
- reflog and pseudo-ref recovery anchors;
- the complete local object-file transport set and full `git fsck` result;
- every linked or detached worktree, including lock/prune state;
- the exact raw index digest and typed index entries, stages, intent-to-add,
  skip-worktree, and assume-unchanged flags;
- every worktree directory, regular file, and symlink with exact mode, size,
  and content digest; and
- derived staged, conflicted, modified, deleted, and untracked dirt.

The `.git` pointer and linked-worktree administration are never copied as
working bytes. Apply initializes or reuses the destination repository, adds
verified object files, recreates worktrees through Git, installs the exact
index, then overlays exact working bytes and deletions.

Destination ref divergence does not destroy an object or ref tip. Before a
source ref update, Bulkload creates a deterministic
`refs/bulkload/recovery/destination/<digest>` ref at the destination tip.
Source recovery-only OIDs receive `refs/bulkload/recovery/source/<oid>` anchors.
Destination-only refs remain. Source worktree bytes, modes, index, deletions,
and directory topology are authoritative; exact destination state enters the
rollback journal before mutation.

The longest path-map prefix wins. This deliberately maps
`/Users/jess/git` to `/srv/fast-local/jess/git` even when the broader source
home maps elsewhere. Non-Git content beneath the declared Git fleet root is a
sibling typed catalog and is preserved without pretending it is a repository.
Catalogs retain logical Git/provider install roots, physical backing roots, and
nofollow link proofs. Translation uses logical paths; writes use backings, so
Sting's Home Manager `.codex`, `.claude`, `.gstack`, and `git` links remain.
AgentPlanV4 retains each catalog once; operations bind catalog identities rather
than embedding repeated workspace or provider records.
Capture, plan, and receipt JSON have a hard 4 GiB ceiling. When the four input
captures total more than 512 MiB, planning is destination-only: Neo pushes the
source evidence to Sting, and Sting must expose at least four times the combined
input size plus 2 GiB as `MemAvailable`. A and B are loaded and released
sequentially; equal catalog digests bind the stable body without retaining four
decoded manifests at once.

## Agent state

Provider roots are explicit and typed. Both roles bind one component-bounded
managed-exclusion policy. Relative symlinks must remain within the provider;
Codex session links may name archived sessions. Absolute/private symlinks stop
unless pruned as exact managed or regenerate state.

### Codex

The adapter includes every SQLite family (including committed WAL state),
sessions, archived sessions, history, goals, memory, queue, rules/skills, and
`auth.json`. SQLite capture uses the backup API over a stable family; it does
not raw-copy a live main/WAL pair. Auth is source-authoritative typed state and
is installed only inside the same rollback transaction.

### Claude

Projects, transcripts, history, todos, plans, memory, queue, commands, agents,
skills, settings, and unknown-named binary state are unioned. Only explicit
path-bearing JSON/JSONL/text and path-encoded project names rewrite. Credentials are
nonportable: they produce a visible hold, preserve destination auth, and
require attended destination authentication.

### Pi

Sessions, archives, history, state, memory, queue, skills, prompts, settings,
models, and typed auth are unioned. Pi auth is source-authoritative and receives
the same private rollback custody as Codex auth.

### Mutable seat state

An operator may declare named directory seats or exact optional regular-file
singletons. Their state enters as a source-authority overlay. Whole-home seats
are invalid and undeclared home content stays out of scope.

Append JSONL uses record digests: source supersets install, destination
supersets remain, and forks install source with destination custody. Equal bytes
with different mode install the source mode. Shared portable/non-Git paths are
source authoritative; destination-only union identities remain.

SQLite compatible union requires matching application ID, user version, schema,
columns, primary keys, foreign-key topology, and table set. Rows use type-tagged
hashes. Schema/shared-row/duplicate canonical-key conflicts select the consistent
source snapshot while preserving the exact destination rollback. Otherwise
source-only rows enter a reflinked destination snapshot. The result must pass
`quick_check`, `foreign_key_check`, and a fresh logical catalog before sealing.
Triggers, views, virtual tables, corrupt databases, and unknown families stop.

## Stage and capacity

Destination prepare creates the external mode-0700 stage/quarantine, verifies
captured GNU rsync, writes a mode-0600 plan-derived NUL allowlist, and gates
capacity. Neo pulls the small prepare receipt and allowlist over its outbound
connection, then streams only that sealed authority through strict SSH without
decoding AgentPlanV4. Sting materializes only after validating the plan,
allowlist, chained prepare/push receipts, and every byte. No Sting-originated
credential is needed and no live path changes. Preliminary changed-late entries defer. Final accepts a
fresh plan, reuses verified content objects across plan digests, restages every
operation, snapshots destination SQLite, and seals the only apply manifest.

Reflink reuse is mandatory when an exact destination byte source is selected.
A failed clone is a hard stop. A full copy is allowed only for truly incoming
or transformed data when the operator supplies `--allow-accounted-copy`; it is
then identified in the receipt and charged before materialization.

The capacity contract is:

```text
available >= incoming_unique + sqlite_compose + exact_overwritten + reserve
```

It never budgets or creates a full destination duplicate. On Sting, stage and
rollback live on the same reflink-capable XFS authority as the destination.
Every clone is required, fsynced, and rehashed. Rollback contains only entries
the transaction can overwrite or delete. Existing Git objects are additive;
ref tips have logical rollback plus durable recovery refs.

## Apply, recovery, and verification

Apply refuses a stale destination precondition. Before its first live mutation
it reflink-snapshots every existing target, records absent targets and exact
transaction-created parent directories, observes capacity, and seals rollback.
File installation uses a
same-directory reflinked temporary plus rename and directory fsync. Git ref
updates use Git's ref transaction primitives. Journal progress is persisted
after every mutation.

Re-running apply against a completed journal returns the exact stored receipt.
Recovery reads the same journal and deterministically continues forward or
restores rollback state. Rollback is itself idempotent and requires the exact
apply receipt digest. Additive Git object files may remain after rollback; no
source or previously existing destination object becomes unreachable because
ref state is restored.

Verification reads live paths afresh. It checks every staged byte and mode,
every exact index, all planned and recovery refs, Git object integrity, every
SQLite logical catalog, absence of retired sidecars, and all known holds. The
receipt keeps `provider_runtime_acceptance_verified=false`; a fresh attended
Codex/Claude/Pi action is required before claiming working authentication or
resume continuity.

## Explicit boundaries

Bulkload does not invoke a terminal multiplexer, TCFS runtime, Home Manager,
deployment, activation, browser profile migration, or credential rotation. It
does not mirror undeclared home state and never deletes source data. It does
not silently resolve structural ambiguity, unsafe typed state, capacity
shortfall, failed reflinks, or post-plan destination movement.
