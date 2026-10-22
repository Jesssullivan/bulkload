# 2026-10-22: build output, own quarantines, just-in-time capture

Lane `feat/rebuildable-jit-20261022` (from main 31e1ee7). These are the
coordinator's three fixes after the estate-20261022 rerun, whose closure was
`unaccounted=68`, with `CAPTURE_DRIFTED:137` and `STATE_PAYLOAD_LEFT:21`;
3040 file holds, 2979 of them under `build/` and `.svelte-kit/`.

1. **SPEC rule 5.** The streamed capture also omits untracked `build/`,
   `dist/` and `.svelte-kit/` (`SPEC_REBUILDABLE_DIRECTORIES`), and
   untracked `bazel-*` links at any level. The untracked-only guard stays:
   a tracked directory is carried. The legacy capture keeps
   `REBUILDABLE_DIRECTORIES` alone, so its capture keys never move.
2. **Own quarantines only.** `fetch_into` records the quarantine paths that
   appear while its fetch runs. `own_quarantine_left` names only those, so
   another writer's in-flight `incoming-*` or `tmp_pack_*` is never this
   pass's payload.
3. **Drift.**
   - **Just in time.** A land or converge that refuses `CAPTURE_DRIFTED`
     after its 3 top-up rounds asks the source session for a fresh capture
     (`negotiate` `Ask::Capture`: the source half's own read-only capture,
     answered over the wire and never recorded on neo). It fetches the
     fresh capture's objects and lands or consolidates again, up to
     `JIT_ROUNDS` (2). Only then is the item typed `CAPTURE_DRIFTED`.
   - **Pruned reflog objects.** `union::reflogs` no longer writes a source
     reflog entry naming an object neither store holds (one a gc pruned on
     the source). That entry would make the live repository name a missing
     object forever.
   - **Unreachable count.** The `unreachable` count never refuses a capture:
     a store whose reflog names a pruned object fails `fsck` and is still
     carried.

Tests:
- `build_output_is_never_a_seat`
- `another_writers_quarantine_never_refuses_the_item`
- `a_reflog_entry_naming_a_pruned_object_is_never_unioned`
- `git_carry::preview::tests::a_capture_is_answered_with_the_items_snapshot`
