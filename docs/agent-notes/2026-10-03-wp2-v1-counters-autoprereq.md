# 2026-10-03 — WP2 v1 counters and auto-prerequisite (lane wp2-v1-counters-autoprereq)

Lane: WP2 PR 1 and PR 2 of the 2026-10-03 architecture review, host sting.
Worktrees `bulkload.worktrees/wp2-v1-counters-autoprereq-20261003` (PR 1,
branch `feat/wp2-v1-counters-autoprereq-20261003`) and `…-pr2` (PR 2,
stacked on PR 1), cut from origin/main c65dee5.

Rulings cited:
- OI-1003-Q15 (WP0 (a)): land counted v1 pack reads, a `census_walks`
  counter and v1 auto-prerequisite bundles, then measure. carry_v2 is
  frozen and untouched by this lane.
- OI-1003-Q7: properties over examples, with a fixed CI seed.
- R-N13: receipts cite rulings; this note.

## PR 1 — counters

- `pack_child` reaps the git children that pack a capture (`bundle create`,
  the shallow envelope's `pack-objects`) with `wait4`.
  `read_source_pack_readback_bytes` is `ru_inblock` x 512. On Linux that
  equals `/proc/<pid>/io` `read_bytes`. On Darwin it is a labelled lower
  bound. Both are blind to page-cache hits (measured 0 on a warm sting
  cache).
- Logical measure, new: `write_source_pack_bytes` (bundle as git wrote it)
  and `write_source_pack_objects` (pack header). `Export::pack` carries the
  same numbers.
- `read_source_capture_reuse_bytes`: the retained bundle each blob-reuse
  fetch reads.
- `census_walks`: one per metadata census of a checkout. Pinned by
  `tests/git_capture_counters.rs` at 4 for a changed item and 1 for a reuse
  hit (review finding 5; WP7 owns reducing it).
- `copy_hashing` (bundle staging) is counted as `read_bundle_stage_bytes`,
  `blake3_bundle_stage_bytes` and `write_bundle_stage_bytes`.
- Deleted six never-incremented pack-store counters (`read_other_chunk`,
  `write_dest_pack`, `write_legacy_chunk`, `blake3_capture_file`,
  `blake3_store_read_verify`, `blake3_legacy_put`). The brief named only
  `read_other_chunk`. The new source-scan test
  `every_counter_is_incremented_somewhere` found the other five.
- Baseline measured by the PR 1 test: a changed rerun re-packed all history
  (`write_source_pack_bytes` ≥ the 256 KiB incompressible blob).

## Open

- WP6 PR 1 still owns `source_child_read_bytes` for non-pack children and
  the counter consolidation.
