# PR #52 drift-tolerant capture: R-N72 fixes

Date: 2026-09-23. Ruling: R-N72 (Linear TIN-4540). Branch:
`feat/m2-w1-drift-tolerant`, rebased on main after #51 and #54.

An adversarial review blocked #52. Each finding was fixed test-first: the
test was run red against the unfixed code, then green after the fix.

| Finding | Fix | Test |
| --- | --- | --- |
| 1. Stale-key reuse (critical) | `KeyParts::drift_to`: Hit only when pre- and post-pass key parts are exactly equal; any difference goes to `{bundle}.drift` | `a_ref_deleted_before_the_export_snapshot_never_reuses_a_stale_key`, `a_ref_deleted_before_the_export_snapshot_is_key_drift` |
| 1a. A clean pass can reproduce a drifted bundle byte for byte | The stale `.drift` sidecar is retired after the clean record is durable | `a_clean_pass_after_pre_snapshot_drift_retires_the_drift_record` |
| 2. Racy timestamps | Pass start recorded (`Export::started_ns`, `.parts`); seats stamped at or after start minus 2 s are never reused by identity | `a_same_size_rewrite_in_the_capture_tick_is_never_reused_by_identity` |
| 3. Drifted apply | `CAPTURE_DRIFTED` refusal; `held_uncaptured` removed; R-N29 deferred to W6 (bulkload#48) | `a_drifted_capture_refuses_to_apply_until_a_later_pass_extends_it` |
| 4a. Label | `capture-extended-from-drift` only after a drifted capture | `a_change_after_a_clean_capture_is_not_labelled_extended_from_drift` |
| 4b. Reuse ref leak | Retained fetch/decode failure degrades to no reuse; `refs/carry-reuse/*` deleted and proved gone, else refuse | `an_undecodable_retained_capture_degrades_to_no_reuse_and_leaks_no_refs` |
| 4c. Shallow | `reuse_unavailable=shallow` (also `retained-unreadable`, `pass-start-unrecorded`) | `a_shallow_checkout_reports_reuse_unavailable_instead_of_silently_rereading`, `a_shallow_item_reports_reuse_unavailable_on_its_receipt` |

Follow-up ruling R-N76 (2026-09-23, TIN-4540) closed the whole-capture racy
gap: a `capture-reused-after-census` Hit also requires a recorded pass start
and no seat racy against it (`KeyParts::racy_since`). Otherwise the item
takes the per-seat extend path. Tests:
`a_same_size_rewrite_in_the_capture_tick_never_reuses_the_whole_capture`
(red before, green after) and
`a_capture_whose_seats_predate_the_racy_window_is_still_reused_whole`. Legacy
records keep their bit-identical key and are re-captured once
(`retained_captures_from_before_this_change_keep_their_key_and_recapture_once`).
Tests that expect a Hit now wait out the 2 s racy window with `settle()`.

## Re-review of 5305b36 (R-N72, TIN-4540)

| Finding | Fix | Test |
| --- | --- | --- |
| 1 HIGH ref A to B to A across the two unguarded windows | `Export::refs_before`/`refs_after`; `KeyParts::drift_across` requires pre == export-before and export-after == post | `a_ref_deleted_and_recreated_across_both_unguarded_windows_is_drift` |
| 2 MED-HIGH missing sidecar fails open | Key drift and export drift split; a drifted record's key is poisoned; apply refuses on the in-band marker | `deleting_the_drift_sidecar_never_lets_an_export_drifted_capture_apply`, `deleting_the_drift_sidecar_never_makes_a_drifted_capture_a_hit`, `a_key_only_drifted_capture_is_a_coherent_snapshot_and_applies`, `two_state_dirs_sharing_a_corpus_fail_closed_on_interleaved_records` |
| 3 MED only estate-apply guarded | `refuse_drift_marked` at the entry of every restore/import verb (shallow custody probed in a removed scratch repo) | `every_restore_and_import_verb_refuses_a_drift_marked_bundle`, `a_drift_marked_shallow_bundle_refuses_to_restore` |
| 4 whole-capture racy guard | R-N76 commit; now also judged against the pass's own clock | R-N76 tests |
| 5 LOW wall-clock reference | Seats stamped later than the pass's clock are racy; NFS/NTP skew noted in design.md | `a_same_size_rewrite_in_the_capture_tick_is_never_reused_by_identity` (future-clock assertion) |
| 6 LOW | Fixed by the split in 2 | as 2 |
| 7 LOW timing-dependent racy test | Pass start and clock injected relative to the seat's own stamps | same test, now deterministic |
| 8 LOW wrong refusal code | `GIT_INVENTORY_MALFORMED`; deletion uses `--no-deref` (a symbolic reuse ref deleted its target before) | `clearing_reuse_refs_never_follows_a_symbolic_ref_and_refuses_as_git_inventory` |

Gates run with `CARGO_TARGET_DIR` inside this worktree after the #53 lane
reported a shared target dir.

## Round-3 re-review of 72fc71e (R-N72, TIN-4540)

The reviewer's adversarial tests (review-artifacts/pr52-r3) were ported; the
authority round trip is split per class so each reproduction fails alone.

| Finding | Fix | Test |
| --- | --- | --- |
| N1 HIGH authority A to B to A | `CarriedAuthority`: one read of HEAD, symbolic HEAD, index, exclude, stash reflog, configuration and frontier; the export carries exactly it and re-reads it at pass end; `drift_across` refuses `GIT_AUTHORITY_CHANGED` unless both key parts carry it | `r3_detached_head_…`, `r3_symbolic_head_…`, `r3_exclude_…`, `r3_config_…`, `r3_index_…`, `r3_stash_reflog_round_trip_across_unguarded_windows_refuses` |
| N2 MED shallow probe fetched the pack | Envelope lifts the marker into its headers (`shallow-drift-v1`); the check reads headers only; the duplicate apply check is gone | `a_drift_marked_shallow_bundle_refuses_to_restore` (headers assertion) |
| N3 LOW probe name collision | Probe directory removed; staging directories use an atomic counter and exclusive create | covered by N4 |
| N4 LOW check/import TOCTOU | `stage_bundle`: private copy; the check, digest and restore all read it | `a_bundle_swapped_after_its_drift_check_is_never_the_one_restored` |
| N5 LOW silent future stamp | `reuse_unavailable=future-stamp` | `a_future_stamped_seat_reports_reuse_unavailable_future_stamp` |
| INFO unauthenticated records | Known limit in design.md | `r3_sidecar_deleted_and_key_forged_still_refuses_in_band` (guard) |

Guards that must stay green: `r3_packed_refs_rewrite_is_not_drift`,
`r3_branch_reflog_only_change_keeps_the_hit`.
