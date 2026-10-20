> 2026-10-20: superseded in part by #292 (ledgers and hand-off records
> dated before their writes). The p90 and h14 re-stamps described below
> were removed there: with #292 every seat a landing or hand-off writes is
> racy against its own record, which both tests assume. The estate_move
> and reseat changes stand.

# 2026-10-19: four bulkload-agent tests no longer rest on timing or free-space drift

Lane: coordinator brief 2026-10-19 (deflake `estate_move`; later the same
day, with #283's, #280's and #286's gates failing on them too, the reseat
space test, p90 and estate_handoff h14). Branch
`test/deflake-estate-move-20261019` from `origin/main` `9854236`. Edited and
committed on sting; every build and test ran on honey only (OI-1003-Q196),
in this lane's own directories: worktree
`~/git-build/deflake-estate-move-20261019`, one `CARGO_TARGET_DIR` and
`CARGO_HOME` under `~/git-build/deflake-estate-move-20261019-cargo`,
`TMPDIR=/var/tmp/jess-bulkload-tests/deflake-estate-move-20261019`, all
under `nice -n 19` with `CARGO_BUILD_JOBS=4` and `RUST_TEST_THREADS=4`, one
cargo run at a time; removed after. Test-only: no product code changed.
Nothing ran against the live estate. R-N13 (lab
R-HOOK-CONVERGENCE-20261004).

Requirement served (docs/SPEC.md R2.3, verbatim): "**Piecemeal.** Any run
can stop at any point and resume later. A live lane is caught up by
repeated passes as its agents and projects finish, with no appointment
needed (ONCE-01, LIVE-04, LIVE-07, LIVE-09, LIVE-12, ONCE-07, ONCE-08,
SEAT-11, PUR-06, PUR-18)." The test fixed here is the proof that a later
`estate-move` pass converges a beside landing (gap C12, ruling A-115)
instead of refusing it; a proof that only runs when the host is idle does
not hold that line.

## The failure

`tests/estate_move.rs`
`one_pass_lands_adopts_besides_carries_and_accounts_for_every_item` failed
on unmodified `origin/main` under load. The assertion that fails is line
391, `converged.stdout.contains("beside-converged=1")`; the
`refs-in-kept=1` assertion just above it (381) passes. On the converge pass
the item `diverged` reports
`outcome=previous-workspace-beside-not-revalidated`: it was not captured
again, so its beside landing never met a new capture and the beside
converge the test is there to prove never ran.

Cause (the reviewer's diagnosis, confirmed in the code): nothing in
`src/diverged` changes between the passes. `estate.rs` `retained_capture`
treats an equal key as a hit only when it is `settled`, that is when no seat
is racy against the retained capture's pass start
(`git_carry::racy`: mtime or ctime at or after `started_ns -
RACY_GRANULARITY_NS`, 2 s). So `diverged` was captured again only when its
fixture files were stamped within 2 s of the first `estate-move-source`
pass's start: on an idle host the fixture and first pass take well under
2 s, under load they do not (the failing runs took 33 to 73 s). The product
is right; the test rested on wall-clock time.

## What changed (tests only)

- `estate_move.rs`: before the converge pass's `estate-move-source`, the
  source re-stamps `src/diverged/d.txt`, bytes unchanged, with
  `File::set_modified` (mtime one hour before its old one, so it differs
  and is never in the future; the ctime moves with it). The seat's stat
  identity moves, so the pass captures `diverged` again whatever the clock
  did, and every assertion after it stands as it was. No new crate.
- `estate_adopt.rs` p90: same cause, other side. The last assertion
  expected the converge to read every seat of the worktree the first apply
  landed (`infra.worktrees/only-src`), "all racy against its own ledger".
  `converge::record_landing` takes the ledger's `recorded_ns` after the
  landing wrote those seats, and `prove` trusts an unchanged print for a
  seat stamped more than 2 s before it, so under load some seats were
  proved by print (failing gates read 10 and 24 bytes, not 57). The test now
  re-stamps each of those seats, bytes unchanged, before the converge, so
  each is proved by content; the exact count, and with it "the adopted
  checkouts proved by print alone", stands.
- `src/reseat/tests/findings/space.rs`
  `a_regularization_keeps_the_placements_bytes_reserved_then_retries`: a
  different cause, free-space drift. The test probes free space itself,
  reserves all but half the link (about 37 MB of a ~74 MB transcript), and
  `regularize` probes again (`space::preflight_many`) when it judges the
  link, so the link fitted whenever the file system gained more than
  ~37 MB between the two probes (`space.rs:305:9`, `link-space-refused`
  absent, outcome `regularized`: #283's gate twice, this lane's first full
  run once). `TMPDIR` on honey is on `/`, which other users' builds share:
  six samples two seconds apart moved its free space between 806.6 GB and
  808.1 GB. For the refused pass the target is now extended with a hole to
  an apparent 64 GiB (bounded by the free space so the reservation still
  decides on a small disk), so the margin either way is half of that:
  `regularize` charges `target.len()` and reads nothing of the target
  before the space check refuses. It is cut back to its bytes before the
  pass with room, so that pass and every assertion after it are as they
  were. Not deterministic against a drift of more than 32 GiB inside one
  pass; that is the bound.
- `estate_handoff.rs` `h14_estate_diff_reads_a_handed_off_item_as_identical`:
  the p90 cause on both sides. It asserts that the diff after a hand-off
  reads exactly the targets' added block bytes more than the diff before.
  The landing's ledger (`record_landing`) and the hand-off's record
  (`next.recorded_ns`, taken after the hand-off writes and prints its
  targets) each trust an unchanged print stamped more than 2 s before
  they were recorded, so whether a diff read a target rested on how long
  the landing or the hand-off took: #286's rust-check read 22137 against
  22094, 43 bytes, one worktree's `AGENTS.md` (28) and `CLAUDE.md` (15).
  Each target is now re-stamped, bytes unchanged, before each diff, so
  each diff reads every target by content; the other seats are read alike
  by both diffs against the one ledger and cancel out. The exact
  difference and the `identical` / `ledgered` classification stand.

## Results

All on honey, under the lane limits above, one cargo run at a time, load1
33 to 160 (other users' builds).

- Baseline, unmodified `origin/main` `9854236`: the one-pass test failed
  3 of 3 at `estate_move.rs:391:5` (43.1 s, 73.0 s, 58.5 s); `diverged`
  reported `previous-workspace-beside-not-revalidated`. p90's failure is
  on record from gates (honey `beside-adopt-converge-rustcheck-20c8981`:
  10 bytes read, 57 expected; #280's head: 24, 57 expected). The reseat
  test failed in #283's gate twice and in this lane's first full run.
- `ce05867` (estate_move and p90): fmt and clippy on both test targets
  clean; the one-pass test 10 of 10 (16.5 s to 55.0 s); p90 5 of 5 (44.3 s
  to 90.3 s); `cargo test -p bulkload-agent --locked` stopped after the
  unit tests, `FAILED. 915 passed; 1 failed; 6 ignored` (893.4 s), the one
  failure the reseat test; then `cargo test -p bulkload-agent --locked
  --no-fail-fast`: 39 test binaries, 1233 passed, 0 failed, 8 ignored,
  `estate_move` 1 passed, `estate_adopt` 40 passed, exit 0 (54 min).
- `9d240db` (adds the reseat fix): `cargo fmt --all --
  --check` clean, `cargo clippy -p bulkload-agent --all-targets --locked
  -- -D warnings` clean; the reseat test 10 of 10 (0.7 s to 1.4 s); the
  one-pass test 10 of 10 (8.8 s to 34.2 s); p90 10 of 10 (23.3 s to
  62.2 s); `cargo test -p bulkload-agent --locked --lib`: 916 passed, 0
  failed, 6 ignored (604.8 s).
- `f0a8e4d` (adds h14; the PR's test head): fmt and clippy on
  `estate_handoff` clean; h14 10 of 10 (8.8 s to 21.1 s); the whole
  `estate_handoff` binary 53 passed, 0 failed (264.0 s).

## The other flakes named, and one finding

- `git_carry::source_inert_tests::p34_a_capture_leaves_the_source_lstat_census_unchanged`:
  not this cause; not changed. Seen once on honey
  (`~/git-build/logs/beside-adopt-converge-testall-20c8981.log`), panicking
  at `source_inert_tests.rs:296:50` with `CaptureDrift {
  ObjectStoreRewritten "objects/pack" }`. `export_repository_inner` returns
  that only when one of the export's Git children refused
  (`GIT_CHILD_FAILED` or `GIT_INVENTORY_MALFORMED`) and the source's pack
  listing (names, inodes and sizes under `objects/pack`) changed during the
  pass. p34 already keeps the racy window out of it (it backdates the
  objects to 2001 and presents each prior pass start 10 s later), and its
  fixture Git runs with `gc.auto=0` and `maintenance.auto=false`, so
  nothing in the test should rewrite the pack directory. What did is not
  established: that branch discards the refused child's class. If it was
  one of the export's own children writing into the source store, that is
  a source-inertness defect p34 exists to catch. Next step: a lane that
  records the swallowed refusal in that test, then repeats it under load.
- The reseat space test: a different cause, fixed here (above).

- Not a flake, found reading h14's cause: the hand-off record's comment
  says the prints it takes are "racy against `recorded_ns` (the pass's
  start), so converge trusts none until a rerun re-dates it", but
  `handoff.rs` sets `next.recorded_ns = pass_start_ns()` after the writes
  and the prints. A hand-off that takes more than 2 s from writing a
  target to that line leaves the target's print trusted at once, and the
  instant is taken after the prints, where `adopt.rs` takes its instant
  before the first read (review of 2026-10-16, finding 4). Product code;
  not changed here. A reviewer should decide whether the instant moves to
  the pass's start or the comment changes.

## Open

- The hand-off record's `recorded_ns` (above): product, for review.

- p34: find what rewrites the source's `objects/pack` (see above).
- The brief named line 394 and `refs-in-kept=1`; on `9854236` the failing
  assertion is 391 (`beside-converged=1`), and `refs-in-kept=1` was
  present in every failing run's output.
- Merge needs the local gate receipt (`just local-gate-receipt --pr N
  --post`) on the exact head, which this lane did not run.
