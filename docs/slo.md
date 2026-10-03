# Bulkload SLO and policy charter

Ratified by operator interview 2026-10-03 (OI-1003-Q1–Q12), recorded under R-N13
and on Linear TIN-4543. This charter defines what "bulkload is complete" means.
It also defines the SLOs every claim must be proven against. Where it conflicts
with an older gate, the dated ruling here wins. Change it only by a new dated
ruling, never by editing a number in place.

## Completion bar (OI-1003-Q4)

Bulkload is complete when its SLOs are **definable and provable**, not when a
migration happens to succeed. Three bodies of work make up the bar:

- **Proofs**
  - A whitepaper with a bibliography of prior art.
  - Runtime proofs that can be demonstrated.
  - A formal model of the protocol.
- **Engineering**
  - Simplifications and refactors.
  - An adversarial architecture review.
  - State-of-the-art parallelisation.
  - Performance-seeking structural examination.
  - Reduction of code and feature sprawl.
- **Migration in full.** The lab shape (neo → sting) and the migration
  relationship are defined well enough to run with confidence.

The single neo→sting migration runs once, after this bar is met (OI-1002-Q24).

## Core SLOs

| SLO | Definition | Proof obligation |
|---|---|---|
| **S1 Faster than rclone** | Gate (a): local copy beats rclone, B/A/B/A/B order, B passes only if all 3 B reps pass (OI-1002-Q30). Gate (b): a neo→sting pull beats rclone over sftp. No hard wall-clock SLA for the migration run itself (OI-1003-Q3). | Gated samples under R-N81/R-N91 on the sealed corpus, then evidence in `docs/evidence/`. |
| **S2 Never interrupt the source** (OI-1003-Q5, Q9) | **Proven properties:** bulkload never takes a git lock or any flock or lease on source repositories, never writes to the source, and never signals a process. It runs at background IO and CPU priority. **Measured budget:** with bulkload running, a reference agent workload on the source shows at most **+25 % p95 latency**, and bulkload adds at most **+2.0 to load1**. | The properties are covered by property tests plus the formal model. The budget is measured and recorded in every gated run. |
| **S3 Idempotent incremental completion** (OI-1003-Q6, Q10) | A rerun reads **0 content bytes** for unchanged seats (R25 / R-N58), and its walk is metadata only. On an unchanged estate a rerun finishes in **≤ 10 % of the first pass's wall-clock**. After N changed files, a rerun reads only those files' changed chunks. As neo's work lanes complete, re-running sync brings them over with nothing re-walked for content and nothing re-read. | Counters `source_bytes_read` and `transferred_content_bytes`, plus the rerun/first-pass ratio, recorded per run. R25 is also covered by the formal model. |
| **S4 Complete and accounted** (OI-1003-Q1) | The closure report shows **0 unaccounted**: every planned item is `applied`, `referenced-only`, or a typed refusal. Every typed refusal carries an operator-reviewed disposition (accept, re-carry, or abandon) before the run counts as complete. A bare `IO` or `FRAME_CODEC` never counts. A daily-work verdict on sting (#40) is required. | The native closure report plus a bound attestation ledger (#119, #133), and the disposition ledger. |
| **S5 Source stays live** (OI-1003-Q2) | Agents keep working on neo during the run. Bulkload tolerates movement as drift custody (R-N30, extended by OI-1003-Q11), and a final delta pass (#103) catches up. There is no freeze window. | Drift custody recorded per item; the delta pass is under S3. |

## Proof package (OI-1003-Q7)

- **Whitepaper and bibliography.** Covers design, invariants, the SLO
  definitions above, and the proof method. Positions bulkload against prior art:
  - rsync; rclone; git's pack protocol;
  - casync/desync; restic/borg; bup; Unison; ZFS send/receive;
  - content-defined chunking (FastCDC); BLAKE3;
  - crash-consistency literature (ALICE, CrashMonkey).
- **Formal model.** A TLA+ (or equivalent) specification of wire v5, `Held`,
  ledger commit and resume. It is model-checked for R25 (no committed capture is
  re-read), durability ordering and S2's no-write/no-lock properties.
- **Property tests, not fuzzing.** Decompose existing tests and checks into
  local property tests wherever they can be expressed as properties. Keep CI
  tests and actions as slim as possible, so that the tested corpus stays
  bounded. Fuzz testing is out of scope.

## Policy rulings

- **#41, amends R-N43 (OI-1003-Q8).**
  - A restore compares **effective** ignore rules, after normalising comments,
    order and duplicates.
  - It proceeds when the effective rules agree, and records any difference as
    custody in the outcome. Differences are never silently merged.
  - It refuses only on a real conflict: a rule that would hide a carried file.
- **#38 (OI-1003-Q11).** Extend R-N30 drift custody to HEAD moves and index
  rewrites inside a captured worktree. Record them as `captured-with-drift`, and
  let the next sync pass bring the new state.
- **#39 (OI-1003-Q11).**
  - A device preflight refuses before the walk when the private state and the
    corpus are on different volumes and the publish cannot cross.
  - Publish survives a cross-device move: copy, fsync, then rename on the
    target volume.
- **#66 (OI-1003-Q12).** All four lows are folded into the refactor wave, each
  as a stated property with a property test:
  - drift checks run before any write;
  - disk-full gives a typed refusal;
  - a non-regular source is refused up front;
  - stage-parent ownership is verified.
- **#124 (OI-1002-Q33).** Salvaged temporaries are kept only for refusals that
  touched bytes. Salvage is bounded, with a typed refusal when the bound is hit.
  R25 stays strict for anything the destination held durably.

## Priority (OI-1003-Q4)

1. Make S1–S5 provable: proof package, property-test decomposition, and
   SLO-recording runs.
2. Simplification, refactoring and the adversarial architecture review, along
   with the pre-migration correctness fixes.
3. Gates (a) and (b), measured against these SLOs.
4. Coverage inventory (#102, #103), then the single migration run, then the
   operator lifts R-N56.
