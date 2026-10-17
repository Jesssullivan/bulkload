# Bulkload SLO and policy charter

Ratified by operator interview 2026-10-03 (OI-1003-Q1–Q13), recorded under R-N13
and on Linear TIN-4543. This charter defines what "bulkload is complete" means.
It also defines the SLOs every claim must be proven against. Where it conflicts
with an older gate, the dated ruling here wins. Change it only by a new dated
ruling, never by editing a number in place.

**Scope (2026-10-16, editorial under R-N13; no new ruling).** This charter is
the canonical source for the SLO definitions, their proof obligations and the
rulings that amend them ([SOURCES.md](SOURCES.md)). What bulkload must achieve
is [SPEC.md](SPEC.md) (bulkload#236); its conflict readings C1–C20 are
standing rulings (OI-1003-Q194, Linear TIN-4543 comment `14484c4a`). SPEC
does not supersede the Completion bar below: it keeps it as OI-1003-Q4's
record (intent ledger SEAT-10; RUL-06 "Unchanged: SEAT-10") and adds when
bulkload is done (C20). A superseded line below keeps its text and carries a
bracketed `[2026-10-16: …]` pointer to what replaced it. The 2026-10-16
amendment at the end indexes rulings not yet indexed here.

## Completion bar (OI-1003-Q4)

[2026-10-16: SPEC.md keeps this bar as OI-1003-Q4's record (SEAT-10) and
adds when bulkload is done: OI-1003-Q194 C20, the full run meets SPEC R0
and R3 is proven. The intent ledger cites the second paragraph as the
ruling record of OI-1003-Q13 (ONCE-08).]

Bulkload is complete when its SLOs are **definable and provable**, not when a
migration happens to succeed [2026-10-16: agent wording, not in OI-1003-Q4
(SPEC.md, "Not superseded, and what is corrected"); it must not be read as
"the migration does not count". The migration in full is part of the bar,
below]. Three bodies of work make up the bar:

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
(no full migration before bulkload is complete) still stands [2026-10-16:
relaxed for S1 by OI-1003-Q142, and superseded on order by OI-1003-Q194 C6
(RUL-06): the full run does not wait for the proofs. SPEC also corrects the
reading of Q24 as a *single* run. See the 2026-10-16 amendment]. What
OI-1003-Q13 removes is the framing of that run as a one-off milestone.

## Core SLOs

| SLO | Definition | Proof obligation |
|---|---|---|
| **S1 Faster than rclone** | Gate (a): local copy beats rclone, B/A/B/A/B order, B passes only if all 3 B reps pass (OI-1002-Q30). Gate (b): a neo→sting pull beats rclone over sftp. No hard wall-clock SLA for the migration run itself (OI-1003-Q3). [2026-10-16: both gates are decided on the hermetic rig and a neo→sting run is a field confirmation (OI-1003-Q97, amendment 2026-10-07). OI-1003-Q194 C2 makes cross-host gate (b) on an estate-shaped many-file corpus S1's headline and the rig's gate (a) a regression guard, and C3 reads the 1 % delta as a real in-place change to about 1 % of files; see the 2026-10-16 amendment.] | Gated samples under R-N81/R-N91 on the sealed corpus, then evidence in `docs/evidence/`. [2026-10-16: R-N91 is read as superseded by OI-1003-Q194 C18 together with SPEC R3 ("Samples run on live hosts"); RUL-18 itself names CONF-01/06/07/08, not R-N91. R-N81 stands; how its load gate fits SPEC R3 is open, see the 2026-10-16 amendment.] |
| **S2 Never interrupt the source** (OI-1003-Q5, Q9) | **Proven properties:** bulkload never takes a git lock or any flock or lease on source repositories, never writes to the source, and never signals a process. [2026-10-16: with the counted exceptions OI-1003-Q16 (the SQLite backup's shared read lock), Q36 (`-shm`) and Q72 (empty `-wal`), below.] It runs at background IO and CPU priority. **Measured budget:** with bulkload running, a reference agent workload on the source shows at most **+25 % p95 latency**, and bulkload adds at most **+2.0 to load1**. | The properties are covered by property tests plus the formal model. The budget is measured and recorded in every gated run. |
| **S3 Idempotent incremental completion** (OI-1003-Q6, Q10) | A rerun reads **0 content bytes** for unchanged seats (R25 / R-N58), and its walk is metadata only. On an unchanged estate a rerun finishes in **≤ 10 % of the first pass's wall-clock**. After N changed files, a rerun reads only those files' changed chunks. As neo's work lanes complete, re-running sync brings them over with nothing re-walked for content and nothing re-read. | Counters `source_bytes_read` and `transferred_content_bytes`, plus the rerun/first-pass ratio, recorded per run. R25 is also covered by the formal model. |
| **S4 Complete and accounted** (OI-1003-Q1) | The closure report shows **0 unaccounted**: every planned item is `applied`, `referenced-only`, or a typed refusal. Every typed refusal carries an operator-reviewed disposition (accept, re-carry, or abandon) before the run counts as complete. A bare `IO` or `FRAME_CODEC` never counts. A daily-work verdict on sting (#40) is required. [2026-10-17: #40 was closed not_planned 2026-10-16 ~17:40Z; the S4 verdict carrier is Linear TIN-6592.] [2026-10-16: OI-1003-Q194 C14 adds a carried-as-files outcome, marked corrupt in closure, for trees and rescue copies bulkload cannot capture natively; no code records it yet. #40 tracks the handoff-verify probe verdict, not SPEC R0's resume check.] | The native closure report plus a bound attestation ledger (#119, #133), and the disposition ledger. |
| **S5 Source stays live** (OI-1003-Q2) | Agents keep working on neo during the run. Bulkload tolerates movement as drift custody (R-N30, extended by OI-1003-Q11), and a final delta pass (#103) catches up. There is no freeze window. | Drift custody recorded per item; the delta pass is under S3. |

## Proof package (OI-1003-Q7)

- **Whitepaper and bibliography.** Covers design, invariants, the SLO
  definitions above, and the proof method. Positions bulkload against prior art:
  - rsync; rclone; git's pack protocol;
  - casync/desync; restic/borg; bup; Unison; ZFS send/receive;
  - content-defined chunking (FastCDC); BLAKE3;
  - crash-consistency literature (ALICE, CrashMonkey).
- **Formal model.** A TLA+ (or equivalent) specification of wire v5 [2026-10-16: the wire is v6 since #218; `docs/formal/README.md` lists the modules], `Held`,
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
  R25 stays strict for anything the destination held durably. [2026-10-16:
  the bounds are OI-1003-Q24 below; R25's reading is OI-1003-Q40 below.]

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
    held, and new work avoids it. [2026-10-16: carry_v2 is deleted; amendment
    2026-10-05 below.]
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
  addition to R23's corpus v1. [2026-10-16: no S1 sample on it exists yet
  (the hermetic-rig amendment: "still owed"); OI-1003-Q194 C2 makes an
  estate-shaped many-file corpus the corpus of S1's headline gate.]
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
  - Amendment 2026-10-06 (#169): the strict reading now holds in the model,
    within stated limits. `R25_StrictNoDurableReread` passes in
    `MC_r25_unrowed_bytes`, `MC_r25_strict_deep`, `MC_r25_strict_main` (two
    seats) and, across a lost source authority, `MC_r25_strict_unsealed` and
    `MC_r25_strict_authority` (`docs/formal/`), with the capture record's
    adoption (`crates/bulkload-agent/src/transfer/unrowed.rs`). The limits:
    - Only a non-racy capture gets a record. A racy capture's unrowed output
      is read again, as before.
    - The record is an extended attribute. A file system without them, or an
      existing output adopted against a manifest whose mode gives its owner
      no write permission, keeps no record, and its unrowed bytes are read
      again. Both are counted (`transfer_capture_records_unset`,
      `transfer_unrowed_unproven`).
    - The model assumes the record can be written; it does not model its
      loss.
    - The Haskell explorer agrees with TLC on the model with the adoption
      (`MC_nv_core_adopt`, `MC_nv_ledger_adopt`). `MC_nv_core`'s count of
      record is still the transfer before #169; moving it needs a ruling.

    On the code, the power-loss proof
    `an_unrowed_output_is_adopted_without_source_reads`, P74 and
    `an_output_adopted_against_a_manifest_carries_its_capture_record` check
    it. `R25_NoDurableReread` stays the SLO's obligation (OI-1003-Q40).

## Amendment 2026-10-05: carry_v2 deleted (OI-1003-Q44, Q54, Q56)

- **WP0(a) closed: v1 is the Git carry engine.** OI-1003-Q15 and Q44 keep
  v1. OI-1003-Q54 held the deletion of carry_v2 until v1 carried refs-heavy
  repositories, and OI-1003-Q56 rules that precondition met by #182 and #184.
  WP2 PR 3 deletes carry_v2, its ingest and journal, the `git_ingest` fault
  points and the W6 M1 spike; tag `carry-v2-final` holds the last main with
  them. `git-carry-estimate` stays as a read-only verb (P66), and wire v5's
  reserved W6 frames stay until WP3's v6 cut. (a)'s "frozen behind a
  feature" text above is superseded by this line, not edited.

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

## Amendment 2026-10-06: no SQLite source read as root (OI-1003-Q76)

- **2026-10-06, SQLite provider refuses root (OI-1003-Q76, #157).** The
  SQLite provider refuses to snapshot as root, with the typed refusal
  `SQLITE_SOURCE_AS_ROOT`, before it opens the source.
  - **What is refused.** Every provider verb that opens a database to read
    it, when the effective uid is 0: `snapshot`, `compose`, `compose-state`,
    `hydrate-state` and `apply-state-candidate`. The refusal is by uid,
    whatever the source's journal mode.
  - **Why.** Run as root, SQLite re-applies the database's owner to the
    `-wal` and `-shm` it opens (`fchown`), and skips that call as any other
    user. Measured on sting: the same read-only, WAL-aware open plus backup
    leaves an existing source `-wal`'s ctime unchanged as uid 1000 and moves
    it as uid 0, while the `-wal`'s size, mtime and bytes stay the same. PR
    CI runs as root and P75 failed there on exactly that. It is a source
    metadata write outside OI-1003-Q16, Q36 and Q72, and neither S2 counter
    sees it.
  - **What it does not change.** The two counted exceptions (Q36, Q72) and
    the Q16 lock stand as they are for every other uid. No SLO number
    changes.
  - **Proof.** P75 SQLITE-SHM-EXCEPTION as root asserts the refusal and
    that the refused snapshot left the source directory byte- and
    metadata-identical (no `-shm`, no `-wal`, no ctime moved), then runs the
    whole property again in a child process that has dropped to an
    unprivileged uid. Where that drop cannot be made, the run says so on
    stderr and proves the refusal only.

## Amendment 2026-10-06: S1 gate (b) harness (OI-1003-Q66)

- The gate (b) harness is built (`gate_b.py`, `just bench-gate-b`) and its
  protocol is [plans/2026-10-06-s1-gate-b-protocol.md](plans/2026-10-06-s1-gate-b-protocol.md).
  It changes no SLO and no number above. Its choices are drafts until the
  operator ratifies them: SQLite seats are left out of the comparable set, the
  delta is a 1 % XOR, the arms run N/R/N/R/N, and gate (a)'s 2 GiB RSS cap
  applies. No neo run happens until gate (a) passes. [2026-10-16: gate (b)'s
  protocol was ratified in OI-1003-Q82 and Q89 (see the hermetic-rig
  amendment); a neo→sting run is a field confirmation (Q97); the full run's
  order is OI-1003-Q142 and Q194 C6.]
- Gate (b)'s rep rule is gate (a)'s rule with two unratified deviations
  [2026-10-16: one since the 2026-10-07 (fourth) amendment]. It
  gates the warm resume (0 bytes received, 0 content bytes read), but:
  - it runs no interrupted-resume phase, so `r25_interrupted_zero` is not
    part of a gate (b) verdict (the harness never signals a process);
  - its warm-resume reads are net of the 16-byte SQLite magic probes.
- R-N81's load bound (load1 < 2.5) holds on both hosts in gated mode.
  `--dest-load-limit` can only tighten it.

## Amendment 2026-10-06: S4 proof status (WP3 PR 3)

- **S4 is provable on the estate ledger (OI-1003-Q1; a proof-status line, not a
  new ruling).** Outcome records are a typed `Outcome` with
  `Refusal{code, site, errno}`, persisted with postcard and decoded with no
  bytes left over; legacy string records are mapped by a reader. Closure
  matches the enum. The `gate` passes only when every planned item is
  accounted and every typed refusal carries a disposition (accept, re-carry
  or abandon, with reviewer and date) from the `closure-dispose` ledger. A
  bare `IO` or `FRAME_CODEC` stays unaccounted: no disposition can name it
  and no attestation can close it, whatever reason the item is unaccounted
  for. Property P72 (codec round trip and legacy mapping) and P73 (green iff
  every refusal is dispositioned and no untyped IO exists) carry the proof.
  Transfer and SQLite provider outcomes join this ledger in WP3 PR 4; until
  then S4 is proven for estate items only. [2026-10-16: replaced by
  "S4 covers a whole run", amendment 2026-10-15 (later).]
- **What a disposition is bound to.** The ledger is bound to its plan (path
  and a digest of the plan's bytes) and SOURCE label. An item disposition is
  bound to the refusal instance it reviews (the item's current capture and
  its outcome record) and is recorded only while the item holds that
  refusal; a later refusal of the same code against another capture or with
  another record is pending review again.
- **Standing policies are open-ended (a design statement of WP3 PR 3, not
  yet an operator ruling [2026-10-16: ruled, OI-1003-Q74 below]).** A standing-policy disposition covers every
  refusal with its code under the bound plan and label, now or later. It is
  bounded only by the plan digest and the label. Whether that is the
  intended meaning of "operator-reviewed disposition" for S4 is an open
  question for the operator.
- **Codes that leave the taxonomy fail closed.** A recorded refusal whose
  code has since been deleted is unaccounted (`refusal-code-retired`); no
  disposition or attestation closes it, and a disposition row naming it
  counts for nothing. Only a verb recording a current outcome closes the
  item.

## Amendment 2026-10-06 (later): S4 disposition rulings (OI-1003-Q74, Q75)

- **Standing policies are open-ended in time (OI-1003-Q74, ruled as
  built).** A standing-policy disposition stands until the plan's bytes
  change: it covers every refusal with its code under the bound plan digest
  and SOURCE label, now or later. This replaces the "design statement, not
  yet an operator ruling" wording in the amendment above; the behaviour is
  unchanged.
- **A retired refusal code fails closed (OI-1003-Q75, ruled as built).** A
  record naming a code that has left the taxonomy stays unaccounted
  (`refusal-code-retired`); no disposition or attestation closes it.

## Amendment 2026-10-08: S4 refusal sweep (#219, #220, #162, #183, #126)

A proof-status line, not a new ruling (OI-1003-Q1). Each refusal below now
names the fact its site knows, so a disposition reviews the real cause.

- **New codes.** The taxonomy gains three codes, each raised by product code
  and pinned by a test that fails without it:

  | Code | Raised when | Was |
  |---|---|---|
  | `CAPTURE_ABSENT` | estate-apply meets a planned item whose capture the corpus does not hold (its capture refused, or none ran; the capture's own outcome record names why) (#219) | `SEALED_OBJECT_MISSING` |
  | `GIT_SOURCE_ALTERNATES path=` | a source borrows through alternates deeper than a capture follows (a store five down naming one not yet linked; self-references, cycles and diamonds are linked once, as Git links them), or through a quoted entry, decided at capture and by the estimate probe before any export; or a capture misses an object behind an alternates entry naming a store Git cannot open (#219) | `GIT_INVENTORY_MALFORMED`; `GIT_CHILD_FAILED` (estimate, missing lender) |
  | `SPACE_EXHAUSTED [path=]` | a write meets `ENOSPC` or `EDQUOT` and its site names the directory that filled: a capture's temporary under `TMPDIR`, a bundle stage (the directory that outlives it), a capture's Git child (PRIVATE_STATE, which nothing charges), a store's state directory (`Store::open`, the source ledger); with no path, a Git child of a verb that names no directory, such as the estimate probe (R-N121) (#220, #126) | bare `IO`, with or without an errno |

- **Existing codes at more sites.** A bare repository with an unborn HEAD
  captures as ref custody instead of refusing `GIT_CHILD_FAILED` (#162). A
  refs import into a missing destination repository, or a directory Git
  finds no repository in, refuses `GIT_REPOSITORY_NOT_AT_PATH` as that
  item's receipt instead of `GIT_INVENTORY_MALFORMED` or
  `GIT_DESTINATION_OCCUPIED`, or (an existing non-repository) aborting the
  whole apply with no item record (#183). A destination group commit whose
  store is out of space, and an estate-apply Git child out of space, refuse
  `DESTINATION_SPACE_INSUFFICIENT` (the writes the preflight charges)
  instead of `IO (errno 28)` or `GIT_CHILD_FAILED` (#126, #220). A store's
  I/O error is `IO` with its errno, never `SQLITE_INTEGRITY_CHECK_FAILED`
  (#126). A lender's repack under a capture of a source that borrows from
  it is drift custody, not `GIT_CHILD_FAILED` (#219, S5).
- **Old records.** No code left the taxonomy, so no recorded refusal becomes
  `refusal-code-retired`. A record written before this change keeps the code
  it names (`SEALED_OBJECT_MISSING` for a never-captured item, a bare `IO`
  for a full `TMPDIR`); re-running the verb records the new code, and only
  that record can be dispositioned under it.
- **Still open.** A bare `IO` carries its errno but no path; a path would
  need a new payload on the `IO` variant. An unborn HEAD in a non-bare
  repository still refuses `GIT_CHILD_FAILED`. The capture space preflight
  does not charge `TMPDIR` (#134).

## Amendment 2026-10-07: the hermetic rig is S1's gate of record (OI-1003-Q96, Q97)

- **Why.** neo and sting are live, busy hosts. R-N81's gate (AC power, load1
  below 2.5) almost never opens on them: from 2026-10-03 to 2026-10-07 every
  gated gate (a) attempt on neo was refused or aborted, on load or on
  battery power. A gate that
  cannot be sampled proves nothing.
- **The gate of record (OI-1003-Q97).** S1 gates (a) and (b) are decided on
  the hermetic rig, on the sealed corpus. Nothing else in S1 changes: gate
  (a) is still B/A/B/A/B with all three B reps passing (OI-1002-Q30), R-N81
  holds before every rep and every arm, and gate (b) keeps the protocol
  ratified in OI-1003-Q82 and Q89.
- **Hosts and roles (OI-1003-Q96).**

  | Host | Platform | Role |
  |---|---|---|
  | mbp-13 | Linux x86_64, 4 cores, 16 GB, XFS | Gate (a), local copy. Destination of both gate (b) runs. |
  | yoga | Linux x86_64, 8 cores, 16 GB | Gate (b) source, Linux to Linux. |
  | PZM | macOS arm64 | Gate (b) source, macOS to Linux, mirroring neo to sting. |

  [2026-10-16: the core counts in this table count logical CPUs; the
  2026-10-07 corrections amendment below corrects them (mbp-13 is 2 cores
  and 4 threads; yoga's cores were not probed).]

  Every rig file lives under one bounded directory, `~/git-bulkload/`, on
  each host: the clone, the builds, the sealed corpus and all run output.
- **The sealed corpus.** Gate (a) runs on R23 corpus v1 (OI-1002-Q28):
  23 regular files, 239,819,837 bytes, content identity
  `f4a7619f7b88f2e0e1eeadb5995c79a809eafa8b4a8cf3f82d1ff6f29b5b07c7`,
  generated on the host by `r23_corpus.py`, verified and sealed read-only.
  The harness refuses any other corpus in gated mode. The estate-shaped
  class of OI-1003-Q19 is unchanged and still owed.
- **Power on Linux.** R-N81 is not weakened. On Linux the bench and the
  harness read `/sys/class/power_supply`: `ac` only when a `Mains` supply is
  online, or when the host has no battery at all; a battery with no mains
  online is `battery`; an unreadable class is `unknown`. Only `ac` opens the
  gate. The exact rule is on `linux_power` in `bulkload-bench` and
  `r23_ab.py`.
- **The baseline arm on Linux.** The pinned baselines (A `7c3ecc7`, v4
  `41bf9a4`) only know `pmset`. They are built at their exact sha, unpatched,
  and run with a translator named `pmset` first on PATH that reports the
  Linux power state in pmset's words. B never runs behind it. A stays
  informational and the verdict rule is unchanged.
- **Open: the A control on the rig** [2026-10-16: ruled, OI-1003-Q103, the
  third amendment below]. On mbp-13 the pinned A fails on its
  own, before any power check matters: its copy refuses with `IO (errno
  32)` every time on the dry-run corpus and in one of three native-only runs
  on corpus v1 (2026-10-07, see the lane's agent note). A gated sample
  aborts when any rep is refused, so B/A/B/A/B cannot be relied on to
  complete there. Until the operator rules, `r23_ab.py
  --b-only-no-a-control` runs B/B/B under every other gated check and marks
  its verdict `(NO A CONTROL)`. Such a sample is reported; it is not a gate
  verdict of record. The operator's choices: accept B/B/B on the rig, pin a
  newer A that runs on Linux, or let a refused A rep be recorded without
  aborting, as OI-1003-Q50 does under load.
- **Every sample names its rig.** The report records the platform, kernel,
  machine, CPU model, core count, RAM and the filesystem type of the work
  root, so a sample from one rig is never read as another's.
- **Field confirmation.** A neo to sting run (gate (a) on neo, gate (b) as a
  neo to sting pull) is a field confirmation. It is run when the hosts
  allow, on the same corpus and harness, and it is reported beside the rig
  verdict. It never decides S1, in either direction: a field pass does not
  rescue a rig fail, and a field fail does not revoke a rig pass. A field
  fail is filed as an issue and explained.
- **What the rig does not cover.**
  - macOS code paths on gate (a). Gate (a) on the rig is Linux only until
    PZM has room for the corpus and five destination copies (it has about
    13 GB free). Until then, macOS local-copy performance is covered only by
    a field confirmation on neo. [2026-10-16: SPEC R4 says builds, tests
    and heavy work do not run on neo (FOOT-03, 2026-08-26; FOOT-10,
    2026-09-01); the newer R-N69 (about 2026-09-23) allowed prefixed local
    builds there. Which governs is open for the operator (`AGENTS.md`,
    "Agent lanes"); SPEC R3 leaves gate (b)'s hosts to a new question.]
  - APFS and the TinylandState volume. The rig's work root is XFS on
    mbp-13's internal SSD.
  - Tailscale link variance. The rig's hosts are quiet and close; the
    neo to sting link's jitter and tailscaled CPU cost (see
    `docs/evidence/m0-2026-09-23.md`) are seen only in a field confirmation.
  - Estate scale and live-host pressure. `--under-load` samples stay
    informational (OI-1003-Q39).
- Runbook: [plans/2026-10-07-s1-hermetic-rig.md](plans/2026-10-07-s1-hermetic-rig.md).

## Amendment 2026-10-07 (later): corrections to the hermetic rig amendment (OI-1003-Q96, Q97)

These come from the review of the amendment above, which is left as
written. Where the two differ, this section is right. Nothing here changes
a ratified number or rule; what needs the operator is listed at the end.

- **mbp-13 is 2 cores and 4 threads.** The i7-5557U has 2 physical cores
  with 2 threads each. "4 cores" in the hosts table counted logical CPUs,
  because the harness printed `os.cpu_count()` as cores. The report now
  records `cpu_physical_cores` and `cpu_logical` separately. yoga's "8
  cores" was counted the same way and has not been probed again.
- **R-N81 is unchanged in text, but looser in effect on this rig.** The
  load bound is an absolute load1 below 2.5. It is not scaled to a rig's
  core count. On a 2-core host, 2.5 is more than every physical core busy,
  so the same number admits more on mbp-13 than it does on neo. The one
  sample so far shows it: the harness compiled B inside the sample for
  2 m 54 s, rep 0 began seconds later at load1 2.03, and the three B reps
  ran at 1.8 to 2.0 on a host that idles at 0.00 to 0.10. Both arms of each
  rep shared that state, so the 2.4 times FAIL is unlikely to turn on it; a
  close result would. Since then the harness records load1 right after the
  builds, how long it waited and the reading that admitted rep 0, and
  names every revision compiled inside the sample; `--build-only` prebuilds
  so that none is, and the runbook requires it. The bound itself is not
  changed here.
- **The rig is asserted, not only recorded.** `RIG_OF_RECORD` in
  `r23_ab.py` is the committed list of gate-of-record hosts for gate (a):
  mbp-13, pinned to Linux, x86_64 and its product name. A sample from a
  listed host has `rig_role=record`; from any other host, `rig_role=field`.
  The status line, the evidence title, the verdict line and the default
  evidence file name all carry the rig and its role. `of_record=true` needs
  gated mode, a `record` rig, the A control and a completed sample. A gated
  sample is refused when an identity field cannot be read, when a host has
  a listed name but not the listed identity, or when its evidence file name
  lacks the rig's name. A field sample therefore cannot be read as the S1
  verdict, and a rig sample cannot be read as a field one.
- **B/B/B is weaker than B/A/B/A/B, and says so in one word.** The
  ratified order has never completed on the rig, and the A-behind-translator
  path has never run against a real A binary there. In B/B/B the three B
  reps run back to back (33 s in the one sample), so "all three B reps
  pass" would be three readings of one host moment, with no baseline rep to
  show the rig was behaving normally. Its verdict is now the single token
  `PASS-NO-A-CONTROL` or `FAIL-NO-A-CONTROL`, with `of_record=false`. It
  stays reported only. A's `IO (errno 32)` on Linux is not diagnosed, so it
  is not shown to be absent from B.
- **What the rig does not cover: additions.**
  - *The seal is a different operation on each platform.* In group mode the
    native arm seals each file with `fsync` on Linux
    (`durable::seal_file`), which makes data and metadata durable and, on
    XFS, flushes the device write cache. On macOS it uses `F_BARRIERFSYNC`,
    which orders the writes without that flush. (The io layer's own
    `sys::barrier` on Linux is `fdatasync`; the group seal does not use it,
    so a later `fchmod` is durable with the data.) The rclone arm is run
    with no sync flag on either platform. So gate (a) on the rig compares a
    flushed native copy with an unflushed rclone copy, and the native arm
    pays more for its seal there than on neo. In the one sample, seal time
    is 64 % of the native initial wall time, and rclone's 362 ms for 240 MB
    is a page-cache copy. The rig verdict is not the same test as gate (a)
    on neo: a rig fail beside a field pass could be this difference alone.
    The bench header now prints `seal_primitive`, and the evidence names
    it.
  - *The rig's hardware class.* Two physical cores carry rclone's four
    transfers and the native workers. A result here is a result for a
    2-core laptop of 2015.
  - *A load bound scaled to the rig.* See the R-N81 bullet above.
- **For the operator.** Each of these needs a ruling; none is assumed.
  1. Does the rig need a tighter load bound than 2.5 (for example one
     scaled to physical cores), or a required quiet period after a build?
  2. Is gate (a) on Linux meant to compare a flushed native copy with an
     unflushed rclone copy?
  3. The A control on the rig, as asked above. If B/B/B is accepted, should
     the B reps be spaced (a v4 or rclone-only rep between them)?

## Amendment 2026-10-07 (third): the rig's A control and its rclone (OI-1003-Q103, Q105)

The operator ruled on question 3 of the section above (OI-1003-Q103) and on
the rig's rclone (OI-1003-Q105). The verdict rule does not change: B passes
R23 if and only if every B rep's bench verdict passes, and A is
informational (OI-1002-Q30). Questions 1 and 2 of that section are still
open.

- **The rig pins a newer A, and keeps B/A/B/A/B (OI-1003-Q103).** On mbp-13
  the A control is `3931471738cc3995a0e564e73af3134d7a7f1ff2`: main at the
  merge of #195, the last main commit before the rig work of 2026-10-07. It
  is `RIG_BASELINE_A` in `r23_ab.py`, pinned 2026-10-07. Every other host
  keeps `7c3ecc7`. B/B/B (`--b-only-no-a-control`) is no longer how the rig
  samples.
- **What A is for on the rig.** A is a fixed commit run between the B reps.
  B changes with main and A does not, so a change in A's numbers, inside a
  sample or between samples, is a change in the rig. A shows drift of the
  rig. It is not a comparison of old code with new, and it never decides
  the gate. When the pin changes, A's numbers on either side are different
  series.
- **Why `7c3ecc7` could not be that control, measured.** Native-only runs on
  mbp-13, 2026-10-07, each a full native bench (initial copy, warm resume,
  interrupted resume, delta). "Refused" means the bench ended with a bare
  `IO (errno 32)`, which is EPIPE.

  | Bench built at | Corpus v1: refused / runs | Dry-run corpus: refused / runs |
  |---|---:|---:|
  | `7c3ecc7`, the old A | 4 / 15 | 15 / 15 |
  | `41bf9a4`, v4 | 0 / 15 | 0 / 15 |
  | `3931471`, the new A | 0 / 24 | 0 / 24 |
  | `5875610`, main on 2026-10-07 | 0 / 12 | 0 / 12 |

  Twelve of the new A's runs on each corpus were R-N81 gated behind the
  `pmset` translator, as an A rep runs; the rest, and every other row, were
  `--informational`. The host was not quiet throughout: the runs were back
  to back (load1 0.3 to 2.3), and another lane's build and measurements
  overlapped some of them (load1 up to 4.6). So the zeroes were measured
  under more pressure than a gated sample allows, not less.
- **What that says about #201.** Issue #201 is `transfer::copy` returning a
  bare EPIPE now and then on current main, seen on sting under a load of 80
  to 113. It did not occur on mbp-13 on current main in 24 runs (12 on each
  corpus). The old
  A's failure is not that rare race: it is every run on the dry-run corpus.
  It also reproduces off the rig, on sting, when the bench is held to two
  CPUs, and the new A does not (0 of 40 there). So something between
  `7c3ecc7` and `3931471` fixed it. It is the same symptom as #201 and may
  share its cause; #201 stays open, and the rig has not shown it. If a B
  rep ever refuses with `IO (errno 32)`, the sample aborts with no verdict
  and the refusal goes on #201.
- **A's power preflight.** No commit on main reads Linux power itself until
  the rig lane (#204) merges, so the new A runs behind the `pmset`
  translator of the first amendment, like v4. B never does.
- **The rig's rclone is the flake-pinned build (OI-1003-Q105).** Every
  sample on every host runs the rclone that the repo's `flake.lock` pins
  (`nix build --inputs-from <repo> nixpkgs#rclone`). Today that is `rclone
  v1.74.4`, nixpkgs `241313f4e8e508cb9b13278c2b0fa25b9ca27163`, store path
  `/nix/store/v5xbkynmfg8ml23d82m09s802nmj2r6f-rclone-1.74.4` on
  `x86_64-linux` and `/nix/store/5aw5dn7z12pnwjp1yghg9xar9bcl21fk-rclone-1.74.4`
  on `aarch64-darwin` (evaluated, not yet built by a rig lane). `RCLONE_PIN`
  in `r23_ab.py` records it.
  - A gated or under-load sample refuses any other rclone binary. A
    deliberate exception takes `--rclone-override-reason`; it is recorded,
    its verdict is `PASS-RCLONE-OVERRIDE` or `FAIL-RCLONE-OVERRIDE`, and it
    is never of record.
  - A gated sample is refused when the flake resolves to a build other than
    `RCLONE_PIN`. A version bump is therefore a deliberate, recorded change:
    `flake.lock`, `RCLONE_PIN` and a dated line here, in one change. Samples
    on either side of a bump are not one series.
  - Every report records the rclone's store path, version, binary sha256
    and the flake's nixpkgs revision, and says whether they match the pin.
    A rep whose bench header names another version aborts the sample.
  - This is the build the 06:52Z sample already used; no number changes.
- **`of_record` now also needs the pinned rclone.** `of_record=true` needs
  gated mode, a `record` rig, the A control, a completed sample and no
  rclone override. On a rig of record the A control must be the pinned
  commit, or the gated sample is refused.
- **The first sample under this pin.** 2026-10-07 14:10Z, mbp-13, gated,
  B/A/B/A/B, B `adfdf57` (agent and proto equal to main `8006085`), A
  `3931471`, the pinned rclone. Printed verdict: `FAIL`, `of_record=true`,
  0 of 3 B reps pass (every B rep: `r23_initial_win` and `r23_delta_win`
  false, both R25 zero checks and RSS true). B's medians: initial 856.695 ms
  native against 374.447 ms rclone, delta 140.475 ms against 107.739 ms.
  A's two reps: native initial rep medians 866.452 and 907.211 ms. A's
  series on this rig starts here.
  [evidence/r23-2026-10-07-1410Z-mbp-13-record.md](evidence/r23-2026-10-07-1410Z-mbp-13-record.md).
  Open question 2 of the second amendment (flushed native against unflushed
  rclone) bears on how to read it.
- Runbook: [plans/2026-10-07-s1-hermetic-rig.md](plans/2026-10-07-s1-hermetic-rig.md),
  sections 3 and 5.

## Amendment 2026-10-07 (fourth): S3 inequalities 1 and 2 on the file transfer (#186, #187)

This records what the code now does under the rulings above (OI-1003-Q6,
Q10, Q18). It changes no number and no ruling.

- **Inequality 1 holds with refused seats (#186).** A seat refused for its
  `SQLite` header is sniffed (16 bytes) once per stat identity, and its
  refusal is remembered in the source ledger; an unchanged refused seat is
  refused again, with the same code, without being opened. Sniff bytes are
  counted as `source_sniff_bytes`, apart from content (`source_bytes_read`,
  `read_source_file_bytes`). So the unchanged-estate clause reads 0 content
  bytes and 0 sniff bytes. The limits:
  - a seat that was racy when it was sniffed is not remembered, and is
    sniffed again until it is not (as a racy capture is read again, #86);
  - the source ledger's rows may be lost under WP0(g); a lost row costs one
    more sniff.
- **Inequality 2 and convergence hold for changed seats (#187, WP0(d)).** A
  changed seat whose output this store wrote, untouched since, is
  superseded by the exchange design of `docs/formal` (`MC_wp0d_exchange`),
  filled from the old output's own chunks, so the wire carries only the
  absent ones. The limits:
  - only an output with a committed row is this store's own: a reuse row,
    or an ownership row (its path and identity, no seat), which an output
    published from a racy capture, or exchanged into place by a publish
    whose row never committed, has in its stead (review fix below). Any
    other file is refused `DESTINATION_OCCUPIED`, as before;
  - a file system without an atomic exchange cannot supersede: a changed
    seat there is refused `DESTINATION_EXCHANGE_UNSUPPORTED` before it is
    staged, and does not converge. Whether such destinations need a
    fallback is unruled [2026-10-16: ruled, OI-1003-Q100];
  - a seat past the source's retention budget (512 MiB) is streamed whole,
    as an added seat of that size is (#77);
  - an output this session supersedes stays readable for later seats of the
    session through a bounded set of descriptors; past the bound, a chunk
    only that output held is sent again.
- **Proof.** `tests/s3_transfer_resume.rs`: P21 and P23 with refused seats
  and P21 over in-place changes, which were ignored and red, are green and
  unweakened; the power-loss and crash-resume harnesses hold the old output
  with its old row or the new output with its new row in every state
  (`tests/power_loss.rs`, `tests/fault_harness.rs`).
- **Refusal code.** A transfer's occupied destination path is
  `DESTINATION_OCCUPIED`; `GIT_DESTINATION_OCCUPIED` is the Git carry's
  alone.
- **Gate (b)'s second deviation is gone.** The 2026-10-06 amendment lets
  gate (b)'s warm-resume reads be "net of the 16-byte SQLite magic probes".
  `source_bytes_read` no longer holds them, so the raw counter is 0 on a
  warm resume, as gate (a) requires, and nothing is left to net out.
  `gate_b.py` no longer subtracts them (`content_bytes_read` is the raw
  counter, and the deviation is out of `deviations_from_gate_a`); gate (b)
  now has one unratified deviation, the missing interrupted-resume phase.
- **Review fixes (2026-10-07, same change).** What the review of #186 and
  #187 found, and what the code does now. None of this is ruled
  [2026-10-16: ruled, OI-1003-Q100 to Q102 below]; each is
  listed for the operator in the lane's note.
  - *A seat standing refused is not read on every run.* A seat the
    destination refuses `DESTINATION_OCCUPIED` (or
    `DESTINATION_EXCHANGE_UNSUPPORTED`) was read in full by every unchanged
    run to rebuild its manifest, the defect class of #186. The destination
    now remembers the refusal under the seat's row key and the identity of
    the file at the path, and refuses the entry when it is offered: 0
    source bytes. So the unchanged-estate clause reads 0 content bytes with
    such seats in the corpus too. The limits: a racy capture, or a file at
    the path that was not settled when it was read, is not remembered and
    is read again (as #86); a lost record costs one more read.
  - *An actively written file converges.* An output published from a racy
    capture has an ownership row, so a seat that changes again before a
    settled run adopts it is superseded, not refused on every run.
  - *An interrupted supersede keeps its ownership.* A superseding publish
    whose exchange took effect and whose row never committed leaves the new
    file with an ownership row, written by the next sweep from the
    publish's record, so a seat that changes once more still converges.
  - *The model does not hold these records yet* [2026-10-16: it does,
    OI-1003-Q102 below]
    (`docs/formal/README.md`, "What the #187 review added to the code and
    not to the model").

## Amendment 2026-10-07 (fifth): the superseding publish is ruled and modelled (OI-1003-Q100, Q101, Q102)

Operator rulings of 2026-10-07 on #187 and its review. They supersede three
statements of the amendment above: "Whether such destinations need a
fallback is unruled", "None of this is ruled", and "The model does not hold
these records yet". That amendment's text is left as written.

- **OI-1003-Q100: no atomic exchange, no superseding publish.** A
  destination file system without the atomic exchange of two names refuses
  a superseding seat up front with `DESTINATION_EXCHANGE_UNSUPPORTED`, as
  built: before anything is staged or asked of the source, and with no
  fallback. The old output keeps its row; the refusal is typed (S4) and
  remembered, so an unchanged rerun reads 0 source bytes. Such a seat does
  not converge on that destination, by ruling.
- **OI-1003-Q101: a racy publish is owned, never reused.** An output
  published or adopted from a racy capture gets an ownership row and no
  reuse row, as built. The ownership row lets a later change of the seat
  supersede the output; it never answers `Reuse`, so a racy output is
  never a reuse source (R25, #86).
- **OI-1003-Q102: model first, then merge.** The TLA+ model covers what
  #187 adds before it lands. `docs/formal/BulkloadTransfer.tla` now holds
  the intent of a superseding publish and its sweep, the ownership row,
  the remembered refusal and Q100's refusal, with five invariants checked
  by TLC on every row that has the superseding publish on: `NoClobber`
  (a destination file is replaced or removed only when its identity is one
  this store recorded), `SupersedeAtomic` (after any crash, the old output
  with its old rows or the new output with a row of its own, never mixed),
  `OwnershipNeverReuse`, `RememberedRefusalSound` and
  `ExchangeRefusedUpFront`. `R25_NoDurableReread` and
  `R25_StrictNoDurableReread` hold with the superseding publish on. Seven
  mutations each fail on their named property
  (`docs/formal/README.md`, "#187's records in the model"). The Haskell
  explorer does not have these records; they are pinned outside its
  domain, and its four counts of record did not move.
- **S3 status.** With #186 and #187 on main, WP0(c)'s inequality 1 (source
  bytes read ≤ sizes of changed or racy seats, with refused seats in the
  corpus) and inequality 2 (wire bytes ≤ absent chunks, for changed seats)
  both hold on the file transfer, within the limits the amendment above
  lists and Q100's. Evidence: P21 and P23 with refused seats and P21 over
  in-place changes (`tests/s3_transfer_resume.rs`), P78, the power-loss
  and crash-resume harnesses, and the model rows above. Still open under
  S3: the twins reading of inequality 2
  (`p21_pinned_twin_seats_cross_their_shared_chunks_once`, ignored and
  unruled), gate (b)'s missing interrupted-resume phase, and the model
  rows that do not yet combine the superseding publish with the relaxed
  source ledger, a lost source authority, estate reads or liveness.

## Amendment 2026-10-07 (sixth): WP0(g) landed as ratified (OI-1003-Q104)

The first gated gate (a) sample on the hermetic rig failed S1 (native
initial copy 873 ms against rclone 362 ms). OI-1003-Q104 ruled that the
first response is to build WP0(g), measuring first. This records what was
built under OI-1003-Q20 and OI-1003-Q37. It changes no number and no
ruling, and it is not an S1 result.

- **What changed.** The SOURCE ledger's ROW commits run
  `synchronous=NORMAL`, `fullfsync=OFF` (`LedgerSync::Relaxed`, the
  default; `--source-ledger-sync=full` restores the old behaviour for
  comparison). A power loss may lose the newest rows. A lost row costs at
  most one more read of its seat (one more sniff for a remembered refusal,
  #186), and only where the destination no longer holds that seat.
  - A row commit that fails is counted (`source_ledger_commit_failed`,
    `source_ledger_rows_dropped`) and the transfer goes on (#163).
  - A ledger read that fails is a miss (`source_ledger_unreadable`): "a
    corrupt or absent source ledger is treated as empty".
  - `source_ledger_relaxed_commits` counts the relaxed commits, and
    `source_ledger_miss_reads` the seats read because the ledger had no
    row for them, an upper bound on the rows lost and re-read.
- **What did not change.**
  - The commit that creates a store, with its authority, is
    `synchronous=FULL`, `fullfsync=ON`, on both sides, and the state root
    is sealed first (#161, merged as #166).
  - Every destination commit, file seal and directory seal, and the order
    between them: `Held` still follows the destination's FULL commit
    (R25, R-N58). The destination's commit path has no code change.
  - A checkpoint on the relaxed connection is still a full flush
    (`checkpoint_fullfsync=ON`).
- **Which R25 reading holds.** The committed-row reading (OI-1003-Q40,
  `R25_NoDurableReread`) holds with the relaxation (`MC_wp0g`,
  `MC_wp0g_deep`). The strict reading (#169) holds too, within #169's
  limits (`MC_wp0g_strict`). A relaxed authority still fails
  (`MC_wp0g_authority`).
- **Proof on the code.** P79 RELAXED-LEDGER-LOSS over generated crash
  points, and `tests/power_loss.rs` over every WAL prefix of a real copy:
  a seat the destination holds reads 0 bytes whatever the ledger lost; a
  lost row costs its seat once, only at a seat a third party removed; the
  authority survives every loss.
- **Measured, informational** (`docs/evidence/s1-seal-breakdown-2026-10-07.md`;
  mbp-13, corpus v1, no A control, not a gate sample): native initial
  copy 880.3 ms before and 880.0 ms after, against rclone at about 350 ms.
  The source row commits went from 35 ms summed to 0.2 ms; they were 4 %
  of the copy and off its critical path. The destination's file syncs
  (47 %), directory seals (9 %) and commits (15 %) are the rest, and this
  ruling does not touch them. WP0(g) does not close the S1 gap; what
  would is listed in that file for the operator and is not built.
- **Not ruled.** A source store that cannot be opened at all is still a
  refusal, not an empty ledger: opening it as empty would mint a new
  authority, which is exactly `MC_wp0g_authority`. `--source-ledger-sync`
  and its default are this change's; OI-1003-Q37 names the settings, not a
  flag.

## Amendment 2026-10-15: estate convergence ruled, with three stated limits (OI-1003-Q144, Q149)

- **Convergence is ruled (OI-1003-Q144).** A rerun of `estate-apply` may update
  a checkout bulkload itself landed only when its ledger proves it unchanged
  since landing, by content; any local change refuses
  `GIT_DESTINATION_MODIFIED` and is never overwritten. Branches and tags
  deleted at the source are kept and reported, remote-tracking refs follow the
  source, configuration changes are report-only. The `EstateConverge` model
  (`docs/formal/EstateConverge.tla`) passes against the code.
- **Three known limits (OI-1003-Q149).** None loses or overwrites work.
  1. A HEAD detached for a branch-name conflict re-attaches only at the next
     changed capture, not on a no-op rerun.
  2. A ref the operator changed and put back to the landed value re-converges
     only at the next changed capture.
  3. A concurrent `git switch` can race the HEAD locks: Git commits the index
     before it takes `HEAD.lock`, so no lock closes the window.

  Limits 1 and 2 follow from S3: an unchanged rerun reads 0 content bytes and
  does not re-read the ledgers. Their model rows fail `ConvergesWhenQuiet` as
  stated limits.
- **Existing landings** (before this change) carry no ledger and are not
  converged; `--adopt` is not built (OI-1003-Q145). [2026-10-16: built
  and merged (#233); amendment 2026-10-15 (#226) below.]

## Amendment 2026-10-15 (later): S4 transfers, S5 HEAD and index drift, the S2 harness alongside a run (OI-1003-Q152)

Proof-status lines, not new rulings (OI-1003-Q1, Q2, Q5, Q11, Q148, Q152,
Q155). No SLO number changes.

- **S4 covers a whole run (WP3 PR 4).** Every `copy` and `pull` session
  records its outcome in its DEST_STATE (`transfer.closure.d`, one record
  per transfer, so transfers sharing a DEST_STATE never replace each
  other's rows; a session that ends refused keeps the earlier session's
  refused rows; the S2 overlap checks run before the first record write),
  and `closure-report --transfer DEST_STATE` joins it to the same report,
  disposition ledger and gate as the planned estate items: a session row
  (`applied` when it ended; `unaccounted` when it never ended, left no
  record, or refused with a bare `IO`), one row per refused path, typed and
  pending review like an estate refusal, and one `covered` row per live
  SQLite sidecar a snapshot covers. An integrity failure
  (`SQLITE_INTEGRITY_CHECK_FAILED`, or `SQLITE_BACKUP_FAILED` with primary
  code 11 or 26) carries OI-1003-Q148's default `abandon`, which a review
  overrides. The run's expected transfers are bound to its PLAN by a
  transfer manifest (`transfer-expect`, `closure-report
  --transfer-manifest`): a listed transfer with no record, or whose session
  started before the manifest, is unaccounted, and no transfer row counts
  twice. With the manifest, one report shows 0 unaccounted over estate
  items and transfers, and a bare `IO` or `FRAME_CODEC` never counts.
  Without it the report covers only the transfers named. This replaces
  the 2026-10-06 amendment's "until then S4 is proven for estate items
  only". Still outside: the offline provider verbs (`snapshot`, `compose`,
  `compose-state`, `hydrate-state`, `apply-state-candidate`), which belong
  to no run. Proof: P86 (`closure/transfer_tests.rs`), the CLI row in
  `tests/space_closure_cli.rs`, report schema `bulkload.closure.v3`
  [2026-10-16: `bulkload.closure.v4` since #233, below].
- **S5: HEAD and index drift is custody (#38, OI-1003-Q11, under
  OI-1003-Q152).** An agent committing, switching or staging inside a
  captured checkout, or in a worktree nested in it, during a capture pass
  no longer refuses `GIT_AUTHORITY_CHANGED`. The pass records `HeadMoved`,
  `IndexRewritten` or `NestedWorktreeMoved` drift custody (also when the
  move went away and back across the windows outside the export, nested
  worktrees included); its key is poisoned, so it is never a reuse hit,
  and the next quiet pass carries the move. A capture whose export the
  move raced (in-band drift) is never laid down; a capture the move raced
  only outside the export's window (key drift) is a coherent snapshot of
  the state the export read and applies, and the closure counts it
  applied, as it would a clean pass the source moved past a moment later.
  So the run closes by converging deltas with no freeze. Still refusals [2026-10-16: the
  stash reflog is custody since OI-1003-Q161, next amendment]: the exclude file, the stash reflog,
  configuration, the shallow frontier, foreign nests and gitlinks, and the
  omitted roots moving under a pass. Proof: P87 (capture raced by a commit
  and a stage, then a switch; each recapture converges the landing to the
  source), the three git_carry drift tests, the `r3_*` round trips (a
  nested worktree's among them), and the EstateConverge rows
  `MC_ec_capture_raced` (pass; it models both drift classes and checks
  `NoDriftedLanding`), `MC_ec_reach_drift_carried` and
  `MC_ec_reach_key_drift_landed` (reached), `MC_ec_neg_drift_key_reused`
  (fails `ConvergesWhenQuiet`) and `MC_ec_neg_drift_applied` (fails
  `NoDriftedLanding`).
- **S2 budget alongside a real run (#165).** `s2_budget.py observe` samples
  the reference workload and load1 on the source host for the length of a
  real run and starts nothing; it reads the host's load (agents' and
  bulkload's) without touching any agent process. `alongside` joins it with
  the run's own intervals (`mark`, or `START END` lines) into OFF and ON
  windows and gives `analyze`'s verdict and evidence rows. It runs on Darwin
  (vm.loadavg, pmset). Each ON window starts one settle before its pass, so
  a pass's first minute is measured; missed ticks stream to disk; an
  observation with no recorded end, or whose wall and monotonic clocks
  part by more than 2 s, is INCONCLUSIVE; evidence needs the kept windows
  to alternate OFF/ON with at least two ON windows once same-state
  neighbours merge.
- **Open for the operator (not ruled here).** (1) Whether an alongside
  observation counts as a gated S2 run: as built, `--evidence` labels it so
  only with every kept window at least 300 s, at least five windows, the
  pinned gate, AC power and an OFF baseline under R-N81's 2.5, with no
  coordinator quiet (OI-1003-Q152 keeps neo live), so a busy neo stays not
  evidence. (2) Whether HEAD and index drift should also cover the stash
  reflog (`git stash` under a pass still refuses). Both are ruled in the
  next amendment (OI-1003-Q160, Q161).

## Amendment 2026-10-15 (ruled): S2 alongside a live baseline, the stash as drift custody (OI-1003-Q160, Q161)

Operator rulings, Linear TIN-4543, 2026-10-15. No SLO number changes.

- **S2 alongside a live run is judged against the source's own live load
  (OI-1003-Q160).** An observation alongside a real run (Q152; `s2_budget.py
  alongside`) takes the OFF windows between passes as the baseline, with
  **no absolute load1 cap**: R-N81's load1 < 2.5 is amended for alongside
  observations only. Standalone gated S2 runs (`s2_budget.py run`) keep
  R-N81 unchanged. The budget is unchanged: at most +25 % p95 latency and
  +2.0 load1 relative to the OFF baseline. Evidence still needs AC power at
  every window boundary, at least five alternating windows of at least
  300 s, and recorded load1 in every window; it is labelled `alongside,
  live baseline (OI-1003-Q160)`. A pathological baseline is guarded, not
  capped: the verdict reports the OFF windows' load1 levels and spread
  (still INCONCLUSIVE when the spread exceeds half the budget) and compares
  each ON window with the OFF windows on both sides of it, interpolated in
  time to the pass, which removes a linear drift whatever the windows'
  lengths. Pairs are judged gate by gate: a pooled PASS stands only when
  every pair passes both gates; a pooled FAIL stands when every pair fails
  a gate that failed pooled (a pair also over the other gate adds a reason,
  never a downgrade); otherwise the verdict is INCONCLUSIVE, keeping the
  pooled reasons and naming each pair, its worst gated metric and its
  baseline drift; a pass with no OFF window on one side is not judged. A
  PASS also needs the OFF windows after passes free of a recovery tail
  (pooled, their first settled half at most half the budget over their
  second, on p95 and load1), since a pass's cost that outlasts it sits on
  both sides of every pair. Evidence needs every pair compared on every
  gated metric, `--min-window-seconds` pinned at 300, no pass dropped as a
  short ON window (every pass is judged; a converge run's passes under
  about 240 s cannot be alongside evidence, and dropped passes and their
  seconds are reported), and no recorded pass ending within one settle plus
  300 s before the observation began. The pair rule removes linear drift
  only: host load correlated with the passes themselves (agents quiet
  while bulkload runs) is not separable from bulkload's cost, and the
  instrument never lists processes to separate it (R-N11, R-N104). Proof:
  `AlongsideLiveBaselineTests` and `LiveBaselineReviewTests` in
  `test_s2_budget.py`.
- **A stash push, pop or drop during a capture pass is drift custody
  (OI-1003-Q161),** as HEAD and index moves are (#38): the pass records a
  `StashMoved` row named `refs/stash` (beside any `refs/stash` ref row; a
  drop below the tip moves only the reflog, which only this row names);
  the raced capture is never laid down (in-band marker, `CAPTURE_DRIFTED`)
  or reused (its key is poisoned, a move away and back included), and the
  next pass carries the new stash, which the apply converges in the
  source's order. It no longer refuses `GIT_AUTHORITY_CHANGED`; the exclude
  file, configuration, the shallow frontier, foreign nests and gitlinks and
  the omitted roots still do. The stash path changes no protocol step: the
  EstateConverge model's raced moves gain the stash (`stashrace`), with
  rows `MC_ec_capture_raced_stash` (pass), `MC_ec_reach_stash_drift_carried`
  (reached), `MC_ec_neg_stash_drift_key_reused` (fails
  `ConvergesWhenQuiet`) and `MC_ec_neg_stash_drift_applied` (fails
  `NoDriftedLanding`). Proof: P87's stash row (`p87_an_agent_stashing_...`:
  push, pop and drop under passes, each converged), the git_carry rows
  `stash_pushed_during_capture_is_drift_custody` and
  `stash_dropped_during_capture_is_drift_custody`, and
  `r3_stash_reflog_round_trip_across_unguarded_windows_is_drift_custody`.

## Amendment 2026-10-15: S2 forbids a clone of a source file (OI-1003-Q164, Q165, Q166)

The clone-lever study (OI-1003-Q164,
[agent note](agent-notes/2026-10-15-s1-clone-lever.md)) measured that a
clone writes the source while every `stat` field stays the same. The
operator ruled the clone **excluded** (OI-1003-Q165), and S2 gains a typed
rule. No SLO number changes.

- **Why it is a source write.** `FICLONE`, `FICLONERANGE` and a
  same-file-system `copy_file_range` (which reflink XFS and btrfs serve as
  a clone) set the source inode's reflink flag in a logged transaction, bump
  its `i_version` and add references to its extents. Writers to the source
  stall for the whole remap, and every later write to a shared block is
  copy-on-write. Only `FIEMAP`'s `SHARED` flag shows it outside the kernel;
  the stat rows, the bench's source-row check and P34/P77's `lstat` census
  do not.
- **(a) No clone call on a source file (S2 property, OI-1003-Q165).** A
  source file is read by `pread`, or by `read`/`splice` into a pipe. It is
  never the input of `FICLONE`, `FICLONERANGE`, `FIDEDUPERANGE`,
  `copy_file_range`, `clonefile`/`fclonefileat` or
  `copyfile`/`fcopyfile` (remap). `sendfile` is allowed: it only reads.
  - Rust's `std::fs::copy` uses `copy_file_range` on Linux, and
    `std::io::copy` does when both ends are regular files. So both count as
    remaps for this rule.
  - **Enforced in the typed source access layer.** The rule is stated on
    the io layer (`io/mod.rs`, "Source access never clones"). P84
    SOURCE-NO-CLONE (`tests/source_command_registry.rs`) scans every crate's
    non-test code.
    - No remap call is named.
    - No clone ioctl number is written, matched by value in any radix,
      with `_` separators or a type suffix (`0x4004_9409`, `148`).
    - No `use` hides a copy: an import of `copy`, `fs` or `io` renamed
      (`self as` too), or a glob import of either.
    - Every raw `syscall` is a registered site named by its `libc::SYS_*`
      constant, so no number can reach `copy_file_range`.
    - Every `fs::copy` and `io::copy` must be a registered site whose input
      is argued not to be a source file. A registered site with generic
      parameters has its callers pinned too: the raw-tree capture's
      `io::copy` reads a source file, and its writer is pinned to `git
      fast-import`'s stdin, a pipe, for which std uses `splice`.
      `batch_objects::read_body`'s one caller passes `git cat-file`'s stdout.
  - **Proven on the kernel.** P84's kernel leg (`tests/source_lock_trace.rs`)
    proves on the host that `FIEMAP` sees a `FICLONE` and a `std::fs::copy`
    that the `lstat` census misses. Then three verbs must leave every source
    extent unshared: a real `copy`, a `serve` in its own process, and a v1
    Git capture, whose raw-tree pass reads every seat. A `SQLite` provider
    snapshot must too, as an ordinary user; as root the provider refuses
    to read a source at all (OI-1003-Q76).
    - The leg needs a directory on a reflink file system: `TMPDIR`, or else
      cargo's `CARGO_TARGET_TMPDIR`.
    - Where neither can clone, the leg says so on stderr and returns, and
      cargo still reports `ok`. The same holds for the bench's and the
      harness scripts' reflinked halves. So a pass count from the PR gate
      or `cargo test` is not proof that these legs ran.
    - `just p84-reflink` is that proof. It sets
      `BULKLOAD_P84_REQUIRE_REFLINK`, which turns each early return into a
      failure, and it requires each leg's own output line. It is on
      demand, like `props-deep`, and is not a PR gate.
  - **What `FIEMAP` can say.** It shows sharing that exists when it looks.
    A clone whose target is then overwritten in full, or unlinked and
    freed, leaves the source unshared again, although the clone call wrote
    the source inode. So a `0` means "shares nothing now", not "was never
    cloned"; the static half is what rules the call out.
    - `FIEMAP_EXTENT_SHARED` is trusted only on xfs, btrfs, bcachefs and
      ocfs2.
    - ext2/3/4 and tmpfs cannot reflink, so a file there counts as 0 by
      construction.
    - Any other file system (CIFS/SMB3, which clones but never sets the
      flag, NFS, overlayfs, ZFS) counts as `unsupported`, never `0`.
    - An unreadable directory fails the check instead of being skipped.
  - **The two destination-side `std::fs::copy` calls stay.**
    `git_carry::prepare_attachment` and `git_carry::registered::restore`
    copy bulkload's own private staged capture bundle into a receipt it has
    just created. Neither file is a source file, so S2 does not govern them.
    On a reflink file system the receipt may share extents with the staged
    copy; that is the study's Q-d, answered by Q165 for these two: they may
    stay.
  - **A third destination-side copy arrived with estate converge
    (OI-1003-Q144), after Q165.** `git_carry::converge::publish_index`
    copies the landed checkout's `.git/index` (on the destination; the
    operator's Git may have rewritten it since landing) to converge's
    private merged index beside it, under the `index.lock` it holds. Its
    input is never a source file, so S2 does not govern it, and P84 pins it
    with that reason. Q165 named only the two receipt copies; whether Q-d's
    answer extends to this one (the merged index may share extents with the
    live index until Git rewrites it) is not ruled.
- **(b) Rig corpus copies are read and write, verified unshared
  (OI-1003-Q165).** Every copy of the sealed corpus a sample reads is an
  explicit `read`/`write` copy. It is never `cp`, whose default is
  `--reflink=auto`, and never `shutil`, which uses `copy_file_range` on
  Linux from Python 3.14. The copy is checked with `FIEMAP`
  (`FIEMAP_EXTENT_SHARED` never set) before the sample runs, and a shared
  extent refuses the sample.
  - `r23_ab.py`'s working copy is `r23_corpus.plain_copy_tree`. It is
    checked before any build, and a shared extent refuses with exit 2 in
    every mode.
  - On Linux, a work root whose sharing cannot be known (above) refuses a
    gated sample, and an informational one records `unsupported`. Off
    Linux the check is `n/a`.
  - After every rep, the working copy and the rep's private fixture are
    checked again. A shared extent aborts the sample with exit 3.
  - The bench (`bulkload-bench`) checks its seeded fixture after the seed
    and after every arm, and refuses on a shared extent in every mode. Its
    header records `fixture_shared_extents` and
    `sealed_corpus_shared_extents`, and every sample row records
    `source_shared_extents`.
  - `gate_b.py`'s source working copy follows the same rule: a read/write
    copy, and a shared extent refuses SOURCE.
  - `r23_corpus.py copy` makes a read/write, verified, unshared copy of a
    corpus. `seal` refuses a copy that shares an extent, and `fiemap`
    prints a tree's count.
  - The sealed corpus is checked once per sample. Since Q166 re-sealed the
    rig's corpus unshared, a shared sealed corpus refuses a gated sample in
    both the bench and `r23_ab.py`; an informational one records the count.
  - The rig's own tooling, outside the repository, follows the rule
    (receipt `~/git-bulkload/logs/q165b-rig-tooling-20261015.receipt`,
    sha256 `5cc030df4a6ae8b1…`):
    - The sota driver's working copy is `~/git-bulkload/sota/src-unshared`,
      a read and write copy with 0 shared extents. The old `sota/src`, a
      `cp -a` reflink, is kept and no longer used.
    - The driver checks the copy before and after every sample, and refuses
      its clone configurations.
    - `diag.sh` and `pin-diag.sh` copy with `rigcopy.py`, not `cp -R`.
    - Every run script refuses a checkout whose `r23_ab.py` lacks this
      gate.
- **(c) Re-seal (OI-1003-Q166), done 2026-10-15 on mbp-13.** Corpus v1
  shared all 23 of its files' extents with `~/git-bulkload/sota/src` (the
  study's §1).
  - **Old path (kept, not deleted):** `~/git-bulkload/r23-corpus-v1/corpus`.
  - **New path (the rig default):**
    `~/git-bulkload/r23-corpus-v1-unshared/corpus`.
  - **Made** by an explicit `os.read`/`os.write` copy:
    - The tool was `~/git-bulkload/reseal-20261015/reseal_copy.py` (sha256
      `523a06458c47cbfa…`), a rig script; the repository's
      `r23_corpus.py copy` was not on the rig's checkout yet.
    - `strace` showed no `copy_file_range`, `sendfile`, `splice` or clone
      ioctl.
    - It was then sealed 0444/0555, as the original was.
  - **Verified** to the same content identity
    (`f4a7619f7b88f2e0e1eeadb5995c79a809eafa8b4a8cf3f82d1ff6f29b5b07c7`,
    `r23_corpus.py verify` at 59e5e60).
  - **Unshared:** 0 of its 25 extents are shared, by
    `~/git-bulkload/reseal-20261015/fiemap_check.py` (sha256
    `0ff568b6d8203bae…`).
  - **Receipt:** `~/git-bulkload/logs/reseal-corpus-v1-20261015.receipt`,
    sha256 `5e716ee5988a7107…`.
  - **The default** is the runbook's `--corpus`
    (`docs/plans/2026-10-07-s1-hermetic-rig.md` §2 and §5). The rig's dated
    run scripts are records of past runs, not defaults.

  A re-seal changes inode and ctime, so the stat-based fixture identity
  (`sealed_corpus_blake3`) starts a new series.

## Amendment 2026-10-15 (ruled, later): S2 passes on a schedule recorded in advance, stash-order-v2 (OI-1003-Q168, Q169)

Operator rulings, Linear TIN-4543, 2026-10-15. No SLO number changes.

- **During an S2-measured run, pass start times come from a schedule
  recorded in advance, never from host load (OI-1003-Q168).** The schedule
  is fixed (slot times) or seeded-random (a first time, a period, a pass
  count and a uniform jitter drawn from a recorded seed, which re-derives
  from those parameters alone), sealed by a digest, and made before any of
  its slots (`s2_budget.py schedule`). `observe --schedule` records it in
  the observation at its start; `drive` starts each pass at its slot's
  time, marks it with its slot and the schedule's digest, and waits for it
  to exit (never ended early, R-N11; it never reads the host's load). A
  pass still running at the next slot's time makes that pass late; a slot
  already past when the drive began is recorded as skipped, never run
  late, as a `skip` record. `alongside --evidence` verifies the schedule
  was followed and refuses evidence otherwise: the observation recorded a
  valid schedule made before it began; the observation covers the whole
  schedule (no slot before its origin or within 5 s of its end, so which
  slots are measured is never chosen by when the run started or stopped);
  every pass that did not end before the observation began names a slot
  of it; no slot has two passes, none was skipped and none inside the
  observation lacks a pass; every pass exited 0 (a pass that failed or
  refused, or recorded no exit status, is not evidence: a failure under
  load would correlate with neo's activity; this lane's choice, the
  operator may relax it to a stated deviation); the passes were started
  by `drive` on the observed host (its `drive` record names the host and
  its monotonic clock), and every pass started within 5 s of its slot
  measured on the observed host's monotonic clock against the
  observation's own origin, with the drive's wall-clock marks within 2 s
  of that clock. The deviations (on the observer's clock and on the
  drive's), skews, failed, skipped and outside slots are reported (the
  verdict's `schedule`, `summary.md`, an `evidence.tsv` `schedule` row).
  `drive` must therefore run on the observed host.
  This answers the 2026-10-15 review's finding 4 by protocol: pass
  placement cannot correlate with neo's own activity; load that the passes
  themselves cause is bulkload's cost. Proof: `ScheduleTests` in
  `test_s2_budget.py`, two of them live drives.
- **stash-order-v2: a stash entry is its commit and its reflog message
  (OI-1003-Q169).** The source's stash list is read as each entry's commit
  and reflog message; the capture key hashes both, a capture with a stash
  carries `stash-order-v2` (that list, byte for byte), the destination's
  ledger stash row (`LandedStash`) records both, and the stash table
  (T1 to T6) compares entries by both, storing each entry under its own
  message (an empty one and one that is not UTF-8 included). So a
  converged stash is the source's `git stash list` exactly, messages and
  order; a message-only change at the source moves the key (a `StashMoved`
  drift row when it races a pass) and converges as a rewrite (T5); one at
  the destination is the destination's (T6). **Deliberate invalidation:**
  the key of every checkout with a stash changed once with the format, so
  its retained capture is re-captured at its next pass (a checkout with
  no stash keeps its key). Old records still decode or refuse typed,
  never misread: a capture taken before Q169 (`stash-order-v1`) applies
  with each commit's subject, folded as Git folds a reflog message, as its
  message (`stash-messages-inferred`); a ref ledger or ref intent written
  before Q169 has its own prefix and decodes through its own layout (a
  ledger's stash row then names entries by commit, and is upgraded to the
  destination's entries the first time it names them,
  `stash-ledger-upgraded`; a landing whose source messages differed from
  the subjects is then rewritten once, T5); a pending v1 intent is
  resolved against the destination's stash; any other prefix or layout
  refuses `FRAME_CODEC`. The EstateConverge model needs no change: its
  stash values are opaque ids, one per entry. Proof:
  `p85_the_stash_list_is_the_sources_exactly_messages_and_order`
  (capture, apply, then pushes, a pop, a drop, a stored message and a
  message-only change, each converged to the source's `git stash list`),
  `p85_a_source_message_git_would_fold_lands_folded_and_later_pushes_land`
  (a message Git itself would never write, from a libgit2 tool or a
  hand-edited log, lands folded as Git folds every reflog message it
  writes, counted `stash-message-folded`: the list then equals the
  source's up to Git's normalization, and later pushes still land),
  `a_stash_message_is_part_of_the_key_and_the_capture`, and the stash
  table, decode and store rows in `native_refs/tests.rs`.

## Amendment 2026-10-15 (#226): bulkload lands into the live tree (OI-1003-Q171, Q172)

The ruling is OI-1003-Q171/Q172 (Linear TIN-4543, quoted from the
workflow's task text): bulkload is the interim flagship data mover, ahead
of tcfs. [2026-10-16: corrected attribution. "Interim flagship data mover"
is the operator's own statement, intent ledger PUR-20 (operator,
2026-10-15T17:04:18Z, about #226): "bulkloads bread and butter, interim
flagfip data mover as a precursion to the solutions we want TCFS to
provide". It is not OI-1003-Q171/Q172: per TIN-4543 comment `aff765a8`
(2026-10-15), Q171 runs three sprints in parallel (sprint 1 is "bulkload as
the everyday mover") and Q172 moves GloriousFlywheel neo→sting the way GFTB
moved.] #226 lists what the GFTB resume needed a hand script for. No SLO
number changes. These are proof-status lines and the contract's new
surface; the design note is `docs/agent-notes/2026-10-15-estate-adopt-design.md`.

- **`--adopt` is built** (it supersedes the last line of the
  OI-1003-Q144/Q149 amendment above: "`--adopt` is not built,
  OI-1003-Q145"). It is explicit per run and never inferred.
  - An existing checkout bulkload never landed becomes bulkload's when it
    proves equal to its capture, by content: HEAD, the index entries, the
    exclude and every carried seat. Nothing is moved. Ledgers are written
    in place, absent carried refs are created and equal ones adopted, and
    differing and destination-only refs and worktrees stay native.
  - A checkout that differs refuses the new typed code
    `GIT_DESTINATION_DIVERGED` with nothing written. An unrelated
    repository refuses `GIT_DESTINATION_OCCUPIED`.
  - **S4:** both are typed and pending review. `closure-dispose --policy
    GIT_DESTINATION_DIVERGED accept` is the standing accept. A crashed
    adoption is `journal-missing` until its rerun finishes it.
  - **S3:** the adopt proof reads each carried destination seat once
    (`read_dest_verify_existing_bytes`). An unchanged rerun is the journal
    no-op and reads 0 bytes. The adopted prints then prove later converges
    by print.
  - **S2:** the source is never read.
- **`estate-diff`** reports a plan against the destination, read-only, in
  the apply's own terms (P94).
- **A bare capture into an absent path lands natively** (#226 item 4).
- **`--activate-origin PREFIX`** activates a named scp-style or `ssh://`
  origin at a first landing (#226 item 6). The #216 default is unchanged:
  only HTTPS origins are active, and a local path never.
- **Rebuildable omissions are counted** (#226 item 5). Closure is
  `bulkload.closure.v4`: an `omissions` block names every omitted root
  with its entries and bytes, and a capture from before the sidecar shows
  `unknown`, never zero. "Equals the source" is exact except for those
  counted roots.
- **Proof.** The GFTB acceptance row P90 (`tests/estate_adopt.rs`) goes
  from a live tree and a source to a converged live tree with bulkload
  verbs only. Then an unchanged rerun reads 0 bytes, and a source change
  converges in place. The other rows:
  - P91 to P95;
  - the `estate.adopt.*` fault-harness rows;
  - EstateConverge's `Adopt` rows, with the invariants
    `AdoptClaimsOnlyEqual` and `ForeignRefsUnowned` and four mutation
    rows (`docs/formal/README.md`, "#226").
- **Decisions this lane took on the design's open questions** are in the
  note's section 11. They are not operator rulings. Each is conservative
  and reversible:
  - adopt without asides meets #226;
  - adopt recreates refs the destination deleted, shown first by
    `estate-diff`;
  - no older-capture baseline;
  - a bare landing takes the standalone origin rule;
  - the origin prefix is a run flag;
  - the existing `--policy` is the standing accept.
- **Not built:** `--protect TAG` (rename-aside for items adopt refuses)
  and `EstateProtect.tla`.
- **Review of 2026-10-16** (twelve findings; the design note's section
  13). No SLO number changes. S4: what `--adopt` now refuses
  `GIT_DESTINATION_OCCUPIED`, typed and pending review, with nothing
  written:
  - a destination spelled through a symlink, a checkout of another
    repository than the plan's, a linked worktree whose admin directory
    names another path (P97, P98, P100, P102);
  - a re-key whose capture is not the landed tree's or whose ledger names
    an item of the same plan (P99);
  - a refs-only item into an unrelated repository, after the provenance
    import (P101; it was counted applied);
  - a ledger another apply wrote while the adoption waited for the lock.

  The adopt proof's racy instant is taken before its first read. The
  model's adoption decisions are ghosts (`AdoptKept` excuses only what the
  ref step left), T0 is reached, and four mutation rows bite
  (`docs/formal/README.md`, "#226").

## Amendment 2026-10-16: the hand-off block (OI-1003-Q183, TIN-6569)

The ruling is OI-1003-Q183 (Linear TIN-6569, quoted from the workflow's
task text; the relayed operator request was cut off and the quote was not
checked against Linear in this lane): a beside-landing carries a builtin
addendum at the top of `AGENTS.md` and `CLAUDE.md` with the landing's
absolute paths, dated details and high-level differences, and tells agents
to converge and consolidate completely before any other work with the
user. No SLO number changes. The design note is
`docs/agent-notes/2026-10-16-estate-handoff-design.md`; the contract is
`docs/design.md`, "Hand-off block".

- **S5:** a block that is intact and recorded is bulkload's bytes, never
  a local change: converge proves the seat as the block followed by the
  landed blob and keeps it. Any edit inside the block or below it is a
  local change: `GIT_DESTINATION_MODIFIED`, as for any edit. An edit
  inside the block also makes the hand-off refuse
  `HANDOFF_BLOCK_MODIFIED` with nothing written; under an edit below it,
  a rerun re-renders the block alone and keeps the user's bytes, and the
  record's print never vouches for them (review of 2026-10-16).
  `NoLocalWorkLost` is amended in one place: an operator value a render
  left under an intact block is kept. A block carried from another
  landing is content and is never dropped (OI-1003-Q185).
- **S4:** the hand-off is an annotation outside closure's gate. Its four
  refusals (five until OI-1003-Q188 removed `HANDOFF_NOT_PRIVATE`) are
  typed per workspace and never change an item's outcome;
  the H step's result is a line of its own after the apply's receipts.
- **S3:** an unchanged `estate-handoff` rerun reads 0 destination bytes
  and writes nothing once its record's prints are past the racy window
  (one settling rerun re-dates them); a no-op `estate-apply` does no H
  step. A converge reads a handed-off target once, as it reads any seat
  whose print moved.
- **S2:** the source is never read.
- **Review of 2026-10-16** (two refuters against `9ec8d36`; the design
  note's section 18 has the per-finding table): the record's print is
  re-checked at every write; a crashed clear is finished; a carried block
  is kept as content (OI-1003-Q185); the privacy guard counts a preserved
  inert origin and URL rewrites (the guard is since removed,
  OI-1003-Q188); commit detection reads branch and tag
  history; `ready` is reachable by consolidating; options persist across
  reruns; `AGENTS.override.md` is covered. No SLO number changes.
- **Re-reviews of 2026-10-16** (design note section 19): a block of this
  landing's identity whose whole file is the target's landed blob (a round
  trip, with or without the record) is carried content, kept below the new
  block and never dropped or refused (S5, OI-1003-Q185); a block removed by
  hand and put back byte for byte is bulkload's again, never presented as
  carried; a created file stays `created` across re-renders. No SLO number
  changes.
- **Operator rulings of 2026-10-16** (Linear TIN-6569; design note
  section 19): OI-1003-Q188, the privacy guard is removed and the block is
  always written; OI-1003-Q189, `ready` also needs a clean working tree in
  the live sibling and in every destination-only worktree; OI-1003-Q190,
  `--clear --clear-carried` removes carried blocks from landed content as a
  recorded edit converge accepts (S5: never from bytes that are not the
  landed blob; S3 unchanged, no read on an unchanged rerun beyond the
  `git status` of the sibling and destination-only worktrees that Q189
  rules); OI-1003-Q191, commit detection flags the token only in commits
  the carried capture does not reach, and a token in the source's history
  is information. No SLO number changes.
- **Re-review 3 of 2026-10-16** (design note section 20): a block of this
  landing's identity that the target's landed blob itself starts with is
  carried content whatever the user wrote below it since (a round trip
  the H step missed is never dropped by a render or `--clear`, S5 and
  OI-1003-Q185); a crashed `--clear-carried` finished after a user's edit
  still records its removal; commit detection flags only token lines the
  source's history never added (a cherry-picked source block is
  information) and reads remote-tracking refs and every stash entry; the
  `git status` of the sibling and destination-only worktrees runs no
  filter driver. No SLO number changes.
- **Proof.** `tests/estate_handoff.rs` H0 to H19 (H0 is the control: a
  naive prepend refuses MODIFIED), the hand-off unit rows and the
  `render_then_clear_is_identity` property, the `estate.handoff.*`
  fault-harness rows (render, clear, lift, converge and H step), and
  EstateConverge's `Handoff` rows with the invariant
  `HandoffOwnsOnlyItsBlock` and five mutation rows
  (`docs/formal/README.md`, "TIN-6569").
- **Open for the operator:** which copy carries the work forward for a
  landing beside a live copy (agents never delete either). Answered: which
  repositories count as private (none is asked: OI-1003-Q188) and what
  `ready` reads (commits and clean trees: OI-1003-Q189).

## Amendment 2026-10-16: the delta lever, opt-in pending a reading of S3 inequality 2 (OI-1003-Q154)

A lever under OI-1003-Q154 (authorized S1 levers where R25 holds), built to
OI-1003-Q155's quality and merged only on an independent review
(OI-1003-Q173). It changes no number and no ruling, and it is **off by
default**. Diagnosis: `docs/agent-notes/2026-10-16-delta-regression.md`
(PR #234).

- **What it does.** Since wire v5 (`c2f8b663`) the destination asks for a
  manifest first for every entry it cannot reuse once its store holds any
  output hint: any earlier non-empty output. So a new file copied into a
  populated destination is read and hashed whole by the source before its
  first byte is placed. With `BULKLOAD_LOCAL_NEW_PATH_STREAM=1`, a local
  `copy` with the loopback hand-off answers `Send` for a seat new to both
  stores: no file at its path, no reuse row under its key, no capture of
  it in the source's ledger, and no temporary salvaged in the session
  (`Inbound::wants_manifest`). It keeps `WantManifest` for an existing
  path (adoption, the superseding publish's local fill, #187), for a seat
  whose reuse row is here and whose output a third party removed (its
  source ledger row still gives the manifest without a read: P79), for a
  seat the source's ledger captured for another destination from the same
  source state (one source fanned out: the same zero-read manifest; the
  destination reads that ledger read-only, both halves being one
  process), and whenever the sweep salvaged a temporary (a temporary's
  name does not say which path it was staged for). A source ledger the
  destination cannot read keeps the manifest. `pull`, `serve`, `receive`
  and a `copy` with the hand-off off decide as before, lever or not.
- **Why it is off by default: S3 inequality 2.** WP0(c)'s inequality 2
  (wire content bytes ≤ the changed seats' chunks the destination holds no
  verified copy of, OI-1003-Q18) is proven on `copy` with `bytes_received`
  (`tests/s3_transfer_resume.rs`, P18 and P21 over added seats). With the
  lever on, an added seat that copies or slices a carried file is streamed,
  and its handed bytes count in `bytes_received` whole. Ten tests fail,
  each on that bound alone (lane note): P18 and the P21 added-seat cases
  in `tests/s3_transfer_resume.rs` (eight), `w3_engine_properties` in
  `tests/w3_engine.rs` (W3's cross-file prefix fill, `bytes_received`
  below the grown file's length), and the lib test
  `a_lost_output_does_not_lose_reuse_of_its_chunks` (#59: when a chunk's
  newest holder is gone an older one serves it). In a local `copy` with
  the hand-off those bytes never cross the socket (only 64-byte headers
  do), and the source reads the seat once either way, so whether handed
  bytes are "wire content bytes" is a reading of inequality 2 that no
  ruling gives. Until the operator rules it, the lever is for A/B only and
  every ruled proof runs as before. If handed bytes are ruled not to be
  wire bytes, turning the lever on by default is the default's change in
  `local_new_path_stream` plus moving each of those clauses to an arm
  where a new path still asks for its manifest: inequality 2's added-seat
  clauses and W3's prefix fill to the transport arm (`serve` against
  `receive`, which the harness already has, or the hand-off off), and
  #59's older-holder fallback likewise, since on `copy` it would then run
  only through a removed output's manifest.
- **R25 and S3's other clauses hold with it on.** The source reads a new
  seat exactly once either way (a manifest's chunks are kept and served
  from memory). An unchanged rerun is answered `Reuse` before this decision
  and reads 0 bytes. A changed seat has a file at its path and keeps its
  manifest. A seat streamed by the lever has no capture in the source's
  ledger under its key, checked at the decision, so the lever never turns
  a ledger manifest into a read: not for this destination's own rows (a
  source row follows `Held`, which follows the reuse row here) and not for
  rows recorded for another destination from the same source state. One
  residual, shared with an empty destination: a file a third party places
  at a streamed seat's path during the session is refused at its publish
  (`DESTINATION_OCCUPIED`, not remembered), not adopted; the next run asks
  for that path's manifest and adopts the file if it verifies, the source
  reading the seat once more across the two runs (`ReadOnce` holds per
  session; no committed row vouched for those bytes, so
  `R25_NoDurableReread` holds). Passing adoption to a streamed seat would
  close it, but the snapshot path refuses an adopting stream
  (`sqlite_snapshot`), so it is left as it is for an empty destination.
- **Model.** `BulkloadTransfer.tla`'s `RecvEntry` already chooses freely
  between `send` and `manifest` for an absent output. The lever narrows
  that choice, so no step and no row changes. The rows that check the
  choice under R25 were re-run (lane note). The model abstracts chunks, so
  inequality 2 is not a model property either way.
- **Proof on the code.** `tests/loopback_handoff.rs`, with the lever on for
  the test's own destination: a new path in a populated local destination
  is streamed (`transfer_manifests_asked` 0, `transfer_new_paths_streamed`
  1 per seat, `source_ledger_miss_reads` 0, `read_dest_local_reuse_bytes`
  0) and its unchanged rerun reads 0 bytes; the same assertion fails on the
  code before (`source_ledger_miss_reads` 1). A removed output whose reuse
  row is here is asked for by manifest and filled with 0 source bytes. A
  seat the source's ledger captured for a second destination keeps its
  manifest and is filled with 0 source bytes and 0 received, lever on or
  off (on the first build, without the ledger check, it was read whole).
  An existing path still asks for its manifest and is filled locally. The
  default, the lever turned off, and the hand-off off each ask for a new
  path's manifest as before (the default arm follows the variable, so the
  suite passes with it exported). A cross-process session with the lever
  on for its destination still asks for one, fills a duplicate file
  entirely from the destination's own output, and hashes every received
  byte. Lib tests with the lever on: a session that salvaged a temporary
  asks for a new seat's manifest and fills it from the temporary (0
  received), and a database whose output was removed is found under its
  snapshot key (manifest) while a new database is streamed. Each of these
  fails under the mutation it guards (lever reaching `pull`, salvage
  skipped, snapshot key dropped, ledger check dropped). The crash,
  power-loss and resume suites run in both hand-off arms with the lever
  off, and again with it on (`just delta-lever`, in `just fault-harness`:
  `loopback_handoff` first to prove the variable took, then
  `fault_harness`, `power_loss`, `estate_converge`, `sqlite_carry` and
  `resume-power-loss`; `s3_transfer_resume` and `w3_engine` are left out
  for the bound above).
- **Measured, informational** (mbp-13, not a sample, OI-1003-Q39): one new
  33.5 MB file into a destination seeded with one 1 MiB output, 10 reps
  per arm, ABBA, load1 0.0: lever on 107.6 ms median against off
  153.4 ms, first data 0.9 ms against 51.5 ms, the same bytes read
  (33,555,209) and identical output (lane note). The off arm sits above
  the diagnosis's manifest arm (136.9 ms) on the same rig; read the
  difference, not the levels. An informational series
  on the bench comes before any gated sample (Q143); the diagnosis expects
  about 0.89x rclone on the bench delta, at Q143's 0.90x margin.

## Amendment 2026-10-16: R0, every agent lane continues on the destination (TIN-6587)

The operator's core user expectation (2026-10-16, Linear TIN-6587; R0 of the
draft `docs/SPEC.md`, PR #236): after a run, `claude --resume` and `codex
resume` in each lane's destination working directory list and resume its
sessions, transcripts byte-identical. Until now no SLO measured it, and the
2026-10-16 investigation found it holds for no neo lane on sting
(`docs/agent-notes/2026-10-16-reseat-investigation.md`). No SLO number
changes. This adds a verb, its acceptance check and its proof rows; the
contract is `docs/design.md`, "Agent session re-seat".

- **R0 acceptance is `estate-reseat --check`**, read-only: per lane, the
  transcripts and rollouts the two pickers read in the destination
  directory hold the landed ones (exactly, or continued by a resume there;
  never ending inside a line), the Git state is `estate-diff`
  `identical`, and the hand-off note is present when TIN-6569's marker is
  given. It replaces the undefined "daily-work verdict" (#40) as the
  per-lane record. A lane without its Git input is `incomplete`, never a
  pass. R0 also fails when nothing was checked, when retention is unsafe,
  and for every session or thread with no lane (unmapped, unresolved) or
  failed by ID, unless the operator names it (`--waive`, `--waive-from`).
- **S3:** an unchanged `estate-reseat` reads 0 landed bytes
  (`read_reseat_landing_bytes=0`), the landed Codex snapshot included, and
  writes 0. A grown transcript is read once from the landing: every placed
  byte proven by its BLAKE3, then the new tail (the source is never read,
  R25).
- **S4:** every session, thread or file that is not re-seated is a typed
  row: `RESEAT_DEST_DIVERGED`, `RESEAT_DEST_CONTINUED`,
  `RESEAT_EXTEND_RACED`, `RESEAT_SOURCE_REWRITTEN`,
  `RESEAT_LANE_UNMAPPED`, `RESEAT_LANE_UNRESOLVED`,
  `RESEAT_LANE_COLLISION`, `RESEAT_INDEX_CONFLICT`,
  `RESEAT_INDEX_ROW_ABSENT`, `RESEAT_UNREADABLE_FORM`,
  `DESTINATION_OCCUPIED`; a run refuses `RESEAT_LANDING_ABSENT`,
  `RESEAT_RETENTION_UNSAFE` and `RESEAT_INDEX_ABSENT` before writing, and
  `--check` `RESEAT_R0_FAILED`. They are report rows, not closure items:
  the closure report does not join them yet.
- **S5 and never overwrite:** a destination copy is adopted only when equal,
  extended only when a verified byte prefix (whole lines, the destination's
  size checked before every write), and otherwise kept beside the landed
  one; a placed file changed at the destination is the destination's from
  then on. Codex rows are inserted only, never updated; a row the
  destination already holds gets no second rollout beside it.
- **Proof:** `reseat::tests` and `reseat::tests::findings` (synthetic
  transcripts and Codex stores in throwaway homes, one row per review
  finding, each seen failing against the first build), `tests/estate_reseat.rs`
  (the CLI, and canaries on the installed Claude Code picker and the
  installed Codex resume in throwaway homes with no network), and eleven
  fault-harness rows over the seven `reseat.*` points.
- **Open, for the operator** (the investigation note's section 6, as
  restated in `docs/agent-notes/2026-10-16-estate-reseat-review-fixes.md`):
  which destination directory a lane keys to when the run lands Git beside
  the live tree; the gftb and GloriousFlywheel sessions (D4b); neo's own
  sessions (and sting's) blocked at their destination key by sting's
  symlinked transcripts, and ~9,242 Codex rows spelled through the `~/git`
  symlink (TIN-5268's hold); whether existing Codex rows may be updated;
  and home and `/private/tmp` sessions (`--unmapped`, `--waive`). The verb
  takes no decision on any of them: its defaults are the conservative ones.

### Addendum 2026-10-16: the re-reviews of `8a89f9a` and OI-1003-Q198 (TIN-6587)

No SLO number changes. The two re-reviews of `8a89f9a` (data safety:
fix first, one MAJOR; user outcome: mergeable, four minors) and the
operator's rulings OI-1003-Q198 (Linear TIN-6587, comment `fbccc49b`)
amend the R0 amendment above; the contract is `docs/design.md`, "Agent
session re-seat", and the receipt
`docs/agent-notes/2026-10-16-estate-reseat-rereview-fixes.md`.

- **S3** now holds on neo's real shape: a session refused (unmapped,
  unresolved) or unreadable is ledgered by its landed identity and not
  read again, so an unchanged rerun reads 0 landed bytes with them too.
- **S4** gains `RESEAT_LOCKED` (another run holds a destination root or
  STATE), `RESEAT_REGULARIZE_RACED` (both copies kept),
  `RESEAT_UNDO_TARGET_CHANGED` (`--undo` over an archive target that is
  gone or no longer the recorded bytes: nothing changed) and, before writing,
  `DESTINATION_SPACE_INSUFFICIENT` (the engine's `--min-free-percent` rule
  over the bytes a pass would write). Everything under a project key that
  is no session's is carried with its key or named by a `key` line
  (Q185). A rollout recorded outside every `--codex-source-root` is
  `RESEAT_LANE_UNMAPPED:rollout-root` in `--check` too.
- **S5, as ruled:** the destination's own Codex row or rollout form for a
  thread is never replaced and nothing is placed beside it, for rollouts
  with no snapshot row too (the MAJOR); a destination file with more than
  one link is never written. Two destination changes are ruled
  (OI-1003-Q198): an existing row whose `cwd` only spells the lane through
  a symlink gets that column re-spelled, and a symlinked transcript is
  replaced by a regular copy of its target (the archive kept); both are
  ledgered with their old values and reversed by `--undo` wherever the
  destination still holds exactly what re-seat wrote.
- **R0 acceptance** also fails a longer landed form of a rollout that was
  never placed (`extra-form-unplaced`), and names a row carrying the
  source's `cwd` (`row-cwd-is-source`). Rollouts with no snapshot row get a
  row from their header (Q198 D), so their lane's picker lists them; home
  and `/private/tmp` sessions are placed by ID and are in no lane (Q198 C).
- **Proof:** `reseat::tests::findings::rereview` (nine rows, each failing
  against `8a89f9a` first) and `reseat::tests::findings::q198` (seven
  rows), the CLI space row and two more real-binary canaries in
  `tests/estate_reseat.rs`, and sixteen fault-harness rows over twelve
  `reseat.*` points.
- **Resolved by OI-1003-Q198** (the open items above): where lanes key
  (A), gftb and GloriousFlywheel (B), home and `/private/tmp` sessions (C),
  rowless rollouts (D), `.zst` rollouts (E), sting's symlinked transcripts
  and the ~9,242 `~/git`-spelled Codex rows.

## Priority (OI-1003-Q4)

[2026-10-16: what bulkload must achieve, and the order that rules now, is
SPEC.md (OI-1003-Q194). Item 4's order is superseded by C6 (RUL-06): the
full run does not wait for the proofs, and R-N56's lift is no longer a
precondition. "A routine, repeatable sync run" stands (SPEC corrects the
*single*-run reading of OI-1002-Q24).]

1. Make S1–S5 provable: proof package, property-test decomposition, and
   SLO-recording runs.
2. Simplification, refactoring and the adversarial architecture review, along
   with the pre-migration correctness fixes.
3. Gates (a) and (b), measured against these SLOs.
4. Coverage inventory (#102, #103) and the operator's lift of R-N56. From then
   on, migrating neo to sting is a routine, repeatable sync run (OI-1003-Q13):
   rerun as neo's work lanes complete, each run reaching closure with 0
   unaccounted.

## Amendment 2026-10-16: rulings not yet indexed here, and status lines (editorial, R-N13)

No new ruling and no SLO number changes. This indexes operator rulings that
bear on this charter and had no index entry here, each with its record,
and states what they leave standing. The operative text is the record cited.

- **OI-1003-Q142** (2026-10-08; TIN-4543 comment `7734678e`; intent ledger
  SEAT-31): the full neo→sting run happens once estate reruns converge and
  refusals are fully typed, even with S1 still failing, as an S3/S4/S5 proof
  run. It relaxes OI-1002-Q24 for S1.
- **OI-1003-Q150** (2026-10-15; TIN-4543 `b6c52dd2`): S1 stays gated
  against plain rclone. (Q152, no freeze window, and Q154, three S1 levers,
  from the same record, already have their amendments above.)
- **OI-1003-Q170** (2026-10-15; TIN-4543 `8062b656`): the full-run landing
  gets its own XFS project on sting's fast-local.
- **OI-1003-Q185, Q192** (2026-10-16; TIN-4543 `a92634c1`, `68248cf1`):
  nothing is ever excluded; only truly recreatable content (build outputs,
  dependency directories, caches) may be dropped.
- **OI-1003-Q194** (2026-10-16; TIN-4543 `14484c4a`): the operator accepted
  every recommended reading of SPEC's conflicts C1–C20 as standing rulings,
  recorded in SPEC.md (#236). Those that bear on this charter:
  - C2: S1's headline is cross-host gate (b) on an estate-shaped many-file
    corpus; the rig's gate (a) is a regression guard (this reopens Q97).
  - C3: the 1 % delta is a real in-place change to about 1 % of files. The
    bench does not measure that today: it XORs the head of the first file
    in row order and removes that file from each destination
    (`docs/agent-notes/2026-10-16-delta-regression.md`).
  - C5: the 2026-10-03 S3 reading stands (0 content bytes re-read, a
    metadata-only re-walk, a rerun within 10 %).
  - C6: the full run does not wait for the proofs.
  - C14: a carried-as-files outcome for what cannot be captured natively
    (S4 row above); not built.
  - C18: the old quiet-window selections are superseded by never-halt.
  - C20: bulkload is done when the full run meets SPEC R0 and R3 is proven.
- **Status of OI-1002-Q24 and R-N56.** Q142 relaxed Q24 for S1, and Q194 C6
  rules that the full run does not wait for the proofs. R-N56's freeze of
  estate operations until the M2 gates pass (2026-09-23) is therefore
  superseded on order (C6, RUL-06; SPEC.md, "Not superseded, and what is
  corrected"); estate carries have run since (the GFTB carry, OI-1003-Q163,
  `docs/evidence/field-2026-10-15-gftb-carry.md`).
- **R-N91.** Holding other lanes quiet while a gated sample runs is read
  as superseded by C18 together with SPEC R3 ("Samples run on live hosts";
  intent ledger LIVE-40 to LIVE-42, L2). RUL-18 itself names CONF-01, 06,
  07 and 08, not R-N91, so this is a reading, not the ruling's text. R-N81
  (AC power, load1 below 2.5 before every rep and every arm) stands for
  gated samples on the rig. Open for the operator: how R-N81's load gate
  fits SPEC R3's samples on live hosts ("a sample taken under load is
  recorded as such").
- **Still open here.** S1's core-table text is not rewritten for C2 and C3:
  that waits for the gate (b) protocol on the estate-shaped corpus and a
  bench delta that matches C3.
- **What this pass labelled.** The "Completion bar" section and the
  "Priority (OI-1003-Q4)" section keep their text, with dated pointers to
  SPEC.md: the Completion bar stays OI-1003-Q4's record (SEAT-10; ONCE-08
  cites it for Q13), and Priority item 4's order is superseded by C6.

## Amendment 2026-10-16 (M2 item 3): estate-capture priority and bounded starvation

Source: the operator's M2 lane instruction of 2026-10-16 ("bulkload is one
command", item 3: on macOS background priority starved `estate-capture`, a
Git child in uninterruptible IO for 25 minutes behind a normal-priority
`serve`; "propose and implement a bounded-starvation rule: escalate to
normal after N seconds without progress, recorded in counters. At minimum,
document it and default `estate-capture` to normal when no interactive
load is detected"), the plan of record's M2 fix 5 ("adaptive priority").
To be entered in the ruling ledger (TIN-4543); until it is, this amendment
records the instruction, not a ruling.

- **(f) for `estate-capture`.** Its default class is `auto`: background
  when a terminal was read or written within 300 s, normal otherwise,
  recorded `priority_from=auto priority_auto=...`. The other source-side
  verbs of (f) and OI-1003-Q25 are unchanged; gate (a)'s rule (opt out only
  with an explicit, recorded flag) is unchanged for `copy`.
- **Bounded starvation.** A background `estate-capture`, or any background
  verb given `--escalate-after`, escalates its IO class once after the
  threshold (default 300 s) with no counter or descendant-IO movement,
  recorded on its counters line. An S2 sample's process (`serve`, `copy`)
  runs no watcher unless asked, so its class is what (f) says.
