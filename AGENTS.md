# bulkload Development Context

## Ratified rulings (read first)

Operator interview 2026-09-20, Linear TIN-3692; placement per R-N14
(2026-09-21). These three rulings outrank everything else in this file. The
fuller text with enforcement paths, tests, and history lives in lab
`AGENTS.md` "Hard Rules" (carrier PR xoxd-ai/lab#1850) — this block is the
summary; it applies in this repo unchanged, and the session line in "Estate
rules" below defers to its R-N11 bullet.

- **R-N11 — Agents never kill agents, tmux or live sessions** (amended
  2026-09-24, TIN-3692). Operator-only on any host: signalling another
  agent's processes (codex, claude, pi, pi-*, kimi, junie, opencode and their
  parent sessions) by PID, pattern or name; killing a tmux server, session,
  pane or window (`tmux kill-*` in any spelling, `respawn-* -k`); killing any
  live operator or interactive session "and similar" (stopping sshd or a
  session transport, `loginctl terminate`/`kill`, a whole-domain
  `launchctl bootout`, `systemctl isolate`, `launchctl reboot logout`/`apps`).
  Raw `kill`/`pkill`/`killall` and the other kill-family tools stay refused in
  every form, as do `systemctl kill`, `launchctl kill` and `systemctl freeze`,
  because a literal PID cannot be proven not to be an agent or tmux process.
  Permitted when ruled work needs it: service lifecycle on a NAMED job or
  unit (`launchctl bootout`/`bootstrap`/`kickstart <domain>/<label>`,
  `systemctl [--user] stop`/`start`/`restart <unit>`), except a stop,
  restart, reload, disable or mask of an agent, tmux, cmux, sshd or
  login-session surface; and host reboots, power-off and halt. Before any
  of them the agent states who is logged in, any tmux servers and any agent
  processes on that host, and asks first if anything is live (operator
  interviews 2026-09-24). Kexec, suspend, hibernate, a firmware-setup or
  boot-menu hold, and a reboot, power-off or halt over D-Bus stay
  operator-only.
  Incident: an agent walked process ancestry to PID 1, found the operator's
  tmux server, and killed it with a literal PID after the guard hook had
  refused twice; critical work was lost.
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
  provider-state composition.
- `crates/bulkload-bench`: the R23 benchmark against rclone.
- `crates/bulkload-handoff`: the credential-class handoff probes, kept out of
  the agent binary (WP10).

Product bar: [#34](https://github.com/Jesssullivan/bulkload/issues/34) (R25).
[docs/design.md](docs/design.md) is the product contract; `docs/evidence/` holds
measured results and the historical rulings ledger (evidence, not current
instructions).

## Sources of truth

Point to these; do not copy their content into agent files or notes.

- **Product shape, real goals, SLO summary, proof package and the live
  workstream ledger:** the Linear document
  [Bulkload — product shape, SLOs, proofs and live workstreams (SSOT)](https://linear.app/tinyland/document/bulkload-product-shape-slos-proofs-and-live-workstreams-ssot-5e4bab288565)
  in project P-TIN-177.
- **SLO definitions S1–S5, the completion bar and policy rulings
  (versioned):** [docs/slo.md](docs/slo.md) (OI-1003-Q1..Q13).
- **Engine history and gate receipts:** Linear TIN-4543. **Estate and cohort
  history:** TIN-3692.
- **Work items:** GitHub issues, labelled `gate-a`, `pre-migration` or `later`.

The completion bar is confidence in the code and the product. A full neo→sting
migration is one more run of a trusted tool, not a scheduled event
(OI-1003-Q13).

## Live workstreams

Adopted from lab `LAB-FEDERATED-EFFICIENCY-20261001`, by operator direction on
2026-10-03:

- **Restate the workstreams every context.** At least once in every context,
  including after compaction or recovery, visibly restate the live, queued and
  held bulkload workstreams. For each, give:
  - its ticket or goal;
  - its owner (seat, agent or workflow);
  - its repo and branch, PR or worktree;
  - its current state and evidence;
  - its dependency or blocker;
  - its next action.

  Mark reported facts separately from verified ones, and label unknowns
  explicitly.
- **Keep the ledger current.** Reconcile the ledger in the Linear SSOT document
  when a stream changes state: a PR opened or merged, a gate run, a hold lifted.
  The chat restatement and the document must agree.
- **Compact format is allowed.** A compact table works after a readable
  explanation, provided holds and unfinished work stay visible.

## Engine rules

- Performance is the bar (R23, R-N57): the engine must beat rclone on the
  R23 gates, measured by `bulkload-bench` with 3-rep A/B medians.
- R23 amendment (2026-10-02, OI-1002-Q30): the #88 gate (a) sample runs
  the outer order B/A/B/A/B (`r23_ab.py`); B passes the R23 gate if and
  only if every B rep's bench verdict passes. A is the informational
  baseline and never decides the gate.
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

- Keep all operator and agent sessions running. Never signal another agent's
  processes, and never kill a tmux server, session, pane or window or any
  live session. Other process control follows the R-N11 bullet above: named
  service lifecycle, reboots, power-off and halt are permitted when ruled
  work needs it, after a live-session check; D-Bus power calls stay refused.
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

Local first (OI-1001-Q2, 2026-10-01): run `just check-fast` (the mandatory
tier: every ratified-contract guard, and what PR CI runs) inside
`nix develop` before pushing; GloriousFlywheel CI is a contended backstop,
not the first signal. `just check-optional` runs the optional tier (the W6 M1
spike behind the `m1-spike` feature, bench-script stubs, the history secret
scan, the flake and Bazel graph) on demand; `just check-full` runs both. The
fault harness, the R-N88/R-N119 power-loss proofs and the R25 counter tests
are mandatory and never move to the optional tier.

Deep tier (OI-1003-Q78, OI-1003-Q81): `just props-deep` and
`just crash-sweep` run on demand. They are never a PR gate and are in
neither tier above. `props-deep` sets `BULKLOAD_PROPTEST_DEEP=1`: every
property that runs through `test_support::prop_config` draws twenty times
its cases from the same fixed seed, and the heavy fixed rows that skip
themselves in the PR gate run. Two properties do not run through the helper
yet (the `EXEMPT` list in `tests/prop_seed_guard.rs`):
`git_carry_v2::random_dags_equal_upload_pack` draws a random seed in both
tiers until #189 deletes its file, and `refusal_taxonomy` keeps its own
fixed seed and case count. `crash-sweep` crashes one copy at every hit of
every point in the fault harness's `scenarios!` table. Both recipes fail
unless the rows and the sweep really ran; a skipped one still reports `ok`
to cargo (R-N122). A test moves to the deep tier only by a ruling, and its
PR names it.

`just check` runs the repository contract checks, `just rust-check`
(`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
the `io-trace` clippy pass, and `just rust-test`: the workspace tests, built
once and run as concurrent groups that must cover every built test binary)
and the W7 fault harness (`just fault-harness`). The fault harness is one
`fault-injection,io-trace` build; it also runs the traced `io::` lib tests
and the P5 partial-write proof, which the source gate no longer builds
(OI-1003-Q81). CI remains on GloriousFlywheel tinyland-nix; keep
`.bazelrc.flywheel` endpoint-free. Changing `flake.nix`, the public-read guard
or its composite action requires updating the pinned digests in
`scripts/ci-public-read-guard.sh`, the action and `tests/test_ci_contract.py`.
CI note (R-N122): the W7 fault harness (`just fault-harness`) runs as its own
`fault-harness` terminal gate, parallel to `source`. PR CI runs exactly
those two gates: OI-1003-Q65 (2026-10-06) dropped the Bazel `build` and
`test` gates. They built the docs filegroup `//:bulkload` and ran the two
Python contract tests (`//:tests`); neither compiled Rust. `ci-source` now
runs those tests directly (`contract-test`). Residual gap: no PR loads the
Bazel graph (bzlmod and `rules_python` resolution, the `//:bulkload` globs,
the `//:tests` sandbox). `just test-local` (in `check-optional`) runs only
`//:tests`; `just check` and `just ci` run both targets; none is a PR gate.
The contract test pins `MODULE.bazel` and checks that `ci_contract_test`
data lists every file it reads. Changing the workflow updates
`WORKFLOW_SHA256` in `tests/test_ci_contract.py`.
Stage explicit paths.
