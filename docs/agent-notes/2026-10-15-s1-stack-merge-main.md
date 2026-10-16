# 2026-10-15: S1 stack merged with main a8d837a (OI-1003-Q173)

Branch `feat/s1-stack-20261015` (f9a0c0e: #210, S1 throughput 8a24161,
levers 59e5e60, clone study 9806d36 and f9a0c0e) merged with `origin/main`
a8d837a (#225 transfer closure, #227 Q160/Q161, estate converge Q144). The
merge is resolved and staged, not concluded; the coordinator commits it
signed. Rulings: OI-1003-Q173 (merge on an independent review), Q154, Q165,
R-N13.

## Resolution

- `catalogue/Types.dhall`: `Module` holds all five modules,
  `BulkloadTransfer` 0, `GitCarry` 1, `SqliteCarry` 2, `EstateConverge` 3
  (main's index kept), `DirectoryRecords` 4.
- `catalogue/Catalogue.dhall`: `files` and `grounding` take both
  `EstateConverge` and `Directories`; both invariant exports kept.
  `just tla-render --check`: 239 files current.
- `docs/formal/README.md`: the EstateConverge and Directory records sections
  both kept (in that order); the file table names `configs_ec.tsv` in the
  render and check lines; main's duplicate `Lib.dhall` row dropped.
- `docs/slo.md`: main's three 2026-10-15 amendments (Q144/Q149, Q152,
  Q160/Q161) first, then this stack's clone amendment (Q164-Q166), in ruling
  order.
- `transfer.rs`: main's `copy_roots_apart` (#225, called by
  `transfer_outcome::record_session` before its first write) now makes the
  stack's full overlap decision (WP1 PR 3 plus Q143 item 1b: source state
  vs destination and vs destination state). `copy_with` calls the shared
  `copy_roots`, which returns the canonical destination root the loopback
  hand-off is keyed by. So the CLI's recorded copy refuses the same overlaps
  as the library copy, before anything is written.

## Test fixes

- `transfer::tests::a_copy_moves_the_hand_off_timers` was stale after lever
  1 (verify once in loopback leaves `recv_verify_ns` at 0 by design). It now
  runs both arms: hand-off on asserts `wire_handed_bytes` grew by the file;
  hand-off off asserts `blake3_dest_wire_verify_bytes` grew by the file and
  a verify timer (inline or worker) moved.
- `source_command_registry` P84 (Q165): main's estate converge added
  `fs::copy(&index,&merged)` in `git_carry::converge::publish_index`. It is
  registered with its argument: both files are the destination checkout's
  (`.git/index` and converge's private temporary), never a source file.
- `io::durable::tests::a_group_waits_out_its_idle_and_closes_at_its_age`
  failed once under host load 45-60 on 32 CPUs (`[2, 1, 1, 1]`: a 60 ms gap
  against a 20 ms idle). The split case now gaps 200 ms (ten idles); the
  grouping and age cases are unchanged.

## Validation

Recorded in the coordinator's receipt for this merge (fmt, clippy x3,
`just rust-test`, fault_harness, power_loss, io lib, P5, resume-power-loss,
handoff-off, contract-test, bench script tests, tla-render --check, and
tla-check of MC_main, MC_r25_strict_three, MC_sq_rollback, MC_ec_refs,
MC_dir_batched with each module's budget self-test: all as expected, total
wall 878 s, peak RSS 2023 MiB).

## Pre-merge review round (2026-10-15, OI-1003-Q173)

Eight findings from the independent review, each verified on this tree.
Mutation receipt: worktree files only, staged, not committed (R-N13,
OI-1003-Q173, Q154, Q155).

1. **One write-back error refused later healthy groups (minor; fixed).**
   Confirmed: `seal_group_data` ran `syncfs` on the first member's staged
   temporary, whose errseq cursor dates from its open, so a superblock
   error raised since then was reported again for every later group whose
   seal ran on another such descriptor (and the second step's long-lived
   destination directory did the same). `io::durable::seal_device(handle,
   directory)` now runs `syncfs` on a descriptor of `directory` it opens for
   the seal (`openat(dir, ".")`; if that open fails, on `directory` itself,
   which can only report more), then `fsync`s `handle` as before; the trace
   is unchanged (`Fsync` of the member, then `FsSync` naming it). Failing
   first: `materialize::tests::a_write_back_error_refuses_its_own_group_and_no_later_one`
   with a test model of the errseq check (`durable::superblock_error`): it
   failed (group 2 refused `EIO`) before the fix and passes after.
2. and 7. **Q141 cited as pending (minor, duplicate; fixed).** The
   `SYNCFS_REPORTS_ERRORS_SINCE` doc now cites OI-1003-Q141, ruled
   2026-10-08 (TIN-4543 comment 7734678e). design.md, slo.md and the
   2026-10-07 note already said ruled.
3. **Darwin never built (major; fixed: built and tested on Darwin).**
   Confirmed: `cargo clippy -D warnings` failed on aarch64-apple-darwin
   (neo, macOS 26.7.2, rustc 1.96.1): four `missing_const_for_fn` in stack
   code (`durable.rs`: `syncfs_reports_errors`, `syncfs_seals_handle`,
   `start_writeback`, `kick_writeback`) and one in main's bench
   (`sync_destination`, #209, already broken on main). Each now allows the
   lint off Linux with a reason. New recipe `just darwin-check` (rust-check
   plus the whole fault harness, Darwin only). The first Darwin test run
   died with neo's reboot (about 20:24); the rerun found two more:
   - **Stack test hang (fixed).**
     `transfer::tests::throughput::a_protocol_violation_with_jobs_in_flight_ends_the_session`
     (8a24161) hung for good on Darwin: the source blocked writing a data
     frame into a full socket, and the test joined it after only a
     `shutdown` of the destination end. Probed on both hosts: on Darwin a
     `shutdown(SHUT_RDWR)` of a socketpair end leaves the peer's blocked
     `write` blocked, and only a `close` of that end ends it (`EPIPE`); on
     Linux the `shutdown` ends it. The test now closes every descriptor of
     its end before the join, as `copy_with` already does (production is
     unaffected: `copy_with` drops `output`, the reader clone and
     `receiver` before `producer.join()`). Failing first: hung on Darwin
     (sampled: `serve` in `write_data`, the test in the join); passes after
     on both.
   - **Main's Darwin defect (not the stack's; not fixed here).**
     `io::tests::background_priority_is_inherited_by_threads_and_children`
     fails on Darwin with or without an outer `nice`: a thread spawned
     after `enter_background` does not report `in_background`. Neither the
     test nor `sys_darwin::enter_background`/`in_background` differs from
     `origin/main` (the stack adds only `barrier_untraced` to
     `sys_darwin.rs`), so it is on main as is; it needs its own issue.
     `just darwin-check` fails on it until then.
   Darwin results on this tree (neo, `nice -n 15`, 4 jobs): fmt and both
   clippy passes clean; `just rust-test` every group passes except the
   unit group's one main failure (709 passed, 1 failed, 5 ignored);
   fault-harness clippy clean, `fault_harness` 105 passed, `power_loss` 16
   passed (the Linux-only batched proof does not build there); the
   remaining fault steps are listed under Validation. Residual, stated not
   fixed: the WAL template's `F_BARRIERFSYNC` ordering is the database
   file's own I/O, outside the R-N88 trace on both platforms, so no
   power-loss proof runs against it on APFS; it rests on the design
   argument, and design.md now says so.
4. **No exhaustive proof of the batched path (minor; fixed).** New
   `power_loss::every_power_loss_state_of_a_small_batched_copy_is_explored`
   (Linux): `a`, `b`, `p/c` on `/dev/shm`, asserts at least two `FsSync`s
   on the destination device, no bounded point (42 crash points, about
   13,100 states). Teeth: the `FsSync` before the renames dropped yields a
   wrong-bytes violation for a file that seal covered without its own
   `fsync`.
5. **`writes_precede_seal` accepted any `FsSync` (minor; fixed).** The
   helper now takes the last durable sync before the file's rename that is
   its own or an `FsSync` of its own device. Failing first: the new test's
   two mutations (a write moved to just after the seal before its rename;
   every `FsSync` moved to another device) passed the old helper and fail
   the new one.
6. **P84 docs said two copy sites (minor; fixed).** `io/mod.rs`, design.md
   and slo.md name the third, converge's `publish_index`; slo.md says Q165
   answered Q-d for the two receipt copies only, and whether it extends to
   the index copy is not ruled. The registry reason now says the input is
   the landed checkout's index, possibly rewritten by the operator's Git.
8. **Hand-off and R-N57 (minor; partly fixed, partly rejected).** Fixed:
   `r23_ab.py` records each rep's `wire_handed_bytes_total`, the report's
   `loopback_handoff` per label, and an evidence line naming the open
   R-N57 wording (tests in `test_r23_ab.py`, failing first). Rejected: a
   code refusal of `of_record=true` until a ruling. Admissibility of the
   hand-off under gate (a) is an operator wording decision, not a defect,
   and no gated sample is near (Q143's 0.90x informational pre-check comes
   first). **Operator question:** amend R-N57 or the Q154 ruling text to
   name the hand-off before any gated S1 sample.

### Review round, second pass (2026-10-15 late)

The first pass's edits were found unstaged after its session ended; each
was re-verified here before staging (R-N13; OI-1003-Q173, Q155).

- Failing first, re-run: finding 1's test fails (group 2 refused `EIO`)
  with `syncfs` put back on the member's own descriptor, and passes with
  the fix; finding 5's old helper fails the new batched proof ("a write
  after the device seal ahead of its rename passes the ordering check").
  The batched proof on the real trace: 45 events, 42 crash points, 13,111
  states, exhaustive, 0 bounded, 0 violations.
- design.md: the WAL template paragraph now says its ordering is outside
  the R-N88 trace and unmeasured on APFS.
- Linux (sting, nice 10, 8 jobs): `just check-fast` (check-source with
  rust-check, the whole fault harness with handoff-off, contract-test),
  `test_r23_ab.py` (90 tests, 1 skipped) and `just tla-render --check` (239
  files current): pass, 15 min 47 s, peak RSS 1.8 GiB. `just tla-check
  MC_main MC_r25_strict_three MC_sq_rollback MC_ec_refs MC_dir_batched`:
  every row as expected, total wall 1023 s, peak RSS 2010 MiB. After the
  throughput test fix: that test, fmt and clippy again, pass.
- Darwin (neo): the run in finding 3, then the fault harness's remaining
  steps with only main's one failing test skipped: `io::` lib 71 passed,
  io-partial-write-alone, resume-power-loss (7), and handoff-off
  (`w3_engine`, `fault_harness` 105, `power_loss` 16, `s3_transfer_resume`
  20, `sqlite_carry` 3, resume-power-loss 7): pass.

Darwin build copy: `neo:~/git/bulkload.worktrees/s1-stack-darwin-20261015`
(a tar of this tree, not a checkout), target on
`/Volumes/TinylandSSD/tinyland/cache/cargo-target/s1stack-darwin`
(R-N69 prefix, nice 15, 4 jobs).
