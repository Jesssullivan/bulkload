# M2 W1 drift follow-ups (round-4 deferrables of #52)

Date: 2026-09-23. Rulings: R-N72 (Linear TIN-4540), R-N69, R-N80, R-N101,
R-N104. Branch `fix/m2-w1-drift-followups`, cut from #52's head `0f3c4d2`
because #52 had not landed yet.

#52 came back clean in round 4. This PR fixes the deferrables the round-4
review listed. Each fix has a test that failed before it and passes after.

| Item | Fix | Test |
| --- | --- | --- |
| R1 | `shallow::unpack` refuses `CAPTURE_DRIFTED` when the envelope's inner inventory carries `capture-drift-v1`, even with no header marker | `r4_shallow_envelope_with_inner_marker_only` (ported from the round-4 review) |
| P1 | The stage directory sits next to the bundle (the corpus, for an estate apply), falling back to TMPDIR. The copy is hashed while it is written, and apply compares that digest instead of reading the bundle again. The "clone on APFS" claim is corrected: the stage is a full copy | `a_stage_lives_next_to_its_bundle_and_is_removed_on_drop`, `a_stage_falls_back_to_tmpdir_when_its_parent_refuses` |
| P2 | `apply_item` reads the journal before staging, so a re-apply of a done item copies nothing | `a_reapply_of_a_done_item_stages_nothing` |
| R2 | `source_index` runs its checks with `GIT_INDEX_FILE` pointing at a private copy of the carried bytes | `the_index_checks_validate_the_carried_bytes` |

Process note (R-N104): before R-N104 reached main, one of this lane's waits
polled a process ID until it exited, and one `ps` listing was taken. From
here on the lane waits only on its own task notifications and on files.

Open: none from the round-4 list.
