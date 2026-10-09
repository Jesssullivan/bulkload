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

## Verdict for OI-1003-Q144's merge gate

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

## Model-vs-implementation discrepancies (final, after review 3)

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
