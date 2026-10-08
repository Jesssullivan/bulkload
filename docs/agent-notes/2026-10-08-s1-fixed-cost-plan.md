# 2026-10-08: S1 fixed per-copy cost, a ranked plan (analysis only)

Lane: S1 analysis subagent, run by the coordinator's workflow. Read-only: no
code was changed and nothing was built. This file is the only output.
Inputs:

- the worktree `feat/s1-group-syncfs-20261007` at `14a9790` (#210, with
  its review fixes staged and uncommitted);
- `origin/main` at `b9d6c46` (it has #209, #212 and #217);
- `docs/evidence/r23-2026-10-07-1410Z-mbp-13-record.md` (the FAIL of
  record: native 856.7 ms against rclone 360.7 ms);
- `docs/evidence/s1-profile-2026-10-07.md`;
- `docs/evidence/s1-seal-breakdown-2026-10-07.md`;
- `docs/evidence/r23-2026-10-08-0239Z-mbp-13-handoff.{md,json}`;
- `docs/slo.md`.

Rulings cited:

- OI-1003-Q119 (the timeline, then fewer and cheaper commits);
- OI-1003-Q115 (hand-off timers);
- OI-1003-Q114 (profile first);
- OI-1003-Q107 and Q112 (#210 and its held review);
- OI-1003-Q20, Q37 and Q104 (WP0(g));
- R-N102 (directory records);
- R25 and R-N58;
- R-N81 and R-N91;
- OI-1002-Q30;
- OI-1003-Q96, Q97, Q103 and Q105 (the rig);
- R-N13.

Nothing below is ruled. Every item that changes an ordering or the unit of
a commit needs the operator's ruling before it is built.

## 0. What the counters already say (B = `ec53448`, 9 initial copies)

These are the medians of the handoff sample's B reps, read from its JSON.
Summed counters overlap one another; the "on the receiving thread" column
says whether the work runs serially on the thread that is S1's critical
path.

| Work per initial copy | Count | Summed ms | On the receiving thread? |
|---|---:|---:|---|
| Wall time | | 882 (rclone 366) | |
| `reuse_census_ns` (time in `Inbound::entry`; it includes `Destination::directory`) | 4 new directories | **66.1** (delta: 0.9) | yes: **measured** |
| FULL `SQLite` commits | 13 (+2 relaxed source rows) | ~128 in total, ~9.8 each | see below |
| of which: creating the stores (`sqlite_schema_commits`) | 2 | ~20 | yes (setup) |
| of which: `directory_pending` | 4 | ~39 | yes (inside the census) |
| of which: `directory_complete` | 4 | ~39 | yes (tail) |
| of which: destination output groups | 3 | ~29 | no, except the last group |
| Directory barriers (`flush_dir_barrier`) | 20 | 69.4, ~3.5 each | 8 in the census, 4 in the tail, ~6 in the groups, 2 at store setup |
| State-root seals (`flush_dir`) | 2 | 7.7 | yes (setup) |
| File seals (`flush_barrier`, `fsync`) | 23 | 437 (~550 MB/s effective) | only the last group's (the drain) |
| `recv_settle_ns` | | 0.3 | yes |
| `send_wait`, `send_handle` and `queue_wait` | | 449, 288 and 587 | source side |

Three findings correct or sharpen the handoff note.

1. **"About 17 fully synced commits" is not what the counters show.** They
   show 13 FULL commits and 2 relaxed ones. Any further syncs are outside
   the counters. The likely candidate is SQLite's own syncs when
   `configure_sqlite` switches each new database to WAL
   (`PRAGMA journal_mode=WAL` runs while `synchronous` is still the
   default). That is unverified; step 0 counts it.
2. **The final group's drain is not attributed to any timer.** The last
   `End` reaches `Inbound::end`, which calls `settle_held()` at
   `transfer.rs` ~2834. That call is not timed, so the
   `committer.sync()` that waits for the last group's file seals, directory
   seals and FULL commit is in no counter. It is not in `recv_settle`
   (0.3 ms), which times only the call at the top of the loop. Until #217,
   the drain was part of the unexplained 450 to 500 ms.
3. **The counted serial sync work explains only about 150 to 185 ms.** That
   is setup (~35 ms counted), plus directory creation (66 ms, measured),
   plus finishing the directories (~52 ms, from the per-call means). The
   rest of the ~500 ms is the drain and the stream's ramp, and nothing
   measures either yet. **Removing every counted fixed cost cannot close
   the gap on its own.** The steady stream (~350 ms of the receiving
   thread: read 79, verify 106, placement ~83) is already about rclone's
   whole 366 ms. A pass needs fixed cost of about 100 ms or less **and** a
   faster stream (the profile's options 2 to 4). Section 3 gives the
   arithmetic.

## 1. Ranked plan

The ranking weighs the evidence-backed saving against invariant risk.
Step 0 comes first because items 1 and 5 cannot be sized without it.

### Step 0: attribute the missing ~300 ms (counters only; no behaviour change)

- **Code sites.**
  - `transfer.rs` `Inbound::settle_held`: wrap the `committer.sync()` in a
    new `RECV_DRAIN_NS` timer, so the call from `end` is counted too.
  - `Inbound::run`: record the offset of the first `Data` frame
    (`recv_first_data_ns`) inside `recv_stream`.
  - `bulkload-bench` `print_sample` and `r23_ab.py` `NEW_TIMING_KEYS`:
    emit both.
- **Runs.**
  - One informational run of #217's `recv_setup`/`recv_stream`/`recv_tail`
    on mbp-13, with the new timers.
  - One untimed `strace -f -T -e trace=fsync,fdatasync,syncfs,sync_file_range`
    copy, to count the syncs per phase, SQLite's own included.
- **Saves:** 0 ms. It tells us whether item 1 or items 2 to 4 come first.
- **Risk:** none. **Proof:** the `transfer/tests.rs` timing test is
  extended to assert `recv_drain_ns > 0` on a two-file copy, and the
  `test_r23_ab.py` key list is updated.
- **Ruling:** covered by OI-1003-Q115 and Q119 (measurement), so no new
  ruling. The coordinator should confirm.

### 1. Start write-back while the data streams, not at publish (the drain and the delta)

- **Code sites.**
  - `transfer.rs` `Streaming::accept` and the `Filling` placement path,
    after `place()`: call `io::durable::start_writeback` on the staged
    file every N MiB placed (N = 4 to 8), for that byte range.
  - Today #210 calls it only once, at publish
    (`transfer.rs` ~3097 on #210's branch).
  - It runs on Linux only, through `sync_file_range(SYNC_FILE_RANGE_WRITE)`.
    Darwin is unchanged.
- **Saves (inferred; step 0's drain timer confirms it).**
  - The last group's seal must write back whatever is still dirty. Pre-#210
    that is the whole of the last files, and the last `End` is usually the
    largest file (`big/blob-a.bin`, 88 MiB). At the measured ~550 MB/s
    effective, that is 100 to 160 ms.
  - #210's publish-time hint starts that write-back only when the file
    ends. That fits #210's sample: flush time fell by 140 ms, but wall
    time did not move.
  - Expected: the drain falls to roughly the last N MiB plus one journal
    force, a saving of **50 to 150 ms on the initial copy**.
  - On the 1 % delta (one 33.6 MB file), the file seal is **34.8 ms
    (measured median)**. Expect 20 to 30 ms saved, which is most of the
    delta's 34 ms gap (136 against 102 ms). #210 does not help the delta,
    because a single-file group keeps the per-file path.
- **Invariant risk: low.**
  - `SYNC_FILE_RANGE_WRITE` makes nothing durable. The seal is unchanged,
    and so is the data, then record, then `Held` order.
  - Without `WAIT_AFTER` it does not consume the file's errseq, so the
    later `fsync`, or #210's `check_writeback`, still reports a write-back
    error. Verify that claim at v6.12 (`ksys_sync_file_range`), as #210's
    review did.
  - Side effects: more, smaller xfs extents, and destination write-back
    competing with source reads on a shared device. That matters for S2's
    measured budget on `copy` only, not on `pull`.
- **Proof.**
  - Every existing power-loss and crash proof must pass unchanged
    (`just resume-power-loss` = 4 proofs on Linux, `tests/power_loss.rs`,
    the fault harness).
  - A new io-trace test asserts that `sync_file_range(WRITE)` events are
    never treated as seals: a trace with the hints and with the seal
    removed must fail `check_view`. This is the mutation check.
  - A test with an injected write-back error after the hint asserts that
    the group is still refused, using #210's `fail_writeback_of` hook.
- **Ruling:** probably an extension of OI-1003-Q107, which already ruled
  `start_writeback`. It needs the operator's confirmation, because it
  reaches the per-file and strict paths, not only the batched one.

### 2. Batch the creation of sibling directories (R-N102 records)

- **Code sites.**
  - `materialize.rs` `Destination::directory`. Today each new directory
    is: a tagged `mkdirat`, `seal_entry(parent)`, a FULL
    `record_directory_created`, `rename_exclusive`, then
    `seal_dir(parent)`. That is 2 barriers and 1 FULL commit per
    directory, serially on the receiving thread.
  - `transfer.rs` `Inbound::entry` and `Inbound::run`. When a `Directory`
    entry arrives, drain the `Entry` frames that are already readable.
    The source offers up to `ENTRY_WINDOW` = 1024 entries before any data,
    and the walk is pre-order. Create that batch's new directories level by
    level:
    1. all the `mkdirat`s;
    2. one `seal_entry` per parent;
    3. one FULL transaction holding every record;
    4. the renames;
    5. one `seal_dir` per parent. Under #210's allowlist, this seal can
       instead be left to the next group's device `syncfs`.

    Then decide the files in entry order.
- **Saves.** The census measures **66 ms** (range 58 to 78) on the initial
  copy and 0.9 ms on the delta. Corpus v1's four directories (`big/`,
  `copies/`, `medium/`, `small/`) are siblings under the root, so one
  round replaces four: about **45 to 50 ms**.
- **Why it matters beyond S1.** On an estate-shaped tree (OI-1003-Q19, git
  trees with thousands of directories) today's cost is ~16 ms per
  directory, serially: 1,000 directories cost 16 s. Batching makes the
  cost one round per depth level per window.
- **Invariant risk: medium.** Each directory keeps its own R-N102 order:
  its entry is sealed before its record, and its record is committed
  before its rename. The commit is now shared. A failure inside the batch
  must refuse each affected entry as a value and remove the batch's
  temporaries, as `discard_directory` does today. The R-N119 fallback
  (no no-replace rename) stays per directory.
- **Proof.**
  - **Directory records are outside `BulkloadTransfer.tla`** (line 63:
    "Directory records (R-N102) ... are not modelled"). Either extend the
    model with a batched directory action and a mutation
    (`MC_neg_dir_rename_before_record`), or the operator rules that the
    R-N88 trace and the crash sweep are the proof.
  - Extend `tests/power_loss.rs` `populate` with two or more sibling
    directories and a nested one (for example `nested2/`,
    `nested/deeper2/`), and keep
    `every_power_loss_state_of_a_copy_is_consistent`.
  - Mutation checks: renaming before the batch commit must fail
    `check_view`; dropping the pre-record seal must fail.
  - Fault harness: `DirectoryAfterMkdir`, `DirectoryAfterPendingRecord`
    and `DirectoryAfterRename` at every index of a batch (`just crash-sweep`).
  - An S3 test: resuming after each crash point reads 0 source bytes for
    the seats already held.
- **Ruling:** needed. It changes R-N102's commit unit and its proof basis.

### 3. Finish all of a session's directories in one transaction (the tail)

- **Code sites.**
  - `materialize.rs` `Destination::finish_directories`. Today each
    directory is: `fchmod`, `seal_entry`, then a FULL `complete_directory`,
    one directory at a time.
  - New `transfer_store.rs` `Store::complete_directories(&[key])`, one
    transaction.
  - Keep deepest-first for the `fchmod`s, because a parent's final mode may
    deny the search its children need. Seal each directory, or use one
    device `syncfs` under #210's allowlist. Then commit every completion
    at once.
- **Saves (from the per-call means; #217's `recv_tail` confirms it).** Today
  the tail runs 4 × (3.5 + 9.8) ≈ 53 ms. Batched, that is one commit and
  four barriers, or one device seal: **30 to 40 ms**. It scales with the
  number of directories, as item 2 does.
- **Invariant risk: low.** Every directory is sealed before the
  transaction that completes it, so `MC_neg_commit_before_dirseal`'s rule
  holds per directory. A crash before the commit leaves every record
  pending. The resume then adopts each directory through
  `existing_directory` and finishes it again, which is what a crash before
  the first completion does today. The only new state is "all completions
  missing together", which is a subset of today's states.
- **Proof.**
  - Run `tests/power_loss.rs` over every state, with the fixture of
    item 2.
  - Fault point `DirectoryBeforeComplete` at every directory of the batch.
  - An io-trace mutation: the completion commit before any member's seal
    must fail.
  - A test that a refused session (`stats.refusals` not empty) still
    completes nothing, the current guard in `finish_receive`.
- **Ruling:** needed but light. It changes R-N102's completion commit unit
  and nothing else.

### 4. Create the two stores at the same time, not one after the other (setup)

- **Code sites.**
  - `transfer.rs` `receive`. Today it runs `Store::open(destination_state)`,
    `Destination::open`, `publication_committer` (a second
    `Store::open`), `sweep_root` and `has_output_hints`, and only then
    writes `Control::Open`.
  - `serve` creates the source store only after it reads `Open`. The two
    store creations are therefore serial.
  - The change: canonicalize and run the overlap checks first, as `copy`
    already does, then write `Open`, then do the destination's setup while
    the source does its own. `receive` still reads `Start` only after its
    setup.
- **Saves (estimated).** One store creation is about 1 FULL commit
  (~9.8 ms), 1 state-root seal (~3.9), 1 parent barrier (~3.5), the WAL
  switch's own syncs (uncounted; step 0's `strace` gives them) and the file
  opens: about **17 to 30 ms** per store. Overlapping the two saves about
  one of them.
  - For gate (b)'s `pull`, the destination's setup also overlaps one
    round trip to the remote `serve`.
- **Pipelining setup with the walk is not worth building.** The walk costs
  0.12 ms per copy (`walk_ns` median 123,481 ns). The overlap worth having
  is between the two stores.
- **Invariant risk: low.**
  - S2: the source's state may now be created before the destination
    refuses at setup. It is still outside the source root, which is checked
    before `Store::open` (WP1 PR 3), so nothing is written inside the
    source.
  - No source content is read before `Decide`. Keep the destination's
    `store.root()` against `target.path()` overlap refusal before `Open`,
    by canonicalizing both first.
  - The authority's durability is unchanged: `Start` still follows the
    source store's FULL creation commit and its root seal (#161,
    `MC_wp0g_authority`).
- **Proof.**
  - The S2 overlap properties, `MC_s2`, and the WP1 PR 3 tests.
  - A new test: a destination refused at setup (state inside the
    destination) makes the source read 0 bytes and write nothing under the
    source root.
  - `every_power_loss_state_of_a_store_open_keeps_the_store` must hold for
    both stores when they are opened together.
- **Ruling:** probably none; it changes no ordering of durable state. The
  coordinator should confirm it.

### 5. Shape the final group (only if step 0 shows a large drain after item 1)

- **Code sites:** `io/durable.rs` `run` (`GROUP_IDLE` = 20 ms) and
  `publication_committer` (the group size).
- **The idea.** Once `walk_done` and the remaining payload is below a
  threshold, close groups as each output is queued. The last group then
  holds one file, which item 1 has already written back.
- **Saves:** unknown until the drain is measured. It could be 0 after
  item 1.
- **Invariant risk: low** (a group is a performance unit). More groups mean
  more FULL commits, about 9.8 ms each on the committer thread, and they
  are off the critical path except the last.
- **Proof:** the existing group-commit and `fault-injection`
  `side_group` tests.
- **Ruling:** none expected.

### 6. Make store creation cheaper (only if step 0's `strace` shows extra syncs)

- **Code sites:**
  - `io/durable.rs` `configure_sqlite` (`PRAGMA journal_mode=WAL` on a
    database that is still empty);
  - `transfer_store.rs` `Store::open` (state root, seal, creation commit).
- **The idea.** Build the new database in a tagged temporary with
  `synchronous=OFF`: switch to WAL, create the schema, insert the
  authority and the markers, and checkpoint. Then:
  1. `fsync` the file once;
  2. rename it to `transfer.sqlite` without replacement;
  3. seal the state root once;
  4. only then open it `synchronous=FULL`.
- **Saves:** 0 to 25 ms per store. It is uncertain until the WAL switch's
  syncs are counted.
- **Invariant risk: medium to high.**
  - The authority must be durable before `Start` (`MC_wp0g_authority`).
  - The WAL header bytes must be durable before the first WAL commit;
    otherwise a reopen ignores the WAL and loses the authority.
  - SQLite's own syscalls are outside the io trace (the seal breakdown's
    caveat), so a proof must reason at the file level: one fsynced file,
    one sealed entry.
- **Proof.**
  - `every_power_loss_state_of_a_store_open_keeps_the_store`, plus new
    mutations: the rename before the `fsync`, and the open before the
    seal.
  - `MC_store_root_unsealed`, and the #125 and #161 marker tests.
- **Ruling:** needed. Do this last.

### Not recommended

- **Reusing either store across initial samples.** The bench creates a new
  destination and both stores per arm, and S1's initial copy is the first
  run. A destination store is bound to its destination by the authority. A
  reused source store would answer from its ledger, so the copy would no
  longer be an initial copy, and state would carry over between reps. Both
  stores must stay fresh in the gate. The lever is what a creation costs
  and overlapping the two (items 4 and 6), not avoiding creation.
- **Relaxing destination commits** (output groups, or the directory
  pending and complete records) to `synchronous=NORMAL`.
  - `Held` must follow a FULL destination commit (R25, R-N58,
    `MC_neg_held_before_commit`).
  - A pending record that is lost after its rename was sealed leaves a
    directory with no record and a 0700 mode. The resume then refuses it
    as occupied.
  - WP0(g) covers only the source ledger's rows. The extra durability is
    obtained by batching (items 2 and 3) instead.
- **Capturing source bytes before `Decide`.** It would read seats the
  destination may already hold (R25), and S3's inequality 1 would fail.
- **Dropping the second `Store::open` per side** (the publisher's
  connection). It commits no change, so it costs about 1 to 2 ms. Do it
  only as part of item 4's refactor.

## 2. Order of work and its dependencies

1. Step 0. Measure, and record the result as informational evidence.
2. Item 1 is first, provided the drain measured in step 0 is more than
   50 ms. It is independent of #210, which stays held under Q112. It can
   land on main with a minimal `start_writeback`, or after #210.
3. Items 3 and 4 have low risk and one light or no ruling, and can run in
   parallel lanes.
4. Item 2 needs the R-N102 ruling and the decision on the model or the
   trace as its proof.
5. Items 5 and 6 only if the measurements call for them.

Every durability-touching item (1, 2, 3 and 6) gets an adversarial review
before it merges. Under the subagent limit, those reviews come after
Oct 12, as #210's do.

## 3. Arithmetic: what a pass needs

These figures are approximate and come from the medians in section 0.

| | Now | After items 1 to 4 (best case) |
|---|---:|---:|
| Setup | ~35 counted + uncounted | −17 to −30 |
| Directories during the stream | 66 | −45 to −50 |
| Drain | unmeasured (inferred 100 to 160) | −50 to −150 |
| Tail | ~53 | −30 to −40 |
| Wall time | 882 | **~610 to 740** |
| rclone | 366 | 366 |

The fixed-cost items alone do not pass S1. The rest has to come from:

- the stream: the receiving thread's ~270 ms of serial read, verify and
  placement (profile options 2 to 4: a wider receiving side, a looser
  in-process wire, verifying once in loopback; each is unruled);
- whatever step 0 attributes to the ramp.

The delta, 136 against 102 ms, needs item 1 above all. It must also pass:
OI-1002-Q30 requires every B rep's bench verdict to pass, the delta
included.

## 4. Measurement protocol for the next gated sample

1. **Before any gated sample (informational, not verdicts).** For each
   candidate build `X`, run against main `M` (`b9d6c46` or its successor)
   on mbp-13:
   - Use a private writable copy of the sealed corpus v1 (identity
     `f4a7619f...`) and the flake-pinned rclone 1.74.4 (OI-1003-Q105).
   - Pass `--informational` with the same `--priority` and `--durability`
     as the gated harness.
   - Run five rounds, each one invocation of `M` then `X` with
     `--reps 3`: 15 native initial samples and 15 deltas per arm. This is
     the seal breakdown's `ab-final` shape.
   - Wait for load1 below 1.0 before each invocation, and record power
     and load1 on every row (R-N81).
   - Report, per arm, medians and min to max of:
     - wall time;
     - `recv_setup`, `recv_stream`, `recv_tail`, `recv_drain` and
       `recv_first_data`;
     - `reuse_census`, `send_wait` and `queue_wait`;
     - every `flush_*` and `sqlite_*` count and ns.
   - Closure check: the median of
     (`recv_setup` + `recv_stream` + `recv_tail`) / wall must be at least
     0.95, or the gap is reported as unattributed.
   - One separate `strace` copy per build counts the syncs per phase. It is
     never in a timed sample.
2. **Admission to a gated sample.** Spend a coordinator-quiet window
   (R-N91) only when, over the 15 informational samples, both hold:
   - X's native initial median ≤ 0.90 × rclone's median;
   - X's native delta median ≤ 0.90 × rclone's delta median.

   Below that margin a gated sample would only record another FAIL.
3. **The invariant gate before the sample:**
   - `just check-fast`;
   - `just fault-harness`;
   - `just resume-power-loss` (4 proofs on Linux);
   - the item's new proofs;
   - `just props-deep` and `just crash-sweep` for items 2 and 3;
   - TLC: `MC_main`, the `MC_r25_strict_*` configs, the `MC_wp0g*`
     configs, the `MC_supersede_*` configs and `MC_s2` pass; every `MC_neg_*`
     config still fails on its named property.
4. **The gated sample:**
   - `r23_ab.py` in `gated` mode, order `BABAB`, on mbp-13
     (`rig_role=record`).
   - A is `3931471738cc` (OI-1003-Q103) and V4 is diagnostic.
   - R-N81 (AC, load1 < 2.5) before every rep and every arm, with the
     other lanes held quiet (R-N91).
   - B is the combined head, built before the sample (no builds inside
     it).
   - The verdict follows OI-1002-Q30: B passes only if all three B reps
     pass both the initial copy and the delta, with R25 warm-zero,
     interrupted-zero and RSS true.
5. **Evidence.** Write `docs/evidence/r23-<stamp>-mbp-13-record.{md,json}`
   with:
   - the timeline table (setup, stream, tail, drain, first data);
   - the sync census;
   - the equal-durability table (#209's `rclone_synced`, informational
     only);
   - the seal primitive in the header (`fsync`, or `syncfs` if #210 has
     merged).

   Then update the Linear SSOT ledger and the whitepaper's section 5.2 and
   table in 5.3. A PASS is not claimed from informational runs.

## Open

- Step 0 is not run, and none of items 1 to 6 is built. Each needs the
  ruling marked above.
- The "17 synced commits" figure in the handoff note and the whitepaper's
  section 5.2 should be corrected to 13 FULL and 2 relaxed (plus any
  `strace`-found syncs) once step 0 has run.
- Item 1's errseq claim (`SYNC_FILE_RANGE_WRITE` does not consume the
  error) is from reading the kernel source by memory. Check it at v6.12
  before item 1 is built.
