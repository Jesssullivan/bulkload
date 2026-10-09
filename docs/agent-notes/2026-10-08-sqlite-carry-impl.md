# 2026-10-08: SQLite carry, implementation (#218)

Lane `sqlite-carry`, branch `feat/sqlite-carry-20261008`, worktree
`bulkload.worktrees/sqlite-carry-20261008`, from `origin/main` `bdb8994`.
**Uncommitted and staged**: no commit, no push, no PR, no Linear or GitHub
comment (the lane was told not to). Builds went to
`/srv/fast-local/jess/cache/cargo-target/sqlite`.

Rulings cited: OI-1003-Q16, Q36, Q72 and Q76 (S2's SQLite exceptions, none
amended), OI-1003-Q18, Q100 to Q102 (the superseding publish; model first),
OI-1003-Q40 and #169 (R25 strict, the capture record), OI-1003-Q7 and Q78
(fixed seeds, no fuzzing), R25 / R-N58, R-N59 / R-N118 (hard wire cut),
R-N13. **Nothing here is a ruling**: every open question of the design got
a conservative answer, built and recorded as such.

## Done

- Amended [the design](2026-10-08-sqlite-carry-design.md) for the review's
  15 findings (section 0.1: all accepted; R5 with a change: the destination
  sidecar check moved to Decide and is not remembered) and recorded a
  conservative answer to each of the 12 open questions (section 0.2), with
  what the operator must still rule (section 0.3). Later sections edited to
  match what was built, each marked "(amended)".
- Built the seat: wire v6, the walk's sidecar classification, the source's
  snapshot capture, the destination's keying, verification and refusals,
  the CLI flag (default `refuse`), the counters, one new refusal code
  (`SQLITE_SOURCE_NOT_OWNER`), and the model. The design's section 14 lists
  every file and function.
- Tests: P80 to P83 (`crates/bulkload-agent/src/transfer/tests/sqlite_carry.rs`,
  20 tests), the CLI end-to-end test (`crates/bulkload-agent/tests/sqlite_carry.rs`),
  frame and store unit tests; rows in the property-test plan.
- Model: `docs/formal/sqlite/SqliteCarry.tla` (moved to `docs/formal/` in round 3), 21 configs, all outcomes as
  expected (README there).
- Docs: `docs/design.md` (Wire v6, SQLite snapshot seats, the S2 lock now
  counted). `docs/slo.md` is unchanged.

## Validation (local, uid 1000, `nix develop`)

- `cargo fmt --all -- --check`: clean.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`, the
  `io-trace` pass and the `fault-injection,io-trace` pass: clean.
- `cargo test --workspace --locked --no-fail-fast`: 754 passed, 0 failed,
  8 ignored, 32 binaries (before the last small edits); then again, after
  them: `bulkload-proto` 16 passed; agent `--lib` 568 passed, 5 ignored;
  `tests/sqlite_carry.rs` 3, `refusal_taxonomy` 5, `sqlite_wal_index` 4,
  `s3_transfer_resume` 20 (1 ignored), `source_lock_trace` 13,
  `prop_seed_guard` 8, all passed.
- Fault gate (`--features fault-injection,io-trace`): `fault_harness` 60
  passed on a rerun (the first run had `serve_before_done` stopped by its
  child watchdog while the machine was loaded; it passed alone and in the
  full rerun), `power_loss` 12 passed, `io::` 64 passed (2 ignored).
- As root (`unshare -r`): the 20 `sqlite_carry` unit tests pass (3 legs
  skipped with a note); the CLI test proves the refusal and reports its
  dropped leg skipped (no uid 65534 in the user namespace).
- `tests/test_ci_contract.py`, `validate_repo_manifest.py --self-test`,
  `test_s2_budget.py`, `test_s3_estate.py`: OK.
- TLC 1.7.4 on `docs/formal/sqlite/`: all 21 configs as expected.
- Not run: `just check-fast` as one recipe, `just tla-check` (unchanged
  models), `props-deep`, `crash-sweep`.

## Decisions taken without a ruling (conservative)

`--sqlite=refuse` stays the default; Q16 is not amended (stepped backup in
both journal modes, no pinned read transaction); quick_check at the source
plus integrity_check at the destination, FK violations carried and counted;
DELETE mode at the destination; root refused per session; no IO-class
change; a snapshot output is never superseded in place; W6 frames stay in
v6; P80 to P83; one backup at a time, 4 GiB of slots, `max(4,
ceil(pages/128) x 4)` steps, 30 s per backup; corrupt stores are typed
refusals only; name matching for orphans, base-bound coverage otherwise;
S4 for transfer refusals waits on WP3 PR 4.

## Review round 2 (staged carry, eight findings)

Recorded in the design note, section 0.1b. Accepted and fixed with a test
that failed first: 1 (an encrypted store beside a live `-wal` is refused,
never published raw), 3 (sniff descriptors closed under the backup lock),
4 (`integrity_check` at the source), 5 (a database's manifest is not
bounded by the memory budget), 7 (every slot reserved against the slot
budget, free-space floor before each backup). Accepted as coverage gaps,
tests added: 2 (a commit between backup steps, both journal modes) and 6
(a commit only in the `-wal`; a commit after the last step; a `wal_settled`
table). Rejected as stated: 8 (`SQLITE_OPEN_NOFOLLOW` refuses a symlink at
any path component in SQLite 3.46.0); its refusal now reports
`PATH_ESCAPES_ROOT`, with a regression test.

Scratch mutants, run once against the new tests and then reverted (file
checksums verified afterwards):
- `sqlite_key` ignores the `-wal`: killed by
  `p81_a_commit_only_in_the_wal_is_not_reused` (and by nothing else).
- `wal_settled` always settles: killed by `wal_settled_judges_every_pair`
  and `p81_a_commit_after_the_last_step_leaves_the_capture_unsettled`.
- backup restarts not counted: killed by
  `p80_a_commit_between_steps_restarts_the_backup_and_arrives_whole` and
  `p80_a_busy_wal_store_is_carried_or_refused_for_budget`.

Also found while rerunning the checks: the P80 property failed 2 of 18
parallel runs under load (a live DELETE-mode store's `-journal`, listed and
deleted before the walk's stat, was refused `IO`). Fixed in the walk
(design note 0.1b); P80 then passed 18 of 18 parallel runs.

Validation of round 2 (uid 1000, `nix develop`, load average about 200 to
240 on the shared host): `cargo fmt --check` clean; clippy `-D warnings`
clean in the plain, `io-trace` and `fault-injection,io-trace` passes;
`cargo test --workspace --no-fail-fast` all green except the P80 flake
above (agent lib then 578 passed, 5 ignored, after the walk fix);
`fault_harness` 60, `power_loss` 12, traced `io::` 64 (2 ignored);
`sqlite_carry` 3, `refusal_taxonomy` 5, `sqlite_wal_index` 4,
`s3_transfer_resume` 20 (1 ignored), `source_lock_trace` 13,
`prop_seed_guard` 8; `tests/test_ci_contract.py` 24 OK. Not run this
round: the root legs (`unshare -r`), TLC (the model is unchanged: a raw
base is outside the model's SQLite store), `props-deep`, `crash-sweep`.

## Round 3 (2026-10-09): operator rulings OI-1003-Q146 to Q148 (Linear TIN-4543)

Built on top of the staged carry, still **staged and uncommitted** (no
commit, no push, no PR, no Linear or GitHub comment). Rulings cited:
OI-1003-Q146, Q147, Q148 (TIN-4543), OI-1003-Q18 and Q100 to Q102 (WP0(d)),
R-N13. The design note's section 0.4 records the rulings and what was built.

- **Q146 (supersede).** D7 is gone: `plan_file` and the streamed path no
  longer short-circuit a snapshot (`Plan::Occupied` removed; `plan_file`'s
  `snapshot` parameter removed), so a changed store's snapshot takes
  WP0(d)'s path (`owned_output`, `prepare_supersede`, intent,
  `RENAME_EXCHANGE`, displaced check). A verified snapshot's staged file is
  marked (`StagedFile::mark_sqlite`); `StagedFile::exchange` looks for a
  `-wal`, `-journal` or `-shm` beside the leaf (`sqlite_sidecar_appeared`,
  helper `sqlite_sidecar_beside`) after the last identity look and the
  before-exchange test hook, immediately before `io::exchange`: present, it
  refuses `DESTINATION_OCCUPIED` with nothing exchanged and the old rows
  given back (`Exchanged::Undone{restore}`). `StagedFile::publish` (a free
  leaf) makes the same look. Not-owned or touched outputs refuse
  `DESTINATION_OCCUPIED` and are remembered, as files are. Counters
  `dest_sqlite_superseded`, `dest_sqlite_sidecar_refused`. Files:
  `materialize.rs`, `transfer.rs`, `counters.rs`, `main.rs` (USAGE).
- **Q147.** `--sqlite=refuse` stays the default; USAGE and design.md say the
  full neo→sting run passes `--sqlite=snapshot`.
- **Q148.** Transfer refusals are not closure-ledger rows (the disposition
  ledger holds planned items only; WP3 PR 4), so as ruled for that case:
  docs, the `SqliteIntegrityCheckFailed` doc text, and the refusal's report
  text: `report_transfer` prints `default-disposition <path>: abandon
  (SQLITE_INTEGRITY_CHECK_FAILED, OI-1003-Q148)` after the refusal line
  (`transfer::default_disposition`). No ledger integration.
- **Tests** (`transfer/tests/sqlite_carry.rs`): `q146_a_changed_store_
  supersedes_its_own_output_through_the_exchange` (WAL, commit only in the
  `-wal`; DELETE; then the unchanged rerun reuses with 0 bytes),
  `q146_a_touched_output_is_refused_and_kept_while_an_untouched_one_is_
  superseded` (mtime set; written by an app), `q146_a_sidecar_at_decide_
  or_before_the_exchange_refuses_and_keeps_the_old_output` (`-wal` and
  `-journal`, at Decide and from the before-exchange hook; then removed,
  converges by exchange), `q148_an_integrity_failure_defaults_to_abandon`.
  Flipped from D7: `p80_a_commit_after_the_backup_is_not_carried`,
  `p81_a_commit_only_in_the_wal_is_not_reused`,
  `p81_an_unchanged_store_is_reused_and_only_a_changed_one_is_read` (no
  longer moves the old output aside). Fault harness: module
  `tests/fault_harness/sqlite_supersede.rs`, eight rows in `scenarios!`
  (`materialize.after_temp_write`, `materialize.after_temp_seal`,
  `supersede.after_intent`, `supersede.after_exchange` at hits 1 and 2,
  `publish.destination.after_dir_seal`, `before_commit`, `after_commit`)
  over a closed WAL store and a DELETE store: at the crash each path holds
  the old or the new snapshot, whole (integrity_check, DELETE mode, rows
  old or new, no sidecar), every row describes its file; the rerun
  converges and a further run reads and receives 0. Skipped with a note as
  root (the session is refused there).
- **Failed first.** Scratch mutant "never supersede a snapshot" (a
  `Publication::Superseding` of a marked staged file refused
  `DESTINATION_OCCUPIED`, i.e. D7's outcome), run once and reverted
  (sha256 verified): 6 of 33 `sqlite_carry` unit tests failed (the three
  `q146_*` and the three flipped rows) and 8 of 8 new fault rows failed
  (their points never reached). Scratch mutant "no look before the
  exchange" (`if false && ...`): `q146_a_sidecar_at_decide_or_before_the_
  exchange_...` failed at `-wal late=true`. Reverted, sha256 verified.
- **Model.** `SqliteCarry.tla` moved to `docs/formal/` and folded into the
  Dhall catalogue as a third module (`catalogue/SqliteCarry.dhall`,
  `Types.dhall`'s `Module`, `Catalogue.dhall`), rendering `configs_sq.tsv`
  and 25 `MC_sq_*.cfg`; the hand-written `docs/formal/sqlite/` is removed.
  New: `Exchange`, `RenameAfterUnlink`, `DestTouch`, the intent and its
  sweep, constants `DestTouches` and `BudgetSeconds`, `WithinBudget`,
  invariants `SqliteNoClobber` and `SqliteOldOrNewWhole`,
  `SqliteNoForeignSidecar` now covering the exchange; mutations
  `sqlite_supersede_no_recheck`, `sqlite_supersede_unowned`,
  `sqlite_supersede_unlink_rename`; `Witness_Superseded`. D7's
  `SqliteNeverSupersede` and `sqlite_supersede` are removed. README
  section "SqliteCarry" with the results.

### Validation of round 3 (uid 1000, `nix develop`, load average 200 to 240)

- `cargo fmt --all -- --check`: clean.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`, the
  `io-trace` pass and the `fault-injection,io-trace` pass: clean.
- `cargo test -p bulkload-agent --lib`: 582 passed, 5 ignored. (The first
  full run had `p74_unrowed_outputs_are_adopted_without_source_reads` fail
  once at its first `corpus.run().unwrap()`, a file-seat test this round
  does not touch; it passed three times alone and in the full rerun.)
- `--test sqlite_carry` 3, `--test s3_transfer_resume` 20 (1 ignored),
  `--test refusal_taxonomy` 5; fault gate (`fault-injection,io-trace`):
  `fault_harness` 68 (60 before plus the 8 new rows), `power_loss` 12;
  `tests/test_ci_contract.py` 24 OK.
- `just tla-check` over every `MC_sq_*` row: catalogue current and grounded
  (SqliteCarry.tla: 41 operators, 16 constants, 15 mutations, 21 code
  symbols); budget self-test INCONCLUSIVE as required; 6 PASS, 3 REACHED,
  15 FAIL each on its named property; total wall 816 s. Results table in
  `docs/formal/README.md`, "SqliteCarry". The other two modules were not
  re-run (their catalogue rendering is unchanged).
- Not run: `just check-fast` as one recipe, `cargo test --workspace` as one
  run, the root legs (`unshare -r`), `props-deep`, `crash-sweep`.

## Round 4 (2026-10-09): review of the Q146 change (five findings)

Still **staged and uncommitted**. Rulings cited: OI-1003-Q146 to Q148
(TIN-4543), OI-1003-Q76, R-N122, R-N11, R-N13. Two fix sessions: the
first left its edits unstaged and stopped before validating (its note
numbered the findings 1 to 6); the second reviewed those edits against the
code, kept them, re-ran their mutations where they are lib-level, fixed
finding 3 (which the first had only documented as an operator question),
and validated. Both sessions' edits are now staged with the rest (`git
add -u`); nothing is committed.

- **1 MAJOR, the 8 Q146 fault rows skipped as root (accepted, fixed).**
  The eight `sqlite_supersede*` / `sqlite_superseding*` rows returned early
  as root and reported ok, and PR CI runs the `fault-harness` gate
  (`just fault-harness`, `cargo test ... --test fault_harness`) as root.
  Now, as root, each row re-runs itself, crash child included, in a copy
  of the test binary dropped to uid and gid 65534 (scratch `TMPDIR`
  chowned to it; the same drop `tests/sqlite_wal_index.rs`, on main since
  #157, makes in CI), and passes only when that child printed `test
  result: ok. 1 passed; 0 failed`; a root run that cannot drop (plain
  `unshare -r`) fails the row, never a silent pass (R-N122). The
  production root refusal (`SQLITE_SOURCE_AS_ROOT`, Q76) is untouched and
  still proved by `p82_*` and `tests/sqlite_carry.rs`'s root leg: no test
  override of it was added. `Scenario::run_named` (default: `run`) gives a
  row its test name; `fault_harness/sqlite_supersede.rs` `as_unprivileged`;
  `scenario` asserts it never runs as root. `tests/test_ci_contract.py`
  pins no fault-harness test counts (only the single-test recipes'
  `1 passed` lines), so there is no count to pin. Evidence: the old tree's
  rows (`git show :crates/bulkload-agent/tests/fault_harness/
  sqlite_supersede.rs`) `return` after an `eprintln!` as root; first
  session's scratch mutant "every snapshot exchange refused" run as
  namespace root (`unshare -r --map-auto`): 8 of 8 FAILED where the old
  code passed. This session re-ran the 8 rows as namespace root (below).
- **2 minor, no test of the sidecar look before a fresh rename (accepted,
  test added).** Test hook `materialize::set_before_rename` (same registry
  as `set_before_exchange`, cfg test / fault-injection / io-trace), run in
  `StagedFile::publish` after the seal and before the sidecar look. Test
  `transfer::tests::sqlite_carry::a_sidecar_before_a_fresh_rename_
  refuses_and_publishes_nothing` (`-wal`, `-journal`): `DESTINATION_
  OCCUPIED`, nothing at the leaf, no temporary, `dest_sqlite_sidecar_
  refused` 1, converges once removed. Mutation: `if false &&
  self.sqlite_sidecar_appeared()` in `publish` fails it (below).
- **3 minor, Q148 for backup-detected corruption (accepted, fixed).** A
  store whose corruption the backup step meets (`SQLITE_BACKUP_FAILED`,
  primary 11 `SQLITE_CORRUPT` or 26 `SQLITE_NOTADB`) never reaches
  `integrity_check` and printed no `abandon` default; the receiver could
  not tell it from a busy or I/O backup failure, because the wire carried
  the code string only. Fix: wire v6 `Control::Refused` gains
  `sqlite_code: Option<i32>` (`BulkloadRefusal::sqlite_code`, sent by both
  source refusal sites); the receiver refuses `PROTOCOL_STATE_VIOLATION`
  when one rides on any other code, and keeps it in
  `TransferStats::refusal_sqlite_codes`; `transfer::default_disposition(
  code, sqlite_code)` gives `abandon` to `SQLITE_INTEGRITY_CHECK_FAILED`
  and to `SQLITE_BACKUP_FAILED` with primary 11 or 26 (masked as R10
  masks); `main.rs` prints the same `default-disposition <path>: abandon
  (<code>, OI-1003-Q148)` line through `TransferStats::default_disposition`.
  The code stays `SQLITE_BACKUP_FAILED` (no taxonomy change); a hot
  journal's backup failure keeps no default. `wire_id` re-pinned
  `a6e9842a...0203a5d0` (v6 is unreleased, still a hard cut). Tests:
  `q148_an_integrity_failure_defaults_to_abandon` (11, 26, 267, 779 abandon;
  5, 10, 1032, None and other codes none), new `q148_the_backup_code_
  crosses_the_wire_and_only_with_its_refusal` (scripted peer), `p82_a_
  corrupt_store_...` now asserts `abandon` for every damage kind and every
  round (from the record too), `p80_a_hot_journal_...` asserts none; frame
  round-trip includes a `Refused` with a code. Failed first: mutant "old
  code-only rule" (`let _ = sqlite_code;` and integrity only): 3 FAILED
  (`q148_an_integrity...` left None, `q148_the_backup_code...` left None,
  `p82_a_corrupt_store...` "Header round 0: SQLITE_BACKUP_FAILED {..: 26}"
  left None); mutant "source sends `sqlite_code: None`": `p82_a_corrupt_
  store...` FAILED "Header round 0: SQLITE_BACKUP_FAILED {}". Both
  reverted, sha256 `9aa6fa1b...76b3239d` verified. Docs: design.md (wire,
  SQLite snapshot seats), design note 0.4 Q148 row and section 10 (wire),
  property-test plan P82. Not modelled: `SqliteCarry.tla` has no
  disposition, so `just tla-check` was not re-run.
- **4 minor, the crash sweep exchanged back on inode alone (accepted,
  fixed).** `settle_supersedes` exchanged a foreign displaced file back
  when the leaf held the staged inode, then unlinked the staged name: a
  write made to the new snapshot after the crash (an application
  committing after a reboot) would be deleted. It now exchanges back only
  when `published(found, &intent)` holds (inode, and the size and mtime
  the intent recorded before the exchange; ctime is not comparable, the
  exchange itself moves it); otherwise the displaced file is kept aside,
  reported in `Sweep::left`, intent kept. `StagedFile::exchange`'s
  in-session `in_place` uses the same predicate. The model already
  assumed this (`SwStaged` compares `id`, which `ForeignWrite` renews).
  Test `materialize::supersede_tests::a_displaced_file_is_not_put_back_
  over_writes_to_the_new_output` (grown; same size with only mtime moved).
  Mutation: the old predicate (`at_leaf.is_some_and(|found|
  is_staged(&found, &intent))`) fails it (below). Residual, stated in the
  module docs: a write that keeps size and restores mtime is not seen.
- **5 minor, the idle-connection residual misdescribed SQLite (accepted,
  docs).** Read in the bundled SQLite 3.46.0 (`libsqlite3-sys` 0.30.1):
  `pager_open_journal` calls `databaseIsUnmoved` before it opens a
  rollback journal; it asks `SQLITE_FCNTL_HAS_MOVED`, whose unix
  `fileHasMoved` `stat`s the path and compares the inode it opened, and a
  moved file fails the write `SQLITE_READONLY_DBMOVED`, no `-journal`
  created. `fileHasMoved` needs the `unixInodeInfo` that on Linux only the
  default posix-lock style keeps, so `unix-none`, `unix-dotlock`,
  `unix-flock` and custom VFSes have no check. The first session also
  checked it empirically with SQLite 3.51.2 (idle connection, and one in
  `BEGIN IMMEDIATE` that had not journaled: both `SQLITE_READONLY_DBMOVED`,
  no `-journal`, new file `integrity_check` ok). Residuals restated in
  design.md (SQLite snapshot seats, Supersede), the design note's section
  0.4 (and a correction under the D7 text) and `docs/formal/README.md`:
  (1) a `-journal` or `-wal` created in the `lstat` window; (2) SQLite
  before 3.8.3 or a VFS without the moved-file check.

Property-test plan rows P81 (root leg, the fresh-rename row) and P82
(Q148 for backup corruption) updated.

### Validation of round 4 (uid 1000, `nix develop`)

Load average 165 to 200. Results on the final tree (mutants restored and
sha256-verified before each run):

- `cargo fmt --all -- --check`: clean (exit 0).
- `cargo clippy --workspace --all-targets --locked -- -D warnings`, the
  `-p bulkload-agent --features io-trace` pass and the
  `--features bulkload-agent/fault-injection,bulkload-agent/io-trace` pass:
  clean. (The first default pass caught one `semicolon_if_nothing_returned`
  in `finish_session`'s `stats.refuse`; fixed, all three re-run.)
- `cargo test -p bulkload-agent --lib`: 585 passed, 0 failed, 5 ignored
  (round 3's 582 plus the supersede, fresh-rename and wire tests).
- `--test sqlite_carry` 3 passed; `--test s3_transfer_resume` 20 passed, 1
  ignored; `--test refusal_taxonomy` 5 passed; `bulkload-proto` 16 passed
  (`wire_id_is_pinned` at the new pin).
- Fault gate (`fault-injection,io-trace`, own target dir): `fault_harness`
  68 passed, 0 failed (as uid 1000); `power_loss` 12 passed.
- The 8 Q146 rows as root, the mode CI uses (`unshare -r --map-auto`,
  namespace root with uid 65534 mapped; real root not available here):
  `sqlite_supersed --test-threads=1 --nocapture`: 8 passed, 0 failed, each
  row's summary line ending `uid=65534` (they executed, not skipped).
  Plain `unshare -r` (cannot drop): the row FAILS, "as root this row runs
  as uid 65534, and its scratch directory cannot be given to that uid:
  Invalid argument", exit 101.
- `python3 tests/test_ci_contract.py`: 24 tests OK.
- Mutations (lib, each reverted, sha256 verified): finding 3 old rule, 3
  FAILED; finding 3 wire dropped, 1 FAILED; finding 4 old predicate
  `is_staged` only, `a_displaced_file_is_not_put_back_...` FAILED
  ("grown=true: the writes made to the new file are kept"); finding 2 `if
  false && self.sqlite_sidecar_appeared()`, `a_sidecar_before_a_fresh_
  rename_...` FAILED (`-wal`: refusals `[]`).
- `just tla-check`: not run; the model was not touched this round.
- Not run: `just check-fast` as one recipe, `props-deep`, `crash-sweep`,
  real-root CI (the PR is not opened).

## Consequences the operator should see

- ~~**D7 blocks incremental updates of changed stores.**~~ Replaced by
  OI-1003-Q146 (round 3): a changed store supersedes its own untouched
  output; a touched or foreign output, or a sidecar beside it, is refused
  `DESTINATION_OCCUPIED` and kept. A store failing `integrity_check` is
  reported with `default-disposition ...: abandon` (Q148).
- **D2 can starve busy stores.** A store committed to between most backup
  steps ends `BUDGET_EXCEEDED` (counted restarts, not remembered).
- A non-database whose destination path has a `-wal`, `-journal` or `-shm`
  beside it is refused in `snapshot` mode (a visible over-refusal).
- In `snapshot` mode a file without the `SQLite` magic beside a non-empty
  `-wal` is refused `SQLITE_STATE_CHANGED` (an encrypted store such as
  Signal's, while its app runs; a non-SQLite `foo` beside a `foo-wal` too,
  a visible over-refusal). Quit the app, or checkpoint, to carry it.
- Snapshot slots: 4 GiB alive at once, a capture thread waits up to 30 s
  for room, and no backup runs when its slot would take the source state's
  filesystem under `--min-free-percent` (25 % by default in the binary):
  both refuse `BUDGET_EXCEEDED`, not remembered. On a source disk fuller
  than the floor every snapshot is refused until the flag is lowered.
- As root (CI) SQLite re-owns the `-wal` it opens, so the unit legs that
  need a `-wal` to keep its identity across a read are skipped there with a
  note; they run as an ordinary user, and the CLI test re-runs as `nobody`.
  The eight Q146 fault rows re-run as `nobody` as root and fail if they
  cannot (round 4).

## Open

- Operator rulings: section 0.3 and every item of section 18 of the design.
  (Round 4 applied Q148's `abandon` to backup-detected corruption as well;
  the operator may narrow it.)
- (Done in round 3: the Dhall fold and the fault-harness rows.)
- Q148's closure-ledger integration once transfer refusals are ledger rows
  (WP3 PR 4). P77's lock-trace leg for
  `serve --sqlite=snapshot`; the S2 gated window (Q34); the neo→sting field
  confirmation and its evidence; scratch-mutant runs of the other named
  mutants.
- The distilled decision belongs on #218 and the Linear SSOT ledger.

Workstream line: `sqlite-carry | this lane (implementation) | bulkload
feat/sqlite-carry-20261008 @ bdb8994, staged, uncommitted | Q146-Q148
built and tested locally (round 3), review round 4 fixed and validated |
remaining rulings: design 0.3, 18 |
next: review, split into PRs, then the neo→sting field run with
--sqlite=snapshot`.
