# 2026-10-08: S1 throughput, OI-1003-Q143 items 1 to 3

Lane: S1 throughput subagent, run by the coordinator's workflow. Branch
`feat/s1-throughput-20261008` (worktree
`bulkload.worktrees/s1-throughput-20261008`, at `dd33936`: #210's `0da8e4e`
merged with main `b9d6c46`). The changes are staged and not committed,
pushed or opened as a PR. That is left to the coordinator.

Rulings: OI-1003-Q143 (Linear TIN-4543, 2026-10-08): item 1 (drain and
first-data timers, the `strace` sync count, write-back while data streams,
the two stores opened in parallel; extends OI-1003-Q107), item 2 (directory
batching with directory records in the formal model) and item 3 (the
receive side widened: verify and write on several threads). Not authorized,
and not done: bigger frames, buffer hand-off across the in-process wire,
skipping the destination's re-hash. Also OI-1003-Q119 (the timeline),
OI-1003-Q115 (hand-off timers), R-N102, R-N119, R-N13. Plan:
`2026-10-08-s1-fixed-cost-plan.md`. Sections: step 1a's timers and sting
measurement (below), the amended design with the #217 review's findings,
what was built, the validation results and the after measurement.

## Step 1a: timers (no behaviour change)

- `transfer.rs`: two new process-wide timers on `TransferTiming`. Both are
  in `snapshot`, `render` and `since`.
  - `recv_drain_ns` times the `committer.sync()` in `Inbound::settle_held`.
    That call is the wait for the open group's file and directory seals
    and its store commit, once every entry has been offered and none has
    content outstanding. It is now counted from every caller: `end` (the
    plan's ~2834 site), the `WalkDone` arm and the loop-top settle. The
    drain is part of `recv_stream_ns`. When the loop-top settle is the call
    that drains, it is also part of `recv_settle_ns`.
  - `recv_first_data_ns` is the time from the start of `Inbound::run`'s
    frame loop to its first `Data` frame. This is the stream's ramp, and it
    is part of `recv_stream_ns`. It is 0 for a copy with no data frame.
- `bulkload-bench`: `native_timing` prints `recv_drain_ns` and
  `recv_first_data_ns` after `recv_tail_ns`.
- `r23_ab.py`:
  - Both keys are added to `KNOWN_TIMING`.
  - A new `receive_timeline(parsed)` gives, for each phase, the native
    samples' medians in ms. It also gives `cover_share_of_wall_median`, the
    median share of wall time covered by (setup + stream + tail). The drain
    and the ramp are inside the stream, so they are never added to that
    cover. The result is in `summarize()` as `receive_timeline`.
- Tests:
  - `transfer::tests::a_copy_moves_the_timeline_timers` (new): a two-file
    copy moves setup, stream, tail, drain and first-data, and the rendering
    names each one.
  - #217's commit message says that `a_copy_moves_the_hand_off_timers`
    asserts its three timers, but that assertion is not in the tree. This
    new test covers them.
  - `test_r23_ab.ParseTests.test_summary_reads_new_walk_wait_counter_and_seal_proxy`
    now checks the new keys, the timeline medians and the 0.96 cover.
- Validation:
  - `cargo fmt --all --check` passes.
  - Both timer tests pass (`cargo test -p bulkload-agent --lib -- transfer::tests::a_copy_moves`).
  - `python3 -m unittest test_r23_ab` passes (79 tests).
  - `cargo clippy --workspace --all-targets -- -D warnings` is clean, and
    `ruff check` and `ruff format --check` pass on both scripts.

## Measurement (informational, sting)

**Not of record.** sting is shared and was saturated: the 1-minute load was
256 to 433 on 32 CPUs, and every row is `gated=false`. The gate of record
is the hermetic rig on mbp-13 (docs/slo.md, amendments of 2026-10-07).
Absolute times here are 3 to 90 times the rig's, and the spread between
reps is larger than any effect. Only the sync counts and the timer
coverage carry over.

### Setup

- **Corpus:** R23 corpus v1, regenerated with `r23_corpus.py generate` and
  sealed: 23 files, 239,819,837 bytes, identity `f4a7619f…`. The bench
  read a writable working copy, as `r23_ab.working_copy` makes one. The
  corpus sits on xfs on NVMe at `/srv/fast-local/jess/cache/s1tp-bench`.
- **Binary:** `bulkload-bench` release, built from the staged tree
  (`--revision dd33936-step1a-staged`), run with `--informational`.
  - The defaults were used: group durability, background priority and the
    relaxed source ledger.
  - The run script was also under `nice -n 10`.
- **rclone:** not in the `nix develop` shell, and the pinned
  1.74.4 store path is not on sting. The rclone arm used
  `/nix/store/n3qh17w26dgs04dhdsqq27zm04yharix-rclone-1.75.1`, which was
  already in the store. It is not the pin (OI-1003-Q105).
- **Defects found:**
  - `--only rclone` can never finish. Interrupted-resume always resumes
    from `0-Native/source-state`, which a rclone-only run never creates,
    so the run refuses with `IO (errno 2)` after the warm phase. The
    rclone samples therefore come from 3 combined `--reps 3` runs (2
    rclone reps each), plus the 4 initial samples that the rclone-only
    run recorded before it refused.
  - A sealed (0444/0555) corpus passed directly as `--corpus-root` is
    refused: native gives `Permission denied` on the delta mutation, and
    rclone gives `.partial` `permission denied`. `r23_ab.py` already
    works around this with `working_copy`.

### Sync counts per copy (`strace -f -C -e trace=fsync,fdatasync,syncfs,sync_file_range`)

- **Method:** each trace was split at the bench's per-sample stdout
  writes. The bench's own syncs were subtracted: the delta mutation's 2
  `fsync`s and `remove_delta_targets`' 1 `fsync` per delta sample.
  - The native-only `--reps 1` run was traced once.
  - The combined `--reps 3` run was traced once (it carries the rclone
    windows).
  - One more native-only run used `strace -y`, which adds the target
    paths.

| Copy | fsync | fdatasync | syncfs | sync_file_range | Total |
|---|---:|---:|---:|---:|---:|
| native initial, native-only trace | 69 | 0 | 8 | 40 | 117 |
| native initial, combined trace seq 0 / 2 / 4 | 57 / 63 / 63 | 0 | 4 / 2 / 2 | 42 / 39 / 39 | 103 / 104 / 104 |
| native initial, `-y` trace | 66 | 0 | 2 | 38 | 106 |
| native delta (every trace) | 4 | 0 | 0 | 1 | 5 |
| native warm-resume | 0 | 0 | 0 | 0 | 0 |
| rclone initial / delta / warm | 0 | 0 | 0 | 0 | 0 |

rclone issues no sync of its own. The one `syncfs` in each rclone window is
the bench's untimed `sync_destination`, the equal-durability add-back.

The native initial copy varies between traces because the number of
groups varies (`durable_groups` 10 to 18, `flush_fs_count` 2 to 8). The
`syncfs` count always equals `flush_fs_count`.

**Breakdown of the `-y` trace's initial copy (66 fsyncs).**

| Issuer and target | fsync |
|---|---:|
| `SQLite`, destination store: `-wal` 19, `-journal` 2, `.sqlite` 1 | 22 |
| `SQLite`, source store: `-wal` 2, `-journal` 2, `.sqlite` 1 | 5 |
| Engine: destination root 8, depth-1 entries 13, depth-2 entries 9, sample root 2 | 32 |
| Engine: state directories (destination 4, source 3) | 7 |

- **Engine fsyncs:**
  - The engine counters show 32 (`flush_barrier` 8 + `flush_dir` 2 +
    `flush_dir_barrier` 22). The trace shows 39.
  - The difference of 7 matches the 7 state-directory fsyncs. That
    attribution is inferred, not verified.
- **SQLite fsyncs:**
  - The 27 `SQLite` fsyncs are outside every engine counter.
  - The `-journal` and `.sqlite` fsyncs (3 per store) are the
    rollback-journal commit made before WAL mode is set. That confirms the
    plan's finding 1 (6 fsyncs across the two stores).
  - The destination's 19 `-wal` fsyncs pair with its 18 FULL commits
    (schema 1, groups 9, `directory_pending` 4, `directory_complete` 4),
    plus 1, which is probably a checkpoint.
  - The source's 2 `-wal` fsyncs fit relaxed group commits plus a
    checkpoint.
- **Delta:**
  - The engine issues 4 fsyncs: the parent directory, the file, the state
    directory and the destination WAL.
  - It also issues 1 `sync_file_range`, #210's publish-time hint.
- **`sync_file_range` on the initial copy:** the 38 to 42 calls are all on
  destination files (#210's hint, then the committer's per-group
  re-issue).

### Timer breakdown (untraced)

Primary set: `--only native --reps 5` (5 initial, 5 delta). The pooled
figures add the native reps of the 3 combined runs (14 initial, 14 delta).
All values are in ms.

| Native, ms | Initial, 5-run median | Initial, pooled median (min to max) | Delta, 5-run median | Delta, pooled median (min to max) |
|---|---:|---:|---:|---:|
| wall | 7369.8 | 8058.6 (2490.4 to 83694.7) | 3829.4 | 3735.9 (696.6 to 9769.2) |
| setup | 913.0 | 246.1 (89.4 to 8126.5) | 15.1 | 17.9 (8.5 to 1046.2) |
| stream | 4674.8 | 6457.1 (2260.1 to 51811.6) | 3646.2 | 3658.6 (685.5 to 8722.1) |
| tail | 52.3 | 70.1 (21.9 to 31770.1) | 0.7 | 0.7 (0.3 to 5.9) |
| drain (inside stream) | 947.7 | 430.5 (18.9 to 22453.0) | 126.0 | 205.9 (27.0 to 1258.4) |
| first-data (inside stream) | 361.6 | 208.8 (29.0 to 1464.6) | 1894.1 | 1493.1 (126.6 to 6809.0) |
| (setup + stream + tail) / wall | 0.99995 | 0.9999 (min 0.9995) | 0.99985 | 0.9995 (min 0.9965) |

**Coverage.** Setup + stream + tail covers at least 99.65 % of wall time in
every untraced native sample, so the ≥ 95 % bar is met. On the two traced
copies it covered 99.72 % (initial) and 96.00 % (delta).

The drain lies inside the stream. Adding it to the sum double-counts: the
naive setup + stream + tail + drain reaches a median of 1.04 (initial) and
1.06 (delta) of wall, and up to 1.33.

**Drain and ramp shares (pooled medians).**

| Share of wall | Initial | Delta |
|---|---:|---|
| drain | 3.8 % | 6.6 % |
| first-data | 2.0 % | 54 % |

The delta ramp is, by inference, the walk and census of the 22 unchanged files before the
one changed file's data arrives.

**rclone 1.75.1 (not the pin).**

| | Samples | Median ms | Range ms |
|---|---:|---:|---|
| initial | 10 | 9385.5 | 6845.6 to 430677.5 |
| delta | 6 | 29240.4 | 5870.9 to 69947.8 |

Under this load, rclone's delta is no measure of its delta path.

**Raw outputs.** `/srv/fast-local/jess/cache/s1tp-bench/`:

- `out-*.txt` and `strace-*.log`;
- the run scripts `run.sh`, `run2.sh` and `run3.sh`.

These are not durable and may be deleted. The numbers above are the
record.

## The design review (#217 review, 2026-10-08): dispositions

Every finding is accepted. The table says where each lands; the item
sections below are the design as amended and as built.

| # | Finding (short) | Disposition | Where |
|---|---|---|---|
| 1 | A new directory can join a batch under an existing directory still held, so its sweep and adoption seal (#74 N1) come too late | **Accepted.** A directory joins only under the root, a member or a directory decided before the batch; a directory under a held, undecided parent closes the batch first. The model has the code's rule and the refuted one as mutation `admit_under_held`, which must fail the new invariant `NoDirectoryUnderTemporary` | `transfer.rs` `Inbound::walked`; `DirectoryRecords.tla` `CanDecide`; proof `a_batch_waits_for_a_held_adoptable_directory_before_making_inside_it` (the refuted rule run too, and caught) |
| 2 | The drafted model's crash was weaker than CrashChoice and crash_check, and had no inode reuse; grounding needed code first | **Accepted.** The crash is per name (persist or revert, a prefix of mkdir and rename, nothing inside a lost directory), `EnvSeal(p)` persists names behind the protocol's back, inode numbers are reused (a third party may take a freed one, `ForeignReplace`), and mutations `discard_unlink_before_clear` and `level_rename_seals_one_parent` are added. The code landed with the model, so the catalogue grounds every symbol now | `docs/formal/DirectoryRecords.tla`, `catalogue/Directories.dhall` |
| 3 | 1a cannot save 20–30 ms on the delta: its file is filled locally in `plan_file`, where no kick ran | **Accepted.** `plan_file` kicks right after its local fill, before `NeedChunks`. The delta saving is restated as at most the overlap of the `NeedChunks` round trip and the missing chunks' capture, unmeasured | `transfer.rs` `plan_file`; test `streamed_and_locally_filled_files_kick_their_write_back` |
| 4 | Savings rested on a pre-#210 baseline and were summed without a critical path or floors | **Accepted.** The arithmetic table is withdrawn. The projection is to be rebuilt on the rig at this branch's head from the critical-path timers, with a device and a CPU floor (below). sting cannot give that (load 230 to 430 on 32 CPUs). rclone's 366 ms leaves its device sync untimed | "Projection" below; rig run still owed |
| 5 | Item 3's saving had no measured stall behind it; extra threads compete with capture on a 2-core rig; the timers were to change meaning | **Accepted.** The saving is unknown until the rig A/B at W=1 and W=2. Existing timers keep their meaning (`transfer_ns`, `recv_verify_ns` are the receiving thread's); new: `recv_worker_verify_ns`, `recv_worker_place_ns`, `recv_pool_wait_ns`, and the sender's `send_write_ns` (blocked on the wire) | `TransferTiming`, bench, `r23_ab.py` |
| 6 | "Same refusal" failed on same-index ties; early staging made W=0 differ from W≥1 | **Accepted.** Within one chunk the order is fixed: verify or size, then stage, then write, then kick. The chunk that stages a streamed file is verified on the receiving thread first (one join), so a corrupt first chunk stages nothing, at every W, as before | `Step`, `fold_failure`, `Streaming::accept`; test `a_corrupt_first_chunk_is_refused_before_its_stage_fails` |
| 7 | A worker panic hung the receive; tiny payloads were not memory-bounded | **Accepted.** Each job runs under `catch_unwind`; a panic answers it as lost and the session ends `WORKER_LOST`. Each job is charged at least 4 KiB against the 8 MiB budget, which also caps jobs in flight at 2048 | `recv_worker`, `RECV_JOB_MIN_BYTES`; tests `a_receive_worker_lost_mid_job_ends_the_session`, `the_receive_workers_stay_within_their_byte_budget` |
| 8 | Children of a member refused at flush were undefined; caps must count every held frame | **Accepted.** A member whose batch parent was not created leaves the batch and is decided alone at replay against the file system, exactly as one at a time (a member refused at its rename: its child is made inside the third party's directory, as before). The 256 cap counts every held frame | `close_batch`; test `a_member_refused_at_its_rename_leaves_its_children_to_the_file_system` (racing third-party directory via a rename hook) |
| 9 | "Already reachable today" was false; the power-loss proof weakens silently | **Accepted.** The argument is now per-directory independence plus the TLA module. Bounded crash points are reported before and after; the directory segment is crash-checked exhaustively in its own trace | "Validation" below; `power_loss.rs` `every_power_loss_state_of_a_directory_batch_is_explored` |
| 10 | The ownership check misread discards: `complete_directory` was also the clear | **Accepted.** Clears are traced as `CommitRecord::DirectoryCleared` (`Store::clear_directory`, counter `sqlite_directory_cleared_commits`); a completion is `DirectoryComplete` only from `finish_directories` | `transfer_store.rs`, `io/trace.rs`, both power-loss invariants |
| 11 | TLC proves nothing about 1b; same-path and nested stores; flushes share one device | **Accepted.** No TLC claim for 1b. `copy` refuses a state root that overlaps the other state root or the destination root. `receive` refuses its own overlap before `Open`. The saving is to be read from the rig's `recv_setup_ns`, not added up from per-call means | `transfer.rs` `copy`, `receive`; tests `copy_refuses_state_roots_that_overlap_each_other_or_the_destination`, `a_destination_refused_at_setup_reads_no_source_byte` |
| 12 | A level held one descriptor per distinct parent, outside the descriptor budget | **Accepted.** The batch cap is min(64, descriptor budget / 16): 12 at a 256 soft limit. Parents are held for one level only | `materialize::directory_batch`; test `sixty_four_sibling_directories_with_children_batch_under_a_256_limit` |

## Design as amended and built

Scope is exactly OI-1003-Q143. Every received byte is still hashed once on
the destination; the wire format and the socket pair are unchanged.

### Item 1b: open the source and destination stores in parallel

- `receive` canonicalizes the destination root and refuses an overlap of
  its own state root with it, then sends `Open` **and its whole credit
  grant**, then opens its store, the destination, the committer, sweeps the
  root, and only then reads `Start`. The grant moved up because a source
  with nothing to send finishes and closes the stream while the
  destination is still opening its store (the empty-tree case of
  `a_run_leaves_the_source_lstat_census_unchanged` failed with `EPIPE`
  until it did). Nothing else is written before `Start`.
- `copy` also refuses a source state root that overlaps the destination
  state root or the destination root (finding 11), all before either store
  exists. Two first-time `Store::open`s on one path can therefore not race
  inside one `copy`; across hosts (`pull`) the two stores are on different
  machines.
- `copy` now reports the destination's own refusal when it is not a
  transport error and the source only failed after it (a destination
  refused at setup used to surface as the source's broken pipe).
- No durable order changes: each side's store creation, authority commit
  and state-root seal still precede that side's first record. No TLC claim
  (finding 11: `StartRun` is atomic in the model).
- Saving: to be read from the rig's `recv_setup_ns`. Both stores' FULL
  commits and seals flush the same device, so the overlap can be well
  under one store creation.

### Item 1a: write-back while the data streams

- `io::durable::kick_writeback`: `sync_file_range(SYNC_FILE_RANGE_WRITE)`
  over the whole file, Linux group mode only (strict and Darwin: no-op),
  counted as `writeback_kicks`. Errseq: checked against torvalds/linux
  v6.12. `fs/sync.c` `sync_file_range` (lines 228 to 302) calls
  `file_fdatawait_range`, which returns `file_check_and_advance_wb_err`
  (`mm/filemap.c` 594 to 607), only under `WAIT_BEFORE` or `WAIT_AFTER`;
  `WRITE` alone calls `__filemap_fdatawrite_range` (`mm/filemap.c` 421 to
  432), which never touches `file->f_wb_err`. So a write-back error of a
  kicked page is still reported by the file's own `fsync`, or by #210's
  per-file `check_writeback`. The plan's open item is closed.
- Rule: an entry's placed bytes (every offset written, local fill included)
  are counted in its `WriteTarget`; each crossing of a multiple of
  `WRITEBACK_KICK_BYTES` (8 MiB) kicks once. Kick sites: after each chunk
  write (on the worker that wrote it, item 3) and once in `plan_file` right
  after the local fill, before `NeedChunks` (finding 3). The publish-time
  kick stays.
- An error a kick returns refuses the entry (a full device maps to
  `DESTINATION_SPACE_INSUFFICIENT`); Q3's alternative (ignore, rely on
  errseq) would also be sound now that errseq is checked.
- Durability: unchanged. A kick is `SyncKind::Kick` in `crash_check`
  (nothing durable).
- Saving: initial copy unknown until the rig's `recv_drain_ns` (sting's
  drain was 3.8 % of wall). Delta: at most the overlap of the `NeedChunks`
  round trip and the capture of the missing chunks (finding 3); not counted
  until measured.

### Item 2: directory batching (R-N102)

The walk is depth-first, so batching siblings defers decisions.

- **Holding.** `Inbound::walked` takes every walk frame (`Entry`,
  `Refused { entry: None }`, `EngineTemporary`). With the batch cap above
  1, a directory entry whose leaf is free (`Destination::directory_is_new`)
  and whose parent is the root, a member, or a directory decided before
  (not held) opens a batch or joins it as a member. While a batch is open,
  every walk frame is held in stream order. A directory whose parent is
  held but no member (an existing directory, or one whose probe failed)
  **closes the batch first** (finding 1), so that parent's sweep and
  adoption seal run before anything is made inside it.
- **Closing** at `WalkDone`, at the cap's members, or at 256 held frames of
  any kind (finding 8): walk frames only, so the boundaries never depend
  on timing. 256 < `ENTRY_WINDOW` (1024): the source always has window.
- **Creating** (`Destination::create_directories`, one level at a time,
  shallowest first): a tagged `mkdirat` each (`directory.after_mkdir` per
  directory); one seal per distinct parent (a full flush off the store's
  device); one transaction binding every record
  (`Store::record_directories_created`, one `Event::Commit`;
  `directory.after_pending_record` once per level); an exclusive rename
  each (`directory.after_rename` per directory): `EEXIST` refuses that one
  `DESTINATION_OCCUPIED` and discards it (record cleared, then temporary
  removed), no exclusive rename sends that one through today's fallback;
  then one seal per distinct parent holding a renamed directory. A failed
  shared step refuses every directory it covers with the same code.
- **Replay.** Every held frame is handled in stream order as it would have
  been at once; a member's decision is its creation's outcome. A member
  whose batch parent was not created is decided alone, against the file
  system, by `Destination::directory` (finding 8).
- **Finishing** (`finish_directories`): deepest first, `fchmod` and seal
  each (`directory.before_complete` each), then one
  `Store::complete_directories` commit; a directory that changed refuses
  the finish after the ones sealed before it commit. With the cap at 1,
  each directory is finished and committed alone, as before.
- **Caps.** `materialize::directory_batch`: min(64, descriptor budget /
  16) (finding 12); `BULKLOAD_FAULT_DIR_BATCH=N` under `fault-injection`;
  `set_directory_batch(root, cap)` for tests.
- **Durability argument** (finding 9): each directory keeps its own R-N102
  order (entry sealed before its record; record before its name; name
  sealed before anything inside it is decided; mode sealed before its
  completion), and recovers on its own: the sweep removes its temporary
  after clearing the records bound to it, `existing_directory` adopts it by
  its record and seals its parent, and a completion is never committed for
  a directory whose mode is not sealed. Sharing a commit or a seal adds
  states today's code cannot reach (N records committed with no rename
  done; a child's final mode pending while its parent's is durable), so the
  claim is per-directory independence, checked by the TLA module with
  per-name crashes and by the trace proofs below, not reachability.
- **Clears are not completions** (finding 10): `Store::clear_directory`
  and `clear_directories` trace `DirectoryCleared`.

**Formal model: `docs/formal/DirectoryRecords.tla`** (its own module,
`catalogue/Directories.dhall`, `configs_dir.tsv`; Q1 below). Tree:
siblings `a`, `b` under the root and (Deep) `c` under `a`, `e` under `b`.
Variables per directory: current and durable name (`none`, `tmp`, `final`),
inode (model values, `SYMMETRY`), current and durable mode, a third
party's directory at the name, the committed record, the phase, and
ghosts. Actions: `StartRun` (root sweep), `DecideExisting` (sweep, adopt
by record and seal the parent, or clear a stale record), `Mkdir`,
`SealParent`, `Record(S)` (one commit for a set, `|S| = 1` unless
`BatchCreate`), `Rename`, `DiscardClear`, `DiscardUnlink`, the fallback's
`Intent`, `FallbackMkdir`, `Bind`, `ChildCommit`, `Chmod`, `SealDir`,
`Complete(S)`, `Exit`; environment `Stop` (process crash), `PowerLoss`
(per name), `EnvSeal(p)`, `Foreign`, `ForeignReplace`. Invariants:
`RecordNamesDirectory`, `NoStrandedDirectory`, `CompleteImpliesFinal`,
`ChildImpliesNamed`, `NeverAdoptForeign`, `NoDirectoryUnderTemporary`.
Witnesses: `Witness_PartialLevel` (one power loss keeps one sibling's
rename and loses the other's, under one parent: finding 2's
unreachable-before case), `Witness_AdoptBatched`. Liveness (`AllFinish`)
was optional in the draft and is not built.

### Item 3: the receive workers

- Threads: the receiving thread keeps every protocol decision, in stream
  order: `read_frame`, the credit check, index, offset, size and manifest
  checks, `stage`, `End`, `publish`, `Held`. Verify and `place()` (and the
  item 1a kick) run on `W` scoped `std::thread`s inside `receive` (no
  tokio). `W` = `BULKLOAD_RECV_WORKERS` (0 to 16) or 2, at most
  `available_parallelism - 1`; 0 is the inline path, kept as the
  reference. `set_recv_workers(root, w)` for tests.
- Jobs: `RecvJob { entry, index, digest, payload (moved), verify, write:
  (Arc<WriteTarget>, offsets), charge }`; done as `RecvDone`. A failure is a
  `Failure { index, step, refusal }`; the least `(index, step)` wins
  (finding 6).
- `End(e)` and a source `Refused(e)` wait until `e` has no job in flight
  (`settle_entry`), so every write of a file precedes its submit, so its
  seal; `SessionChunks::insert` stays at publish.
- Budget: 8 MiB of charged payload (at least 4 KiB a job); dispatch waits
  on completions past it. Credit stays accounted at receipt.
- Teardown: a session error drops the job queue; the scope joins the
  workers before `receive` returns. A worker panic is caught per job and
  ends the session `WORKER_LOST` (finding 7).
- Timers: see finding 5.
- Saving: unknown until the rig A/B at W=1 and W=2 (finding 5). The 2-core
  rig is CPU-bound on FastCDC and BLAKE3 (2146Z evidence), and the sending
  thread is one serial stage too (`send_write_ns` now shows its wire time).

### Projection (replaces the withdrawn arithmetic table, finding 4)

No saving is projected from item estimates any more. The next rig sample,
at this branch's head, gives per phase: setup (`recv_setup_ns`), ramp
(`recv_first_data_ns`), stream (`recv_stream_ns`, with drain
`recv_drain_ns`), tail (`recv_tail_ns`), the workers' and the sender's
time, and `sync_file_range` / `syncfs` / `fsync` counts. Two floors bound
it from below: the device floor (240 MB at the device's measured
sequential write rate, from a `dd oflag=direct` on the rig, not the
summed-`fsync` rate) and the CPU floor (FastCDC + BLAKE3 over 240 MB on
the rig's 2 physical cores, source capture plus destination verify).

## Built (staged, not committed; OI-1003-Q143)

- `crates/bulkload-agent/src/transfer.rs`: item 1b (`copy`'s overlap
  refusals and refusal precedence; `receive` sends `Open` and its credit
  grant before its setup); item 1a (`WriteTarget`, `crosses`, `kick_after`,
  `place_counted`, the `plan_file` kick); item 3 (`Step`, `Failure`,
  `fold_failure`, `RecvJob`, `RecvDone`, `run_recv_job`, `recv_worker`,
  `RecvPool`, `recv_workers`, `set_recv_workers`, `RECV_WORKERS_ENV`,
  `Inbound::dispatch`, `reap`, `finished`, `settle_entry`, the workers'
  scope in `receive`, the new timers and `TimedWrite`); item 2
  (`DirectoryBatch`, `Walked`, `Inbound::walked`, `handle_walked`,
  `close_batch`, `parent_of`). Test-only hooks: `CORRUPT_DIGESTS`,
  `PANIC_DIGESTS`, `POOL_HIGH_WATER`, `KICK_BYTES_OVERRIDE`,
  `ADMIT_UNDER_HELD`.
- `crates/bulkload-agent/src/materialize.rs`: `directory_is_new`,
  `create_directories`, `level_parent`, batched `finish_directories`,
  `batch()`, `directory_batch`, `set_directory_batch`, `DIRECTORY_BATCH`,
  `DIRECTORY_BATCH_ENV` (fault-injection), the rename hook
  `set_before_directory_rename`, clears via `clear_directory`,
  `space_refusal` now `pub(crate)`.
- `crates/bulkload-agent/src/transfer_store.rs`:
  `record_directories_created`, `complete_directories`, `clear_directory`,
  `clear_directories`, `in_transaction`.
- `crates/bulkload-agent/src/io/durable.rs`: `kick_writeback`.
  `io/trace.rs`: `CommitRecord::DirectoryCleared`. `io/crash_check.rs`:
  `View::node_at`, `View::paths_of`. `counters.rs`: `writeback_kicks`,
  `sqlite_directory_cleared_commits` (in `sqlite_commits_total`).
  `fault.rs`: the batch's points documented.
- Tests: `src/transfer/tests/throughput.rs` (new, 13 tests, 2 of them
  properties), `src/materialize/adoption_power_loss.rs` (4 new proofs),
  `src/io/crash_check/tests.rs` (1), `tests/power_loss.rs` (ownership
  invariant, a two-level fixture, the teeth test and the exhaustive
  directory-segment test), `tests/fault_harness.rs` (14 directory-batch
  rows, `DIRECTORIES` and `DIRECTORIES_ONE` fixtures),
  `tests/fd_limit.rs` (64 siblings with children at 256),
  `tests/directory_batch_counters.rs` (new binary).
- `crates/bulkload-bench`: the four new timers printed; `r23_ab.py`'s
  `KNOWN_TIMING`.
- `docs/formal`: `DirectoryRecords.tla`, `catalogue/Directories.dhall`,
  `configs_dir.tsv` and 17 `MC_dir_*.cfg` (rendered; `MC_dir_foreign_replace` added 2026-10-09), `Types.dhall`'s
  module entry, `Catalogue.dhall`'s import, `README.md`'s section,
  `BulkloadTransfer.tla`'s abstraction line. `justfile`: comments, and
  `resume-power-loss` expects 8 proofs on Linux (7 elsewhere);
  `tests/test_ci_contract.py` pins that body.
- `docs/design.md`: Durability gains the four items.

## Validation (sting, 2026-10-08/09; results verbatim)

Build: `CARGO_TARGET_DIR=/srv/fast-local/jess/cache/cargo-target/s1tp`,
`nice -n 10 nix develop .#default`. `just fault-harness` builds with its own
`--target-dir target/fault`.

- `cargo fmt --all --check`: `exit 0`.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`:
  `Finished`, exit 0. `cargo clippy -p bulkload-agent --all-targets
  --locked --features io-trace -- -D warnings`: `Finished`, exit 0.
  `fault-injection,io-trace` (the fault-harness recipe's first line):
  `Finished`, exit 0.
- `cargo test -p bulkload-agent --lib --locked`: `test result: ok. 587
  passed; 0 failed; 5 ignored; 0 measured; 0 filtered out; finished in
  364.12s`.
- `--test w3_engine --test fd_limit --test directory_batch_counters --test
  directory_ownership`: `ok. 1 passed` (w3_engine), `ok. 3 passed`
  (fd_limit), `ok. 1 passed` (directory_batch_counters), `ok. 6 passed`
  (directory_ownership).
- `just fault-harness` (exit 0): `fault_harness`: `test result: ok. 74
  passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in
  142.08s`; `power_loss`: `test result: ok. 14 passed; 0 failed`; the
  traced `io::` lib tests: `ok. 69 passed; 0 failed; 2 ignored`; the P5
  partial-write proof: `ok. 1 passed`; `just resume-power-loss`: `test
  result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 601 filtered out`.
  The first two runs failed only in
  `every_power_loss_state_of_a_relaxed_ledger_costs_at_most_its_lost_seats`,
  which hard-coded the old fixture's 7 files; it now counts them.
- `python3 tests/test_ci_contract.py`: `Ran 24 tests ... OK` (after pinning
  the new `resume-power-loss` body). `python3 -m unittest test_r23_ab`:
  `Ran 79 tests ... OK`.
- `just tla-check` with every `MC_dir_` config (targeted: the full run of
  the other modules is hours on this host): grounding `DirectoryRecords.tla:
  36 operators, 10 constants, 9 mutations, 0 label sets, 20 code symbols`,
  and every row matched (table in `docs/formal/README.md`, "Directory
  records"): self-test INCONCLUSIVE; `MC_dir_per_dir` PASS 306,150;
  `MC_dir_batched` PASS 581,639; `MC_dir_batched_crashes` PASS 41,224;
  `MC_dir_fallback` PASS 91,044; both reach rows REACHED; all nine
  `MC_dir_neg_` rows FAIL on their named property. `total wall 2807s, peak
  rss 1836 MiB`, exit 0. An uncatalogued run of the batched bound with two
  crashes passed at 1,191,551 states (4787 s).
  `just tla-check MC_main MC_gc_core`: `MC_gc_core` PASS 45,062 (the record);
  `MC_main` **INCONCLUSIVE** (`WithinBudget` at 600 s, 847,861 of its
  963,314 states, load 270 to 300). `BulkloadTransfer.tla` changed by one
  comment line only, so this is the host, not the spec; not re-run.

**Power-loss enumeration, bounded crash points (finding 9).** Before is the
step 1a tree (old fixture), after is this tree (two more directories and
files, batched):

| Test | Before: points, states, bounded | After: points, states, bounded |
|---|---|---|
| `every_power_loss_state_of_a_copy_is_consistent` | 66, 13,109, 11 | 93, 9,801, 22 |
| `every_power_loss_state_of_a_strict_copy_is_consistent` | ~~63, 10,897, 0~~ 60, 16,451 to 16,757, 12 to 14 (corrected 2026-10-09) | 87, 10,639, 30 |
| `every_power_loss_state_of_a_directory_batch_is_explored` (new) | – | 32, 54, 0 (exhaustive, limit 16) |

**Corrected 2026-10-09 (S1 throughput review, findings 3 and 4).** The
strict "before" row was wrong. Re-measured at `dd33936` (`git archive HEAD`,
built apart, `--features io-trace`, 5 runs), the old fixture's strict copy
gives 60 crash points, 12 to 14 of them bounded, every run: no exhaustive
strict proof of a whole copy existed before this lane either. The bounded
points do not come from batching or the workers: on this tree the old
fixture's strict copy gives 14 bounded points at `BULKLOAD_FAULT_DIR_BATCH=1`
and at 64, and at `BULKLOAD_RECV_WORKERS=0` and 1 (fault-injection build),
and the enlarged fixture gives 30 at batch 1 and at 64. They grow with the
fixture. An exhaustive strict proof of a whole copy now exists:
`every_power_loss_state_of_a_small_strict_copy_is_explored` (below).

## Measurement after items 1 to 3 (informational, sting; not of record)

Same corpus and bench as step 1a; 1-minute load 255 to 295 on 32 CPUs, every
row `gated=false`, and `just fault-harness` ran beside parts of it. Three
interleaved rounds of `--only native --reps 3`: A = the step 1a binary
(before), B = this tree (W = 2), C = this tree with `BULKLOAD_RECV_WORKERS=0`.
9 samples per arm and phase; medians in ms.

| | A before | B after, W=2 | C after, W=0 |
|---|---:|---:|---:|
| initial wall | 6027.4 | 3210.1 | 4738.7 |
| initial range | 3858.8 to 13744.1 | 1650.8 to 17878.9 | 3604.2 to 6109.8 |
| setup | 340.0 | 67.7 | 103.3 |
| first data (ramp) | 214.0 | 59.0 | 69.8 |
| stream | 5593.5 | 2915.8 | 4634.0 |
| drain (in stream) | 169.0 | 49.1 | 134.3 |
| tail | 52.1 | 35.7 | 25.6 |
| reuse census | 96.9 | 20.7 | 30.2 |
| `transfer_ns` (receiving thread) | 2463.0 | 571.5 | 2015.1 |
| `recv_worker_verify_ns` | – | 804.0 | 0 |
| `recv_pool_wait_ns` | – | 647.6 | 0 |
| `send_write_ns` | – | 2119.1 | 3843.9 |
| directory pending / complete commits | 4 / 4 | 1 / 1 | 1 / 1 |
| `writeback_kicks` | – | 25 | 25 |
| delta wall | 1227.1 | 1431.3 | 1408.1 |
| delta range | 466.1 to 11444.5 | 394.8 to 3561.0 | 320.8 to 1891.1 |

`strace -f -C` of one `--reps 1` run of each binary, every sync between the
bench's sample lines (the bench's own setup syncs included, so not the step
1a table's engine-only method; same method both sides):

| Copy | fsync | syncfs | sync_file_range | total |
|---|---:|---:|---:|---:|
| initial, before | 132 | 12 | 80 | 224 |
| initial, after | 108 | 12 | 130 | 250 |
| delta, before | 7 | 0 | 1 | 8 |
| delta, after | 7 | 0 | 5 | 12 |

The fsync drop (−24) fits the batching (fewer directory seals and
directory commits); the extra `sync_file_range` calls are the kicks (25
on the initial copy's 240 MB, 4 on the delta's 33.6 MB file, which the
bench streams whole after removing it, so its `plan_file` kick does not
apply there). On sting the initial copy's medians move the way the design
expects and the delta does not move beyond its spread. None of it is a
saving of record: the rig at this branch's head is (finding 4).

## S1 throughput review (2026-10-09): dispositions

Six findings from the reviewers of the staged changes (findings 3 and 4 are
one issue). Each was verified on sting before it was fixed; each fix has a
test that failed first. Rulings: OI-1003-Q143 (items 2 and 3, the formal
model of item 2), R-N102, #74 N3, R-N13.

| # | Finding (short) | Verdict | Fix and proof |
|---|---|---|---|
| 1 | A level whose shared seal or commit fails removes its temporaries without clearing their records | **Confirmed.** `create_directories`' `bound` error path only unlinked; `discard_directory` clears first. Also its `open_dir`/`fstat` failure path left a stale record at the key | Both paths clear before the unlink (`clear_directories` of the made keys, best effort; the first-loop path now calls `discard_directory`). Test hook `transfer_store::fail_directory_records_after_commit` (test-only: commits, then reports `EIO`). Tests `materialize::tests::a_level_whose_commit_fails_clears_its_records_before_its_temporaries` (failed first: `PendingDirectory { dev: 26, ino: 39854360, mode: 493 }` survived) and `a_level_whose_seal_fails_clears_a_stale_record_at_its_keys` (failed first: the stale record survived) |
| 2 | The same directory offered twice in one batch leaves an owned 0700 directory with no record, refused on every resume | **Confirmed**, exactly as described: cap 64 refused `d` twice `DESTINATION_OCCUPIED`, left it 0700, and the rerun refused it again; cap 1 adopted it | `Inbound::walked` closes the batch before a directory whose path it already holds, so the second offer is decided as one at a time decides it (adopts by its record). Not refused as a protocol violation: batching keeps the unbatched outcome. Test `transfer::tests::throughput::a_directory_offered_twice_in_a_batch_is_decided_as_one_at_a_time` (a source writer that re-offers `d` before `WalkDone` and a relay that drops the destination's answer to it; caps 1 and 64 must agree, on the copy and its rerun; failed first with the outcome above) |
| 3, 4 | The strict-mode proof is no longer exhaustive (0 to 30 bounded points), and the lane note blames batching | **Partly confirmed.** The note's explanation was wrong: the bound comes from the fixture (see the corrected table above). The premise "before: 0 bounded" was wrong too: at `dd33936` the old fixture's strict copy was already bounded (12 to 14 of 60 points), so keeping the old fixture would not have kept an exhaustive proof (on this tree it gives 14 bounded at every batch cap and worker count) | New `power_loss::every_power_loss_state_of_a_small_strict_copy_is_explored`: a whole strict copy (a root file, a symlink, two levels of two sibling directories in one batch, a file under a second-level member), `accept_bounded: false`, limit 16: `crash_points=50 states=694 exhaustive=true bounded_points=0`. Its teeth: the seal of `q` after `q/s`'s rename dropped must fail on `q/s/f`. The first attempt (the old fixture asserted exhaustive) failed with 14 bounded points and was dropped. The enlarged fixture's runs stay bounded and say so |
| 5 | `ForeignReplace` is never enabled in a passing `DirectoryRecords` config | **Confirmed**: it needs `MaxForeign = 2` (`foreign < MaxForeign` after a `Foreign`), and every pass row had 1 | New pass row `MC_dir_foreign_replace` (shallow, four inode numbers, `MaxForeign = 2`, never set `Bind,FallbackMkdir,Intent`), rendered with `just tla-render`. `just tla-check MC_dir_foreign_replace`: PASS, 26,533 distinct, `ForeignReplace` enabled. README: the row, and why it was missing |
| 6 | No power-loss trace covers worker-written chunks or write-back kicks; "End waits for every write" is caught only by a side effect | **Confirmed.** With `settle_entry` a no-op, both copy proofs and (without a delay) the new test still passed: the race is rarely lost | New io-trace hooks `transfer::set_writeback_kick_bytes` and `transfer::set_recv_worker_delay` (`cfg(any(test, feature = "io-trace"))`, doc-hidden; the old test-only `KICK_BYTES_OVERRIDE` was unused). New `power_loss::every_power_loss_state_of_a_worker_written_copy_is_consistent`: a 400 KB file and a small one, W=2, 64 KiB kicks, each worker waits 300 ms per job; the explicit check `writes_precede_seal` runs before the copy's outcome is read; then at least 4 chunk writes, at least 4 kicks on Linux, the workers' place timer moved, every power-loss state consistent (`crash_points=35 states=549 exhaustive=true`), and its teeth: the last chunk write moved after the output commit must fail the ordering check and the power-loss check. Failed first: with `settle_entry` a no-op (a temporary env-gated edit, removed), 3 of 3 runs failed on the ordering check, `write at event 29 comes after the seal at event 25 its commit at event 26 relies on` |

**Validation after the fixes (sting, 2026-10-09; results verbatim; same
build environment as above; timings not of record).**

- `cargo fmt --all --check`: `fmt exit 0`. `cargo clippy --workspace
  --all-targets --locked -- -D warnings`: exit 0; `-p bulkload-agent
  --features io-trace`: exit 0; `--features
  bulkload-agent/fault-injection,bulkload-agent/io-trace`: exit 0.
- `cargo test -p bulkload-agent --lib --locked`: `test result: ok. 590
  passed; 0 failed; 5 ignored; 0 measured; 0 filtered out; finished in
  247.44s`.
- `--test w3_engine --test fd_limit --test directory_batch_counters --test
  directory_ownership`: `ok. 1 passed`, `ok. 3 passed`, `ok. 1 passed`,
  `ok. 6 passed`.
- `just fault-harness` (exit 0): `fault_harness`: `test result: ok. 74
  passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in
  178.01s`; `power_loss`: `test result: ok. 16 passed; 0 failed; 0 ignored;
  0 measured; 0 filtered out; finished in 173.61s`; traced `io::` lib
  tests: `ok. 69 passed; 0 failed; 2 ignored`; P5: `ok. 1 passed`; `just
  resume-power-loss`: `test result: ok. 8 passed; 0 failed; 0 ignored; 0
  measured; 604 filtered out`.
- `python3 tests/test_ci_contract.py`: `Ran 24 tests ... OK`. `python3 -m
  unittest test_r23_ab`: `Ran 79 tests ... OK`. `ruff check`: `All checks
  passed!`; `ruff format --check`: `2 files already formatted`.
- `just tla-check` with all 17 `MC_dir_` configs, exit 0, `total wall
  2979s, peak rss 1891 MiB`; every row as expected: self-test INCONCLUSIVE;
  `MC_dir_per_dir` PASS 306,150; `MC_dir_batched` PASS 581,639;
  `MC_dir_batched_crashes` PASS 41,224; `MC_dir_foreign_replace` PASS
  26,533 (never enabled: `Intent FallbackMkdir Bind` only);
  `MC_dir_fallback` PASS 91,044; both reach rows REACHED; all nine
  `MC_dir_neg_` rows FAIL on their named property. Grounding unchanged (36
  operators, 10 constants, 9 mutations, 20 code symbols).
- `just tla-check MC_main MC_gc_core` (exit 1, as on 2026-10-08):
  `MC_gc_core` PASS 45,062; `MC_main` **INCONCLUSIVE** (`WithinBudget` at
  619 s, 620,284 distinct states; load about 150 to 200).
  `BulkloadTransfer.tla` is unchanged by these fixes.
- Power-loss crash points (`--nocapture`, informational): small strict
  copy `crash_points=50 states=694 exhaustive=true bounded_points=0`;
  worker-written copy `crash_points=35 states=549 exhaustive=true
  bounded_points=0`; enlarged strict copy 87 points, 30 bounded; enlarged
  group copy 93 points, 21 bounded.

Docs: `docs/design.md` (a repeated directory closes the batch; a level's
failed shared step clears before it unlinks), `docs/formal/README.md`
(the new row), `catalogue/Directories.dhall`, `configs_dir.tsv`,
`MC_dir_foreign_replace.cfg`.

## Open

- **Rig.** Re-baseline on mbp-13 at this branch's head with the new
  timers, W=1 and W=2 (finding 5), the `strace` census, and the two floors;
  report 1b's saving from `recv_setup_ns`. Gate of record only there.
- **Commit/PR.** Staged, not committed; the coordinator commits, opens the
  PR and runs the adversarial review owed by every durability item (1a, 2,
  3).
- **Linear.** The distilled facts (dispositions, results, the errseq check)
  belong on TIN-4543 as a dated comment; not posted from this lane.
- `copy` now reports a destination's non-transport refusal before the
  source's broken stream: a visible behaviour change for a destination
  refused at setup (it used to surface as the source's `IO`).
- `walked` probes every directory entry's leaf (`directory_is_new`) when
  batching is on, and a directory decided at once then opens its parent
  again: a few extra syscalls per existing directory on a rerun. Not
  measured.
- `MC_main` was not re-run to completion on this host (budget at 600 s).
- The bench's `--only rclone` defect (step 1a) is still open.
- Questions for the operator (carried, narrowed): **Q1** the directory
  model is its own module, as built; **Q2** strict mode does not kick;
  **Q3** a kick's returned error refuses the entry (errseq now checked, so
  ignoring it would also be sound); **Q4** the batch bounds are 64 members
  (descriptor-scaled), 256 frames, `WalkDone`; **Q5** directory finishing
  does not use #210's device seal; **Q6** W = 2 by default, capped at the
  spare cores; **Q7** the next lever after the rig numbers.
