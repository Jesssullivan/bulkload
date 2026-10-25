# 2026-10-24: hand-off checks honor accepted refusal instances

Authority: user direction 2026-10-24, complete bulkload across the neo and
sting estates. R-N13 and R-HOOK-CONVERGENCE-20261004 govern this receipt.
SPEC Done 2 requires "nothing unaccounted, every hold listed for the operator".
SPEC rule 1 requires gaps to be fixed in bulkload.
Lexical: XL-ACC-004.

TIN-6569 records accepted refusals remaining open in `estate-handoff --check`.
The check read raw outcomes without joining the disposition ledger.

`estate-handoff --dispositions LEDGER` reads the existing ledger with its
plan path, plan digest and source binding. An exact item-instance `accept`
closes that refusal's open row. The capture and complete outcome record
define the instance. Changed capture, site, errno or reason invalidates it.
Policies, `re-carry` and `abandon` leave the row open. Unaccounted outcomes
cannot inherit acceptance. Other open rows are unaffected.

Accepted refusals remain visible as `reviewed-refusal` JSON lines with the
item, instance, code, reviewer and date. Their changed facts invalidate the
cached open count. `--check` still writes nothing. A later invocation must
name the ledger again. This does not declare a lane complete.

| stream | owner | repo/branch | state/evidence | next |
| --- | --- | --- | --- | --- |
| disposition-aware hand-off | Codex handoff lane | bulkload/codex/handoff-dispositions-20261024 | source patch and regression fixtures | targeted tests, fmt and clippy on honey |

Regression fixtures exercise exact acceptance, unchanged check writes, stale
capture and outcome instances, work decisions, policies, foreign source and
plan digests, placed main and linked worktrees, and independent destination
work. Placed paths identify family members without changing original item
identities or disposition bindings. No build or test ran on sting.
Validation is pending the coordinator's isolated honey lane. No estate
transfer, disposition write against a live estate or source retirement ran.
