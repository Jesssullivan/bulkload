# 2026-10-16 local-gate lane: the merge gate moves to a receipt on sting

Lane: implement operator ruling OI-1003-Q199 (Linear TIN-4543 comment
`b208cbae`). Branch `ci/local-gate-20261016`, worktree
`bulkload.worktrees/local-gate-20261016`, on sting. Rulings cited:
OI-1003-Q199, OI-1003-Q196 (no builds or tests on neo; Darwin on pzm),
OI-1003-Q173 (adversarial review for code PRs), OI-1003-Q65 (the Bazel graph
gap this closes), R-N13. The ruleset change ("Protect main" no longer
requires status checks) was made before this lane and is not in the repo.

## What changed

- `justfile`: `local-gate` (= `check-fast` + `secrets-scan-history` +
  `bazel-graph`), `bazel-graph` (a `bazelisk query` of
  `deps(//:bulkload + //:tests)` with `--lockfile_mode=error`; no build),
  `local-gate-receipt`, `darwin-touched`; `shell-lint` covers the new
  scripts; the `darwin-check` comment drops "neo with the R-N69 prefix".
  The Flywheel wrapper (`gloriousflywheel-bazel`) is not on sting's PATH,
  so `bazel-graph` uses the flake's bazelisk, as `test-local` does.
- `scripts/local-gate-receipt.sh`: refuses Darwin and neo, a dirty tree, a
  HEAD without origin/main, and (with `--pr`) a PR head other than HEAD;
  runs `just local-gate`, logs under `$BULKLOAD_LOCAL_GATE_DIR` (default
  `~/.local/state/bulkload/local-gate`), prints and appends the receipt
  line; `--post` comments it after re-checking the PR head.
- `scripts/darwin-touched.sh`: exit 0 when a diff touches a Darwin path, a
  Rust file carrying a Darwin cfg (`macos`/`ios`, `apple`,
  `not(target_os = "linux")`), or a Cargo/toolchain/flake input; else 1.
- `.github/workflows/ci.yml`: triggers are `push: tags v*` and
  `workflow_dispatch` only; the PR-only job condition and PR/merge-queue
  sha and repository expressions are gone (`github.sha`,
  `github.repository`). The public-read guard admits exactly `push` and
  `workflow_dispatch` (dispatch on a branch or `v*` tag, never uploading);
  its digest is re-pinned in the action and the contract test.
- `tests/test_ci_contract.py`: re-pinned workflow, action and guard
  digests; the trigger inventory must equal {push tags v*,
  workflow_dispatch}; the guard's event arms must equal {push,
  workflow_dispatch}; `local-gate`'s recipe closure must equal CI's
  (`ci-source` + `ci-fault-harness`) plus `check-fast` and `bazel-graph`;
  the receipt script must run `local-gate` and record every field. Each
  has mutation tests that fail closed.
- `AGENTS.md` (Agent lanes bullet and Validation only) and `docs/slo.md`
  (amendment 2026-10-16) record Q199 and Q196.

## Open

- The coordinator's merge train (`bulkload.worktrees/local-gate-merge.sh`,
  outside the repo) should call `just local-gate-receipt --pr N --post`
  instead of its inline gate, and run `just darwin-touched` to decide on
  pzm.
- #240 and #241 also edit the AGENTS.md lines this PR edits (the R-N69
  bullet and Validation); whichever lands second resolves the conflict.
