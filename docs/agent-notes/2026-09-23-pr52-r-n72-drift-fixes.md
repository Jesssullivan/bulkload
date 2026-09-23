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
