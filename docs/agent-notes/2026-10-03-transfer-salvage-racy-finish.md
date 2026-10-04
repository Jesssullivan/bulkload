# 2026-10-03: transfer-salvage-racy-finish lane (#124, #125)

**Seat:** sting, workflow lane `transfer-salvage-racy-finish`, branch
`feat/transfer-salvage-racy-finish-20261003` from `origin/main` (efc3266).
Finishes the interrupted lane `transfer-salvage-racy` (uncommitted work in
`bulkload.worktrees/transfer-salvage-racy-20261003`, based on c65dee5), whose
diff applied cleanly onto current main.

**Rulings:** OI-1002-Q33 (#124: keep salvage only for byte-touching
refusals, bounded, typed refusal at the bound), OI-1003-Q15..Q21 (WP0
(a)-(g), wave 3; carry_v2 frozen and untouched), OI-1003-Q7 (property tests,
fixed seed), OI-1002-Q24, OI-1001-Q15, OI-1001-Q17, R25/R-N58, R33, R-N13.
Plan: advances WP3 (#124 conservative-refusal path) and WP5 (#125) of
`docs/plans/2026-10-03-architecture-review.md`.

## Done

- **#124, bounded salvage for byte-touching refusals.** The receiver records,
  per entry, which salvaged temporaries its staged file took chunks from.
  When the session finishes, a temporary is kept only if one of those
  entries was refused (verification, publication or group commit failed
  after it was filled). Every other temporary is removed, as #97 did. Kept
  salvage is bounded at 1024 temporaries and 4 GiB per session, greedily in
  salvage order (`bound_salvage`). A temporary past the bound is removed and
  refused with the new typed code `SALVAGE_BOUND_EXCEEDED`, under its current
  name. `Destination::remove_salvaged` became `retire_salvaged(keep)`.
- **#125, rows from before the racy guard.** Decision: refuse to trust them
  for Reuse, which costs one counted re-read per seat. A fresh store gets a
  `racy_guard` row in `settings` in its first commit. The first writable
  open of a store without that row deletes every `captures` and `outputs`
  row in the same transaction that adds it, and counts them in
  `transfer_legacy_rows_invalidated`. A read-only handle on an unmarked store
  serves no capture and matches no output. Chunk hints are kept, so existing
  outputs are adopted after verification and send no wire bytes. Stores
  written between #86 and this change carry no marker either, so they pay
  the same one re-read.
- **Docs.** `docs/design.md` covers the salvage rule and bound, the legacy-row
  pass, and the NFS/SMB clock-skew limit of the racy predicate. Destinations
  are local (OI-1001-Q17); sources may not be.
- **Shared proptest helper.** `crates/bulkload-agent/src/test_support.rs`
  (`prop_config`, fixed CI seed) is copied verbatim from the in-flight WP1
  branch `feat/wp1-s2-source-safety-20261003`; whichever lands second takes
  the other's copy.

## Tests

- `a_byte_touching_refusal_keeps_its_salvage`: under `Held{false}` the
  salvage is kept; the retry reads 0 source bytes and receives 0 wire bytes.
- `a_refusal_that_staged_nothing_drops_the_salvage`: a refusal at decision
  time removes the salvage; the retry reads and sends the file once.
- `salvage_past_its_bound_is_a_typed_refusal`: count bound, byte bound and an
  unbounded control.
- `the_salvage_bound_keeps_greedily_within_both_limits` (proptest, 256
  fixed-seed cases): partition, both limits, no refusal with room left.
- `rows_from_before_the_racy_guard_are_read_again_once` (transfer) and
  `a_store_from_before_the_racy_guard_trusts_none_of_its_rows` (store).

## Validation

On sting, base efc3266 (origin/main had not moved; merge was a no-op):
`nix develop .#default --command just check-fast` exit 0 (agent lib 435
passed, 6 ignored; fault harness, power-loss and CI contract all ok), then
`just resume-power-loss` exit 0 (2 passed). Wave-3 PRs #143-#153 were still
open; when #145 lands, main's `test_support.rs` wins and this branch keeps
only its additions.

## Open

- The bound values (1024 files, 4 GiB) are unruled engineering defaults,
  marked so in `docs/design.md` and at the constants. An operator ruling can change the two constants in `transfer.rs`.
- The interrupted worktree `transfer-salvage-racy-20261003` is superseded by
  this branch and can be removed by the operator.
