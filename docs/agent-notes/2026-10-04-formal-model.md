# 2026-10-04 — formal-model review fixes (lane B: proof package, formal model)

Rulings:

- OI-1003-Q7: proof package, formal model; CI stays slim.
- OI-1003-Q20: WP0(g), "only if proven in the formal model".
- OI-1003-Q23.
- OI-1003-Q32: TLA+ with TLC is the checker of record, with an N-version
  core.
- R-N13.
- Also cited: OI-1002-Q33 (#124: "R25 stays strict for anything the
  destination held durably"); OI-1003-Q24 and OI-1003-Q26 (recorded on
  `docs/coordinator-20261003` at 978446734, implemented by PR #154, which
  merged during this session as `4a7b86b`);
  OI-1001-Q15; R-N92 and R-N101 (wording); R-N11, R-N12 and R-N104.

Branch `docs/tla-model-20261003`, worktree
`bulkload.worktrees/tla-model-20261003`. This session was its only writer;
the two duplicate copies of 2026-10-03 were gone (reported by the
coordinator, not verified here). It started from d7589e0, the tip the review
read.

Shas:

- `8bc6672`: the model, generator, configs and runner fixes.
- The commit on top of `8bc6672`: the README (results, coverage, the WP0(g)
  verdict, disagreements), this note, corrections to the 2026-10-03 note,
  and comment-only spec and config edits (#124's closure).
- A follow-up commit records this note's check-fast result.

## What the review found, and what was done

Eight medium or high findings, all valid; none rejected.

| # | Finding | Fix |
|---|---|---|
| 1 (high) | The model assumes both stores exist durably once their first commit returns. `private_dir` never seals the state root's parent, so under strict settings a source power loss can still lose the store and its authority (`MC_wp0g_authority`'s counterexample, in the code today). | New constant `StoreRootSealed` (TRUE in every positive config) makes the assumption explicit. `MC_store_root_unsealed` (strict settings, unsealed root) fails `R25_NoDurableReread`. The destination store's loss stays an assumption (`HeldPhys` needs a row). The README adds "state root entry sealed" to the WP0(g) conditions. The code fix (seal the parent and the root in `Store::open`, plus a `crash_check` trace) is agent source, outside this lane: open. |
| 2, 7 (medium) | `R25_NoCommittedCaptureReread`, slo.md's R25 wording, is vacuous in code shape, and no fail row names it. | New mutation `reread_ignore_ledger` (no `Reuse` and the source ignores its ledger) and the row `MC_neg_reread_exchange` (`reread_durable` under the exchange design) each fail it. `gen_cfgs.py` asserts that every safety invariant except `TypeOK` has a fail row. README: it is vacuous in code shape, and `R25_NoDurableReread` is the operative check. A ruling on slo.md's wording is open. |
| 3 (medium) | R25 is narrowed by definition ("held" means a committed row), and the spec overclaimed that both clauses are one check. No ruling defines that reading: #124 asked for one, and its answer OI-1002-Q33 says "strict" without defining "held" (#124 closed with #154 on 2026-10-04). | The overclaim is struck. Ghost `TrackStrictHeld` and the property `R25_StrictNoDurableReread` (not code-shape, not frozen) make the gap visible: `MC_r25_unrowed_bytes` fails it in code shape (a crash after the seals, before `commit_outputs`). The README's disagreements list OI-1002-Q33, #124, OI-1003-Q24, OI-1003-Q26 and PR #154, and the re-check #154 needs (`Store::open`'s `racy_guard` row deletion, `SALVAGE_BOUND_EXCEEDED` missing from `TypedCodes`). |
| 4 (medium) | `LedgerCommit` never fails; in the code a failed ledger group is sticky and fails the session before `SourceDone`. | Listed under "Not proven here", noted in the spec header and at `LedgerCommit`, and added as a WP0(g) condition: log and count a failed ledger commit, never fail the transfer. The code change is outside this lane: open. |
| 5 (high) | `MC_wp0g` (MC_main's bound) never read the relaxed ledger; its coverage equalled `MC_main`'s. | `MC_wp0g` is re-bounded to {a,b} R3 C1 E0 F1, relaxed, with symmetry. The ghost `ledgerLost` records rows a source power loss dropped, and `MC_reach_wp0g_lost_row` reaches `Witness_LostRowRead` at that bound: a later run consults the ledger for a lost row, misses, and reads. The README verdict and the 2026-10-03 note no longer cite the old bound as evidence. |
| 6 (medium) | The budget self-test was accepted on any INCONCLUSIVE, including a log with no `Finished` line. | The self-test needs `Finished` and `WithinBudget` alone violated. A log with no `Finished` line is ABORTED and matches no expectation. Checked with a stand-in tlc: exit 137 gives ABORTED and the recipe stops; a clean pass gives WRONG; `WithinBudget` plus `TypeOK` gives WRONG. |
| 8 (medium) | Coverage was printed, never enforced; the ledger branches were dead in `MC_main`, `MC_wp0g` and `MC_nv_core`. | configs.tsv's new `never` column holds each pass row's exact never-enabled set, and a difference fails the row. Reach rows prove the ledger-manifest and ledger chunk-read branches reachable at `MC_nv_ledger`'s bound. `MC_nv_ledger` (MC_nv_core plus one third-party write) is a second N-version row. The digest-mismatch branch is documented as dead by `LedgerSound`. |

Line-level coverage assertions were done as reach rows (a `Witness_`
invariant that must be violated at the same bound as a pass row) rather
than as line numbers from TLC's report, which move with every spec edit.

The nine low findings were left untouched, as the brief asked, and are
listed under Open. Two wording changes that would have touched them were
backed out of the README.

## Validation

Host sting, TLC 2.19, one JVM at a time, `-Xmx4g`, 3 workers, `nice -n 10`,
every metadir under a private `$TMPDIR/tla-owner-*` directory, no outside
clock.

- **Run of record:** one full `just tla-check` over `8bc6672`, 2026-10-04
  01:30–01:38 EDT, load average near 30. **43/43 rows matched: 10 PASS,
  3 REACHED, 28 FAIL, 1 SIMULATION, 1 INCONCLUSIVE, 0 ABORTED, 0 WRONG**;
  every pass row's never-enabled set equalled its `never` column; 480 s,
  peak RSS 1,882 MiB. Comment-only edits landed in the worktree during the
  run.
- Hand runs (one JVM each, logs kept in scratch) gave the counterexample
  lengths and coverage details the README cites:
  - `MC_wp0g_authority` and `MC_store_root_unsealed`: 15 states each.
  - `MC_r25_unrowed_bytes`: 13 states.
  - `MC_neg_reread_ignore_ledger`: 19 states. `MC_neg_reread_exchange`: 23.
  - `CrashBoth` adds 3,874 distinct states in `MC_wp0g`, 10,681 in
    `MC_wp0g_deep` and 0 in `MC_nv_ledger`.
  - `MC_nv_ledger` has 2 initial states and a maximum out-degree of 9.
- Before it, the new small rows and the positive rows ran separately on the
  same spec, with the same counts and verdicts.
- `MC_main`, `MC_main_deep`, `MC_dest_faults`, `MC_nv_core`, `MC_wp0d_exchange`,
  `MC_s2` and `MC_live` keep their 2026-10-03 counts exactly. `MC_wp0g_deep`
  grew (354,580 to 419,020 distinct) because `ledgerLost` splits states
  that differ only in which rows a crash dropped; its behaviour is
  unchanged. `MC_wp0g` is a new bound, 496,830 distinct, against the
  review's ghost-free 427,276 at the same bound.
- `just check-fast` (CI toolchain, shared lock): run on this commit's tree; the result is recorded by the follow-up commit.

## Open

- **Code, outside this lane (agent sources):**
  - Seal the state root's parent after `private_dir` creates it, and the
    root after `transfer.sqlite` is created, before `Store::open` returns.
    Add a `crash_check` trace over `Store::open`. Both stores; needed even
    without WP0(g).
  - Make a failed source ledger commit log-and-count, never fail the
    session (a WP0(g) condition).
- **Operator rulings:**
  - Which reading of R25's "held durably" holds: a committed row (the
    model's `R25_NoDurableReread`), or strict (OI-1002-Q33, where
    `MC_r25_unrowed_bytes` shows the code fails). #124 closed with #154
    without settling it.
  - docs/slo.md's model obligation is worded as
    `R25_NoCommittedCaptureReread`, which is vacuous in code shape. Should
    it name `R25_NoDurableReread`?
  - Freeze `MC_nv_ledger` as the second N-version row.
- **PR #154 merged (`4a7b86b`, 2026-10-04 05:18 UTC) during this session;
  the model is not yet re-checked against it.** `StartRun` omits
  `Store::open`'s deletion of every `captures` and `outputs` row when the
  `racy_guard` marker is missing, and `TypedCodes` lacks
  `SALVAGE_BOUND_EXCEEDED`. Bounded salvage is still folded into the
  destination's free choice.
- **Low findings, untouched:**
  - adoption at End against the path, not the descriptor;
  - destination identity unique per write;
  - the 16-byte prefix and post-read refusals that re-read every run;
  - fail rows check only `TypeOK` and their named property;
  - S2 mutants that only read a ghost;
  - the exchange design under a failed group commit;
  - liveness only on the happy path, and `MC_main_sim` never fires
    `ForeignDelete`;
  - an unknown config name exits 0;
  - the WP0(g) caveat's subset-versus-suffix claim, and point 2's
    narrative.
- Carried over from 2026-10-03: the WP0(d) exchange shape for WP5 PR 3, the
  design.md WantManifest wording, P34 and the backup's `-shm`, Sprint 2's
  Dhall catalogue and Haskell explorer, Linear (TIN-4543 and the SSOT
  ledger, owned by the coordinator), and the PR for this branch.
