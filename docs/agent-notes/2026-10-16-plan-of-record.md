# 2026-10-16 plan of record: the neo→sting move and the bulkload product

Ratified by the operator on 2026-10-16. This note is the repo copy of the
plan of record. The live copy, with the daily checkpoints, is the Linear
SSOT document "Bulkload — product shape, SLOs, proofs and live workstreams
(SSOT)" on project P-TIN-177. The ruling ledger is Linear TIN-4543. Where
this note and an older plan, spec section or agent note disagree on
sequencing or on what "done" means, this note wins. R-N13.

## Axioms (operator's words, ruled)

1. **Same paths.** Work lands at its live path on the target.
2. **Nothing excluded** except truly recreatable content (Q185/Q192).
3. **Never overwrite.** Differences land beside as `<path>.neo-YYYYMMDD`,
   and refs come in as `neo-YYYYMMDD/*`.
4. **Never halt the source.**
5. **Done means the operator resumes their own sessions on the target**
   (`claude --resume`, `codex resume`). No other "done" counts.
6. **Every item is accounted for**: closure with 0 unaccounted.
7. **bulkload must beat rclone** on real many-file, cross-network estates,
   proven by measurement, not by model.

## Architecture as run on 2026-10-16

```text
plan (source, live repo, live workspace)
  -> estate-capture on the source
       (git bundles + seats; files via native `pull` with SQLite snapshots)
  -> native SSH wire
  -> estate-apply --adopt
       (land absent, adopt equal, refuse diverged -> beside + refs-in)
  -> estate-reseat      (claude/codex sessions keyed to the live paths)
  -> estate-handoff     (AGENTS.md/CLAUDE.md block, so the resumed agent
                         converges first)
  -> converge passes    (deltas only)
  -> closure-report
```

Every verb is on `main` as of `b7787bb` (#242, re-seat). The glue between
them is lane script in the operator's lane directory
(`inplace-20261016/scripts`), not in this repo. Milestone M2 folds that glue
into one verb.

## Milestones

| Milestone | Date | Exit criterion |
| --- | --- | --- |
| M0 sting live for dev | 10-17 | The operator resumes in 3 real lanes |
| M1 cutover | 10-17 → 10-18 | Lab's Q200 items closed; neo becomes a teletype; the lab and cluster seats move; the archival set (~485 GB) lands on the target's bulk disk in the background |
| M2 bulkload is one command | 10-19 → 10-23 | `estate-move` folds the glue plus the 7 fixes below; repeated on a second source machine with no hand edits |
| M3 bulkload proven | 10-24 → 10-30 | S1 against rclone on the same neo→sting link and estate; S3 zero re-reads on converge; S4 closure 0 unaccounted; evidence published; code mass cut |
| M4 tcfs | after M3 | The tcfs backend chain (P-TIN-143) |

### M0 checkpoints (UTC, night of 10-16 → 10-17)

| Checkpoint | Target | What it proves |
| --- | --- | --- |
| CP1 | ~00Z | i02 capture done and its diff reviewed |
| CP2 | ~02Z | i02 applied, refs-in done, telefreeze and partial clones repaired |
| CP3 | ~02–04Z | Session-state pulls done |
| CP4 | ~04–05Z | Re-seat, hand-off, `--check` |
| CP5 | operator's morning | Catch-up pass, then the resume test (the only gate) |

### The 7 fixes for M2

1. `estate-apply` creates missing parent directories.
2. Captures are re-pointable (item IDs hash the destination today).
3. Partial clones and malformed inventory are carried natively.
4. Pull destination state proves by content instead of by path.
5. Adaptive priority (macOS background I/O starved the captures).
6. Static or GC-rooted binaries (a nix GC broke the agent).
7. An `estate-move` verb folding the presence probe, adopt-or-beside
   planning, refs-in and carried-as-files.

## Cadence

- One fixed daily checkpoint at **13:00Z**, posted as a comment on the
  Linear SSOT document. It carries:
  - one metric: items landed / sessions re-seated / operator resume yes-no;
  - blockers;
  - decisions only the operator can make.
- Wide lanes run only when they serve the current milestone.
- Every claim names its evidence.

## Rulings that still bind (compact index)

Full text is on TIN-4543 unless noted.

| Ruling | Binds |
| --- | --- |
| Q185, Q192 | Nothing excluded except truly recreatable content |
| R-C841 | Q185 as written: credentials, tokens, user keys and neo's per-host sops identity are carried; the target keeps its own host keys and signing subkey |
| Q152, Q194 C18 | Never halt the source; no freezes or quiet windows |
| Q194 C1–C20 | Spec conflicts, standing; in particular C8 (session IDs byte for byte), C9 (no recompressed or symlinked session stores), C10 (lossless union, never a mirror), C11 (replace only with the original preserved beside), C14 (carry non-recreatable leftovers as files), C15 (neo Codex recovery-tree deletion cancelled until carried and verified), C17, C19 (the migrating session is carried too) |
| R-C844 | Done = the resume test plus a hash manifest; supersedes Q4, Q7, Q32, Q155 and refines Q194 C20 |
| R-C842 | R3 gate of record = end-to-end time to a verified working seat, against rclone on the same home and link; the gate (b) harness becomes the rclone arm; gate (a) stays only as a regression guard (Q194 C2) |
| R-C843 | Condemned artifacts deleted in a signed commit citing the spec (whitepaper, formal model except `EstateConverge.tla` until #237, clone-race lever notes); the rest labelled superseded; ruling records never deleted |
| Q196 | No builds or tests on neo; neo binaries are built elsewhere and shipped with their sha256 checked |
| Q197 | S1 samples of record use rig controls, never quiet windows |
| Q199 | Merge on a local gate receipt; no required CI checks |
| Q195 | Force push allowed |
| Q200 | Priority order; cutover waits for lab's four sting items |
| Q183, Q198 | Hand-off block; re-seat respell and regularize |
| R-C838 | Target quota rows (lab's knob); stage in the dev-seat project, not the estate project |

## Superseded by this plan

- The Q194 "P1 R0 / P2 R3" live board and the neo-full-20261016 batch
  framing as the M0 path: M0 is the in-place run (i01, i02) landing at live
  paths. Bytes already landed are data and are not deleted.
- The S1–S5, proof package and refactor milestones as top-level tracks:
  they are sub-gates of M3.
- The proof package as an exit criterion (R-C843, R-C844).
