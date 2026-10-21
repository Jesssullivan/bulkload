# 2026-10-21: streamed capture B2, part 1 (consolidate a related live repository)

Lane: SPEC rule 2 (reset 2026-10-20). Coordinator decisions:
- (a) seats land as files;
- the refs rule of 2026-10-21;
- index compared by entries (rule 8).

Branch `feat/streamed-capture-b2-20261021`, from #302 merged with main
(merge commits only). Edited on sting; targeted tests on honey. R-N13.

## What part 1 does

- **Selection.** An item whose live path holds a repository related to
  the source is consolidated, never replaced or landed beside. Related
  means the destination holds one of the source's root commits; the
  snapshot carries `history.roots`. An unrelated repository or tree is
  held, `GIT_DESTINATION_OCCUPIED`, with nothing written. A consolidated
  item is marked (`private/ITEM.converged`) and is consolidated again on
  every pass, never landed over.
- **Objects.** Fetched by id into the destination's own store, as B1 does.
  The store only gains the repository's real content.
- **Refs union** (`git_carry::union::refs`, compare-and-swap):
  - a name only the source has is created;
  - an ancestor is fast-forwarded to its descendant;
  - a branch checked out in a clean tree is advanced with its checkout
    (`merge --ff-only`, "switch when clean"); one checked out in a dirty
    tree keeps the destination's tip;
  - a name that diverged keeps the destination's tip at its name, and the
    source's goes in as a remote-tracking ref, `refs/remotes/SOURCE/NAME`
    (other namespaces as `refs/remotes/SOURCE/<ns>/NAME`). Each tracked
    ref is printed `ref=NAME union=tracked at=...`.
  - No merge commits.
- **Stash and reflogs.** The source's stash entries join the
  destination's (`stash store`, newest on top). Its reflog entries are
  appended to the destination's reflog of the same ref, or of the
  tracking ref, so every source id stays reachable. Counts are on the
  receipt.
- **Seats.** Through the engine with #297's unions, the destination's
  administration excluded: equal files adopted, new files created, the
  destination's own files kept. A file both sides hold differently is held
  typed (`HOMES_CONSOLIDATE_UNSUPPORTED`), nothing written, and the item's
  outcome is `held`.
- **Index (rule 8).** It is never landed by the engine.
  - `settle_index` compares by entries (`ls-files -s`; stat data and
    extensions ignored).
  - The source's index is written when the live index is absent, holds the
    entries a landing last wrote, or holds the source's own.
  - An index holding the destination's own staging is kept and reported
    `held`.
  - A destination `git status` is no change: the B1 rerun test now runs a
    real `git status` before rerunning, and the rerun reports `index=same`.
- **The registry argues the new destination writers.** `update-ref` goes
  to 10, and `merge` (1) and `stash` (1) are added.

## Tests (honey, gate devShell)

fmt is clean, and all three clippy passes are clean with `-D warnings`.

- `tests/estate_move.rs`: 13 passed, none ignored. New:
  `a_related_live_repository_is_consolidated_and_refs_union_by_name`
  - created, switched when clean, tracked when diverged;
  - stash joined;
  - the destination's own files kept, new seats created, fsck clean, no
    payload;
  - then a seat both sides changed is held typed, the destination's bytes
    kept.

  The B1 test re-runs after a real `git status` with `index=same`.
- `tests/estate_move_source_roots.rs`: 5. `source_command_registry`: 22.
- Unit: `git_carry::stream`, `git_carry::union`, `estate::streamed` (7).

## Open (B2 part 2)

- The file-by-file three-way merge against the merge base, for a seat
  both sides changed: it needs a Git-aware union (the merge base and the
  source's blob) in #297's dispatch. Until then such seats are held
  typed, as above.
- Nested repositories and submodules as their own items.
