# 2026-10-15 finish-line: S4 transfers, S5 HEAD and index drift, the S2 harness alongside a run

Lane `finish-line`, worktree `bulkload.worktrees/finish-line-20261015`,
branch `feat/finish-line-20261015`, cut from the convergence PR #224 head
`6869ba6`. Changes are staged, not committed: the coordinator commits
signed.

Rulings cited: OI-1003-Q1 (S4), OI-1003-Q2 and Q11 (S5, #38), OI-1003-Q5,
Q9 and Q34 (S2, #165), OI-1003-Q148 (abandon default for an integrity
failure), OI-1003-Q152 (no freeze window: the neo->sting run closes by
converging deltas while neo stays live), OI-1003-Q145 to Q147 (unchanged
here), OI-1003-Q155 (proof quality), R-N11, R-N104, R-N13.

## 1. WP3 PR 4: transfers are rows of the closure report (S4)

- `crates/bulkload-agent/src/transfer_outcome.rs` (new): one durable
  record per transfer, `DEST_STATE/transfer.closure.d/<session item>.closure`
  (`bulkload.transfer-closure.v1`, magic + postcard, strict decode; layout
  changed in the review round below): the S2 overlap decision first, then
  the record lock and `running` (only when DEST_STATE exists; never creates
  a state root), then its end: `Completed` with its refused paths
  (code, site, `SQLite` code) and covered sidecars, or `Refused`. A wire
  code that is no code token is recorded as `FRAME_CODEC` (untyped, never
  counts). Written with `estate::write_bytes` (now `pub(crate)`): extend,
  not fork.
- `closure.rs`: `Report::join_transfers` adds rows (`RowKind`
  `TransferSession`/`TransferEntry`) classified by the estate's own rule,
  factored out as `refusal_disposition`. New `Disposition::Covered`. A ruled
  default (`ruled_default`, OI-1003-Q148 via `transfer::default_disposition`)
  disposes an integrity failure when no review row does; a review wins.
  Attestations never close a transfer row. Schema `bulkload.closure.v3`
  (estate rows print as in v2; totals add `transfer_rows`, `covered`,
  `ruled_default`).
- `disposition.rs`: `record_joined` lets `closure-dispose --transfer`
  review a transfer row the report joined; every other item must still be
  planned.
- `main.rs`: `copy` and `pull` run under `record_session` (source and
  destination canonical when they resolve, so a rerun spelled another way
  names the same rows; `pull_recorded` wraps the unchanged `pull_command`,
  whose ssh child the P76 registry pins byte for byte);
  `closure-report` and `closure-dispose` take `--transfer DEST_STATE`
  (repeatable, before PLAN) and `--transfer-manifest MANIFEST`;
  `transfer-expect` writes the manifest. USAGE updated.
- Tests: P86 `closure/transfer_tests.rs` (property, 256 fixed-seed cases,
  and four pinned rows), three `transfer_outcome` unit tests, CLI row
  `a_copy_session_is_rows_of_the_closure_report_and_its_gate`.
- Not joined: the offline provider verbs (`snapshot`, `compose`, ...),
  which belong to no run; a source's engine temporaries stay listed only.

## 2. #38: HEAD and index drift is custody (S5 under Q152)

- `git_carry.rs`: `DriftKind` gains (appended, so recorded sidecars keep
  their encoding) `HeadMoved` (`HEAD`), `IndexRewritten` (`index`),
  `NestedWorktreeMoved` (path). `export_pass`'s end check is
  `pass_drift`: HEAD, the symbolic HEAD, the index and nested worktrees
  moving under the export are drift rows (the bundle carries the
  snapshot's HEAD and index and is marked drifted: no restore or apply lays
  it down); the exclude file, the stash reflog, configuration, the
  frontier, gitlinks, foreign nests, omitted roots and bareness still
  refuse. `KeyParts::drift_only` no longer compares HEAD, index or nested
  worktrees; `drift_to` and `drift_across` name them as rows
  (`head_index_drift`, `nested_worktree_drift`); `carries_fixed` is the
  part that still refuses. The estate's poisoned key keeps a raced capture
  from ever being a reuse hit, so the next pass carries the move.
- Tests changed (they asserted the refusal): `head_moving_...`,
  `index_rewritten_...` and `nested_worktree_head_moving_during_capture_still_refuses`
  are now `..._is_drift_custody`; `r3_detached_head`, `r3_symbolic_head`
  and `r3_index` round trips are drift custody (never a Hit; the next pass
  carries A); exclude, config and stash round trips still refuse.
- End to end, P87 `estate/drift_custody_tests.rs`: pass 1 raced by a commit
  and a `git add` (drift custody; apply refuses `CAPTURE_DRIFTED`, nothing
  laid down), pass 2 lands, the agent commits, switches and stages, pass 3
  raced by a `git switch` (custody, landing untouched), pass 4 converges:
  seats, index entries, HEAD and its branch and local branches equal the
  source; closure 0 unaccounted, gate green.
- Model: `EstateConverge.tla` `Capture(i)` gains the raced case (one
  action, so earlier rows' coverage is unchanged), `CapOf`, `RacedMoves`,
  `Begin` not enabled on a drifted capture, `Witness_DriftCarried`,
  mutation `drift_key_reused`. Catalogue rows `MC_ec_capture_raced`
  (LiveSpec pass), `MC_ec_reach_drift_carried`, `MC_ec_neg_drift_key_reused`
  (fails `ConvergesWhenQuiet`). Formal README section added.

## 3. #165: the S2 budget alongside a real run

- `s2_budget.py observe`: the reference workload and load1 sampler for as
  long as the run takes, starting nothing; rows stream to `rows.jsonl`
  (flushed per row, fsynced every 30 s), power every 60 s; ends on
  `--stop-file` or `--max-seconds`. It reads the host's load (sysctl
  vm.loadavg on Darwin) and never lists, signals or asks about an agent
  (R-N11, R-N104). `mark FILE on|off` appends run-interval marks.
  `alongside OBS --intervals FILE` makes the intervals ON windows and the
  gaps OFF, drops windows under `--min-window-seconds`, runs `analyze`
  unchanged (its ON-run checks skipped in this mode), and writes
  `trace.json`, `summary.md` and `evidence.tsv`. An ON interval still open
  at the end is INCONCLUSIVE. `--evidence` uses `alongside_problems`: every
  kept window at least 300 s, at least five, pinned gate, plus R-N81's power
  and OFF-baseline checks The review round below adds the
  alternation rule, the settle lead, streamed missed ticks, the end record
  and the clock check.
- Tests: `AlongsideTests` (10, one a live 3.5 s observe that starts nothing
  and is read back by `alongside`).

## Docs

`docs/design.md` (Completion: transfer rows and v3; Drift: #38),
`docs/slo.md` amendment 2026-10-15 (later), property plan rows P86 and P87,
`docs/formal/README.md`, whitepaper status lines (branch, not `main`).

## Validation, first round (2026-10-15, sting, inside `nix develop`, lane target dirs)

Host shared, load average about 50 on 32 CPUs. Final pass, after the last
code edit:

| step | result |
|---|---|
| `cargo fmt --all -- --check` | exit 0 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | exit 0 |
| `cargo clippy -p bulkload-agent --all-targets --locked --features io-trace -- -D warnings` | exit 0 |
| `cargo clippy --workspace --all-targets --locked --features bulkload-agent/fault-injection,bulkload-agent/io-trace -- -D warnings` | exit 0 |
| `cargo test -p bulkload-agent --locked --no-fail-fast` | exit 0: lib 662 passed, 5 ignored; every integration binary ok, among them `estate_converge` 21, `refusal_taxonomy` 5, `space_closure_cli` 7, `source_command_registry` 16, `prop_seed_guard` 8, `source_lock_trace` 13 |
| `cargo test ... --features fault-injection,io-trace --test fault_harness` | 91 passed |
| `cargo test ... --features fault-injection,io-trace --test power_loss` | 12 passed |
| `python3 tests/test_ci_contract.py` | 24 tests OK |
| `python3 -m unittest crates/bulkload-bench/scripts/test_s2_budget.py` | 83 tests OK; `ruff check` and `ruff format --check` clean on both files |
| `just tla-render --check` | all 214 committed files equal the catalogue's rendering |
| `just tla-check` every `MC_ec_` row (87 + self-test) | exit 0: 34 PASS, 48 FAIL, 4 REACHED, 1 SIMULATION, self-test INCONCLUSIVE; 2632 s; peak RSS 2501 MiB; the 33 pass rows the README tabulates explore exactly their recorded distinct-state counts |

The first pass failed one test, `source_command_registry`
`every_child_goes_through_the_source_safe_builder`: wrapping the pull
session had split the registered ssh child's text. Fixed by restoring
`pull_command` verbatim (it returns the stats) and wrapping it from
`pull_recorded`; no registry entry changed.

## Mutation evidence (scratch copy under /dev/shm, file restored byte-identical after each)

| Mutant | Result, red tests |
|---|---|
| S4-M1 no ruled default | RED: `a_transfer_session_joins_the_gate_row_by_row`, `p86_*` |
| S4-M2 a `running` session counts as applied | RED: `a_transfer_that_never_ended_or_left_nothing_is_unaccounted`, `an_attestation_never_closes_a_transfer_row`, `p86_*` |
| S4-M3 the record drops its entries | RED: `a_transfer_session_joins_the_gate_row_by_row`, `a_transfer_review_is_bound_to_its_refusal_instance`, two `transfer_outcome` tests |
| S4-M4 an attestation closes a transfer row | RED: `an_attestation_never_closes_a_transfer_row` |
| S4-M5 `copy` writes no record | RED: `a_copy_session_is_rows_of_the_closure_report_and_its_gate` |
| S5-M6 HEAD/index under the export refuse (the code before #38) | RED: `head_moving_...`, `index_rewritten_..._is_drift_custody`, `p87_*` |
| S5-M7 key parts refuse HEAD/index (before #38) | RED: `p87_*` |
| S5-M8 a nested worktree's HEAD refuses (before #38) | RED: `nested_worktree_head_moving_..._is_drift_custody` |
| S5-M9 a drifted capture's key recorded unpoisoned | **survived**: the `.drift` sidecar is a second guard (the decision core's `drifted` input), so no Rust row isolates the key alone; the model's `drift_key_reused` abstracts both guards as one |
| S2-M10 short windows kept | RED: `test_windows_alternate_and_short_ones_are_dropped` |
| S2-M11 alongside window length not checked | RED: `test_short_windows_are_not_evidence` |
| S2-M12 alongside run checked for ON commands | RED: `test_a_real_runs_intervals_give_the_budget_verdict`, `test_evidence_rows_carry_every_gated_metric_and_window` |
| S2-M13 an open interval accepted | RED: `test_an_interval_still_open_is_inconclusive_and_not_evidence` |

On the base `6869ba6` the old behaviour is what M6 to M8 restore: the
three `*_still_refuses` tests asserted it.

## Review round (2026-10-15, later): 11 findings, all verified real

Each fix has a test that was red on the code before it (failing-first run
on the pre-fix tree, or a mutant restoring the pre-fix code), unless said.

| # | finding | verdict | fix | red-first evidence |
|---|---|---|---|---|
| 1 | transfer record written before copy/pull's S2 overlap checks | real (reproduced: `src/.dstate/transfer.closure` left in the source) | `transfer::copy_roots_apart` / `receive_roots_apart` (factored out of `copy_with`), run by `record_session` before any write; an overlap refusal writes nothing, not even a Refused record | CLI `a_copy_refused_for_overlapping_roots_writes_nothing_inside_them` red on the pre-fix tree (the 329-byte record in the source tree); unit `an_overlap_refusal_writes_nothing`; mutant R1 |
| 2 | one `transfer.closure` per DEST_STATE: sharing transfers and concurrent sessions erase rows | real | one record per transfer, `transfer.closure.d/<session item>.closure`; `RecordLock` (`<item>.lock`, non-blocking flock) held for the session, so a second session of the same transfer refuses before it writes; a session that does not complete (`running`, `Refused`) keeps the replaced record's entry rows; an unreadable record is never overwritten (`CLOSURE_UNACCOUNTED`) | CLI `transfers_sharing_a_dest_state_keep_each_others_rows` red on the pre-fix tree (1 transfer row, x lost); units `a_session_that_does_not_complete_keeps_the_rows_it_replaces`, `transfers_sharing_a_dest_state_are_each_their_rows`; mutants R2, R3, R4, R9 |
| 3 | nothing binds the transfer set to the plan; stale or aliased DEST_STATEs pass | real | `transfer_outcome::Manifest` (`bulkload.transfer-manifest.v1`, plan digest + `since` + one line per transfer), `transfer-expect` writes it, `--transfer-manifest` joins exactly its transfers (`no-transfer-record`, `transfer-record-stale`, `transfer-record-mismatch` unaccounted), `RECEIPT_BINDING_INVALID` for another plan; DEST_STATE canonicalized before dedup; any transfer row joined twice refuses `FIELD_DOMAIN_VIOLATION`; records carry `started`; `record_session` refuses `CLOSURE_UNACCOUNTED` when DEST_STATE exists but took no closing record | CLI `a_dest_state_named_twice_through_an_alias_is_refused` red on the pre-fix tree (joined twice, applied 2); units `a_manifest_binds_the_runs_transfers_to_its_plan`, `a_dest_state_that_takes_no_closing_record_refuses`, `a_manifest_round_trips_strictly`; mutants R5, R6, R7, R8 |
| 4 | nested worktree HEAD away and back across the unguarded windows is a clean key | real | `Export::nested_before` / `nested_after` (the export's own censuses); `drift_across` compares the pre-key with the first and the second with the post-key, as for HEAD and index | `r3_nested_worktree_round_trip_across_unguarded_windows_is_drift_custody` red on the pre-fix tree (whole-capture Hit on the stale bundle); mutant R10. Note: `NestedWorktree` records the HEAD commit only, so a switch between two branches at one commit is no custody change (the test's agent-y is another commit of the same tree) |
| 5 | model blocks Begin on every drifted capture, the code applies key-drift captures | real (model and docs overstated) | chose to model both classes, not change the code: `CapOf(s, drift, inband)`; `Capture` raced classes `export` (in-band, never laid down), `after` and `before` (key drift, coherent, applies); `Begin` guarded on `inband` only; `Witness_KeyDriftLanded` + `MC_ec_reach_key_drift_landed` show the applied class is explored; slo.md, design.md and README wording corrected (key drift applies and the closure counts it applied, as it would a clean pass the source moved past a moment later) | model change, not a code fix: `MC_ec_reach_key_drift_landed` REACHED, `MC_ec_capture_raced` PASS with both classes |
| 6 | stopped observe loses coordinated-omission rows and reads as complete | real | missed ticks written through `RowSink` as the loop skips them (`MissedRows`); SIGTERM/SIGHUP handlers (received only, main thread, restored after) end the observation like the stop file; a terminal `{"k":"end"}` row; `alongside` sets `cut_short` (INCONCLUSIVE) when the header has no `end_reason`/`span_s` or rows have no end row | `test_an_observation_with_no_recorded_end_is_cut_short`, `test_observe_streams_missed_ticks_and_ends_its_record_on_request` red before; `test_missed_ticks_are_backfilled_into_their_windows` kills mutant a |
| 7 | alongside evidence accepts one ON window / non-alternating pattern | real (reproduced OFF,OFF,ON,OFF,OFF as evidence) | `alongside_problems`: kept pattern, adjacent same-state windows merged, must be OFF-first-and-last with at least 2 ON (so at least 3 OFF); the 5-window rule kept | `test_one_on_window_among_five_is_not_evidence` red before; `test_three_windows_are_not_evidence_by_the_five_window_rule` kills mutant b |
| 8 | pinned 60 s settle drops the start of every pass | real | `derive_windows(..., lead=settle)`: each ON window starts one settle before its pass, taken from the gap before it (`lead_s` recorded); a pass with less than its whole settle observed before it is not evidence (`short_leads`) | `test_the_start_of_every_pass_is_measured` red before (PASS at +0 %; now +127.6 %, not PASS); `test_a_pass_with_no_room_for_its_settle_is_not_evidence` |
| 9 | one wall-clock origin plus monotonic offsets breaks across sleep / clock steps | real | every sampler row carries `e` (time.time()); `alongside` adds an instrument error (INCONCLUSIVE) when any sampler row's wall and monotonic clocks part by more than `CLOCK_SLACK_S` = 2 s, or rows carry no wall clock. Detection, not remapping: an observation that slept is not evidence | `test_a_wall_clock_jump_is_an_instrument_error` red before |
| 10 | 9 of 11 alongside mutants survive | real | tests added per mutant (power changing inside a window, a bad intervals line, a torn row, an open interval among 5+ kept windows, mixed window lengths under `--min-window-seconds` 100, missed rows, periodic power sampling) | mutation table below: 21 of 21 killed |
| 11 | "a drifted capture is never laid down" is not a checked property | real | `NoDriftedLanding` (no in-band drifted capture in flight, journaled, or in the landing ledger) as a safety invariant of every EstateConverge pass row; mutation `drift_applied` (Begin guard removed) with `MC_ec_neg_drift_applied`, which must violate it | `MC_ec_neg_drift_applied` FAIL NoDriftedLanding (37 states, depth 4); catalogue `NoDriftedLanding` trace row |

### Mutation evidence, review round (scratch copies; sources restored byte-identical)

Rust (`/dev/shm` copy of the tree, `--lib` transfer/closure/r3 rows plus
`space_closure_cli`):

| mutant | result, red tests |
|---|---|
| R1 `roots_apart` after the first write | KILLED: `a_copy_refused_for_overlapping_roots_writes_nothing_inside_them`, `an_overlap_refusal_writes_nothing` |
| R2 one record file per DEST_STATE | KILLED: 9 tests, among them both sharing tests and `p86_*` |
| R3 a refused session drops earlier rows | KILLED: `a_session_that_does_not_complete_keeps_the_rows_it_replaces` |
| R4 no record lock | KILLED: same |
| R5 DEST_STATE not canonicalized | first SURVIVED (the duplicate-row check subsumed it for states with records); killed after adding the empty-state alias case: `a_transfer_that_never_ended_or_left_nothing_is_unaccounted` |
| R6 a transfer row may count twice | KILLED: `a_manifest_binds_...`, `a_transfer_that_never_ended_...` |
| R7 stale records accepted | KILLED: `a_manifest_binds_the_runs_transfers_to_its_plan` |
| R8 no closing record passes | KILLED: `a_dest_state_that_takes_no_closing_record_refuses` |
| R9 unreadable earlier record overwritten | KILLED: `a_session_is_recorded_running_then_ended_and_never_creates_its_state` |
| R10 nested worktrees not compared with the export (the pre-fix code) | KILLED: `r3_nested_worktree_round_trip_across_unguarded_windows_is_drift_custody` |

S2 (`s2_budget.py`, full 96-test suite per mutant; the unmutated copy
passes):

| mutant | result |
|---|---|
| a missed rows ignored | KILLED `test_missed_ticks_are_backfilled_into_their_windows` |
| b 5-window rule removed | KILLED `test_three_windows_are_not_evidence_by_the_five_window_rule` |
| c open-interval problem removed | KILLED `test_an_open_interval_is_named_among_five_kept_windows` |
| d power_at always the first sample | KILLED `test_power_that_changes_inside_a_window_is_not_evidence` |
| e power_end from the window start | KILLED same |
| f periodic power sampling removed | KILLED `test_observe_streams_missed_ticks_and_ends_its_record_on_request` |
| g window_seconds max, not min | KILLED `test_the_shortest_kept_window_decides_the_window_rule` |
| h bad interval lines not errors | KILLED `test_a_bad_intervals_line_is_inconclusive` |
| i torn row not an error | KILLED `test_a_torn_row_is_inconclusive` |
| j origin shifted 30 s | KILLED (3 tests) |
| k intervals not merged | KILLED `test_intervals_read_epochs_iso_and_marks_and_merge` |
| l missed ticks not written | KILLED `test_observe_streams_...` |
| m no end-signal handlers | KILLED same |
| n no end row | KILLED same |
| o header end check removed | first SURVIVED (the end-row check masked it); killed after the test kept the end row: `test_an_observation_with_no_recorded_end_is_cut_short` |
| p end-row check removed | KILLED same |
| q clock check removed | KILLED `test_a_wall_clock_jump_is_an_instrument_error` |
| r missing wall clock accepted | KILLED same |
| s no settle lead | KILLED `test_the_start_of_every_pass_is_measured` (and 2 more) |
| t alternation rule removed | KILLED `test_one_on_window_among_five_is_not_evidence` |
| u short-lead problem removed | KILLED `test_a_pass_with_no_room_for_its_settle_is_not_evidence` |

The earlier table's S5-M9 (key recorded unpoisoned survives in Rust, the
`.drift` sidecar is a second guard) is unchanged.

### Validation, review round (2026-10-15, sting, inside `nix develop`, lane target dir)

Host shared, load average 34 to 53 on 32 CPUs. Full pass after the last
code edit (one rustfmt-only reflow of a test line followed; `cargo fmt
--check` and clippy re-run green after it):

| step | result |
|---|---|
| `cargo fmt --all -- --check` | exit 0 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | exit 0 |
| `cargo clippy -p bulkload-agent --all-targets --locked --features io-trace -- -D warnings` | exit 0 |
| `cargo clippy --workspace --all-targets --locked --features bulkload-agent/fault-injection,bulkload-agent/io-trace -- -D warnings` | exit 0 |
| `cargo test -p bulkload-agent --locked --no-fail-fast` | exit 0: lib 669 passed, 5 ignored; every integration binary ok, among them `space_closure_cli` 10, `estate_converge` 21, `source_command_registry` 16, `refusal_taxonomy` 5, `prop_seed_guard` 8, `source_lock_trace` 13 |
| `cargo test ... --features fault-injection,io-trace --test fault_harness` | 91 passed |
| `cargo test ... --features fault-injection,io-trace --test power_loss` | 12 passed |
| `python3 tests/test_ci_contract.py` | 24 tests OK |
| `python3 -m unittest crates/bulkload-bench/scripts/test_s2_budget.py` | 96 tests OK (re-run after the last test edit); `ruff check` and `ruff format --check` clean |
| `just tla-render --check` | all 216 committed files equal the catalogue's rendering |
| `just tla-check` every `MC_ec_` row (89 + self-test) | exit 0: 34 PASS, 49 FAIL, 5 REACHED, 1 SIMULATION, self-test INCONCLUSIVE; 3133 s; peak RSS 2370 MiB; every pass row but `MC_ec_capture_raced` explores its README-recorded distinct-state count; `MC_ec_capture_raced` 135185 (two drift classes) |

## Open

- Operator: does an alongside observation count as a gated S2 run, and
  does R-N81's load bound apply to a live neo under Q152? As built a busy
  neo stays not evidence.
- Operator: should the closure gate require a transfer manifest (as built
  it is optional; without it the report covers only the transfers named,
  and its JSON has no `transfer_manifest` block)? Review finding 3's
  case (a) is closed only when the manifest is used.
- `NestedWorktree` custody is the nested HEAD commit only: a switch
  between two branches at the same commit is not a custody change.
- An observation whose host slept is INCONCLUSIVE, not remapped
  (finding 9 detection only).
- Operator: should the stash reflog join HEAD and index as drift custody?
- A staged gitlink change under a pass still refuses (nested custody).
- No Linear or GitHub comment posted by this lane; the coordinator owns
  them.

## Q160 and Q161 (2026-10-15, lane `q160-q161`)

Worktree `bulkload.worktrees/q160-q161-20261015`, branch
`feat/q160-q161-20261015` off `feat/finish-line-20261015` (PR #225).
Changes are staged, not committed: the coordinator commits signed.
Rulings: OI-1003-Q160 and OI-1003-Q161 (Linear TIN-4543, 2026-10-15), under
OI-1003-Q152; R-N81 (amended for alongside observations only), R-N13.
The two operator questions in "Open" above (alongside evidence under
R-N81; the stash reflog as drift) are answered by these rulings.

### Q160: S2 alongside a live run, judged against the live baseline

- `s2_budget.py`: in `analyze`, R-N81's OFF-level cap (2.5) applies to
  `run` evidence only; alongside, power is still checked at every window
  boundary. New `live_baseline` (the OFF windows' load1 levels, min, max,
  spread; per ON window its nearest OFF window before and after, equally
  weighted: `baseline_load1`, `drift_load1`, `d_load1`, `d_p95` per metric),
  `pair_latency`, `pair_disagreements` (a pooled PASS or FAIL of the load1
  or latency gate stands only when every pair gives it; a pass with no OFF
  window on one side is not judged) and `live_baseline_problems` (evidence
  needs recorded load1 in every window, a known spread, every pair
  compared). The verdict carries `live_baseline` and `evidence_label`
  (`alongside, live baseline (OI-1003-Q160)`, or `gated run (R-N81)`);
  `summary.md` gains a live-baseline table, `evidence.tsv` a `baseline`
  row and one `pair` row per pass, and its label column carries the
  alongside label. Budget, window rules and the noise floor unchanged.
- Tests: `AlongsideLiveBaselineTests` (8). Red-first: the 8 tests and the
  amended `test_evidence_rows_carry_every_gated_metric_and_window`, run
  against the pre-change `s2_budget.py` (HEAD's, in a `/dev/shm` copy),
  all fail (busy baseline INCONCLUSIVE by R-N81; falling-baseline and
  latency-drift cases PASS; rising-baseline case FAIL; no-OFF-after case
  PASS; evidence with a load gap labelled evidence; missing
  `live_baseline`/label keys).

### Q161: a stash push, pop or drop during a pass is drift custody

- `git_carry.rs`: `DriftKind::StashMoved` (appended: recorded sidecars keep
  their encoding), named `refs/stash`; `stash_drift`; the stash reflog
  leaves `fixed_equal`, `drift_only` and `carries_fixed` and joins
  `pass_drift`, `drift_to`, `drift_across` (both unguarded windows) and
  `carries`. The bundle carries the snapshot's stash (commits and
  `stash-order-v1`); in-band moves are marked (never laid down); key moves
  poison the key (never reused, away and back included). `main.rs` USAGE.
- Tests: git_carry `stash_pushed_during_capture_is_drift_custody` (store
  under the pass: `RefAdded`+`StashMoved`, snapshot stash carried,
  `CAPTURE_DRIFTED`, key parts `drift_to` names it, next pass carries the
  stash, strict export still refuses) and
  `stash_dropped_during_capture_is_drift_custody` (drop below the tip:
  `StashMoved` alone); `r3_stash_reflog_round_trip_..._is_drift_custody`
  (was `..._refuses`); P87 stash row
  `p87_an_agent_stashing_through_every_pass_is_carried_by_the_next_converging_delta`
  (push, pop and drop each under a pass: custody, nothing laid down; each
  next quiet pass converges seats, index, HEAD and the stash in the
  source's order on sting; closure 0 unaccounted, gate green).
  Red-first: with only the variant added (old behaviour), all four fail
  (`GitAuthorityChanged` / `refused GIT_AUTHORITY_CHANGED`).
- Model: `EstateConverge.tla` `RacedMoves` gains stash push/drop under the
  new `SrcKinds` label `stashrace` (no protocol step changes; rows without
  `race`/`stashrace` unchanged by construction). Catalogue: `SrcKind.stashrace`,
  `racedStash`/`racedStashBack`, rows `MC_ec_capture_raced_stash` (pass,
  82086 states), `MC_ec_reach_stash_drift_carried` (REACHED),
  `MC_ec_neg_stash_drift_key_reused` (FAIL ConvergesWhenQuiet) and
  `MC_ec_neg_stash_drift_applied` (FAIL NoDriftedLanding), both via
  `Against.also` on #38's mutations; `stash_drift` grounded on
  NoDriftedLanding.

### Mutation evidence (scratch copies under /dev/shm; sources untouched)

| mutant | result |
|---|---|
| S2-a OFF cap dropped in run mode too | KILLED `test_a_gated_run_keeps_r_n81s_load_cap`, `test_a_busy_off_baseline_is_inconclusive` |
| S2-b OFF cap kept alongside (pre-Q160) | KILLED `test_a_busy_live_baseline_is_alongside_evidence` |
| S2-c no pair guard | KILLED falling, rising, latency-drift and no-OFF-after tests |
| S2-d load bracket = OFF before only | KILLED falling, rising, spread/pairs tests |
| S2-e pooled load FAIL not checked by pairs | KILLED `test_a_rising_baseline_does_not_fail_as_bulkload_cost` |
| S2-f latency pairs not checked | KILLED `test_a_latency_baseline_drift_is_seen_pair_by_pair` |
| S2-g no live-baseline evidence problems | KILLED `test_evidence_needs_load_recorded_and_every_pair_compared` |
| S2-h run label on alongside evidence | KILLED busy-baseline and evidence-rows tests |
| S2-i p95 bracket = OFF before only | KILLED latency-drift test |
| S2-j a pass with no comparison not a disagreement | KILLED `test_a_pass_with_no_off_window_after_it_is_not_judged` |
| S2-k spread unreported | KILLED 4 tests |
| S2-l power unchecked alongside | KILLED busy-baseline, `test_battery_is_not_evidence`, power-in-window test |
| Q1 stash compared as fixed authority again (pre-Q161) | KILLED all four stash rows |
| Q2 `pass_drift` names no stash row | KILLED both git_carry stash tests |
| Q3 `drift_across` names no stash | KILLED `r3_stash_reflog_round_trip_...` |
| Q4 `drift_to` names no stash | KILLED `stash_pushed_during_capture_is_drift_custody` |
| Q5 key-drift stash refuses (`carries_fixed`) | KILLED P87 stash row, `r3_stash_...` |

Unmutated copy: the 49 filtered lib rows pass; S2 copies restored per mutant.

### Validation (2026-10-15, sting, inside `nix develop`, lane target dir `cargo-target/q160`)

Host shared, load average about 19 on 32 CPUs. Final pass after the last
code edit (docs edited after it):

| step | result |
|---|---|
| `cargo fmt --all -- --check` | exit 0 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | exit 0 |
| `cargo clippy -p bulkload-agent --all-targets --locked --features io-trace -- -D warnings` | exit 0 |
| `cargo clippy --workspace --all-targets --locked --features bulkload-agent/fault-injection,bulkload-agent/io-trace -- -D warnings` | exit 0 |
| `cargo test -p bulkload-agent --locked --no-fail-fast` | exit 0: lib 672 passed, 5 ignored; every integration binary ok, among them `estate_converge` 21, `space_closure_cli` 10, `source_command_registry` 16, `refusal_taxonomy` 5 |
| `cargo test ... --features fault-injection,io-trace --test fault_harness` | 91 passed |
| `cargo test ... --features fault-injection,io-trace --test power_loss` | 12 passed |
| `python3 tests/test_ci_contract.py` | 24 tests OK |
| `python3 -m unittest crates/bulkload-bench/scripts/test_s2_budget.py` | 104 tests OK; `ruff check` and `ruff format --check` clean |
| `just tla-render --check` | all 220 committed files equal the catalogue's rendering |
| `just tla-check` the 9 raced rows | exit 0: 2 PASS, 3 REACHED, 4 FAIL as expected, self-test INCONCLUSIVE; 334 s; peak 2004 MiB; `MC_ec_capture_raced` 135185 states (unchanged) |

### Open (Q160/Q161)

- No alongside observation has been run on neo; the verdict logic is
  proven on synthetic traces only.
- The pair rule is conservative in both directions: a real cost that one
  pass does not show, or a real PASS across a baseline step, reads
  INCONCLUSIVE, not PASS or FAIL.
- AGENTS.md's R-N81 lane bullet now carries the alongside amendment
  (review round below, finding 7).
- The model's stash race is on the main only and a pop is modelled as a
  drop (the seats it restores are the Rust rows').
- No Linear or GitHub comment posted by this lane; the coordinator owns them.

### Review round (2026-10-15, lane `q160-q161`, same worktree, staged)

Nine reviewer findings against the staged Q160/Q161 change. Rulings:
OI-1003-Q160, OI-1003-Q161 (TIN-4543), R-N11, R-N104, R-N13.

| # | finding | outcome |
|---|---|---|
| 1 | pair veto crossed gates; FAIL reasons replaced | fixed: `judge_pairs` judges gate by gate. A pooled PASS needs every pair to pass both gates; a pooled FAIL stands when every pair fails a gate that failed pooled, and a pair also over the other gate adds an "also reads" reason; a downgrade keeps the pooled reasons |
| 2 | short passes dropped, run still alongside evidence | fixed: under `--evidence`, any pass dropped as a short ON window is an evidence problem, and `min_window_seconds` is pinned at 300; the verdict's `dropped_on` (windows, pass seconds) goes to the emit line, `summary.md` and an `evidence.tsv` `dropped` row |
| 3 | post-pass recovery tail in every OFF baseline | fixed: `recovery_after_passes` pools the OFF windows that start at or after the first pass and compares the first settled half with the second (p95 by 30 s blocks; load1 lag-corrected per half). Over half the budget on a gated metric or on load1 turns a PASS INCONCLUSIVE. Evidence also needs no recorded pass ending within settle + 300 s before the origin; pre-origin intervals are kept in `config.pre_origin_intervals`. Reported in `summary.md` and as an `evidence.tsv` `recovery` row |
| 4 | ON/OFF placement unchecked (passes in quiet spells) | partly accepted: the doc claim is softened everywhere to "removes linear drift only", and the limitation is stated. Rejected: per-process CPU via `ps`, because the instrument never lists processes (R-N11, R-N104; module docstring). Rejected: receipts instead of marks, because in E5 the intervals are accurate and the confound is the host's own timing. Open for the operator |
| 5 | "cancels a linear drift" false with unequal OFF windows | fixed: the two OFF levels and p95s are interpolated to the ON window's settled midpoint (`settled_mid`) |
| 6 | three surviving pair mutants; reason named step | fixed: the pair reason names the gated metric furthest over (ties: step); tests for an each-op-only overrun, a missing op p95, and a pooled latency FAIL contradicted by one pass |
| 7 | AGENTS.md R-N81 bullet had no alongside exemption | fixed: the bullet now says a standalone `run` keeps load1 < 2.5 and an alongside observation is judged against its live OFF baseline (OI-1003-Q160) |
| 8 | popping or dropping the last stash between the ref read and the reflog read refused GIT_CHILD_FAILED | fixed: `source_stash_reflog`. When the reflog read fails as `GitChildFailed` and `rev-parse --verify -q refs/stash` exits 1, the read is no stash, so the move is `StashMoved` (pass re-read), `RefRemoved` (snapshot) or a torn key. Other failures still refuse. New test stage `mid_pass::Stage::StashReflog` |
| 9 | stash messages: commit subject, not reflog message; message-only change unseen | real, predates Q161 (#215 native refs), **not fixed**: the fix makes the message part of the stash identity in the key, in `stash-order` (v2), in the destination's `LandedStash` ledger and `stash_decision`, and in the ratified stash table. That is an operator decision. `docs/design.md` no longer claims reflog messages are carried |

Tests (red first: the S2 tests against the index's pre-round
`s2_budget.py` in a `/dev/shm` copy; the Rust test before the fix):
`LiveBaselineReviewTests` (12; 11 red, `test_a_pair_missing_one_operation_is_not_evidence`
kills a mutant only), amended `test_the_baseline_spread_and_each_adjacent_pair_are_reported`
(pass 3's baseline 1.741, interpolated; red at 1.75), and
`stash_popped_between_ref_read_and_reflog_read_is_drift_custody` (red:
`GitChildFailed(Other)`).

Mutants (scratch copies under `/dev/shm/q160-tmp`; sources restored):
all 14 S2 mutants KILLED (both-gate ignoring ops; complete-on-step-only;
FAIL never checked by pairs; no "also reads"; downgrade dropping pooled
reasons; equal weights; no recovery check; recovery at the full budget;
recovery load unchecked; dropped pass not a problem; window rule
unpinned; pre-origin unchecked; reading always step; PASS needing
latency pairs only). Rust: an empty stash on any failed reflog read is
KILLED (the out-of-space assertion). Not killable with the available
injection: an empty stash on every `GitChildFailed` (the scratch fault
classifies as `SpaceExhausted`).

Reviewer repros after the fix: E1/E6 FAIL with the breach reason and an
"also reads" reason; E2 not evidence (4 passes, 800 s dropped); E3
INCONCLUSIVE (recovery); E4 pair errors 0.005 to 0.008 (were 0.013 to
0.058); E5 unchanged (finding 4, open); E7b names `git +32.0%`.

Open (review round):
- Finding 4: load correlated with the passes themselves is not separable
  without listing processes, which R-N104 rules out for this instrument.
  The way out is protocol, for example pass start times independent of
  the host's load. Operator decision.
- Finding 9: stash reflog messages as stash identity. Operator decision
  (a key invalidation and a ledger format change).
- The recovery check and the dropped-pass rule are alongside only;
  gated `run` is unchanged.
- A converge run whose passes are under about 240 s cannot be alongside
  evidence. That follows from the ruled 300 s window.

Validation (review round, sting, inside `nix develop`, target dir
`cargo-target/q160`, after the last code edit; docs edited after it):

| step | result |
|---|---|
| `cargo fmt --all -- --check` | exit 0 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | exit 0 |
| `cargo clippy -p bulkload-agent --all-targets --locked --features io-trace -- -D warnings` | exit 0 |
| `cargo clippy --workspace --all-targets --locked --features bulkload-agent/fault-injection,bulkload-agent/io-trace -- -D warnings` | exit 0 |
| `cargo test -p bulkload-agent --locked --no-fail-fast` | exit 0: lib 673 passed, 5 ignored; every integration binary ok (`estate_converge` 21, `space_closure_cli` 10, `source_command_registry` 16, `refusal_taxonomy` 5) |
| `cargo test ... --features fault-injection,io-trace --test fault_harness` | 91 passed |
| `cargo test ... --features fault-injection,io-trace --test power_loss` | 12 passed |
| `python3 tests/test_ci_contract.py` | 24 tests OK |
| `python3 -m unittest crates/bulkload-bench/scripts/test_s2_budget.py` | 116 tests OK; `ruff check` and `ruff format --check` clean |
| `just tla-render --check` | exit 0 |
| `just tla-check` the 9 raced rows | exit 0: 2 PASS, 3 REACHED, 4 FAIL as expected, self-test INCONCLUSIVE; 484 s; `MC_ec_capture_raced` 135185 and `MC_ec_capture_raced_stash` 82086 states (unchanged; no model change this round) |
