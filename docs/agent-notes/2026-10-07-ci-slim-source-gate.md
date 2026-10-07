# 2026-10-07: slim the PR source gate (lane `ci-slim-source-gate`)

Branch `ci/slim-source-gate-20261007`, worktree
`bulkload.worktrees/ci-slim-source-gate-20261007`, host sting. No PR is open
yet: the lane pushes the branch and the coordinator opens it.

Rulings: OI-1003-Q81 (the slim-CI source gate is in the next wave),
OI-1003-Q7 and OI-1003-Q14 (CI stays slim; fixed-seed bounded corpus),
OI-1003-Q78 (the fixed seed everywhere; the deep tier only multiplies
cases), OI-1003-Q85 (check-fast runs detached, per lane), R-N122 (pins move
with their recipes), R-N13.

## What changed

All of it is in justfile recipe bodies and test code. `.github/`,
`flake.nix` and `flake.lock` are untouched, so no workflow, action or guard
digest moved. No crate was added: `cargo nextest` is not in the dev shell
(`error: no such command: nextest`).

1. **Source gate runs its tests in parallel** (`just rust-test`, called by
   `rust-check`). One `cargo test --workspace --locked --no-run`, then up to
   five concurrent cargo invocations: `--lib --bins`, `--doc`, and the
   integration binaries the build made, read from its `Executable
   tests/<name>.rs` lines and split by first letter (a to e, f to g, the
   rest). The recipe fails if a group fails, and unless the groups ran
   exactly as many test executables as the build made. Name globs
   (`--test '[f-g]*'`) were tried first and dropped: cargo refuses a glob
   that matches a target whose required feature is off (`fault_harness`,
   `power_loss`).
2. **Feature-union fault gate.** `fault-harness` builds once with
   `--features fault-injection,io-trace` and runs `fault_harness`,
   `power_loss`, the `io::` lib tests, `io-partial-write-alone` (P5) and
   `resume-power-loss`. The source gate no longer builds `io-trace` tests;
   it keeps the `io-trace` clippy pass, the one place PR CI compiles that
   feature alone.
3. **Deep tier.** `just props-deep` and `just crash-sweep`, on demand, never
   a PR gate, in neither `check-optional` nor `check-full` (they take 30 and
   20 minutes here).
4. **OI-1003-Q78.** `test_support::prop_config` keeps
   `RngSeed::Fixed(CI_SEED)` in the deep tier and only multiplies cases by
   20. `prop_seed_guard::the_helper_fixes_the_seed_and_persists_nothing`
   now asserts the seed in both tiers. Doc text that said the deep tier is
   random is corrected in `git_estimate_dag.rs`, `s3_walk_resume.rs`,
   `s3_transfer_resume.rs`, `decide_tests.rs` and the plan (the conventions,
   D3, and the P18, P19, P21, P23, P66 and P67 rows).
5. **P30.** `fd_limit.rs` holds both rows; `fd_limit_directories.rs` is
   deleted. Both `#[test]` names are unchanged; a mutex in the file runs
   them one at a time, because both lower the process-wide limit.
6. **Contract test.** `rust-check`, `fault-harness`,
   `io-partial-write-alone` and `resume-power-loss` are re-pinned;
   `rust-test`, `props-deep` and `crash-sweep` are new pins.
   `validate_p5_alone` holds P5 to `fault-harness`;
   `validate_rust_test_groups` holds `rust-test` to its build, groups and
   count check; 11 new mutation rows.

## Every test that moved, and where it runs now

**Moved to the deep tier** (each skips itself in the PR gate unless
`BULKLOAD_PROPTEST_DEEP=1`; `just props-deep` runs them):

| Test | Reason | What the PR gate keeps |
|---|---|---|
| `git_carry::refs_scale_tests::refs_scale_131072_refs_carry_and_the_old_format_is_refused_typed` | over a minute of wall | new `a_small_capture_carries_and_its_old_format_over_the_cap_is_refused_typed`: a small generated shape carries and restores in both formats under the header law; its old-format bundle is accepted by `requires_base`, `chain::advertised` and `PackStats::record`, and the same bundle with its header grown past `HEADER_CAP` is refused `GIT_INVENTORY_OVER_CAP` by each. Unchanged: `every_header_reader_refuses_an_over_cap_header_by_size`, `writers_measure_an_over_cap_header_before_writing_it`. Not kept in the PR gate: a real capture of 130,000 refs or more with a header under 1 MiB. |
| `refs_scale_distinct::distinct_heavy_32768_refs_import_linearly_and_chain_thin` | 87 to 112 s of wall | new `distinct_heavy_16384_refs_import_linearly_and_chain_thin`: the same row at half the size. The quadratic fetch would cost about 169 s of CPU there against a 61 s budget, so it still fails the row. |

**Retired as duplicates** (fault gate; 8 live-writer tests before, 4 now):

| Retired | Still covered by |
|---|---|
| `live_writer_in_place_overwrite_refuses` | `live_writer_in_place_overwrite_leaves_no_source_ledger_row` |
| `live_writer_truncate_refuses` | `live_writer_truncate_leaves_no_source_ledger_row` |
| `live_writer_rename_replace_refuses` | `live_writer_rename_replace_leaves_no_source_ledger_row` |
| `live_writer_same_size_mtime_restored_refuses` | `live_writer_same_size_mtime_restored_leaves_no_source_ledger_row` |

Each retired test was one call, `live_writer(mutation)`. Its survivor calls
`assert_no_victim_ledger_row(mutation)`, whose first statement is the same
`live_writer(mutation)`.

**Moved between PR gates** (source gate → fault gate, still in PR CI):

- `cargo test -p bulkload-agent --lib … io::`: 61 tests run and 2 ignored at
  `34e945e` (`io::buf::tests` 5, `io::chunker::tests` 10,
  `io::crash_check::tests` 26, `io::durable::tests` 3, `io::limits::tests`
  1, `io::tests` 17 of which `traced` 6, `io::transport_tests` 1). The 63
  result lines of the source gate's last run and of the fault gate's run
  are the same set, apart from the ignore reason this lane reworded.
- `io::tests::traced::partial_write_prefix_is_traced` (P5), through
  `io-partial-write-alone`.
- The non-traced `io::` tests also still run in the source gate, under the
  shipped configuration, in `rust-test`.

**Merged, same names:**
`more_new_directories_than_the_descriptor_limit_all_finish` moved from
`tests/fd_limit_directories.rs` into `tests/fd_limit.rs`, beside
`seven_hundred_files_copy_under_a_256_descriptor_limit`.

**New in the PR gate:** the two small rows above, and
`fault_harness::crash_sweep_every_point_and_nth`, which only checks the
sweep's table unless `BULKLOAD_CRASH_SWEEP=1`.

Why the union build is safe: a fault point fires only under
`BULKLOAD_FAULT` (`fault::armed`), and a traced call records only while a
recorder is attached (`recorder::serialize` returns with nothing held when
none is). The lib test list is identical under `io-trace` alone and under
the union (490 tests at `34e945e`, compared with `--list`).

## Measurements (sting, 32 cores, never quiet)

`just check-source` and `just fault-harness`, each from a clean build of its
own and then warm. "Before" on tmpfs and in the isolated fault rows is a
`git archive` of main in `/srv/cache/jess/ci-slim-source-gate-base-src`;
"after" is this worktree. The 1-minute load is given start → end.

| Gate | Run | Before | After |
|---|---|---|---|
| `check-source` | main `d9aa729`, TMPDIR on tmpfs, clean build | 594 s (117 → 40) | 319 s (45 → 49) |
| `check-source` | same, warm | 304 s (40 → 55) | 135 s (61) |
| `check-source` | main `34e945e`, TMPDIR on the scratch disk, clean build | 545 s (7 → 16) | no green run (below) |
| `check-source` | same, warm | 408 s (41 → 29) | `rust-check` alone 188 s (18 → 27), all groups green; the lints and the secret scan add about 12 s |
| `fault-harness` | main `d9aa729`, tmpfs, clean build | 290 s (28 → 47) | 264 s (32 → 41) |
| `fault-harness` | same, warm | 36 s (30) | 52 s (46) |
| `fault-harness` | main `34e945e`, disk, clean build | 280 s (33 → 38) | 265 s (71) |
| `fault-harness` | same, warm | 57 s (31) | 85 s (53) |

- Source gate: about −45 % from a clean build, −55 % warm.
- Fault gate: −5 to −9 % from a clean build; warm it is slower by a few
  seconds, because it gained the `io::` lib tests and P5 and lost only four
  short runs. The plan's estimate (−60–150 s) was too high.
- Per group, warm, tmpfs, after: unit 125 s; a to e 20 s; f to g 53 s; rest
  92 s. The unit group bounds the gate.
- On the scratch disk the groups contend: beside the others `estimate_cli`
  took 115 to 152 s (55 s alone) and `fd_limit` 129 s (43 s alone).
- Deep tier, once each, green: `props-deep` 1,795 s (lib 593 s,
  `refs_scale_distinct` 308 s with all three rows run); `crash-sweep`
  1,171 s: 19 points, 645 crashes, 0 failures, 8 points at a time. Hits per
  point: 49 for the per-file points, 51 for `receive.after_decide`, 1 for
  the five `directory.*` points and `serve.before_done`.

Disk runs of the new `check-source` that are **not** in the table: 310 s and
355 to 394 s from a clean build, 151 to 177 s warm. In each,
`git_capture_counters` (and once `git_group_minimality`) was refused
`DESTINATION_SPACE_INSUFFICIENT`, so cargo stopped that group early and the
time is understated.

## The host's free-space floor blocked disk runs

`git_capture_counters` and `git_group_minimality` run the CLI, which keeps
its default 25 % free-space floor on the filesystem that holds TMPDIR.
During this lane `/srv/scratch` sat at 75 to 82 % used (most of it
`tmp/claude-1000`, 45 GB, and one 6.8 GB log; neither is this lane's), and
`/srv/cache/jess` is at 91 % of its project quota. Unmodified main failed
the same way in the scratch copy (18 `git_group_minimality` tests), and a
hand-run `estate-capture` printed the refusal. So the gate runs that count
used a private TMPDIR on tmpfs, `/dev/shm/ci-slim-source-gate-cf`, as lane
`q42-l6b-grouped-chain` did the same evening. tmpfs makes fsync free, so
those times are lower than a disk's for both columns.

## Validation

- `tests/test_ci_contract.py`: 24 tests, OK.
- `cargo clippy` with default features, and with
  `fault-injection,io-trace`: clean.
- `just props-deep` and `just crash-sweep`: green (above), at `34e945e` plus
  this lane.
- `just check-fast`: see the receipt at the end of this note.

## Open

1. **The PR's own CI run is the proof of the gate time.** Local numbers are
   from a loaded host and partly from tmpfs.
2. **The unit-test group now bounds the source gate.** Two lib tests run for
   over a minute: `an_old_format_capture_of_65536_refs_imports_like_the_new_format`
   and `transfer::tests::a_capped_subtree_is_reported_never_carried`. Next
   are `estimate_cli::no_corpus_probe_reaches_a_refusal_line` (55 s) and
   `s3_transfer_resume` (27 s of real settle waits). None was touched: the
   brief named two REFS-SCALE rows. Moving or shrinking them needs a ruling.
3. **Parallel groups on a small or slow-disk runner.** The groups contend
   for fsync. If the CI runner shows the same 2 to 3 times stretch, fewer
   groups may be faster; the recipe's group count is one `case` statement.
4. **The 25 % free-space floor makes check-fast depend on the host's disk.**
   Two mandatory binaries fail whenever TMPDIR's filesystem is under the
   floor. That is not this lane's to fix (the tests are not its files); a
   `--min-free-percent=0` in those tests' verb calls, or a roomier TMPDIR
   for lanes, would remove it.
5. **`crash-sweep` does not sweep `git_scenarios!`** (its file goes with
   carry_v2 in Q42 L5), and it runs `nth > 1` with one file per commit
   group, as the pinned `_mid` scenarios do.
6. **L5 (#189) will conflict** in `justfile` (`check-optional`) and
   `fault_harness.rs` (`every_fault_point_has_a_scenario`); neither hunk
   overlaps this lane's logic. After L5, `git_carry_v2` leaves group f to g.
7. A measurement script of this lane removed its own `target/fault`
   directories through a variable path, not a literal one. The paths were
   this worktree's and the scratch copy's.

## Shas and receipts

- Base: main `3931471` (#195). The lane started on `34e945e` (#193) and was
  moved onto `d9aa729` (#194, #198) and then `3931471` while it had no
  commit of its own, so each move was a fast-forward with the work
  reapplied. Conflicts with #194 and #198 were resolved by taking main's
  side and reapplying this lane's change (`git_estimate_dag.rs`,
  `fault_harness.rs`, the plan, the `resume-power-loss` comment).
- The lane's work is one signed commit on that base, on
  `ci/slim-source-gate-20261007`; its sha is in the lane's result and in
  `git log`.
- `just check-fast` on that tree (main `3931471` plus the lane's changes,
  before this receipt was added to the note): **exit=0**, 2026-10-07 00:23:30
  to 00:31:21 EDT (471 s, including the rebuild after the fast-forward),
  load 29 → 30, TMPDIR `/dev/shm/ci-slim-source-gate-cf`,
  `CARGO_TARGET_DIR=/srv/cache/jess/cargo-target/ci-slim-source-gate`. 31
  `test result: ok` lines, no failure: lib 503 passed and 5 ignored;
  `rust-test: all 23 test executables ran, in 5 groups`; `fault_harness`
  53; `power_loss` 9; `io::` 62 passed and 2 ignored; P5 1; resume proofs
  3; the contract test OK.
- Rulings cited: OI-1003-Q81, OI-1003-Q7, OI-1003-Q14, OI-1003-Q78, R-N13.
