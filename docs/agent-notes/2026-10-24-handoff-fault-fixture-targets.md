# Legacy handoff fault fixtures select their targets

Authority: the operator's 2026-10-24 bulkload completion request, SPEC goal 1
and R-N13. Lexical: XL-ACC-004.

The handoff fault fixtures assert preservation of `AGENTS.md` and `CLAUDE.md`.
Production handoff now targets `CLAUDE.local.md`. The fixtures did not select
their existing debug switch, `BULKLOAD_HANDOFF_TRACKED_TARGETS`.

Honey's `validation14-fault.log` recorded 164 passes, 17 failures and two
ignored cases. Failures included legacy block assertions and unreachable
fault points. The fixture and underlying handoff implementation matched
`5b5efd7` byte for byte. Baseline execution remains the coordinator's check.

Each handoff scenario now reruns its exact test in an owned subprocess.
That command selects the legacy targets. Its crash commands also select them
explicitly. The shared harness environment is never changed. Existing fault
points, exit checks, recovery assertions and idempotency checks remain.

No production source changed. No build or test ran on sting. Honey and PZM
must supply fresh fault-harness results. No estate or resumed-session claim
follows from this fixture change.

Mutation receipt: actor=completion reviewer | target=owned completion branch,
`tests/fault_harness/estate_handoff.rs` and this note | reason=select the
fixtures' intended legacy targets | ruling=SPEC goal 1, R-N13, operator
2026-10-24 | prior_state=legacy assertions with production target selection |
result=legacy selection scoped to each scenario subprocess and crash child.
