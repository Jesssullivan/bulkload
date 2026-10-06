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
- `3828ddd`: merge of `origin/main` `8e23b1d` (#191, L6a).
- `8c03ac3`: merge of `origin/main` `48bd697` (#192, the S3 properties).
- `615e373`: the two S3 test files use the shared helper.

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

## #192 landed first: merged and switched (2026-10-06)

#192 (`feat/s3-properties-20261006`) merged to main as `48bd697` after
PR #194 was opened. It added `tests/s3_transfer_resume.rs` (15 findings) and
`tests/s3_walk_resume.rs` (10), each with a mirrored helper, so #194 merged
with main failed `every_property_routes_through_the_shared_helper`. The
coordinator authorised this lane to edit the two files (their lane has
merged).

- `8c03ac3`: merge of `origin/main` `48bd697`. No text conflict; the plan's
  rows from both sides are kept. The guard is red on this commit alone.
- `615e373`: both files compile `src/test_support.rs` as a `#[path]` module
  and write each block as `#![proptest_config(test_support::prop_config(N))]`
  with the case counts they had (walk: 16; transfer: 8, 4, 3, 6, 3, 4, 6, 4).
  The local `CI_SEED`, `DEEP`, `fn prop_config` and the braced `test_runner`
  import are gone. No strategy, assertion, `#[ignore]` or test name changed.
- **Behaviour change in the deep tier.** The mirrors kept `CI_SEED` under
  `BULKLOAD_PROPTEST_DEEP=1` on purpose. The shared helper uses
  `RngSeed::Random` there, so the S3 properties now draw random seeds in the
  deep tier, with twenty times the cases as before. CI is unchanged. The plan
  rows for P18, P19, P21 and P23 and the two files' module docs now say this.
  Whether the deep tier should keep a fixed seed for these properties is an
  operator question; it would be a change to the helper, not a mirror.
- `docs/agent-notes/2026-10-06-s3-props-p21-p23.md` (the other lane's note)
  still describes the fixed-seed mirror. This lane did not edit it.

Receipts (CI toolchain, `nix develop .#default`):

- `cargo test -p bulkload-agent --test s3_walk_resume --test
  s3_transfer_resume --test prop_seed_guard`: guard 6 passed,
  `scanned=87 exempt_present=2 escapes=0`; transfer 12 passed, 4 ignored
  (the same four as on main); walk 2 passed.
- `just check-fast` on the `615e373` tree, under the shared lock: exit 0.

## Second recheck (2026-10-06, head `73305db`)

A separate recheck stage read the diff since `b91d9d2` (the merge `8c03ac3`,
the switch `615e373`, the note `73305db`). Verdict: clean; PR #194's body is
updated in place and nothing is merged.

- The medium finding (the two S3 files mirrored the helper, 25 guard findings
  once main was merged) is fixed. `origin/main` is still `48bd697` and is an
  ancestor of the branch.
- Re-run on `73305db`: `cargo test -p bulkload-agent --test prop_seed_guard
  --test s3_walk_resume --test s3_transfer_resume`: guard 6 passed,
  `scanned=87 exempt_present=2 escapes=0`; transfer 12 passed, 4 ignored;
  walk 2 passed. `cargo fmt --check` and `just contract-test` (24 tests) pass.
- `615e373` changes only the config lines, the removed mirror and the module
  docs in the two S3 files. The commits since `b91d9d2` are signed and carry
  the rulings trailer.
- check-fast was not run again: its exit 0 covers the `615e373` tree, and
  the commits after it change only this note.
- `tests/sqlite_wal_index.rs` is still not on `origin/main`; the conversion
  below stays with the later lander.

## Landing order (one lane still to convert)

`feat/s2-shm-counter-20261006` adds `tests/sqlite_wal_index.rs` with a
mirrored helper (13 findings). It is not on `origin/main` (`48bd697`), so it
is not converted here. Whichever of that branch and #194 lands second must
merge main, convert the file and re-run check-fast:

- delete the local `CI_SEED`, `DEEP` and `fn prop_config`, and the
  `use proptest::test_runner::{Config, RngSeed, ..};` line;
- add `#[path = "../src/test_support.rs"] mod test_support;`;
- import `use proptest::test_runner::TestRunner;` and build the runner as
  `TestRunner::new(test_support::prop_config(12))`, with no other use of the
  `TestRunner` name (a return type counts).

The exemption ceilings are not the way out. This lane cannot write in that
worktree; the coordinator has to pass this on.

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
