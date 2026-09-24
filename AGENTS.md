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

## Repository

This repository is bulkload: a live-host estate mover in Rust. It is its own
project and shares no code with TCFS (tummycrypt); it may carry TCFS data
(R-N53, 2026-09-23). The Python engine, the stillness ceremony and the
installer are retired and are not recreated.

- `crates/bulkload-proto`: wire frames, row schema, refusal taxonomy.
- `crates/bulkload-agent`: walk, chunk transport, Git estate carry, SQLite
  provider-state composition, handoff probes.
- `crates/bulkload-bench`: the R23 benchmark against rclone.

Product bar: [#34](https://github.com/Jesssullivan/bulkload/issues/34) (R25).
Current engineering work: Linear project "Bulkload M2: SLO engine"
(P-TIN-177) and GitHub issues #42–#49. [docs/design.md](docs/design.md) is
the product contract; `docs/evidence/` holds measured results and the
historical rulings ledger (evidence, not current instructions).

## Engine rules

- Performance is the bar (R23, R-N57): the engine must beat rclone on the
  R23 gates, measured by `bulkload-bench` with 3-rep A/B medians.
- Unsafe-first (R-N54): raw syscalls, zero-copy and reused buffers are the
  default design choice. Every `unsafe` block carries a `// SAFETY:` comment.
- R25 (R-N58): never re-read a byte the destination already holds durably,
  and never re-read a seat whose stat identity is unchanged. Resume must be
  measurable with `source_bytes_read` counters.
- Durability: a record is never committed before the bytes it describes are
  durable. Refusals are values, never panics (R33 lint wall in each crate).
- Dependency wall: the agent must not depend on tokio, opendal, reqwest, ring
  or tonic (`crates/bulkload-agent/tests/dep_graph.rs`). Fix a violation by
  removing the dependency, not by widening the list.

## Estate rules

- Keep all operator and agent sessions running. Never signal a process.
- Include Git, agents, credentials, dots, SQLite and worktrees. Preserve both
  hosts' unique state; an unresolved state class is not an implicit exclusion.
- Git-aware union preserves refs, objects, real stashes, indexes, working bytes
  and worktree administration without changing an active checkout.
- Carry account credentials privately; preserve destination machine identity
  and Home Manager authority. Never print credentials.
- Capture SQLite through its backup API; never copy live WAL/SHM bytes or
  replace a database with unique rows. Preserve conflicts for explicit
  resolution.
- Reclaim only proven redundant data after preserving unique content.
- Estate operations are frozen until the M2 gates pass (R-N56).

## Agent lanes

- A guard-hook refusal stops the lane. Report it verbatim and run nothing
  more, not even a corrected marker placement, until the operator rules
  (R-N12, R-N80).
- Keep process-control words out of command text. The repo's pre-commit
  process-safety audit is the authoritative wording check (R-N92). If a
  self-audit that searched for those words is refused, drop the audit and
  continue. Any other refusal still stops the lane (R-N101).
- Never check whether a PID is alive. Wait only on your own background
  tasks' notifications, or on files (R-N104).
- Never bypass git hooks from the shell (`core.hooksPath`, `--no-verify`).
  Only the product's own cargo tests may disable hooks, and only in fixture
  repos they create and destroy (R-N98).
- Each lane builds with its own `CARGO_TARGET_DIR`. A shared target
  directory lets one lane test another lane's binaries.
- Local cargo builds on neo carry the inline
  `TINYLAND_ALLOW_LOCAL_BUILD="<lane> (R-N69)"` prefix as the first token.
- A gated benchmark sample is recorded only on AC power with a 1-minute
  load under 2.5. Every sample row records power and load (R-N81). The
  coordinator holds the other lanes quiet while gated samples run (R-N91).

## Durable notes

Every session writes one entry in `docs/agent-notes/`, named
`YYYY-MM-DD-<lane>.md` (R-N13, R-N84). An entry records what was done,
the rulings it cites, the PRs and shas it produced, and what is still open.
Distilled facts and rulings also go on the owning Linear issue. Nothing
durable is left in `/tmp` or a harness scratchpad. Delete entries that are
no longer true (R-N55).

## Validation

`just check` runs the repository contract checks, `just rust-check`
(`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace`) and the W7 fault harness (`just fault-harness`). CI remains on GloriousFlywheel tinyland-nix; keep
`.bazelrc.flywheel` endpoint-free. Changing `flake.nix`, the public-read guard
or its composite action requires updating the pinned digests in
`scripts/ci-public-read-guard.sh`, the action and `tests/test_ci_contract.py`.
CI note (R-N122): the W7 fault harness (`just fault-harness`) runs as its own
`fault-harness` terminal gate, parallel to `source`, `build` and `test`.
Stage explicit paths.
