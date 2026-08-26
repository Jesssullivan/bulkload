# Migration contract

## State matrix

| State | Capture | Union/apply | Verification |
|---|---|---|---|
| Git refs and objects | every ref, object transport file, recovery root, full fsck | additive objects; source refs; destination divergence recovery refs | exact planned refs/anchors plus fsck |
| Linked worktrees | path, branch/detached HEAD, lock/prune, Git dir | recreate with Git, never raw-copy `.git` pointers | exact path/HEAD/index/work bytes |
| Index and dirt | raw index digest, stages, intent flags, deletions, symlinks, modes, bytes | install exact index and overlay exact worktree | fresh digest/mode/index checks |
| Non-Git `~/git` | complete sibling tree outside repositories | source overwrites shared paths with rollback custody | exact typed entries |
| Codex | SQLite/WAL, session/archive/history/goals/memory/queue/auth | append and row union; typed source auth | logical DB and exact byte checks plus attended provider turn |
| Claude | project/session/history/memory/queue/settings/auth classification | JSON path rewrite; nonportable auth hold | rewritten digest, union, attended auth |
| Pi | session/archive/history/state/memory/queue/auth | append/state union; typed source auth | exact union plus attended provider turn |
| Mutable seat | exact directory or optional regular file | explicit source-authority overlay | exact typed entries/absence |
| Unsafe/ambiguous state | blocker | never inferred | no receipt |


## Required sequence

1. Without signaling sessions, capture preliminary source and destination A,
   then B with the exact A seal passed through `--snapshot-base-seal`.
2. Compile/review/accept the preliminary plan, then resume writers. If the four
   captures total more than 512 MiB, Neo first pushes its two captures to the
   private Sting evidence directory and planning runs on memory-qualified Sting.
   The gate is per input file and applies to every subcommand: any JSON input
   over 512 MiB requires `/proc/meminfo` to report four times that file's size
   plus 2 GiB as `MemAvailable`. macOS has no `/proc/meminfo`, so such an input
   fails closed on Neo — including the final plan that Neo's cutover-release
   step must load.
3. On Sting prepare the private stage and sealed NUL allowlist; Neo pulls the
   small prepare receipt and allowlist, then streams that authority without
   loading AgentPlanV4; on Sting materialize preseed. Every SSH connection
   originates on Neo.
4. Capture fresh final A/base-B pairs and accept a fresh plan digest. Before
   source B, begin an attended no-interaction interval on Neo; processes remain
   alive and unsignaled.
5. Repeat prepare/push/materialize with `--phase final`. Neo's push requires two
   matching complete live epochs before and after transport. Keep the
   no-interaction interval after the push; the transport receipt does not end it.
6. Pass the exact capacity gates and reflink-snapshot every overwritten entry.
7. Apply through the journal and verify on Sting. Neo then pulls the exact verify
   receipt over its outbound connection and runs `agent-verify` with
   `--destination-verify-receipt`; only its sealed cutover-release receipt ends
   the interval. The release is an observational cut; it does not claim
   atomicity after interaction resumes. Then perform attended provider
   acceptance.
8. Retain plan, stage, journal, rollback, and receipts until disposition.

## Path authority

Path maps are absolute source-to-destination prefix pairs. Longest prefix wins.
For Neo to Sting the contract includes:

```text
/Users/jess/git     -> /srv/fast-local/jess/git
/Users/jess/.codex  -> /srv/fast-local/jess/state/codex
/Users/jess/.claude -> /srv/fast-local/jess/state/claude
/Users/jess/.gstack -> /srv/fast-local/jess/state/gstack
```

A broader `/Users/jess` map may coexist; it cannot override the exact Git map.
Logical provider/install paths drive translation.
Nofollow-proven physical backings drive reads and writes.
Sting's `.codex`, `.claude`, `.gstack`, and `git` Home Manager links are never
replaced.
Every linked worktree and declared agent/seat root must translate; unmapped
state blocks.

## Divergence rules

- Destination-only Git refs and agent union members remain.
- A destination ref at the same name but another OID is anchored before update.
- Git worktree topology/bytes, non-Git shared paths, and portable provider state
  are source authoritative with exact destination rollback snapshots.
- Append prefixes still union; true forks install source with destination custody.
- Codex/Pi auth and declared mutable seats are explicit source authorities.
- Claude auth is held, never copied.
- Compatible SQLite schemas union. Schema/shared-row/duplicate-key conflicts use
  a consistent source snapshot while preserving the exact destination snapshot.
- Declared provider descendants default portable-private. Exact managed
  exclusions/regenerate trees prune first; special entries and unsupported
  SQLite objects block.

No blocker may coexist with operations in a ready plan.

## Capacity and reflinks

Keep the stage and rollback root outside live paths.
On Sting, use the reflink-capable `/srv/fast-local` XFS authority.
Every selected clone is required and rehashed.
Failure stops; Bulkload never silently copies the whole file.

The plan and receipts separate:

- bytes reusable by destination reflink;
- unique incoming bytes;
- bytes created by path rewriting;
- SQLite composition bytes; and
- exact bytes at paths that final apply can overwrite.

The gate is `available >= incoming + compose + overwritten + reserve`. A full
destination rollback duplicate is forbidden.

Destination `prepare` creates mode-0700 stage/quarantine, writes a mode-0600
plan-derived allowlist, and gates capacity.
Neo `push` validates and streams that sealed allowlist plus captured GNU-rsync
evidence without decoding the plan.
Sting `materialize` requires chained prepare/push receipt digests and gates
again.
Plans and receipts use exact owner-private paths and travel only over
Neo-initiated strict SSH; no Sting-originated credential exists.

## Journal states

The apply journal moves monotonically through:

```text
preparing-rollback -> rollback-sealed -> applied -> verified
                                      \-> rolled-back
```

Mutation progress is fsynced after each entry.
Recovery validates the plan, stage manifest, transaction ID, and existing
journal before continuing forward or restoring.
A completed operation returns the stored exact receipt on repeat.

## Receipt truth

Receipts may claim only offline state facts: artifact digests, exact paths and
modes, ref/index/object observations, SQLite logical catalogs, holds, capacity,
rollback custody, and transaction state.
They never include provider payloads.
They always retain `provider_runtime_acceptance_verified=false` where relevant.

Only fresh attended Codex, Claude, and Pi actions establish runtime
authentication, picker visibility, resume/dialog continuity, and new-session
persistence.
