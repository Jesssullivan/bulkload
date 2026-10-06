# 2026-10-06: ci-drop-bazel — drop the Bazel build/test CI gates (D6)

**Lane:** `ci-drop-bazel`, workflow subagent, worktree
`bulkload.worktrees/ci-drop-bazel-20261006`, branch
`ci/drop-bazel-gates-20261006` from `origin/main` `b6ecd50`.
**Rulings:** OI-1003-Q65 (drop the Bazel `build` and `test` gates; this lane
may edit `.github/` for them, not `flake.nix`/`flake.lock`), OI-1003-Q68 (the
merge train's minimum check count becomes 2, `MIN_CHECKS`), OI-1003-Q69 (the
coordinator PATCHes ruleset `21184520` just before merge), OI-1003-Q14 and
OI-1003-Q7 (slim CI, bounded tested corpus), R-N13 (receipts and this note).
**Commits:** `32ae47f7a05a4b2592539e9ebe6944a3697eefd1` (the drop) and
`1dbc9d2e668b07babef206533677237b7a46910c` (review follow-up), both signed,
on base `b6ecd50d46d3348baf049272e57e3b406df2d071`. This note's own commit
follows `1dbc9d2`.

## Why

The completion-bar audit of the main CI run on 2026-10-05 measured: `source`
432–690 s, `fault-harness` 303–370 s, Bazel `build` 60–101 s, Bazel `test`
69–98 s. `build` builds the docs filegroup `//:bulkload`; `test` runs
`//:tests`, which is the two Python contract tests. Neither compiles Rust, so
they add runner time and two required checks without adding Rust coverage.

## What changed

In `32ae47f`:

- `.github/workflows/ci.yml`: matrix `gate: [source, build, test,
  fault-harness]` became `gate: [source, fault-harness]`. Nothing else
  changed in the workflow.
- `tests/test_ci_contract.py`: `WORKFLOW_SHA256` re-pinned from
  `6b3df5845e5f33c4d6acdabcd912898846d69695930373ff14bca7f8152f61f2` to
  `88912d6d7867551c4191e84e671d4e5e4f8b0fca5456579246773ce1113b1b84`. New
  `WORKFLOW_GATES` / `WORKFLOW_GATE_MATRIX` hold the exact matrix literal, and
  the mutation test fails closed when `build` or `test` is put back.
  `TERMINAL_GATES` (the four paths the composite action accepts) is unchanged.
- `justfile`: `ci-source` now also runs `contract-test`
  (`validate_repo_manifest.py --self-test` and `tests/test_ci_contract.py`),
  which only the dropped Bazel `test` gate ran in CI.
  `PINNED_JUST_RECIPES['ci-source']` is re-pinned to match.
- `AGENTS.md` CI note and `docs/plans/2026-10-03-property-test-plan.md` (D6)
  record the ruling.

In `1dbc9d2` (review follow-up):

- `tests/test_ci_contract.py`: `MODULE_BAZEL_SHA256`
  (`8182bbe9cf04ddb0743bd1e68a9653a851b8dbf104234ab367bc1386899c26e3`) pins
  `MODULE.bazel`, and `test_bazel_data_lists_every_workspace_read` checks that
  the `ci_contract_test` `data` list covers every workspace file the test
  reads. The file now has 24 tests.
- `BUILD.bazel`: `MODULE.bazel` joins the `ci_contract_test` data.
- `AGENTS.md` and D6 state the residual gap below and no longer say the gates
  duplicated a compile or that `check-optional` covers the Bazel graph.
- `justfile`: the `check-fast` tier comment and the `contract-test` comment
  no longer name a CI `test` gate (comments only, no re-pin).

Unchanged on purpose: the composite action (`ACTION_SHA256`), the guard
(`GUARD_SHA256`), `flake.nix`/`flake.lock`, `MODULE.bazel` and the other
Bazel configuration files.

## Residual gap (accepted loss under OI-1003-Q65, pending operator confirmation)

No PR loads the Bazel graph any more: bzlmod and `rules_python` resolution,
`BUILD.bazel` evaluation, the `//:bulkload` filegroup globs, and the
`//:tests` sandbox. `just test-local` (in `check-optional`) runs only
`//:tests`; `just check` and `just ci` run both targets; none is a PR gate.
`tinyland.repo.json` still declares `contracts.build` and the `bazel-ssot`
layer, so that contract is verified off the PR path only. The two guards in
`1dbc9d2` are text-level: they catch a `MODULE.bazel` edit and a missing
`data` entry, not a broken glob or a `BUILD.bazel` evaluation error.

## Validation

- At `32ae47f`: `just check-fast` green as reported by the review of that
  commit (29 suites ok, `repo-manifest: PASS`, 22 contract tests OK). This
  lane did not re-run it at that sha.
- At `1dbc9d2`: `python3 tests/test_ci_contract.py` ran 24 tests, OK, and
  `validate_repo_manifest.py --self-test` printed `repo-manifest: PASS`
  (both run directly, outside `nix develop`).
- At `1dbc9d2`: `just check-fast` in `nix develop .#default` under the shared
  `.check-fast.lock` was **not completed**. It waited more than 10 minutes
  behind the lock without starting and the lane ended first. `1dbc9d2` and
  this note are committed locally and **not pushed**; run check-fast, then
  push.

## Coordinator merge-time steps (not in this diff, by design)

1. **Ruleset (OI-1003-Q69).** The coordinator PATCHes ruleset `21184520` on
   `main` to remove the `build terminal gate` and `test terminal gate`
   required checks just before this PR merges. `source terminal gate` and
   `fault-harness terminal gate` stay required.
2. **Merge train (OI-1003-Q68).** The merge train's minimum check count
   becomes 2 (`MIN_CHECKS`), because one CI run now produces 2 check runs per
   head, not 4.

Until step 1 the PR's own CI shows 2 checks and the ruleset blocks the merge.
That is expected.

## Open

1. **Residual Bazel gap.** See above. The operator has not explicitly ruled
   the loss acceptable; if it is not, the fix is a PR gate that loads the
   graph, not more text checks.
2. **Contract test in the source step is unproven in CI.**
   `test_guard_executes_pr_and_main_upload_policy` calls `/usr/bin/git` and
   `/bin/bash` and uses `tempfile` under the source step's hardened `HOME`,
   `PATH` and `TMPDIR`. It ran in Bazel's sandbox before. The first PR run is
   the evidence. The source step took 432–690 s against a 900 s cap.
3. **Dormant action paths and cache upload.** The composite action still
   carries the `build`/`test` steps and the guard's `bazel` mode, and
   `upload-bazel-results` is still computed but does nothing, so `main` no
   longer warms the Flywheel cache that `just build`, `just test` and
   `just ci` use. Pruning re-pins `ACTION_SHA256` and the step digests.
4. **Stale rationale in one comment.** The `WORKFLOW_GATES` comment in
   `tests/test_ci_contract.py` still says the gates "duplicated the compile".
   Low finding, left for a follow-up.
5. **Plan §3 table.** The `source` row of the property-test plan does not
   mention that `ci-source` runs `contract-test`. Low finding, left.
6. **No positive absence assertion.** Nothing asserts that `build` and `test`
   are absent from `WORKFLOW_GATES`; one change that edits `ci.yml`,
   `WORKFLOW_GATES` and `WORKFLOW_SHA256` together brings them back. Low
   finding, left.
7. **No PR opened by this lane.** The PR body must carry all six rulings and
   both coordinator merge-time steps.
