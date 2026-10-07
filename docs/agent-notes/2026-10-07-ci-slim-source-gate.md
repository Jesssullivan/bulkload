# 2026-10-07: slim the PR source gate (lane `ci-slim-source-gate`)

Branch `ci/slim-source-gate-20261007`, worktree
`bulkload.worktrees/ci-slim-source-gate-20261007`, host sting. The recheck
stage opened the PR from this branch after the note's last section was
written (`gh pr list --head ci/slim-source-gate-20261007`); it is not merged.

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
   20 minutes here). Both recipes fail unless their tests really ran (see
   "Review fixes").
4. **OI-1003-Q78.** `test_support::prop_config` keeps
   `RngSeed::Fixed(CI_SEED)` in the deep tier and only multiplies cases by
   20. The tier is a parameter (`prop_config_for(cases, deep)`), and
   `prop_seed_guard` asks for both tiers in the PR gate (see "Review
   fixes"). Doc text that said the deep tier is random is corrected in
   `git_estimate_dag.rs`, `s3_walk_resume.rs`, `s3_transfer_resume.rs`,
   `decide_tests.rs` and the plan (the conventions, D3, and the P18, P19,
   P21, P23, P66 and P67 rows). Two properties are not on the helper; see
   Open 8.
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

**Read these as rough local numbers, not as the gate saving.** The PR's own
CI time is the acceptance number; no CI run of this branch existed when this
was written. What limits them:

- They were taken at main `d9aa729` and `34e945e`, not at the pushed base
  `3931471`.
- "Before" ran from a `git archive` of main in
  `/srv/cache/jess/ci-slim-source-gate-base-src`; "after" ran from this
  worktree on `/srv/fast-local`. Different filesystems for the sources.
- The host was loaded and the load differed between the two sides. The
  1-minute load is given start → end.
- Every green run of the new `check-source` had TMPDIR on tmpfs, where
  fsync is free. **There is no green run of the new `check-source` on a
  disk** (next section).

| Gate | Run | Before | After | Comparable? |
|---|---|---|---|---|
| `check-source` | main `d9aa729`, tmpfs, warm | 304 s (40 → 55) | 135 s (61) | yes: the one like-for-like pair, tmpfs only |
| `check-source` | main `d9aa729`, tmpfs, clean build | 594 s (117 → 40) | 319 s (45 → 49) | no: "before" started at load 117, "after" at 45 |
| `check-source` | main `34e945e`, scratch disk, clean build | 545 s (7 → 16) | no green run | no |
| `check-source` | same, warm | 408 s (41 → 29) | `rust-check` alone 188 s (18 → 27), all groups green; the lints and the secret scan were not run with it (about 12 s elsewhere) | no: not the same recipe |
| `fault-harness` | main `d9aa729`, tmpfs, clean build | 290 s (28 → 47) | 264 s (32 → 41) | roughly |
| `fault-harness` | same, warm | 36 s (30) | 52 s (46) | roughly |
| `fault-harness` | main `34e945e`, disk, clean build | 280 s (33 → 38) | 265 s (71) | no: load 33 against 71 |
| `fault-harness` | same, warm | 57 s (31) | 85 s (53) | no: load 31 against 53 |

- Source gate, warm, tmpfs: 304 s → 135 s (−55 %). That is the only
  comparable pair, and it is tmpfs only.
- Source gate, clean build: **not measured like for like.** The first
  commit's message says "594 s to 319 s from a clean build"; that pair is
  confounded by load and is tmpfs only, and the commit cannot be reworded
  without a force-push. The low-load disk "before" was 545 s, so the clean
  saving is at best about 545 s → 320 s, and only where fsync is free. On a
  disk-backed runner the saving may be much smaller or absent: the groups
  contend (below).
- Fault gate: 15 to 26 s less from a clean build (−5 to −9 %), and 16 to
  28 s more warm (36 → 52 s on tmpfs, 57 → 85 s on disk, +44 to +49 %),
  because it gained the `io::` lib tests and P5 and lost only four short
  runs. The plan's estimate (−60–150 s) was too high.
- Per group, warm, tmpfs, after: unit 125 s; a to e 20 s; f to g 53 s; rest
  92 s. The unit group bounds the gate.
- On the scratch disk the groups contend: beside the others `estimate_cli`
  took 115 to 152 s (55 s alone) and `fd_limit` 129 s (43 s alone).
- Deep tier, once each, green, before the review fixes: `props-deep`
  1,795 s (lib 593 s, `refs_scale_distinct` 308 s with all three rows run);
  `crash-sweep` 1,171 s: 19 points, 645 crashes, 0 failures, 8 points at a
  time. Hits per point: 49 for the per-file points, 51 for
  `receive.after_decide`, 1 for the five `directory.*` points and
  `serve.before_done`.

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

## Review fixes (second commit, 2026-10-07)

The review of `0493713` had 12 findings, 5 medium and 7 low. The five medium
ones are fixed in a second signed commit. The round was interrupted by a
reboot of sting at about 01:03 EDT; the edits were on disk, were reviewed
again line by line, and all nine files were checked whole (`cargo fmt
--check`, `py_compile`, `just --list`, no NUL bytes).

1. **Q78 is now held in the PR gate.** Before, the guard skipped the helper
   file and asserted the seed only in the tier the process was in, and PR CI
   never sets the deep switch; the pre-Q78 helper (random in the deep tier)
   would have passed. Now:
   - `test_support::prop_config_for(cases, deep)` takes the tier;
     `prop_config(cases)` reads the switch through `test_support::deep()`
     and calls it.
   - `prop_seed_guard::the_helper_fixes_the_seed_in_both_tiers_and_persists_nothing`
     asks for `deep = false` and `deep = true` and requires
     `Fixed(CI_SEED)`, no persistence, and 7 and 140 cases.
   - `the_helper_source_names_one_fixed_seed` reads the helper's source: one
     config literal, one seed line (`rng_seed: RngSeed::Fixed(CI_SEED),`),
     no `Random`, persistence off once.
   - `the_guard_refuses_a_helper_that_is_not_fixed_everywhere` holds eight
     helper mutants, the first being the pre-Q78 helper.
   - The scan refuses a property that calls `prop_config_for` itself.
2. **"Fixed everywhere" is reworded.** It was false of the pushed tree. The
   justfile comment, AGENTS.md, the helper's module doc and the plan (§1
   Conventions, §3, D3) now say "every property that runs through the
   helper" and name the two that do not (Open 8).
3. **The deep recipes fail closed (R-N122).** A skipped deep row, a skipped
   sweep and a name filter that matches nothing all exit 0 in cargo.
   - `props-deep` leaves the three heavy rows out of the workspace run
     (`--skip`) and runs each alone with `--exact --nocapture`. It fails on
     a cargo failure, on a `skipped: set` line, on anything but one
     `1 passed; 0 failed` result with the row's own `ok` line, and unless
     the row's measurement line appears once (`REFS-SCALE row=fixed refs=`,
     `REFS-SCALE-DISTINCT row=deep-32768 pass=2`, `REFS-SCALE-DISTINCT
     row=deep pass=2`). Only the row's body prints that line.
   - `crash-sweep` fails on a cargo failure, on the `crash sweep skipped`
     line, on anything but one `1 passed; 0 failed` result with the sweep's
     own `ok` line, and unless there is one `<n> points, <m> crashes, 0
     failures` line and n `hits=` lines, with n and every count above 0.
   - `refs_scale_distinct.rs` no longer mirrors the switch's name: it
     compiles `src/test_support.rs` and calls `test_support::deep()`, as
     `refs_scale_tests.rs` now does too.
   - Contract test: both bodies are re-pinned, `validate_deep_recipes`
     requires each guard line exactly once and no extra `cargo test`, and
     there are 18 new mutation rows (a lost switch, a lost row, a lost
     `--exact` or `--nocapture`, each check turned off).
4. **The timing claim is withdrawn as a headline** (Measurements, above).
   Only the warm tmpfs pair is like for like. Nothing was re-measured: the
   host ran at a 1-minute load of 100 to 127 during this round, so a new
   pair would be no better. The PR's CI run is the evidence.

## Validation

- `tests/test_ci_contract.py`: 24 tests, OK (after the review fixes).
- `prop_seed_guard`: 8 tests, OK.
- A skipped deep row was run by hand with the recipe's flags and no switch:
  it prints `REFS-SCALE-DISTINCT row=deep-32768 skipped: set
  BULKLOAD_PROPTEST_DEEP=1` and then `1 passed`, which is the output the
  recipe now refuses.
- `just props-deep` and `just crash-sweep` were green at `34e945e` plus the
  first commit, with the old one-line bodies. For the new bodies see the
  receipts at the end of this note.
- `just check-fast`: see the receipts at the end of this note.

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
8. **Two properties are not on the helper**, so the seed is not yet fixed
   everywhere (OI-1003-Q78). Both are `EXEMPT` entries of the seed guard.
   - `git_carry_v2::random_dags_equal_upload_pack` draws 12 cases from a
     random seed in the PR gate and in the deep tier. Q42 L5 (#189) deletes
     the file and lowers the guard's ceilings (`EXEMPT_CEILING` 2 to 1,
     `FINDINGS_CEILING` 11 to 7). #189 was not on main when this was
     written; when it is merged here, its side of those lines wins.
   - `refusal_taxonomy` has its own fixed seed and does not multiply its
     cases in the deep tier. It migrates after #189.
9. **Low review findings, not fixed** (for the PR body):
   - `rust-test` parses cargo's `Executable` lines; a forced colour setting
     on the runner would make it count 0 and fail every run. Group output
     is printed only after all groups end, so a stall shows nothing.
   - `check-fast`, `check-optional` and `check-full` are not pinned, so
     nothing stops a deep recipe from being added to them; `just_recipe()`
     stops reading a body at a blank line.
   - No green run of the new `check-source` on a disk exists (Open 4), so
     peak scratch use of the parallel groups is unmeasured.
   - The 16,384 PR row has less margin than the 32,768 row had: about 2.5
     to 2.8 times against the quadratic fetch, where it was 6.6 times.
   - No PR gate compiles `fault-injection` alone any more, and the fault
     harness now runs with the `io-trace` recorder mutex compiled in.
   - The warm fault gate is slower by 16 to 28 s; this note's text is
     corrected, the first commit's message is not.

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
- First commit: `0493713`, signed, pushed.
- Review fixes: a second signed commit on the same branch (its sha is in
  the lane's result and in `git log`). `origin/main` was still `3931471`
  when it was made, so there was nothing to merge and #189 had not landed.
- `just check-fast` on the review-fix tree (the exact tree of the second
  commit, apart from this receipt): **exit=0**, 2026-10-07 01:53:01 to
  01:59:45 EDT (404 s, warm), 1-minute load 127 → 107, TMPDIR
  `/dev/shm/ci-slim-tmp` (tmpfs, as the host note for this round directs),
  `CARGO_TARGET_DIR=/srv/cache/jess/cargo-target/ci-slim-source-gate`. 31
  `test result: ok` lines, no failure and no `error` line: lib 503 passed
  and 5 ignored; `prop_seed_guard` 8; `rust-test: all 23 test executables
  ran, in 5 groups`; `fault_harness` 53; `power_loss` 9; `io::` 62 passed
  and 2 ignored; P5 1; resume proofs 3; the contract test OK. Both clippy
  passes and the union clippy pass are clean.
- Review fixes: `478ee06`, signed, pushed.
- The new deep recipe bodies, run on `478ee06` after it was pushed
  (2026-10-07 02:00 to 02:26 EDT, 1-minute load 110 → 63, TMPDIR on tmpfs):
  - `just crash-sweep`: **exit 0**, the whole recipe. `19 points, 645
    crashes, 0 failures, 8 at a time`, 19 `hits=` lines, `1 passed` in
    764 s, then the recipe's own `crash-sweep: all 19 points were swept`.
  - `props-deep`, **the three rows only**: the recipe's body was copied to a
    scratch script without its workspace line and run; exit 0, `props-deep:
    the workspace and all 3 deep rows ran`. 131,072 refs 94 s; 32,768
    commits 135 s (import CPU 22.6 s and 32.7 s against 101.9 s); 110,000
    commits 482 s (77.0 s and 84.9 s against 295 s, written
    self-contained).
  - **Not run:** the recipe's first line, the 20-times workspace run with
    the three `--skip` filters (about 30 minutes on a quiet host). The
    filters were checked with `--list` on `refs_scale_distinct`: only the
    16,384 row is left. So `just props-deep` has not been run end to end
    in its new form.
  - The refusing paths (a skipped row, a missing line) were not run through
    the recipes; they are held by the contract test's mutation rows, which
    check the recipe text, and by the skipped-row output shown under
    Validation.
- Rulings cited: OI-1003-Q81, OI-1003-Q7, OI-1003-Q14, OI-1003-Q78, R-N13.

## Recheck and ship (2026-10-07, 02:28 to 03:25 EDT)

The recheck stage read the diff `0493713..c0afc52` against the five medium
findings and found each one fixed. It changed no code, recipe or pin; it
merged main twice and reworded comments and docs after #189 (below).

- **Main moved.** `origin/main` went from `3931471` to `95f43dc` (#196: the S2
  wal-index counter, `tests/sqlite_wal_index.rs`) during the recheck. Merged
  as signed merge commit `1e887a6`, no conflict.
- **Main moved again: #189 (Q42 L5) landed** (`a80c63b`). Merged as signed
  merge commit `db6f1eb`. `prop_seed_guard.rs`, `fault_harness.rs` and the
  justfile merged without conflict, and the guard's `EXEMPT` list and
  ceilings are #189's (1 file, 7 findings). The plan conflicted in two
  places; each line is the side that changed it, and P66, which both sides
  changed, is #189's text with this lane's "240 cases from the same fixed
  seed". The same commit rewords what this lane had written about the
  random-seed property, now that its file is gone: AGENTS.md, the
  `props-deep` comment, the helper's module doc and the plan say one
  property is off the helper (`refusal_taxonomy`, its own fixed seed) and
  that no property draws a random seed. **Open 6 is closed and Open 8 is
  down to `refusal_taxonomy`**; the text of both above predates the merge.
- **`just check-fast`** (TMPDIR `/dev/shm/ci-slim-tmp`, the lane's own
  `CARGO_TARGET_DIR`):
  - on `c0afc52`: exit=0, 02:28 to 02:31, load 42 → 39, 31 `test result: ok`
    lines, `rust-test: all 23 test executables ran, in 5 groups`;
  - on `1e887a6`, first run: **exit=1**. The unit group failed on
    `disposition::tests::a_ledger_is_bound_to_the_plan_bytes_not_only_its_path`
    (`Err(Io(Some(11)))`, not `ReceiptBindingInvalid`). That is #200, a known
    flake on main under load; this lane does not touch `disposition`;
  - on `1e887a6`, second run: **exit=0**, 02:50 to 02:55, load 80 → 41, 32
    `test result: ok` lines, `rust-test: all 24 test executables ran, in 5
    groups` (the new one is `sqlite_wal_index`), the contract test OK;
  - on `db6f1eb` (the final code tree): **exit=0**, 03:14 to 03:22, load
    30 → 28, 32 `test result: ok` lines, 736 passed and 13 ignored, `rust-test:
    all 24 test executables ran, in 5 groups` (`git_carry_v2` left,
    `git_estimate_shapes` came), the contract test's 24 tests OK.
- **The refusing paths of both deep recipes were run**, which the fix round
  had not done. Each recipe body was copied to a scratch script
  (`/srv/cache/jess/ci-slim-mut/recheck`) with `cargo` replaced by a shell
  function that prints a canned log. `props-deep` refused a skipped row, a
  filter that matched nothing, a row without its measurement line and a
  failing cargo status (the status is passed through). `crash-sweep` refused
  a skipped sweep, a filter that matched nothing, fewer `hits=` lines than
  points, a zero hit count and a non-zero failure count, and accepted a
  well-formed log.
- **The first line of `props-deep` was run, twice** (the 20-times workspace
  run with the three `--skip` filters), which closes the "not run" item in
  the receipts above:
  - on `c0afc52`, load 80 to 99: **exit=101**. The lib binary failed on
    `transfer::tests::a_run_leaves_the_source_lstat_census_unchanged` (P-S2)
    with `Io(Some(32))`, EPIPE, at case 139 of 240. Alone and deep it then
    failed 2 runs of 3 (cases 233 and 236); in the PR tier (12 cases) it
    passed 60 runs of 60. The seed is fixed, so this is a timing race in
    `transfer::copy`, whose sources are identical to main's here. Filed as
    #201; not this lane's to fix.
  - on `1e887a6`, with `--no-fail-fast`, load 78 → 34: **exit=0** in about
    17 minutes. 27 results, all `ok`: lib 504 passed, 5 ignored, 1 filtered
    out; `refs_scale_distinct` 1 passed, 2 filtered out, so the three
    filters leave out exactly the three deep rows.
  - Neither the three rows nor the deep workspace run was repeated on
    `db6f1eb`. #196 does not touch `git_carry`; #189 deletes carry_v2 and
    changes `git_carry/estimate.rs`, which the rows do not call.
- **Still true:** `just props-deep` as one command has not been green from
  start to end in its new form. Every part of it has been, separately. On a
  loaded host it can fail on #201.

Open, added by the recheck:

10. **#201**: `transfer::copy` can return a bare `Io(EPIPE)` for a finished
    copy; P-S2 flakes in the deep tier under load.
11. **#200** failed one check-fast of this lane (above). Both are bare IOs
    under load on main.
12. Low, not fixed: `decide_tests.rs` still says "the fixed seed everywhere"
    of its own draws (true of that file, which is on the helper). With one
    test thread (`RUST_TEST_THREADS=1` or a one-core runner) libtest prints
    the test name before the row's output, so both deep recipes would refuse
    a run that really happened; they fail closed.

Shas: `1e887a6` (merge of `95f43dc`), `b7d1866` (this section's first form),
`db6f1eb` (merge of `a80c63b`), then this note's last commit (`git log`).
Rulings cited: OI-1003-Q81, OI-1003-Q7, OI-1003-Q14, OI-1003-Q78, R-N13.
