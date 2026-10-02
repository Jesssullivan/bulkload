# 2026-10-02 — #112: split `walk_ns` from walk-ahead wait

Lane: walk-ns split, host sting. Worktree
`bulkload.worktrees/walk-ns-split-20261002`, branch
`feat/walk-ns-split-112`, cut from origin/main 643b58d (#108 W4 PR 3).

Rulings cited:
- OI-1002-Q27 ("Do #112 now"): this lane.
- OI-1002-Q25: #112 filed from the #108 review.
- R-N13: receipts cite rulings; this note.
- R23, R-N57: the counter feeds the gate (a) `native_timing` line.

## What changed

- **Counters.** `walk_ns` keeps its name and now means walk work only
  (listing, stat, hand-over). A new `walk_wait_ns` counts the time the walk
  thread is blocked on a walk-ahead slot (`WalkGate`). Their sum is the walk
  thread's lifetime, which is what `walk_ns` measured after #108.
- **Why keep the name.** Before #108 the walk had no gate, so `walk_ns` was
  pure walk work (as in docs/evidence/m0-2026-09-23.md). Keeping the name
  with that meaning keeps old and new samples comparable and touches no
  consumer; the only addition is one key.
- **Where reported.** `TransferTiming` (`walk_wait_ns` field, `render`,
  `since`) and the bench `native_timing` line, right after `walk_ns`. There
  are no JSON timing counters; `m0_gate_a.py` does not parse timings.
- **Mechanics.** `WalkGate::take` reads the clock only when the gate is
  full, so the fast path gains no clock read. `walk_source` returns its
  `WalkTime` and adds it to the process counters. `WalkGate::with_limit`
  exists for the test; production uses `WALK_AHEAD`.

No wire change, no change to walk order, gate bound or `WalkEnded`.
No `unsafe`, no new dependency.

## Test

`transfer::tests::walk_ahead_wait_is_accounted_apart_from_walk_work`: a
one-slot gate and a consumer that holds each item 25 ms. Asserts
`wait_ns >= (items-1) * 12.5 ms`, `walk_ns < wait_ns / 4`, and
`walk_ns + wait_ns <= lifetime`. The pre-split accounting (`walk_ns` =
lifetime) fails the second assertion.

## Gate

`just check-fast` through `nix develop .#default`, own `CARGO_TARGET_DIR`,
`CARGO_BUILD_JOBS=4`, `RUST_TEST_THREADS=2`: exit 0. Lib 389 passed
(6 ignored, one more than #108 for the new test); io-trace io:: 69;
fault_harness 56/56; power_loss 5/5; contract tests 22 OK. Host load about
16 (not a benchmark sample, R-N81).

## Open

- The #88 gate (a) sample should read `walk_ns` and `walk_wait_ns`
  together; a large `walk_wait_ns` means the wire, not the walk, bounds it.
