# Formal model: wire v5, Held, group commits and resume

The proof package's formal model ([docs/slo.md](../slo.md), OI-1003-Q7): a
TLA+ specification of bulkload's transfer, model-checked with TLC. TLA+ and
TLC are the checker of record (OI-1003-Q32). Sprint 2 adds a Dhall config
catalogue and a Haskell N-version explorer on [the shared core](#n-version-core-oi-1003-q32).
The model covers:

- wire v5 per entry;
- the destination's staging, seal, no-replace publish, directory seal and
  SQLite group commit;
- `Held` and the source's digest-only capture ledger;
- the source store's authority, which is part of every row key;
- crashes of either host or both, and the rerun that follows;
- S2's typed source access;
- the WP0(d) and WP0(g) rulings.

It models the code at **adb9c66** (`origin/main` after #145). Every code
symbol cited below was checked with `git grep` at adb9c66 and again at
`origin/main` 6268175, where the cited files are unchanged except
`git_carry.rs`, whose cited items (`git`, `git_env`, `racy`) are still there.

| File | What it is |
|---|---|
| [`BulkloadTransfer.tla`](BulkloadTransfer.tla) | The specification. Its header states the scope, the abstractions and the code map. Every action cites the function it models. |
| [`gen_cfgs.py`](gen_cfgs.py) | One table that renders every `MC_*.cfg` and `configs.tsv`. Edit the table, never the outputs. Sprint 2's Dhall catalogue replaces it (OI-1003-Q32). |
| `MC_*.cfg` | TLC configurations, rendered by `gen_cfgs.py`. |
| [`configs.tsv`](configs.tsv) | The run order. Columns: `name`, `expect`, `named-property` (the one property a fail row must violate), `flags` (extra TLC arguments, one argv element per word). |

## Running it

```sh
just tla-check              # every row of configs.tsv
just tla-check MC_nv_core   # the budget self-test, then the named configs
```

`tla-check` is a standalone recipe at the end of the justfile. No tier
depends on it: `check-fast`, `check-optional`, `check-full` and CI never start
TLC, so CI stays slim (OI-1003-Q7).

How the recipe runs:

- TLC 2.19 comes from the flake's pinned nixpkgs (`nix shell --inputs-from .
  nixpkgs#tlaplus`); there is no flake change.
- The rows of `configs.tsv` run in order, one JVM at a time, with `-Xmx4g`,
  `-workers 3`, `nice -n 10` and `-coverage 1`.
- TLC state and logs go under a private `mktemp -d` in `$TMPDIR`. It is
  removed when every row matches its expectation. Otherwise the logs stay,
  and their paths are printed.
- The first row is the budget self-test. If it does not trip `WithinBudget`,
  the recipe stops before any other config, because every other config relies
  on that budget to end.
- It prints one line per config: outcome, the properties the log reports
  violated, distinct and generated states, diameter (TLC's "depth of the
  complete state graph search"), wall time and peak RSS, then the actions
  that were never enabled. It ends with the total wall time and the peak RSS.

A config gets one of five outcomes:

| Outcome | Meaning |
|---|---|
| PASS | Model checking finished with no error. |
| FAIL | The row's named property was violated, and nothing else was: no other invariant, no `TypeOK`, no deadlock. Only this counts as a caught mutant. |
| SIMULATION | `-simulate` finished its random behaviours with no error. This is evidence, never a model-checking result. |
| INCONCLUSIVE | The wall-clock budget tripped (`WithinBudget`), or the log has no `Finished` line. It never counts as a pass or a caught mutant. |
| WRONG | Anything else. |

The recipe fails unless every row's outcome equals its `expect` column. Every
fail config also checks `TypeOK`, so a type error in a mutant shows up as
WRONG rather than hiding behind the named property. Deadlock checking stays
on everywhere: `Terminated` is the only place a behaviour may stop.

To run one config by hand:

```sh
cd docs/formal
JAVA_TOOL_OPTIONS=-Xmx4g nice -n 10 nix shell --inputs-from ../.. nixpkgs#tlaplus --command \
  tlc -workers 3 -metadir "$(mktemp -d)" -config MC_nv_core.cfg BulkloadTransfer.tla
```

### The budget is state-level

`WithinBudget` reads TLC's own clock (`TLCGet("duration")`). It is written

```tla
WithinBudget == run \in 0..MaxRuns => TLCGet("duration") < BudgetSeconds
```

on purpose:

- **A budget over the clock alone never trips.** TLC evaluates a zero-arity
  definition that names no variable once, at startup. A two-variable probe
  spec with a 2 s budget showed it on TLC 2.19: the clock-only form explored
  all 2,253,001 states in 7 s without tripping, and the state-level form
  tripped at 2 s. The first draft of this model used the clock-only form;
  the lane's duplicate copies reported that a two-seat, three-run config ran
  28 minutes past its 1,500 s budget, through 27.6 million distinct states
  (not reproduced here).
- **The conjunct over `run`** makes TLC evaluate the budget on every state.
  It is always true (`TypeOK` bounds `run`), so it changes no verdict.
- **The proof.** `MC_budget_selftest` runs `MC_main`'s constants with a 5 s
  budget and must come out INCONCLUSIVE. Its bound is `MC_main`'s, so even a
  broken budget would end (as a PASS, which the recipe rejects).
- **No outside clock.** Nothing else ends a TLC run: no wrapper, no signal.

## Results

The run of record: one full `just tla-check` on host sting (32 cores,
Linux 6.12, TLC 2.19 on OpenJDK 8), 2026-10-04, over the spec and configs
committed in `3760263`. **All 35 rows matched their expectation: 9 PASS,
1 SIMULATION, 1 INCONCLUSIVE (the budget self-test), 24 FAIL, 0 WRONG. Total
wall time 379 s; peak RSS 1,901 MiB.** An earlier full run the same night
gave the same verdicts and, for every positive config, the same distinct
and generated counts and diameters (345 s, 1,869 MiB).

| Config | Constants | Expect | Verdict | Violated | Distinct | Generated | Diameter | Wall | RSS MiB |
|---|---|---|---|---|---:|---:|---:|---:|---:|
| `MC_budget_selftest` | {a,b} R2 C1 E1 sym budget 5 s | inconclusive | **INCONCLUSIVE** | `WithinBudget` | 48,872 | 170,150 | 14 | 7s | 965 |
| `MC_main` | {a,b} R2 C1 E1 sym budget 600 s | pass | **PASS** | – | 869,296 | 2,825,196 | 51 | 68s | 1901 |
| `MC_main_deep` | {a} R3 C2 E1 F1 X1 space budget 600 s | pass | **PASS** | – | 334,296 | 1,310,981 | 49 | 31s | 1758 |
| `MC_dest_faults` | {a,b} R2 C0 E0 F1 X1 space sym budget 600 s | pass | **PASS** | – | 181,785 | 504,315 | 52 | 20s | 1710 |
| `MC_nv_core` | {a} R3 C2 E1 budget 600 s | pass | **PASS** | – | 15,834 | 44,312 | 45 | 4s | 592 |
| `MC_wp0g` | {a,b} R2 C1 E1 relaxed sym budget 600 s | pass | **PASS** | – | 878,950 | 2,905,085 | 51 | 85s | 1826 |
| `MC_wp0g_deep` | {a} R3 C2 E1 F1 X1 space relaxed budget 600 s | pass | **PASS** | – | 354,580 | 1,482,185 | 49 | 39s | 1709 |
| `MC_wp0d_exchange` | {a} R3 C1 E1 F1 exchange budget 600 s | pass | **PASS** | – | 70,086 | 186,977 | 51 | 9s | 1002 |
| `MC_s2` | {a} R2 C1 E1 estate budget 600 s | pass | **PASS** | – | 61,956 | 236,660 | 39 | 7s | 810 |
| `MC_live` | {a,b} R2 C0 E0 space budget 600 s | pass | **PASS** | – | 12,649 | 31,118 | 47 | 8s | 784 |
| `MC_main_sim` | {a,b} R3 C1 E1 F1 X1 space budget 600 s | simulate | **SIMULATION** | – | 1,147,363 | – | – | 37s | 1560 |
| `MC_wp0g_authority` | {a} R2 C1 E0 relaxed relaxed-auth budget 300 s | fail | **FAIL** | `R25_NoDurableReread` | 714 | 1,340 | 18 | 2s | 284 |
| `MC_wp0d_check_rename` | {a} R2 C0 E1 F1 check_rename budget 300 s | fail | **FAIL** | `NoClobber` | 6,323 | 12,153 | 27 | 2s | 501 |
| `MC_neg_live_unfair` | {a} R1 C0 E0 budget 300 s | fail | **FAIL** | `RunsClose` | 78 | 109 | – | 2s | 234 |
| `MC_neg_held_before_commit` | {a} R3 C2 E1 mut=held_before_commit budget 300 s | fail | **FAIL** | `HeldAfterCommit` | 278 | 661 | 7 | 2s | 273 |
| `MC_neg_commit_before_fsync` | {a} R3 C2 E1 mut=commit_before_fsync budget 300 s | fail | **FAIL** | `RecordImpliesBytes` | 720 | 1,857 | 9 | 1s | 286 |
| `MC_neg_commit_before_dirseal` | {a} R1 C0 E0 mut=commit_before_dirseal budget 300 s | fail | **FAIL** | `RecordImpliesBytes` | 86 | 129 | 17 | 1s | 251 |
| `MC_neg_adopt_without_seal` | {a} R1 C0 E0 F1 mut=adopt_without_seal budget 300 s | fail | **FAIL** | `RecordImpliesBytes` | 334 | 544 | 14 | 2s | 271 |
| `MC_neg_ledger_before_held` | {a} R1 C0 E0 mut=ledger_before_held budget 300 s | fail | **FAIL** | `LedgerAfterHeld` | 42 | 58 | 12 | 1s | 212 |
| `MC_neg_done_before_sync` | {a} R1 C0 E0 mut=done_before_sync budget 300 s | fail | **FAIL** | `DoneAfterLedger` | 80 | 117 | 17 | 2s | 251 |
| `MC_neg_reread_durable` | {a} R2 C1 E0 mut=reread_durable budget 300 s | fail | **FAIL** | `R25_NoDurableReread` | 347 | 727 | 20 | 2s | 299 |
| `MC_neg_reread_unchanged` | {a} R2 C1 E0 mut=reread_durable budget 300 s | fail | **FAIL** | `S3_UnchangedReadsZero` | 396 | 839 | 23 | 2s | 283 |
| `MC_neg_reread_changed_only` | {a} R2 C1 E0 mut=reread_durable budget 300 s | fail | **FAIL** | `S3_ReadsOnlyChanged` | 448 | 980 | 29 | 2s | 323 |
| `MC_neg_skip_output_row` | {a} R1 C0 E0 mut=skip_output_row budget 300 s | fail | **FAIL** | `S3_ClosedPassIsHeld` | 78 | 107 | 17 | 4s | 265 |
| `MC_neg_double_read` | {a} R1 C0 E0 mut=double_read budget 300 s | fail | **FAIL** | `ReadOnce` | 30 | 37 | 8 | 12s | 201 |
| `MC_neg_src_ledger_carries_r25` | {a} R3 C2 E1 mut=src_ledger_carries_r25 budget 300 s | fail | **FAIL** | `R25_NoDurableReread` | 3,646 | 9,527 | 15 | 3s | 462 |
| `MC_neg_record_racy` | {a} R1 C0 E1 mut=record_racy budget 300 s | fail | **FAIL** | `ReuseSound` | 359 | 538 | 14 | 2s | 269 |
| `MC_neg_record_racy_ledger` | {a} R1 C0 E1 mut=record_racy budget 300 s | fail | **FAIL** | `LedgerSound` | 467 | 711 | 16 | 2s | 276 |
| `MC_neg_untyped_space` | {a} R1 C0 E0 X1 mut=untyped_space budget 300 s | fail | **FAIL** | `ClosureAccounted` | 100 | 132 | 16 | 1s | 251 |
| `MC_neg_source_write` | {a} R1 C0 E0 mut=source_write budget 300 s | fail | **FAIL** | `S2_TypedSourceAccess` | 13 | 16 | 5 | 2s | 206 |
| `MC_neg_pause_writer` | {a} R1 C0 E0 mut=pause_writer budget 300 s | fail | **FAIL** | `S2_TypedSourceAccess` | 13 | 16 | 5 | 2s | 209 |
| `MC_neg_git_optional_locks` | {a} R1 C0 E0 estate mut=git_optional_locks budget 300 s | fail | **FAIL** | `S2_TypedSourceAccess` | 8 | 8 | 3 | 2s | 211 |
| `MC_neg_unbounded_backup` | {a} R1 C0 E0 estate mut=unbounded_backup budget 300 s | fail | **FAIL** | `S2_BackupLockBounded` | 313 | 775 | 9 | 2s | 289 |
| `MC_neg_supersede_unchecked` | {a} R1 C0 E0 F1 exchange mut=supersede_unchecked budget 300 s | fail | **FAIL** | `NoClobber` | 266 | 412 | 12 | 2s | 269 |
| `MC_neg_sweep_displaced` | {a} R3 C1 E1 F1 exchange mut=sweep_displaced budget 300 s | fail | **FAIL** | `NoClobber` | 37,715 | 95,046 | 28 | 6s | 745 |

Reading the table:

- **Constants.** Seats; `R` runs, `C` crashes, `E` source edits per seat;
  `F` third-party writes, `X` failed group commits; `space` the space
  refusal; `relaxed` relaxed ledger rows, `relaxed-auth` a relaxed
  store-creation commit; `exchange` or `check_rename` the WP0(d) design;
  `estate` estate capture's typed reads; `mut=` the mutation; `sym`
  `SYMMETRY` over seats; `budget` the `WithinBudget` seconds.
- **Distinct** is counted under symmetry where `sym` is shown. For
  `MC_main_sim` it is the number of states the simulation checked, not a
  distinct count. For `MC_budget_selftest` it is wherever the 5 s budget
  stopped the search, so it changes from run to run.
- **Fail rows.** Their counts are where the search stopped at the first
  violation. With 3 workers that varies a little between runs; the verdict
  and the violated property do not. For a fail row, "Diameter" is the depth
  reached when it stopped; TLC prints none for a liveness counterexample.
- **Violated** lists every property the log reports violated. A FAIL row
  shows exactly its named property, never `TypeOK` and never a deadlock.

Bounds:

- Constants are deliberately small (the small-scope hypothesis).
- Breadth comes from two seats (`MC_main`, `MC_dest_faults`, `MC_wp0g`,
  `MC_live`), with `SYMMETRY` over seats in the safety configs.
- Depth comes from one seat with three runs and two crashes (`MC_main_deep`,
  `MC_wp0g_deep`, `MC_nv_core`).
- No config needed reducing: every positive finishes in under 90 s, far
  inside its 600 s budget.
- The constants first drafted for `MC_main` (two seats, three runs, every
  fault) appear only as `MC_main_sim`: a seeded simulation (`-simulate
  num=3000 -depth 120 -seed 20261003`), recorded as SIMULATION and never as
  a model-checking result.

## Coverage

Coverage is TLC's `-coverage` report, the last one in each log. An action is
*never enabled* in a config when it generated no state at all. `Next` has 37
actions (38 coverage entries with `Init`).

- `MC_main` enables 25 of them, and `MC_main_deep` enables 28.
- Every action not enabled there is switched off by that config's own
  constants, and is enabled in another config.

| Never enabled | `MC_main` | `MC_main_deep` | Why it is off there | Enabled in |
|---|---|---|---|---|
| `ForeignWrite`, `ForeignDelete` | yes | no | `MaxForeign = 0`: `MC_main` has destination faults off by design | `MC_main_deep`, `MC_dest_faults`, `MC_wp0g_deep`, `MC_wp0d_exchange` |
| `CommitFail` | yes | no | `MaxCommitFails = 0`, the same reason | `MC_main_deep`, `MC_dest_faults`, `MC_wp0g_deep` |
| `Exchange`, `VerifyDisp` | yes | yes | `SupersedeMode = "off"`: the code has no superseding publish, and these model WP0(d)'s exchange design | `MC_wp0d_exchange`, `MC_neg_sweep_displaced` |
| `CheckOwn`, `RenameReplace` | yes | yes | `SupersedeMode = "off"`; these model WP0(d)'s rejected check-then-rename design | `MC_wp0d_check_rename`: its counterexample takes `CheckOwn` at state 25 and `RenameReplace` at state 27 |
| `GitRead`, `BackupBegin`, `BackupStepLock`, `BackupStepUnlock`, `BackupEnd` | yes | yes | `EstateReads = FALSE`: estate capture's typed reads do not depend on the transfer, so S2 is checked on its own | `MC_s2` |

Two actions are enabled in both main configs yet add no new state there:

- **`Terminated`** is the stuttering step at the end of a behaviour. It
  never adds a state; it stops a finished behaviour from counting as a
  deadlock.
- **`CrashBoth`** adds 0 distinct states in `MC_main`, `MC_main_deep`,
  `MC_nv_core`, `MC_s2` and `MC_wp0d_exchange`, and 297 + 2,194 in `MC_wp0g`
  and 6,655 in `MC_wp0g_deep`. Under a strict source ledger, a loopback
  power loss reaches only states that `CrashDst` reaches one step earlier,
  before the `Held` round trip. Only a relaxed ledger, which can lose rows
  at the source too, makes the double crash different. So the model's
  WP0(g) checks do exercise it.

The other configs' never-enabled actions also follow from their constants.
For example, `MC_dest_faults` and `MC_live` have no source edit and no crash,
so `Edit`, `SilentRewrite`, the crashes and `RecvRefused` (a refusal needs a
changed source) never fire there. A fail config's coverage covers only the
part of the space it searched before the counterexample.

## N-version core (OI-1003-Q32)

The model is a hybrid. A second, independent explorer, sprint 2's Haskell
BFS, must reproduce TLC's count on one shared core. That core is
`MC_nv_core`:

- one seat, three runs, two crashes, one source edit;
- third-party writes, commit failures, the space refusal, superseding
  publish and estate reads all off;
- no `SYMMETRY`, so the count is the plain state graph's.

| Config | Initial states | Generated | Distinct | Diameter | Max out-degree |
|---|---:|---:|---:|---:|---:|
| `MC_nv_core` | 2 | 44,312 | **15,834** | 45 | 6 |

The distinct count of a completed breadth-first search is the reachable set,
so it does not depend on the number of workers. This session's two full runs
both counted 15,834 with 3 workers; the duplicate copies reported the same
count with 1 and 4.

The mutations `held_before_commit`, `commit_before_fsync` and
`src_ledger_carries_r25` run on the same core as separate fail configs. Both
explorers must find those counterexamples. A failing config's state count
depends on where its search stops, so only the positive core's count is a
cross-check.

## Liveness assumption (WF_vars)

```tla
LiveSpec == Spec /\ WF_vars(Protocol)
```

`Protocol` is every bulkload action: the walk, decisions, captures,
manifests, the destination pipeline, the commits, `Held`, the ledger,
`SourceDone`, finishing, starting a run, and estate capture's reads. The
assumption is weak fairness on that whole disjunction: if some bulkload step
stays enabled, some bulkload step eventually happens. The environment (source
edits, racy rewrites, the clock tick, third-party writes, commit failures and
crashes) is left unfair, so it may act but never has to.

`MC_live` checks two properties under `LiveSpec`, with no crash, a stable
source, no third-party writes and no failed commits, and without `SYMMETRY`
(symmetry is unsound for liveness):

- `RunsClose`: `sess = "on" ~> sess = "done"`.
- `AllRunsFinish`: `<>(run = MaxRuns /\ sess = "done")`.

`MC_neg_live_unfair` drops the fairness, and `RunsClose` fails: a run may
stutter short of closure. So the liveness claim is exactly: under
`WF_vars(Protocol)`, with no crash and a source that has stopped changing,
every started run closes and every run is made. Convergence under a source
that never quiesces, or under unbounded crashes, is not claimed.

## WP0(g) verdict (OI-1003-Q20)

**Q20 holds for the ledger's row commits, on one condition: the commit that
creates the source store's authority must stay durable.**

**1. Relaxed ledger rows are safe** (`MC_wp0g`, `MC_wp0g_deep` pass). The
model lets a source power loss drop any subset of the committed `captures`
rows. Every safety property still holds. R25 is carried by the destination:

- A source row is submitted only on `Held{true}`, which follows the
  destination's own `synchronous=FULL` group commit.
- So a lost source row always names a seat whose output row is already
  durable at the destination.
- On the next run the destination answers that seat `Reuse` from its own row,
  and the source reads nothing.
- A lost row costs a read only where the destination no longer holds the seat
  (a third party removed or replaced the output). R25 and S3 allow that read.
- `MC_neg_src_ledger_carries_r25` shows the converse. Had `Reuse` needed the
  source row, R25 would fail even with a strict ledger, because the row
  trails the destination commit by the `Held` round trip.

**2. Relaxing the store-creation commit breaks R25** (`MC_wp0g_authority`
fails `R25_NoDurableReread`, and only it, with a 15-state counterexample).

The counterexample: run 1 sends seat `a`, seals, publishes and commits its
destination row under authority 1. The source loses power (`CrashSrc`)
before the store-creation commit reaches disk. Run 2's `Store::open` creates
authority 2, the destination finds no row under the new key, and the source
reads seat `a`, whose unchanged bytes the destination holds durably.

- `Store::open` (`A/transfer_store.rs`) writes a random authority into
  `settings` in the same transaction that creates the schema, after
  `configure_sqlite`.
- `serve` sends that authority in `Start`, and both sides put it into every
  row key (`row_key`).
- Suppose the relaxed settings also cover that transaction. Under WAL with
  `SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE`, a small ledger may not checkpoint for
  many runs. A power loss then takes the store, authority and all.
- The next run makes a new authority. No destination row matches its keys,
  so the run re-reads every seat the destination holds durably.

**Conditions for adopting WP0(g):**

- Commit the creation transaction with `synchronous=FULL` (or checkpoint and
  sync it before `Start`), so the authority is durable before any key uses it.
- Relax only `commit_captures`, and only on the source side. The destination
  store stays `synchronous=FULL`, `fullfsync=ON`.
- Treat a corrupt or absent source ledger (its `captures` rows) as empty,
  exactly like a ledger that lost every row. Never refuse on it and never
  trust a damaged row. The destination's `Reuse` still covers every seat it
  holds, so only the other seats are read. The empty ledger is a case the
  model covers (the empty subset).
- Never mint a new authority because ledger rows are gone. The authority must
  survive anything the relaxed commits can do to the ledger: a lost or
  unreadable authority is exactly `MC_wp0g_authority`, a full re-read.

**Caveat.** The model's loss is any subset of committed rows. That
over-approximates WAL `synchronous=NORMAL`, which can only lose a suffix of
commits, so a pass under subset loss covers suffix loss. It does not cover a
store that returns a *wrong* row: with `fullfsync=OFF` on Darwin, a power
loss may also reorder writes. That is why the code must treat a corrupt or
absent ledger as empty, and keep `checkpoint_fullfsync=ON` so that a
checkpoint stays a full barrier and the WAL's frame checksums discard a torn
tail. Corruption itself is outside the model and not proven here.

## WP0(d): exchange versus check-then-rename (OI-1003-Q18)

The code has no superseding publish yet. A changed seat whose output exists
is adopted only if its bytes verify, and otherwise refuses
`GIT_DESTINATION_OCCUPIED`. `SupersedeMode = "off"` models that. Two designs
are checked against `NoClobber`: bulkload replaces or removes a destination
file only when its identity is one this store recorded.

- **`check_rename` fails** (`MC_wp0d_check_rename`, `NoClobber` and only
  it, with a 27-state counterexample). Comparing the output's `(dev, ino, stat)` with the
  store's row, then renaming over it, leaves a window. A third-party write
  that lands in between is clobbered. No fsync ordering closes that window.
- **`exchange` passes** (`MC_wp0d_exchange`). This is `RENAME_EXCHANGE`
  (`renameat2`; `renamex_np(RENAME_SWAP)` on Darwin) of the sealed new file
  with the output. The design then checks the *displaced* file's identity:
  - this store's own file is removed;
  - a foreign file is exchanged back;
  - after a crash, the next session restores a displaced foreign file instead
    of sweeping it like a temporary.

  `MC_neg_supersede_unchecked` and `MC_neg_sweep_displaced` show that the
  identity check and the recovery rule are each load-bearing.

WP5 PR 3 should implement the exchange shape. Where a filesystem has no
exchange, the publish should refuse and keep no-clobber. Its `crash_check`
traces should cover the displaced-name window.

## Abstraction map

Code at adb9c66, re-checked at `origin/main` 6268175. `A` is
`crates/bulkload-agent/src`, `P` is `crates/bulkload-proto/src`.

| Model | Code |
|---|---|
| `Walk` | `A/transfer.rs` `walk_source`, `Outbound::walked`, `Outbound::offer` (`Control::Entry`); `A/walk.rs` `Walker` |
| `RecvEntry` (Reuse / Send / WantManifest / Refuse) | `A/transfer.rs` `Inbound::entry`, `Inbound::admit`; `A/materialize.rs` `Destination::identity`; `A/transfer_store.rs` `Store::output_matches` |
| `RecvDecide` (capture) | `A/transfer.rs` `Outbound::decide`, `run_job`, `send_capture`, `manifest_capture`, `capture_file`, `capture_clock`; `A/git_carry.rs` `racy`; `A/transfer_store.rs` `Store::capture` |
| `RecvManifest` | `A/transfer.rs` `Inbound::manifest`, `plan_file` |
| `RecvNeed` | `A/transfer.rs` `Outbound::need_chunks`, `serve_chunks` |
| `RecvEnd` | `A/transfer.rs` `Inbound::end`, `end_streaming`, `end_filling`, `Inbound::publish`, `Inbound::adopt`; `A/materialize.rs` `verify_existing` |
| `RecvRefused` | `A/transfer.rs` `Inbound::refused` |
| `SealTemp`, `Publish` | `A/materialize.rs` `StagedFile::seal`, `StagedFile::publish`; `A/io/durable.rs` `seal_file`; `A/io/mod.rs` `publish_noreplace` |
| `DirSeal`, `SealAdopted` | `A/materialize.rs` `TouchedDevices::seal`, `PublishSink::commit` (`Publication::Adopted`); `A/io/durable.rs` `seal_dir` |
| `Commit`, `CommitFail` | `A/transfer_store.rs` `StorePublisher::commit_outputs`; `A/materialize.rs` `PublishSink::commit`, `space_refusal`; `A/io/durable.rs` `configure_sqlite`, `Committer` |
| `AnswerHeld` | `A/transfer.rs` `Inbound::answer_held`, `Inbound::settle_held` |
| `RecvHeld`, `LedgerCommit` | `A/transfer.rs` `Outbound::handle` (`Event::Held`); `A/transfer_store.rs` `LedgerSink::publish`, `StorePublisher::commit_captures` |
| `SendSourceDone` | `A/transfer.rs` `serve` (`committer.sync()`, then `Control::SourceDone`) |
| `Finish` | `A/transfer.rs` `Inbound::run`, `finish_receive`; `A/materialize.rs` `Destination::remove_salvaged` |
| `StartRun` | `A/transfer.rs` `receive`, `serve`; `A/materialize.rs` `Destination::sweep_root`; `A/transfer_store.rs` `Store::open`, `Store::authority`, `row_key` |
| `CrashSrc`, `CrashDst`, `CrashBoth` | `A/io/crash_check.rs` (persistence model); `A/io/durable.rs` `Committer` (its drop commits what is pending) |
| `GitRead` | `A/git_carry.rs` `git`, the `git_env` table |
| `BackupBegin`, `BackupStepLock`, `BackupStepUnlock`, `BackupEnd` | `A/provider_sqlite.rs` `snapshot` |
| Messages, refusal codes | `P/frame.rs` `Control`, `Decision`; `P/refusal.rs` `BulkloadRefusal::code` |
| `CheckOwn`, `RenameReplace`, `Exchange`, `VerifyDisp` | WP0(d) candidate designs; no code yet |
| `Edit`, `SilentRewrite`, `Tick`, `ForeignWrite`, `ForeignDelete` | The environment: live source writers, the 2 s racy allowance, third parties at the destination |

What the abstractions are:

- **Content and stat identity.** Content is an opaque version number per
  seat; a digest check is equality of versions. Stat identity
  `(dev, ino, size, mtime, ctime)` is a counter that only grows. The racy
  window is a per-seat flag that `Tick` clears. A capture (stat check, read,
  racy test) is one atomic step.
- **Store rows** are sets of records, and a commit is atomic. A row key is
  `authority epoch × 10 + stat version`.
- **Seals.** A seal is durable at once. On Darwin, `F_BARRIERFSYNC` is only an
  ordering barrier, and the group's full flush is the real durability point.
  That difference only removes crash states that come before the commit,
  where no row exists yet.
- **Relaxed source store.** A power loss drops any subset of committed ledger
  rows, or, under `RelaxedAuthority` before its creation is synced, the
  whole store.

## Properties, SLOs, rulings and tests

The P-numbers are the
[property-test plan](../plans/2026-10-03-property-test-plan.md)'s properties
that test the same claim on the real code. SLOs are
[docs/slo.md](../slo.md)'s S1–S5.

| Model property | Statement | SLO | Rulings | Property tests |
|---|---|---|---|---|
| `R25_NoDurableReread` | No source content read of a seat at a stat identity the destination holds durably. "Holds" means a committed row, recorded from that identity, vouches for the durable output at the path. Stated physically, under any source authority. | S3 | R25 / R-N58, OI-1003-Q7, OI-1003-Q20 | P23, P21, P19, P24, P33 |
| `R25_NoCommittedCaptureReread` | slo.md's wording: no committed capture (a source row whose output the destination still holds) is re-read. | S3 | R-N58, OI-1003-Q7 | P23, P33 |
| `ReadOnce` | A seat is read at most once per session. | S1, S3 | R-N58 | P23 |
| `S3_ReadsOnlyChanged` | A run reads only seats not held when it began (changed, racy, never carried, or lost at the destination) or changed during it. | S3 | OI-1003-Q18 (WP0(c), inequality 1) | P21, P23 |
| `S3_UnchangedReadsZero` | An unchanged source whose every seat is held reads 0 content bytes. | S3 | OI-1003-Q6, R-N58 | P21, P32 |
| `S3_ClosedPassIsHeld` | A session that closes with every seat applied (none racy, nothing changed) leaves every seat held, so `S3_UnchangedReadsZero` is not vacuous. | S3 | R-N58 | P23 |
| `RecordImpliesBytes` | A committed output row describes bytes whose data and name are durable at its path. | Durability (S4) | R-N86, R-N88 | P13, P14, P16, P33 |
| `HeldAfterCommit` | `Held{true}` is sent only after the output's group commit returned. | S3, durability | OI-1001-Q15 | P23, P33 |
| `LedgerAfterHeld` | Every source row, committed or pending, names a capture whose output row committed first. | S3 | R-N58, R-N86 | P17, P29 |
| `DoneAfterLedger` | `SourceDone` follows the ledger's last commit. | Durability | wire v5 (design.md) | P4, P23 |
| `ReuseSound` | Held under the seat's current identity means held with its current bytes (the racy rule). | S3, S5 | #86, R-N76 | P19 |
| `LedgerSound` | A ledger manifest under the current key is the seat's current content. | S3, S5 | #86, R-N58 | P17, P19 |
| `NoClobber` | Bulkload replaces or removes a destination file only when its identity is one this store recorded. | S4 | OI-1003-Q18 (WP0(d)), R-N119 | P7, P8, P26 |
| `S2_TypedSourceAccess` | Every source access is a stat, a content read, an allowlisted git read or the SQLite backup. No write, lock, lease or signal. | S2 | OI-1003-Q5, OI-1003-Q16 (WP0(b)) | P34 |
| `S2_BackupLockBounded` | The backup's lock is only ever shared, held only inside one step of a counted backup, and taken at most `max_steps` times. | S2 (its stated exception) | OI-1003-Q16 | P34; no dedicated test yet |
| `ClosureAccounted` | A finished session leaves every seat applied or with a typed refusal. A bare `IO` closes nothing. | S4 | OI-1003-Q1, #100 | P61 |
| `RunsClose`, `AllRunsFinish` | Under `WF_vars(Protocol)` and the liveness assumption, every started run closes and every run is made. | S4, S5 | OI-1003-Q2 | P28, P23 |

`TypeOK` is a sanity check. `WithinBudget` is the wall-clock bound.

How the S3 properties read WP0(c)'s first inequality, "source bytes read ≤
sizes of changed or racy seats":

- **A qualifier it needs.** As worded, the inequality omits a seat whose
  destination copy a third party removed or replaced. The code must read
  that seat even though its source is unchanged. The model counts it as "not
  held".
- **R25 is per seat.** A new seat whose chunks happen to sit in other outputs
  is still read once, to build its manifest. Cross-file deduplication saves
  wire bytes, not source reads.

## Mutations

Each negative config sets `Mutation` to break exactly one rule, and it must
fail on the one property named in `configs.tsv`, with `TypeOK` checked
alongside. Rows marked (core) run on `MC_nv_core`'s constants. The others use
the smallest bound that reaches the break.

| Mutation | What it breaks (the code it would undo) | Property that must fail |
|---|---|---|
| `held_before_commit` (core) | `Held{true}` before the group commit (`Inbound::answer_held`) | `HeldAfterCommit` |
| `commit_before_fsync` (core) | rename and row without the file seal (`StagedFile::publish`) | `RecordImpliesBytes` |
| `commit_before_dirseal` | row before the directory seal (`TouchedDevices::seal`) | `RecordImpliesBytes` |
| `adopt_without_seal` | adopted output recorded unsealed (`PublishSink::commit`) | `RecordImpliesBytes` |
| `ledger_before_held` | source row before `Held{true}` (`Outbound::handle`) | `LedgerAfterHeld` |
| `done_before_sync` | `SourceDone` before `committer.sync()` (`serve`) | `DoneAfterLedger` |
| `reread_durable` | no `Reuse` decision (`Inbound::entry`) | `R25_NoDurableReread`; also `S3_UnchangedReadsZero`, `S3_ReadsOnlyChanged` (three configs) |
| `skip_output_row` | the group commit records no output row (`commit_outputs`) | `S3_ClosedPassIsHeld` |
| `double_read` | retained chunks dropped, so the seat is read twice (#77 F1, `serve_chunks`) | `ReadOnce` |
| `src_ledger_carries_r25` (core) | `Reuse` also requires the source ledger's row | `R25_NoDurableReread` |
| `record_racy` | a racy capture recorded as a reuse key (#86, `send_capture`, `commit_outputs`) | `ReuseSound`; also `LedgerSound` (two configs) |
| `untyped_space` | a full-disk group reported as bare `IO` (#100, `space_refusal`) | `ClosureAccounted` |
| `source_write` | a capture writes the source | `S2_TypedSourceAccess` |
| `pause_writer` | a capture interrupts a source writer | `S2_TypedSourceAccess` |
| `git_optional_locks` | git without the optional-locks guard (`git_env`) | `S2_TypedSourceAccess` |
| `unbounded_backup` | the backup steps past `max_steps` (`provider_sqlite::snapshot`) | `S2_BackupLockBounded` |
| `supersede_unchecked` | WP0(d) exchange without the identity check | `NoClobber` |
| `sweep_displaced` | WP0(d) recovery deletes a displaced foreign file | `NoClobber` |

What the mutations showed:

- **`Reuse` and the source ledger are redundant on the happy path.**
  `reread_durable` alone does not break S3 under a strict ledger. A rerun that
  never answers `Reuse` still reads nothing: the source serves its ledger
  manifest, and the destination adopts. The mutation bites only after a crash
  between the destination commit and the source row. Across that window,
  `Reuse` alone carries R25.
- **The source ledger cannot carry R25.** `src_ledger_carries_r25` fails even
  with a strict source ledger, for the same reason.

## Not proven here

The model proves the protocol, within its bounds. It does not prove:

- **Code-level S2.** The model's typed access holds by construction: its
  actions are the code's actions, and a mutation can only add an access kind.
  That the binary issues no other syscall against the source (no write,
  lock, lease or signal) is P34's job, a traced run. So is the backup's
  possible touch of the source's `-shm` read marks inside the stated
  exception. Background priority (WP0(f)) is P35's job.
- **Chunking and credits.** FastCDC boundaries, BLAKE3, `manifest_root`,
  cross-file deduplication, `NeedChunks` beyond "none or some", credit flow,
  the 1024-entry window, walk-ahead and the retention budget. So WP0(c)'s
  second inequality (wire bytes ≤ absent chunks) is not proven here; P18
  covers it.
- **Git carry.** v1 bundles, carry_v2 (frozen by WP0(a)), the ingest journal,
  the git sub-stream and estate apply's `.done` journals. Only estate
  capture's typed reads (one git read, the SQLite backup) are modelled.
- **The tree.** Directories and their records (R-N102), symlinks, `Skip`,
  engine temporaries, walk caps and devices other than the store's.
- **Storage below the store.** The Darwin barrier model belongs to
  `crash_check` (R-N88: P13, P14). SQLite corruption, torn pages and
  reordered writes under `fullfsync=OFF` are outside the model (see the
  WP0(g) caveat).
- **Larger bounds.** The checked bounds are small (the small-scope
  hypothesis). The drafted two-seat, three-run, every-fault constants were
  only simulated (`MC_main_sim`), never model-checked.

## Code and design disagreements

The model follows the code where the code and docs/design.md differ:

- **WantManifest.** design.md says it is chosen when the output exists or
  published outputs hold chunks. `Inbound::entry` also chooses it when the
  sweep salvaged any temporary (`self.target.salvaged() > 0`).
- **Superseding publish (WP0(d)).** It is ratified but not implemented.
  `SupersedeMode = "off"` models the code.
- **WP0(g).** It is ratified conditionally and not implemented: both stores
  run `synchronous=FULL`, `fullfsync=ON` (`configure_sqlite`). The verdict
  above sets the condition.
- **"Sealed".** design.md calls a file sealed by `F_BARRIERFSYNC` on Darwin,
  which is a barrier, not a flush. See the seal abstraction above.

## Frozen names

The whitepaper and the property-test plan cite these names. They are frozen:
renaming one is a breaking change to the proof package and needs a ruling.

- Module: `BulkloadTransfer`. Specifications: `Spec`, `LiveSpec`.
- Safety invariants: `TypeOK`, `R25_NoDurableReread`,
  `R25_NoCommittedCaptureReread`, `ReadOnce`, `S3_ReadsOnlyChanged`,
  `S3_UnchangedReadsZero`, `S3_ClosedPassIsHeld`, `RecordImpliesBytes`,
  `HeldAfterCommit`, `LedgerAfterHeld`, `DoneAfterLedger`, `ReuseSound`,
  `LedgerSound`, `NoClobber`, `S2_TypedSourceAccess`,
  `S2_BackupLockBounded`, `ClosureAccounted`.
- Budget invariant: `WithinBudget`.
- Temporal properties: `RunsClose`, `AllRunsFinish`.
- The N-version core: `MC_nv_core`.
