# Home mapping preserves the plan identity

Authority: SPEC Done 2, SPEC rules 4 and 8, R-N13 and the operator's
2026-10-24 instruction to complete the resumable neo-to-sting estate.
Lexical: XL-ACC-004.

The estate closure at
`/home/jess/.local/state/bulkload/move/estate-20261022/closure.json`
records 83 unaccounted items. Its 68 unaccounted blahaj items have
`no-outcome-record`. The pass output nevertheless names their capture holds.

The source receipt
`/srv/data/jess/fast-local-relocated/bulkload/receipts/estate-20261022.source9.out`
records `GIT_HAVES_UNPROVABLE` for all 99 items whose planned repository is
`/srv/fast-local/jess/git/blahaj`. The capture guard refuses a source with
a shallow frontier. This is separate from closure accounting.

The home map changes workspaces under `/Users/jess/.cache` to
`/home/jess/.cache`. The decision retains its original item ID. Outcome
and success-journal writers recomputed an ID from the changed placement.
Closure reads the original plan, so those records became foreign records.

`estate_move.rs` now uses the original planned item for outcome records
and success journals. The mapped live paths still select the destination.
Typed capture refusals remain holds. This change does not migrate a
shallow repository or accept any hold.

The regression covers a refused main repository and a linked worktree
outside the explicit map. Two passes must account for both typed holds
without writing live paths. The existing successful home-map regression
now requires two applied items, zero unaccounted items and one registration
per worktree after a second pass.

Mutation receipt: actor=closure_diagnosis | target=owned branch
`codex/closure-refusals-20261024` | reason=preserve plan identity through
home mapping | ruling=SPEC Done 2, rules 4 and 8, R-N13, operator
2026-10-24 | prior_state=original IDs read, remapped IDs written |
result=original IDs written for outcomes and success journals.

Validation: `git diff --check` passes. No build or test ran on sting.
Targeted tests, formatting and lint still need honey receipts before review.

