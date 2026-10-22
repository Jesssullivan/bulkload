# 2026-10-21: what kept the operator's lanes from landing (pass 1, b7caf85)

Lane `feat/lanes-land-20261021` (from main e9e6c86). The coordinator listed
four causes from the first real `git-20261021` pass. That pass's closure:
`unaccounted=2 refused_pending_review=125`, with `CAPTURE_DRIFTED:108`,
120 files held as add/add, and one `HANDOFF_NOT_LANDED`.

1. **Nested items are never seats.** The streamed capture now excludes the
   registered worktrees nested in a checkout (`git_carry::nested_worktrees`).
   The destination also excludes every other plan item whose source lies
   under this item's source, registered or not. This was the 105
   `.worktrees/` holds in tinyland.dev.
2. **Add/add.** When the merge base has no such file
   (`merge3::NO_BASE`), equal bytes were already adopted. Now a class #297
   unions (append log, JSON or TOML config) is unioned instead of held.
   Anything else stays held.
3. **Drift top-up** (SPEC rule 6). `land` and `converge` fetch what the live
   administration names and the store lacks, by id from the live source,
   for up to `TOP_UPS` (3) rounds. Only then do they refuse
   `CAPTURE_DRIFTED`.
4. **The hand-off reads placed paths.** `estate-handoff --place FROM=TO`
   rewrites the plan's workspace paths; estate-move passes the default home
   map. This was gf-seat-build's `HANDOFF_NOT_LANDED`.

Tests (honey, targeted):
- `nested_worktrees_are_never_the_mains_seats`
- `a_file_both_sides_added_is_adopted_unioned_or_held`
- `an_object_named_after_the_snapshot_is_fetched_in_the_same_pass`
- the home-map test now asserts `phase=hand-off outcome=ok`

5. **The hand-off never modifies a tracked file** (SPEC rules 7 and 8).
   - The note goes to `CLAUDE.local.md` at the workspace root. It is created
     holding the block alone, and a rerun replaces only the block. It is
     added to the repo's shared `info/exclude` (`/CLAUDE.local.md`), so Git
     never shows it.
   - `AGENTS.md` and `CLAUDE.md` are never written. estate-move first runs
     `estate-handoff --clear --legacy-only`. Wherever a recorded hand-off
     block from passes 1 and 2 sits in a tracked file, that recorded clear
     removes exactly the block (by its record), restores the bytes, and
     ledgers the removal. It runs before landing, so the block never holds
     the consolidation of its own file.
   - Codex gets no file. Its hand-off is the hand-off ledger and the
     summary line.
   - The prefix machinery's fixtures (`estate_handoff` H-rows, `estate_adopt`
     p117 and p122, the lib re-dating rows) still run over the legacy
     targets. They set `BULKLOAD_HANDOFF_TRACKED_TARGETS`, which only a
     debug build reads: a release agent writes `CLAUDE.local.md` alone.
   - New test: `the_hand_off_never_modifies_a_tracked_file`.
