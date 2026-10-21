# bulkload

Read [docs/SPEC.md](docs/SPEC.md) first. It is short and it is the whole
requirement: what bulkload must do, what done means, the rules, and how each
kind of data is consolidated. It was reset on 2026-10-20 at the operator's
request. Older specs, SLO text, design notes and rulings are history, not
instructions.

What every lane must keep in mind:

- bulkload consolidates two live estates into one. Where both sides changed
  something, the result contains both, merged at the one path. It never
  writes a second copy beside the first, and it never stages.
- Done is the operator's own `claude --resume` and `codex resume` on sting.
  Nothing else is reported as done, ready or landed.
- A lane plan quotes the SPEC rule it serves. Work that serves no rule there
  is out of scope.
- Do not mint ruling IDs, boards or checkpoint cadences. Ask the operator
  once when a real decision is missing, and record the answer in SPEC only if
  the operator says so.

## Process rules

Process rules are lab's: R-N11, R-N12 and R-N13 in lab `AGENTS.md`
(R-HOOK-CONVERGENCE-20261004). In short:

- Never signal another session's processes.
- Hook output is advisory.
- Cite the operator's words or the SPEC rule in every receipt.
- Write a `docs/agent-notes/` entry per session.

## Repository

bulkload is a live-host estate consolidator in Rust, separate from TCFS
(tummycrypt).

- `crates/bulkload-proto`: wire frames, row schema, refusal taxonomy.
- `crates/bulkload-agent`: walk, transport, Git estate consolidation, agent
  home and SQLite consolidation.
- `crates/bulkload-bench`: the end-to-end race against rclone (SPEC goal 2).
- `crates/bulkload-handoff`: credential handoff probes, kept out of the agent
  binary.

## Engine rules

- Never re-read what the destination already holds durably. Resume is
  measured with `source_bytes_read` counters.
- A record is never committed before the bytes it describes are durable.
- Refusals are typed values, never panics (the deny-panics lints).
- Every `unsafe` block carries a `// SAFETY:` comment.
- The agent must not depend on anything in the `FORBIDDEN` list of
  `crates/bulkload-agent/tests/dep_graph.rs`.
- SQLite is read through its backup API or a held read snapshot. Never copy
  live WAL/SHM bytes, and never replace a database. Rows are merged.

## Lanes

- Builds and tests never run on neo or sting. They run on honey (Linux) and
  pzm (Darwin), at `nice -n 19`, each lane with its own `CARGO_TARGET_DIR`
  and `TMPDIR`. Scratch is deleted after the run.
- Never check whether a PID is alive. Wait on your own task notifications or
  on files.
- Fixture tests disable hooks only in the fixture repos they create.
- Notes are dated records. A note that is no longer true gets a dated label
  naming what superseded it.

## Validation

Merge gate (operator ruling, 2026-10-21, verbatim: "honey load is a
self-fulfillling failiure, cut that gate."): a PR merges on the lane's
targeted tests on honey, `cargo fmt` and `cargo clippy`, gitleaks, and, when
`just darwin-touched` flags it, `just darwin-check` on pzm with no new
failures. The full `just local-gate` described below stays available but is
not required. (Before that ruling, OI-1003-Q199, 2026-10-16, Linear TIN-4543
comment `b208cbae`: a PR merged on a local receipt from sting, not on runner
CI, the build and test half running on honey from 2026-10-17.)
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

Local first (OI-1001-Q2, 2026-10-01): run `just check-fast` inside
`nix develop` before pushing; GloriousFlywheel CI is a contended backstop,
not the first signal. The `justfile`'s recipe comments are the reference for
every tier (`just --list`): `check-fast` (mandatory), `check-optional`,
`check-full`, and the deep tier (`props-deep`, `crash-sweep`; OI-1003-Q78,
Q81), which is never a PR gate. CI (`v*` tags and `workflow_dispatch`) runs
two gates, `ci-source` and `ci-fault-harness` (R-N122, OI-1003-Q65);
`ci-source` also runs the history secret scan, which locally is in the
optional tier and, scoped to HEAD's ancestry, in `local-gate`.

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
- `local-gate` loads the Bazel graph (`bazel-graph`, a query, OI-1003-Q199);
  nothing in a PR gate builds it. `just test` and `just test-local` run
  `//:tests`; `just build` and `just ci` build `//:bulkload`.

Stage explicit paths.
