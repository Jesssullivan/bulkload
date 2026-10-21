# 2026-10-21: streamed capture B2, part 2

Lane: SPEC rule 2 (reset 2026-10-20). The coordinator's part 2 order:
1. the three-way union;
2. a different branch on each side ("switch when clean");
3. nested repositories and submodules.

Branch `feat/streamed-capture-b2p2-20261021`, stacked on #303. Edited on
sting; targeted tests on honey. R-N13.

- **Part 1, confirmed.** A dirty checkout behind the source keeps its tip,
  and the source's goes to `refs/remotes/neo/NAME` and is listed. Test:
  `a_dirty_checkout_behind_keeps_its_tip_and_tracks_the_sources`.
- **Three-way union** (`consolidate::merge3`). In a Git worktree's
  consolidation, every file both sides hold differently is gathered (up
  to `IN_MEMORY_LIMIT`) and merged with `git merge-file -p`. It merges
  against the blob at the merge base of the two HEADs (computed after the
  ref union). The inputs are written apart in the swept scratch and
  removed.
  - The result is swapped in only when no hunk conflicts and the live file
    is still the one read (#297's `swap_in`); otherwise
    `HOMES_CONSOLIDATE_RACED`.
  - A conflicting hunk, or a file the merge base lacks, is held typed,
    with nothing written.
  - Test: `a_file_both_sides_changed_merges_three_way_or_is_held`.
- **A different branch on each side** (the `GloriousFlywheel` case).
  - If the destination's tree is clean and the ref union left the
    source's branch at the source's tip, the destination switches to it
    (`checkout`) and the source's dirty and untracked files land.
  - Otherwise the item is held, `GIT_DESTINATION_DIVERGED`, with
    `branches=held destination=... source=...` listed and nothing switched
    or landed.
  - Test: `a_different_branch_switches_when_clean_and_is_held_when_dirty`.
- **Nested repositories and submodules as their own items.**
  - The inventory walks into a repository's nested repositories and
    populated submodules. A submodule's worktree is planned as the item
    (`kind=submodule`).
  - The enclosing item excludes their paths and its `.git/modules`.
  - A submodule's own administration (`.git/modules/NAME`, outside its
    worktree) lands mapped, with its objects fetched into it and its index
    settled. Its gitfile is written as a host-path file.
  - Test: `nested_repositories_and_submodules_land_as_their_own_items`.
- **Hand-off (R0.5) for streamed landings.** A landing or consolidation
  now records the workspace ledger the hand-off reads
  (`converge::record_streamed`). Before this, B1's hand-off phase refused
  `HANDOFF_NOT_LANDED` and no B1 test checked it. The B1 test now asserts
  `phase=hand-off outcome=ok` and the `AGENTS.md` note.
- **Registry.** `merge-file` is now a read, and `checkout` (1) is argued.

Checks (honey, gate devShell): fmt is clean and all three clippy passes
are clean. `estate_move` 17, `estate_move_source_roots` 5,
`source_command_registry` 22; unit tests (stream, union, streamed,
consolidate, converge) 76.
