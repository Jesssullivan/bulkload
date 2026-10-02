# 2026-10-01 local-first test tiers

Rulings: OI-1001-Q2 (reduce CI/test depth, local-first just recipes, split
mandatory / optional / superseded), OI-1001-Q6 (sign with the global key on
sting), R-N13, R-N69, R-N122.

Branch `chore/local-first-test-tiers`, worktree
`bulkload.worktrees/local-first-tiers-20261001`, PR listed below once opened.

## Done

- `just check-fast` (mandatory tier = `check-source` + `fault-harness` +
  `contract-test`), `just check-optional`, `just check-full`; `contract-test`
  runs the two Bazel py_tests directly.
- `tests/git_m1_spike.rs` moved behind the `m1-spike` feature, so the pinned
  `cargo test --workspace --locked` in `rust-check` (and so PR CI) no longer
  runs it. `check-optional` lints and runs it.
- Removed `git_carry::tests::rv3_print_legacy_key` (an ignored reviewer
  probe that asserted nothing). Legacy-key bit-identity stays asserted in
  `estate.rs`.
- No pinned recipe body, workflow, action or guard changed; the fault
  harness, the R-N88/R-N119 power-loss proofs, P5 and the R25 counter tests
  are untouched and mandatory.

## Measurements (sting, host load 60-92 on 32 cores; noisy)

- Previous CI: source gate 385-514 s, fault-harness 350-585 s, build/test
  63-95 s each; run wall 8-21 min including queueing.
- Baseline per-step local sum: 676 s cold, 455 s warm.
- `just check-fast` inside `nix develop`: 953 s wall, rc 0, with a rebuild
  after the source edit. The < 3 min target is not met on a loaded host;
  fault_harness (116-149 s) and the agent lib tests (50-81 s) dominate.
- Optional tier spike + stubs: 52 s.

## Open (operator rulings)

- A scheduled or manual GF workflow for the optional tier: the guard refuses
  any event outside push/pull_request/merge_group (R-N124 inventory), so it
  needs a guard and digest change or a hosted runner.
- Dropping the `build` / `test` matrix gates on PRs (pinned workflow).
- Deleting `tests/git_m1_spike.rs` outright vs keeping it optional.
- The D1/R5 deferred `#[ignore]` tests in `git_carry.rs`: keep as tracked
  debt or move to issues.
