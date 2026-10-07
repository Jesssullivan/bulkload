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
  applies. No neo run happens until gate (a) passes.
- Gate (b)'s rep rule is gate (a)'s rule with two unratified deviations. It
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
  then S4 is proven for estate items only.
- **What a disposition is bound to.** The ledger is bound to its plan (path
  and a digest of the plan's bytes) and SOURCE label. An item disposition is
  bound to the refusal instance it reviews (the item's current capture and
  its outcome record) and is recorded only while the item holds that
  refusal; a later refusal of the same code against another capture or with
  another record is pending review again.
- **Standing policies are open-ended (a design statement of WP3 PR 3, not
  yet an operator ruling).** A standing-policy disposition covers every
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
- **Open: the A control on the rig.** On mbp-13 the pinned A fails on its
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
    a field confirmation on neo.
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
