# 2026-10-06: no-fuzz seed guard

**Lane:** no-fuzz-seed-guard (workflow subagent, sting).
**Branch:** `feat/seeded-proptests-20261006`, worktree
`bulkload.worktrees/seed-guard-20261006`, based on `origin/main` `b6ecd50`.
PR: #194 (opened by the recheck stage, not merged).
**Rulings:** OI-1003-Q7 (property tests with fixed seeds, no fuzzing),
OI-1003-Q14, OI-1003-Q60, R-N13 (this note).

## Shas

- `2046563`: seed every proptest through `prop_config`; add the guard.
- `9e36a98`: review round 1. The guard accepts only the helper call as the
  whole config; the plan bullet is reworded to match.
- `d1f42c9`: this note, round 1.
- `81c5f4a`: merge of `origin/main` `2247ab8` (#190, the Bazel gates
  dropped). No conflict; main changed no Rust source.

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

## What changed (`2046563`)

- P1 and P2 run through `crate::test_support::prop_config` with their old case
  counts (P1: 256, 8 under Miri; P2: 48). Their seed moved from random to
  `CI_SEED`, and nothing persists.
- `refuse.rs` uses the helper. **Its seed changed** from its own
  `0x7265_6675_7365_6174` to `CI_SEED` (256 cases, unchanged). The property
  ranges over errno 1..4096 and any `u64`, so the seed choice carries no pinned
  shape.
- `git_estimate_dag.rs` drops its mirror and compiles `src/test_support.rs` as a
  `#[path]` module. The seed (`CI_SEED`), the 12 cases and the deep switch are
  identical. No library API was widened.
- New `crates/bulkload-agent/tests/prop_seed_guard.rs`, auto-discovered (no
  `Cargo.toml` edit), so it runs in `cargo test --workspace` and check-fast.
- `docs/plans/2026-10-03-property-test-plan.md`: the P1, P2 and P66 rows, the
  inventory row, §1 Conventions and the §3 contract-test bullet.

## Review round 1 (`9e36a98`)

The first guard was a substring test: a line passed if it contained
`test_support::prop_config(` anywhere. A struct update over the helper, a
trailing comment that spelled the helper, and a `Config` brought in by a
braced or glob import all passed with no finding. The guard now reads this
way (still text based, one line at a time):

- **Accepted, and nothing else:** the helper call as the whole config on one
  line.
  - `#![proptest_config(test_support::prop_config(<cases>))]`, with an optional
    `crate::`, `self::` or `super::`; only a `//` comment may follow `))]`.
  - `TestRunner::new(test_support::prop_config(<cases>))`, plus the plain
    import `use proptest::test_runner::TestRunner;`.
  - A bare `prop_config(..)` only under the plain import
    `use <path>::test_support::prop_config;`.
- **Refused on any line** (a trailing comment is read as code):
  - `ProptestConfig`;
  - `test_runner::Config`, `test_runner::{`, `test_runner::*`,
    `test_runner as`;
  - the bare word `Config` in a file that uses proptest;
  - `RngSeed` / `rng_seed`; `FileFailurePersistence` / `failure_persistence`;
  - `fn prop_config`, `as prop_config`, `as test_support`;
  - any other `proptest_config(`, `#[proptest` or `TestRunner`;
  - a `proptest!` block with no config.
- New test `the_guard_refuses_a_config_built_over_the_helper` holds the
  review's inputs with the lines that must be findings;
  `the_guard_refuses_each_escape` grew from 9 to 35 rows.
- **Exemption counts moved because the rules are stricter, not because the
  files changed:** `git_carry_v2.rs` 3 to 4, `refusal_taxonomy.rs` 6 to 7,
  `FINDINGS_CEILING` 9 to 11. The guard is not on main yet, so the "never
  raised" ceilings are set here for the first time. L5's copy of
  `refusal_taxonomy.rs` also scores exactly 7.
- The plan's §1 guard bullet now says the check is a line-based text match
  and lists what it accepts and refuses.

## Evidence

- `cargo test -p bulkload-agent --test prop_seed_guard`: 6 passed,
  `scanned=83 exempt_present=2 escapes=0`.
- Red run of `2046563`, in a scratch copy of the tree: P1 reverted to
  `ProptestConfig { cases: 256, ..ProptestConfig::default() }` failed the guard
  at `io/buf/tests.rs:116` and `:118`. The scratch copy was deleted.
- `just check-fast` on `9e36a98`'s tree (CI toolchain, `nix develop .#default`,
  under the shared check-fast lock, exit 0; it queued about 90 minutes behind the other lanes on the lock).

## Recheck and ship (2026-10-06)

A separate recheck stage read the diff since `2046563` and ran the guard
against probe inputs in an untracked directory (deleted afterwards).

- Refused, as intended: a struct update over the helper under a braced
  import; `Config as C`; `Default::default()` as the config; a trailing
  comment that spells the helper; a local generic `fn prop_config`;
  `TestRunner::deterministic()`; a method call on the helper.
- Still passes with no finding (low, in "Open" below): a renamed macro
  (`use proptest::proptest as pt;` then `pt! { .. }` with no config) and a
  raw-identifier mirror (`fn r#prop_config`).
- Sibling branches, scanned with this guard:
  - `feat/s3-properties-20261006`: `s3_transfer_resume.rs` 15,
    `s3_walk_resume.rs` 10.
  - `feat/s2-shm-counter-20261006`: `sqlite_wal_index.rs` 13.
  - Trial merge with L5 (#189): `git_estimate_dag.rs` 0,
    `git_estimate_shapes.rs` 0, `refusal_taxonomy.rs` 7 (its exempt count).
  - Trial merge with L6a (#191): `decide_tests.rs` 0.
  - `feat/ingest-token-20261003` (#136): `git_carry_v2.rs` 4 (its exempt
    count).
  - `feat/s2-proof-closure-20261006` and
    `feat/wp3-pr3-outcome-ledger-20261006`: 0.
- `just check-fast` on the `81c5f4a` tree (CI toolchain, shared lock): exit 0.
- #191 (L6a) merged while that check ran. Main `8e23b1d` was merged in a
  second time; the one conflict was the plan's P66 row (this lane's row kept,
  L6a's P67 row kept below it), as `3828ddd`. check-fast on that tree:
  exit 0. PR #194 was opened from `3828ddd`.

## Landing order (blocks two other lanes)

The guard was sized against `origin/main` `b6ecd50`. Two concurrent lanes add
integration tests that mirror the helper the way `git_estimate_dag.rs` used
to. A mirror is seeded, so it follows the policy, but the guard refuses it.
Each branch passes check-fast alone; main goes red on
`every_property_routes_through_the_shared_helper` once this guard and either
of them are both merged. Counts from a read-only scan of the sibling
worktrees with this round's rules:

| Lane | File | Findings |
|---|---|---|
| s3-props-p21-p23 | `tests/s3_transfer_resume.rs` | 15 |
| s3-props-p21-p23 | `tests/s3_walk_resume.rs` | 10 |
| s2-shm-157 | `tests/sqlite_wal_index.rs` | 13 |

What each of those files needs before the second merge:

- delete the local `CI_SEED`, `DEEP` and `fn prop_config`, and the
  `use proptest::test_runner::{Config, RngSeed, ..};` line;
- add `#[path = "../src/test_support.rs"] mod test_support;`;
- write each block as `#![proptest_config(test_support::prop_config(N))]`;
- in `sqlite_wal_index.rs`, import `use proptest::test_runner::TestRunner;`
  and build the runner as `TestRunner::new(test_support::prop_config(12))`,
  with no other use of the `TestRunner` name (a return type counts).

The exemption ceilings are not the way out for them. Either order works:
they switch before merging, or this guard lands first and they merge main and
switch. Re-run check-fast on the merged result before the second merge.
This lane cannot write in their worktrees; the coordinator has to pass this
on.

## Open

- After L5 lands:
  - drop the `git_carry_v2.rs` entry;
  - migrate `refusal_taxonomy.rs` onto the helper (its seed
    `0x5733_7265_6675_7365` becomes `CI_SEED`) and drop its entry;
  - lower `EXEMPT_CEILING` and `FINDINGS_CEILING` to match.
- The plan's P66 row edit conflicts with L5 (rewrites P59 and P66). Whoever
  merges second resolves it by hand. The L6a conflict (P67 below P66) is
  resolved on this branch.
- The guard is text based and reads one line at a time.
  - It refuses valid helper-routed code in another layout: a config split
    over two lines, a braced helper import, `TestRunner` as a type in a
    signature, a refused word in a trailing comment.
  - It does not see a config built through a macro or an alias that hides
    every refused spelling (a renamed `proptest!`, `fn r#prop_config`), a block comment (`/* */`) that hides a pattern,
    or code pulled in by `include!` from a non-`.rs` file.
- Exemptions are pinned by count, not by content, and a missing exempt file
  is tolerated. The ceilings are literals in the guard's own file, so they
  bind by review only.
- The helper ends with `..Config::default()`, which reads `PROPTEST_*`
  environment variables for the fields it does not set. Nothing asserts that
  `BULKLOAD_PROPTEST_DEEP` is unset in CI.
- `tests/refs_scale_distinct.rs` still mirrors `test_support::DEEP` as a local
  constant. The guard does not look for that.
- The plan's inventory row still says "Existing `proptest!` blocks: 3".
- The guard does not refuse a stray `proptest-regressions/` directory.
