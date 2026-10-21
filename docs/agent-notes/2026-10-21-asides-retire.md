# 2026-10-21: the real pass retires beside copies; no empty corpus refusal

Lane `feat/asides-retire-20261021` (from main b7caf85). The coordinator's
next work after #311. SPEC rule 3, verbatim: "There are no beside copies,
`.LABEL` names or numbered second copies. Duplicates already on sting are
consolidated into the live path and removed."

**(a) Asides are retired.** This is phase 3b of `estate-move`. For each item
this pass landed or consolidated with nothing held, every sibling
`<live>.neo-*`, `<live>.sting-*` or `<live>.LABEL` (the plan's `aside=`
lines) is handled in four steps:

1. It is read as a source on this host: its snapshot is taken, and its
   objects are fetched into the live store by the agent's own
   `upload-pack`.
2. It is consolidated through `streamed::converge` with the suffix as the
   source label:
   - refs are unioned, and diverged ones go to `refs/remotes/SUFFIX/NAME`;
   - stash and reflogs are joined;
   - files merge three-way against the merge base, through a local
     recorded copy whose ledgers live under `serve/` and `pull/`.
3. The live checkout's directory is synced.
4. The aside is removed. A linked aside's administration under
   `worktrees/` goes with it.

Any of these is a typed hold, and the aside is kept:
- a different branch on a dirty live tree (`GIT_DESTINATION_DIVERGED`);
- a held file (`HOMES_CONSOLIDATE_UNSUPPORTED`);
- an unrelated or non-Git aside, or a main whose administration other
  worktrees still use (`GIT_DESTINATION_OCCUPIED`).

Each aside gets an `estate-move item=… aside=… outcome=retired|held code=…`
line.

**(b) No corpus.** No current code creates `carry/corpus`. The ones found
on neo (`homes-20261021-source/carry/corpus`, made by the 1ebed7f source
half's `estate-capture`) and the destination's `pull/corpus` (that
version's pull name) are older passes' leftovers. `refuse_retired` now
removes an empty retired directory (`rmdir`, so only when it is empty)
instead of refusing `STATE_FORMAT_RETIRED`. A non-empty one is still named
and refused.

Tests (honey, targeted):
- `a_copy_beside_the_live_path_is_consolidated_and_removed`: a `.neo-*`
  clone with its own branch, a diverged main and a mergeable edit is
  retired. A `.sting-*` clone that conflicts is held and kept.
- `no_pass_leaves_an_empty_corpus_and_an_old_one_is_removed`.
- `state_payload::an_empty_retired_directory_is_removed_not_refused`.
