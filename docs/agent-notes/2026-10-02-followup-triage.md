# 2026-10-02: coordinator session — merges, closure, and the gate plan

**Seat:** sting coordinator, Claude Code session `neo:f1c4ead8`.

**Rulings:** these operator interviews, all cited in their receipts with
R-N13:

| Ruling | What it covers |
|---|---|
| OI-1001-Q2/Q5/Q7/Q11/Q14/Q15/Q16/Q17/Q18 | #75 and #77 fix and review rounds |
| OI-1002-Q8/Q11 | #80, #81 and #96 |
| OI-1002-Q3/Q4/Q5/Q10/Q12/Q13 | Cohort closure work |
| OI-1002-Q24 | The full neo→sting migration runs once, after bulkload is complete: it beats rclone (R23) and the SLO and feature gates are met |
| OI-1002-Q25 | W4 PR 3 lane and follow-up triage |
| OI-1002-Q26 | Closing obsolete issues |

**Linear:** TIN-4543 (engine), TIN-3692 (estate and cohorts).

## Merged

| PR | Merge | Contents | Review |
|---|---|---|---|
| #75 | `bc5e13c` | W6 M1 destination ingest, journal and crash resume | Six adversarial rounds, plus a review of the merge with main |
| #77 | `2eaa36a` | W4 PR 2, wire v5 and the digest-only ledger | Three rounds plus a merge review |
| #80 | `9115ffd` | `closure-report PLAN CORPUS SOURCE PRIVATE_STATE...` and the per-entry destination space preflight (`DESTINATION_SPACE_INSUFFICIENT`) | Two rounds |
| #81 | `5650c49` | Local-first test tiers: `just check-fast`, `check-optional`, `check-full` | One round |
| #96 | — | Cohort3 closure note | — |

**#75 rulings and reviews:**
- NFS and other network-filesystem destinations are refused with `GIT_DESTINATION_FILESYSTEM_UNSUPPORTED` (OI-1001-Q17).

**#77 rulings and reviews:**
- R25 stays strict (OI-1001-Q15): `Held` is sent only after the group commit, and salvaged temporaries are adopted, so a committed capture is never read again.
- Wire v5 is a hard cut. Peers built before `e99a768` refuse to connect.

**#80 rulings and reviews:**
- Bare `IO` and `FRAME_CODEC` refusals now count as unaccounted.
- The exact journal for the current capture is required.

## Cohort closure (receipts under `/srv/fast-local/jess/bulkload/receipts/`)

| Cohort | Native report | With attestation ledgers |
|---|---|---|
| cohort1 | 22 referenced-only, 4 unaccounted (untyped IO) | 3 present + 1 referenced-only: **pass** |
| cohort2a | 147 applied, 13 typed refusals, 9 unaccounted | 6 referenced-only + 3 source-absent: **pass** |
| cohort2b | pass | — |
| cohort3 | 73 unaccounted (repaired, never applied; #95) | 71 referenced-only + 2 refused: **pass** |
| cohort4 | not carried | deferred to the single migration run (OI-1002-Q24) |

- **Root cause of the 13 ENOENT refusals:** the 09-22 apply binary predates
  typed refusals. Two cases produced them: a capture-refused item had no
  `.capture`, and a missing `<repo>.worktrees` parent failed canonicalize.
  Current main returns typed codes for both.

## Process lessons

- **Use the CI toolchain locally.** Run gates through
  `nix develop .#default --command just ...`. Sting's plain-shell cargo is
  1.93 while CI uses 1.96.1, and the gap gave false greens twice:
  `result_large_err` on #75 and `duration_suboptimal_units` on #77.
- **Lanes need private scratch.** Lanes sharing a scratchpad overwrote each
  other's logs; give each lane a private subdirectory.
- **Re-review merges of main into a branch.** #75's merge brought #74 in,
  and #77's merge had conflicts with #75 in the fault-point list. Both merge
  reviews came back CLEAN.

## Gate plan (Linear TIN-4543, labels `gate-a` / `pre-migration` / `later`)

1. #88 measurement (A/B/A/B/A; needs AC power and load under 2.5, R-N81).
2. W4 PR 3, #46 (streaming walk, component-wise `openat`). In progress.
3. Gate (a), R23.
4. W5 (#47), with #48 and #49.
5. Gate (b).
6. Pre-migration fixes:
   - #82, #86, #87, #92, #97, #100
   - #94, #95, #101, #106
   - #38, #39, #41, #66, #89
   - #40
   - #104, #105
7. Coverage inventory: #102 (all of neo) and #103 (the post-09-30 delta).
8. The single migration run, with closure-report at 0 unaccounted.
9. Operator lifts R-N56.

Closed today as obsolete or completed: #24, #33 (retired Python engine) and
#43–#45 (W1–W3 epics, merged).
