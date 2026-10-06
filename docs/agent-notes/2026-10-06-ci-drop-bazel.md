# 2026-10-06: ci-drop-bazel — drop the Bazel build/test CI gates (D6)

**Lane:** `ci-drop-bazel`, workflow subagent, worktree
`bulkload.worktrees/ci-drop-bazel-20261006`, branch
`ci/drop-bazel-gates-20261006` from `origin/main` `b6ecd50`.
**Rulings:** OI-1003-Q65 (drop the Bazel `build` and `test` gates; this lane
may edit `.github/` for them, not `flake.nix`/`flake.lock`), OI-1003-Q14 and
OI-1003-Q7 (slim CI, bounded tested corpus), R-N13 (receipts and this note).

## Why

The completion-bar audit of the main CI run on 2026-10-05 measured: `source`
432–690 s, `fault-harness` 303–370 s, Bazel `build` 60–101 s, Bazel `test`
69–98 s. `build` builds the docs filegroup `//:bulkload`; `test` runs
`//:tests`, which is the two Python contract tests. Neither compiles Rust, so
they add runner time and two required checks without adding Rust coverage.

## What changed

- `.github/workflows/ci.yml`: matrix `gate: [source, build, test,
  fault-harness]` → `gate: [source, fault-harness]`. Nothing else changed.
- `tests/test_ci_contract.py`: `WORKFLOW_SHA256` re-pinned
  `6b3df584…61f2` → `88912d6d…1b84` (the D6 digest). New
  `WORKFLOW_GATES` / `WORKFLOW_GATE_MATRIX` hold the exact matrix literal.
  The mutation test now also fails closed when `build` or `test` is put back
  in the matrix. `TERMINAL_GATES` (the four paths the composite action
  accepts) is unchanged.
- `AGENTS.md` CI note and `docs/plans/2026-10-03-property-test-plan.md`
  (§3 table row and D6) now record the ruling.
- Unchanged on purpose: the composite action (`ACTION_SHA256`), the guard
  (`GUARD_SHA256`), `flake.nix`/`flake.lock`, the justfile (lane L5), and every
  Bazel file.

## Validation

- `python3 tests/test_ci_contract.py`: 22 tests OK.
- `just check-fast` in `nix develop .#default`, under the shared
  `.check-fast.lock`, with `CARGO_TARGET_DIR=/srv/cache/jess/cargo-target/ci-drop-bazel`:
  see the lane's structured result for the exit status and wall time.

## Open

1. **CI contract test coverage gap.** `//:tests` in the dropped `test` gate
   was the only CI run of `tests/test_ci_contract.py` and of
   `validate_repo_manifest.py --self-test`. `ci-source` runs `check-source`
   and the history scan, not `contract-test`. Recommended fix (lane L5 owns
   the justfile): `ci-source: check-source secrets-scan-history contract-test`,
   with the pinned `ci-source` tuple in `tests/test_ci_contract.py` updated in
   the same PR. `check-fast` already runs `contract-test` locally.
2. **Branch ruleset.** Ruleset `21184520` on `main` requires `source terminal
   gate`, `build terminal gate`, `test terminal gate` and `fault-harness
   terminal gate` (integration 15368). The operator must remove
   `build terminal gate` and `test terminal gate`, or no PR can merge after
   this lands.
3. **Merge train.** One CI run now produces 2 check runs per head, not 4.
   `merge-train-20261003.sh` requires `total >= 4`; it should require the two
   names instead (or `>= 2`).
4. **Bazel files are not dead.** `MODULE.bazel`, `MODULE.bazel.lock`,
   `BUILD.bazel`, `tools/bazel/platforms/BUILD.bazel`, `.bazelrc`,
   `.bazelrc.flywheel` and `.bazelversion` are used by `just build`, `just test`,
   `just test-local` (in `check-optional`), `just check`, `just ci`,
   `justfile.flywheel`, `tinyland.repo.json` (`contracts.build`, the
   `bazel-ssot` layer) and the contract test's digest pins. Kept.
5. **Dormant action paths.** The composite action still carries the
   `build`/`test` steps and the guard's `bazel` mode. Pruning them is a
   follow-up that re-pins `ACTION_SHA256` and the step digests.
6. **Stale justfile comments (L5).** The `check-fast` comment says PR CI runs
   "source, fault-harness and test gates", and the `contract-test` comment
   says "CI's `test` gate runs the same two files".
7. No PR opened (by instruction). PR CI only runs on `pull_request`, so the
   "Bazel jobs gone, remaining checks green" evidence comes from the PR run.

## Coordinator follow-up (2026-10-06)

The build agent stopped before committing: its check-fast was still queued behind the shared lock. The coordinator closed the coverage gap the agent found. `ci-source` now runs `contract-test`, so CI keeps running `tests/test_ci_contract.py` and the repo-manifest self-test after the Bazel `test` gate is gone. The `PINNED_JUST_RECIPES['ci-source']` tuple is re-pinned to match (OI-1003-Q65).

The merge train's minimum check count follows OI-1003-Q68: it becomes the number of checks left after the drop. Ruleset 21184520's required checks are an operator settings change.
