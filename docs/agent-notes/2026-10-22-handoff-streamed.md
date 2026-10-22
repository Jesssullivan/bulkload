# 2026-10-22: streamed hand-off, rebuildable nests, held seats in the closure

Lane `fix/handoff-streamed-20261022` (from main 370ecb8). Three defects
from estate pass 4 (`estate-20261022`, agents 86146fc):

1. **The hand-off block named a corpus that does not exist.** A streamed
   pass keeps one snapshot per item in `MOVE_STATE/carry/`, never a
   corpus, yet its `CLAUDE.local.md` blocks named
   `carry/corpus` and gave `estate-diff` and `estate-handoff` commands over
   it (estate-diff over a missing corpus answers `CAPTURE_ABSENT` for
   every item). The GloriousFlywheel seat on sting asked (TIN-6569).
   - `estate-handoff --move-state MOVE_STATE SOURCE` reads
     `carry/plan.postcard`, `carry/corpus` and `private` under it.
   - A streamed pass (those paths, no corpus) renders a `Move state` row
     (with the binding) instead of `Corpus`, says `snapshot`, and gives
     `--dry-run` (differences, read-only), `--check` and `--clear`, all
     with `--move-state`. A corpus pass renders as before.
   - The streamed tag enters the inputs digest, so the stale blocks
     re-render on the next pass; a corpus pass's digest is unchanged.
2. **One read-only workspace ended the hand-off phase** (`refused:IO`,
   errno 13): `tinyland-mail/.build/checkouts/swift-argument-parser`, a
   `SwiftPM` dependency clone.
   - `.build` joins `SPEC_REBUILDABLE_DIRECTORIES` (untracked only).
   - The source inventory never plans a nested repository under a
     rebuildable root (`git_carry::under_rebuildable_root`, the streamed
     capture's own rule): a `rebuildable-nest` note, counted as
     `rebuildable_nests=` on the inventory line, never `not_moved`.
   - The plan is append-only, so the hand-off skips a planned workspace
     under another planned checkout's rebuildable root (a `rebuildable`
     row, counted in the summary) instead of writing into it.
   - `estate-handoff --keep-going` (passed by both `estate-move` hand-off
     phases): a workspace's refusal is its row, with `errno` for `IO`,
     never the run's.
3. **Held seats were not in the closure.** A consolidating landing's
   held seats were stderr lines only.
   - Every recorded transfer session that ends writes
     `<session>.held` beside its record: each held path and why.
   - `estate-move`'s closure names, on each item's own row, the refused
     entries of its seat transfer (`MOVE_STATE/pull/item-<blake3(live)>`)
     as `held_seats: [{path, refusal, errno, reason}]`. Annotation only:
     no total or gate changes.

Tests:
- `estate_move::a_streamed_hand_off_names_its_move_state_and_its_commands_run`
- `estate_move::a_nest_under_a_rebuildable_root_is_never_an_item`
- `estate_move::a_file_both_sides_changed_merges_three_way_or_is_held` (held_seats)
- `estate_handoff::h50_keep_going_makes_a_workspace_refusal_its_row`
