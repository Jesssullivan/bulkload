# bulkload Development Context

## Ratified rulings (read first)

Operator interview 2026-09-20, Linear TIN-3692; placement per R-N14
(2026-09-21). These three rulings outrank everything else in this file. The
fuller text with enforcement paths, tests, and history lives in lab
`AGENTS.md` "Hard Rules" (carrier PR xoxd-ai/lab#1850) — this block is the
summary; it applies in this repo unchanged and sharpens the existing "never
signal a process" line below.

- **R-N11 — Process control is absolute.** Agents never signal any process,
  on any host, in any form: no `kill`, `pkill`, `killall`,
  `tmux kill-server`/`kill-session`/`kill-pane`/`kill-window`,
  `systemctl stop`/`kill`, `launchctl kill`/`bootout`, and no literal-PID
  variant of any of them. Operator-only; the agent's role is to ask. Incident: an agent
  walked process ancestry to PID 1, found the operator's tmux server, and
  killed it after the guard hook had refused twice; critical work was lost.
- **R-N12 — A guard-hook refusal is a stop.** Quote the refusal verbatim;
  propose at most ONE materially different alternative; ask before running
  it. Reformulating a refused command to evade the pattern is itself a
  violation.
- **R-N13 — Ratification.** Every mutating step cites a ruling ID in its
  receipt; a receipt without one is an unratified act. Operator questions
  and asides are never rulings. Every session writes a `docs/agent-notes/`
  entry before ending. Ticket descriptions are never rewritten, only
  superseded by dated comments.

The Python migration engine is retired. Implementation belongs to the TCFS Rust
workspace in [tummycrypt PR #592](https://github.com/Jesssullivan/tummycrypt/pull/592).
M0/M1 work is not a completed M2 mover. Read docs/design.md for the product contract.
Historical rulings and interviews are evidence, not current operational status.

- Keep all operator and agent sessions running. Never signal a process.
- Include Git, agents, credentials, dots, SQLite and worktrees. Preserve both
  hosts’ unique state; an unresolved state class is not an implicit exclusion.
- Git-aware union preserves refs, objects, real stashes, indexes, working bytes
  and worktree administration without changing an active checkout.
- Carry account credentials privately; preserve destination machine identity
  and Home Manager authority. Never print credentials.
- Capture SQLite through its backup API; never copy live WAL/SHM bytes or replace
  a database with unique rows. Preserve conflicts for explicit resolution.
- Reclaim only proven redundant data after preserving unique content.
- Do not recreate the Python engine, stillness ceremony or installer.
- Use existing registered checks and apply_patch; stage explicit paths.
- CI remains on GloriousFlywheel tinyland-nix. Keep .bazelrc.flywheel endpoint-free.

just check validates repository and CI contracts. //:bulkload packages documents,
not an executable. Rust tests and benchmarks belong in the TCFS workspace.
