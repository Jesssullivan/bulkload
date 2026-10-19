# 2026-10-19: `estate-move` carries the source notes

Lane: estate-move carry-notes (coordinator brief 2026-10-19). Operator
rulings served: "NOTHING is EVER excluded" and "no intermidiary stage
patterns" (2026-10-19). Branch `feat/estate-move-carry-notes-20261019` from
`origin/main` `9854236`, stacked on #283
(`fix/single-file-pull-and-serve-state-20261019`, merged here at `b0227c8`)
and #280 (`feat/pull-beside-live-home-20261019`, merged here at `2290e26`):
it must merge after both. Edited on sting; every build and test ran on honey
(OI-1003-Q196), in this lane's own worktree
`~/git-build/estate-move-carry-notes-20261019`, `CARGO_HOME` and
`CARGO_TARGET_DIR` under `~/git-build/estate-move-carry-notes-20261019-cargo`
(debuginfo off, the coordinator's lean-target instruction), test scratch
`TMPDIR=/var/tmp/jess-bulkload-tests/estate-move-carry-notes-20261019`.
Nothing ran against the live estate. R-N13.

## The requirement served

SPEC R1, quoted: "**Everything is carried, regardless of content** (KEEP-12,
KEEP-13, KEEP-09, KEEP-19, KEEP-21, KEEP-22)." SPEC R0, quoted: "R0 also
covers dots, configs and loose home files, whether or not a session is
attached (SEAT-05, SEAT-06, RES-06, KEEP-01, KEEP-10, KEEP-26)." SPEC R1,
quoted: "**What bulkload cannot capture natively is carried as files**,
exactly as it is, and recorded `carried-as-files`." Acceptance 1, quoted:
"**The closure report shows 0 unaccounted** and every refusal is typed and
dispositioned (SEAT-12, `slo.md` S4)." SPEC R2.4, quoted: "A rerun reads **0
content bytes** for unchanged work."

## Before

`estate-move-source` wrote what it could not plan as an item to
`MOVE_STATE/carry/move-source.notes` and printed it; `estate-move` phase 7
summed the counts as `not_moved=` and failed its gate on any of them. No
phase moved them. Tonight's live passes noted gf01 18, r01 1160, r02 553,
r03 3805 and r04 4112 entries (about 9,600): loose files at the top of
`~/git`, archive directories, `*.worktrees` container directories, `.idea`
and `.direnv` trees, scaffolds, symlinks, prunable worktrees, unreadable
entries and custody-only nested repositories. A `not-git` note named a
directory and the count of plain files directly in it, which could not be
carried as it stood: that directory may hold repositories too (`~/git`
itself).

## What was built

- **Source half: notes that can be carried.** A directory that holds no
  repository anywhere beneath it is one `not-git-tree` note (the outermost
  one; its entries counted, at least 1). In a directory that holds one, each
  plain file is a `not-git-file` note and each symlink a `symlink` note. A
  prunable worktree still on disk is one `prunable` note counting its tree;
  one whose directory is gone carries `SOURCE_ROOT_MISSING`. A nested
  custody-only repository is noted by its own path (the item's source joined
  with the nest's). A seat beneath a root the walk cannot read is typed
  `SOURCE_ROOT_UNREADABLE` (#283 types the root and a seat gone mid-walk,
  `SOURCE_VANISHED`). Each line is `WHAT COUNT CODE PATH<TAB>LIVE`, both paths
  escaped losslessly (`escape_ascii`), LIVE by the same `--map` rules as an
  item. `MOVE_STATE/serve/notes` is made beside `serve/carry`.
- **Destination half: phase 4b, after carry, before hand-off.** Each
  carriable note is one recorded transfer of its own
  (`MOVE_STATE/pull/notes/ID`; the source's state
  `SOURCE_MOVE_STATE/serve/notes/ID`) through the existing transport, `pull`
  from HOST or `copy` on `-`, with `--beside .LABEL`: a file or a symlink as
  a `--file` session (#283; a symlink is the link, its target bytes, never
  followed), a `not-git-tree`, `prunable` or `not-in-census` directory as one
  tree session (custody as files; Git never registers it again). A tree lands
  into a live directory holding no repository (a lossless union: an equal
  file adopted by its content proof, a differing one beside, nothing
  removed) and at `<live>.LABEL` beside anything else there (a checkout, a
  file, a symlink); a file or link at `<live>.LABEL` beside a directory.
  Absent parents are made 0755 as `estate-apply` makes an item's, recorded in
  `MOVE_STATE/notes/ID.parents`.
- **Not carried twice.** A note at or under a planned item's source checkout
  is covered by the item; one under a carried tree by the tree; a tree that
  holds a planned item is unaccounted (never carried whole).
- **Held, typed, reviewable.** Nested custody-only (`GIT_NEST_CUSTODY_ONLY`,
  a new code), unreadable, vanished and gone-prunable notes are not carried;
  each is a closure row pending review. A note from a source half before
  this change (`not-git`) is unaccounted until the source half is rerun.
- **Closure.** A carried note is its transfer's rows. Every other note is one
  `source-note` row (closure v5, an added row kind, as gap C12 added keys):
  `covered`, `refused-pending-review` with its code, or `unaccounted`. The
  pass's note rows are kept in `MOVE_STATE/notes.rows`, and
  `closure-dispose --source-notes MOVE_STATE/notes.rows` reviews one by its
  `note=` identity. The `estate-move closure` line adds `notes=`,
  `notes_carried=`, `not_moved=` (the entries of notes neither carried nor
  covered), `not_moved_dispositioned=` and `note_rows=`; the gate passes only
  when every row is accounted and reviewed and `not_moved` equals
  `not_moved_dispositioned`.
- **Rerun.** Each note keeps its own transfer state, so an unchanged rerun
  reuses every output: the `estate-move notes` summary line reads
  `source_bytes_read=0`.

Merge resolutions (stacking #283 and #280): `copy`/`pull` take
`[--beside SUFFIX] [--file]`; `open_regular` types a symlink at the path
`DESTINATION_KIND_MISMATCH` (#283) and a non-directory on the way
`DESTINATION_OCCUPIED` (#280), and `decide_regular` lands either beside under
`--beside`, so #280's beside landing over a live symlink still holds.

Merge follow-up: #280's `a_seat_over_a_destination_symlink_lands_beside_or_refuses_typed`
now expects #283's `DESTINATION_KIND_MISMATCH` for the regular seat without
`--beside`. With `--beside`, `plan_file`, the streamed `other_kind` check
and the two beside identity lookups take either code, so the seat still
lands beside.

## Commits and receipts

Lane commits on the branch, after the two merges:
- `19a2ec3` feat(estate-move): carry the source notes; hold the rest typed;
- `3e51b7c` and `e08fea9`, the #283 × #280 reconciliation: a regular seat
  over a destination symlink is `DESTINATION_KIND_MISMATCH` without
  `--beside` (#283), and every `--beside` site lands it beside (#280);
- `f6b4120` test: settle the fixture's racy window (#86) before the first
  pass, so the rerun's 0 source bytes is R25's under any load;
- `e6deb1f` and `53cb674`, rustfmt and usage text; `0ff064b`, this note.

Receipts, honey, head `f6b4120aa3b9`, tree `d01f1946`, `nice -n 19 nix
develop .#default`, `CARGO_TARGET_DIR` and `CARGO_HOME` under
`~/git-build/estate-move-carry-notes-20261019-cargo` (debuginfo off),
`TMPDIR=/var/tmp/jess-bulkload-tests/estate-move-carry-notes-20261019`.
honey's load was 74 to 119 throughout.
- `cargo fmt --all -- --check`: ok.
- `cargo clippy -p bulkload-agent -p bulkload-proto --all-targets --locked
  -D warnings`: ok.
- `cargo clippy --workspace --all-targets --locked -D warnings`: ok.
- `cargo test -p bulkload-proto`: 18 passed.
- `cargo test -p bulkload-agent --locked --no-fail-fast`: 42 targets, 1279
  passed, 1 failed, 8 ignored. The failure was `estate_move.rs`
  `one_pass_lands_adopts_besides_carries_and_accounts_for_every_item` at
  `:399`: the converge pass did not recapture `diverged`
  (`previous-workspace-beside-not-revalidated`). It is the brief's listed
  load-flake. Rerun alone (throttled, 4 jobs, 4 test threads, load 119) it
  failed once, then passed.
- `just rust-check`: fmt, both clippy passes and the `io-trace` clippy pass
  were ok. In `rust-test`, groups 1, 3, 4 and 5 were ok. Group 2 stopped at
  the same `estate_move` flake: it has no `--no-fail-fast`, so the targets
  after it did not run. Those targets were then run alone:
  `estate_move_notes` 2, `estate_move_source_roots` 6, `estate_reseat` 6,
  `estate_retarget` 3 and `estimate_cli` 4, all passed.
- `estate_move_notes` was also run alone twice on the head: 2 passed both
  times.

The flake comes from `estate_move.rs`'s own design: its converge pass relies
on `diverged`'s first capture still being racy at the second source pass.
The notes phase adds one more session to each pass, which widens that
window a little. The test is #282's (gap C12); settling it would change
what it proves, and that is that lane's call.

Not run in this lane: `just fault-harness` and the R-N88/R-N119 power-loss
proofs (mandatory in `check-fast` and the merge gate). They were held back
because honey was needed for a production image build (coordinator,
2026-10-19).

`just darwin-touched` against `origin/main`: yes, from #280's and #283's
files and the `open_regular` merge resolution (the lane's own commits
alone: no). The stacked diff needs `just darwin-check` on pzm before merge,
or its recorded deferral while pzm is on hold.

## Still open

- One transport session per loose file: a directory of many loose files
  beside repositories costs one session each (one ssh each over `pull`). A
  "loose files of one directory" session would need a new walk mode.
- An unreadable entry inside a carried tree is refused by that tree's
  transfer as the engine types a seat it cannot read (a bare `IO` with its
  errno, which no review closes; per-seat typing is gap C5's), besides its
  own typed note.
- `nested` notes are held, not carried: a nested repository's content is
  still only custody. Carrying it is a separate decision for the operator.
- The live estate has not been moved with this build; no R0 receipt exists.
  This is not a "done" or "ready" claim.
