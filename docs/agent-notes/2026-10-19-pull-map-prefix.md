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
   `estate-move key-map` lines and a `key-maps` summary line (mapped, held
   and same counts); the closure line gains two fields: the count of key
   maps, and the count of keys held.
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
9. The re-seat resolves a beside copy under its lane's destination key by
   the key Claude Code lists it under. It judges the copy against the
   destination's own regular transcript by bytes (review 1 below; this note
   first said `diverged`, accounted, which was assumed rather than checked).
   Without this, the copy was `RESEAT_LANE_UNRESOLVED`, or a `collision`
   with the placement row of the neo-key landing.

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

Host `honey`, the code of `8ce9d42` (the pushed feature commit; the worktree
`~/git-build/pull-map-prefix-20261019` is `a1c1cc1` with the lane's files,
byte-identical to `8ce9d42`'s tree). `CARGO_HOME` and `CARGO_TARGET_DIR`
under `~/git-build/pull-map-prefix-20261019-cargo/`,
`TMPDIR=/var/tmp/jess-bulkload-tests/pull-map-prefix-20261019`, every
command `nice -n 19 nix develop --no-write-lock-file --ignore-environment
.#default`, `CARGO_BUILD_JOBS=4`, `RUST_TEST_THREADS=4`, debuginfo off.
Another session's merge gate held honey throughout, so this lane ran
targeted filters only, one at a time.

- `cargo fmt --all -- --check`: ok. `cargo clippy -p bulkload-agent -p
  bulkload-proto --all-targets --locked -- -D warnings`: ok.
- Lib (`--lib`): `transfer::tests::map_prefix` 6/6, `transfer::map` 2/2,
  `reseat::keymap` 2/2, `closure` 38/38, `transfer_outcome` 8/8,
  `reseat::` 77/77, `transfer::tests::beside` 14/14,
  `transfer::tests::prefix` 6/6.
- Integration: `estate_move` 5 tests, the new
  `sessions_land_once_under_the_mapped_key_and_a_shared_key_stays_typed`
  and the two updated session tests pass;
  `one_pass_lands_adopts_besides_carries_and_accounts_for_every_item`
  failed 2 of 3 runs at its `beside-converged=1` assertion (line 391) and
  passed once. That is the listed honey load flake: a git beside-converge
  path this lane does not touch, failing at the same line in #280's own
  gate log at `a1c1cc1`
  (`~/.local/state/bulkload/local-gate/a1c1cc182a35-2026-10-19T200242Z.log`).
  `estate_reseat` 6/6, `prefix_extend_cli` 2/2, `estate_carried` 12/12,
  `space_closure_cli` 10/10, `sqlite_carry` 3/3, `source_command_registry`
  22/22, `estate_adopt` 40/40.
- Fault harness (`--features fault-injection,io-trace --test
  fault_harness`): `reseat` 23/23, `estate_converge` 23/23, `beside`
  10/10, `sqlite_supersede` 3/3, `prefix_` 9/9, `receive_after` 6/6.
- Not run here: the whole `cargo test -p bulkload-agent`, `just
  rust-check` and the merge gate (`just local-gate-receipt --pr N
  --post`), owed once honey is free; `just darwin-check` on pzm, owed
  before merge.
- Test scratch under `TMPDIR` removed after the runs.

## Review of #294 (2026-10-20, at `fdc8589`) and the fixes

The coordinator's independent review (PR comment) found the items below.
They are fixed on this branch in new commits, after a signed merge of
#280's head `182496f` (no force-push).

0. **Merge of #280 (`182496f`).** Five files conflicted. Resolved as the
   review describes:
   - `sessions_pull` returns `(Result, Option<Homes>, key maps)`. The key map
     is derived for #280's resolved, owner-checked `.claude`, and the proof
     resolves that home read-only (`owned_real`).
   - `Inbound` keeps both the `maps` and `listing` fields. #280's `--file`
     `land()` runs before the map at every frame. The source listing is
     recorded after mapping, so it holds the paths as they land.
   - One option parser takes `--file`, `--beside` and `--map-prefix` in any
     order.
   - Both placer builder fields are kept (`beside`, `kept_suffix`). A copy
     kept aside on an A-106 swap-back, whose `<name><suffix>` is taken by an
     unconsumed beside landing, falls to `<name><suffix>.1` (documented on
     `Placer::kept_suffix`).
1. **(must) `diverged` was assumed, not checked.** The pull also lands
   files beside for reasons other than content: an empty live file, other
   links, another owner or mode, the extension off. The re-seat now judges
   the beside copy against sting's own regular transcript by their bytes
   (`place::judge_beside`):
   - **Live file is a strict prefix of the copy.** It is extended in place
     by the copy's whole lines (`O_APPEND`, size checked before each piece)
     when that is safe: one link, this user's, the copy's mode, quiet past
     the racy window, ending at a line's end, extension on. The copy is
     removed once the live file holds it whole. Otherwise the session is
     held, typed (`beside-held`, with its reason).
   - **The copy is a prefix of the live file.** `covered`, accounted. The
     copy is removed when the live file is longer, because a later pull
     finds that file `dest-ahead`.
   - **Neither is a prefix of the other.** `beside-diverged`
     (`RESEAT_DEST_DIVERGED`). It is not accounted: it holds the gate,
     counts in the closure line's new `diverged=`, and the operator decides.
   - Tests: `reseat::place::judge_tests` (2), and the end-to-end tests: an
     empty live transcript is extended and never landed beside again;
     `a_session_both_sides_continued_is_held_diverged_for_review`.
2. **(must) A live file written within 2 s became a permanent beside
   duplicate.** Under `--beside`, an append-only live file written within
   the racy window is now refused `DESTINATION_UNSETTLED` (new code).
   - Typed and not remembered: nothing lands beside, and the next pass
     decides.
   - It applies to the manifest path and the streamed path alike.
   - Test: `a_live_file_written_now_is_unsettled_and_the_next_pass_decides`.
3. **Same-key lanes were not counted in the collision check.**
   - Keys that land as they are now take their name at the destination:
     same-key lanes, and keys held as ambiguous or unresolved. A map onto
     one of them is held `dest-key-collision`, and holding repeats until
     nothing more is held.
   - The owner rule's refusal has its own code,
     `DESTINATION_MAP_CONFLICT` (new).
   - Test: `a_map_onto_a_key_that_lands_as_it_is_is_a_collision`.
4. **A derived map its sessions did not use was invisible.** After the
   Claude pull, `keymap::unused` reads the listing and prints a typed
   `estate-move key-map-unused from= to= seen= cwd=` line for each
   source-keyed directory that no map moved and that belongs to a mapped
   lane: by name but for case, or by its transcript's `cwd`. These
   directories are the second copies. The closure line gains
   `key_maps_unused=`.
   - Test: `a_map_its_sessions_did_not_use_is_named`.
   - Gap: a source path reached through a symlink keys by its target,
     whose lane is another one, so it is not found.

## Verification of #294 (2026-10-20, at `6e3ae3a`) and the fixes

The coordinator's verification held items 2 to 4 and the merge, and found
one defect and two hardenings in `place::judge_beside`:

1. **(must) A partial extension counted as `extended`.** The append loop
   read the copy in 1 MiB pieces and stopped at a piece with no line end.
   It reported `extended` (accounted) with part of the copy appended, and
   the next pass was held under the wrong reason (`live-mid-line`). Probes:
   a 2 MiB single line got 16 of 2,097,187 bytes; a copy ending mid-line
   got 16 of 21.
   - Now the extension aims at the copy's last line end
     (`last_line_end`). Each piece is a mebibyte ending at its last line
     end, or is read on to the next line end, one record however long
     (`line_piece`, as the pull's `record_piece` does).
   - `extended` only when every byte of the copy was appended, the copy
     ending at a line end; an append cut short is held `live-raced`.
   - A copy with bytes past its last line end that the live file lacks
     (neo still writing that line) is held `copy-mid-line`
     (`RESEAT_EXTEND_RACED`, retryable) on every pass. The pass that
     appends its whole lines holds too, and keeps the copy, so the gate
     never passes once on incomplete content (re-verification at
     `1d8cdb3`). It clears when neo finishes the line and the next pull
     carries it.
2. **(should) The appended range is read back and hashed** against the
   bytes written before the copy may be removed, not checked by size only.
3. **(should) A quiet but open session is never written into.** Before
   extending, any process holding the live transcript open, or working in
   the lane's destination directory, holds the session `live-in-use`
   (`RESEAT_EXTEND_RACED`, not remembered, nothing written). The scan is
   `procs::inside`, added byte-identical to #289's `procs.rs` so the two
   merge cleanly. Where it cannot say (not Linux), `live-in-use-unknown`.
   A shell sitting in the lane's checkout counts too.
- `DESTINATION_UNSETTLED` is rerun-only (`closure::rerun_only`, like
  `TRANSPORT_LOST`): unaccounted, `destination-unsettled`, and neither a
  review row nor a standing policy closes it. Only a later pass that
  decides the path does.
- #280's head `fc0cb4e` (N4 sticky provenance, cumulative listing) is
  merged (`34c9038`; one conflict, both sides' appended tests kept). A
  source path the owner rule refuses (`DESTINATION_MAP_CONFLICT`) is
  listed at its own path, as #280 now lists every source path the walk
  refused, so a destination file of that name is never taken for the
  destination's own; a mapped one is listed where it lands. Test:
  `map_prefix::a_refused_second_route_is_listed_as_the_sources`.
- Tests: `judge_tests::whole_lines_are_appended_however_long_and_a_partial_one_is_held`,
  `judge_tests::a_session_working_in_the_lane_is_never_written_into`,
  `closure::transfer_tests::an_unsettled_destination_is_rerun_only`.

## Receipt after the verification (2026-10-20)

- Host `honey`. The worktree was overlaid with every file of the head that
  differs from `6e3ae3a`. Environment and limits as before.
- Targeted runs only, one at a time.
- fmt: ok. clippy (`-D warnings`, `bulkload-agent` and `bulkload-proto`,
  all targets): ok.
- proto: 18/18.
- Lib:

  | Filter | Passed |
  | --- | --- |
  | `map_prefix` | 8/8 |
  | `judge_tests` | 4/4 |
  | `procs` | 1/1 |
  | `keymap` | 4/4 |
  | `closure` | 39/39 |
  | `transfer_outcome` | 8/8 |
  | `reseat::` | 84/84 |
  | `transfer::tests::beside` | 15/15 |
  | `transfer::tests::prefix` | 6/6 |

- Integration, on the merged tree with #280's `fc0cb4e`:

  | Suite | Passed |
  | --- | --- |
  | `estate_move` | 7/7 (#280's new provenance test included) |
  | `estate_reseat` | 7/7 |
  | `single_file_copy` | 17/17 |
  | `estate_move_source_roots` | 6/6 |
  | `estate_carried` | 12/12 |
  | `source_command_registry` | 22/22 |
  | `prefix_extend_cli` | 2/2 |
  | `estate_adopt` | 40/40 |

- Fault harness: `reseat` 23/23, `beside` 10/10, `prefix_` 9/9,
  `receive_after` 6/6.
- One run of `reseat::` failed first:
  `existing_copies_are_adopted_extended_or_kept` measures the process-wide
  landed-read counter, and the new judge tests read landed bytes beside it.
  The judge tests now take that module's `SERIAL` lock, and the rerun
  passed 84/84.
- Still owed: the whole suite, `just rust-check`, the merge gate, and pzm
  `darwin-check`.

## Receipt after the review (2026-10-20, at `60d769d`)

- Host `honey`. Worktree `~/git-build/pull-map-prefix-20261019` is `fdc8589`
  overlaid with every file of `60d769d` that differs from `a1c1cc1`, so its
  tree is `60d769d`'s.
- Environment and limits as in the first receipt.
- A `local-gate` run (#278's) shared honey throughout, so only targeted
  filters ran, one at a time.
- `cargo fmt --all -- --check`: ok. `cargo clippy -p bulkload-agent -p
  bulkload-proto --all-targets --locked -- -D warnings`: ok.
- `bulkload-proto`: 18/18 (the code taxonomy test, with the two new codes).
- Lib:

  | Filter | Passed |
  | --- | --- |
  | `map_prefix` | 7/7 |
  | `keymap` | 4/4 |
  | `judge_tests` | 2/2 |
  | `transfer::map` | 2/2 |
  | `closure` | 38/38 |
  | `transfer_outcome` | 8/8 |
  | `reseat::` | 82/82 |
  | `transfer::tests::beside` | 15/15 |
  | `transfer::tests::prefix` | 6/6 |

- Integration:

  | Suite | Passed |
  | --- | --- |
  | `estate_move` | 6/6 (both new tests and #280's deflaked one-pass test included) |
  | `estate_reseat` | 7/7 |
  | `single_file_copy` | 17/17 |
  | `estate_move_source_roots` | 6/6 |
  | `estate_carried` | 12/12 |
  | `source_command_registry` | 22/22 |
  | `prefix_extend_cli` | 2/2 |
  | `estate_adopt` | 40/40 |

- Fault harness: `reseat` 23/23, `beside` 10/10, `prefix_` 9/9,
  `receive_after` 6/6, `estate_converge` 23/23.
- Still owed: the whole suite, `just rust-check`, the merge gate, and pzm
  `darwin-check`.

## Consolidation, not beside (operator ruling 2026-10-20; SPEC #298)

The ruling: "neither WIN. they must be consolidated"; "no dupes, no
duplicated besides, no stage dupes"; "a duplicate is inherently not path
for path / byte for byte". This lane owns the consolidation of append logs.

| Commit | What |
| --- | --- |
| `b8456c9` | Receive path: a `.jsonl` seat whose live file differs is merged into one file in place, never landed beside (`transfer::consolidate::merge_jsonl`). The result is the source's bytes, then the live file's records it lacks (Claude by `uuid`, other logs by exact line, counted). It replaces the live file through the superseding exchange; a file held open, mid-line or changed is refused `DESTINATION_UNSETTLED`, rerun only. The row ledgers its seat, live and output BLAKE3 and record counts. |
| `dc6f3f9` | An archive-link transcript is replaced by the union (A-106). The Codex index copy is retired after registration, or kept typed. `move_state_bytes` and `pull_payload_bytes` are on the closure line. No beside of content already present, for any class. |
| `1587da0`, `700ab6e` | A re-seat sets nothing aside. A swap-back copy that a writer appended to is held typed: `held-written`, `DESTINATION_OCCUPIED`, under its private `.<name>.<token>.bulkload-held`. Later passes are held the same way with nothing written. `reseat_kept_aside` is asserted to be 0. Before this round it counted only the A-106 hook test's copy, and no real pass ever reached it. |
| `5300494` | Merged #297 at `7db6b11` (clean). Merge order is #280, then #297, then #294; #297's later heads are merged in, never rebased. |
| `ea656d9` | `consolidate::append_log::union` is #297's `Union` for `Class::AppendLog`, and `union_of` returns it. Adopt, cover (byte prefix or every record), extend, or consolidate. Written through `config::swap_in`. Held open or mid-line is `HOMES_CONSOLIDATE_RACED`. The `append_log_union` ledger table records input and output BLAKE3 and counts. |
| `2e5fa2b` | A `.jsonl` whose live path is a directory, or a link outside the archive roots, is refused `DESTINATION_KIND_MISMATCH` with nothing beside it. A link into a `--replace-archived-dest` root lands for the A-106 union. |
| `e96eea0` | `append_log::plan_at` is the append-log row of #297's `--plan-only` listing. The would-write listing is that row, with no separate listing. |

Tests added: a diverged same-ID transcript (both branches, rerun covered,
growth on either host unioned again); `history.jsonl` by line; a
concurrent append mid-union, never lost; held open or mid-line, untouched;
a link never followed; other kinds refused; held-written on two passes
with the counter at 0.

### Corrections (SPEC rules 3, 4 and 7): `60b9542`, `3e4ea5c`

- `60b9542`: one union. The receive side calls
  `consolidate::append_log::union` and acts on its outcome.
  - Covered (a live log holding every record, in any order) is
    `dest-ahead` and is never rewritten.
  - Extended or consolidated: the union's swap trades the merged file in.
  - A refusal is `DESTINATION_UNSETTLED`, rerun only.
  - The entry is answered without a store publication and remembered
    under the live identity.
- `3e4ea5c`: no hidden copies. `reseat::fold` unions a race's leftover
  copy (the A-106 swap-back copy a writer appended to, every Q198
  regularize copy) into the live file, ledgers it, and removes it once
  durable.
  - Only a refusing union holds. The copy is renamed to its `kept-<inode>`
    name (the same file, because the temporary name is cleared every
    pass), and every later pass retries it first.
  - `held-written` and `kept-held` hold the path meanwhile.
  - `.bulkload-held` is gone, and so is `reseat_kept_aside` (the
    counter-coverage test forbids a counter nothing increments; the a106
    test asserts no copy instead);
    `reseat_copy_folded` counts the folds.
- Honey, targeted: fmt and clippy ok; `reseat::` 84/84, `consolidate`
  36/36, `transfer::tests::consolidate` 6/6, `estate_move` 7/7.
- Whole suite on honey: `cargo test --workspace --locked --no-fail-fast`
  under the lane limits ran 47 test binaries: 1398 passed, 1 failed, 9
  ignored. The one failure was `source_command_registry`
  `every_child_goes_through_the_source_safe_builder`, which matched a local
  variable named `fork` in `reseat/place.rs`; the variable is renamed.
  After that, `source_command_registry` 22/22, `a106` 9/9, fmt and clippy
  `-D warnings` ok. An earlier full run had also caught
  `every_counter_is_incremented_somewhere` on the never-bumped
  `reseat_kept_aside`, which is now removed.
- Still owed: the merge gate (the coordinator's, after #297 merges) and
  pzm `darwin-check`.

### Receipt (2026-10-20, honey, at `e96eea0` plus the docs)

- Same environment and limits as the first receipt. Another lane's full
  workspace run shared honey, so only targeted filters ran, one at a time.
- `cargo fmt --all -- --check`: ok. `cargo clippy -p bulkload-agent -p
  bulkload-proto --all-targets --locked -- -D warnings`: ok.
- Lib: `consolidate` 35/35 (#297's, `append_log` 8 and
  `transfer::consolidate`), `transfer::tests::consolidate` 5/5,
  `a106` 9/9, `reseat::` 84/84, `beside` 27/27.
- Integration: `estate_move` 7/7.
- Also: `closure` 39/39, `map_prefix` 8/8, `transfer::tests::prefix`
  6/6, `keymap` 4/4, `bulkload-proto` 18/18, `estate_reseat` 7/7,
  `single_file_copy` 17/17, fault `reseat` 23/23, fault `beside` 10/10.
- Still owed: the whole suite, the merge gate, and pzm `darwin-check`.

## Merge of #297 at `b833705` (`ba16ab2`)

#297 removed `--beside` (SPEC rule 3) and added the `pull --consolidate`
dispatch. #294 now lands through that dispatch, and the dispatch calls
#294's `consolidate::append_log::union` for every append log, streamed
entries included (#297 held those). This is the single union path
correction (3): a covered file is never rewritten. #294's own `--beside`
pieces are dropped (dest-ahead, the beside consolidation,
`set_consolidate_archives`, the Codex index retirement). `--map-prefix`
(maps, `DESTINATION_MAP_CONFLICT`, the `mapped` rows) is put back on top of
#297's `transfer.rs`, and estate-move keeps the key-map phase. Tests:
`map_prefix` runs under `--consolidate`, and #297's append-log and
estate-move expectations now see the union and the mapped keys. Targeted
on honey: clippy ok, `transfer::` 147/147, `consolidate` 35/35,
`reseat::` 84/84, `estate_move` 9/9.

Whole suite on honey after the merge: `cargo test --workspace --locked
--no-fail-fast` (lane limits). 47 binaries ran: 1382 passed, 1 failed, 9
ignored; fmt and clippy ok. The one failure was `refusal_taxonomy`
`every_refusal_variant_has_a_non_test_constructor`: `DESTINATION_UNSETTLED`
lost its last producer with `--beside`, so the code is removed (it never
shipped). After that, `bulkload-proto` 18/18, `refusal_taxonomy` 5/5,
`closure` 38/38, `transfer::tests::consolidate` 4/4, and fmt and clippy are
ok.

## Resume blockers: `f794163`

1. In-place extension: the live file a prefix of the source gets its
   tail through `O_APPEND` on the same inode. A racing writer is never
   written over; the result is RACED and the next pass merges again.
   The swap only does true unions, and only when nobody holds the file.
2. Archive links: `append_log::union_link` writes the union as a regular
   file at the live name for a link into a `--replace-archived-dest`
   root, and leaves the archive copy alone. I wrote the union at the live
   name rather than into the archive target, so the TIN-5268 archive
   stays untouched. Manifest and streamed paths; Claude and Codex.
3. Source-key fold: the same session at a map's source key on the
   destination is moved (renamed) to the mapped key when absent there,
   else unioned and removed once durable; ledgered (`key_fold`), reported
   (`key-fold`), planned (`fold-move`, `fold-union`) and proofed (`fold`).
   Every file of a session under the source key now folds, its `<id>/`
   subdirectory included. An append log is moved or unioned; any other
   file is moved when absent, removed when it has the same bytes as the
   mapped key's, and held (both kept) when the bytes differ.
   Test: `a_sessions_subfiles_at_the_source_key_fold_the_same_way`.


- Source-key directories already on sting stay (removing a transcript is
  forbidden); `--key-map-proof` is the operator's evidence for a later
  ruling.
- Keys from older Claude Code versions (dots kept, e.g.
  `-Users-jess-git-GloriousFlywheel-.claude-worktrees-*`) and sessions whose
  `cwd` is not a lane source exactly (subdirectories) are not mapped; the
  re-seat re-keys them by `cwd` as before (a second copy).
- A key two lane sources share is held, not mapped (strict: even when both
  would land at one destination key).
- A source path reached through a symlink keys by its target; its derived
  map goes unused without a `key-map-unused` note (review 4's gap).
- A source copy whose bytes equal sting's transcript but which the pull
  still landed beside (another mode, other links) is `covered` and kept:
  removing it would only make the next pull land it again.
- The re-seat's in-place extension, like the pull's, cannot see an
  open-append-close writer between its size check and its write (#280's
  residual), now for every small transcript too.
- A beside output the pull left in an earlier pass (a racy destination, or
  before this lane) is not converged back into the live path (#280's C12).
- The would-write listing is #297's `--plan-only` (its item 3, not built
  yet). This lane supplies its append-log row (`append_log::plan_at`).
- A copy whose union keeps refusing (a process never closes the file)
  stays kept and held until it can be unioned.
- Darwin's open-holder check (libproc) is not built; off Linux the union
  holds typed.
- A link outside the archive roots, and a session held under a key that
  is no map source, stay held (`OTHER_KIND`, `held_elsewhere`).
- `just darwin-check` on pzm is owed before merge.

## 2026-10-21: the gate ruling and the escalation deflake (after #294 merged)

- Operator ruling, 2026-10-21, verbatim: "honey load is a self-fulfillling
  failiure, cut that gate." `AGENTS.md`'s Validation section now records
  it. Merge evidence is the lane's targeted tests on honey, fmt and
  clippy, gitleaks, and the pzm `darwin-check` when the PR is
  darwin-touched. `just local-gate` stays available but is not required.
- `an_idle_background_process_escalates_once_after_the_threshold` raced.
  The watcher moves the IO class first and bumps its counters after, and
  the test closed stdin as soon as the class moved, so on a loaded host
  the counters line came first. The test now waits up to 30 s for the
  escalation event itself: the watcher thread's end, which follows its
  counters.
- On honey (load average about 41): fmt and clippy ok, and
  `priority_escalation` 3/3 three times in a row.
