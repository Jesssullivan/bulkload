# 2026-10-17: review lane (Bulkload Reset Review execution)

Lane: review-execution lane: light work on sting; builds and gates on a separate build host (named in each gate receipt).
Rulings executed: the operator's decisions on the Bulkload Reset Review, 2026-10-17
(91 approved, 2 held: N15, N19), recorded on Linear TIN-4543 `63feb3cf`; R-C841..R-C844
(`ace8649c`); R-N13. Held rows N15 and N19 were not touched.

## Bulkload repo

| PR | Rows | Merge |
|---|---|---|
| #254 docs: claim corrections and dated labels | C10, C11, C12, C13, K06, E05, E06, E03 (kept notes) | `3992d423` |
| #256 chore: delete the condemned paper, formal model and lever notes; retire gate_b and m0_gate_a | E01, E02, E03, E04, N03 | `73df49e6` |
| this PR: this note and the N20 wording draft | N20 (draft), session record | (this PR) |

- #256 carried an independent adversarial review (no blockers; one should-fix fixed in
  `bbcb186d`). `gate_b.py` became `link_common.py`, trimmed to what `m3_link.py` uses.
- GitHub: repo description corrected (C09); #88 retitled and relabelled `R2` (T09); #104
  venue moved to PZM (T10); N17 triage comments on 23 issues (3 M2, 19 M3, #123 closed not
  planned; none fixed by an M2 PR).
- Gates: the interim local gate's logic ran with the build on a separate host (coordinator
  relay of the operator ruling, 2026-10-17: builds and tests off sting while it ingests the
  migration); the merge of main, signing and pushing stayed on sting. Each receipt in
  `receipts.tsv` names its build host. Failed attempts are recorded there too: #254 on sting
  hit ENOSPC in the `/srv/cache/jess` project quota (exit 101) and a rerun was stopped when the
  coordinator paused builds on sting; on the build host, one attempt lost a rustup install race
  with another lane (the review lane then used its own `RUSTUP_HOME`), and one failed
  `estate_handoff::h14_estate_diff_reads_a_handed_off_item_as_identical` by 43 bytes at load
  ~150 (a timing-sensitive racy-seat assertion; docs-only PR; the retry passed). Worth a look
  by the engine lane if it recurs.

## N18 repo hygiene

- Bundles (verified by fetch into fresh repos, ref-by-ref sha match, fsck rc=0), under
  `sting:/srv/data/jess/archive/bulkload-refs-20261017/`:
  `bulkload-sting-only-refs-20261017.bundle` (3 no-upstream branches, 4 `refs/remotes/extract/*`,
  stash@{0} and stash@{1}) and `tummycrypt-sting-only-stash-20261017.bundle` (read-only).
  The live move's `refs/carry/*` were excluded and untouched.
- Tag `v0.1.0-historical-note` (signed, annotated, on `f45f3a5af5`); `v0.1.0` not moved.
- 22 clean, merged worktrees removed after `worktree-reap-manifest-20261017.tsv`.
- 96 merged remote branches pruned after listing (TIN-4543 `a1f1952e`, receipt `fdb981c2`).

## Linear

Dated comments only; no description rewritten. Corrections: T01-T08, C15-C18, S17, N13, N14,
S02. P-TIN-177 renamed (C15). Handoffs: lab seat, GF and egreg (TIN-3692 `e57b1011`), Codex
migration lane (TIN-3692 `30050f06`), tcfs lane (P-TIN-143 `dac1e375`), client lane N20
(TIN-5890 `9b0e4e98`).

## Deferred

- E09: deleting the prebuilt binaries under `sting:/srv/fast-local/jess/state/bulkload-artifacts`
  conflicts with this lane's standing instruction never to delete live state on sting.
- E12: the untracked duplicate is in the tummycrypt checkout, which this lane did not touch;
  handed to the tcfs lane.
- E06's "lift the v1-header limits into SPEC R5" is left to the spec author; the plan carries a
  dated label naming them as R5 salvage.

## Open

- The handoff rows (C01-C08, C19-C22, E07, E08, E10, E11, N09-N12, N16) are pending with
  their owners.
- `tla-check` (EstateConverge only) was never run under TLC after the trim.
