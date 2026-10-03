# 2026-10-03 — WP2 v1 counters and auto-prerequisite (lane wp2-v1-counters-autoprereq)

Lane: WP2 PR 1 and PR 2 of the 2026-10-03 architecture review, host sting.
Worktrees `bulkload.worktrees/wp2-v1-counters-autoprereq-20261003` (PR 1,
branch `feat/wp2-v1-counters-autoprereq-20261003`) and `…-pr2` (PR 2,
stacked on PR 1), cut from origin/main c65dee5. PRs: #144 (PR 1), PR 2 stacked on it.

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

## PR 2 — auto-prerequisite chains

- **Prerequisites.** Without a plan base, a capture after a retained capture
  declares that capture's **source-held** tips as prerequisites. These are
  peeled commits that the source object store answers for with
  `cat-file --batch-check` and `GIT_NO_LAZY_FETCH=1`.
  - Metadata commits (staged, worktree, filesystem rows) are never
    prerequisites. If they were, the next pass's blob-reuse fetch, and every
    later link, would need objects that no private repository holds.
  - A pruned tip just packs more.
- **Packing.** A chained pack comes from `rev-list --objects-edge-aggressive`
  plus `pack-objects`, with a header that bulkload writes itself.
  - Finding: `bundle create ^tip` marks only edge commits' trees as
    uninteresting.
  - The capture's parentless staged and worktree commits therefore re-packed
    every tracked blob. The first property run caught this: an untracked-only
    step produced a 67 KB bundle.
- **Sidecar.** The `{bundle}.prior` sidecar records the predecessor's name,
  digest, stat identity and depth. It is durable before the record, like
  `.base`.
- **Depth bound.** `CHAIN_DEPTH_LIMIT = 8`. The pass after a depth-8 bundle
  re-bases to a self-contained bundle.
- **Broken chains.** A broken chain is never a hit and never extended; the
  next pass re-bases.
- **Apply.** `chain::flatten` verifies every link (digest, identity,
  depth −1 per step, self-contained root, prerequisites satisfied in order).
  It writes one self-contained bundle whose sorted `list-heads` must equal
  the head's.
  - A missing link refuses with `SEALED_OBJECT_MISSING`.
  - A replaced link or a bad depth refuses with `RECEIPT_BINDING_INVALID`.
- **Space plan.** The space plan charges the whole chain.
- **Proptest helper.** `test_support::prop_config` is the proptest helper:
  a fixed CI seed, and 20x random cases under `BULKLOAD_PROPTEST_DEEP=1`.
  If the property-test plan's PR0 lands separately, the two helpers should
  merge.
- **Tests.**
  - P-CHAIN: 6 CI cases. It also passed a local deep run of 120 random cases
    in 333 s.
  - The depth-limit re-base test.
  - The broken-chain test (typed apply refusal, then a re-base).
  - The replaced-link test.
  - The CLI counter test. A changed rerun now writes less than 1/8 of the
    256 KiB history it used to re-pack, and apply restores HEAD and bytes
    exactly.

## Open

- `git-repair-missing-index` and the `git-attach-*` / `git-import` side doors
  still take a single bundle. For a chained capture they need the
  destination to already hold the prerequisites; otherwise they refuse with
  the typed `GIT_INVENTORY_MISSING_PREREQUISITE`. WP4 folds those verbs into
  estate apply.
- The S3 measurement on the estate-shaped corpus (WP0 (e)) is the next step
  under OI-1003-Q15. It decides v1, hybrid or v2.

- WP6 PR 1 still owns `source_child_read_bytes` for non-pack children and
  the counter consolidation.
