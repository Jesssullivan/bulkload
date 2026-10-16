# bulkload Development Context

## Ratified rulings (read first)

Process rulings are lab's, and lab's text is canonical: lab `AGENTS.md`
"Ratified rulings", **R-HOOK-CONVERGENCE-20261004** (Linear TIN-3692 comment
`98cf680c-7299-4949-bfb2-60079053ad43`). It supersedes the former R-N11
blanket process ban and the R-N12 hook-stop mechanics that this file used to
summarise (the 2026-09-20/24 text, last synced from lab#1913 by #78). In
short:

- **R-N11:** own recorded tasks and named services are permitted after
  checking ownership and live sessions. Cross-session actions require
  explicit operator authorization; the receipt names the target and the
  authorization.
- **R-N12:** every Lab hook is advisory. Findings, scanner errors and dark
  hooks may warn but never deny. Use a receipted alternative within existing
  authorization; ask only when the action itself needs a new decision.
- **R-N13:** cite the ruling in mutation receipts. Preserve durable working
  notes in `docs/agent-notes/` and facts in Linear. Explicit operator answers
  and instructions are authority; a reminder is advisory.

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

What bulkload must achieve: [docs/SPEC.md](docs/SPEC.md) (#236). How it
works: [docs/design.md](docs/design.md). `docs/evidence/` holds measured
results and the historical rulings ledger (evidence, not current
instructions).

## Sources of truth

[docs/SOURCES.md](docs/SOURCES.md) names the one canonical place for each
kind of fact. Point to it; do not copy its content into agent files or notes.

## Live workstreams

The live workstream ledger is the Linear SSOT document named in
[docs/SOURCES.md](docs/SOURCES.md). Reconcile it when a stream changes state;
restate it in chat as lab R-HOOK-CONVERGENCE-20261004 describes (brief,
changes only, no whole-board recital).

## Engine rules

- Performance: S1 in [docs/slo.md](docs/slo.md) defines the bar, the gates,
  their order and verdict rule.
- Unsafe-first (R-N54): raw syscalls, zero-copy and reused buffers are the
  default design choice. Every `unsafe` block carries a `// SAFETY:` comment.
- R25 (R-N58): no re-read of what the destination holds durably. The reading
  that is the SLO's obligation, and the strict reading's status and limits,
  are in docs/slo.md (OI-1003-Q40 and its 2026-10-06 amendment). Resume must
  be measurable with `source_bytes_read` counters.
- Durability: a record is never committed before the bytes it describes are
  durable. Refusals are values, never panics: the deny-panics lints in each
  crate's `[lints.clippy]`. Code calls that wall "R33"; the R1–R43 ledger's
  R33 is a different ruling (see that file's header).
- Dependency wall: the agent must not depend on anything in the `FORBIDDEN`
  list of `crates/bulkload-agent/tests/dep_graph.rs`. Fix a violation by
  removing the dependency, not by widening the list.

## Estate rules

- Keep all operator and agent sessions running. Process control follows lab
  R-N11 (above).
- Include Git, agents, credentials, dots, SQLite and worktrees. Preserve both
  hosts' unique state; an unresolved state class is not an implicit exclusion.
  Only truly recreatable content may be dropped (OI-1003-Q185, Q192).
- Git-aware union preserves refs, objects, real stashes, indexes, working bytes
  and worktree administration without changing an active checkout.
- Carry account credentials privately; preserve destination machine identity
  and Home Manager authority. Never print credentials.
- Capture SQLite through its backup API; never copy live WAL/SHM bytes or
  replace a database with unique rows. Preserve conflicts for explicit
  resolution.
- Reclaim only proven redundant data after preserving unique content.
- Estate operations are not frozen. R-N56's freeze is superseded by
  OI-1003-Q142 and OI-1003-Q194 C6 (docs/slo.md, amendment 2026-10-16).

## Agent lanes

- Keep process-control words out of command text. The repo's pre-commit
  process-safety audit is the authoritative wording check (R-N92).
- Never check whether a PID is alive. Wait only on your own background
  tasks' notifications, or on files (R-N104).
- Never bypass git hooks from the shell (`core.hooksPath`, `--no-verify`).
  Only the product's own cargo tests may disable hooks, and only in fixture
  repos they create and destroy (R-N98).
- Each lane builds with its own `CARGO_TARGET_DIR`. A shared target
  directory lets one lane test another lane's binaries.
- Do not build, test or run heavy checks on neo (operator, FOOT-03 and FOOT-10
  in the intent ledger; SPEC R4). The older R-N69 allowance (an inline
  `TINYLAND_ALLOW_LOCAL_BUILD` prefix) conflicts with it and is not used
  without a new operator ruling.
- Benchmark and S2 sampling conditions (R-N81, OI-1003-Q39, Q152, Q160,
  Q168) are in docs/slo.md S1 and S2. R-N91 (holding other lanes quiet for a
  sample) is superseded by OI-1003-Q194 C18 (never-halt).

## Durable notes

Every session writes one entry in `docs/agent-notes/`, named
`YYYY-MM-DD-<lane>.md` (R-N13, R-N84). An entry records what was done,
the rulings it cites, the PRs and shas it produced, and what is still open.
Distilled facts and rulings also go on the owning Linear issue. Nothing
durable is left in `/tmp` or a harness scratchpad. Notes are dated records:
one that is no longer true gets a dated label at its top naming what
superseded it, and is not rewritten or deleted (lab R-N13; this replaces the
earlier R-N55 deletion rule).

## Validation

Local first (OI-1001-Q2, 2026-10-01): run `just check-fast` inside
`nix develop` before pushing; GloriousFlywheel CI is a contended backstop,
not the first signal. The `justfile`'s recipe comments are the reference for
every tier (`just --list`): `check-fast` (mandatory), `check-optional`,
`check-full`, and the deep tier (`props-deep`, `crash-sweep`; OI-1003-Q78,
Q81), which is never a PR gate. PR CI runs two gates, `ci-source` and
`ci-fault-harness` (R-N122, OI-1003-Q65); `ci-source` also runs the history
secret scan, which locally is in the optional tier.

Rules the recipes do not enforce on their own:

- The fault harness, the R-N88/R-N119 power-loss proofs and the R25 counter
  tests are mandatory and never move to the optional tier. A test moves to
  the deep tier only by a ruling, and its PR names it. No property draws a
  random seed.
- CI remains on GloriousFlywheel tinyland-nix; keep `.bazelrc.flywheel`
  endpoint-free. Changing `flake.nix`, the public-read guard or its
  composite action requires updating the pinned digests in
  `scripts/ci-public-read-guard.sh`, the action and
  `tests/test_ci_contract.py`. Changing the workflow updates
  `WORKFLOW_SHA256` in `tests/test_ci_contract.py`.
- No PR loads the Bazel graph. `just test` and `just test-local` run
  `//:tests`; only `just ci` also builds `//:bulkload`.

Stage explicit paths.
