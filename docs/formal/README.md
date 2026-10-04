# Formal model: wire v5, Held, group commits and resume

The proof package's formal model ([docs/slo.md](../slo.md), OI-1003-Q7): a
TLA+ specification of bulkload's transfer, model-checked with TLC. TLA+ and
TLC are the checker of record (OI-1003-Q32). Sprint 2 adds a typed Dhall
catalogue of the configs and a Haskell N-version explorer on [the shared
core](#n-version-core-oi-1003-q32) ([Hybrid roles](#hybrid-roles-oi-1003-q32)).
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
On 2026-10-04 `origin/main` reached `4a7b86b` (#146, #154). The symbols this
revision adds (`private_dir`, `LedgerSink::commit`, `Committer::submit` and
`sync`, `serve`'s `committer.sync()`) are present at both adb9c66 and
`4a7b86b`. #154's salvage and racy-guard changes are not modelled yet ([Code
and design disagreements](#code-and-design-disagreements)).

| File | What it is |
|---|---|
| [`BulkloadTransfer.tla`](BulkloadTransfer.tla) | The specification. Its header states the scope, the abstractions and the code map. Every action cites the function it models. |
| [`catalogue/Catalogue.dhall`](catalogue/Catalogue.dhall) | The typed catalogue: every config's constants and expectation, every mutation's verdict, and every property's traceability row. `just tla-render` renders every `MC_*.cfg` and `configs.tsv` from it. Edit the catalogue, never the outputs. It replaced `gen_cfgs.py` (OI-1003-Q32). |
| [`catalogue/Types.dhall`](catalogue/Types.dhall) | The catalogue's types. Union labels are the TLA+ names themselves. |
| `MC_*.cfg` | TLC configurations, rendered from the catalogue. |
| [`configs.tsv`](configs.tsv) | The run order. Columns: `name`, `expect`, `named-property` (the one property a fail or reach row must violate), `never` (a pass row's exact never-enabled actions), `flags` (extra TLC arguments, one argv element per word). |

## Running it

```sh
just tla-render             # re-render configs.tsv and MC_*.cfg from the catalogue
just tla-check              # every row of configs.tsv
just tla-check MC_nv_core   # the budget self-test, then the named configs
just formal-nv              # the Haskell N-version cross-check (Hybrid roles)
```

`tla-check` is a standalone recipe at the end of the justfile. No tier
depends on it: `check-fast`, `check-optional`, `check-full` and CI never start
TLC, so CI stays slim (OI-1003-Q7).

How the recipe runs:

- TLC 2.19 comes from the flake's pinned nixpkgs (`nix shell --inputs-from .
  nixpkgs#tlaplus`); there is no flake change. So do Dhall, dhall-json and
  jq for the catalogue.
- Before any TLC run, two catalogue checks must pass, or nothing runs:
  - **staleness**: the catalogue, rendered into scratch, equals the
    committed `configs.tsv` and `MC_*.cfg` byte for byte, with no file
    missing or extra;
  - **grounding**: every operator the catalogue names (properties,
    witnesses, the 37 actions, `Spec`, `LiveSpec`, `SeatSymmetry`, `Init`,
    `Next`) is defined in `BulkloadTransfer.tla`, every constant is
    declared, every mutation is in its `Mutations` set, and every code
    symbol is found by `git grep -w` under `crates/`.
- The rows of `configs.tsv` run in order, one JVM at a time, with `-Xmx4g`,
  `-workers 3`, `nice -n 10` and `-coverage 1`.
- TLC state and logs go under a private `mktemp -d` in `$TMPDIR`. It is
  removed when every row matches its expectation. Otherwise the logs stay,
  and their paths are printed.
- The first row is the budget self-test. Unless its log ends normally with
  `WithinBudget`, and nothing else, violated, the recipe stops before any
  other config, because every other config relies on that budget to end. A
  self-test JVM that dies or is ended from outside is ABORTED, never a proof
  of the budget.
- It prints one line per config: outcome, the properties the log reports
  violated, distinct and generated states, diameter (TLC's "depth of the
  complete state graph search"), wall time and peak RSS, then the actions
  that were never enabled. It ends with the total wall time and the peak RSS.

A config gets one of seven outcomes:

| Outcome | Meaning |
|---|---|
| PASS | Model checking finished with no error, and the actions its coverage shows never enabled are exactly the row's `never` column. |
| FAIL | The row's named property was violated, and nothing else was: no other invariant, no `TypeOK`, no deadlock. Only this counts as a caught mutant (a mutation row) or a finding (a finding row). |
| REACHED | The row's `Witness_` invariant, and nothing else, was violated: the scenario it names is reachable within that bound ([Coverage](#coverage)). |
| SIMULATION | `-simulate` finished its random behaviours with no error. This is evidence, never a model-checking result. |
| INCONCLUSIVE | The wall-clock budget tripped (`WithinBudget`). It never counts as a pass or a caught mutant. For the budget self-test it must be `WithinBudget` alone. |
| ABORTED | The log has no `Finished` line: TLC did not end normally (out of memory, ended from outside, a crash). It matches no expectation. |
| WRONG | Anything else, including a pass whose never-enabled actions differ from its `never` column. |

The recipe fails unless every row's outcome equals its `expect` column. Every
fail and reach config also checks `TypeOK`, so a type error shows up as WRONG
rather than hiding behind the named property. Deadlock checking stays on
everywhere: `Terminated` is the only place a behaviour may stop.

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
  budget and must come out INCONCLUSIVE: its log ends normally with
  `WithinBudget` alone violated. Its bound is `MC_main`'s, so even a broken
  budget would end (as a PASS, which the recipe rejects as WRONG). A JVM that
  dies is ABORTED and also stops the recipe.
- **No outside clock.** Nothing else ends a TLC run: no wrapper, no signal.

## Results

The run of record: one full `just tla-check` on host sting (32 cores,
Linux 6.12, TLC 2.19 on OpenJDK 8), 2026-10-04 01:30–01:38 EDT, at a load
average near 30 from other lanes, over the spec and configs committed in
`8bc6672`. Comment-only edits to the spec and one config's comment landed
in the worktree during the run, and are committed with this README; they
change no state or verdict. **All 43 rows matched their expectation: 10 PASS,
3 REACHED, 28 FAIL, 1 SIMULATION, 1 INCONCLUSIVE (the budget self-test),
0 ABORTED, 0 WRONG. Every pass row's never-enabled actions equalled its
`never` column. Total wall time 480 s; peak RSS 1,882 MiB.** Two earlier
partial runs the same night, on the same spec, covered every pass and
reach row. They gave the same verdicts and, for every pass row, the same
distinct and generated counts and diameters, with lower wall times at a
lighter load (`MC_main` 87 s against 117 s).

The 2026-10-03 run of record (35 rows over `3760263`, 379 s) is superseded.
Its `MC_wp0g` row used `MC_main`'s bound, which never read the relaxed
ledger ([WP0(g) verdict](#wp0g-verdict-oi-1003-q20)).

Sprint 2 changed only each config's provenance comment, when the catalogue
replaced `gen_cfgs.py` ([Hybrid roles](#hybrid-roles-oi-1003-q32)). A full
`just tla-check` over the re-rendered configs (2026-10-04, sting, load
average near 35) passed its staleness and grounding gates. It then matched
all 43 rows again: 10 PASS, 3 REACHED, 28 FAIL, 1 SIMULATION and
1 INCONCLUSIVE. Every pass row's distinct and generated counts and diameter
equalled the table's. Total wall time 529 s; peak RSS 1,865 MiB.

| Config | Constants | Expect | Verdict | Violated | Distinct | Generated | Diameter | Wall | RSS MiB |
|---|---|---|---|---|---:|---:|---:|---:|---:|
| `MC_budget_selftest` | {a,b} R2 C1 E1 sym budget 5 s | inconclusive | **INCONCLUSIVE** | `WithinBudget` | 4,785 | 15,753 | 9 | 8s | 589 |
| `MC_main` | {a,b} R2 C1 E1 sym budget 600 s | pass | **PASS** | – | 869,296 | 2,825,196 | 51 | 117s | 1882 |
| `MC_main_deep` | {a} R3 C2 E1 F1 X1 space budget 600 s | pass | **PASS** | – | 334,296 | 1,310,981 | 49 | 34s | 1742 |
| `MC_dest_faults` | {a,b} R2 C0 E0 F1 X1 space sym budget 600 s | pass | **PASS** | – | 181,785 | 504,315 | 52 | 22s | 1742 |
| `MC_nv_core` | {a} R3 C2 E1 sym budget 600 s | pass | **PASS** | – | 15,834 | 44,312 | 45 | 7s | 550 |
| `MC_nv_ledger` | {a} R3 C2 E1 F1 sym budget 600 s | pass | **PASS** | – | 142,450 | 497,089 | 49 | 25s | 1632 |
| `MC_wp0g` | {a,b} R3 C1 E0 F1 relaxed sym budget 600 s | pass | **PASS** | – | 496,830 | 1,430,743 | 74 | 64s | 1805 |
| `MC_wp0g_deep` | {a} R3 C2 E1 F1 X1 space relaxed budget 600 s | pass | **PASS** | – | 419,020 | 1,651,581 | 49 | 51s | 1712 |
| `MC_wp0d_exchange` | {a} R3 C1 E1 F1 exchange budget 600 s | pass | **PASS** | – | 70,086 | 186,977 | 51 | 8s | 1069 |
| `MC_s2` | {a} R2 C1 E1 estate budget 600 s | pass | **PASS** | – | 61,956 | 236,660 | 39 | 7s | 902 |
| `MC_live` | {a,b} R2 C0 E0 space sym budget 600 s | pass | **PASS** | – | 12,649 | 31,118 | 47 | 7s | 803 |
| `MC_main_sim` | {a,b} R3 C1 E1 F1 X1 space sym budget 600 s | simulate | **SIMULATION** | – | 1,147,363 | – | – | 36s | 1559 |
| `MC_reach_ledger_manifest` | {a} R3 C2 E1 F1 budget 600 s | reach | **REACHED** | `Witness_LedgerManifest` | 37,243 | 118,518 | 19 | 6s | 806 |
| `MC_reach_ledger_chunks` | {a} R3 C2 E1 F1 budget 600 s | reach | **REACHED** | `Witness_LedgerChunkRead` | 53,297 | 167,699 | 21 | 7s | 809 |
| `MC_reach_wp0g_lost_row` | {a,b} R3 C1 E0 F1 relaxed sym budget 600 s | reach | **REACHED** | `Witness_LostRowRead` | 52,882 | 184,715 | 19 | 11s | 1297 |
| `MC_wp0g_authority` | {a} R2 C1 E0 relaxed relaxed-auth budget 300 s | fail | **FAIL** | `R25_NoDurableReread` | 771 | 1,436 | 18 | 3s | 333 |
| `MC_store_root_unsealed` | {a} R2 C1 E0 unsealed-root budget 300 s | fail | **FAIL** | `R25_NoDurableReread` | 766 | 1,423 | 19 | 2s | 316 |
| `MC_r25_unrowed_bytes` | {a} R2 C1 E0 strict-held budget 300 s | fail | **FAIL** | `R25_StrictNoDurableReread` | 339 | 706 | 19 | 3s | 269 |
| `MC_wp0d_check_rename` | {a} R2 C0 E1 F1 check_rename budget 300 s | fail | **FAIL** | `NoClobber` | 6,146 | 11,771 | 27 | 3s | 508 |
| `MC_neg_live_unfair` | {a} R1 C0 E0 sym budget 300 s | fail | **FAIL** | `RunsClose` | 78 | 109 | – | 2s | 285 |
| `MC_neg_held_before_commit` | {a} R3 C2 E1 mut=held_before_commit budget 300 s | fail | **FAIL** | `HeldAfterCommit` | 276 | 669 | 7 | 3s | 263 |
| `MC_neg_commit_before_fsync` | {a} R3 C2 E1 mut=commit_before_fsync budget 300 s | fail | **FAIL** | `RecordImpliesBytes` | 867 | 2,302 | 10 | 2s | 321 |
| `MC_neg_commit_before_dirseal` | {a} R1 C0 E0 mut=commit_before_dirseal budget 300 s | fail | **FAIL** | `RecordImpliesBytes` | 83 | 118 | 16 | 2s | 246 |
| `MC_neg_adopt_without_seal` | {a} R1 C0 E0 F1 mut=adopt_without_seal budget 300 s | fail | **FAIL** | `RecordImpliesBytes` | 392 | 658 | 16 | 2s | 273 |
| `MC_neg_ledger_before_held` | {a} R1 C0 E0 mut=ledger_before_held budget 300 s | fail | **FAIL** | `LedgerAfterHeld` | 56 | 85 | 17 | 2s | 225 |
| `MC_neg_done_before_sync` | {a} R1 C0 E0 mut=done_before_sync budget 300 s | fail | **FAIL** | `DoneAfterLedger` | 80 | 117 | 17 | 2s | 221 |
| `MC_neg_reread_durable` | {a} R2 C1 E0 mut=reread_durable budget 300 s | fail | **FAIL** | `R25_NoDurableReread` | 407 | 865 | 25 | 2s | 272 |
| `MC_neg_reread_unchanged` | {a} R2 C1 E0 mut=reread_durable budget 300 s | fail | **FAIL** | `S3_UnchangedReadsZero` | 443 | 962 | 28 | 2s | 300 |
| `MC_neg_reread_changed_only` | {a} R2 C1 E0 mut=reread_durable budget 300 s | fail | **FAIL** | `S3_ReadsOnlyChanged` | 427 | 908 | 26 | 2s | 279 |
| `MC_neg_reread_ignore_ledger` | {a} R2 C0 E0 mut=reread_ignore_ledger budget 300 s | fail | **FAIL** | `R25_NoCommittedCaptureReread` | 119 | 162 | 31 | 2s | 283 |
| `MC_neg_reread_exchange` | {a} R2 C0 E0 exchange mut=reread_durable budget 300 s | fail | **FAIL** | `R25_NoCommittedCaptureReread` | 121 | 164 | 31 | 3s | 271 |
| `MC_neg_skip_output_row` | {a} R1 C0 E0 mut=skip_output_row budget 300 s | fail | **FAIL** | `S3_ClosedPassIsHeld` | 78 | 107 | 17 | 3s | 238 |
| `MC_neg_double_read` | {a} R1 C0 E0 mut=double_read budget 300 s | fail | **FAIL** | `ReadOnce` | 32 | 40 | 9 | 2s | 204 |
| `MC_neg_src_ledger_carries_r25` | {a} R3 C2 E1 mut=src_ledger_carries_r25 budget 300 s | fail | **FAIL** | `R25_NoDurableReread` | 3,555 | 9,271 | 15 | 3s | 489 |
| `MC_neg_record_racy` | {a} R1 C0 E1 mut=record_racy budget 300 s | fail | **FAIL** | `ReuseSound` | 320 | 483 | 12 | 2s | 297 |
| `MC_neg_record_racy_ledger` | {a} R1 C0 E1 mut=record_racy budget 300 s | fail | **FAIL** | `LedgerSound` | 433 | 659 | 15 | 2s | 289 |
| `MC_neg_untyped_space` | {a} R1 C0 E0 X1 mut=untyped_space budget 300 s | fail | **FAIL** | `ClosureAccounted` | 100 | 128 | 16 | 2s | 261 |
| `MC_neg_source_write` | {a} R1 C0 E0 mut=source_write budget 300 s | fail | **FAIL** | `S2_TypedSourceAccess` | 13 | 16 | 5 | 2s | 206 |
| `MC_neg_pause_writer` | {a} R1 C0 E0 mut=pause_writer budget 300 s | fail | **FAIL** | `S2_TypedSourceAccess` | 13 | 16 | 5 | 1s | 209 |
| `MC_neg_git_optional_locks` | {a} R1 C0 E0 estate mut=git_optional_locks budget 300 s | fail | **FAIL** | `S2_TypedSourceAccess` | 8 | 8 | 3 | 2s | 212 |
| `MC_neg_unbounded_backup` | {a} R1 C0 E0 estate mut=unbounded_backup budget 300 s | fail | **FAIL** | `S2_BackupLockBounded` | 356 | 904 | 10 | 2s | 299 |
| `MC_neg_supersede_unchecked` | {a} R1 C0 E0 F1 exchange mut=supersede_unchecked budget 300 s | fail | **FAIL** | `NoClobber` | 267 | 420 | 12 | 2s | 288 |
| `MC_neg_sweep_displaced` | {a} R3 C1 E1 F1 exchange mut=sweep_displaced budget 300 s | fail | **FAIL** | `NoClobber` | 37,428 | 94,195 | 28 | 7s | 734 |

Reading the table:

- **Constants.** Seats; `R` runs, `C` crashes, `E` source edits per seat;
  `F` third-party writes, `X` failed group commits; `space` the space
  refusal; `relaxed` relaxed ledger rows, `relaxed-auth` a relaxed
  store-creation commit; `unsealed-root` `StoreRootSealed = FALSE` (the code
  today); `strict-held` `TrackStrictHeld`; `exchange` or `check_rename` the
  WP0(d) design; `estate` estate capture's typed reads; `mut=` the mutation;
  `sym` `SYMMETRY` over seats; `budget` the `WithinBudget` seconds.
- **Distinct** is counted under symmetry where `sym` is shown. For
  `MC_main_sim` it is the number of states the simulation checked, not a
  distinct count. For `MC_budget_selftest` it is wherever the 5 s budget
  stopped the search, so it changes from run to run.
- **Fail and reach rows.** Their counts are where the search stopped at
  the first violation. With 3 workers that varies a little between runs;
  the verdict and the violated property do not. For these rows, "Diameter"
  is the depth reached when the search stopped; TLC prints none for a
  liveness counterexample.
- **Violated** lists every property the log reports violated. A FAIL or
  REACHED row shows exactly its named property, never `TypeOK` and never a
  deadlock.
- **Counts that moved on 2026-10-04.** `MC_wp0g` has a new bound.
  `MC_wp0g_deep` grew from 354,580 to 419,020 distinct, because the ghost
  `ledgerLost` splits states that differ only in which rows a crash dropped;
  its behaviour is unchanged. Every other positive count is the same as on
  2026-10-03.

Bounds:

- Constants are deliberately small (the small-scope hypothesis).
- Breadth comes from two seats (`MC_main`, `MC_dest_faults`, `MC_wp0g`,
  `MC_live`), with `SYMMETRY` over seats in the safety configs. `MC_wp0g`
  also has three runs and a third-party write, so that a run follows a
  relaxed-only loss and consults the ledger (WP0(g) verdict).
- Depth comes from one seat with three runs and two crashes (`MC_main_deep`,
  `MC_wp0g_deep`, `MC_nv_core`, `MC_nv_ledger`).
- No config needed reducing: every positive finishes in under 120 s at
  a load near 30 (`MC_main`, the slowest), far inside its 600 s budget.
- The constants first drafted for `MC_main` (two seats, three runs, every
  fault) appear only as `MC_main_sim`: a seeded simulation (`-simulate
  num=3000 -depth 120 -seed 20261003`), recorded as SIMULATION and never as
  a model-checking result.

## Coverage

Coverage is TLC's `-coverage` report, the last one in each log. An action is
*never enabled* in a config when it generated no state at all. `Next` has 37
actions (38 coverage entries with `Init`).

**Coverage is enforced.** Each pass row's `never` column in `configs.tsv` is
the exact set of actions its report must show never enabled. `tla-check`
fails the row (WRONG) on any difference, so neither a spec edit that disables
an action nor a bound that stops reaching one can pass unnoticed. Fail, reach
and simulation rows are not checked: a fail or reach row stops at its first
violation, and a simulation only samples.

| Pass row | Never enabled (besides the groups below) | Groups off |
|---|---|---|
| `MC_main` | `ForeignWrite`, `ForeignDelete`, `CommitFail` | WP0(d), estate |
| `MC_main_deep` | – | WP0(d), estate |
| `MC_dest_faults` | `Edit`, `SilentRewrite`, `RecvRefused`, `CrashSrc`, `CrashDst`, `CrashBoth` | WP0(d), estate |
| `MC_nv_core` | `ForeignWrite`, `ForeignDelete`, `CommitFail` | WP0(d), estate |
| `MC_nv_ledger` | `CommitFail` | WP0(d), estate |
| `MC_wp0g` | `Edit`, `SilentRewrite`, `RecvRefused`, `CommitFail` | WP0(d), estate |
| `MC_wp0g_deep` | – | WP0(d), estate |
| `MC_wp0d_exchange` | `CommitFail`, `CheckOwn`, `RenameReplace` | estate |
| `MC_s2` | `ForeignWrite`, `ForeignDelete`, `CommitFail` | WP0(d) |
| `MC_live` | `Edit`, `SilentRewrite`, `RecvRefused`, `CrashSrc`, `CrashDst`, `CrashBoth`, `ForeignWrite`, `ForeignDelete`, `CommitFail` | WP0(d), estate |

Each action is off because of that config's own constants, and every one is
enabled in another config:

| Never enabled | Why it is off | Enabled in |
|---|---|---|
| WP0(d): `Exchange`, `VerifyDisp` | `SupersedeMode = "off"`: the code has no superseding publish, and these model WP0(d)'s exchange design | `MC_wp0d_exchange`, `MC_neg_sweep_displaced` |
| WP0(d): `CheckOwn`, `RenameReplace` | `SupersedeMode` is not `"check_rename"`; these model WP0(d)'s rejected check-then-rename design | `MC_wp0d_check_rename`: its counterexample takes `CheckOwn` at state 25 and `RenameReplace` at state 27 |
| estate: `GitRead`, `BackupBegin`, `BackupStepLock`, `BackupStepUnlock`, `BackupEnd` | `EstateReads = FALSE`: estate capture's typed reads do not depend on the transfer, so S2 is checked on its own | `MC_s2` |
| `ForeignWrite`, `ForeignDelete` | `MaxForeign = 0` | `MC_main_deep`, `MC_dest_faults`, `MC_nv_ledger`, `MC_wp0g`, `MC_wp0g_deep`, `MC_wp0d_exchange` |
| `CommitFail` | `MaxCommitFails = 0` | `MC_main_deep`, `MC_dest_faults`, `MC_wp0g_deep` |
| `Edit`, `SilentRewrite`, `RecvRefused` | `MaxEdits = 0`: a refusal needs a changed source | every config with `E1` |
| `CrashSrc`, `CrashDst`, `CrashBoth` | `MaxCrashes = 0` | every config with `C1` or `C2` |

**Branches, not only actions.** Action-level coverage can hide a dead branch
inside an action. Three reach rows prove that the branches R25 and WP0(g)
depend on are reachable. Each runs at the bound of a pass row, so that pass
row's complete search explores the branch:

| Reach row | Witness | Branch | At the bound of |
|---|---|---|---|
| `MC_reach_ledger_manifest` | `Witness_LedgerManifest` | `RecvDecide`: a manifest served from the source ledger, with no read | `MC_nv_ledger` |
| `MC_reach_ledger_chunks` | `Witness_LedgerChunkRead` | `RecvNeed`: a ledger manifest's chunks re-read (pread) to fill an absent output | `MC_nv_ledger` |
| `MC_reach_wp0g_lost_row` | `Witness_LostRowRead` | WP0(g): a row a relaxed ledger lost, then a later run's ledger miss and read | `MC_wp0g` |

A witness invariant is used instead of line counts from the coverage report,
whose line numbers move with every spec edit. Neither ledger branch is
reachable without a third-party write or delete: the destination then answers
`Reuse` for every seat it holds, so the source never consults its ledger.
That is why `MC_main` and `MC_nv_core` never take them, and why
`MC_nv_ledger` and the new `MC_wp0g` bound exist.

**One branch is dead by design.** `RecvNeed`'s digest-mismatch branch (a
ledger manifest whose chunks no longer verify) is never taken in a positive
config, and cannot be. A ledger row under the current key always holds the
seat's current bytes (`LedgerSound`), and `RecvNeed` reads only under an
unchanged stat identity. The branch models the code's re-verification, which
could fire only if `LedgerSound` failed, as it does under `record_racy`.

Two actions are enabled yet add no new state in some configs:

- **`Terminated`** is the stuttering step at the end of a behaviour. It
  never adds a state; it stops a finished behaviour from counting as a
  deadlock.
- **`CrashBoth`** adds 0 distinct states in the strict configs (`MC_main`,
  `MC_main_deep`, `MC_nv_core`, `MC_nv_ledger`, `MC_s2`,
  `MC_wp0d_exchange`). It adds 3,874 distinct states in `MC_wp0g` and 10,681 in
  `MC_wp0g_deep`. Under a strict source ledger, a loopback power loss reaches
  only states that `CrashDst` reaches one step earlier, before the `Held`
  round trip. Only a relaxed ledger, which can lose rows at the source too,
  makes the double crash different. So the model's WP0(g) checks do exercise
  it.

A fail config's coverage covers only the part of the space it searched
before the counterexample.

## Hybrid roles (OI-1003-Q32)

The model is checked by three tools, each with one job:

| Role | Tool | What it does | Run |
|---|---|---|---|
| Checker of record | TLA+ with TLC 2.19 | `BulkloadTransfer.tla` is the model. Every verdict, count and counterexample this README cites as a result is TLC's. | `just tla-check` |
| Typed catalogue | Dhall 1.42 ([`catalogue/`](catalogue/)) | Holds every config's constants and expectation, every mutation's verdict and every property's traceability row. It renders `configs.tsv` and every `MC_*.cfg`, and its staleness and grounding checks gate every TLC run. | `just tla-render` |
| N-version cross-check | Haskell, GHC 9.10, base and containers ([`hs/Explorer.hs`](hs/Explorer.hs)) | An independent explicit-state BFS of the same transition relation on the core. It must reproduce TLC's counts and mutation verdicts. | `just formal-nv` |

All three come from the flake's pinned nixpkgs through `nix shell
--inputs-from`; there is no flake change. None of them is in `check-fast`,
`check-optional`, `check-full` or CI.

### The catalogue

What the catalogue guarantees, at the type level or by an `assert`, every
time it is evaluated (a catalogue that breaks one renders nothing):

- **Typed constants.** Each config's constants are a `Constants` record:
  `Seats` is a list of `< a | b >`, `SupersedeMode` is
  `< off | check_rename | exchange >`, and `Mutation` is an optional
  `Mutation`. A misspelt value does not type-check.
- **An expectation carries only what its kind needs.** A pass row carries
  its invariants, temporal properties and never-enabled actions. A fail row
  carries exactly one property, and a reach row exactly one witness, which
  is a separate type. The rendering adds `TypeOK` to fail and reach rows and
  `WithinBudget` to every row, so neither can be forgotten.
- **Every mutation has a verdict.** A total `merge` maps each `Mutation` to
  the property its `MC_neg_` row must violate. A mutation added to the union
  without a verdict is a type error. An assert also requires exactly one
  such primary row per mutation. A second row for the same mutation names
  its other property explicitly (`also`), as `MC_neg_reread_unchanged` and
  `MC_neg_record_racy_ledger` do.
- **`gen_cfgs.py`'s checks, kept.** Every safety invariant except `TypeOK`
  has a fail row, and no row with a temporal property uses `SYMMETRY`.
- **Names are the TLA+ names.** Union labels are rendered with
  `showConstructor`, so the catalogue cannot misspell a property, action or
  mutation. The frozen names are unchanged.
- **Traceability rows.** One `{tla, slo, ruling, codeSymbol, ptest}` row
  per frozen safety invariant except `TypeOK`, and per temporal property:
  the table under [Properties, SLOs, rulings and
  tests](#properties-slos-rulings-and-tests), with the code symbols each
  property is about.

What `tla-render` and `tla-check` add in the shell:

- The rendered file names are unique and safe (`MC_<name>.cfg` or
  `configs.tsv`).
- **Staleness.** The committed `configs.tsv` and `MC_*.cfg` equal the
  catalogue's rendering byte for byte, with no file missing or extra.
- **Grounding.** Every operator the catalogue names (21 properties, 3
  witnesses, 37 actions, `Spec`, `LiveSpec`, `SeatSymmetry`, `Init` and
  `Next`: 66 names) is defined in the spec. All 16 constants are declared,
  all 19 mutations are in its `Mutations` set, and all 19 distinct code
  symbols are found by `git grep -w` under `crates/`.

**It replaced `gen_cfgs.py` byte for byte.** At `0781bd6` the catalogue's
rendering, `gen_cfgs.py`'s output and the committed files were identical:
44 files, with the same sha256 manifest `259bd98c…f340` for all three. The
next commit changed only each file's provenance comment line (43 `\*`
lines and one `#` line) and deleted `gen_cfgs.py`. TLC ignores `\*`
comments and `tla-check` skips `#` lines, so no state or verdict moved.

### The explorer

`hs/Explorer.hs` is an explicit-state breadth-first search of the same
actions and invariants as the spec, on the core.

- **Independent implementation.** It is a separate program in another
  language, with another state representation (one record per seat instead
  of one function per variable). It reads no TLA+, no `.cfg` and no rendered
  file, and shares no code with the spec. It was written by hand from the
  spec's action definitions and the code they cite; nothing in it is
  generated from the TLA+ text. So it is an independent implementation,
  but not independent authorship: a misreading of the protocol shared by
  both texts would not show.
- **The core's actions only.** It has the 27 actions that TLC's coverage
  shows enabled in `MC_nv_ledger`: `StartRun`, the 13 per-seat protocol
  actions, `Commit`, `LedgerCommit`, `SendSourceDone`, `Finish`, `Edit`,
  `SilentRewrite`, `Tick`, `ForeignWrite`, `ForeignDelete`, the three
  crashes and `Terminated`. The other 10 are absent: `CommitFail`, WP0(d)'s
  four and estate capture's five. So are the space refusal, relaxed stores,
  an unsealed state root and the strict-held ghost. `NoClobber` and
  `S2_BackupLockBounded` are constantly true in the explorer, because the
  state they read does not exist in the core.
- **Presets:** `nv_core` (`MC_nv_core`) and `nv_ledger` (`MC_nv_ledger`).
  It supports the 14 mutations that need no absent action, and refuses the
  other 5 at the command line.
- **The same checks as TLC.** A state with no successor is a deadlock, as
  in TLC, and the search stops at the first state that violates a checked
  invariant. `formal-nv` checks every safety invariant on the presets, and
  `TypeOK` plus the row's named property on the mutations, as the TLC
  configs do. The recipe, not the explorer, reads that property from the
  row's `configs.tsv` line, which is TLC's expectation, and the counts to
  match are TLC's run of record.
- **Counterexamples are JSON.** One file per violation: the row, the
  mutation, the invariants checked and violated, any other invariant false
  in the last state, and the trace. Each state uses the spec's variable
  names and value spellings, leaving out the variables the core holds
  constant. `formal-nv` keeps them under a private `mktemp` directory in
  `$TMPDIR` and prints its path.

Parity, 2026-10-04, host sting, explorer built with `ghc -O1`. TLC's
verdicts and positive counts are the run of record above. Its
counterexample lengths come from one-worker hand runs of the three
mutation configs on this branch:

| Row | TLC | Explorer | Match |
|---|---|---|---|
| `MC_nv_core` | PASS: 15,834 distinct, 44,312 generated, diameter 45 | pass: 15,834 distinct, 44,312 generated, 45 levels | yes |
| `MC_nv_ledger` | PASS: 142,450 distinct, 497,089 generated, diameter 49 | pass: 142,450 distinct, 497,089 generated, 49 levels | yes |
| `MC_neg_held_before_commit` | FAIL `HeldAfterCommit`, 7-state counterexample | violation `HeldAfterCommit`, 7-state counterexample | yes |
| `MC_neg_commit_before_fsync` | FAIL `RecordImpliesBytes`, 9 states | violation `RecordImpliesBytes`, 9 states | yes |
| `MC_neg_src_ledger_carries_r25` | FAIL `R25_NoDurableReread`, 15 states | violation `R25_NoDurableReread`, 15 states | yes |

What the parity shows:

- **The positive counts match exactly, and so do the generated counts and
  the depths.** The cross-check needs only the distinct count. A matching
  generated count also means that both compute the same total number of
  successors over the reachable states, counted as TLC counts them: the
  initial states plus every successor computed, `Terminated`'s stuttering
  step included. `MC_nv_ledger` reaches both ledger branches, so its
  counts cover the ledger-manifest and ledger chunk-read semantics too.
- **The counterexamples are the same behaviours.** With one worker
  (`-workers 1`), TLC's counterexamples have the same length and the same
  action sequence as the explorer's:
  - `held_before_commit`: run 1 sends and stages seat `a`, then answers
    `Held{true}` before any commit.
  - `commit_before_fsync`: run 1 publishes the unsealed temporary,
    seals the directory and commits its row.
  - `src_ledger_carries_r25`: run 1 commits the output, and the source
    crashes before `Held`. Run 2 is refused `Reuse`, because the source
    ledger has no row, and reads the seat again.
- **Where a fail search stops is not a cross-check.** The distinct count at
  the first violation depends on the order within a BFS level: 179, 549 and
  3,381 for the explorer; 196, 589 and 3,501 for TLC with one worker; and
  209, 612 and 3,541 with three workers in `tla-check`.
- **With every invariant checked**, `src_ledger_carries_r25`'s shortest
  counterexample also violates `S3_ReadsOnlyChanged` and
  `S3_UnchangedReadsZero` in the same state: the seat it reads again was
  held when the run began and is unchanged. The other two mutations violate
  only their verdicts.

Runtime on sting at a load average near 45: the build takes about 26 s;
`MC_nv_core` takes 0.7 s and `MC_nv_ledger` 7.6 s. Under `runghc`,
without a build, `MC_nv_core` takes 17 s.

## N-version core (OI-1003-Q32)

The model is a hybrid. A second, independent explorer, sprint 2's Haskell
BFS, must reproduce TLC's count on a shared core. The core is `MC_nv_core`:

- one seat, three runs, two crashes, one source edit;
- third-party writes, commit failures, the space refusal, superseding
  publish and estate reads all off;
- no `SYMMETRY`, so the count is the plain state graph's.

`MC_nv_core` never consults the source ledger: with no third-party write the
destination answers `Reuse` for every seat it holds. So a second explorer
could get the ledger-manifest and ledger chunk-read semantics wrong and
still match it. `MC_nv_ledger` is a second core row: `MC_nv_core` plus one
third-party write or delete. Both ledger branches are reachable at its bound
(`MC_reach_ledger_manifest`, `MC_reach_ledger_chunks`). The Haskell
explorer matches both counts exactly ([Hybrid roles](#hybrid-roles-oi-1003-q32)).
Freezing `MC_nv_ledger` as a core name needs a ruling.

| Config | Initial states | Generated | Distinct | Diameter | Max out-degree |
|---|---:|---:|---:|---:|---:|
| `MC_nv_core` | 2 | 44,312 | **15,834** | 45 | 6 |
| `MC_nv_ledger` | 2 | 497,089 | **142,450** | 49 | 9 |

The distinct count of a completed breadth-first search is the reachable set,
so it does not depend on the number of workers. `MC_nv_core` counted 15,834
with 3 workers in every run on 2026-10-03 and 2026-10-04, and the duplicate
copies reported the same count with 1 and 4 workers. This revision's spec
changes leave it unchanged. The new ghost `ledgerLost` stays empty under a
strict ledger. Unless `TrackStrictHeld` is set, the new field of an output
record is always FALSE, and the new field of a read record equals `held`.

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

**Q20 holds for the ledger's row commits, on one condition: the source
store's creation must be durable before `Start`. That means both the commit
that creates its authority and its state root's directory entry. The model
also assumes a ledger commit never fails, which the code does not honour yet
(the conditions below).**

**1. Relaxed ledger rows are safe** (`MC_wp0g`, `MC_wp0g_deep` pass, and
`MC_reach_wp0g_lost_row` shows `MC_wp0g` explores the relaxed-only path).
The model lets a source power loss drop any subset of the committed
`captures` rows. Every safety property still holds.

The evidence, and what each piece adds:

- **`MC_wp0g`, breadth.** Two seats, three runs, one crash, one third-party
  write or delete, relaxed rows, `SYMMETRY`.
- **`MC_reach_wp0g_lost_row`, at the same bound.** It reaches
  `Witness_LostRowRead`: a crash drops a committed row, and a later run asks
  the ledger for that seat, misses, and reads the seat. A strict ledger never
  loses a row (`ledgerLost` stays empty), so a strict config cannot reach
  this. It is exactly the behaviour `MC_wp0g`'s PASS has to cover.
- **`MC_wp0g_deep`, depth.** One seat, three runs, two crashes and every
  destination fault.
- **The previous `MC_wp0g` bound was no evidence.** It used `MC_main`'s
  bound: two seats, two runs, no third-party write. Its coverage equalled
  `MC_main`'s for every protocol action, and the ledger was never read
  (review, 2026-10-04). With no third-party write the destination answers
  `Reuse` for every seat it holds, so the source never consults its ledger.
  The new bound replaces it.

R25 is carried by the destination:

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

**3. The code today has the same exposure under strict settings**
(`MC_store_root_unsealed` fails `R25_NoDurableReread`, and only it, with a
15-state counterexample). In it, run 1 sends, seals and publishes seat `a`.
The source then loses power and its unsealed store with it, and the
destination's dropped `Committer` seals the directory and commits the row.
Run 2's `Store::open` mints authority 2, and the source reads `a`, whose
unchanged bytes the destination holds durably.

- `private_dir` (`A/transfer_store.rs`) creates the state root with
  `DirBuilder::create` and never seals its parent directory.
- SQLite's directory sync covers only the entries inside the state root.
- `crash_check` does not model a store's own files. Under its rule 1, a mkdir
  with no later sync of its parent may be lost.

So a first serve on a fresh state root can commit its authority with
`synchronous=FULL`, send `Start`, and have the destination commit rows under
it. A source power loss can then still lose the store, because the parent's
entry for the new directory was never synced. The next `Store::open` mints a
new authority, every key changes, and every seat the destination holds
durably is read again.

On mainstream journaling filesystems an fsync usually persists the earlier
mkdir as well, but neither this model nor `crash_check` proves it. The
positive configs assume a sealed root (`StoreRootSealed = TRUE`).

The destination store has the same gap. The model cannot express losing it:
`HeldPhys` needs one of its rows, so a re-read after that loss would be
invisible to `R25_NoDurableReread`. It is an assumption (spec header,
ABSTRACTIONS).

**Conditions for adopting WP0(g):**

- Seal the state root's parent directory after `private_dir` creates it, and
  seal the state root after `transfer.sqlite` is created, before
  `Store::open` returns (so before `Start`). Add a `crash_check` trace over
  `Store::open`. This holds for both stores, and the code needs it even
  without WP0(g) (`MC_store_root_unsealed`). It is a code change outside this
  lane.
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
- Log and count a failed ledger commit, and never fail the transfer on it.
  R25 is carried by the destination, so a lost ledger write must cost at most
  a re-read. Today the first failed ledger group is sticky
  (`LedgerSink::commit` drops every later capture). `Committer::submit`, and
  `serve`'s `committer.sync()` before `SourceDone`, then fail the session, so
  the run never closes, and every rerun fails the same way while the source
  state disk stays full. The model's `LedgerCommit` never fails, so its
  liveness and closure results do not cover this ([Not proven
  here](#not-proven-here)).

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
| `StartRun` | `A/transfer.rs` `receive`, `serve`; `A/materialize.rs` `Destination::sweep_root`; `A/transfer_store.rs` `Store::open`, `private_dir`, `Store::authority`, `row_key` |
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
  whole store. Under `StoreRootSealed = FALSE` (the code today) a strict
  store can also be lost whole by a crash, since its state root's entry was
  never sealed. Positive configs assume it sealed, and assume the
  destination store's root durable.
- **Ledger commits never fail**, unlike the code ([Not proven
  here](#not-proven-here)).
- **"Held" is a committed destination row** (`HeldPhys`); see [Code and
  design disagreements](#code-and-design-disagreements).

## Properties, SLOs, rulings and tests

The P-numbers are the
[property-test plan](../plans/2026-10-03-property-test-plan.md)'s properties
that test the same claim on the real code. SLOs are
[docs/slo.md](../slo.md)'s S1–S5.

| Model property | Statement | SLO | Rulings | Property tests |
|---|---|---|---|---|
| `R25_NoDurableReread` | No source content read of a seat at a stat identity the destination holds durably. "Holds" means a committed row, recorded from that identity, vouches for the durable output at the path. Stated physically, under any source authority. **The operative R25 check in code shape.** "Held" is narrowed to "a committed row"; no ruling fixes that reading yet ([disagreements](#code-and-design-disagreements)). | S3 | R25 / R-N58, OI-1003-Q7, OI-1003-Q20 | P23, P21, P19, P24, P33 |
| `R25_NoCommittedCaptureReread` | slo.md's wording: no committed capture (a source row whose output the destination still holds) is re-read. **Vacuous while `SupersedeMode = "off"` (the code today)**: the source reads with its ledger row present only to serve chunks for an absent output, so `reread_durable` (no `Reuse`) alone never violates it. It fails only when the source also ignores its ledger (`MC_neg_reread_ignore_ledger`) or under the exchange design (`MC_neg_reread_exchange`). Not evidence for R25 in code shape. | S3 | R-N58, OI-1003-Q7 | P23, P33 |
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

Not code-shape invariants, never checked by a pass row, and not frozen:

| Property | What it is for |
|---|---|
| `R25_StrictNoDurableReread` | R25 under the strict reading of "held durably" (OI-1002-Q33): bulkload's own output from a non-racy capture, durable at the final path with the seat's current bytes, counts as held whether or not a row records it. Meaningful only under `TrackStrictHeld`. The code fails it (`MC_r25_unrowed_bytes`), which shows the gap between `R25_NoDurableReread` and the strict reading. |
| `Witness_LedgerManifest`, `Witness_LedgerChunkRead`, `Witness_LostRowRead` | Reachability witnesses ([Coverage](#coverage)): each says a scenario never happens, and its reach row must violate it. |

docs/slo.md states R25's model obligation as "no committed capture is
re-read". That wording is `R25_NoCommittedCaptureReread`, which is weaker
than `R25_NoDurableReread` and vacuous in code shape. So slo.md's wording
should be raised for a ruling (an open question for the operator), and the
whitepaper should cite `R25_NoDurableReread` as the R25 result.

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
| `reread_durable` | no `Reuse` decision (`Inbound::entry`) | `R25_NoDurableReread`; also `S3_UnchangedReadsZero`, `S3_ReadsOnlyChanged`; and, under the exchange design only, `R25_NoCommittedCaptureReread` (`MC_neg_reread_exchange`) (four configs) |
| `reread_ignore_ledger` | no `Reuse` decision, and `manifest_capture` ignores the source ledger | `R25_NoCommittedCaptureReread` |
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
- **slo.md's R25 wording is vacuous in code shape.** `reread_durable` against
  only `R25_NoCommittedCaptureReread` passes, both at
  `MC_neg_reread_durable`'s bound and at `MC_main_deep`'s (review,
  2026-10-04). It takes `reread_ignore_ledger` (both protections removed), or
  the exchange design, to violate it. The catalogue asserts that every
  safety invariant except `TypeOK` has at least one fail row.

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
- **A failing source ledger commit.** `LedgerCommit` never fails. In the code
  the first failed ledger group (a full or failing source state disk) is
  sticky, and the session fails before `SourceDone` (`LedgerSink::commit`,
  `Committer::submit`, `serve`). So `RunsClose`, `AllRunsFinish`,
  `ClosureAccounted` and WP0(g)'s "losing a source row costs at most a
  re-read" do not cover it. In the code today a lost ledger write costs the
  whole session (WP0(g) conditions).
- **Losing a store's state root.** The positive configs assume each store's
  state root and database file are durable once its first commit returns
  (`StoreRootSealed = TRUE`; the code does not seal the parent yet). Losing
  the destination store is not expressible here at all.
- **R25 under the strict reading.** `R25_NoDurableReread` counts bytes as held
  only through a committed destination row. Durable bytes with no row are
  read again, and the model reports no R25 violation:
  - a crash after `StagedFile::publish` and `TouchedDevices::seal` but before
    `commit_outputs`;
  - a failed group whose files were already renamed;
  - a sealed salvaged temporary, which the model folds away.

  `MC_r25_unrowed_bytes` makes this visible under the strict reading. Its
  13-state counterexample: run 1 seals and publishes seat `a`. The
  destination then loses power before the directory seal and the commit,
  and the rename survives, so the bytes are durable at the final path with
  no row. Run 2 reads `a` again.

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
- **The state root is never sealed** (`MC_store_root_unsealed`).
  `private_dir` creates each store's state root and never seals its parent.
  The positive configs assume it is sealed (WP0(g), point 3).
- **What "held durably" means for R25.** The model reads "held" as "a
  committed destination row" (`HeldPhys`). design.md's `Held` paragraph and
  OI-1001-Q15 ("a committed capture is never read again") fit that reading,
  but no ruling defines it:
  - #124 asked the operator to ratify exactly this reading of R25 clause 1.
    The answer, OI-1002-Q33 (docs/slo.md), keeps bounded salvage for
    refusals that touched bytes and says "R25 stays strict for anything the
    destination held durably". It does not say whether bytes with no row
    count as held.
  - #154 implemented that ruling and closed #124 on 2026-10-04.

  Under the strict reading the code re-reads durable bytes that have no row
  (`MC_r25_unrowed_bytes`; [Not proven here](#not-proven-here)). Until a
  ruling settles it, every R25 result here is for the narrowed reading.
- **PR #154 merged on 2026-10-04** (`4a7b86b`, after the code this model
  describes; `fix(transfer): bounded salvage …; distrust pre-racy-guard
  rows`). It implements OI-1003-Q24 (salvage bounds: 1024 temporaries and
  4 GiB) and OI-1003-Q26 (a pre-guard store is invalidated in place). Both
  rulings are recorded on the coordinator branch (978446734), not yet in
  slo.md. The model has **not** been re-checked against it. The re-check
  needs:
  - `Store::open` will delete every `captures` and `outputs` row when the
    `racy_guard` marker is missing. That is a durable effect at session start
    which `StartRun` omits.
  - `SALVAGE_BOUND_EXCEEDED` is a new typed refusal, missing from
    `TypedCodes`.
- **A failed ledger commit fails the session.** slo.md's WP0(g) says "losing
  a source row costs at most a re-read". In the code a failed ledger commit
  fails the whole session ([Not proven here](#not-proven-here)).
- **R25's model obligation in slo.md** is worded as
  `R25_NoCommittedCaptureReread`, which is vacuous in code shape
  ([Properties](#properties-slos-rulings-and-tests)).

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

The names this revision adds are not frozen: `R25_StrictNoDurableReread`,
the `Witness_` invariants, the constants `StoreRootSealed` and
`TrackStrictHeld`, the ghost `ledgerLost`, and the configs `MC_nv_ledger`,
`MC_reach_*`, `MC_store_root_unsealed`, `MC_r25_unrowed_bytes`,
`MC_neg_reread_ignore_ledger` and `MC_neg_reread_exchange`. Freezing
`MC_nv_ledger` as the second N-version row needs a ruling.
