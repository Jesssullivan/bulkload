# 2026-10-04/05: coordinator session — Q42 git-carry push, v1 ref table, pause

**Seat:** sting coordinator, Claude Code session `neo:f1c4ead8`.
**Earlier:** this note continues `2026-10-03-coordinator.md`, which covers rulings Q23–Q38.
**Linear:** TIN-4543, comments from 2026-10-04 14:45 EDT to 2026-10-05; the SSOT document's sections "Afternoon 2026-10-04" and "RESUME HERE (2026-10-06)".

## Rulings this session (operator interviews, R-N13)

| Ruling | Decision |
|---|---|
| OI-1003-Q39 | Run gate (a) under host pressure, labelled informational (`r23_ab.py --under-load`) |
| Q40 | R25 uses the committed-row reading (`R25_NoDurableReread`); the strict reading is tracked in #169 |
| Q41 | Clear the S3 scratch by literal path, keeping the JSON and logs |
| Q42 | Treat Q15 as a refactoring, language, test and formalisation push |
| Q43 | A pure Rust `decide()` core, with a Haskell reference copy as a differential oracle; Dhall holds the Basis/Decision unions; TLC checks custody only |
| Q44 | v1 is the git engine; carry_v2 is deleted after L2 and L3 |
| Q45 | Object-set laws gate merges; a byte allowance M is proposed later |
| Q46 | Re-root plus STATE/corpus GC that never deletes a link something depends on |
| Q47 | Let the under-load R23 run finish despite neo's memory pressure |
| Q48 | Clear `/home` caches right away |
| Q49 | Reclaim merged, clean worktrees |
| Q50 | Fix the harness, then rerun under load later |
| Q51 | Refresh the whitepaper after the carry_v2 deletion |
| Q52 | Hand the sting ENOSPC root-cause fix to the lab seat (xoxd-ai/lab#2233) |
| Q53 | Remove `~/tmp` entries older than 7 days. None qualified. |
| Q54 | Hold the carry_v2 deletion until v1 carries refs-heavy repos |
| Q55 | Use the ref-table capture format for every new capture, failing closed for older readers |
| Q56 | The Q54 precondition is met; run L5 |
| Q57 | Pause: L6a and L5 run to a reviewed PR; L6a merges if clean; L5 waits for the operator |

## Merged to main (since `cdfe5f4`)

| PR | What |
|---|---|
| #175 | Q42 L2: the estimate's random-DAG oracle property (P66), independent of carry_v2 |
| #176 | `r23_ab.py --under-load`: informational mode, refused non-B reps recorded, power checked per rep, 40 tests |
| #177 | Q42 L1: thin group bases (P64/P65). On the 64 MiB fixture, a head move went from 67.2 MB to 2.5 KB and first-pass item bundles from 268.8 MB to 10 KB |
| #180 | Q42 L3: large-ref probes and evidence. v1 refused about 97k+ carry-shaped refs; per-ref CPU was mostly header read-back |
| #179 | Q42 L4: `GitCarry.tla` custody model, Dhall unions, Haskell reference `decide`, 363 pinned rows |
| #182 | Q54: v1 ref table. Refs-heavy repos carry exactly; per-ref CPU fell from 0.53 to 0.04 ms; `GIT_INVENTORY_OVER_CAP`; fails closed for older readers (Q55) |
| #184 | Q54 evidence: real blahaj (123k refs) restores exactly, with its source census unchanged. Was in the train at writing |

These landed earlier the same day and are covered by the 10-03 note: #166–#168, #170–#173.

## Filed

- #169: R25 strict reading.
- #178: v1 large-ref defects (fixed by #182).
- #181: `import_base` refuses a missing plan base with a bare IO.
- #183: two `GIT_INVENTORY_MALFORMED` misattributions.
- xoxd-ai/lab#2233: sting ENOSPC root causes.

## Measured (informational, ungated)

- **Under-load R23 on neo** (B = `cd4ffad`, swap nearly exhausted): native initial copy 9.3 / 38.8 / 18.1 s against rclone's 112 / 221 s. The 1% delta was mixed, with one 10-minute native outlier. The sample aborted when the A baseline failed with EPIPE, which #176 now records instead. **This is not a gate verdict; S1 is still NOT MET.**
- **S3 on the estate corpus** (#173): unchanged reruns read 0 content bytes, in 0.6–1.7% of the first pass's wall time. Git inequality 2 failed before L1 (worktree re-pack). SQLite has no S3 path.

## Incidents and lessons

- **Subagent weekly limit, then API 529 overloads.** Lanes stalled and were resumed with `resumeFromRunId`; completed stages replay from cache.
- **GitHub GraphQL budget exhaustion.** The merge train now uses REST only: pulls, check-runs, merge, update-branch.
- **`find` is `bfs` on sting** and rejects relative dates (`-newermt "7 days ago"`). The error was swallowed, so a manifest misclassified recent entries. It was caught before deleting anything. Use `-newer <ref>` or numeric `-mtime`.
- **Fix rounds need their own review.** A recheck stage before each PR caught fix-introduced defects in L3 (CPU attribution) and #164 (staleness).
- **sting disks.**
  - `/home` reached 99% because tool caches default to `~/.cache` while `/srv/cache` is idle.
  - `/` hit ENOSPC from Docker image staging.
  - This session reclaimed about 80G of bulkload worktrees and about 4G of Bazel caches.

## Open (owners and next actions in the SSOT "RESUME HERE (2026-10-06)" section)

- **Running at pause:**
  - L6a: `decide()` core, P67.
  - L5: carry_v2 and M1 deletion, tag `carry-v2-final`. Its PR waits for the operator (Q57).
- **Next:** L6b (grouped chaining, P68), then L7 (manifest reuse and sidecar order, P69/P70), then L8 (re-root and GC, P71), then the whitepaper refresh (#164, Q51), then L9 (the `git_carry.rs` split).
- **Waiting on others:**
  - S1 needs a quiet gated window on neo, or an under-load rerun once neo's memory is relieved (operator).
  - The S2 gated run is #165.
  - The neo estimate pass from L3 is pending neo.
- **Operator questions carried over:**
  - the per-capture metadata allowance M terms (L3 data);
  - whether to freeze the MC_nv_ledger and MC_gc_core names;
  - lane A's seal modes;
  - the #163 ledger-failure policy;
  - symref targets in the ref table;
  - the Gerrit-scale distinct-object cap (about 151k).
