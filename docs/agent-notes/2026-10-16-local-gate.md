# 2026-10-16 local-gate lane: the merge gate moves to a receipt on sting

Lane: implement operator ruling OI-1003-Q199 (Linear TIN-4543 comment
`b208cbae`). Branch `ci/local-gate-20261016`, worktree
`bulkload.worktrees/local-gate-20261016`, on sting. Rulings cited:
OI-1003-Q199, OI-1003-Q196 (no builds or tests on neo; Darwin on pzm),
OI-1003-Q173 (adversarial review for code PRs), OI-1003-Q65 (the Bazel graph
gap this closes), R-N13. The ruleset change ("Protect main" no longer
requires status checks) was made before this lane and is not in the repo.

## What changed

- `justfile`: `local-gate` (= `check-fast` + `secrets-scan-head` +
  `bazel-graph`), `secrets-scan-head` (`secrets-scan-history` with
  `--log-opts=HEAD`: the coordinator found the full scan failed #236's
  gate on a local-only backup branch's test fixture, commit `b765b444`,
  which CI's fresh clone never sees), `bazel-graph` (a `bazelisk query` of
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
- `scripts/darwin-touched.sh`: exit 0 when a diff touches a Darwin path,
  the io layer, a Rust file with any platform cfg, or a build input; else
  1 (widened after review; see below).
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
  (`ci-source` + `ci-fault-harness`, with the history scan swapped for the
  HEAD-scoped one, whose body must be CI's plus `--log-opts=HEAD` only)
  plus `check-fast` and `bazel-graph`;
  the receipt script must run `local-gate` and record every field. Each
  has mutation tests that fail closed.
- `AGENTS.md` (Agent lanes bullet and Validation only) and `docs/slo.md`
  (amendment 2026-10-16) record Q199 and Q196.

## Review fixes (2026-10-16, second commit set)

Two adversarial reviews of head `930a40d` (one FIX-FIRST, one MERGEABLE
with minors). The branch was rebased onto main `d7d2ba35` (#236) first.
Contract tests were written first and failed (12 failures, 3 errors) before
the scripts changed.

- MAJOR, gate environment: the receipt script now runs, from the repo root,
  `nix flake check --no-build --no-write-lock-file || exit` then
  `exec nix develop --no-write-lock-file --ignore-environment` with CI's
  keeps only (HOME, NIX_*_HOME, XDG_*_HOME) running `just local-gate`, in a
  subshell whose status is the gate's. `local_gate_run_block` derives that
  block from the action's two `exec nix develop` steps, so a keep added on
  either side or a dropped flake check fails the contract. The script also
  refuses overlays (ignored/excluded files other than `target/`, `bazel-*`
  and Python caches; skip-worktree and assume-unchanged files; cargo
  configs above the checkout or in `~/.cargo`) unless `--allow-overlay`,
  recorded as the receipt's `overlay` field. Admitted residual: HOME is the
  operator's, so `~/.bazelrc` (on sting: output root, repository and disk
  cache paths only) applies to `bazel-graph`; inside the shell TMPDIR is
  nix's own, and cargo builds in the checkout's `target/`.
- MEDIUM, staleness: AGENTS.md and slo.md state receipt.head == PR head and
  receipt.main == main's tip; `just local-gate-verify --pr N`
  (`scripts/local-gate-verify.sh`) checks both, the tree, and that the
  newest writer-authored gate comment for that head and main is a PASS.
  Comments from non-writers (author_association other than OWNER, MEMBER,
  COLLABORATOR) are ignored.
- MEDIUM, darwin-touched: any `.rs` mentioning `target_os`,
  `target_vendor`, `target_family`, `cfg!(` or a unix/windows cfg on either
  side; `crates/bulkload-agent/src/io/**`; `build.rs`, `.cargo/`,
  `Cargo.*`, `rust-toolchain(.toml)`, `flake.*`. The marker grep no longer
  reads a pipe (under pipefail a large file's `git show` could die of
  SIGPIPE and hide a match). The reviewer's examples are fixtures in
  `test_darwin_touched_flags_what_darwin_compiles`.
- MINOR: the receipt leads with PASS/FAIL; `--post` posts a FAIL only as
  "Local gate FAILED ...; not a receipt". The kind is derived (docs only
  when every changed path is `*.md`); `--kind docs` on a code diff is
  refused. The contract pins the exit-code capture (and allows exactly two
  `gate=` assignments), the origin/main lookup, the PR-head pre-check, the
  post-gate HEAD/tree re-check and the post-time PR-head re-check;
  `test_local_gate_receipt_executes_its_refusals_and_verdicts` runs the
  script against a fixture origin with `nix` and `gh` stubbed.

## Open

- The coordinator's merge train (`bulkload.worktrees/local-gate-merge.sh`,
  outside the repo) should call `just local-gate-receipt --pr N --post`
  instead of its inline gate, run `just darwin-touched` to decide on pzm,
  and call `just local-gate-verify --pr N` right before
  `gh pr merge --match-head-commit`. Running both scripts from main's
  checkout (not the PR's copy) closes the self-attestation gap the second
  review noted.
- #240 and #241 also edit the AGENTS.md lines this PR edits (the R-N69
  bullet and Validation); whichever lands second resolves the conflict.

## Merge of main, 2026-10-18 (operator ruling A-20261018-110, "Rebase, gate, merge")

Merged `origin/main` `74ede7c` into the branch. None of this PR's intent was
in main (main's `ci.yml` still ran on `pull_request` and `merge_group`, and
no `local-gate` recipe or script existed). Conflicts and their resolution:

- `AGENTS.md`: main's OI-1003-Q196 neo bullet and sampling bullet kept
  (they supersede this PR's versions). Validation: this PR's merge-gate
  paragraph kept above main's restructured text; main's long `just check`
  paragraph deletion kept. Main's now-false lines corrected: "PR CI runs
  two gates" (CI runs on `v*` tags and dispatch only) and "No PR loads the
  Bazel graph" (`local-gate` runs `bazel-graph`).
- `docs/slo.md`: both sides' 2026-10-16 amendments kept; the Q199
  amendment gains a dated "Gate host" bullet: since the operator's
  2026-10-17 ruling the gate's build and test half runs on honey on the
  exact head, with merge, signing, receipt and merge on sting.
- `justfile`: `shell-lint` checks main's `scripts/install-agent.sh` and this
  PR's three scripts; main's `install-agent` recipe kept; the `check-fast`
  comment is this PR's.
- `tests/test_ci_contract.py`: `WORKFLOW_SHA256` re-pinned to the merged
  `ci.yml` (`cd23a47e…`); the trigger-mutation test uses main's 45-minute
  cap (mutation to 50).

The `/dev/shm/m2r2-gate-honey.sh` gate (check-fast plus the HEAD-scoped
secret scan on honey) is the receipt for this merge. It does not run
`bazel-graph`.

Rulings: A-20261018-110, OI-1003-Q199, OI-1003-Q195, R-N13.
