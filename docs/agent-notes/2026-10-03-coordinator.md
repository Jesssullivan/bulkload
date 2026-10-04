# 2026-10-03/04: coordinator session — wave-3 train, evening sprints, overnight lanes

**Seat:** sting coordinator, Claude Code session `neo:f1c4ead8`.
**Linear:**
- TIN-4543 (engine and gates): comments from 2026-10-03 20:45 EDT through 2026-10-04 06:45 EDT.
- The SSOT document, sections "Post-maintenance resume", "Evening sprints" and "Overnight".

## Rulings this session (operator interviews, R-N13)

| Ruling | Decision |
|---|---|
| OI-1003-Q23 | Fan out lanes A (corpus + S3), B (formal model) and C (whitepaper) beside the merge train; the train has priority |
| Q24 | Salvage bounds are 1024 temporaries and 4 GiB, as #154 wrote them |
| Q25 | Background priority covers every source-reading verb, including `copy` and `git-export` |
| Q26 | #125 invalidates legacy rows in place |
| Q27 | Remove the four leftover worktrees without archiving |
| Q28 | Gate (a) runs overnight on neo while it is on AC |
| Q29 | Lane PRs merge once their R-N71 review is clean and CI is green |
| Q30 | The operator is in the lab until 05:00; neo's quiet window is 05:00–12:00 |
| Q31 | Keep going as is under heavy sting load |
| Q32 | Hybrid formal model: TLC is the checker of record; a Dhall catalogue renders its configs; a Haskell N-version explorer re-checks a core config |
| Q33 | Gate (a) waits for the full post-train main, with B pinned by `--rev-b` |
| Q34 | The S2 budget uses the v0 synthetic workload with interleaved OFF/ON windows |
| Q35 | S3 byte counters and the CPU ratio from sting count as evidence for Q15; estate verbs on the synthetic corpus are a test |
| Q36 | The SQLite `-shm` wal-index is admitted under the Q16 exception |
| Q37 | WP0(g) is ratified with conditions: row commits may be relaxed, the authority commit stays FULL, and it lands only after #161 |
| Q38 | Overnight scope is sprint 2 plus fix lanes for #161 and #162 |

This branch records Q24–Q26, Q34 and Q36 in `docs/slo.md`. It also fixes the
P35 row in the property-test plan, which still named a test WP1 had removed.

## Merged to main (10 PRs)

| PR | Merged as |
|---|---|
| #145 | `adb9c66` |
| #144 | `04ea9cb` |
| #152 | `6268175` |
| #146 | `46587af` |
| #154 | `4a7b86b` |
| #153 | `4a10bb8` |
| #159 | `cb681d3` |
| #160 | `edf6120` |
| #150 | `8d1edd3` |
| #151 | `dfb9604` |

Every train PR got a merge-from-main, a check-fast run, and a scoped
adversarial merge review. Low findings are in #156 and #158. Lane PRs also got
a separate review of their fix rounds before merging.

## Filed

- #156: S2 follow-ups.
- #157: the `-shm` counter and property test.
- #158: merge-review lows.
- #161: durability. The store's state root is created without sealing its parent.
- #162: S2 source-object freshening during v1 capture, and an S4 gap where bare repos refuse with an untyped IO.
- #163: a failed source-ledger commit should be counted, not fatal.

## Incidents and lessons

- **Duplicate agents (coordinator error).** I sent redirect messages to running
  workflow agents. Each first message resumed a second copy of the agent instead
  of reaching the live one, so lanes A, B and C each had two writers in one
  worktree. In lane B one copy's TLC run was destroyed. The fix was to stop the
  lane B workflow and relaunch every lane as a fresh workflow with a single
  owner. Rule: never message running workflow agents. To redirect a lane,
  relaunch it.
- **CI timeouts under sting load.** A job that runs past its `timeout-minutes`
  ends as "cancelled". The train now reruns such a job once.
- **GitHub GraphQL budget.** The shared GraphQL budget ran out with the lanes
  running. The train now uses only REST calls: pulls, check-runs, merge and
  update-branch.
- **GitHub mergeability right after a retarget.** Immediately after a retarget,
  GitHub can report a false conflict. Merge main locally, run check-fast, and
  push instead.
- **Lane fix rounds.** These need their own verification pass. The pass on #164
  found that the fix round was stale against events from the same night.

## Open (owners and next actions in the SSOT "Overnight" section)

- Gate (a) runs on `dfb9604` as user unit `bulkload-r23-gate-20261004`, retrying until 11:30 EDT.
- #164, the whitepaper, is being refreshed and needs a verification pass.
- Five overnight lanes are running: the #161 fix, the #162 fix, the S3 harness with the Q15 packet, the Dhall + Haskell hybrid, and the S2 instrument.
- For the operator:
  - Q15;
  - the reading of "held durably" in R25;
  - the slo.md wording of the model's R25 obligation;
  - whether to freeze MC_nv_ledger;
  - lane A's seal modes and archive use;
  - #163.
