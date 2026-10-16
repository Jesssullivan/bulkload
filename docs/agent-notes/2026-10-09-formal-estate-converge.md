# 2026-10-09 formal: EstateConverge.tla (estate convergence, OI-1003-Q144)

Lane: formal-converge-20261009 (worktree
`bulkload.worktrees/formal-converge-20261009`, branch
`feat/formal-estate-converge-20261009` off `origin/main` `91d4294`).
Rulings cited: OI-1003-Q144 (Linear TIN-4543, 2026-10-09: convergence may
update a checkout bulkload landed only if it is unchanged locally since
landing, by content, not stat; otherwise a typed refusal; local branches
and tags deleted at the source kept and reported; remote-tracking refs
follow the source; configuration report-only; merge of the convergence
waits on this model passing), OI-1003-Q32 and Q43 (the catalogue and
tla-check pipeline), OI-1003-Q7 (TLA+/TLC is the checker of record), R-N11
(only processes this lane started; none signalled), R-N13 (this note).

Nothing is committed or pushed; the coordinator stages, signs and opens
the PR.

## What was done

- `docs/formal/EstateConverge.tla` (new): destination custody of the
  convergence on `feat/estate-converge-20261008` (read-only; design note
  sections 2 to 19 there). Header: scope, abstractions, code map; every
  action cites the function it models.
- `docs/formal/catalogue/EstateConverge.dhall` (new): the module's typed
  catalogue (constants, rows, verdicts, traceability, closed unions);
  `Catalogue.dhall` renders it beside the other two modules;
  `Types.dhall` gains `Module.EstateConverge` (index 2, `configs_ec.tsv`,
  definition grounding) and lane `Converge` for pending symbols.
- `docs/formal/configs_ec.tsv`, `docs/formal/MC_ec_*.cfg`: rendered by
  `just tla-render` (63 rows after review 3: 24 added).
- `docs/formal/README.md`: an "EstateConverge" section (properties,
  mutations, results, findings, limits) and the file-table rows.
- The justfile is unchanged: tla-check and tla-render read the module list
  from the catalogue.

## Results

First pass (before review 3): all 39 rows matched (`MC_ec_refs_op` 329,305
states, the largest).

After review 3 (final, on the spec as left in the worktree):
`cd <worktree> && export TMPDIR=/dev/shm/formal-tmp && nice -n 10 nix
develop .#default --command just tla-check <rows>`, two concurrent
invocations (20 existing rows; 42 new and negative rows), each preceded
by its budget self-test: **all 63 rows matched** (19 pass, 5 of them
LiveSpec with both temporal properties; 3 reach; 11 finding rows and the
unfair negative FAIL their named property; 28 mutations FAIL theirs;
self-test INCONCLUSIVE). Total wall 3,033 s and 2,590 s, peak RSS
2,010 MiB, host load about 200. `just tla-render --check`: all 163 files
equal the catalogue. Per row: `docs/formal/README.md`, "Results
(EstateConverge)". A first full run on an intermediate spec had one
mismatch (`MC_ec_neg_dir_replace_unchecked` passed once INTERRUPTED was
justified by any live edit); justifying it only by edits since the run or
intent restored it, and the whole set was re-run.

## Review 3 (2026-10-09, later): fourteen findings against the model

Rulings cited: OI-1003-Q144 (Linear TIN-4543), OI-1003-Q32 and Q43 (the
catalogue and tla-check pipeline), OI-1003-Q7, R-N11 (only TLC processes
this lane started; none signalled), R-N13 (this note). The convergence
code on `feat/estate-converge-20261008` was re-read at `d6a8b86` plus its
staged files during this pass; every cited site was still as the reviewers
described (no fix had landed). Nothing committed or pushed.

Convention adopted (stated in the module header and README): where the
reviewers named the rule the code must follow, the model now holds that
rule and the code's current behaviour is a mutation marked CODE TODAY in
the catalogue, whose `MC_ec_neg_` row must fail; the code fix makes that
rule the code's. Where the fix is open, a finding row fails on the code as
it is.

### Per finding

| # | Verdict | What changed in the model | Rows |
|---|---|---|---|
| 1 o/x -> o/x/y wedge | CONFIRMED (`native_refs.rs:759-764`; the model's RefFinish copied it) | `RefFinish`: the names after the step are native minus the deletes that will run, plus the creates that will; a blocked create is dropped (`ref-name-conflict`), its row restored | `MC_ec_refs_rename_op` (LiveSpec) pass; `MC_ec_neg_finish_after_all_deletes` FAIL `NoWedge` (the reviewer's scenario: crash after the ref intent, operator moves `o/x`, endless INTERRUPTED) |
| 2 rebase/bisect branch moved | CONFIRMED (`checked_out_by` reads only `branch` lines; a rebasing worktree is `detached`; Git's own `branch -f` refuses via is_worktree_being_rebased) | `dhead[i].held`; `CheckedBy` adds the held branch; operator kind `rebase` (in `OpHead`) detaches HEAD and takes custody of the branch's value | `MC_ec_rebase` pass; `MC_ec_neg_rebase_unguarded` FAIL `NoLocalWorkLost` |
| 3 ledger re-reads index | CONFIRMED (`converge.rs:1466-1467`, `349-350`) | `NextLedger` records `caps[c].index` (the digest publish_index installed); written seats' prints are recorded at write (`pc.ws`), as `Engine.written` does | `MC_ec_index_concurrent` (MaxSrc 2) pass; `MC_ec_neg_ledger_reads_index` FAIL `NoLocalWorkLost`. Exclude digest: not modelled (fix item) |
| 4 unborn HEAD untyped | CONFIRMED (`converge.rs:254`, `text(rev-parse --verify HEAD)`) | `OpHead` may switch to an unborn branch; `OpRef` may delete a checked-out branch; an unborn HEAD reads with oid 0 (proof MODIFIED, settle INTERRUPTED) | `MC_ec_head_op` pass; `MC_ec_neg_unborn_head_untyped` FAIL `TypedRefusal` |
| 5 seat parent not a dir after intent | CONFIRMED (`Engine::run` `converge.rs:1026-1037`; `safe_destination` `git_carry.rs:4605`) | operator kind `dir` (in `OpSeat`): `rm -r d` or `d` replaced by a file; every orphan seat step after the intent refuses INTERRUPTED; new refusal label `UNTYPED` | `MC_ec_seat_parent` (LiveSpec, with restore) pass; `MC_ec_neg_seat_parent_untyped` FAIL `TypedRefusal` |
| 6 TypedRefusal too loose | CONFIRMED (model) | `Interrupted(i, j)`: each INTERRUPTED site names its justification (an operator edit of a component that step writes, made since the item's run began or its intent was written and still live, or a Git lock on one: `RefWhy`, `SeatWhy`, `IndexWhy`, `HeadWhy`); the global `opSince` becomes the per-component `edSince`, and `childCrashed` is removed (locks justify instead). The "since" part keeps `MC_ec_neg_dir_replace_unchecked` failing (an untracked file older than the run must refuse MODIFIED before writes, not INTERRUPTED after). It exposed a new defect path (finding row `MC_ec_finding_settle_branch_conflict`) and required finding 14's ghost fixes | all rows re-run |
| 7 Git renames not durable | PLAUSIBLE (no directory sync after `update_refs` in the code: confirmed; Git not fsyncing the directory under `core.fsync=reference`: Git's documented behaviour, not reproduced here) | variable `dur`; `Crash` may be a power loss that reverts non-durable Git writes; rule = sync after every bulkload Git write (`GitSync`) | `MC_ec_neg_refs_unsynced` FAIL `TypedRefusal`; under the rule every crash row covers it |
| 8 stash lock, first-landing txn | CONFIRMED (`stash_apply` returns `Ok(false)` on any failed store; `finish` clears `ledger.stash`) | `ChildCrashTxn` inside a stash store leaves `refs/stash.lock`; `RefStash` fails on it; operator stash blocked by it; `FirstPlanWt` now goes through `native_refs::commit` (ref intent, r_txd...r_led) | `MC_ec_finding_stash_lock` FAIL `TypedRefusal` (code as is) |
| 9 probe files left | CONFIRMED (`converge.rs:1306-1316`, before the intent) | `probe` counter, action `ProbeExchange`, Begin's sweep under the lock, a `CrashAtomicity` conjunct | `MC_ec_neg_probe_left` FAIL `CrashAtomicity` |
| 10 settle moves checked-out branch (blocker) | CONFIRMED (no `checked_out_by` in `finish`; X_head3 reproduced as the neg row) | `RefFinish` never redoes an op on a branch now in `Checked` (row restored, `checked-out-elsewhere`); `TxnOK` has symref-verify (no op on a branch checked out since the plan) | `MC_ec_head_op`, `MC_ec_head_concurrent` pass; `MC_ec_neg_settle_moves_checked_out` (crash, then `git switch foo`) and `MC_ec_neg_txn_no_symref_verify` (switch between plan and update-ref) FAIL `NoLocalWorkLost` |
| 11 no row tests the CAS | CONFIRMED (catalogue gap) | mutations `txn_no_cas`, `head_no_cas` | `MC_ec_refs_concurrent`, `MC_ec_head_concurrent` pass; both negs FAIL `NoLocalWorkLost` |
| 12 no liveness after edits + restore | CONFIRMED (Z_refop reproduced) | `OpRestore` only between applies | `MC_ec_finding_restored_ref` FAIL `ConvergesWhenQuiet` (code as is; needs a fix or a ruling); `MC_ec_live_ops` (seat, head, restore) and `MC_ec_seat_parent` (dir, restore) pass under LiveSpec. Stash put back is not live-checked: earlier finding 4 breaks safety there |
| 13 no positive ChildCrash row | CONFIRMED | `childCrashed` removed (finding 6) | `MC_ec_child_crash` (every safety invariant, ChildCrash, no HEAD switch, no stash) pass; `MC_ec_finding_set_head_partial` FAIL `CrashAtomicity` |
| 14 ghost: revert to old is no edit | CONFIRMED (X_head2 reproduced) | `LandedHead`/`LandedSeat`: during the item's own pass, only the value bulkload holds at that point (old before its write, new after); the HEAD-moving branch is old or new only until HEAD is on it; an operator write equal to what `WriteLanded` lands is absorbed (Q144 content); a commit on the operator's checked-out branch moves its HEAD ghost. Refs and the stash keep old-or-new (stated limit) | `MC_ec_head_concurrent` (X_head2's superset) pass |

New defect found while re-running (not in the review): **a settle replays a
HEAD-branch create that a put-back ref blocks**
(`MC_ec_finding_settle_branch_conflict`, `TypedRefusal`): source renames
`foo` to `foo/bar` and switches to it; the operator deletes `foo`; the
converge plans HEAD onto a new `foo/bar`; crash before `set_head`; the
operator puts `foo` back; the settle's `create foo/bar` fails beside `foo`
on every rerun. Same class as finding 2 (settle replays a stale branch
line). `MC_ec_refs_op` drops source switches so it stays a pass row.

## Verdict for OI-1003-Q144's merge gate (review 3; superseded by "Verdict (updated)" below)

The model passes every row at its bounds, but it now holds rules the code
does not yet follow, and eleven finding rows fail on the code as it is. By
Q144's gate **the convergence should not merge** until the CODE TODAY
mutations below are fixed in the code (items 8 to 13 and 15 to 17 of the
list) and the finding rows are fixed or ruled acceptable (items 1 to 7, 14,
18, 19). The blocker is item 16 (a settle moves a branch the operator
checked out after a crash: `NoLocalWorkLost`).

| Invariant | Current code (feat/estate-converge-20261008 at d6a8b86 + staged) |
|---|---|
| NoLocalWorkLost | Fails: settle moves a checked-out branch (16), no symref-verify (17), rebase/bisect branch moved (9), ledger re-reads the index (10), stash settle undoes a drop (4). Holds for operator bytes in seats (proof by content, exchange and displaced checks, index CAS, ref and HEAD CAS: pinned by `txn_no_cas`, `head_no_cas`). |
| NoSourceWorkLost | Holds (provenance kept; local refs deleted at the source kept). |
| TypedRefusal | Fails: unborn HEAD (11), seat parent not a directory (12), unsynced ref renames (13), stash lock (14), settle replays a blocked HEAD-branch create (19), file -> directory settle (1). |
| NoReRead | Holds. |
| CrashAtomicity | Fails: probe files left (15), set_head partial commit (3), stale HEAD-branch CAS (2), #216 residual (7). |
| ConvergesWhenQuiet | Fails: restored ref never re-planned (18), old HEAD branch (5), detached re-attach (6). |
| NoWedge | Fails: finish's names-after (8), stale lock (3), file -> directory (1), #216 (7). |

## Model-vs-implementation discrepancies (final, after review 3; every item's status is in "Fixes and the model against the fixed code" below)

These are the convergence lane's fix items. "Mutation" items: the model
holds the rule and its `MC_ec_neg_<mutation>` row is the code today; the
fix is done when the code matches the rule (the neg row then stays a
negative). "Finding" items: the row fails on the code as it is and turns
into a pass row when the code and the model's action change together.

Earlier report (still open):

1. **File -> directory wedges after a crash** (finding row
   `MC_ec_finding_file_to_dir_settle`, `NoWedge`): settle's
   `Engine::remove` (`converge.rs:1090-1100`) on a path that already holds
   the new directory.
2. **Settle replays a stale HEAD-branch compare-and-swap**
   (`MC_ec_finding_settle_stale_branch`, `CrashAtomicity`; by review 3's
   justification it should also fail `TypedRefusal`, but a 1,800 s
   simulation probe did not reach it, so that is not shown): `settle`
   (`converge.rs:1879`) replays `intent.branch`; `Place::refs` does not
   skip the branch a pending intent names.
3. **No recovery from a crash inside a Git child**
   (`MC_ec_finding_child_crash` `NoWedge`, `MC_ec_finding_set_head_partial`
   `CrashAtomicity`): stale `<ref>.lock`; `set_head` commits HEAD without
   its branch line.
4. **Stash settle undoes an operator's drop or clear**
   (`MC_ec_finding_stash_settle_revert`, `NoLocalWorkLost`).
5. **Old HEAD branch stays stale while the source is quiet**
   (`MC_ec_finding_old_head_branch`, `ConvergesWhenQuiet`).
6. **Detached HEAD never re-attaches on a no-op apply**
   (`MC_ec_finding_detached_reattach`, `ConvergesWhenQuiet`).
7. **#216 residual** (`MC_ec_finding_first_linked_crash`, `NoWedge`).

Review 3:

8. **`finish`'s names-after-the-step** (`native_refs.rs:759-764`; mutation
   `finish_after_all_deletes`, `NoWedge`): remove only the deletes whose
   native value is still `op.old` (and not checked out), add the creates
   that will run; drop a create that set blocks (`ref-name-conflict`, row
   restored).
9. **Skip set misses rebase/bisect** (`checked_out_by`,
   `converge.rs:291-307`, used at 1510-1514, `native_refs.rs:401`,
   `plan_linked`, `head_plan`'s others; mutation `rebase_unguarded`,
   `NoLocalWorkLost`): add each worktree's `rebase-merge/head-name`,
   `rebase-apply/head-name` and `BISECT_START` branch.
10. **Ledger re-reads index (and exclude)** (`next_ledger`
    `converge.rs:1466-1467`, `record_landing` 349-350; mutation
    `ledger_reads_index`, `NoLocalWorkLost`): record the digest
    `publish_index` installed and the exclude digest bulkload wrote. The
    exclude half and the linked first landing are not modelled; fix them
    the same way.
11. **Unborn HEAD refuses `GIT_CHILD_FAILED`** (`head_of`,
    `converge.rs:254`; mutation `unborn_head_untyped`, `TypedRefusal`):
    `rev-parse -q --verify`, exit 1 = unborn; prove -> MODIFIED, settle ->
    INTERRUPTED.
12. **`Seat::at` errors after the intent** (`Engine::run`,
    `converge.rs:1026-1037`; mutation `seat_parent_untyped`,
    `TypedRefusal`): map to `interrupted()` in both modes (keep the settle
    skip for Remove/RmDir/DirMode).
13. **Git ref renames not durable before the ledgers** (`update_refs`,
    `set_head`, stash stores; mutation `refs_unsynced`, `TypedRefusal`):
    `sync_dir` the touched ref directories, the git dir (HEAD,
    packed-refs) and `logs/refs` before any ledger write. Verdict
    PLAUSIBLE (Git's directory fsync behaviour taken from its docs, not
    reproduced).
14. **Killed stash store** (`stash_apply` `native_refs.rs:617-645`,
    `finish`; finding row `MC_ec_finding_stash_lock`, `TypedRefusal`):
    remove bulkload's own stale `refs/stash.lock` under the destination
    lock, or refuse typed instead of clearing `ledger.stash`. Same for
    `plan_linked`'s `commit` (now modelled through the ref intent).
15. **Probe files left by a crash** (`probe_exchange`,
    `converge.rs:1306-1316`; mutation `probe_left`, `CrashAtomicity`):
    sweep `.bulkload-converge-*-probe-*` at the start of
    `apply_workspace` under the lock; create probes `O_EXCL|O_NOFOLLOW`
    (not modelled).
16. **Settle moves a branch checked out since the crash** (blocker;
    `finish` `native_refs.rs:754-800`; mutation `settle_moves_checked_out`,
    `NoLocalWorkLost`): compute `checked_out_by` in `finish`; an op on a
    checked-out branch is not redone, its row restored,
    `checked-out-elsewhere`.
17. **No `symref-verify` in the ref transaction** (`transact`; mutation
    `txn_no_symref_verify`, `NoLocalWorkLost`): add `symref-verify HEAD`
    lines for every worktree (git 2.46+), or rule the race a limit.
18. **Restored ref never re-planned on an A4 no-op** (finding row
    `MC_ec_finding_restored_ref`, `ConvergesWhenQuiet`): metadata check on
    the no-op path, or a ruling that it is a known limit.
19. **Settle replays a HEAD-branch create a put-back ref blocks** (new;
    finding row `MC_ec_finding_settle_branch_conflict`, `TypedRefusal`):
    fix with item 2 (the settle re-plans the branch line).
20. **Already right, now pinned**: `transact`'s and `set_head`'s
    compare-and-swap (mutations `txn_no_cas`, `head_no_cas`; positive rows
    `MC_ec_refs_concurrent`, `MC_ec_head_concurrent`).

Accepted readings and limits (not defects): a settle re-adds a seat the
operator deleted while a type change was pending; an operator write equal
to what bulkload then lands is landed content (Q144); not modelled: the
timestamp tick inside one seat operation, refs-only items, foreign
repositories, `--adopt`, symbolic refs, modes, exclude, configuration, A8,
the in-flight ghost for refs and the stash (old-or-new kept), and the
operator's own Git writes' durability.

Merge note: the convergence branch also edits `docs/formal/README.md` (a
pending-symbols row); this branch's README section supersedes it, so a
textual conflict there takes this branch's text plus that row if still
wanted.

## Open

- The findings above are against the code on
  `feat/estate-converge-20261008` as read on 2026-10-09; another agent is
  changing it. Each finding row turns into a pass row (rename to the
  `MC_ec_` positive form, as GitCarry's `MC_gc_base_missing_untyped` did)
  when the code fixes it and the model's action is changed to match.
- The convergence's symbols are pending in the catalogue (lane
  `Converge`); when that branch merges, move them to `codeSymbol`.
- Freezing EstateConverge's names needs a ruling.
- Operator decisions the fix list needs: item 17 (add `symref-verify`, or
  rule the plan-to-update-ref race a limit), item 18 (re-plan a restored
  ref on an A4 no-op, or rule it a limit), item 14 (remove bulkload's own
  stale `refs/stash.lock`, or refuse typed).
- When a CODE TODAY item is fixed, keep its `MC_ec_neg_` row (it pins the
  rule); when a finding item is fixed, change the model's action to the
  fix and rename its row to a positive one.
- Linear TIN-4543: record this model's verdict for OI-1003-Q144's merge
  gate (the coordinator posts it).

## Fixes and the model against the fixed code (2026-10-09, later)

Lane: the convergence lane, worktree
`bulkload.worktrees/estate-converge-20261008`, branch
`feat/estate-converge-20261008` at `5b8e550` (the convergence code merged
with `origin/main` `91d4294` and with this model's branch), with every
fix below staged. Rulings cited: OI-1003-Q144 (merge only when the model
passes against the code), OI-1003-Q32 and Q43, OI-1003-Q7, R-N11 (only
processes this lane started; its own stale TLC and cargo runs were stopped
through the harness by task id, none signalled by PID), R-N13 (this
section and the design note's section 20). Nothing committed or pushed.

The code side (per finding: change, Rust row, evidence) is the design
note's section 20. This section is the model side.

### Per finding (the list above, "Model-vs-implementation discrepancies")

| Item | Status | Model change | Rows |
|---|---|---|---|
| 1 file -> dir settle | Fixed | `SeatOp` Remove, settling: a seat holding the new directory is done; mutation `settle_remove_dir` | `MC_ec_file_to_dir_settle` (LiveSpec) pass; `MC_ec_neg_settle_remove_dir` fail `NoWedge` |
| 2 stale settle branch | Fixed | `IntentBr(i)` in every step's skip set (and `FirstStepWt`); mutation `step_moves_intent_branch` | `MC_ec_settle_stale_branch` (simulate, 20,000 seeded traces) clean; `MC_ec_neg_step_moves_intent_branch` (simulate) fail `CrashAtomicity` |
| 3 child crash, set_head partial | Fixed | settle removes the locks of its intent (`Begin` for the ref intent, `Decide_` for HEAD and the branch); `set_head` two transactions (`pc.sub`, `TwoTxns`, `ChildCrashHead` per transaction); mutations `stale_lock_kept`, `set_head_one_txn` | `MC_ec_child_crash_live` (LiveSpec), `MC_ec_set_head_partial` pass; `MC_ec_neg_stale_lock_kept` fail `NoWedge`, `MC_ec_neg_set_head_one_txn` fail `CrashAtomicity` |
| 4 stash settle revert | Fixed | durable progress `refInt.sp`; continue only from it or `StashNext` of it; on an operator change the row names the progress; the stash ghost follows finding 14's rule (`LandedStash`); mutation `stash_settle_any_suffix` | `MC_ec_stash_settle_revert` pass; `MC_ec_neg_stash_settle_any_suffix` fail `NoLocalWorkLost` |
| 5 old HEAD branch | Fixed | a second ref step for the branch HEAD left (`w_refs2`, `SecondStep`), deferred, never refused, when its transaction fails (`LeftBranchDeferred`); mutation `old_head_branch_skipped` | `MC_ec_old_head_branch` (LiveSpec) pass; `MC_ec_neg_old_head_branch_skipped` fail `ConvergesWhenQuiet` |
| 6 detached re-attach | **Documented limit** | none (A4 reads nothing) | `MC_ec_finding_detached_reattach` fails `ConvergesWhenQuiet`, as the limit |
| 7 #216 residual | Fixed | landing intent (`wsInt` with `from` 0) in `FirstAddWt`; `Decide_` finishes it forward; `CrashAtomicity` allows a worktree its landing intent names; mutation `no_landing_intent` | `MC_ec_first_linked_crash` (LiveSpec) pass; `MC_ec_neg_no_landing_intent` fail `NoWedge` |
| 8 finish names-after | Fixed in code (the model's rule since review 3) | none | `MC_ec_neg_finish_after_all_deletes` fail `NoWedge` |
| 9 rebase/bisect | Fixed in code | none | `MC_ec_neg_rebase_unguarded` |
| 10 ledger index | Fixed in code | none | `MC_ec_neg_ledger_reads_index` |
| 11 unborn HEAD | Fixed in code | none | `MC_ec_neg_unborn_head_untyped` |
| 12 seat parent | Fixed in code | none | `MC_ec_neg_seat_parent_untyped` |
| 13 refs unsynced | Fixed in code | none | `MC_ec_neg_refs_unsynced` |
| 14 stash lock | Fixed | `RefStash` with `refs/stash.lock` refuses typed (the settle removed bulkload's own); the `also` row of `stale_lock_kept` | `MC_ec_stash_lock` pass; `MC_ec_neg_stale_stash_lock` fail `TypedRefusal` |
| 15 probe files | Fixed in code | none | `MC_ec_neg_probe_left` |
| 16 settle moves checked-out (blocker) | Fixed in code | none | `MC_ec_neg_settle_moves_checked_out` |
| 17 symref-verify | Fixed in code (Git's own `HEAD.lock` of every worktree: `update-ref` refuses `worktrees/<id>/HEAD`) | `TxnOK` comment | `MC_ec_neg_txn_no_symref_verify` |
| 18 restored ref | **Documented limit** | none | `MC_ec_finding_restored_ref` fails `ConvergesWhenQuiet`, as the limit |
| 19 settle branch conflict | Fixed | settle re-plans HEAD when its branch line can no longer apply (`ReplanDue`, `BrLineFails` over the other worktrees), the intent rewritten; mutation `settle_replays_branch` | `MC_ec_settle_branch_conflict` pass; `MC_ec_neg_settle_replays_branch` fail `TypedRefusal` |
| 20 CAS pinned | Already right | none | `MC_ec_neg_txn_no_cas`, `MC_ec_neg_head_no_cas` |
| new: HEAD moved again after set_head | Fixed (found by a probe of `MC_ec_head_concurrent` with a crash) | `wsInt.hdone` (`HeadDone`), `LandedHead` and `CrashAtomicity` read it; a HEAD phase finding HEAD already new is done; mutation `head_not_recorded` | `MC_ec_head_concurrent_crash` pass; `MC_ec_neg_head_not_recorded` fail `NoLocalWorkLost` |

Other model changes: `head_branch_in_step`'s verdict is now `TypedRefusal`
(the re-plan removed its wedge; the mutation still causes an unjustified
refusal), and the mutation skips neither the HEAD branch nor its own
intent's. The catalogue grounds every symbol it cites as a Rust
definition on this branch (none pending); the `simulate` expectation was
added to the module's `Expect` union for the stale-branch pass row.

### Results against the fixed code

`cd <worktree> && export TMPDIR=/dev/shm/converge-tmp && nice -n 10 nix
develop .#default --command just tla-check <rows>`, two concurrent
invocations (29 pass and simulate rows; 44 reach, finding and negative
rows), each after its budget self-test, on the final spec (sha256
`bdf29103dfa931baad57172299156bcf37f925cf13cdbc362e7e1577162997c6`):
**all 74 rows matched** (28 pass, 9 of them LiveSpec; the seeded
simulation clean, 2,400,001 states checked in 215 s; 3 reach; the two
documented-limit finding rows and the unfair negative fail
`ConvergesWhenQuiet`; 38 mutation rows fail their property; self-test
INCONCLUSIVE). Total wall 2,058 s and 1,160 s, peak RSS 2,486 MiB, host
load 60 to 100. Largest: `MC_ec_refs_op` 300,997 distinct (136 s),
`MC_ec_live_linked` 223,978 (514 s). `just tla-render --check`: all 174
files equal the catalogue. Per row: README, "Results (EstateConverge)".

Earlier runs on intermediate specs found four interactions between the
fixes, each fixed in code and model before the final run (design note
section 20, "Found while re-checking"): the re-plan's stutter on the
item's own branch (`MC_ec_child_crash_live`), the cleared stash row
(`MC_ec_stash_settle_revert`), the left-branch step's refusal
(`MC_ec_head_concurrent`), and HEAD moved again after `set_head` (a probe
of `MC_ec_head_concurrent` with a crash, now `MC_ec_head_concurrent_crash`
and `MC_ec_neg_head_not_recorded`). `MC_ec_neg_head_branch_in_step`
passed once the re-plan removed its wedge; its verdict is now
`TypedRefusal`. One run lost TLC to a Nix store GC mid-run (ABORTED, `tlc:
No such file`); the tools are now pinned by GC roots under the lane's
cache directory (`tla-gcroot*`).

### Verdict for OI-1003-Q144's merge gate (updated; superseded by "after review 4" below)

Every finding the model reported against the code is fixed in the code,
and the model passes every row at its bounds against the fixed code,
except the two documented limits (items 6 and 18: liveness only, the next
capture converges both). By Q144's gate the convergence may merge once
the operator rules items 6 and 18 known limits (or rules a metadata-only
A4 check to fix them).

| Invariant | Fixed code (staged on `feat/estate-converge-20261008`) |
|---|---|
| NoLocalWorkLost | Holds at every bound |
| NoSourceWorkLost | Holds |
| TypedRefusal | Holds |
| NoReRead | Holds |
| CrashAtomicity | Holds |
| ConvergesWhenQuiet | Holds, except the two documented limits |
| NoWedge | Holds |

Open: the operator's ruling on items 6 and 18; Linear TIN-4543 (the
coordinator posts this verdict); freezing EstateConverge's names needs a
ruling.

## Review 4 (2026-10-09, later): thirteen findings against the fixed code and model

Lane: the convergence lane, worktree
`bulkload.worktrees/estate-converge-20261008`, branch
`feat/estate-converge-20261008` at `5b8e550` with the fixes above staged.
Rulings cited: OI-1003-Q144 (the convergence merges only when the model
passes against the code), OI-1003-Q32 and Q43, OI-1003-Q7, R-N11 (only
processes this lane started: TLC, cargo and the fixture repositories' Git
children, never signalled; the one live `update-ref --stdin` child a test
starts is ended by closing its input and reaped), R-N98 (fixture
repositories only), R-N13 (this section and the design note's section
21). Nothing committed or pushed; everything staged.

The reviewers' reproductions (`adv_*`, `strace-stash.log`) were not in the
worktree; each finding was re-derived from the code and reproduced by a
new Rust row that fails on the code as review 4 found it (the staged tree,
exported with `git checkout-index` before any edit, plus the new test
files), or by a mutant that puts that code back (design note section 21).

| # | Finding | Verdict | Code | Model | Rows |
|---|---|---|---|---|---|
| 1, 8 | Settle (and a fresh `set_head`) attaches HEAD to a branch another worktree checked out (blocker, two reports of one defect) | CONFIRMED (`branch_line_fails` returned at `now == after` before `others`; `set_head`'s HEAD-only path checked nothing; `drop(held)` before the HEAD transaction). Model had the same gap (reviewer's scratch `OneCheckout`, reproduced) | `set_head` holds every other worktree's `HEAD.lock` from before the branch transaction to after HEAD's when it attaches HEAD to a branch it is not on, and refuses `GIT_CONVERGE_INTERRUPTED` when another worktree has it checked out or held; the settle checks `others` first and re-plans also with no branch line (`taken`), keeping a done branch line's row | invariant `OneCheckout`; `HeadTxnOK` attach check (`Attaches`, `OthersChecked`); `BrLineFails` checks others first; `TargetTaken` in `ReplanDue`; the re-plan keeps a done branch's row; `OpHead` switches any worktree; mutation `head_attach_unchecked` | `MC_ec_one_checkout`, `MC_ec_one_checkout_crash` pass; `MC_ec_neg_head_attach_unchecked` fails `OneCheckout` |
| 2 | `recover_linked_landing` discards the operator's index and overwrites a prefix | CONFIRMED (unconditional `read-tree`; `blob.starts_with(&bytes)`) | index must have no entries or be `installed_index`'s, else `GIT_DESTINATION_MODIFIED`; every check before any write; prefix rule dropped: a linked landing lays each entry under `.bulkload-landing-temporary` and renames it without replacing (`restore_entries_with`, `LANDING_TEMPORARY`), so a differing file at an entry's name is the operator's; the recovery removes the temporary; its own killed `read-tree`'s `index.lock` (digest = the installed index) is removed, anyone else's refuses INTERRUPTED | component `wtw` (the linked worktree's content), operator kind `wtwork`; `Decide_`'s recovery requires `wtw` none or landed; mutation `recovery_overwrites` | `MC_ec_first_linked_crash_op` pass; `MC_ec_neg_recovery_overwrites` fails `NoLocalWorkLost` |
| 3 | Stash reflog never fsynced | CONFIRMED (reproduced: `strace` of `git update-ref --create-reflog` under `core.fsync=reference,index,objects`, git 2.52: `fsync` of `refs/stash.lock`, an `O_APPEND` open of `logs/refs/stash`, no fsync of it; `logs/r4/strace-stash-reflog.log` in the lane's cache) | `update_refs` syncs `logs/refs/stash` (`ref_files`, `sync_ref_files`) before the directory syncs and any record | `GitSync` keeps the stash undurable under mutation `stash_reflog_unsynced`; `Crash`'s power loss enabled under it | `MC_ec_neg_stash_reflog_unsynced` fails `TypedRefusal`; under the rule every stash crash row covers it |
| 4 | Empty `HEAD.lock`/`index.lock` after a power loss wedges | CONFIRMED (create, then write, never synced) | `publish_lock`: the marker is written to `bulkload-lock-<pid>-<n>`, synced, and renamed into place without replacing (link fallback), so the lock never exists without its marker; `sweep_lock_temps` removes a crash's temporaries; used by `HeadLocks` and `publish_index` | none (a lock's content is not modelled; the code's rule is the Rust row's) | Rust row only (MU16) |
| 5 | Killed `read-tree` leaves `bulkload-index-<tag>-fresh.lock`; untyped `GIT_CHILD_FAILED` after the intent | CONFIRMED | `publish_index`'s cleanup removes `fresh.lock` and `merged.lock`; `installed_index` uses one scratch name per worktree, removed with its lock; every `GIT_CHILD_FAILED` after a workspace intent is `GIT_CONVERGE_INTERRUPTED` (`after_intent`) | not modelled (private scratch names; `publish_index` is one step) | fault row `estate_converge_index_locked_fresh_lock` |
| 6 | `packed-refs.new` left by a killed delete wedges | CONFIRMED | `remove_orphan_packed_new`: with deletes or a stash op pending and no `packed-refs.lock`, the orphan is removed (checked lock first) | lock `N:packed` left by `ChildCrashTxn`; `Begin` removes it when no `E:packed`; mutation `packed_new_kept` | `MC_ec_empty_lock` (LiveSpec) pass; `MC_ec_neg_packed_new_kept` fails `NoWedge` |
| 7 | A create beside an operator's symbolic ref never demoted | CONFIRMED | `plan_names`, `finish` and `head_plan` count the symbolic names (and `branch_line_fails`) | not modelled (symbolic refs, stated) | Rust rows (MU21, integration) |
| 9 | `HEAD.lock` of a killed `set_head` beside a pending ref intent wedges | CONFIRMED | `settle_repository`, before any ref settle (apply_workspace, apply_refs, settle_refs, plan_linked): every pending workspace intent's HEAD and branch locks (content = its new values), marked locks, lock temporaries | `TxnOK` fails on any `HEAD:j` lock; a killed child is `INTERRUPTED` in a pass that goes on (`KilledOrCrashed`), or a host crash; a HEAD-only `set_head` child can be killed too; `Begin` removes `IntentHeadLocks`; mutation `intent_head_lock_kept` | `MC_ec_child_crash_linked` pass; `MC_ec_neg_intent_head_lock_kept` fails `TypedRefusal` |
| 10 | Settle removes live Git locks ("empty and older than 2 s") | CONFIRMED (packed-refs.lock and a delete's lock are empty for live writers; reproduced with a live `update-ref --stdin` session) | no empty lock is ever removed (`remove_owned_lock` takes content only); the transaction that meets one refuses `GIT_CONVERGE_INTERRUPTED` until its owner or the operator removes it: a stated limit with a `NoWedge` exception | empty locks `E:<ref>`, `E:packed`, `E:stash`; operator kind `lock` (a live writer holds one, or the operator removes a stale one); ghost `opLock`, `NoLocalWorkLost` requires `opLock \subseteq locks`; `NoWedge`'s exception `Unproven`; `Clean` requires none; mutation `empty_lock_removed` | `MC_ec_empty_lock` pass; `MC_ec_neg_empty_lock_removed` fails `NoLocalWorkLost` |
| 11 | `git switch` reads the branch before `HEAD.lock` | CONFIRMED (a limit; no lock closes it: the index is committed before HEAD is locked) | stated in `transact`'s doc and the limits | stated abstraction (a switch is one step); not modelled | none |
| 12 | No Rust test pins the two-transaction `set_head` | CONFIRMED | test hook over `update_refs` playing Git's killed child (HEAD's line renamed first) | none | `set_head_never_commits_head_without_its_branch` (MU15 fails) |
| 13 | Unseeded negative simulation; positive by simulation only | CONFIRMED | none | `MC_ec_neg_step_moves_intent_branch` gets `-seed 20261009` (its violation is found at that seed); the positive row stays a seeded simulation: a BFS probe of its bound reached 2,093,825 distinct states at depth 16 with 608,901 queued after 11 minutes and was stopped; it does not fit the 1,800 s budget, and a smaller bound loses the scenario (three source changes: a switch with a commit, then another commit on that branch, and an index edit to hold the settle). So finding 2's "fixed" rests on 20,000 seeded traces plus its Rust row (`a_pending_intents_head_branch_is_left_to_its_item`), stated as sampling in the verdict below | rows |

Model changes besides the per-finding ones: a killed Git child is a
nondeterministic host crash or an `INTERRUPTED` refusal (justified by the
lock it left) in a pass that goes on (`KilledOrCrashed`, both
`ChildCrash*` actions); `TxnOK` fails on any `HEAD:` lock and on a lock no
content proves bulkload's (`LockedOn`; `RefWhy` counts it); two operator
actions (`OpLock`, `OpWtWork`) and two variables (`opLock`, `wtw`); the
catalogue adds property `OneCheckout` (safety, index 6), operator kinds
`lock` and `wtwork`, six mutations (CODE BEFORE THE FIX), five pass rows
and six negative rows, and `OpLock` and `OpWtWork` in every earlier pass
row's never-enabled set. Probes during the work: `MC_ec_one_checkout_crash`
with `OpConcurrent` took 2,530,460 states (19 min), so its row runs the
operator's switch between applies (1,061,046 states), the concurrent case
being `MC_ec_one_checkout`'s; `MC_ec_neg_intent_head_lock_kept` first
passed (558,743 states) because the model let only the second of
`set_head`'s two transactions die, never a HEAD-only one: the code's
HEAD-only `update-ref` child leaves `HEAD.lock` the same way, so
`ChildCrashHead` now covers it, and the row fails `TypedRefusal` at the
reviewer's trace.

### Results against the code after review 4

`cd <worktree> && export TMPDIR=/dev/shm/converge-tmp && nice -n 10 nix
develop .#default --command just tla-check <rows>`, two concurrent
invocations (34 pass and simulate rows; 50 reach, finding and negative
rows), each after its budget self-test, on the final spec (sha256
`809734fac73c5b1456c0644834937ebcad121e11f4ad87671c266e22baa29e18`):
**all 85 rows matched** (33 pass, 10 of them LiveSpec; the seeded
simulation clean, 2,400,001 states checked in 426 s; 3 reach; the two
documented-limit finding rows and the unfair negative fail
`ConvergesWhenQuiet`; 44 mutation rows fail their property; self-test
INCONCLUSIVE). Total wall 5,360 s and 3,689 s, peak RSS 2,641 MiB, host
load 70 to 110. New rows: `MC_ec_one_checkout` 110 distinct,
`MC_ec_one_checkout_crash` 1,061,046 (245 s), `MC_ec_child_crash_linked`
562,192 (150 s), `MC_ec_empty_lock` (LiveSpec) 837,
`MC_ec_first_linked_crash_op` 55; `MC_ec_neg_head_attach_unchecked` fails
`OneCheckout` (98 states), `MC_ec_neg_intent_head_lock_kept` `TypedRefusal`
(119,775), `MC_ec_neg_empty_lock_removed` `NoLocalWorkLost` (198),
`MC_ec_neg_packed_new_kept` `NoWedge` (849), `MC_ec_neg_stash_reflog_unsynced`
`TypedRefusal` (105), `MC_ec_neg_recovery_overwrites` `NoLocalWorkLost`
(55); `MC_ec_neg_step_moves_intent_branch` (seeded) fails `CrashAtomicity`
(204,947 states checked). `just tla-render --check`: all 185 files equal
the catalogue. Per row: README, "Results (EstateConverge)". Logs:
`/srv/fast-local/jess/cache/cargo-target/converge/logs/r4/tla-final-{A,B}.log`.

### Verdict for OI-1003-Q144's merge gate (after review 4)

Every review 4 finding is fixed in the code and, where the model can
express it, in the model, and the model passes every row at its bounds
against the fixed code, except the two documented limits (items 6 and 18
of review 3's list: liveness only) and with three stated limits added
here: an empty lock (a killed delete's, a stash rewrite's, Git's own
log-only `HEAD.lock`) refuses typed until the operator removes it
(`NoWedge`'s exception), a `git switch` that read its branch before a
transaction moved it is not caught (finding 11), and finding 2's
stale-branch fix is checked by seeded simulation, not exhaustively. By
Q144's gate the convergence may merge once the operator rules items 6 and
18 known limits (or a metadata-only A4 check) and accepts the three limits
above (or rules otherwise).

| Invariant | Code after review 4 (staged) |
|---|---|
| NoLocalWorkLost | Holds at every bound (now also: a live writer's lock is never taken; a crashed linked landing's recovery never overwrites the operator's work) |
| NoSourceWorkLost | Holds |
| TypedRefusal | Holds |
| NoReRead | Holds |
| CrashAtomicity | Holds |
| OneCheckout | Holds (new) |
| ConvergesWhenQuiet | Holds, except the two documented limits |
| NoWedge | Holds, except while an empty lock stays (stated) |

Open: the operator's rulings above; Linear TIN-4543 (the coordinator posts
this verdict); freezing EstateConverge's names needs a ruling.
