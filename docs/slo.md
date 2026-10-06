# Bulkload SLO and policy charter

Ratified by operator interview 2026-10-03 (OI-1003-Q1–Q13), recorded under R-N13
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

**Completion means confidence in the code and the product (OI-1003-Q13).** The
full neo→sting migration is not a major scheduled event. It is one more run of
a tool that is already trusted, no different from any other sync. Because S3
and S5 hold, it can be started whenever convenient and repeated as neo's work
lanes finish, and every rerun converges without re-reading anything. OI-1002-Q24
(no full migration before bulkload is complete) still stands. What
OI-1003-Q13 removes is the framing of that run as a one-off milestone.

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

## Amendments 2026-10-03: architecture rulings (WP0, OI-1003-Q15..Q20)

The [architecture review](plans/2026-10-03-architecture-review.md) work package
WP0 was ratified as follows. The
[property-test plan](plans/2026-10-03-property-test-plan.md) implements S2 and S3
as properties.

- **(a) Git carry engine: measure first (OI-1003-Q15).**
  - Land counted v1 pack reads, a `census_walks` counter, and v1 bundles that
    derive their prerequisite from the previous retained capture's tips.
  - Then measure S3 on the estate-shaped corpus (e).
  - Choose v1, hybrid (v1 estate layer with v2 negotiation) or v2 on the numbers.
  - Until then, carry_v2 stays frozen behind a feature. Its open PRs (#136) are
    held, and new work avoids it.
- **(b) S2 typed source access (OI-1003-Q16).**
  - Every source access is typed by kind: file read, an allowlisted git read
    command, or SQLite backup.
  - The SQLite backup API's shared read lock on a live provider database is the
    one stated exception to "no locks". It is shared-read only, bounded in
    duration, and counted. Everything else stays lock-free.
- **(c) S3 delta, restated as two inequalities (OI-1003-Q18).**
  - Source content bytes read ≤ the sum of sizes of changed or racy seats.
  - Wire content bytes ≤ the sum of absent chunks.
  - The unchanged-estate clause (0 content bytes, ≤ 10 % wall-clock) is
    unchanged.
- **(d) Superseding publish (OI-1003-Q18).**
  - A rerun may replace a destination output only when the output's
    (dev, ino, stat) equals this store's own ledger row: bulkload wrote it, and
    nothing has touched it since.
  - Anything else stays no-clobber.
  - New crash traces prove the replacement ordering.
- **(e) Estate-shaped S1 corpus (OI-1003-Q19).** A deterministic, sealed
  generator of git-heavy, many-small-file trees. S1 is measured on it in
  addition to R23's corpus v1.
- **(f) Background priority by default (OI-1003-Q17).**
  - Source-side verbs (serve, estate-capture, snapshot, estimate) enter
    background CPU and IO priority at startup:
    - Linux: nice 19 and ioprio IDLE.
    - Darwin: IOPOL_THROTTLE and QoS background.
  - Every S1 sample records its class.
  - Gate (a) may opt out only with an explicit, recorded flag.
- **(g) Relaxed source-ledger durability (OI-1003-Q20).**
  - The source-side ledger may run with `synchronous=NORMAL` and
    `fullfsync=OFF`.
  - Losing a source row costs at most a re-read. R25 is carried by the
    destination's durable records.
  - This holds only if proven in the formal model.

## Amendments 2026-10-03 (evening): salvage, priority and legacy rows (OI-1003-Q24..Q26)

- **#124 salvage bounds (OI-1003-Q24).** At most 1024 salvaged temporaries
  and 4 GiB of them outlive a session per destination root. Past either
  bound, the temporary is removed and refused as a value
  (`SALVAGE_BOUND_EXCEEDED`). This ratifies the #154 defaults.
- **WP0(f) scope (OI-1003-Q25).** Background priority covers every verb that
  reads a live source on its own host:
  - `serve`, `estate-capture`, `snapshot`, `git-carry-estimate`, `git-export`
    and `copy`;
  - not `pull`, whose source half is the remote `serve`.

  On Darwin the class is IOPOL_THROTTLE, QoS background and nice 19, which
  extends (f)'s Darwin list. `--priority=normal` opts out and is recorded as
  `priority_from=flag`. The bench records its class with every sample.
- **#125 legacy rows (OI-1003-Q26).** A store written before the racy-capture
  guard is opened in place, not refused.
  - Its ledger and output rows are invalidated in one transaction and counted
    in `transfer_legacy_rows_invalidated`.
  - Each such seat is read once more; R25 allows this because its row could
    not prove the seat was not racy.
  - Chunk hints are kept, so the re-read costs no wire bytes for content the
    destination still holds.

## Amendments 2026-10-03 (night): S2 measurement and the SQLite wal-index (OI-1003-Q34, Q36)

- **S2 measured budget (OI-1003-Q34).** The budget is measured against the
  v0 synthetic reference workload, in a sibling directory on the source's
  device. Each operation runs at a fixed 1 Hz cadence and records its own
  latency:
  - JSONL append plus fsync;
  - a SQLite WAL transaction;
  - git status and diff;
  - rg over a tree.

  The protocol interleaves OFF and ON windows of at least 5 minutes each. A
  result is INCONCLUSIVE when the OFF-window noise floor exceeds half the
  budget. The budget is recorded in every gated S2 run. Those runs are separate
  from the S1 gate, because a reference workload inside S1 would break
  R-N81's per-arm load1 < 2.5.
- **SQLite wal-index (OI-1003-Q36).** This extends WP0(b)'s SQLite exception
  (OI-1003-Q16). A backup-API read of a WAL-mode source database may create
  or touch its `<db>-shm` wal-index. That is SQLite's own coordination file,
  holds no user data, and any live writer creates it anyway. The effect is
  counted and recorded in S2 evidence. The main database and its `-wal` must
  stay byte-identical, and a property test asserts that no other source
  write occurs.
- **2026-10-06, S2 wal-index counter (OI-1003-Q36, #157).** The counter is
  `source_wal_index_touched`, on every counters line and in S2 evidence
  (`s2_budget.py`). It adds 1 for each WAL-aware snapshot that leaves a
  `-shm` beside its source, whether or not the file's bytes changed: such a
  read always opens the wal-index read-write, maps it and locks it. P75
  SQLITE-SHM-EXCEPTION is the property.

## Amendments 2026-10-04: WP0(g) adopted, and the R25 reading (OI-1003-Q37, Q40)

- **WP0(g) adopted with conditions (OI-1003-Q37).** This ratifies (g) above on
  the formal model's evidence in `docs/formal/README.md`:
  - MC_wp0g and MC_wp0g_deep pass;
  - MC_wp0g_authority fails;
  - MC_store_root_unsealed fails.

  The conditions:
  - Source-ledger *row* commits may run with `synchronous=NORMAL` and
    `fullfsync=OFF`.
  - The commit that creates the source store, its authority, stays FULL.
  - The relaxation lands only after #161, which seals the state root and its
    parent. #166 merged that fix with power-loss proofs.
  - A corrupt or absent source ledger is treated as empty.
- **R25 reading (OI-1003-Q40).** "Never re-read a byte the destination already
  holds durably" means a byte that a **committed destination row** proves
  durable. That is `R25_NoDurableReread` in `docs/formal/`. It is the SLO's
  proof obligation, and it holds in the model.
  - After a crash between an output's durable publish and its row commit,
    those durable but unrowed bytes may be read once more. Salvage (#124, #154,
    OI-1003-Q24) bounds this.
  - Removing that re-read is the strict reading. It is tracked as an
    improvement in #169 and is not part of the SLO.
  - This narrows "the destination held durably" in OI-1002-Q33 to "a committed
    row proves it".

## Amendment 2026-10-06: the empty `-wal` (OI-1003-Q72)

- **2026-10-06, SQLite empty `-wal` (OI-1003-Q72, #157).** This extends
  OI-1003-Q36 to the empty `-wal`. A read-only, WAL-aware snapshot of a
  WAL-mode source that has no `-wal` may create an empty `-wal` beside it.
  It is a counted S2 exception like the `-shm`:
  - it is allowed only where no `-wal` existed, and the file is zero bytes;
  - the main database file stays byte-identical;
  - a `-wal` that existed before the read stays byte-identical;
  - the counter is `source_wal_created`, on every counters line and in S2
    evidence (`s2_budget.py`), beside `source_wal_index_touched`.

  Every source read is the locked, WAL-aware backup-API read (OI-1003-Q16).
  An unlocked `immutable=1` read of the source is not ratified and is not
  used. P75 SQLITE-SHM-EXCEPTION asserts both exceptions.

## Priority (OI-1003-Q4)

1. Make S1–S5 provable: proof package, property-test decomposition, and
   SLO-recording runs.
2. Simplification, refactoring and the adversarial architecture review, along
   with the pre-migration correctness fixes.
3. Gates (a) and (b), measured against these SLOs.
4. Coverage inventory (#102, #103) and the operator's lift of R-N56. From then
   on, migrating neo to sting is a routine, repeatable sync run (OI-1003-Q13):
   rerun as neo's work lanes complete, each run reaching closure with 0
   unaccounted.
