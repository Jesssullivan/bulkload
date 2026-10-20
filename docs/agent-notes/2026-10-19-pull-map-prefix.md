# 2026-10-19: `pull --map-prefix`, the Claude key map, and one copy of neo's bytes

Lane: #280 "finding 9" (no duplicate bits), the next lane its note specified
(`docs/agent-notes/2026-10-19-pull-beside-live-home.md`, "Finding 9").
Operator rulings served: "no duplicates", "nothing excluded", "nothing
overwritten" (2026-10-19); C8/C9 (no transcript removed or linked). Branch
`feat/pull-map-prefix-20261019` from #280's head `a1c1cc1`, stacked on #280
(PR base `feat/pull-beside-live-home-20261019` until #280 merges, then
`main`). Edited on sting; every build and test ran on honey in this lane's
own directories. R-N13.

## The gap

Claude Code lists a session under the key of `realpath(cwd)` (every byte
outside `[A-Za-z0-9]` becomes `-`). neo's keys (`-Users-jess-git-<repo>`)
are not sting's (`-srv-fast-local-jess-git-<repo>`). Through #280 the pull
landed neo's `projects/<neo key>/` in sting's live `~/.claude`, and the
re-seat copied each re-keyed transcript to `projects/<sting key>/`: every
re-keyed transcript twice. An A-106 replacement also left neo's
`<id>.jsonl.LABEL` beside the copy it placed at the live name.

## What was built

1. `pull`/`copy --map-prefix SRC_REL=DST_REL` (repeatable; `transfer::map`).
   A receive-side rename, the wire unchanged: an entry at `SRC_REL` or under
   `SRC_REL/` (whole components, longest source wins) is decided, keyed
   (`row_key` over the mapped row), recorded and published at `DST_REL` plus
   the rest of its path. Owner rule: a destination path lands only through
   the map whose destination is the deepest holding it; any other route (the
   source holds the destination's name itself, or a shorter map's image
   inside a longer map's destination) is refused `DESTINATION_OCCUPIED` with
   everything under it, whatever the walk order, so nothing merges and no
   two seats meet at one name. Refused `FIELD_DOMAIN_VIOLATION` before
   either store exists: a side that is not a relative path of whole
   components, one source or one destination twice, a destination whose
   parent is not where its source's parent lands. The transfer record gains
   one `mapped` entry per map used (source path, destination path),
   accounted like `applied` in closure (`"mapped":` in the row). Stats line
   `mapped=`, stderr `mapped SRC at=DST`. The capture-refusal check now
   compares the wire path mapped and redirected (it also fixes the same
   compare for an entry under a `--beside` directory redirect).
2. `estate-move --sessions` derives the Claude key map from the re-seat's
   own lanes (`reseat::keymap::derive`: the run's `--map` pairs, the plan's
   items, `SOURCE_HOME/git`; the destination resolved to its real path as
   `Lanes::lane` does and keyed by `reseat::key::project_key`, Claude Code
   2.1.299's function) and pulls the Claude tree with
   `projects/<key(S)>=projects/<key(D)>`. Codex is pulled without maps
   (rollouts are kept by date; the `cwd` respell stays A-055's).
   `estate-move key-map` and `key-maps mapped= held= same=` lines; closure
   line `key_maps= key_maps_held=`.
3. Keys more than one lane source keys to (`ambiguous-source-key`), keys
   whose destination key another source key also maps to
   (`dest-key-collision`) and lanes that do not resolve (`lane-unresolved`)
   stay unmapped, typed on `estate-move key-map-held` lines; the re-seat
   resolves their sessions by each transcript's `cwd` as before.
4. A mapped session lands once, where Claude Code lists it; the re-seat finds
   it `in-place`. The closure names each source key with its destination
   key (the `mapped` rows).
5. `estate-move --key-map-proof` (report only; `reseat::keymap::proof`):
   from the plan an earlier pass pulled, the same key map, then one JSON line
   per file under each source-keyed directory of the live
   `DEST_HOME/.claude/projects` with its destination key (the map's, else
   its transcript's lane by `cwd`, else the one lane key the directory's
   transcripts agree on) and how that key holds it: `identical`,
   `prefix-extended`, `identical-beside`, `prefix-extended-beside`,
   `differs`, `absent`, `dest-link`, `dest-other`, `in-place`,
   `unresolved`; then `estate-move key-map-proof dirs= files= statuses=`.
   Nothing is pulled, locked or written. The source-key directories stay.
6. A-106 `.LABEL`: on a live-home landing, when the placer replaces an
   archive link with the source's copy the pull landed beside it in the same
   directory, and that copy's whole bytes were placed and it is unchanged,
   the beside copy is removed (counter `reseat_beside_consumed`): one copy
   of neo's bytes. `estate-reseat --undo` puts the copy back at the beside
   name (or keeps it aside) instead of removing it. Claude and Codex.

Found while testing, and fixed here, because the map alone would have made
the first mapped pass on sting worse for sessions sting resumed:

7. `dest-ahead` (`pull --beside`): a settled append-only live file this
   store does not own that holds the seat's bytes and more (sting continued
   a neo session) is left as it is and recorded `dest-ahead` (closure:
   covered), remembered under its identity (`RefusedOutput::DestAhead`, tag
   5), so an unchanged rerun answers at `Decide` and reads nothing. Without
   it the map would land neo's bytes beside the resumed transcript as a
   third copy.
8. A-103 under `--beside`: a live transcript smaller than its first chunk
   that the source grew is extended in place (its head judged whole
   against the staged first chunk at the end; one whose head differs lands
   whole beside, as a false prefix did). Before, the proof needed one whole
   chunk, so every small grown transcript landed beside (pre-existing in
   #280; it would have made the map's first pass copy them beside).
9. The re-seat keeps a beside copy under its lane's destination key, beside
   the destination's own regular transcript, as `diverged` (accounted,
   nothing written; both sides continued), and resolves such a copy's lane
   by the key Claude Code lists it under. Without it the copy was
   `RESEAT_LANE_UNRESOLVED`, or a `collision` with the placement row of the
   neo-key landing, and held the gate.

## Tests

- `transfer::map::tests` (2): whole-component matching, longest source
  wins, the owner rule; refused sets.
- `transfer::tests::map_prefix` (6): a mapped key lands once under its
  destination and the rerun reads 0; a growing transcript is extended in
  place under the mapped key (twice) and the rerun reads 0; a small grown
  transcript is extended and a small fork lands beside; a live file that
  holds the seat and more is `dest-ahead`, nothing lands, rerun reads 0
  (both continued: beside); a source path at a mapped destination is refused
  with its entries in both walk orders, nothing merges; an invalid set
  refuses before either store exists.
- `closure::transfer_tests::a_mapped_entry_is_accounted_like_applied_and_names_both_paths`.
- `reseat::keymap::tests` (2): a shared key (`a.b`/`a-b`) and a destination
  collision stay unmapped and typed, the default lane resolves through a
  symlinked `~/git`; the proof's statuses.
- `tests/estate_move.rs::sessions_land_once_under_the_mapped_key_and_a_shared_key_stays_typed`:
  end to end through the binary: no source-key directory, the re-seat in
  place, the shared key held and re-seated as before, a resumed session
  `dest-ahead`, a both-continued one beside and `diverged`, the closure's
  `mapped` and `dest-ahead` rows, the transcript growing on the source
  extended in place under the mapped key, an unchanged pass whose Claude
  transfer reads 0, and `--key-map-proof`.
- Updated for the map: `sessions_land_in_the_live_home_beside_and_the_gate_reads_them`
  (lands under the lane key, no neo-key directory, the `mapped` closure
  row) and `archive_links_follow_a106_and_destination_links_stay` (the
  `.LABEL` consumed for the Claude transcript and the Codex rollout, nothing
  under the neo key).

## Receipt

RECEIPT_PLACEHOLDER

## Open

- Source-key directories already on sting stay (removing a transcript is
  forbidden); `--key-map-proof` is the operator's evidence for a later
  ruling.
- Keys from older Claude Code versions (dots kept, e.g.
  `-Users-jess-git-GloriousFlywheel-.claude-worktrees-*`) and sessions whose
  `cwd` is not a lane source exactly (subdirectories) are not mapped; the
  re-seat re-keys them by `cwd` as before (a second copy).
- A key two lane sources share is held, not mapped (strict: even when both
  would land at one destination key).
- A beside output the pull left in an earlier pass (a racy destination, or
  before this lane) is not converged back into the live path (#280's C12).
- `just darwin-check` on pzm is owed before merge.
