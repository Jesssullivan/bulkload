# 2026-10-06: no-fuzz seed guard

**Lane:** no-fuzz-seed-guard (workflow subagent, sting).
**Branch:** `feat/seeded-proptests-20261006`, worktree
`bulkload.worktrees/seed-guard-20261006`, based on `origin/main` `b6ecd50`.
No PR opened (the coordinator opens it).
**Rulings:** OI-1003-Q7 (property tests with fixed seeds, no fuzzing),
OI-1003-Q14, OI-1003-Q60, R-N13 (this note).

## What was wrong

`ProptestConfig::default()` means `RngSeed::Random` and a
`proptest-regressions` persistence file, so three properties drew a new
corpus on every CI run:

- P1 `io::buf::tests::pool_matches_its_model` (256 cases);
- P2 `io::chunker::tests::fused_and_segmented_match_the_oracle` (48 cases);
- `tests/git_carry_v2.rs` `random_dags_equal_upload_pack` (12 cases,
  persistence off, seed still random).

Two more files carried their own fixed-seed configs instead of
`test_support::prop_config`: `src/refuse.rs` and `tests/git_estimate_dag.rs`
(a mirrored copy of the helper). `tests/refusal_taxonomy.rs` does the same and
is L5's file, so it was left alone.

## What changed

- P1 and P2 run through `crate::test_support::prop_config` with their old case
  counts (P1: 256, 8 under Miri; P2: 48). Their seed moved from random to
  `CI_SEED`, and nothing persists.
- `refuse.rs` uses the helper. **Its seed changed** from its own
  `0x7265_6675_7365_6174` to `CI_SEED` (256 cases, unchanged). The property
  ranges over errno 1..4096 and any `u64`, so the seed choice carries no pinned
  shape.
- `git_estimate_dag.rs` drops its mirror and compiles `src/test_support.rs` as a
  `#[path]` module. The seed (`CI_SEED`), the 12 cases and the deep switch are
  identical, and the two can no longer drift. No library API was widened.
- New `crates/bulkload-agent/tests/prop_seed_guard.rs`, auto-discovered (no
  `Cargo.toml` edit), so it runs in `cargo test --workspace` and check-fast. It
  reads every workspace `.rs` file (symlinks not followed; hidden dirs,
  `target`, `bazel-*` skipped; comment lines ignored) and fails on:
  - a `ProptestConfig` or `test_runner::Config` construction;
  - any `RngSeed`;
  - a local `fn prop_config`;
  - a `#[proptest]` or `TestRunner::` without the helper;
  - a `proptest!` block whose first item is not
    `#![proptest_config(test_support::prop_config(..))]`. A bare
    `prop_config(..)` passes only where the file imports
    `test_support::prop_config`.

  The guard skips two files: the helper and the guard itself, whose test
  inputs spell the patterns it refuses. Its tests:
  - `the_helper_fixes_the_seed_and_persists_nothing` checks `RngSeed::Fixed(CI_SEED)`
    and `failure_persistence: None`;
  - `the_guard_refuses_each_escape` and `the_guard_accepts_the_helper` cover the
    patterns;
  - `the_exemption_list_only_shrinks` keeps at most 2 entries and 9 findings,
    with the list sorted and unique.
- Exemptions, each pinned to its **exact** finding count (fewer or more fails):
  - `tests/git_carry_v2.rs`: 3 findings. It disappears with L5 (carry_v2
    deletion). A missing exempt file is tolerated and logged, so L5 does not
    have to edit the guard. Drop the entry afterwards.
  - `tests/refusal_taxonomy.rs`: 6 findings. It migrates onto the helper
    after L5 lands.
- `docs/plans/2026-10-03-property-test-plan.md` changes:
  - the P1 and P2 rows state their seed policy;
  - the inventory row records the move;
  - §1 Conventions describes the guard;
  - the §3 contract-test bullet is marked landed;
  - P66's "mirrored `prop_config`" now names the `#[path]` module.

## Evidence

- Green run: `cargo test -p bulkload-agent --test prop_seed_guard`: 5 passed,
  `scanned=83 exempt_present=2 escapes=0`.
- Red run, in a scratch copy of the tree: P1 was reverted to
  `ProptestConfig { cases: 256, ..ProptestConfig::default() }` and the guard
  failed with these two findings:
  - `crates/bulkload-agent/src/io/buf/tests.rs:116: proptest! block without
    #![proptest_config(test_support::prop_config(..))]`
  - `crates/bulkload-agent/src/io/buf/tests.rs:118: ProptestConfig outside the
    helper`

  The scratch copy was deleted afterwards.
- `just check-fast` (CI toolchain, `nix develop .#default`, under the shared
  check-fast lock): green; see the commit's lane receipt.

## Open

- After L5 lands:
  - drop the `git_carry_v2.rs` entry;
  - migrate `refusal_taxonomy.rs` onto the helper (its seed
    `0x5733_7265_6675_7365` becomes `CI_SEED`) and drop its entry;
  - lower `EXEMPT_CEILING` and `FINDINGS_CEILING` to match.
- The guard is text based. It does not see a config built through a macro
  that hides the `proptest!` spelling, or a block comment (`/* */`) that
  hides a pattern.
- The guard does not refuse a stray `proptest-regressions/` directory. A
  leftover in a developer's checkout would make it red locally without
  showing a regression in the tree.
