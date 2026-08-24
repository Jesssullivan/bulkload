# Switch ceremony — migration-agent contract

Applies to: bulkload 0.2.0 · ceremony revision 1 · reviewed 2026-08-23

This file is the agent half of the Bulkload switch ceremony. The operator half
is the `## Ceremony` section of the repository README. Each fact appears once.
The README owns the phase sequence and the quiet-window cost. This file owns
the agent contract: the invariant, the read-only probe set, the verification
procedure, the refusal text, and the self-exclusion rule. Lab owns every fact
about terminal multiplexers, eGreg, MCP, and Linear. This file links to lab. It
does not restate lab.

Read section 5 before anything else. Section 5 decides whether a plan can reach
an accepted digest at all.

## 1. How this file is written

The procedure text follows an ASD-STE100 subset:

1. One instruction per sentence. Never join two actions with "and".
2. Imperative mood for every procedure step.
3. At most 20 words per instruction sentence. At most 25 per descriptive one.
4. Active voice. Present tense.
5. One word, one meaning. Section 2 is the controlled vocabulary.
6. No pronoun without its antecedent in the same sentence.
7. Every warning comes before the step it governs.
8. Numbered steps. At most two sentences per step. Each step names its owner.
9. The banned-word list and the forbidden process-control verb list live in
   `ceremony_contract_test`, not here. The test is their single authority. This
   file names no banned token, so the file cannot trip its own check.

## 2. Controlled vocabulary

| Term | Exact meaning in this ceremony |
|---|---|
| ceremony | The full named sequence. Never a synonym for one phase. |
| quiet window | The announced clock period with no writer running in scope. |
| wind-down | An operator-driven cooperative stop. The writer saves, then exits by itself. |
| fence | A named operator command that stops a non-interactive writer. |
| writer | Any process that can write inside a declared capture scope. |
| capture scope | Every path reachable through a declared `--home`, `--git-root`, provider root, `--seat`, or `--file-seat`. |
| quiesced | No writer runs. A process in `T` or `Ts` state is not quiesced. |
| capture pair | Two `agent-capture` runs of one role with no writer between them. |
| plan digest | The exact `plan_sha256` value the operator reviews and accepts. |
| receipt | One typed JSON artifact produced by one command. |
| migration agent | The single agent that runs Bulkload commands. It sends no signal. |
| seat | One declared mutable-state directory or singleton file. Never a whole home. |

One negative definition carries the whole ceremony:

> **quiesced does not mean stopped.** A process in `T` or `Ts` state holds its
> open file locks. It also holds an unflushed SQLite write-ahead log, mid
> transaction, for as long as it stays suspended. That is the worst possible
> state for a capture. Cooperative wind-down is the only writer state that
> produces a capture Bulkload can prove.

## 3. The hard invariant

> **HARD INVARIANT.** The migration agent must never send a signal to a process
> it does not own.
>
> The migration agent must never suspend, resume, or end an interactive agent
> session, a daemon, or a launchd job.
>
> The migration agent inventories writers. The operator stops writers.
>
> If a writer runs, the migration agent reports it and stops. The migration
> agent does not act.

The invariant hardens two existing rules. It introduces no new policy:

- `AGENTS.md`: "Migration tooling may inventory session artifacts, but terminal
  ownership remains with the operator."
- Lab `scripts/validation/codex-process-residue.py` carries the same safety
  message. It asks the operator to close the reported sessions by hand.

The migration agent emits no process-control command of any class. It emits no
signal-sending command. It emits no launchd control verb. It emits no
service-manager stop verb. It emits no multiplexer session-removal verb. Every
one of those verbs belongs to the operator, in an operator-owned step, in the
README.

## 4. Permitted read-only probes

This allowlist is closed. A command outside it is not a quiescence probe.

- `ps`
- `launchctl list`
- `launchctl print`, read form only
- `lsof`, read form only
- `stat`
- `git status`
- the lab recipe `tcfs-status`
- the lab recipe `codex-process-residue`
- the lab recipe `sting-agent-state-preflight`
- `mcp__tcfs__daemon_status`
- `mcp__tcfs__sync_status`

Every probe above reads. None of them changes a process state.

## 5. The migration agent excludes itself

`/Users/jess/.claude` is a declared provider root. A live Claude Code session
writes into `projects/**` continuously. A migration agent hosted in such a
session is therefore a writer inside its own capture scope. A Codex-hosted
migration agent has the same problem with `/Users/jess/.codex`.

Exclusion alone does not close that hole. A Neo-hosted agent also appends shared
provider-root surfaces on every turn: `~/.claude/history.jsonl`,
`~/.claude/backups/`, `~/.claude/plans/`, `~/.claude/paste-cache/`, and
`~/.codex/history.jsonl`. The carry-everything ruling carries every one of those
files. An exclusion that covered them would drop them from the migration. So
they stay in scope. One agent turn between the two captures then makes the pair
unequal, and `agent-plan` fails closed on catalog inequality.

Two rules follow. The first rule is declaration, not suspension:

> Declare the migration agent's own provider state as a reviewed managed
> exclusion on both roles. Pass
> `--managed-exclusion claude:projects/<migration-session-id>` to `agent-capture`
> for the source role. Pass the identical value for the destination role.

The second rule is the capture-pair carve-out:

> The migration agent drives every phase except the capture pairs. The operator
> closes every Neo-hosted agent session before each capture pair. The operator
> then runs the capture commands from a plain terminal. This rule holds in both
> quiet windows.

[Ruling refinement 2026-08-23, pending operator confirmation.]

Three consequences bind the declaration:

1. The exclusion value must be identical on both roles. `agent-capture` binds
   one component-bounded managed-exclusion policy per role. The two policies
   must agree.
2. The operator must read the exclusion in the plan before accepting the digest.
   An unnamed self-exclusion is a review failure.
3. The migration transcript stays on Neo. Neo is the rollback source for seven
   days, so the transcript is retained. It is not migrated.

If the operator wants that transcript on the destination later, treat the move
as a separate attended copy after the ceremony.

## 6. Verification procedure

Run these six checks in order at the start of every quiet window. The quiet
window is long; the README states its measured cost.

1. **Writer census.** Read `ps` output. Require zero `codex` root processes and
   zero `claude` root processes inside the capture scope.
2. **Suspended-process check.** Require zero processes in `T` or `Ts` state
   inside the capture scope.
   > **WARNING.** Do not resume a suspended process. Report its process
   > identifier to the operator. Stop.
3. **Lock check.** Require no `-shm` sidecar newer than one hour beside a
   declared SQLite database. A clean close unlinks that sidecar, so a newer one
   means a live writer, or one that died moments ago.
   > **NOTE.** An older `-shm` sidecar is unclean-exit debris, and no writer
   > remains to end. The probe records it as a `stale-shm-orphan` warning, and
   > the A/B catalog equality stays the proof of record. A live `-wal` sidecar
   > is a ratified capture input, never a blocker.
4. **Daemon check.** Read `launchctl list`. Require no process identifier for
   every fenced label in README Phase 1. Confirm with the lab recipe
   `tcfs-status`.
5. **Stability check.** Sample the capture-scope modification times twice.
   Separate the two samples by a fixed interval. Require no change.
6. **Destination check.** Run the lab recipe `sting-agent-state-preflight`. That
   gate is read-only.
   > **NOTE.** That recipe carries its own exit codes. Its `0` is ready, its `1`
   > is findings, and its `2` is a usage or probe error. The table below governs
   > Bulkload alone.

On any failure, print one line in the established stderr shape, then stop:

```text
bulkload-ceremony: FAIL: writer active in capture scope: pid <n> state <S> scope <path>
```

Follow that line with the refusal, verbatim:

> The migration agent sends no signal. Ask the operator to close this writer.
> Re-run the verification afterwards.

That template is the agent's own line; Bulkload itself prints
`bulkload: REFUSED: <code>: <path> (N observation(s); ...)` on exit `3`.

Bulkload's whole exit surface is four codes:

| Code | Meaning |
|---|---|
| 0 | success |
| 1 | handled error |
| 2 | launcher `BootstrapError` |
| 3 | quiescence refusal |

Code 3 is the only code this ceremony adds. Never report an exit code outside
this table.

`--acknowledge-writers-quiesced` records a procedural fence only. `docs/design.md`
states this under "Capture barrier". The flag proves no process state. Byte
equality across the capture pair is the only proof of quiescence.

> **NOTE.** The probe watches SQLite families, history journals, and
> `.git/HEAD` and `.git/index` in primary checkouts. A seat directory of
> ordinary files, a `--file-seat` singleton, and a linked worktree carry no
> witness, so they rest on the A/B catalog barrier alone. That gap is
> revision-2 scope.

## 7. What the migration agent may claim

The migration agent may claim offline state facts that a receipt carries. It
may name digests, catalogs, holds, blockers, and journal states.

The migration agent must not claim provider authentication. Every receipt keeps
`provider_runtime_acceptance_verified=false`. The attended provider turn in
README Phase 8 is the only source of that fact.

The migration agent must not report a quiet machine before all six checks in
section 6 pass. A partial pass is a refusal.
