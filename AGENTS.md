# bulkload Development Context

> **Read [docs/SPEC.md](docs/SPEC.md) first.** It states what bulkload is for,
> traced to the operator's own words in
> [docs/intent-ledger.md](docs/intent-ledger.md). R0: every work lane carries
> on at the destination (conversations resume and can be carried on,
> transcripts intact, repos and worktrees intact, nothing dropped but truly
> recreatable content; transcripts byte for byte under their original
> session IDs, SPEC C8/C9). R3: proven faster than rclone on complex networks and many-file estates,
> staging work data before tcfs launches. Where this file, `docs/slo.md` or a
> Linear document disagrees with SPEC on what bulkload must achieve, SPEC
> wins.
>
> - **Quote the requirement you serve.** Every lane plan and every audit
>   quotes, verbatim, the SPEC requirement it serves (R0–R4), with the ledger
>   IDs SPEC cites for it. A plan or audit that cannot quote one is out of
>   scope. "Done", "ready" or "verified" is reported only with SPEC's
>   acceptance receipts.
> - **Never re-ask a standing ruling.** The rulings in SPEC's "Standing
>   operator rulings" table are never re-asked, and no interview offers an
>   option that contradicts them. SPEC's 2026-10-16 rulings on C1–C20
>   (OI-1003-Q194) are standing too. A new conflict may be asked once, as a
>   dated question. When the operator restates a standing ruling, even
>   phrased as a question, that is confirmation, not an open question.
> - **The ledger is append-only.** `docs/intent-ledger.md` is never trimmed or
>   paraphrased, R-N55 included. New operator statements are added as new
>   entries.

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
- No builds or tests on neo (OI-1003-Q196, 2026-10-16; it retires R-N69
  and upholds FOOT-03 and FOOT-10). Linux gates run on sting; Darwin checks
  (`just darwin-check`) run on pzm.
- A gated benchmark sample is recorded only on AC power with a 1-minute
  load under 2.5. Every sample row records power and load (R-N81). The
  coordinator holds the other lanes quiet while gated samples run (R-N91).
  Standalone `s2_budget.py run` keeps load1 < 2.5. An S2 observation
  alongside a live run (`observe`/`alongside`, OI-1003-Q152) is judged
  against its own OFF windows as the live baseline, with no absolute load1
  cap and the budget unchanged (OI-1003-Q160, Linear TIN-4543); it is not
  interchangeable with a gated `run` and carries its own evidence label.
  Its passes start on a schedule recorded in advance (`s2_budget.py
  schedule`, `observe --schedule`, `drive`), never on the host's load, and
  evidence verifies the schedule was followed (OI-1003-Q168).

## Durable notes

Every session writes one entry in `docs/agent-notes/`, named
`YYYY-MM-DD-<lane>.md` (R-N13, R-N84). An entry records what was done,
the rulings it cites, the PRs and shas it produced, and what is still open.
Distilled facts and rulings also go on the owning Linear issue. Nothing
durable is left in `/tmp` or a harness scratchpad. Delete entries that are
no longer true (R-N55).

## Validation

Merge gate (OI-1003-Q199, 2026-10-16, Linear TIN-4543 comment `b208cbae`):
a PR merges on a local receipt from sting, not on runner CI.
`just local-gate-receipt --pr N --post` (`scripts/local-gate-receipt.sh`)
runs, from the repository root, exactly what the CI action runs:
`nix flake check --no-build --no-write-lock-file`, then
`nix develop --no-write-lock-file --ignore-environment` keeping only CI's
`HOME`, `NIX_*_HOME` and `XDG_*_HOME`, running `just local-gate`
(`check-fast`, the history secret scan limited to HEAD's ancestry
`secrets-scan-head`, and the Bazel graph load `bazel-graph`; the contract
test holds it to CI's gate set and derives the `nix develop` line from the
action). Ambient `BULKLOAD_*`, `RUSTFLAGS`, `CARGO_*` and `PATH` never reach
the gate. It refuses a dirty tree and an overlay (an ignored or excluded
file other than build output, such as `.envrc.local` or `.env.local`; a
skip-worktree or assume-unchanged file; a cargo config above the checkout
or in `~/.cargo`) unless `--allow-overlay` is given, which the receipt
records. The receipt line leads with PASS or FAIL and records head, tree,
main, the flake.lock and log digests, the exit code, the kind and the
overlay; `--post` comments a PASS as "Local gate receipt" and a FAIL only
as "Local gate FAILED", never as a receipt. The kind comes from the diff:
docs when every changed path is Markdown, else code; `--kind` may raise
docs to code, never lower it. Docs PRs need the receipt only; code PRs also
need the lanes' adversarial review (OI-1003-Q173).
A receipt is valid only while receipt.head is the PR head and receipt.main
is main's current tip: a push to the PR or any merge to main voids it.
Right before `gh pr merge --match-head-commit`, the merge runs
`just local-gate-verify --pr N`, which checks both against GitHub, the
receipt's tree against the head's, and that the newest gate comment from a
writer for that head and main is a PASS. A PR that `just darwin-touched`
flags (a Darwin path, the io layer, a Rust file with any `target_os`,
`target_vendor`, `target_family`, `cfg!(` or unix/windows cfg, `build.rs`,
`.cargo/`, `Cargo.*`, the toolchain or `flake.*`) also needs
`just darwin-check` on pzm, never neo (OI-1003-Q196); while pzm is on hold
that check is deferred and recorded with the receipt. CI runs only on `v*`
tags and `workflow_dispatch`, as the backstop. The local gate retires when
GloriousFlywheel v4 fabric check runs land (TIN-5946).

Local first (OI-1001-Q2, 2026-10-01): run `just check-fast` (the mandatory
tier: every ratified-contract guard, and the core of the merge gate) inside
`nix develop` before pushing; GloriousFlywheel CI is a contended backstop,
not the first signal. `just check-optional` runs the optional tier
(bench-script stubs, the estate corpus self-test, the history secret scan,
the flake and Bazel graph) on demand; `just check-full` runs both. The
fault harness, the R-N88/R-N119 power-loss proofs and the R25 counter tests
are mandatory and never move to the optional tier.

Deep tier (OI-1003-Q78, OI-1003-Q81): `just props-deep` and
`just crash-sweep` run on demand. They are never a PR gate and are in
neither tier above. `props-deep` sets `BULKLOAD_PROPTEST_DEEP=1`: every
property that runs through `test_support::prop_config` draws twenty times
its cases from the same fixed seed, and the heavy fixed rows that skip
themselves in the PR gate run. One property does not run through the helper
yet (the `EXEMPT` list in `tests/prop_seed_guard.rs`): `refusal_taxonomy`
keeps its own fixed seed and case count. No property draws a random seed.
`crash-sweep` crashes one copy at every hit of every point in the fault
harness's `scenarios!` table. Both recipes fail unless the rows and the
sweep really ran; a skipped one still reports `ok` to cargo (R-N122). A
test moves to the deep tier only by a ruling, and its PR names it.

`just check` runs the repository contract checks, `just rust-check`
(`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
the `io-trace` clippy pass, and `just rust-test`: the workspace tests, built
once and run as concurrent groups that must cover every built test binary)
and the W7 fault harness (`just fault-harness`). The fault harness is one
`fault-injection,io-trace` build; it also runs the traced `io::` lib tests
and the P5 partial-write proof, which the source gate no longer builds
(OI-1003-Q81), and runs its copy-based suites a second time with the
loopback hand-off off (`just handoff-off`, `BULKLOAD_LOOPBACK_HANDOFF=0`,
OI-1003-Q154 review), so the transport path `pull` takes keeps its crash
coverage, and a third time with the delta lever on (`just delta-lever`,
`BULKLOAD_LOCAL_NEW_PATH_STREAM=1`, OI-1003-Q154 review), so the opt-in
streamed new path keeps it too. CI remains on GloriousFlywheel tinyland-nix; keep
`.bazelrc.flywheel` endpoint-free. Changing `flake.nix`, the public-read guard
or its composite action requires updating the pinned digests in
`scripts/ci-public-read-guard.sh`, the action and `tests/test_ci_contract.py`.
CI note (R-N122): the W7 fault harness (`just fault-harness`) runs as its own
`fault-harness` terminal gate, parallel to `source`. CI runs exactly
those two gates: OI-1003-Q65 (2026-10-06) dropped the Bazel `build` and
`test` gates. They built the docs filegroup `//:bulkload` and ran the two
Python contract tests (`//:tests`); neither compiled Rust. `ci-source` now
runs those tests directly (`contract-test`). The Q65 residual gap (no PR
loaded the Bazel graph) is closed by `bazel-graph` in `local-gate`
(OI-1003-Q199): a query that resolves bzlmod against `MODULE.bazel.lock` and
loads `rules_python`, the `//:bulkload` globs and `//:tests`; nothing is
built, so the `//:tests` sandbox is not exercised. `just test-local` (in
`check-optional`) runs `//:tests`; `just check` and `just ci` run both
targets.
The contract test pins `MODULE.bazel` and checks that `ci_contract_test`
data lists every file it reads. Changing the workflow updates
`WORKFLOW_SHA256` in `tests/test_ci_contract.py`.
Stage explicit paths.
