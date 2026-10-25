# 2026-10-24: bounded native reconnect

Authority: SPEC rules 6 and 8 and the operator's October 24 direction.
Neo may reboot repeatedly while softconnect, lab and blahaj estates move.
R-N13. Lexical: XL-ACC-004.

| stream | owner | repo/branch | state/evidence | next |
| --- | --- | --- | --- | --- |
| Reconnect | Bulkload coordinator | codex/reconnect-20261024 | Source implemented, validation pending | Targeted honey tests and review |

`--reconnect-seconds=N` opts native file pulls into a reconnect budget.
The default is zero. Each attempt uses the same roots and durable states.
Only `TRANSPORT_LOST` retries. Backoff grows from one second to fifteen.
The admission/backoff window starts at the first transport loss.
Only newly committed payload resets it. Completed receives, equal adoptions
and bookkeeping commits never reset it. Failed connections never reset it.
The window does not cancel an in-flight attempt or promise a wall-time deadline.

The option applies to file pulls composed inside `estate-move`.
It does not replay Git imports, SQLite unions or source retirement.
The existing SSH builder sends keepalives every fifteen seconds.
Two unanswered keepalives end an established connection.

An absent remote root can close its stream before `Start`.
That presently appears as `TRANSPORT_LOST`, too. The option remains explicit.
It never treats a missing source as an empty source or deletes destination bytes.

The fake SSH regression cuts a data frame after another file is durable.
It refuses the next connection and then returns against the same states.
The committed file is reused. The partial file completes on the resumed pull.
Assertions require one output per path and no state payload.
Fixtures use the engine's full racy-window helper before initial transfer.
The cut waits for committed output metadata, not a visible filename alone.
State measurements use the real `serve/` and `pull/` ledger layout.
A separate case loses SSH status after complete publication, twice.
Other cases cover budget exhaustion, default behavior, occupied destinations
and invalid budgets. The device-alias regression covers three source renumberings.
A fake clock verifies repeated productive intervals longer than the reconnect
budget. Each subsequent outage still receives its full reconnect budget.
Persistent lost SSH status is bounded for empty, warm, equal-adopted and cold
roots. This guards against resetting the window on an unchanged receive.

Source: `crates/bulkload-agent/src/reconnect.rs`, `src/main.rs`,
`tests/reconnect.rs` and `src/transfer/tests/source_dev.rs`.
The source-command registry pins the keepalive arguments.

Validation so far: `cargo fmt --all` and `git diff --check` pass.
No build, test, host delivery or live reboot acceptance has run in this lane.
