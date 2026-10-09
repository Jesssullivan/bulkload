# Formal model: wire v5, Held, group commits and resume; git carry custody

The proof package's formal model ([docs/slo.md](../slo.md), OI-1003-Q7): a
TLA+ specification of bulkload's transfer, model-checked with TLC. TLA+ and
TLC are the checker of record (OI-1003-Q32). Sprint 2 adds a typed Dhall
catalogue of the configs and a Haskell N-version explorer, a second encoding
of the spec, on [the shared core](#n-version-core-oi-1003-q32) ([Hybrid
roles](#hybrid-roles-oi-1003-q32)). The Q42 push adds a second module,
[`GitCarry.tla`](#gitcarry-chain-and-base-custody-oi-1003-q43-oi-1003-q46):
git carry v1's chain and base custody, beside a Haskell reference copy of
the capture's decision core whose pinned rows the Rust code is to be checked
against (OI-1003-Q43).
The transfer model (`BulkloadTransfer.tla`) covers:

- wire v5 per entry;
- the destination's staging, seal, no-replace publish, directory seal and
  SQLite group commit;
- `Held` and the source's digest-only capture ledger;
- the source store's authority, which is part of every row key;
- crashes of either host or both, and the rerun that follows;
- S2's typed source access;
- the WP0(d) and WP0(g) rulings;
- the destination store's records of the superseding publish (#187,
  OI-1003-Q100 to Q102): the intent and its sweep, the ownership row, the
  remembered refusal, and the refusal where there is no atomic exchange
  ([#187's records](#187s-records-in-the-model-oi-1003-q102)).

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
| [`catalogue/Catalogue.dhall`](catalogue/Catalogue.dhall) | The typed catalogue: every config's constants and expectation, every mutation's verdict, and every property's traceability row. `just tla-render` renders every `MC_*.cfg`, `configs.tsv`, `configs_gc.tsv` and `configs_sq.tsv` from it (GitCarry's part is [`catalogue/GitCarry.dhall`](catalogue/GitCarry.dhall), SqliteCarry's [`catalogue/SqliteCarry.dhall`](catalogue/SqliteCarry.dhall)). Edit the catalogue, never the outputs. It replaced `gen_cfgs.py` (OI-1003-Q32). |
| [`catalogue/Types.dhall`](catalogue/Types.dhall) | The catalogue's types. Union labels are the TLA+ names themselves. |
| `MC_*.cfg` | TLC configurations, rendered from the catalogue. |
| [`configs.tsv`](configs.tsv) | The run order. Columns: `name`, `expect`, `named-property` (the one property a fail or reach row must violate), `never` (a pass row's exact never-enabled actions), `flags` (extra TLC arguments, one argv element per word). |
| [`GitCarry.tla`](GitCarry.tla) | The git carry custody module (OI-1003-Q43): chain links, the plan base, depth, Q46's re-root and GC, crash order and restore-or-recapture ([GitCarry](#gitcarry-chain-and-base-custody-oi-1003-q43-oi-1003-q46)). |
| [`configs_gc.tsv`](configs_gc.tsv), `MC_gc_*.cfg` | GitCarry.tla's run order and configs, in the same format, rendered from [`catalogue/GitCarry.dhall`](catalogue/GitCarry.dhall). |
| [`catalogue/Lib.dhall`](catalogue/Lib.dhall) | The catalogue's list and text helpers, shared by both modules. |
| [`SqliteCarry.tla`](SqliteCarry.tla) | The SQLite snapshot seat module (#218): the stepped backup, the settled key, sidecars, root and owner refusals, and OI-1003-Q146's superseding publish of a changed store's snapshot ([SqliteCarry](#sqlitecarry-the-sqlite-snapshot-seat-218-oi-1003-q146)). |
| [`configs_sq.tsv`](configs_sq.tsv), `MC_sq_*.cfg` | SqliteCarry.tla's run order and configs, rendered from [`catalogue/SqliteCarry.dhall`](catalogue/SqliteCarry.dhall). |
| [`hs/GitCarryCore.hs`](hs/GitCarryCore.hs) | The reference decision core `decide`, its pinned rows ([`decide_rows.tsv`](../../crates/bulkload-agent/tests/data/decide_rows.tsv)), and an explorer of GitCarry.tla. |

## Running it

```sh
just tla-render             # re-render configs.tsv, configs_gc.tsv and MC_*.cfg from the catalogue
just tla-render --check     # fail unless the committed files are the catalogue's rendering
just tla-check              # every row of configs.tsv, then of configs_gc.tsv
just tla-check MC_nv_core   # that module's budget self-test, then the named configs
just tla-check MC_gc_core   # the same for GitCarry.tla
just formal-nv              # the Haskell cross-checks of both modules (Hybrid roles)
```

`tla-check` and `formal-nv` are standalone recipes at the end of the
justfile. No tier depends on them: `check-fast`, `check-optional`,
`check-full` and CI never start TLC or GHC, so CI stays slim (OI-1003-Q7).
`tla-check` runs each module's rows against that module, after its own
budget self-test; a module none of the named configs belongs to is skipped.

How the recipe runs:

- TLC 2.19 comes from the flake's pinned nixpkgs (`nix shell --inputs-from .
  nixpkgs#tlaplus`); there is no flake change. So do Dhall, dhall-json and
  jq for the catalogue.
- Before any TLC run, two catalogue checks must pass, or nothing runs:
  - **staleness**: the catalogue, rendered into scratch, equals the
    committed `configs.tsv` and `MC_*.cfg` byte for byte, with no file
    missing or extra;
  - **grounding**: every operator the catalogue names (properties,
    witnesses, the 38 actions, `Spec`, `LiveSpec`, `SeatSymmetry`, `Init`,
    `Next`) is defined in `BulkloadTransfer.tla`; the catalogue's
    constants are exactly the spec's `CONSTANTS`, and its mutations exactly
    the spec's `Mutations` set (`"none"` aside), each checked in both
    directions; and every code symbol is in the Rust code under `crates/`
    (outside `tests/`, never a data file or a comment; per module below).
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

**The table below is one run: `just tla-check` over both modules on host
sting (32 cores, Linux 6.12, TLC 2.19), 2026-10-07 08:41 to 09:01 EDT for
BulkloadTransfer's rows, at a load average near 30 from other lanes, over
the spec, catalogue and configs of the #187 model extension (OI-1003-Q102;
the commit that adds `BeginSupersede` and `MC_supersede_noexchange`;
nothing under `docs/formal` but this README and a comment in
`hs/Explorer.hs` changed between the run and that commit). All 70 rows
matched their expectation: 22 PASS, 8 REACHED, 38 FAIL, 1 SIMULATION,
1 INCONCLUSIVE (the budget self-test), 0 ABORTED, 0 WRONG. Every pass
row's never-enabled actions equalled its `never` column. No row needed a
second run. The rows' wall times sum to 1,155 s; peak RSS 2,127 MiB.**

The whole recipe ran once more after the merge of main `8006085` (#202,
which changed the justfile and nothing under `docs/formal`), 2026-10-07
09:52 to 10:10 EDT for BulkloadTransfer's rows, at a load average near 17.
All 70 rows matched again, with no row run twice, and every pass row's
distinct and generated counts and diameter equalled the table's. Fail and
reach rows stop at their first violation with 3 workers, so their counts
differed a little, as they do between any two runs.

What moved against the run before it (56 rows, 2026-10-06 23:38 to 23:54
EDT, the #186/#187 revision, 881 s, peak RSS 1,976 MiB; superseded):

- **No row without the superseding publish moved.** Every pass row with
  `SupersedeMode = "off"` has the same distinct and generated counts and
  diameter as before: the extension's variables keep their initial values
  there. Their `never` columns gained `BeginSupersede`.
- **The three exchange pass rows grew**, because the intent, the ownership
  row and the remembered refusal split states: `MC_wp0d_exchange` from
  88,569 to 146,058 distinct, `MC_supersede_main` from 1,072,654 to
  1,664,264, `MC_supersede_deep` from 506,397 to 792,260.
- **14 rows are new**: three pass rows (`MC_supersede_strict_main`,
  `MC_supersede_strict_deep`, `MC_supersede_noexchange`), five reach rows
  and six mutation rows ([#187's
  records](#187s-records-in-the-model-oi-1003-q102)).
- **The seeded simulation checked 1,144,601 states**, against 1,144,513
  before. `Next` gained a disjunct, and TLC's simulator appears to draw
  over the disjuncts, so the same seed walks other behaviours. That
  reading is not verified; the row's verdict is SIMULATION either way.

GitCarry.tla and its configs are untouched; its rows ran after these and
are not re-tabled here ([Results (GitCarry)](#results-gitcarry)).

The 2026-10-06 run of record (54 rows over `8714c61`, the #169 review
revision, 18:27 to 18:37 EDT, 558 s, peak RSS 2,003 MiB) is superseded
too. The runs described next are the history before #169; their counts are
no longer the table's.

The first run of record: one full `just tla-check` on host sting (32 cores,
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

The Q42 lane L4 run (2026-10-04 16:42 to 16:58 EDT, sting, over `8bb9921`)
ran both modules after the recipe gained a second one. The 44 rendered
BulkloadTransfer files are byte-identical to `8dc26c1`'s, and this spec is
unchanged. All 43 rows matched again, and every pass row's distinct and
generated counts and diameter equalled the table's ([Results
(GitCarry)](#results-gitcarry)).

The current table is from the WP0(g) merge run (2026-10-07 17:05 to 17:48
EDT, sting, load average near 40 from other lanes). It covers WP0(g)'s
branch merged with main `11ff019`, which brought #203's superseding-publish
model round (`BeginSupersede`, the ownership and sweep rows). One full
`just tla-check` over both modules passed the staleness and grounding gates,
and **all 93 rows matched their expectation: 33 PASS, 12 REACHED, 46 FAIL,
2 INCONCLUSIVE (the budget self-tests). Total wall time 2,561 s; peak RSS
2,111 MiB.** Every row below carries that run's counts. The log is
`tla-check-wp0g-merge-20261007.log` in the coordinator's worktree root;
the counts are copied here.

| Config | Constants | Expect | Verdict | Violated | Distinct | Generated | Diameter | Wall | RSS MiB |
|---|---|---|---|---|---:|---:|---:|---:|---:|
| `MC_budget_selftest` | {a,b} R2 C1 E1 sym budget 5 s | inconclusive | **INCONCLUSIVE** | `WithinBudget` | 22,047 | 75,081 | 12 | 7s | 640 |
| `MC_main` | {a,b} R2 C1 E1 sym budget 600 s | pass | **PASS** | – | 963,314 | 3,080,708 | 51 | 100s | 1949 |
| `MC_main_deep` | {a} R3 C2 E1 F1 X1 space budget 600 s | pass | **PASS** | – | 457,464 | 1,787,195 | 49 | 43s | 1812 |
| `MC_dest_faults` | {a,b} R2 C0 E0 F1 X1 space sym budget 600 s | pass | **PASS** | – | 216,196 | 597,125 | 52 | 22s | 1792 |
| `MC_nv_core` | {a} R3 C2 E1 pre-#169 budget 600 s | pass | **PASS** | – | 15,834 | 44,312 | 45 | 4s | 569 |
| `MC_nv_ledger` | {a} R3 C2 E1 F1 pre-#169 budget 600 s | pass | **PASS** | – | 142,450 | 497,089 | 49 | 19s | 1667 |
| `MC_nv_core_adopt` | {a} R3 C2 E1 budget 600 s | pass | **PASS** | – | 17,027 | 47,053 | 45 | 5s | 581 |
| `MC_nv_ledger_adopt` | {a} R3 C2 E1 F1 budget 600 s | pass | **PASS** | – | 185,852 | 644,493 | 49 | 20s | 1771 |
| `MC_wp0g` | {a,b} R3 C1 E0 F1 relaxed sym budget 600 s | pass | **PASS** | – | 945,031 | 2,586,810 | 75 | 117s | 1983 |
| `MC_wp0g_deep` | {a} R3 C2 E1 F1 X1 space relaxed budget 600 s | pass | **PASS** | – | 711,661 | 2,776,377 | 49 | 73s | 1835 |
| `MC_wp0g_strict` | {a} R3 C2 E1 F1 X1 space relaxed strict-held budget 600 s | pass | **PASS** | – | 717,633 | 2,794,980 | 49 | 77s | 1889 |
| `MC_wp0d_exchange` | {a} R3 C1 E1 F1 exchange budget 600 s | pass | **PASS** | – | 146,058 | 366,446 | 55 | 20s | 1672 |
| `MC_supersede_main` | {a,b} R2 C1 E1 exchange sym budget 600 s | pass | **PASS** | – | 1,664,264 | 5,212,679 | 60 | 199s | 2111 |
| `MC_supersede_deep` | {a} R3 C2 E1 F1 X1 space exchange budget 600 s | pass | **PASS** | – | 792,260 | 2,970,789 | 55 | 78s | 1853 |
| `MC_supersede_strict_main` | {a,b} R2 C1 E1 exchange strict-held sym budget 600 s | pass | **PASS** | – | 1,664,264 | 5,212,679 | 60 | 187s | 2095 |
| `MC_supersede_strict_deep` | {a} R3 C2 E1 F1 X1 space exchange strict-held budget 600 s | pass | **PASS** | – | 797,105 | 2,985,041 | 55 | 79s | 1878 |
| `MC_supersede_noexchange` | {a} R3 C1 E1 F1 exchange no-exchange budget 600 s | pass | **PASS** | – | 105,398 | 263,597 | 49 | 12s | 1260 |
| `MC_s2` | {a} R2 C1 E1 estate budget 600 s | pass | **PASS** | – | 64,386 | 243,765 | 39 | 8s | 1024 |
| `MC_live` | {a,b} R2 C0 E0 space budget 600 s | pass | **PASS** | – | 12,649 | 31,118 | 47 | 10s | 800 |
| `MC_r25_unrowed_bytes` | {a} R2 C1 E0 strict-held budget 300 s | pass | **PASS** | – | 448 | 974 | 30 | 2s | 297 |
| `MC_r25_strict_deep` | {a} R3 C2 E1 F1 X1 space strict-held budget 600 s | pass | **PASS** | – | 461,893 | 1,801,788 | 49 | 44s | 1774 |
| `MC_r25_strict_main` | {a,b} R2 C1 E1 strict-held sym budget 600 s | pass | **PASS** | – | 963,928 | 3,081,821 | 51 | 98s | 1964 |
| `MC_r25_strict_unsealed` | {a} R2 C1 E0 unsealed-root strict-held budget 300 s | pass | **PASS** | – | 1,050 | 2,035 | 30 | 3s | 367 |
| `MC_r25_strict_authority` | {a} R2 C1 E0 relaxed relaxed-auth strict-held budget 300 s | pass | **PASS** | – | 1,188 | 2,303 | 30 | 2s | 369 |
| `MC_main_sim` | {a,b} R3 C1 E1 F1 X1 space budget 600 s | simulate | **SIMULATION** | – | 1,144,601 | – | – | 40s | 1541 |
| `MC_reach_ledger_manifest` | {a} R3 C2 E1 F1 pre-#169 budget 600 s | reach | **REACHED** | `Witness_LedgerManifest` | 37,543 | 119,680 | 19 | 5s | 844 |
| `MC_reach_ledger_chunks` | {a} R3 C2 E1 F1 pre-#169 budget 600 s | reach | **REACHED** | `Witness_LedgerChunkRead` | 51,760 | 162,053 | 21 | 7s | 828 |
| `MC_reach_wp0g_lost_row` | {a,b} R3 C1 E0 F1 relaxed sym budget 600 s | reach | **REACHED** | `Witness_LostRowRead` | 66,713 | 226,906 | 19 | 10s | 1372 |
| `MC_reach_wp0g_failed_commit` | {a,b} R3 C1 E0 F1 relaxed sym budget 600 s | reach | **REACHED** | `Witness_FailedRowRead` | 268,470 | 802,831 | 29 | 27s | 1781 |
| `MC_reach_exchange_refused` | {a} R3 C1 E1 F1 exchange no-exchange budget 600 s | reach | **REACHED** | `Witness_ExchangeRefused` | 18,393 | 47,246 | 19 | 4s | 631 |
| `MC_reach_remembered_refusal` | {a} R3 C1 E1 F1 exchange budget 600 s | reach | **REACHED** | `Witness_RememberedRefusal` | 5,976 | 16,897 | 13 | 3s | 484 |
| `MC_reach_ownership_superseded` | {a} R3 C1 E1 F1 exchange budget 600 s | reach | **REACHED** | `Witness_OwnershipSuperseded` | 21,932 | 55,697 | 20 | 5s | 620 |
| `MC_reach_sweep_ownership` | {a} R3 C1 E1 F1 exchange budget 600 s | reach | **REACHED** | `Witness_SweepOwnership` | 53,987 | 125,711 | 27 | 6s | 835 |
| `MC_reach_sweep_restore` | {a} R3 C1 E1 F1 exchange budget 600 s | reach | **REACHED** | `Witness_SweepRestore` | 44,020 | 104,243 | 25 | 7s | 812 |
| `MC_wp0g_authority` | {a} R2 C1 E0 relaxed relaxed-auth pre-#169 budget 300 s | fail | **FAIL** | `R25_NoDurableReread` | 679 | 1,241 | 17 | 2s | 312 |
| `MC_store_root_unsealed` | {a} R2 C1 E0 unsealed-root pre-#169 budget 300 s | fail | **FAIL** | `R25_NoDurableReread` | 692 | 1,274 | 17 | 2s | 335 |
| `MC_r25_unrowed_no_adopt` | {a} R2 C1 E0 strict-held pre-#169 budget 300 s | fail | **FAIL** | `R25_StrictNoDurableReread` | 285 | 586 | 16 | 2s | 288 |
| `MC_wp0d_check_rename` | {a} R2 C0 E1 F1 check_rename budget 300 s | fail | **FAIL** | `NoClobber` | 6,974 | 13,317 | 27 | 3s | 504 |
| `MC_neg_live_unfair` | {a} R1 C0 E0 budget 300 s | fail | **FAIL** | `RunsClose` | 78 | 109 | – | 2s | 278 |
| `MC_neg_held_before_commit` | {a} R3 C2 E1 mut=held_before_commit budget 300 s | fail | **FAIL** | `HeldAfterCommit` | 267 | 639 | 7 | 2s | 293 |
| `MC_neg_commit_before_fsync` | {a} R3 C2 E1 mut=commit_before_fsync budget 300 s | fail | **FAIL** | `RecordImpliesBytes` | 794 | 2,141 | 10 | 2s | 362 |
| `MC_neg_commit_before_dirseal` | {a} R1 C0 E0 mut=commit_before_dirseal budget 300 s | fail | **FAIL** | `RecordImpliesBytes` | 86 | 129 | 17 | 1s | 270 |
| `MC_neg_adopt_without_seal` | {a} R1 C0 E0 F1 mut=adopt_without_seal budget 300 s | fail | **FAIL** | `RecordImpliesBytes` | 317 | 510 | 13 | 2s | 289 |
| `MC_neg_ledger_before_held` | {a} R1 C0 E0 mut=ledger_before_held budget 300 s | fail | **FAIL** | `LedgerAfterHeld` | 50 | 70 | 14 | 2s | 256 |
| `MC_neg_done_before_sync` | {a} R1 C0 E0 mut=done_before_sync budget 300 s | fail | **FAIL** | `DoneAfterLedger` | 80 | 117 | 17 | 1s | 283 |
| `MC_neg_reread_durable` | {a} R2 C1 E0 mut=reread_durable budget 300 s | fail | **FAIL** | `R25_NoDurableReread` | 368 | 764 | 20 | 2s | 291 |
| `MC_neg_reread_unchanged` | {a} R2 C1 E0 mut=reread_durable budget 300 s | fail | **FAIL** | `S3_UnchangedReadsZero` | 361 | 749 | 20 | 2s | 295 |
| `MC_neg_reread_changed_only` | {a} R2 C1 E0 mut=reread_durable budget 300 s | fail | **FAIL** | `S3_ReadsOnlyChanged` | 350 | 725 | 20 | 2s | 294 |
| `MC_neg_reread_ignore_ledger` | {a} R2 C0 E0 mut=reread_ignore_ledger budget 300 s | fail | **FAIL** | `R25_NoCommittedCaptureReread` | 119 | 162 | 29 | 1s | 283 |
| `MC_neg_reread_exchange` | {a} R2 C0 E0 exchange mut=reread_durable budget 300 s | fail | **FAIL** | `R25_NoCommittedCaptureReread` | 133 | 180 | 33 | 2s | 312 |
| `MC_neg_skip_output_row` | {a} R1 C0 E0 mut=skip_output_row budget 300 s | fail | **FAIL** | `S3_ClosedPassIsHeld` | 78 | 107 | 17 | 2s | 280 |
| `MC_neg_double_read` | {a} R1 C0 E0 mut=double_read budget 300 s | fail | **FAIL** | `ReadOnce` | 27 | 33 | 8 | 2s | 240 |
| `MC_neg_src_ledger_carries_r25` | {a} R3 C2 E1 mut=src_ledger_carries_r25 budget 300 s | fail | **FAIL** | `R25_NoDurableReread` | 3,687 | 9,524 | 15 | 2s | 500 |
| `MC_neg_record_racy` | {a} R1 C0 E1 mut=record_racy budget 300 s | fail | **FAIL** | `ReuseSound` | 316 | 478 | 12 | 2s | 280 |
| `MC_neg_record_racy_ledger` | {a} R1 C0 E1 mut=record_racy budget 300 s | fail | **FAIL** | `LedgerSound` | 459 | 698 | 15 | 2s | 313 |
| `MC_neg_untyped_space` | {a} R1 C0 E0 X1 mut=untyped_space budget 300 s | fail | **FAIL** | `ClosureAccounted` | 100 | 131 | 16 | 1s | 280 |
| `MC_neg_source_write` | {a} R1 C0 E0 mut=source_write budget 300 s | fail | **FAIL** | `S2_TypedSourceAccess` | 13 | 16 | 5 | 2s | 240 |
| `MC_neg_pause_writer` | {a} R1 C0 E0 mut=pause_writer budget 300 s | fail | **FAIL** | `S2_TypedSourceAccess` | 13 | 16 | 5 | 2s | 243 |
| `MC_neg_git_optional_locks` | {a} R1 C0 E0 estate mut=git_optional_locks budget 300 s | fail | **FAIL** | `S2_TypedSourceAccess` | 8 | 8 | 3 | 1s | 249 |
| `MC_neg_unbounded_backup` | {a} R1 C0 E0 estate mut=unbounded_backup budget 300 s | fail | **FAIL** | `S2_BackupLockBounded` | 286 | 710 | 9 | 2s | 315 |
| `MC_neg_supersede_unchecked` | {a} R1 C0 E0 F1 exchange mut=supersede_unchecked budget 300 s | fail | **FAIL** | `NoClobber` | 333 | 528 | 13 | 2s | 314 |
| `MC_neg_sweep_displaced` | {a} R3 C1 E1 F1 exchange mut=sweep_displaced budget 300 s | fail | **FAIL** | `NoClobber` | 57,438 | 132,874 | 28 | 6s | 827 |
| `MC_neg_adopt_unkeyed` | {a} R2 C1 E1 mut=adopt_unkeyed budget 300 s | fail | **FAIL** | `ReuseSound` | 1,695 | 3,870 | 15 | 3s | 432 |
| `MC_neg_adopt_unverified` | {a} R2 C1 E0 F1 mut=adopt_unverified budget 300 s | fail | **FAIL** | `RecordImpliesBytes` | 1,646 | 4,097 | 15 | 2s | 409 |
| `MC_neg_reuse_ignores_row` | {a} R2 C0 E0 mut=reuse_ignores_row budget 300 s | fail | **FAIL** | `AdoptOnlyUnrowed` | 118 | 161 | 31 | 1s | 290 |
| `MC_neg_adopt_unrecorded` | {a} R3 C1 E0 strict-held mut=adopt_unrecorded budget 300 s | fail | **FAIL** | `R25_StrictNoDurableReread` | 724 | 1,472 | 43 | 2s | 331 |
| `MC_neg_owned_ignores_identity` | {a} R2 C0 E0 F1 exchange mut=owned_ignores_identity budget 300 s | fail | **FAIL** | `NoClobber` | 994 | 1,567 | 27 | 2s | 356 |
| `MC_neg_sweep_drops_ownership` | {a} R3 C1 E1 exchange mut=sweep_drops_ownership budget 300 s | fail | **FAIL** | `SupersedeAtomic` | 5,133 | 10,467 | 26 | 3s | 500 |
| `MC_neg_exchange_before_intent` | {a} R2 C1 E1 exchange mut=exchange_before_intent budget 300 s | fail | **FAIL** | `SupersedeAtomic` | 3,728 | 8,442 | 26 | 3s | 474 |
| `MC_neg_own_is_reuse` | {a} R2 C0 E0 exchange mut=own_is_reuse budget 300 s | fail | **FAIL** | `OwnershipNeverReuse` | 90 | 120 | 23 | 1s | 285 |
| `MC_neg_refusal_unbound` | {a} R2 C0 E0 F2 exchange mut=refusal_unbound budget 300 s | fail | **FAIL** | `RememberedRefusalSound` | 1,264 | 2,984 | 17 | 2s | 406 |
| `MC_neg_late_exchange_refusal` | {a} R2 C0 E1 exchange no-exchange mut=late_exchange_refusal budget 300 s | fail | **FAIL** | `ExchangeRefusedUpFront` | 762 | 1,151 | 22 | 2s | 337 |

Reading the table:

- **Constants.** Seats; `R` runs, `C` crashes, `E` source edits per seat;
  `F` third-party writes, `X` failed group commits; `space` the space
  refusal; `relaxed` relaxed ledger rows, `relaxed-auth` a relaxed
  store-creation commit; `unsealed-root` `StoreRootSealed = FALSE` (the code
  when this row was written; slo.md records #166's seal since);
  `strict-held` `TrackStrictHeld`; `pre-#169` `AdoptUnrowed = FALSE`, the
  transfer before #169, with no capture record. Every row without that tag
  has `AdoptUnrowed = TRUE`, the code since #169 ([the seven rows that
  keep it off](#rows-that-model-the-transfer-before-169)); `exchange` or
  `check_rename` the WP0(d) design (`exchange` is the code since #187; a
  row with neither models [the transfer before
  #187](#rows-that-model-the-transfer-before-187)); `no-exchange`
  `ExchangeSupported = FALSE`, a destination with no atomic exchange
  (OI-1003-Q100); `estate` estate capture's typed reads; `mut=` the mutation;
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
- **Counts on 2026-10-07 (the #187 model extension, OI-1003-Q102).** Listed
  above the table. `MC_supersede_strict_main` counts exactly what
  `MC_supersede_main` does (1,664,264): with the superseding publish on,
  an output this store owns is superseded, never adopted against a
  manifest, so the strict ghost never differs from what the capture
  record already says. `MC_supersede_strict_deep` differs (797,105 against
  792,260) through the third-party write. Fail and reach rows stop at
  their first violation, so their counts are that run's.
- **Counts on 2026-10-07, earlier (#186, #187).** No pass row's count
  moved; `MC_supersede_main` (then 1,072,654) and `MC_supersede_deep`
  (then 506,397) were new.
- **Counts that moved on 2026-10-06 (#169 and its review).** Every row
  but seven now runs with `AdoptUnrowed = TRUE` (the code since #169), so
  every count outside those seven moved: an output's capture record splits
  states, a third-party write may rewrite an output in place, and an
  adoption against a manifest writes a record. `MC_nv_core` and
  `MC_nv_ledger` keep the transfer before #169 and still count 15,834 and
  142,450; `MC_nv_core_adopt` and `MC_nv_ledger_adopt` are the same bounds
  with the adoption. The whole table is from one `tla-check` run over
  every BulkloadTransfer row on 2026-10-06 (sting, load average near 20).
  Fail rows stop at their first violation, so their counts are that run's.
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
- The superseding publish (the code since #187) has both: `MC_supersede_main`
  at `MC_main`'s bound and `MC_supersede_deep` at `MC_main_deep`'s, each
  again under the strict-held ghost.
- No config needed reducing: every positive finishes in about 185 s or less
  at a load near 30 (`MC_supersede_main`, the slowest, 183 s), inside
  its 600 s budget.
- The constants first drafted for `MC_main` (two seats, three runs, every
  fault) appear only as `MC_main_sim`: a seeded simulation (`-simulate
  num=3000 -depth 120 -seed 20261003`), recorded as SIMULATION and never as
  a model-checking result.

## Coverage

Coverage is TLC's `-coverage` report, the last one in each log. An action is
*never enabled* in a config when it generated no state at all. `Next` has 38
actions (39 coverage entries with `Init`).

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
| `MC_nv_core_adopt` | `ForeignWrite`, `ForeignDelete`, `CommitFail` | WP0(d), estate |
| `MC_nv_ledger_adopt` | `CommitFail` | WP0(d), estate |
| `MC_wp0g` | `Edit`, `SilentRewrite`, `RecvRefused`, `CommitFail` | WP0(d), estate |
| `MC_wp0g_deep` | – | WP0(d), estate |
| `MC_wp0d_exchange` | `CommitFail`, `CheckOwn`, `RenameReplace` | estate |
| `MC_supersede_main`, `MC_supersede_strict_main` | `ForeignWrite`, `ForeignDelete`, `CommitFail`, `CheckOwn`, `RenameReplace` | estate |
| `MC_supersede_deep`, `MC_supersede_strict_deep` | `CheckOwn`, `RenameReplace` | estate |
| `MC_supersede_noexchange` | `CommitFail`, `CheckOwn`, `RenameReplace`, `BeginSupersede`, `Exchange`, `VerifyDisp` | estate |
| `MC_s2` | `ForeignWrite`, `ForeignDelete`, `CommitFail` | WP0(d) |
| `MC_live` | `Edit`, `SilentRewrite`, `RecvRefused`, `CrashSrc`, `CrashDst`, `CrashBoth`, `ForeignWrite`, `ForeignDelete`, `CommitFail` | WP0(d), estate |
| `MC_r25_unrowed_bytes`, `MC_r25_strict_unsealed`, `MC_r25_strict_authority` | `Edit`, `SilentRewrite`, `RecvRefused`, `ForeignWrite`, `ForeignDelete`, `CommitFail` | WP0(d), estate |
| `MC_r25_strict_deep` | – | WP0(d), estate |
| `MC_r25_strict_main` | `ForeignWrite`, `ForeignDelete`, `CommitFail` | WP0(d), estate |

Each action is off because of that config's own constants, and every one is
enabled in another config:

| Never enabled | Why it is off | Enabled in |
|---|---|---|
| WP0(d): `BeginSupersede`, `Exchange`, `VerifyDisp` | `SupersedeMode = "off"`: the row models the transfer without superseding publish, the code before #187 ([those rows](#rows-that-model-the-transfer-before-187)). In `MC_supersede_noexchange` they are off because `ExchangeSupported = FALSE`: with no atomic exchange nothing is ever superseded (OI-1003-Q100) | `MC_wp0d_exchange`, `MC_supersede_main`, `MC_supersede_deep`, `MC_supersede_strict_main`, `MC_supersede_strict_deep`, `MC_neg_sweep_displaced` |
| WP0(d): `CheckOwn`, `RenameReplace` | `SupersedeMode` is not `"check_rename"`; these model WP0(d)'s rejected check-then-rename design | `MC_wp0d_check_rename`: its counterexample takes `CheckOwn` at state 25 and `RenameReplace` at state 27 |
| estate: `GitRead`, `BackupBegin`, `BackupStepLock`, `BackupStepUnlock`, `BackupEnd` | `EstateReads = FALSE`: estate capture's typed reads do not depend on the transfer, so S2 is checked on its own | `MC_s2` |
| `ForeignWrite`, `ForeignDelete` | `MaxForeign = 0` | `MC_main_deep`, `MC_dest_faults`, `MC_nv_ledger`, `MC_wp0g`, `MC_wp0g_deep`, `MC_wp0d_exchange`, `MC_supersede_deep`, `MC_supersede_strict_deep`, `MC_supersede_noexchange` |
| `CommitFail` | `MaxCommitFails = 0` | `MC_main_deep`, `MC_dest_faults`, `MC_wp0g_deep`, `MC_supersede_deep`, `MC_supersede_strict_deep` |
| `Edit`, `SilentRewrite`, `RecvRefused` | `MaxEdits = 0`: a refusal needs a changed source | every config with `E1` |
| `CrashSrc`, `CrashDst`, `CrashBoth` | `MaxCrashes = 0` | every config with `C1` or `C2` |

**Branches, not only actions.** Action-level coverage can hide a dead branch
inside an action. Nine reach rows prove that the branches R25, WP0(g) and
#187's records depend on are reachable. Each runs at the bound of a pass
row, so that pass row's complete search explores the branch:

| Reach row | Witness | Branch | At the bound of |
|---|---|---|---|
| `MC_reach_ledger_manifest` | `Witness_LedgerManifest` | `RecvDecide`: a manifest served from the source ledger, with no read | `MC_nv_ledger` |
| `MC_reach_ledger_chunks` | `Witness_LedgerChunkRead` | `RecvNeed`: a ledger manifest's chunks re-read (pread) to fill an absent output | `MC_nv_ledger` |
| `MC_reach_wp0g_lost_row` | `Witness_LostRowRead` | WP0(g): a row a relaxed ledger lost to a power loss, then a later run's ledger miss and read | `MC_wp0g` |
| `MC_reach_exchange_refused` | `Witness_ExchangeRefused` | `RecvManifest`, `RecvEnd`: a changed seat's own output refused `DESTINATION_EXCHANGE_UNSUPPORTED` (OI-1003-Q100) | `MC_supersede_noexchange` |
| `MC_reach_remembered_refusal` | `Witness_RememberedRefusal` | `RecvEntry`: an entry refused from its remembered refusal, with no source read | `MC_wp0d_exchange` |
| `MC_reach_ownership_superseded` | `Witness_OwnershipSuperseded` | `BeginSupersede`: a superseding publish of an output owned by its ownership row alone, a racy publish whose seat changed again (OI-1003-Q101) | `MC_wp0d_exchange` |
| `MC_reach_sweep_ownership` | `Witness_SweepOwnership` | `StartRun`'s sweep: an interrupted supersede whose staged file is at the leaf gets the ownership row | `MC_wp0d_exchange` |
| `MC_reach_sweep_restore` | `Witness_SweepRestore` | `StartRun`'s sweep: an interrupted supersede whose exchange did not take effect gets its rows back | `MC_wp0d_exchange` |
| `MC_reach_wp0g_failed_commit` | `Witness_FailedRowRead` | WP0(g), #163: `LedgerCommit`'s failing branch (counted, not fatal), then with no crash a later run's ledger miss and read | `MC_wp0g` |

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
  `MC_wp0g_deep` (2026-10-04 counts, before #169; not re-measured). Under a
  strict source ledger, a loopback power loss reaches only states that
  `CrashDst` reaches one step earlier, before the `Held` round trip. Only a
  relaxed ledger, which can lose rows at the source too, makes the double
  crash different. So the model's WP0(g) checks do exercise it.

A fail config's coverage covers only the part of the space it searched
before the counterexample.

## Hybrid roles (OI-1003-Q32)

The model is checked by three tools, each with one job:

| Role | Tool | What it does | Run |
|---|---|---|---|
| Checker of record | TLA+ with TLC 2.19 | `BulkloadTransfer.tla` is the model. Every verdict, count and counterexample this README cites as a result is TLC's. | `just tla-check` |
| Typed catalogue | Dhall 1.42 ([`catalogue/`](catalogue/)) | Holds every config's constants and expectation, every mutation's verdict and every property's traceability row. It renders `configs.tsv` and every `MC_*.cfg`, and its staleness and grounding checks gate every TLC run. | `just tla-render` |
| N-version cross-check | Haskell, GHC 9.10, base and containers ([`hs/Explorer.hs`](hs/Explorer.hs)) | A second encoding of the spec: independent code, shared design. An explicit-state BFS transliterated by hand from the spec's actions, on the core and every mutation row inside its domain. It must reproduce TLC's counts and mutation verdicts. It shows that TLC evaluates the spec as its text reads; it cannot catch a misreading of the code that the spec makes. | `just formal-nv` |

OI-1003-Q43 widens these roles for git carry: Haskell also holds a reference
copy of the code's decision core, a differential oracle for the Rust code,
not only a second encoding of TLC's spec ([GitCarry
roles](#roles-oi-1003-q43)).

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
- **Every mutation has a verdict and a primary row.** A total `merge` maps
  each `Mutation` to the property its `MC_neg_` row must violate. The
  primary rows are a record with one field per mutation label (`primary`),
  also merged over the union. So a label added without a verdict, or
  without a primary row, is a type error (`Missing handler`), and a field
  without a label is one too (`Unused handler`). An assert requires every
  field to hold its own label's row. Another requires the rendered rows to
  contain exactly one primary row per field, at positions 0, 1, 2, ...
  counted from the record, not from a literal list. A second row for the
  same mutation names its other property explicitly (`also`), as
  `MC_neg_reread_unchanged` and `MC_neg_record_racy_ledger` do.
- **No union is listed by hand.** Dhall cannot list a union's labels, so
  every list of all properties, actions, witnesses or mutations comes from
  a record with one field per label (`Types.dhall`'s `propertyTable`,
  `actionTable` and `witnessTable`, and `primary`), kept exact by a total
  `merge`. The safety invariants are the properties whose class in
  `propertyTable` is `safety`, not a separate list. `grounding.mutations`
  is `primary`'s fields. GitCarry's tables follow the same rule, the
  decision core's closed unions included: each of `basisTable`,
  `rebaseTable`, `reuseTable`, `refusalTable` and `decisionTable` is merged
  over its union (`basisSelf` ... `decisionSelf`), so a label added to
  `Basis`, `Refusal` or `Decision` without a table entry is a `Missing
  handler` error, not a label the rendered lists silently omit.
- **`gen_cfgs.py`'s checks, kept.** Every safety invariant except `TypeOK`
  has a fail row, and no row with a temporal property uses `SYMMETRY`.
  Because the safety invariants come from the property table, a property
  added with class `safety` and no fail row fails the first check.
- **Names are the TLA+ names.** Union labels are rendered with
  `showConstructor`, so the catalogue cannot misspell a property, action or
  mutation. The frozen names are unchanged.
- **Traceability rows.** One `{tla, slo, ruling, codeSymbol, ptest}` row
  per frozen safety invariant except `TypeOK`, and per temporal property,
  in table order; the expected list comes from the property table. It is
  the table under [Properties, SLOs, rulings and
  tests](#properties-slos-rulings-and-tests), with the code symbols each
  property is about.

What `tla-render` and `tla-check` add in the shell:

- The rendered file names are unique and safe (`MC_<name>.cfg`,
  `configs.tsv` or `configs_<module>.tsv`).
- **Staleness.** The committed `configs*.tsv` and `MC_*.cfg` equal the
  catalogue's rendering byte for byte, with no file missing or extra.
  `just tla-render --check` runs this check alone.
- **Grounding, per module** (`T.Module`; GitCarry's numbers are in
  [GitCarry](#results-gitcarry)). For BulkloadTransfer.tla: every operator
  the catalogue names (26 properties, 8
  witnesses, 38 actions, `Spec`, `LiveSpec`, `SeatSymmetry`, `Init` and
  `Next`: 77 names) is defined in the spec. The catalogue's 18 constants
  are exactly the spec's `CONSTANTS`, and its 29 mutations exactly the
  spec's `Mutations` set without `"none"`: a rule break added to the spec
  with no catalogue entry fails, as does a catalogue entry the spec lacks.
  All 31 distinct code symbols (the frozen invariants' and, since
  2026-10-07, those of `supersedeInvariants`, the traceability rows of
  #187's four properties) are found as whole words on lines that are
  not comments, in the Rust sources under `crates/` outside `tests/` (the
  module's `symbolMatch` is `code`: its symbols include enum variants,
  fields and parameters, which have no item definition). GitCarry.tla's
  `symbolMatch` is `definition`: each symbol must be an item definition
  (`fn`, `const`, `struct`, `enum` and so on). A data file such as
  `decide_rows.tsv`, a test or a comment that only names a symbol grounds
  it in neither module.

**It replaced `gen_cfgs.py` byte for byte.** At `0781bd6` the catalogue's
rendering, `gen_cfgs.py`'s output and the committed files were identical:
44 files, with the same sha256 manifest `259bd98c…f340` for all three. The
next commit changed only each file's provenance comment line (43 `\*`
lines and one `#` line) and deleted `gen_cfgs.py`. TLC ignores `\*`
comments and `tla-check` skips `#` lines, so no state or verdict moved.

### The explorer

`hs/Explorer.hs` is an explicit-state breadth-first search of the spec's
actions and invariants inside its domain.

- **A second encoding, not an independent one.** The code is independent:
  a separate program in another language, with another state
  representation (one record per seat instead of one function per
  variable). It reads no TLA+, no `.cfg` and no rendered file, and nothing
  in it is generated from the TLA+ text. The design is shared: it was
  transliterated by hand from `BulkloadTransfer.tla`'s action definitions.
  Its actions are the ones TLC's coverage shows enabled in `MC_nv_ledger`,
  its successor order is `Next`'s disjunct order, and its helpers and
  sentinels follow the spec's: `readSeat` is `DoRead`, `fates` is
  `CrashChoice`, `dstLosses` is `DstLoss`, and `GARBAGE`, `FOREIGN`,
  `FirstForeignId` and `KeyBase` keep their values. So parity shows that
  TLC evaluates the spec as its text reads, and that neither encoding has a
  slip the other lacks. It does not check the spec against the code. If
  the spec misreads a rule of the Rust code, such as how `Inbound::entry`
  decides `Reuse` or which captures `RecvHeld` submits, the explorer
  encodes the same misreading, and every count and counterexample still
  matches. Getting that check needs one of two things, neither done: a
  second author who derives each action from the cited Rust function
  without reading the spec, or property-test or fault-harness traces
  replayed as explorer behaviours.
- **Its domain.** It has the 27 actions that TLC's coverage shows enabled
  in `MC_nv_ledger`: `StartRun`, the 13 per-seat protocol actions,
  `Commit`, `LedgerCommit`, `SendSourceDone`, `Finish`, `Edit`,
  `SilentRewrite`, `Tick`, `ForeignWrite`, `ForeignDelete`, the three
  crashes and `Terminated`. The other 11 are absent: `CommitFail`, WP0(d)'s
  five and estate capture's five. So are the space refusal, relaxed
  stores and an unsealed state root. **#187's records are pinned outside
  it (2026-10-07, OI-1003-Q102).** The intent, the ownership row, the
  remembered refusal, the sweep of intents and the refusal without an
  exchange exist in the spec only under `SupersedeMode = "exchange"`,
  which the explorer does not have; `explorerModels` also requires
  `ExchangeSupported`. With `SupersedeMode = "off"` each new variable
  holds its initial value in every reachable state, so the four presets'
  counts did not move and the explorer needed no new state
  (`hs/Explorer.hs`'s header says the same). Their four properties and six
  mutations are TLC's alone, as WP0(d)'s were already. Since the #169 review it models the
  capture record and its adoption (`--adopt`, TLC's `AdoptUnrowed`: the
  file's `rec` field, `RecvEntry`'s adoption, the manifest adoption's
  record in `RecvEnd`, `AnswerHeld`'s settle of an adopted entry and
  `ForeignWrite`'s in-place rewrite) and the strict-held ghost
  (`--strict-held`, `TrackStrictHeld`). With both off it is the explorer
  of before, state for state. An adopted entry's failed group
  (`AnswerHeld`'s `failed_space` outcome) needs `CommitFail` and stays
  outside. Inside the
  domain, `NoClobber` and `S2_BackupLockBounded` hold by construction, in
  the spec as well as in the explorer: only absent actions set the state
  they read (`clobbered`, and the SQLite backup's lock and steps). The
  explorer encodes both as constantly true, so neither is cross-checked.
- **Presets and bound flags.** The presets are `nv_core` (`MC_nv_core`)
  and `nv_ledger` (`MC_nv_ledger`), the transfer before #169, and
  `nv_core_adopt` and `nv_ledger_adopt` (`MC_nv_core_adopt`,
  `MC_nv_ledger_adopt`), the same bounds for the code since #169.
  `--seats`, `--runs`, `--crashes`, `--edits` and `--foreign` override
  them, as TLC's `Seats`, `MaxRuns`, `MaxCrashes`, `MaxEdits` and
  `MaxForeign`; `--adopt` and `--strict-held` turn on `AdoptUnrowed` and
  `TrackStrictHeld`. It supports the 18 mutations that need no absent
  action, and refuses the other 11 at the command line.
- **What `formal-nv` checks.** The presets must reach TLC's distinct-state
  counts of record, with no invariant violated and no deadlock. The
  mutation rows come from the catalogue, not from the recipe. The
  evaluated catalogue's `nversion` list holds every `MC_neg_` row whose
  constants are inside the explorer's domain (`explorerModels` in
  `Catalogue.dhall`), with its bound, its switches, its mutation and its
  named property.
  Today that is 21 rows: the 18 primary rows of the supported mutations,
  plus `MC_neg_reread_unchanged`, `MC_neg_reread_changed_only` and
  `MC_neg_record_racy_ledger`. Every one of them runs with `AdoptUnrowed`
  on (the code since #169). Outside the domain are
  `MC_neg_reread_exchange`, `untyped_space`, `git_optional_locks`,
  `unbounded_backup`, `supersede_unchecked`, `sweep_displaced` and the
  six rows of #187's records (`owned_ignores_identity`,
  `sweep_drops_ownership`, `exchange_before_intent`, `own_is_reuse`,
  `refusal_unbound`, `late_exchange_refusal`). For
  each row:
  - The explorer runs at the row's bound, with `TypeOK` and the named
    property checked as the row's TLC config does, and must violate
    exactly that property. The property is the catalogue's, which
    `tla-check` holds TLC to.
  - For a primary row, the mutation runs once more with every safety
    invariant checked, and `R25_StrictNoDurableReread` and
    `AdoptOnlyUnrowed` where the row's constants set them (the catalogue's
    `beside`), both on TLC and on the explorer. TLC runs with one
    worker, so its breadth-first order is fixed, from a config the
    catalogue renders into scratch. Both must report the same first
    violated invariant, in the configs' order, after the same number of
    states. This is what makes the other invariants count: an invariant
    that is too weak or too strong in either encoding changes where some
    search stops.

  A state with no successor is a deadlock, as in TLC.
- **Counterexamples are JSON.** One file per violation: the row, the
  bound, the mutation, the invariants checked and violated, any other
  invariant false in the last state, and the trace. Each state uses the
  spec's variable names and value spellings, leaving out the variables the
  core holds constant. `formal-nv` keeps them, and TLC's logs, under a
  private `mktemp` directory in `$TMPDIR` and prints its path.

Parity, 2026-10-06, host sting, explorer built with `ghc -O1`, from one
`just formal-nv` run over commit `8714c61`. TLC's positive counts are the
Results table's.

| Row | TLC | Explorer | Match |
|---|---|---|---|
| `MC_nv_core` | PASS: 15,834 distinct, 44,312 generated, diameter 45 | pass: 15,834 distinct, 44,312 generated, 45 levels | yes |
| `MC_nv_ledger` | PASS: 142,450 distinct, 497,089 generated, diameter 49 | pass: 142,450 distinct, 497,089 generated, 49 levels | yes |
| `MC_nv_core_adopt` | PASS: 17,027 distinct, 47,053 generated, diameter 45 | pass: 17,027 distinct, 47,053 generated, 45 levels | yes |
| `MC_nv_ledger_adopt` | PASS: 185,852 distinct, 644,493 generated, diameter 49 | pass: 185,852 distinct, 644,493 generated, 49 levels | yes |

The mutation rows, at each row's bound (seats, runs, crashes, edits,
third-party writes, and `strict-held` where the row sets it). Every row
runs with `AdoptUnrowed` on, on TLC and on the explorer (`--adopt`). The
named property is the catalogue's verdict, which
the explorer violated in every row; the length is the explorer's
counterexample. The every-invariant columns are TLC's one-worker run and
the explorer's, each as the first violated invariant and the
counterexample's length. All 39 mutation runs matched (21 with the named
property, 18 with every invariant), and so did all four presets.

| Row | Bound | Named property (explorer) | Every invariant: TLC | Every invariant: explorer |
|---|---|---|---|---|
| `MC_neg_held_before_commit` | a, 3, 2, 1, 0 | `HeldAfterCommit`, 7 states | `HeldAfterCommit`, 7 | `HeldAfterCommit`, 7 |
| `MC_neg_commit_before_fsync` | a, 3, 2, 1, 0 | `RecordImpliesBytes`, 9 | `RecordImpliesBytes`, 9 | `RecordImpliesBytes`, 9 |
| `MC_neg_commit_before_dirseal` | a, 1, 0, 0, 0 | `RecordImpliesBytes`, 9 | `RecordImpliesBytes`, 9 | `RecordImpliesBytes`, 9 |
| `MC_neg_adopt_without_seal` | a, 1, 0, 0, 1 | `RecordImpliesBytes`, 11 | `RecordImpliesBytes`, 11 | `RecordImpliesBytes`, 11 |
| `MC_neg_ledger_before_held` | a, 1, 0, 0, 0 | `LedgerAfterHeld`, 5 | `LedgerAfterHeld`, 5 | `LedgerAfterHeld`, 5 |
| `MC_neg_done_before_sync` | a, 1, 0, 0, 0 | `DoneAfterLedger`, 13 | `DoneAfterLedger`, 13 | `DoneAfterLedger`, 13 |
| `MC_neg_reread_durable` | a, 2, 1, 0, 0 | `R25_NoDurableReread`, 15 | `R25_NoDurableReread`, 15 | `R25_NoDurableReread`, 15 |
| `MC_neg_reread_unchanged` | a, 2, 1, 0, 0 | `S3_UnchangedReadsZero`, 15 | (an `also` row) | |
| `MC_neg_reread_changed_only` | a, 2, 1, 0, 0 | `S3_ReadsOnlyChanged`, 15 | (an `also` row) | |
| `MC_neg_reread_ignore_ledger` | a, 2, 0, 0, 0 | `R25_NoCommittedCaptureReread`, 19 | `R25_NoDurableReread`, 19 | `R25_NoDurableReread`, 19 |
| `MC_neg_skip_output_row` | a, 1, 0, 0, 0 | `S3_ClosedPassIsHeld`, 15 | `LedgerAfterHeld`, 12 | `LedgerAfterHeld`, 12 |
| `MC_neg_double_read` | a, 1, 0, 0, 0 | `ReadOnce`, 7 | `ReadOnce`, 7 | `ReadOnce`, 7 |
| `MC_neg_src_ledger_carries_r25` | a, 3, 2, 1, 0 | `R25_NoDurableReread`, 15 | `R25_NoDurableReread`, 15 | `R25_NoDurableReread`, 15 |
| `MC_neg_record_racy` | a, 1, 0, 1, 0 | `ReuseSound`, 11 | `ReuseSound`, 11 | `ReuseSound`, 11 |
| `MC_neg_record_racy_ledger` | a, 1, 0, 1, 0 | `LedgerSound`, 14 | (an `also` row) | |
| `MC_neg_source_write` | a, 1, 0, 0, 0 | `S2_TypedSourceAccess`, 5 | `S2_TypedSourceAccess`, 5 | `S2_TypedSourceAccess`, 5 |
| `MC_neg_pause_writer` | a, 1, 0, 0, 0 | `S2_TypedSourceAccess`, 5 | `S2_TypedSourceAccess`, 5 | `S2_TypedSourceAccess`, 5 |
| `MC_neg_adopt_unkeyed` | a, 2, 1, 1, 0 | `ReuseSound`, 15 | `ReuseSound`, 15 | `ReuseSound`, 15 |
| `MC_neg_adopt_unverified` | a, 2, 1, 0, 1 | `RecordImpliesBytes`, 15 | `RecordImpliesBytes`, 15 | `RecordImpliesBytes`, 15 |
| `MC_neg_reuse_ignores_row` | a, 2, 0, 0, 0 | `AdoptOnlyUnrowed`, 18 | `AdoptOnlyUnrowed`, 18 | `AdoptOnlyUnrowed`, 18 |
| `MC_neg_adopt_unrecorded` | a, 3, 1, 0, 0, strict-held | `R25_StrictNoDurableReread`, 28 | `R25_StrictNoDurableReread`, 28 | `R25_StrictNoDurableReread`, 28 |

What the parity shows:

- **The positive counts match exactly, and so do the generated counts and
  the depths.** The cross-check needs only the distinct count. A matching
  generated count also means that both compute the same total number of
  successors over the reachable states, counted as TLC counts them: the
  initial states plus every successor computed, `Terminated`'s stuttering
  step included. `MC_nv_ledger` reaches both ledger branches, so its
  counts cover the ledger-manifest and ledger chunk-read semantics too.
- **The counterexamples are the same behaviours.** In one-worker
  (`-workers 1`) hand runs of the three core mutations, TLC's
  counterexamples had the same length and the same action sequence as the
  explorer's. `formal-nv` compares only the length and the first violated
  invariant, for every primary row:
  - `held_before_commit`: run 1 sends and stages seat `a`, then answers
    `Held{true}` before any commit.
  - `commit_before_fsync`: run 1 publishes the unsealed temporary,
    seals the directory and commits its row.
  - `src_ledger_carries_r25`: run 1 commits the output, and the source
    crashes before `Held`. Run 2 is refused `Reuse`, because the source
    ledger has no row, and reads the seat again.
- **Where a fail search stops is not a cross-check.** The distinct count at
  the first violation depends on the order within a BFS level: 179, 549 and
  3,487 for the explorer; 196, 589 and 3,607 for TLC with one worker; and
  267, 832 and 3,841 with three workers in `tla-check` (2026-10-06).
- **With every invariant checked**, 16 of the 18 primary rows stop first
  at their verdict, on TLC and on the explorer alike. Two stop first at
  another invariant, on both:
  - `reread_ignore_ledger` stops at `R25_NoDurableReread`, in the same
    19-state counterexample that violates its verdict
    `R25_NoCommittedCaptureReread`;
  - `skip_output_row` stops earlier, at `LedgerAfterHeld` after 12
    states: the source ledger records a capture whose output row the
    mutation never commits. Its verdict `S3_ClosedPassIsHeld` needs 15.

  `reread_durable`'s and `src_ledger_carries_r25`'s counterexamples also
  violate `S3_ReadsOnlyChanged` and `S3_UnchangedReadsZero` in the same
  state: the seat they read again was held when the run began and is
  unchanged.
- **What it does not show.** That the spec matches the code: the explorer
  is a second encoding of the spec, not of the code (above). Nor does
  every invariant fire on the explorer. 13 of the 17 safety invariants are
  some row's named property and fire, and so do #169's two
  (`AdoptOnlyUnrowed`, `R25_StrictNoDurableReread`). `TypeOK` is the sanity
  check.
  `NoClobber` and `S2_BackupLockBounded` hold by construction inside the
  domain. `ClosureAccounted` is violated only by `untyped_space`, outside
  it. The every-invariant runs still show that `ClosureAccounted` does not
  fire early on either side.

Runtime on sting at a load average near 45: the build takes about 26 s;
`MC_nv_core` takes 0.7 s and `MC_nv_ledger` 7.6 s. Under `runghc`,
without a build, `MC_nv_core` takes 17 s. A whole `just formal-nv` run,
with its 14 TLC runs, took about 50 s at a load average between 20 and
30. Those are 2026-10-04 timings, before GitCarry's rows and #169's four
mutation rows joined the recipe (18 BulkloadTransfer TLC runs now). On
2026-10-06 a whole run over both modules took 244 s at a load average near
20.

## N-version core (OI-1003-Q32)

The model is a hybrid. A second encoding of the spec, sprint 2's Haskell
BFS (independent code, shared design; [Hybrid
roles](#hybrid-roles-oi-1003-q32)), must reproduce TLC's count on a shared
core. The core is `MC_nv_core`:

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
| `MC_nv_core_adopt` | 2 | 47,053 | **17,027** | 45 | – |
| `MC_nv_ledger_adopt` | 2 | 644,493 | **185,852** | 49 | – |

**Which transfer the core models.** `MC_nv_core` is a frozen name with a
count of record, and both it and `MC_nv_ledger` model the transfer before
#169 (`AdoptUnrowed = FALSE`): no capture record, no adoption. That is not
the shipped transfer. `MC_nv_core_adopt` and `MC_nv_ledger_adopt` are the
same two bounds with `AdoptUnrowed = TRUE`, and the explorer reaches both
of their counts, with matching generated counts and depths (2026-10-06).
So the cross-check covers #169's `RecvEntry` adoption, the manifest
adoption's record, `AnswerHeld`'s settle and the in-place `ForeignWrite`,
and every mutation row in the explorer's domain runs with the adoption on.
Making the adopt rows the core of record, and retiring or re-freezing
`MC_nv_core`'s count, needs a ruling; until then both pairs run.

The distinct count of a completed breadth-first search is the reachable set,
so it does not depend on the number of workers. `MC_nv_core` counted 15,834
with 3 workers in every run on 2026-10-03 and 2026-10-04, and the duplicate
copies reported the same count with 1 and 4 workers. This revision's spec
changes leave it unchanged. The new ghost `ledgerLost` stays empty under a
strict ledger. Unless `TrackStrictHeld` is set, the new field of an output
record is always FALSE, and the new field of a read record equals `held`.
Unless `AdoptUnrowed` is set (#169), an output's capture record is always
`NoRecord` and no entry is adopted unrowed, so `MC_nv_core` still counts
15,834 (re-checked 2026-10-06, after the review's spec changes too).

The mutations `held_before_commit`, `commit_before_fsync` and
`src_ledger_carries_r25` run at the core's bound, with the adoption on, as
separate fail configs.
`formal-nv` runs them, and every other mutation row inside the explorer's
domain, on both checkers at each row's own bound. A failing config's state
count depends on where its search stops, so only the positive core's count
is a cross-check.

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
that creates its authority and its state root's directory entry. Built
2026-10-07 on these conditions ([WP0(g) as built](#wp0g-as-built-2026-10-07)).**

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
This row models the transfer before #169. Since #169 an output that carries
a capture record is adopted across a lost authority without a read
(`MC_r25_strict_authority` passes at this bound); an output without a
record is still re-read, so the condition stands ([rows that model the
transfer before #169](#rows-that-model-the-transfer-before-169)).

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

### WP0(g) as built (2026-10-07)

OI-1003-Q104 ruled WP0(g) built. `RelaxedSourceLedger = TRUE` is the code's
`LedgerSync::Relaxed`, the default. The conditions above, and where the
code meets each:

| Condition | Code | Model | Test |
|---|---|---|---|
| Only the ledger's row commits are relaxed, and only on the source | `relax_ledger_rows` (`A/io/durable.rs`): `synchronous=NORMAL`, `fullfsync=OFF`, read back; called by `StorePublisher::relax_ledger_rows`, which refuses a destination publisher, from `LedgerSink::with_sync`, which `serve` calls on its second connection | `RelaxedSourceLedger`: `SrcLoss` may drop any subset of `srcLedger` | `only_a_source_ledgers_row_commits_are_relaxed` |
| The creation commit (schema and authority) is FULL | `Store::open` runs `configure_sqlite` (unchanged) and commits before a publisher exists; `serve` opens the store, and sends `Start`, on that connection | `RelaxedAuthority = FALSE`; `MC_wp0g_authority` fails `R25_NoDurableReread` with it `TRUE` | P79 (the authority after every loss), `stores_commit_through_wal_with_full_flushes` |
| After #161 (sealed state root) | `Store::open` seals the root and its parent (#166, on main) | `StoreRootSealed = TRUE`; `MC_store_root_unsealed` fails without it | `tests/power_loss.rs` (#166's) |
| A corrupt or absent ledger is empty | `ledger_read` (`A/transfer.rs`): a failed ledger read is a miss, counted `source_ledger_unreadable` | the empty subset | `an_unreadable_relaxed_ledger_reads_as_a_miss` |
| Never a new authority because rows are gone | `Store::open` inserts the authority once (`INSERT OR IGNORE`) | `SrcLoss` leaves `srcAuth` unless the store itself is lost | P79, `every_power_loss_state_of_a_relaxed_ledger_costs_at_most_its_lost_seats` |
| A failed ledger commit is counted, never fatal (#163) | `LedgerSink::publish`: `source_ledger_commit_failed`, `source_ledger_rows_dropped`, `Ok` | `LedgerCommit`'s second branch (`ledgerLost`, `by = "commit_failed"`) | `a_failed_relaxed_ledger_commit_is_counted_not_fatal` |
| The destination is untouched | no change to `configure_sqlite`, `commit_outputs`, `PublishSink`, `materialize.rs` or `Inbound::answer_held` | `HeldAfterCommit`, `RecordImpliesBytes`, every mutation row: unchanged expectations | the existing power-loss and fault harnesses, unchanged |

**Which R25 reading holds with the relaxation.** Both, each within its
stated limits:

- The committed-row reading (`R25_NoDurableReread`, the SLO's obligation,
  OI-1003-Q40) holds in `MC_wp0g` and `MC_wp0g_deep`, now with the counted
  commit failure as well as the power loss.
- The strict reading (`R25_StrictNoDurableReread`, #169) holds in
  `MC_wp0g_strict`: `MC_wp0g_deep`'s bound with `TrackStrictHeld`. Its
  limits are #169's ([R25's strict reading](#r25s-strict-reading-169)): a
  non-racy capture whose record could be written.

So a lost ledger row, whichever way it was lost, costs a read only of a
seat the destination does not hold: no row there, and no durable unrowed
bytes with a capture record. On the code, P79 and the power-loss test
check the committed-row reading on the real store: every seat the
destination holds is answered `Reuse` and reads 0 bytes whatever the
ledger lost, and a lost row costs its seat's bytes once, only at a seat a
third party removed.

**What the model still abstracts.** The loss is any subset of the
committed rows. `SQLite` in WAL mode loses commits newest first (a suffix),
which P79 and the power-loss test check on the real WAL: cut, torn and
garbled tails. Torn pages inside a checkpointed database are outside both
(`checkpoint_fullfsync` stays ON, and `relax_ledger_rows` reads it back).
The refusal rows of the ledger (#186) are not in the model; a lost one
costs one more 16-byte sniff.

## WP0(d): exchange versus check-then-rename (OI-1003-Q18)

Before #187 the code had no superseding publish: a changed seat whose output
existed was adopted only if its bytes verified, and otherwise refused
(`DESTINATION_OCCUPIED`; `GIT_DESTINATION_OCCUPIED` before #187).
`SupersedeMode = "off"` models that transfer. Two designs were checked
against `NoClobber`: bulkload replaces or removes a destination file only
when its identity is one this store recorded.

- **`check_rename` fails** (`MC_wp0d_check_rename`, `NoClobber` and only
  it, with a 27-state counterexample). Comparing the output's `(dev, ino, stat)` with the
  store's row, then renaming over it, leaves a window. A third-party write
  that lands in between is clobbered. No fsync ordering closes that window.
- **`exchange` passes** (`MC_wp0d_exchange`). This is `RENAME_EXCHANGE`
  (`renameat2`; `renameatx_np(RENAME_SWAP)` on Darwin) of the sealed new file
  with the output. The design then checks the *displaced* file's identity:
  - this store's own file is removed;
  - a foreign file is exchanged back;
  - after a crash, the next session restores a displaced foreign file instead
    of sweeping it like a temporary.

  `MC_neg_supersede_unchecked` and `MC_neg_sweep_displaced` show that the
  identity check and the recovery rule are each load-bearing.

**The code since #187 is the exchange design** (2026-10-07;
`crates/bulkload-agent/src/materialize.rs`, "Superseding publish"). Six
pass rows check it: `MC_wp0d_exchange`, `MC_supersede_main` (`MC_main`'s
bound), `MC_supersede_deep` (`MC_main_deep`'s, with a failed group commit,
the space refusal and a third-party write), the same two bounds under
R25's strict reading (`MC_supersede_strict_main`,
`MC_supersede_strict_deep`), and `MC_supersede_noexchange`, a destination
with no atomic exchange. All pass every safety invariant and the four
properties of [#187's records](#187s-records-in-the-model-oi-1003-q102).

How the model stands to the code since the 2026-10-07 extension
(OI-1003-Q102: model first, then merge):

- **The intent is modelled.** `BeginSupersede` is `begin_supersedes`: one
  store commit writes the intent and moves the path's rows (reuse rows and
  ownership row) out of the store into it, before the exchange. `Exchange`
  trades the two names, `VerifyDisp` checks the displaced file against the
  identity the intent recorded, and `Commit` settles the intent beside the
  new row. `StartRun` sweeps an intent a crash or a failed group left
  (`Destination::settle_supersedes`). Before the extension the model kept
  the old rows in the store across the exchange and read ownership from
  them; that shape is now the mutation `exchange_before_intent`'s.
- **A touched seat.** An owned output whose bytes already equal the
  manifest's is adopted in place by the code; the model exchanges it for an
  equal file. `MC_neg_reread_exchange`'s counterexample rests on that
  abstraction: the code would adopt, and read nothing. On a destination
  with no exchange the model follows the code: equal bytes are adopted,
  other bytes refused.
- **What the model does not see.** The identity check on the displaced file
  compares the inode, size and mtime, not the ctime: the exchange itself
  moves it. A third party that rewrites the output in place and puts its
  mtime back between the publish's last look and its exchange is not seen
  (`materialize.rs`, "One limit"). The model's identity is a version that
  any write moves, and it folds the publish's last look into the exchange,
  so every third-party change after the intent is answered by
  `VerifyDisp`. The syscall ordering inside the group (the seal between an
  exchange back and its unlink, the barrier model on Darwin) is
  `crash_check`'s to prove, on hand-written and real traces
  (`io/crash_check/tests.rs`, `tests/power_loss.rs`).

## Abstraction map

Code at adb9c66, re-checked at `origin/main` 6268175. `A` is
`crates/bulkload-agent/src`, `P` is `crates/bulkload-proto/src`.

| Model | Code |
|---|---|
| `Walk` | `A/transfer.rs` `walk_source`, `Outbound::walked`, `Outbound::offer` (`Control::Entry`); `A/walk.rs` `Walker` |
| `RecvEntry` (Reuse / Send / WantManifest / Refuse; #169's adoption) | `A/transfer.rs` `Inbound::entry`, `Inbound::admit`, `Inbound::adopt_unrowed`; `A/transfer/unrowed.rs` `prove`, `record_key`; `A/materialize.rs` `Destination::identity`; `A/transfer_store.rs` `Store::output_matches` |
| `RecvDecide` (capture) | `A/transfer.rs` `Outbound::decide`, `run_job`, `send_capture`, `manifest_capture`, `capture_file`, `capture_clock`; `A/git_carry.rs` `racy`; `A/transfer_store.rs` `Store::capture` |
| `RecvManifest` | `A/transfer.rs` `Inbound::manifest`, `plan_file` |
| `RecvNeed` | `A/transfer.rs` `Outbound::need_chunks`, `serve_chunks` |
| `RecvEnd` | `A/transfer.rs` `Inbound::end`, `end_streaming`, `end_filling`, `Inbound::publish`, `Inbound::adopt`; `A/materialize.rs` `verify_existing`; `A/transfer/unrowed.rs` `write_record`, `refresh` |
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
| `RecvManifest`'s plan for an owned output (WP0(d), the code since #187; OI-1003-Q100) | `A/transfer.rs` `plan_file`; `A/materialize.rs` `owned_output`, `Destination::exchange_supported`; `A/transfer_store.rs` `Store::output_rows`, `owner_key` |
| `BeginSupersede` | `A/materialize.rs` `PublishSink::supersede`, `StagedFile::prepare_supersede`; `A/transfer_store.rs` `begin_supersedes` |
| `Exchange`, `VerifyDisp` | `A/materialize.rs` `StagedFile::exchange`, `is_owned`, `is_staged`; `A/io/mod.rs` `exchange` |
| `Commit`'s settle of an intent; a racy output's ownership row (OI-1003-Q101) | `A/transfer_store.rs` `commit_outputs`, `settle_in`, `row_in`, `own_in`, `owner_key` |
| `StartRun`'s sweep of unsettled intents | `A/materialize.rs` `Destination::settle_supersedes` (ahead of `Destination::sweep`), `published`; `A/transfer_store.rs` `supersede_intents`, `settle_supersede` |
| `RecvEntry`'s remembered refusal; `RecvEnd`'s record of one | `A/transfer.rs` `Inbound::remembered_refusal`, `Inbound::remember_refusal`, `verify_settled`; `A/transfer_store.rs` `Store::refused_output`, `Store::remember_refused_output` |
| `CheckOwn`, `RenameReplace` | WP0(d)'s rejected check-then-rename design; no code |
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
| `R25_NoDurableReread` | No source content read of a seat at a stat identity the destination holds durably. "Holds" means a committed row, recorded from that identity, vouches for the durable output at the path. Stated physically, under any source authority. **The operative R25 check in code shape.** "Held" is narrowed to "a committed row"; no ruling fixes that reading yet ([disagreements](#code-and-design-disagreements)). | S3 | R25 / R-N58, OI-1003-Q7, OI-1003-Q20, OI-1003-Q37 | P23, P21, P19, P24, P33, P79 |
| `R25_NoCommittedCaptureReread` | slo.md's wording: no committed capture (a source row whose output the destination still holds) is re-read. **Vacuous while `SupersedeMode = "off"` (the transfer before #187)**: the source reads with its ledger row present only to serve chunks for an absent output, so `reread_durable` (no `Reuse`) alone never violates it. It fails only when the source also ignores its ledger (`MC_neg_reread_ignore_ledger`) or under the exchange design (`MC_neg_reread_exchange`). Not evidence for R25 in code shape. | S3 | R-N58, OI-1003-Q7 | P23, P33 |
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
| `NoClobber` | Bulkload replaces or removes a destination file only when its identity is one this store recorded: in a reuse row, in the path's ownership row, or in a row an unsettled intent holds (`RecordedIds`). The ghost is set by every step that unnames a file: `RenameReplace`, `VerifyDisp`'s removal of a displaced file, and the sweep's. | S4 | OI-1003-Q18 (WP0(d)), R-N119 | P7, P8, P26, P78 |
| `S2_TypedSourceAccess` | Every source access is a stat, a content read, an allowlisted git read or the SQLite backup. No write, lock, lease or signal. | S2 | OI-1003-Q5, OI-1003-Q16 (WP0(b)) | P34 |
| `S2_BackupLockBounded` | The backup's lock is only ever shared, held only inside one step of a counted backup, and taken at most `max_steps` times. | S2 (its stated exception) | OI-1003-Q16 | P34; no dedicated test yet |
| `ClosureAccounted` | A finished session leaves every seat applied or with a typed refusal. A bare `IO` closes nothing. | S4 | OI-1003-Q1, #100 | P61 |
| `RunsClose`, `AllRunsFinish` | Under `WF_vars(Protocol)` and the liveness assumption, every started run closes and every run is made. | S4, S5 | OI-1003-Q2 | P28, P23 |

`TypeOK` is a sanity check. `WithinBudget` is the wall-clock bound.

Not frozen, and not the SLO's obligation (OI-1003-Q40):

| Property | What it is for |
|---|---|
| `R25_StrictNoDurableReread` | R25 under the strict reading of "held durably" (OI-1002-Q33): bulkload's own output from a non-racy capture, published or adopted and sealed, durable at the final path with the seat's current bytes, counts as held whether or not a row records it. Meaningful only under `TrackStrictHeld`. The transfer before #169 fails it (`MC_r25_unrowed_no_adopt`), and so does a manifest adoption that writes no record (`MC_neg_adopt_unrecorded`). With the capture record's adoption (`AdoptUnrowed`) it holds beside every safety invariant, within [stated limits](#r25s-strict-reading-169) (`MC_r25_unrowed_bytes`, `MC_r25_strict_deep`, `MC_r25_strict_main`, `MC_r25_strict_unsealed`, `MC_r25_strict_authority`). |
| `AdoptOnlyUnrowed` | #169's adoption is for outputs with no row: an entry queued as an unrowed adoption never has a committed row for its key and its output's identity. A rowed output is answered `Reuse` from its row. Checked by every pass row with `AdoptUnrowed` on; `MC_neg_reuse_ignores_row` fails it. It exists because a regression of `Store::output_matches` would otherwise be invisible: the adoption takes over, hashes and re-rows every output on every run, and reads 0 source bytes, so no R25 or S3 property fires. Rust: `transfer::tests::a_clean_rerun_reuses_rowed_outputs_and_adopts_none`. |
| `SupersedeAtomic` | #187, OI-1003-Q18, OI-1003-Q102: a superseding publish is atomic across any crash. Once it is settled (no intent recorded, no stage of its group pending), while the new file is at the leaf this store holds a row for it (its reuse row, or the ownership row the sweep gives it) and none for the old output; while the old output is at the leaf this store still holds a row for it and none for the new file. Never one file beside the other's rows. A leaf a third party wrote since is neither file. Checked by every pass row with `SupersedeMode = "exchange"`; `MC_neg_sweep_drops_ownership` and `MC_neg_exchange_before_intent` fail it. Rust: P78, `tests/power_loss.rs` (the superseding rerun), `tests/fault_harness.rs` (the `SUPERSEDING` rows). |
| `OwnershipNeverReuse` | OI-1003-Q101, R25, #86: `Reuse` is answered only from a reuse row for the entry's key and the output's identity. An ownership row (a racy publish, an interrupted supersede) names no seat and is never a reuse source. `MC_neg_own_is_reuse` fails it. Rust: `transfer_store::tests::a_racy_output_has_an_ownership_row_and_no_reuse_row`, P19. |
| `RememberedRefusalSound` | #187 review: a remembered refusal is answered only while it is still the right answer: for an unchanged seat, the path holds a file whose bytes are not the seat's. Stated on the files, not on the record. `MC_neg_refusal_unbound` fails it. Rust: `transfer::tests::an_occupied_seat_is_read_once_and_then_refused_from_its_record`, P21, P74. |
| `ExchangeRefusedUpFront` | OI-1003-Q100: on a destination with no atomic exchange nothing is ever staged to supersede an output, no intent is recorded and nothing is displaced. Trivial unless `ExchangeSupported = FALSE` (`MC_supersede_noexchange`); `MC_neg_late_exchange_refusal` fails it. Rust: `transfer::tests::a_changed_seat_without_an_exchange_is_refused_before_anything_is_staged`. |
| `Witness_LedgerManifest`, `Witness_LedgerChunkRead`, `Witness_LostRowRead`, `Witness_ExchangeRefused`, `Witness_RememberedRefusal`, `Witness_OwnershipSuperseded`, `Witness_SweepOwnership`, `Witness_SweepRestore` | Reachability witnesses ([Coverage](#coverage)): each says a scenario never happens, and its reach row must violate it. |

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
the smallest bound that reaches the break. `just formal-nv` also runs every
row inside the Haskell explorer's domain, 21 of the 33, on the explorer
([Hybrid roles](#hybrid-roles-oi-1003-q32)). Every mutation row runs with
`AdoptUnrowed = TRUE`: each break must be caught with #169's adoption in
place ([what the review found](#what-the-169-review-changed)).

| Mutation | What it breaks (the code it would undo) | Property that must fail |
|---|---|---|
| `held_before_commit` (core) | `Held{true}` before the group commit (`Inbound::answer_held`) | `HeldAfterCommit` |
| `commit_before_fsync` (core) | rename and row without the file seal (`StagedFile::publish`) | `RecordImpliesBytes` |
| `commit_before_dirseal` | row before the directory seal (`TouchedDevices::seal`) | `RecordImpliesBytes` |
| `adopt_without_seal` | adopted output recorded unsealed (`PublishSink::commit`) | `RecordImpliesBytes` |
| `ledger_before_held` | source row before `Held{true}` (`Outbound::handle`) | `LedgerAfterHeld` |
| `done_before_sync` | `SourceDone` before `committer.sync()` (`serve`) | `DoneAfterLedger` |
| `reread_durable` | no `Reuse` decision, by row or by capture record (`Inbound::entry`, `Inbound::adopt_unrowed`) | `R25_NoDurableReread`; also `S3_UnchangedReadsZero`, `S3_ReadsOnlyChanged`; and, under the exchange design only, `R25_NoCommittedCaptureReread` (`MC_neg_reread_exchange`) (four configs) |
| `reread_ignore_ledger` | no `Reuse` decision (by row or by record), and `manifest_capture` ignores the source ledger | `R25_NoCommittedCaptureReread` |
| `skip_output_row` | the group commit records no output row (`commit_outputs`) | `S3_ClosedPassIsHeld` |
| `double_read` | retained chunks dropped, so the seat is read twice (#77 F1, `serve_chunks`) | `ReadOnce` |
| `src_ledger_carries_r25` (core) | `Reuse`, by row or by record, also requires the source ledger's row | `R25_NoDurableReread` |
| `record_racy` | a racy capture recorded as a reuse key (#86, `send_capture`, `commit_outputs`) | `ReuseSound`; also `LedgerSound` (two configs) |
| `untyped_space` | a full-disk group reported as bare `IO` (#100, `space_refusal`) | `ClosureAccounted` |
| `source_write` | a capture writes the source | `S2_TypedSourceAccess` |
| `pause_writer` | a capture interrupts a source writer | `S2_TypedSourceAccess` |
| `git_optional_locks` | git without the optional-locks guard (`git_env`) | `S2_TypedSourceAccess` |
| `unbounded_backup` | the backup steps past `max_steps` (`provider_sqlite::snapshot`) | `S2_BackupLockBounded` |
| `supersede_unchecked` | WP0(d) exchange without the identity check (`owned_output` at the plan, `is_owned` on the displaced file) | `NoClobber` |
| `sweep_displaced` | WP0(d) recovery deletes a displaced foreign file (the sweep without `Destination::settle_supersedes`) | `NoClobber` |
| `adopt_unkeyed` | #169 adoption without the record's row-key check (`unrowed::prove`) | `ReuseSound` |
| `adopt_unverified` | #169 adoption without hashing the output against its record (`unrowed::prove`) | `RecordImpliesBytes` |
| `reuse_ignores_row` | `Reuse` ignores the output's row (`Store::output_matches` always false), so the adoption takes every rowed output | `AdoptOnlyUnrowed` |
| `adopt_unrecorded` | an output adopted against a manifest gets no capture record (`Inbound::adopt`, `unrowed::refresh`) | `R25_StrictNoDurableReread` |
| `owned_ignores_identity` | #187: any file at a path that has an ownership row is superseded, whatever its identity (`owned_output` without the identity comparison). A third party's file that replaced a racy publish is exchanged away and removed | `NoClobber` |
| `sweep_drops_ownership` | #187: the sweep deletes the intent of an interrupted supersede whose staged file is at the leaf without recording its ownership (`settle_supersede` with no owned identity) | `SupersedeAtomic` |
| `exchange_before_intent` | #187: the exchange runs ahead of the intent's commit (`begin_supersedes` after `io::exchange`). A crash between the two leaves the new output beside the old output's rows | `SupersedeAtomic` |
| `own_is_reuse` | OI-1003-Q101: an output the path's ownership row names is answered `Reuse` (`output_matches` reading `owned_outputs`), so a racy publish becomes a reuse source | `OwnershipNeverReuse` |
| `refusal_unbound` | #187 review: a remembered refusal is answered by its row key alone, without the identity of the file now at the path (`refused_output`) | `RememberedRefusalSound` |
| `late_exchange_refusal` | OI-1003-Q100: no exchange probe at the plan, so a changed seat is staged and its chunks asked of the source before it is refused (the code before the #187 review) | `ExchangeRefusedUpFront` |

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
- **The adoption can mask a lost `Reuse`.** With `AdoptUnrowed` on, a
  mutation that only drops the row check reads no source byte: the record
  proves the output and the adoption answers `Reuse`. So the source-read
  properties (R25, S3) cannot see it, in the model or in a test that only
  counts `source_bytes_read`. `reread_durable`, `reread_ignore_ledger` and
  `src_ledger_carries_r25` therefore break both ways to `Reuse`, and
  `reuse_ignores_row` breaks only the row check, against
  `AdoptOnlyUnrowed`.

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
- **Git carry, in BulkloadTransfer.tla.** Only estate capture's typed reads
  (one git read, the SQLite backup) are modelled there. v1's chain and base
  custody is GitCarry.tla's ([what it does not
  prove](#what-gitcarry-does-not-prove)). carry_v2 and its ingest journal
  are deleted (OI-1003-Q44, OI-1003-Q56; tag `carry-v2-final`), so there is
  nothing of them to model. The reserved git sub-stream frames and estate
  apply's `.done` journals are modelled nowhere.
- **The tree.** Directories and their records (R-N102), symlinks, `Skip`,
  engine temporaries, walk caps and devices other than the store's.
- **Storage below the store.** The Darwin barrier model belongs to
  `crash_check` (R-N88: P13, P14). SQLite corruption, torn pages and
  reordered writes under `fullfsync=OFF` are outside the model (see the
  WP0(g) caveat).
- **Larger bounds.** The checked bounds are small (the small-scope
  hypothesis). The drafted two-seat, three-run, every-fault constants were
  only simulated (`MC_main_sim`), never model-checked.
- **A failing strict ledger commit.** Under `RelaxedSourceLedger`
  `LedgerCommit` may fail: the group's rows are dropped and the session goes
  on, as `LedgerSync::Relaxed` does in the code since 2026-10-07 (#163;
  `MC_reach_wp0g_failed_commit`). A strict ledger's commit never fails in
  the model. In the code (`LedgerSync::Full`, `--source-ledger-sync=full`)
  its first failed group is sticky and the session fails before
  `SourceDone` (`LedgerSink::commit`, `Committer::submit`, `serve`), so
  `RunsClose`, `AllRunsFinish` and `ClosureAccounted` do not cover that
  mode's failure.
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

  `MC_r25_unrowed_no_adopt` (named `MC_r25_unrowed_bytes` before #169)
  makes this visible under the strict reading. Its counterexample: run 1
  seals and publishes seat `a`. The destination then loses power before
  the directory seal and the commit, and the rename survives, so the bytes
  are durable at the final path with no row. Run 2 reads `a` again. Since
  #169 the code closes the first two cases with the capture record
  ([R25's strict reading](#r25s-strict-reading-169)); a sealed salvaged
  temporary is still folded away. The model assumes every non-racy
  capture's record can be written. Where it cannot (no extended
  attributes, or an adopted output its owner cannot write), the code
  behaves as `MC_r25_unrowed_no_adopt` does, and the strict reading does
  not hold.

## R25's strict reading (#169)

OI-1003-Q40 keeps `R25_NoDurableReread` (a committed row proves the bytes
held) as the SLO's obligation. #169 closes the strict gap in code, and the
model follows it under the constant `AdoptUnrowed` (the code since #169):

- **The capture record.** An extended attribute on the output naming the
  capture its bytes are: a digest of the walked row (the seat's path and
  stat identity, with no store authority; `A/transfer/unrowed.rs
  record_key`), the manifest root and the size. In the model it is the
  file's `rec` field: the row's stat version (`RecordKey`) and the bytes.
  It has two writers:
  - `A/transfer.rs publish` writes it on each non-racy capture's staged
    file before its seal (`write_record`), so the group's file seal makes
    it durable with the data (`NewOut` at `Publish`);
  - `A/transfer.rs adopt` writes or refreshes it on an existing output
    once its bytes are verified against a non-racy capture's manifest
    (`refresh`), and the output's row records the identity after that
    write (`RecvEnd`'s adopt branch). The adoption's file seal makes it
    durable before the row commits (`SealAdopted`).
- **Adoption** (`RecvEntry`). An entry with no matching row whose existing
  output's record names the entry's row, and whose own bytes chunk and
  hash to the recorded root, is answered `Reuse` and queued as an adopted
  publication (`Inbound::adopt_unrowed`, `A/transfer/unrowed.rs prove`):
  `SealAdopted` seals it before `Commit` records its row, and `AnswerHeld`
  settles its outcome without a `Held` (the source was answered `Reuse`).
  The source reads nothing.
- **A lost source authority.** The record's key holds no authority, so a
  source store that was lost and recreated still finds its outputs'
  records, although every row key on both sides changed. Outputs with a
  record, rowed or not, are adopted without a read
  (`MC_r25_strict_unsealed`, `MC_r25_strict_authority`).
- **A third party** may rewrite an output in place, which keeps the record
  over other bytes (`ForeignWrite`'s in-place choice); the hash check then
  refuses the adoption.
- **Only unrowed outputs are adopted** (`AdoptOnlyUnrowed`). An output
  with a matching row is answered `Reuse` from the row.

**Where the strict reading holds, and where it does not.** The passing
rows show it for non-racy captures whose record could be written. It does
not hold, and the code reads the seat once more, for:

- a racy capture's unrowed output (no record, #86);
- an output on a file system without extended attributes;
- an existing output adopted against a manifest whose mode gives its owner
  no write permission (a `user.` attribute needs it), when that adoption's
  row then fails to commit;
- an output published before #169.

The model does not explore these: it assumes the record can be written
(`BulkloadTransfer.tla`, ABSTRACTIONS). The code counts them:
`transfer_capture_records_unset` when a record cannot be written, and
`transfer_unrowed_unproven` when an existing output with no matching row
has no record or one that does not verify. A record that names another row
(the seat moved since that capture) is read again and not counted.

| Config | Expect | Verdict | Violated | Distinct | Generated | Depth |
|---|---|---|---|---:|---:|---:|
| `MC_r25_unrowed_bytes` (one seat, a crash between runs) | pass | **PASS** | – | 448 | 974 | 30 |
| `MC_r25_strict_deep` (`MC_main_deep`'s bound) | pass | **PASS** | – | 461,893 | 1,801,788 | 49 |
| `MC_r25_strict_main` (`MC_main`'s bound, two seats) | pass | **PASS** | – | 963,928 | 3,081,821 | 51 |
| `MC_r25_strict_unsealed` (a lost source store) | pass | **PASS** | – | 1,050 | 2,035 | 30 |
| `MC_r25_strict_authority` (a relaxed creation commit) | pass | **PASS** | – | 1,069 | 2,084 | 30 |
| `MC_r25_unrowed_no_adopt` (the transfer before #169) | fail | **FAIL** | `R25_StrictNoDurableReread` | 302 | 624 | 17 |
| `MC_neg_adopt_unkeyed` (no row-key check) | fail | **FAIL** | `ReuseSound` | 1,787 | 4,059 | 16 |
| `MC_neg_adopt_unverified` (no hash check) | fail | **FAIL** | `RecordImpliesBytes` | 1,669 | 4,163 | 15 |
| `MC_neg_reuse_ignores_row` (the row check gone) | fail | **FAIL** | `AdoptOnlyUnrowed` | 118 | 161 | 29 |
| `MC_neg_adopt_unrecorded` (no record on a manifest adoption) | fail | **FAIL** | `R25_StrictNoDurableReread` | 724 | 1,472 | 43 |

Fail rows' counts are where the search stopped. The Rust proofs of the
same claims:

- the power-loss trace
  `materialize::adoption_power_loss::an_unrowed_output_is_adopted_without_source_reads`
  (`just resume-power-loss`), whose resumes each use a new source store;
- P74 R25-STRICT-ADOPT
  (`transfer::tests::p74_unrowed_outputs_are_adopted_without_source_reads`);
- `an_output_adopted_against_a_manifest_carries_its_capture_record` and
  `a_stale_capture_record_is_refreshed_by_a_manifest_adoption`
  (`adopt_unrecorded`'s scenario on the code);
- `a_recreated_source_store_adopts_from_capture_records`;
- `a_clean_rerun_reuses_rowed_outputs_and_adopts_none`
  (`AdoptOnlyUnrowed`).

### What the #169 review changed

The first #169 revision kept 32 rows on the transfer before #169 and said
every mutation still failed on its named property. That held only because
of the pin. The review (2026-10-06) found, and this revision fixes:

- **Five mutation rows passed with the adoption on.**
  `MC_neg_reread_durable`, `MC_neg_reread_unchanged`,
  `MC_neg_reread_changed_only`, `MC_neg_reread_exchange` and
  `MC_neg_reread_ignore_ledger` found no error under `AdoptUnrowed = TRUE`:
  the adoption answered `Reuse` where the mutation had removed it. Once
  every mutation row ran with the adoption on,
  `MC_neg_src_ledger_carries_r25` passed too, for the same reason. Now
  those three mutations remove both ways to `Reuse`, every
  mutation row runs with the adoption on, and `reuse_ignores_row` with
  `AdoptOnlyUnrowed` covers the break that reads no source byte.
- **A manifest adoption wrote no record**, so an adoption whose row never
  committed was read again. The model could not see it: the strict ghost
  was set only at `Publish`. Now `RecvEnd` writes the record, `SealAdopted`
  sets the ghost, and `adopt_unrecorded` is the old behaviour as a
  mutation.
- **The record's key held the source authority**, so the strict rows
  failed once the authority was lost (an unsealed state root, or a relaxed
  creation commit). The key is now the walked row alone.
- **The explorer's domain was the transfer before #169.** It now models
  the record and the adoption ([N-version core](#n-version-core-oi-1003-q32)).

### Rows that model the transfer before #169

Seven rows keep `AdoptUnrowed = FALSE` (`before169` in the catalogue;
`pre-#169` in the Results table). Their verdicts say nothing about the
code since #169:

| Row | Why it keeps the old transfer |
|---|---|
| `MC_nv_core` | A frozen name with a count of record (15,834). `MC_nv_core_adopt` is its bound for the code since #169. |
| `MC_nv_ledger` | The core's second row (142,450). `MC_nv_ledger_adopt` is its bound for the code since #169. |
| `MC_reach_ledger_manifest`, `MC_reach_ledger_chunks` | Reach rows at `MC_nv_ledger`'s bound: they show that row explores both ledger branches. |
| `MC_r25_unrowed_no_adopt` | The finding #169 closes: the strict reading fails without the record. |
| `MC_store_root_unsealed` | The finding that a lost source store re-keys every row and re-reads held bytes. With the record it no longer does for outputs that carry one (`MC_r25_strict_unsealed` passes `R25_NoDurableReread` at this bound); an output without a record is still re-read. |
| `MC_wp0g_authority` | The same for a relaxed creation commit (`MC_r25_strict_authority`). WP0(g)'s condition stands: it was ratified on this row (OI-1003-Q37), and a record is not guaranteed. |

### Rows that model the transfer before #187

Every row whose constants are not tagged `exchange` or `check_rename` keeps
`SupersedeMode = "off"`: the transfer without superseding publish, which
was the code until #187 (2026-10-07). In that transfer a changed seat whose
output exists is adopted if its bytes verify and refused otherwise. The
rows were not moved, for three reasons:

- **Their verdicts still hold for the code** wherever no output is
  superseded: a first pass, an unchanged rerun, a crash and its resume, a
  lost source store. Those are the behaviours R25, the Held ordering, the
  ledger properties and S2 are about. An `off` row says nothing about a
  changed seat's replacement.
- **Their counts are of record.** `MC_nv_core`, `MC_nv_ledger` and their
  `_adopt` rows are frozen names whose distinct-state counts the Haskell
  explorer must reproduce, and the explorer has no WP0(d) action ([N-version
  core](#n-version-core-oi-1003-q32)). Moving them needs a ruling, as for
  #169.
- **The superseding publish is checked on rows of its own**, at the two
  main bounds: `MC_supersede_main` and `MC_supersede_deep`, with
  `MC_wp0d_exchange`, `MC_supersede_noexchange`, the two strict rows and
  the mutation rows of [#187's
  records](#187s-records-in-the-model-oi-1003-q102).

What is not yet checked with the superseding publish on: the relaxed source
ledger (`MC_wp0g`, `MC_wp0g_deep`), a lost source authority, estate reads
(`MC_s2`) and liveness (`MC_live`). None of those properties reads
`SupersedeMode`, but no row combines them. R25's strict reading is checked
with it since 2026-10-07 (`MC_supersede_strict_main`,
`MC_supersede_strict_deep`).

### #187's records in the model (OI-1003-Q102)

OI-1003-Q102 (2026-10-07): model first, then merge. The review of #187
added three records to the destination store and one refusal; until this
revision none was in the model, and this section replaces the gap note
that said so. They exist only under `SupersedeMode = "exchange"`, the code
since #187. With it off every new variable keeps its initial value, so no
other row's state graph moved ([Results](#results)).

State:

| Variable | Table | What it holds |
|---|---|---|
| `intent` | `supersedes` | Per seat, the intent of a superseding publish not yet settled: the staged file's identity, the identity this store recorded for the output it replaces, and the path's rows, moved out of the store when the intent commits. |
| `dOwn` | `owned_outputs` | Per seat, the identity in the path's ownership row: this store's own output with no reuse row. Written for an output published or adopted from a racy capture (OI-1003-Q101), and by the sweep for an interrupted supersede whose staged file is at the leaf. |
| `dRefused` | `refused_outputs` | Remembered refusals: the entry under a row key was refused, with a code, for the file of a given identity at its path. |
| `ExchangeSupported` (constant) | the probe, `Destination::exchange_supported` | OI-1003-Q100: whether the destination has the atomic exchange. `FALSE` refuses a superseding seat `DESTINATION_EXCHANGE_UNSUPPORTED` before anything is staged. There is no fallback. |
| `lastSup`, `reuseBad`, `wrongRefusal` | (ghosts) | The two files of a seat's last superseding publish, for `SupersedeAtomic`; and the two flags `OwnershipNeverReuse` and `RememberedRefusalSound` read. |

Actions, each with the code it models (the spec's comments and its code
map say the same):

| Action or branch | What it does | Code |
|---|---|---|
| `RecvEntry`, the remembered refusal | After the reuse row and ahead of the capture record: an entry whose row key and whose file at the path are those of a remembered refusal is refused with the same code; the source reads nothing. | `Inbound::entry`, `Inbound::remembered_refusal`, `Store::refused_output` |
| `RecvManifest`, the plan | An output is this store's own when its identity is in a reuse row or the ownership row. Owned: superseded; or, with no exchange, adopted when its bytes are the manifest's and planned for refusal otherwise, with nothing staged and no chunk asked for. | `plan_file`, `owned_output`, `Store::output_rows`, `Destination::exchange_supported` (`Plan::NoExchange`) |
| `RecvEnd`, the refusals | `DESTINATION_EXCHANGE_UNSUPPORTED` for the plan above; `DESTINATION_OCCUPIED` for a file that does not verify. Either is remembered when the capture was not racy. | `Inbound::end`, `end_filling`, `Inbound::remember_refusal`, `verify_settled`, `Store::remember_refused_output` |
| `BeginSupersede` | The leaf must still hold the owned output, or the entry is refused. One commit writes the intent and moves the path's rows into it. | `StagedFile::prepare_supersede`, `PublishSink::supersede`, `begin_supersedes` |
| `Exchange` | The staged name and the leaf trade files atomically. A leaf that vanished exchanges nothing, and the intent is settled without its rows. | `StagedFile::exchange`, `io::exchange` |
| `VerifyDisp` | The displaced file is removed when it has the identity the intent recorded; any other file is exchanged back, or kept aside with its intent. | `StagedFile::exchange` (`is_owned`, `Exchanged::Done`, `Restored`, `Stranded`) |
| `Commit` | Settles each superseding publish of the group beside its new row. A racy output gets the ownership row and no reuse row. | `commit_outputs`, `settle_in`, `row_in`, `own_in` |
| `CommitFail` | A failed group settles nothing: the intent stays for the sweep. | `PublishSink::commit`, `space_refusal` |
| `StartRun`, the sweep | For each unsettled intent: rows back when the old output is still at the leaf; the ownership row when the staged file is; a displaced owned output removed; a displaced file of anyone else exchanged back, or kept aside with its intent. | `Destination::settle_supersedes`, `published`, `supersede_intents`, `settle_supersede` |

The invariants, stated precisely in the spec and in [the property
table](#properties-slos-rulings-and-tests):

- **`NoClobber`**: a destination file is replaced or removed only when its
  identity is one this store recorded, in a reuse row, the ownership row,
  or a row an unsettled intent holds.
- **`SupersedeAtomic`**: after any crash, the old output with its old rows
  or the new output with a row of its own, never mixed.
- **`OwnershipNeverReuse`**: ownership never implies reuse; a racy output
  is never a reuse source (R25, #86).
- **`RememberedRefusalSound`**: a remembered refusal is answered only
  while the path still holds other bytes than the unchanged seat's.
- **`ExchangeRefusedUpFront`**: with no atomic exchange, nothing is staged,
  recorded or displaced for a superseding seat (OI-1003-Q100).
- **`R25_NoDurableReread`** and **`R25_StrictNoDurableReread`** hold with
  the superseding publish on: every exchange pass row checks the first,
  and `MC_supersede_strict_main` and `MC_supersede_strict_deep` the second
  under `TrackStrictHeld`.

Seven mutations fail on their named property: the six [new
ones](#mutations) and `supersede_unchecked` (the supersede without the
ownership check, `NoClobber`), which now runs through the intent. Five
reach rows show the new branches are explored ([Coverage](#coverage)).

What the model still does not hold, or abstracts:

- **One intent per seat.** The code keys an intent by its staged name, so
  a path whose displaced file is kept aside could be superseded again.
  Here a seat with a kept-aside file is never superseded again.
- **`begin_supersedes` never fails**, and commits per seat, not per group.
  A full disk under it is `CommitFail`'s case only for the group commit.
- **The publish's last look** at the output is folded into `Exchange`.
  That is a superset of the code's behaviours: every third-party change
  after the intent is treated as landing in the window the look cannot
  close.
- **Stamps and clocks.** The intent's size and mtime stamp of the staged
  file, the ctime an exchange moves, and the destination's racy window for
  a remembered refusal (`verify_settled`) are not modelled: any write
  moves a model identity. So a refusal remembered from a racy capture, or
  against an unsettled file, cannot be shown wrong here; the Rust tests
  hold those two guards (`a_racy_capture_is_sent_but_never_recorded`,
  `an_occupied_seat_is_read_once_and_then_refused_from_its_record`).
- **Ownership by path.** `OwnIds` ignores the source authority that keys
  the code's rows. A recreated source store finds no owned output in the
  code until a run has adopted it; the model would supersede it. No
  exchange row loses the authority.
- **The exchange probe** is one constant, not an answer per device and
  session, and a refusal for a missing exchange is never re-probed.
- **Chunks of a superseded output** (`materialize::Displaced`) are not
  modelled: chunks are abstracted.
- **The explorer** does not have these records ([its
  domain](#the-explorer)).

## Code and design disagreements

The model follows the code where the code and docs/design.md differ:

- **WantManifest.** design.md says it is chosen when the output exists or
  published outputs hold chunks. `Inbound::entry` also chooses it when the
  sweep salvaged any temporary (`self.target.salvaged() > 0`).
- **Superseding publish (WP0(d)).** Implemented by #187 (2026-10-07) as the
  exchange design. `SupersedeMode = "exchange"` models the code, its
  intent, ownership row and remembered refusal included ([#187's
  records](#187s-records-in-the-model-oi-1003-q102));
  `"off"` models the transfer before it ([WP0(d)](#wp0d-exchange-versus-check-then-rename-oi-1003-q18),
  [the rows that keep it off](#rows-that-model-the-transfer-before-187)).
- **A refused seat's sniff (#186).** The model has no content-based source
  refusal: `SQLITE_STATE_CHANGED` is not in `TypedCodes`, and a refused
  seat's 16 sniffed bytes are not in `reads`. So that the source remembers
  such a refusal and does not sniff an unchanged seat again is not a model
  property; P21 and P23 with refused seats check it on the code.
- **WP0(g).** Built 2026-10-07 (OI-1003-Q104) as ratified
  ([WP0(g) as built](#wp0g-as-built-2026-10-07)): the source ledger's row
  commits are relaxed by default, and nothing else is.
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

  Under the strict reading the code before #169 re-read durable bytes that
  have no row (`MC_r25_unrowed_no_adopt`; [Not proven here](#not-proven-here)).
  OI-1003-Q40 (2026-10-04) made the committed-row reading the SLO's
  obligation. Since #169 the strict reading also holds in the model, for
  non-racy captures whose record could be written ([R25's strict
  reading](#r25s-strict-reading-169)).
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
- **A failed ledger commit fails the session, in the strict mode only.**
  Under `LedgerSync::Relaxed`, the default, it is counted and the session
  goes on (#163). `--source-ledger-sync=full` keeps the behaviour before
  WP0(g) ([Not proven here](#not-proven-here)).
- **R25's model obligation in slo.md** is worded as
  `R25_NoCommittedCaptureReread`, which is vacuous in code shape
  ([Properties](#properties-slos-rulings-and-tests)).

## GitCarry: chain and base custody (OI-1003-Q43, OI-1003-Q46)

[`GitCarry.tla`](GitCarry.tla) is lane L4 of the Q42 git-carry push. It
models v1 git carry's custody of what a capture depends on:

- the plan base a group of items shares (`prepare_base`,
  `shared-{group}.base`);
- the chain links of a capture that declares its retained predecessor's
  tips as prerequisites (WP2, `{bundle}.prior`), and the chain's depth
  (`CHAIN_DEPTH_LIMIT`);
- Q46's re-root policy and CORPUS GC (lane L8, no code yet);
- L6b's fix 2, a chain kept under a plan base (the code's only policy
  since lane L6b, `decide::Policy::CODE`);
- crashes between the bundle, its sidecars and the `{item}.capture`
  record;
- lane L7's reuse manifest (`{bundle}.reuse`, `publish_reuse`), as a file
  that exists or not, published after the dependency sidecars and before
  the record (`ReuseSidecar`; the constant `ReuseManifest` turns the step
  on);
- content names: a bundle's CORPUS name is its digest, so a re-export of
  the same content lands on the same file. `publish_bundle` and
  `prepare_base` reuse it, link a deleted one again, or refuse
  `DIGEST_MISMATCH` when the name holds other bytes, and `publish_prior`
  keeps an intact `.prior` already recorded for the name;
- what a bundle's header declares (its prerequisite tips) and what its
  `.prior` names, as two values, each set from the capture's chain path;
- third-party damage to CORPUS, source history moving and being rewritten;
- what estate-apply would do with every record (restore or a refusal),
  and what the next capture would do with it (reuse, recapture or a
  refusal).

It models the code at **8dc26c1** (`origin/main` after #170), re-checked
at **cdfe5f4** (after #171, #172 and #173) and at **b8521c2** (after #175,
#176 and #177). #172 adds `refuse_bare_capture` to `apply_item`, before
any custody step. #177 (lane L1) makes grouped and chained bundles thin,
which changes pack contents only: `estate.rs`, where every custody step
lives, is unchanged. Every code symbol below is an item definition in the
non-test Rust sources at all three; `tla-check` checks that again on every
run, so a symbol that only a data file (`decide_rows.tsv`), a test or a
comment names does not ground. The module's header states the scope, the
abstractions and the code map.

Lane L6a (OI-1003-Q43) moved the decision into
[`git_carry/decide.rs`](../../crates/bulkload-agent/src/git_carry/decide.rs)
without changing what the code does: bundles, records and restores are as
before, and every existing git carry test passes unchanged. `write_capture`
replaced `write_chained`, whose only caller was `export_pass`'s match, and
`chainable` and `chain_offer` keep their names: the first now only reads
what the decision needs, the second acts on the decision's plan. The
catalogue cites `write_capture`, and decide.rs's `decide`, `Rebase` and
`Inputs`. Each is a definition at L6a's head.

### Roles (OI-1003-Q43)

| Role | Tool | What it does | Run |
|---|---|---|---|
| Decision core of record | Rust [`git_carry/decide.rs`](../../crates/bulkload-agent/src/git_carry/decide.rs), lane L6a | A pure, total `decide(&Inputs) -> Decision`. It replaced the logic of `ExportOptions{prerequisite, chain}`: the "Ignored when prerequisite is set" rule (`chain_offer`'s filter), `chainable`'s rules and `export_pass`'s match. `prepare_base`, `capture_item` and the writer `shared::write_capture` call it and act on the decision. | P67 |
| Reference decision core, a differential oracle | Haskell `decide` in [`hs/GitCarryCore.hs`](hs/GitCarryCore.hs) | Derived by hand from the v1 rules at main and Q46's re-root policy. It renders 363 pinned rows to [`crates/bulkload-agent/tests/data/decide_rows.tsv`](../../crates/bulkload-agent/tests/data/decide_rows.tsv), which L6's fixed-seed property test (P67) checks the Rust `decide` against. | `just formal-nv` (`rows --check`) |
| Typed closed unions | Dhall: `Basis`, `Rebase`, `ReuseEligibility`, `Refusal`, `Decision` in [`catalogue/Types.dhall`](catalogue/Types.dhall) | The labels of what `decide` returns. Each union's table is merged over the union, so the label lists hold exactly its labels. `formal-nv` requires GitCarryCore's constructors (`schema`, from total case analyses, not a literal list) to equal them; `tla-check` requires GitCarry.tla's `Decisions`, `Bases`, `Rebases`, `Reuses` and `Refusals` sets to equal them. | `just formal-nv`, `just tla-check` |
| Custody and crash order | TLA+ with TLC: [`GitCarry.tla`](GitCarry.tla) | Eight safety invariants and one liveness property over chain links, the plan base, GC, the reuse manifest and crash order. TLC checks only this; the decision's own rules are the pinned rows' job. | `just tla-check` |
| Explorer parity | Haskell `explore` in GitCarryCore.hs | An explicit-state BFS of GitCarry.tla whose capture step calls the Haskell `decide`. It must reach TLC's state counts, so TLC's `DecideCore` and the Haskell `decide` agree on every input reachable in those bounds. | `just formal-nv` |

This widens OI-1003-Q32 for this layer. Haskell is no longer only a second
encoding of the spec: through the pinned rows it checks the code (P67,
since L6a). The chain is TLA+ (custody) to Haskell `decide` (explorer
parity) to Rust `decide.rs` (P67). The explorer itself has the limit
Explorer.hs has (independent code, shared design): it and the spec were both
transliterated from the same reading of the Rust code, so parity cannot
catch a misreading they share. The pinned rows can: P67 checks the Rust
`decide`, which the code calls, against every row of all three lanes (the
policy is a column of each row).

L6a grounds `decide.rs`. `ChainDepthBounded` and `BrokenLinkNeverReuseHit`
cite `decide` and a type only decide.rs defines (`Rebase`, `Inputs`): the
bare name `decide` also matches transfer.rs's `Outbound::decide`, so it alone
would not show that decide.rs exists. Every traceability row lists the
symbols no code has yet as **pending**, typed with the lane that lands them
(`T.Lane`), and `tla-check` prints them without grepping.

### The decision core

`decide :: Inputs -> Decision` is what a v1 capture decides before it
exports. The inputs are what the code reads first:

- the group's base record (`prepare_base`): absent, retained or lost;
- the item's record (`retained_capture`): none, a bundle gone, or held;
- for a held record: the key, drift and pass-start conditions of a hit;
  the retained bundle's shape (self-contained, based or chained); whether
  its chain is intact (`chain_links` under `LinkBinding::Custody`); its
  bound base; its chain depth and root age; whether the source still
  holds its tips and its root's tips (`source_held_tips`); a shallow
  source;
- the policy: the depth limit, Q46's root window, fix 2.

The decision is a hit, a typed refusal, or an export with a basis
(`SelfContained`, `Base`, `Chain`, `BaseAndChain`), a depth, a rebase
(`NoRebase`, `NewRoot`, `Reroot`) and the reuse offer (`NoRetained`,
`BlobReuse`, `PassStartUnrecorded`). The rules, each from the code at
8dc26c1:

- **A lost base refuses.** `prepare_base` refuses `RECEIPT_BINDING_INVALID`
  rather than replace a base that older deltas depend on.
- **No record, or a gone bundle, captures afresh**, with no reuse offer
  (`Retained::None`).
- **A hit** needs the same key, no drift, a settled pass start and a
  restorable chain. A hit on a based bundle needs its bound base retained,
  else `RECEIPT_BINDING_INVALID`.
- **Otherwise the capture extends.** v1's `chainable` never chained on a
  broken chain or a based bundle, and chained on a prior only below
  `CHAIN_DEPTH_LIMIT`. Its `chain_offer` dropped the link under a plan base
  (`ExportOptions.chain` was "Ignored when prerequisite is set").
  `export_pass`'s match then wrote a chained bundle (`write_chained`),
  except for a shallow source or when the source held none of the prior's
  tips. In those cases, and with no link, it wrote a based or
  self-contained one (`write_bundle`). At the limit, v1 re-bases: a
  self-contained bundle, which re-packs the item's whole history.

Since L6a these rules are `git_carry::decide::decide`, and the code reads
its inputs in stages, deciding at each. `prepare_base` decides on the base
alone. `capture_item` decides on the record, reading a retained bundle's
bound base only when the decision rests on it. The writer
`shared::write_capture`, which `export_pass` calls, decides again on the
offer once the pass has read whether the source is shallow. It queries the
source's tips only when the decision rests on them, exactly where v1 did.
Each stage is a function of `decide.rs` that takes its lazy read as a
callback (`decide_recorded`, `decide_offered`), so P67 checks that the
stages the code runs, composed, decide what one call on all the inputs
decides, under the code's policy (fix 2, `Policy::CODE`) and under v1's.
An estate test drives the bound-base read through `capture`
itself: a retained based bundle bound to a lost base refuses
`RECEIPT_BINDING_INVALID` while the group's regenerated base is retained.

**Q46's re-root policy, as this model reads it.** The ruling (Decision 4,
option A) is "a re-root policy plus STATE/corpus GC that never deletes a
link something depends on". The plan's L8 text is "at the depth limit,
chain on the root's tips, with a policy that advances the root". The model
fixes that as follows, with `RootWindow` captures per root (0 is v1):

- A capture whose root would reach the window starts a new root
  (`NewRoot`), so a full re-pack happens at most once per window.
- At the depth limit, a capture chains on its chain's root (`Reroot`,
  depth 1) instead of re-packing, while the source still holds the root's
  tips.

The window's value, and whether the age rule should also apply below the
limit, are L8's to set. This is an open question, not a ruling.

**Bounds that reach a re-root's consequences.** Along a chain the root
age equals the depth until the first re-root, which happens at depth `L`,
so a re-root bundle's age is at least `L + 1`. At `RootWindow =
DepthLimit + 2`, as in `MC_gc_q46`, the capture after a re-root already
ends the window, so that row never extends a re-rooted chain and never
re-roots twice. Two more rows do, each with a reach row that shows the
state is reached:

- `MC_gc_reroot_extended` (`L2 W5`): a capture chains on a re-root bundle,
  so `RootOf`, GC and restore run through it
  (`MC_gc_reach_reroot_extended`);
- `MC_gc_reroot` (`L1 W4`): a chain re-roots twice, so GC can collect the
  re-root bundle the second re-root abandons (`MC_gc_reach_second_reroot`).

`MC_gc_fix2_deep` (`L2`) keeps a chain of two links under the plan base,
and `MC_gc_reach_based_chain` shows a restore that imports the base and
flattens both links.

**The pinned rows.** `rows` renders three lanes' rows:

| Lane | Policy | Rows |
|---|---|---:|
| `v1` | the code before L6b: depth limit 8, no re-root, no chain under a base | 79 |
| `L6b` | fix 2, the code's policy (`Policy::CODE`): a chain kept under a plan base | 85 |
| `L8` | Q46: root window 27 (a row parameter, not a ruling), fix 2 | 199 |

Each row covers a family: a lost base; no record or a gone bundle; the hit
path on each shape; each reason to leave it; the extend path over shape,
depth (1, 7, 8), held tips and root, and age; a shallow source; an
unrecorded pass start. A `-` input means `decide` must not depend on that
input in that row. `rows` checks this over the input's whole domain
(every union value and Bool, and depths and ages around the limit and the
window) and refuses to render otherwise. It also refuses unless every label
of every closed union is reached by some row. P67 draws `-` inputs. `rows
--check FILE` is byte-identical or fails, and runs in `formal-nv`.

### Properties

| Property | Statement | SLO | Rulings | Code symbols (pending) | P-tests |
|---|---|---|---|---|---|
| `ChainDepthBounded` | Every bundle's chain is at most the limit deep, so a restore stages at most limit + 1 bundles. Under Q46 its root is younger than the window. | S3, S4 | OI-1003-Q15, OI-1003-Q46 | `CHAIN_DEPTH_LIMIT`, `chainable`, `chain_offer`, `chain_links`, `ExportOptions`, `export_pass`, `write_capture`, `decide`, `Rebase` (the re-root window L8) | P67, P71 |
| `PrereqsSatisfiedByEarlierLinks` | A restore whose digests check never fails `verify_bundle`: every bundle it applies (the oldest link's base first, under fix 2) declares only the prerequisite tips of intact bundles applied before it. A bundle's header (`ExportOptions.chain`'s tips, or the base's) and its `.prior` (the link `Prior`) are separate values in the model, as in the code, so this is the claim that a capture keeps them in step. | S4 | OI-1003-Q15, R-N72 | `flatten`, `prerequisites`, `verify_bundle`, `source_held_tips`, `write_bundle`, `write_capture`, `chain_links`, `bind_base` (a re-root's header prerequisites and `.prior` from one chain path L8) | P68, P67 |
| `BrokenLinkNeverReuseHit` | A capture never reuses a record whose custody is broken: a missing or rewritten link, a lost base, a missing sidecar. | S3, S4 | OI-1003-Q15, R-N72 | `retained_capture`, `chain_links`, `LinkBinding`, `decide`, `Inputs` | P42, P67 |
| `BaseNotReplacedWhileDepended` | The plan base record never moves while a record depends on its base. | S4 | OI-1003-Q15, R-N72 | `prepare_base`, `retained_base`, `requires_base`, `write_new` | P68 |
| `GCNeverDeletesDepended` | GC never removes a bundle that a record or the base record depends on. GC's own choice is definitional (it collects one bundle that nothing depends on per step); what the invariant checks is that no later step makes a record depend on a collected bundle. It holds only with one CORPUS writer at a time: L8's GC must take a CORPUS-level exclusive lock that every capture and every apply also take. | S4 | OI-1003-Q46 | `chain_links` (STATE and CORPUS GC L8; GC's CORPUS-level exclusive lock L8) | P71 |
| `SidecarsBeforeRecord` | A record names a published bundle whose dependency sidecars exist. | Durability, S4 | OI-1003-Q15, R-N86 | `capture_item`, `publish_bundle`, `publish_prior`, `publish_sidecars` | P70 |
| `ReuseManifestBeforeRecord` | Lane L7: a record names a bundle whose reuse manifest (`{bundle}.reuse`) exists, so the pass after any crash reuses that capture's blobs from its manifest and does not fetch the bundle to learn what it holds. No restore and no decision reads the manifest (`ApplyOutcome` and `DecideCore` do not mention it), so either order restores; the invariant is the order's own claim. Checked where `ReuseManifest = TRUE` (`MC_gc_reuse`); vacuous elsewhere. That a manifest matches its bundle, and that reuse reads no bundle byte, are P70's and P69's (Rust). | Durability, S3 | OI-1003-Q42, OI-1003-Q45, OI-1003-Q94 | `capture_item`, `publish_reuse`, `ReuseManifest`, `reuse_sidecar`, `retained_manifest`, `reuse_offer`, `manifest_blobs`, `ManifestSeat` | P70, P69 |
| `RestoreOrRecapture` | Every record restores, or its item's next capture recaptures, or it refuses by name and keeps the missing custody visible. An export whose content name holds rewritten bytes ends in `publish_bundle`'s `DIGEST_MISMATCH`, a refusal by name, so it counts. An apply that does not restore is a typed refusal, never a bare IO. | S4, S5 | OI-1003-Q1, OI-1003-Q46, R-N72 | `apply_item`, `import_base`, `stage_base`, `stage_bundle`, `retained_capture`, `bound_base` (GC L8) | P68, P69, P71 |
| `ChainRecovery` | Under `WF_vars(Protocol)`, once the environment stops, every item whose record does not restore (or has none) gets one that does. Claimed where damage only deletes bundles and never reaches a base (`MC_gc_live`); a bundle rewritten in place defeats it while the source holds still (`MC_gc_live_rewritten`, [Findings](#findings)). | S4, S5 | OI-1003-Q46 | `chainable`, `retained_capture`, `publish_bundle`, `prepare_base` (the re-root window L8) | P71, P68 |

`TypeOK` also evaluates the decision and the restore on every state, so a
partial or ill-typed definition is a TLC error rather than a silent gap.
There is no `PackExcludesHeld`: what a pack excludes is the Rust pack scan's
job (P64, lane L1).

### Mutations

| Mutation | What it breaks (the code it would undo) | Property that must fail |
|---|---|---|
| `chain_ignores_depth` | `chainable`'s `CHAIN_DEPTH_LIMIT` check | `ChainDepthBounded` |
| `gc_deletes_depended` | Q46 GC keeps only what the records name, forgetting their chain links | `GCNeverDeletesDepended` |
| `base_replaced_live` | `prepare_base` replaces a missing base that older deltas depend on | `BaseNotReplacedWhileDepended` |
| `sidecar_after_record` | `capture_item` writes the record before `publish_prior` | `SidecarsBeforeRecord` |
| `skip_flatten_verify` | `chain::flatten` restores without checking digests, the oldest link or prerequisites | `PrereqsSatisfiedByEarlierLinks` |
| `hit_ignores_chain` | `retained_capture` drops `restorable` | `BrokenLinkNeverReuseHit`; also `RestoreOrRecapture` (`MC_gc_neg_hit_ignores_chain_restore`) |
| `reroot_pre_mismatch` | A re-root declares the head's tips in its header while its `.prior` names the root: L8 deriving `ExportOptions.chain` and the link `Prior` from different chain paths | `PrereqsSatisfiedByEarlierLinks` |
| `reuse_after_record` | `capture_item` writes the record before `publish_reuse` (lane L7) | `ReuseManifestBeforeRecord` |

The plan named five mutations. `hit_ignores_chain` is a sixth: none of the
five can reach `BrokenLinkNeverReuseHit`. The catalogue asserts, as for
BulkloadTransfer, that every safety invariant but `TypeOK` has a fail row,
and the sixth mutation is what gives `BrokenLinkNeverReuseHit` one.
`reroot_pre_mismatch` is a seventh. `skip_flatten_verify` breaks
`PrereqsSatisfiedByEarlierLinks` on the apply side, with a link rewritten;
this one breaks it on the capture side with every digest intact, which the
model can say only because a bundle's declared prerequisites (`pre`) and
its `.prior` are separate fields. `RestoreOrRecapture` has one fail row,
`MC_gc_neg_hit_ignores_chain_restore`; the finding
`MC_gc_base_missing_untyped` was its second until lane L6b fixed the code
(#181), and is now an expected pass. `reuse_after_record` is an eighth
(lane L7): it reverses the manifest's order, and
`MC_gc_neg_reuse_after_record` fails `ReuseManifestBeforeRecord` in the
state after the record, before the manifest step runs.
`MC_gc_neg_base_replaced_live` runs at one
commit: a base exported again at the same tip has the same content name,
so it is the same file, and only a later tip can replace it.

### Results (GitCarry)

The run of record (review round, 2026-10-05): `just tla-check` on host
sting (32 cores, Linux 6.12, TLC 2.19 on OpenJDK 8), under the shared lock,
at a load average between 20 and 30 from other lanes, over the spec and
configs committed in `ba6b85c` (on `origin/main` b8521c2, merged at `4fe39b3`).
It ran GitCarry's 23 rows in two invocations, each after the module's
budget self-test: 00:46 to 00:54 EDT (the core, the grouped bound, the
findings and every mutation row; 61 s of TLC) and 00:54 to 01:10 EDT (the
Q46, fix 2 and liveness rows and the reach rows; 931 s). **All 23 rows
matched their expectation:** 8 PASS, 3 REACHED, 11 FAIL and the
INCONCLUSIVE self-test, which matched in both invocations. Every pass row's
never-enabled actions equalled its `never` column. Before any TLC run the
grounding step found, for GitCarry.tla, 31 operators, 14 constants, 7
mutations, 5 closed-union label sets and 24 code symbols, each an item
definition in the non-test Rust sources, and printed its 8 pending
symbols. The slowest row was `MC_gc_live` at 304 s (liveness); the core,
`MC_gc_core`, took 10 s. Peak RSS was 1,805 MiB. BulkloadTransfer.tla and
its configs are unchanged since the first round's run, in which its 43 rows
matched; its grounding (66 operators, 16 constants, 19 mutations, 19 code
symbols) passed again under the new rule.

L6a's check (2026-10-05, sting, load about 6): `just tla-check MC_gc_core`
grounded 27 GitCarry code symbols (with `write_capture` for `write_chained`,
and `decide`, `Rebase` and `Inputs`) and printed 7 pending ones. The
self-test was INCONCLUSIVE as expected, and `MC_gc_core` passed at 45,062
distinct states, unchanged. `just formal-nv` matched all 55 rows, 0
differing, and `rows --check` found the 363 rows current.

L6b's check (2026-10-07, sting): lane L6b lands fix 2 as the code's only
policy and fixes #181, so `MC_gc_base_missing_untyped` now sets
`BaseMissingTyped = TRUE` and is an expected pass. `just tla-check
MC_gc_base_missing_untyped` grounded 31 GitCarry code symbols (with
`bind_base`, `bound_base`, `stage_base` and `write_new` for L6b's two
pending symbols, which are now grounded) and printed 5 pending ones, all
L7's and L8's. The self-test was INCONCLUSIVE as expected, and the row
passed at 137 distinct states with `Advance`, `Crash`, `GC` and `Rewrite`
never enabled, as its `never` column says. The other 21 rows' constants are
unchanged (only the comments of `MC_gc_grouped`, `MC_gc_fix2` and
`MC_gc_fix2_deep` moved), so they were not run again; the table below
keeps their run of record. `just tla-render --check` found all 68 files
current, and `rows --check` found the 363 pinned rows byte-identical.

L7's check (2026-10-07, sting): lane L7 lands the reuse manifest
(`{bundle}.reuse`), so the model gains the variable `manifest`, the action
`ReuseSidecar`, the constant `ReuseManifest`, the invariant
`ReuseManifestBeforeRecord` and the mutation `reuse_after_record`. The
constant is `FALSE` in every row that existed, so each keeps its state
count of record (and the explorer's presets their pins); the step is
checked in two new rows, `MC_gc_reuse` and `MC_gc_neg_reuse_after_record`.
One `just tla-check` run over all 25 GitCarry rows (04:24 to 04:41 EDT,
1,009 s, peak RSS 1,792 MiB) matched every expectation: 10 PASS, 3
REACHED, 11 FAIL and the INCONCLUSIVE self-test. Every pass row's distinct
and generated counts equal the table's, and its never-enabled actions
equal its `never` column, which now lists `ReuseSidecar` wherever the
constant is off. The grounding step found 33 operators, 15 constants, 8
mutations, 5 label sets and 38 code symbols for GitCarry.tla (L7's
`publish_reuse`, `ReuseManifest`, `reuse_sidecar`, `retained_manifest`,
`reuse_offer`, `manifest_blobs` and `ManifestSeat` among them) and printed
4 pending ones, all L8's: L7's pending symbol (the `.reuse` sidecar) is
grounded. `just tla-render --check` found all 81 files current. `just
formal-nv` matched all 67 rows, 0 differing; `rows --check` found the 363
pinned rows byte-identical (the reference `decide` did not change: the
manifest is no input of the decision), and the schema check passed (5
unions). The two new rows below are that run's; the others keep their run
of record.

| Config | Constants | Expect | Verdict | Violated | Distinct | Generated | Diameter | Wall | RSS MiB |
|---|---|---|---|---|---:|---:|---:|---:|---:|
| `MC_gc_budget_selftest` | i1 L2 W4 gc C4 R1 X1 D1 budget 5 s | inconclusive | **INCONCLUSIVE** | `WithinBudget` | 42,853 | 82,795 | 16 | 7s | 713 |
| `MC_gc_core` | i1 L2 C3 R1 X1 D1 | pass | **PASS** | – | 45,062 | 81,520 | 29 | 10s | 768 |
| `MC_gc_q46` | i1 L2 W4 gc C4 R1 X1 D1 | pass | **PASS** | – | 699,419 | 1,606,770 | 39 | 124s | 1792 |
| `MC_gc_grouped` | i1,i2 L2 C2 X1 D1 db typed | pass | **PASS** | – | 85,941 | 184,923 | 35 | 19s | 1158 |
| `MC_gc_fix2` | i1,i2 L1 W3 cub gc C2 X1 D1 db typed | pass | **PASS** | – | 392,515 | 807,467 | 39 | 113s | 1743 |
| `MC_gc_reroot` | i1 L1 W4 gc C4 R1 X1 D1 | pass | **PASS** | – | 675,517 | 1,552,268 | 39 | 106s | 1788 |
| `MC_gc_reroot_extended` | i1 L2 W5 gc C4 R1 X1 D1 | pass | **PASS** | – | 706,430 | 1,623,586 | 39 | 109s | 1785 |
| `MC_gc_fix2_deep` | i1,i2 L2 W3 cub gc C2 X1 D1 db typed | pass | **PASS** | – | 366,285 | 736,323 | 39 | 94s | 1837 |
| `MC_gc_reuse` | as `MC_gc_fix2`, `ReuseManifest` (L7's check) | pass | **PASS** | – | 588,517 | 1,148,997 | 47 | 168s | 1910 |
| `MC_gc_live` | i1 L2 W4 gc C3 R1 X1 D1 deletes, `LiveSpec` | pass | **PASS** | – | 61,792 | 125,732 | 32 | 317s | 1577 |
| `MC_gc_reach_reroot_extended` | as `MC_gc_reroot_extended` | reach | **REACHED** | `Witness_RerootExtended` | 245,863 | 473,575 | 23 | 30s | 1682 |
| `MC_gc_reach_second_reroot` | as `MC_gc_reroot` | reach | **REACHED** | `Witness_SecondReroot` | 66,166 | 126,880 | 18 | 8s | 753 |
| `MC_gc_reach_based_chain` | as `MC_gc_fix2_deep` | reach | **REACHED** | `Witness_BasedChainRestored` | 26,633 | 51,194 | 17 | 5s | 687 |
| `MC_gc_base_missing_untyped` | i1,i2 L2 C0 D1 db typed budget 300 s (L6b's check) | pass | **PASS** | – | 137 | 249 | 16 | 2s | 361 |
| `MC_gc_live_rewritten` | i1 L2 C0 D1, `LiveSpec`, budget 300 s | fail | **FAIL** | `ChainRecovery` | 12 | 17 | – | 2s | 346 |
| `MC_gc_neg_live_unfair` | i1 L2 C0 budget 300 s | fail | **FAIL** | `ChainRecovery` | 4 | 5 | – | 2s | 320 |
| `MC_gc_neg_chain_ignores_depth` | i1 L1 C2 | fail | **FAIL** | `ChainDepthBounded` | 40 | 55 | 13 | 2s | 335 |
| `MC_gc_neg_gc_deletes_depended` | i1 L2 gc C1 | fail | **FAIL** | `GCNeverDeletesDepended` | 16 | 21 | 10 | 2s | 341 |
| `MC_gc_neg_base_replaced_live` | i1,i2 L2 C1 D1 db typed | fail | **FAIL** | `BaseNotReplacedWhileDepended` | 341 | 537 | 11 | 2s | 405 |
| `MC_gc_neg_sidecar_after_record` | i1 L2 C1 | fail | **FAIL** | `SidecarsBeforeRecord` | 14 | 18 | 8 | 2s | 332 |
| `MC_gc_neg_skip_flatten_verify` | i1 L2 C1 D1 | fail | **FAIL** | `PrereqsSatisfiedByEarlierLinks` | 72 | 110 | 13 | 2s | 353 |
| `MC_gc_neg_hit_ignores_chain` | i1 L2 C1 D1 | fail | **FAIL** | `BrokenLinkNeverReuseHit` | 69 | 104 | 13 | 2s | 341 |
| `MC_gc_neg_hit_ignores_chain_restore` | i1 L2 C1 D1 | fail | **FAIL** | `RestoreOrRecapture` | 69 | 106 | 13 | 2s | 346 |
| `MC_gc_neg_reroot_pre_mismatch` | i1 L1 W3 C2 | fail | **FAIL** | `PrereqsSatisfiedByEarlierLinks` | 41 | 56 | 14 | 1s | 344 |
| `MC_gc_neg_reuse_after_record` | i1 L2 C1 `ReuseManifest` (L7's check) | fail | **FAIL** | `ReuseManifestBeforeRecord` | 11 | 11 | 5 | 1s | 315 |

The self-test's row is the second invocation's; the first's tripped at
36,610 distinct states.

Reading the table:

- **Constants.** Items; `L` is `DepthLimit` (8 in the code; 2 here, so a
  chain reaches its limit in a few commits) and `W` is `RootWindow`
  (absent: 0, v1). `cub` is `ChainUnderBase`, `gc` is `GCOn`. `C`, `R`,
  `X` and `D` are `MaxCommits`, `MaxRewrites`, `MaxCrashes` and
  `MaxDamage`. `db` is `DamageBase`, and `typed` is
  `BaseMissingTyped = TRUE` (an assumption: [Findings](#findings)).
  Damage deletes a bundle or rewrites it in place (`DamageRewrites`),
  except where the row says `deletes`. The budget is 600 s unless shown.
- **Fewer states than the first round.** The first round's model gave
  every export a fresh bundle id; a bundle's id is now its content name,
  so a re-export of the same content is the same bundle, and many states
  that differed only in a duplicate's id are now one. `MC_gc_core` went
  from 111,680 to 45,062 distinct states, `MC_gc_live` from 232,067 to
  61,792 (it no longer rewrites).
- **Fail rows** stop at the first violation, as for BulkloadTransfer. Every
  fail and reach row also checks `TypeOK`.
- **Never enabled**, by pass row:
  - `MC_gc_core`: `BaseRecord`, `GC`, `StartBase` (one item has no plan
    base; v1 has no GC);
  - `MC_gc_q46`, `MC_gc_reroot`, `MC_gc_reroot_extended` and
    `MC_gc_live`: `BaseRecord`, `StartBase`;
  - `MC_gc_grouped`: `GC`, `Rewrite`;
  - `MC_gc_fix2` and `MC_gc_fix2_deep`: `Rewrite`.

  Since lane L7 each of those rows also never enables `ReuseSidecar`
  (`ReuseManifest = FALSE`), as does `MC_gc_base_missing_untyped`;
  `MC_gc_reuse` never enables `Rewrite` alone. Every action is enabled in
  some pass row.
- **Bounds.** `MC_gc_fix2` was first drafted at `L2 W4 C3`, on two items
  with every fault. Its budget tripped at 600 s after 2,462,299 distinct
  states (a scratch run of the first round's model, not a result), so it
  runs at `L1 W3 C2`, where it still re-roots on a based root.
  `MC_gc_fix2_deep` adds depth limit 2 at `W3 C2`, which keeps two links
  under the base but does not re-root.

### Explorer parity (GitCarry)

`just formal-nv` builds GitCarryCore.hs and checks seven presets against
TLC's counts of record, one per pass row except `MC_gc_live` (a liveness
row the explorer does not check). Each must also take the capture decisions
it exists for; the explorer lists every decision a step of its search took.
Built with `ghc -O1` on sting:

| Preset | TLC | Explorer | Decisions the search took | Match |
|---|---|---|---|---|
| `gc_core` (`MC_gc_core`) | 45,062 distinct, 81,520 generated, diameter 29 | 45,062, 81,520, 29 levels | `Hit`, `Export:Chain:NoRebase`, `Export:SelfContained:NoRebase`, `Export:SelfContained:NewRoot` (v1's re-base) | yes |
| `gc_q46` (`MC_gc_q46`) | 699,419, 1,606,770, 39 | 699,419, 1,606,770, 39 | as `gc_core`, plus `Export:Chain:Reroot` | yes |
| `gc_grouped` (`MC_gc_grouped`) | 85,941, 184,923, 35 | 85,941, 184,923, 35 | `Hit`, `Export:Base:NoRebase`, `Refuse:ReceiptBindingInvalid` | yes |
| `gc_fix2` (`MC_gc_fix2`) | 392,515, 807,467, 39 | 392,515, 807,467, 39 | `Hit`, `Export:Base:NoRebase`, `Export:BaseAndChain:NoRebase`, `Export:BaseAndChain:Reroot`, `Refuse:ReceiptBindingInvalid` | yes |
| `gc_reroot` (`MC_gc_reroot`) | 675,517, 1,552,268, 39 | 675,517, 1,552,268, 39 | as `gc_q46` | yes |
| `gc_reroot_extended` (`MC_gc_reroot_extended`) | 706,430, 1,623,586, 39 | 706,430, 1,623,586, 39 | as `gc_q46` | yes |
| `gc_fix2_deep` (`MC_gc_fix2_deep`) | 366,285, 736,323, 39 | 366,285, 736,323, 39 | `Hit`, `Export:Base:NoRebase`, `Export:BaseAndChain:NoRebase`, `Refuse:ReceiptBindingInvalid` | yes |

`MC_gc_reuse` has no preset, so `formal-nv` does not pin it. By hand
(2026-10-07, `explore --preset gc_fix2 --reuse-manifest true`, 33 s) the
explorer reached TLC's 588,517 distinct states, 1,148,997 generated and 47
levels with no invariant violated.

The generated counts match too. GitCarry.tla writes a guard that chooses no
successor as an `IF`, never as a disjunction, because TLC branches on every
disjunction inside an action. Written as `DamageBase \/ kind = "capture"`,
the damage guard made TLC count each damage successor twice whenever both
held: in the first round's model, 346,545 generated in `MC_gc_grouped`
against the explorer's 286,977, with the same distinct count. The content
names keep that rule: `Publish`, `StartBase` and `Capture` choose between a
name that exists and a fresh one with an `IF`.

The mutation rows, from one `just formal-nv` run (2026-10-05 01:10 to 01:15
EDT, sting, load average near 25, under the shared lock, over `ba6b85c`).
Every `MC_gc_neg_` row ran on the explorer at its own bound and violated
its named property. Each primary row ran again with every safety invariant
checked, on the explorer and on TLC with one worker. Both stopped at the
same first invariant after the same number of states:

| Row | Named property (explorer) | Every invariant: TLC | Every invariant: explorer |
|---|---|---|---|
| `MC_gc_neg_chain_ignores_depth` | `ChainDepthBounded`, 13 states | `ChainDepthBounded`, 13 | `ChainDepthBounded`, 13 |
| `MC_gc_neg_gc_deletes_depended` | `GCNeverDeletesDepended`, 10 | `GCNeverDeletesDepended`, 10 | `GCNeverDeletesDepended`, 10 |
| `MC_gc_neg_base_replaced_live` | `BaseNotReplacedWhileDepended`, 11 | `BaseNotReplacedWhileDepended`, 11 | `BaseNotReplacedWhileDepended`, 11 |
| `MC_gc_neg_sidecar_after_record` | `SidecarsBeforeRecord`, 8 | `SidecarsBeforeRecord`, 8 | `SidecarsBeforeRecord`, 8 |
| `MC_gc_neg_skip_flatten_verify` | `PrereqsSatisfiedByEarlierLinks`, 10 | `PrereqsSatisfiedByEarlierLinks`, 10 | `PrereqsSatisfiedByEarlierLinks`, 10 |
| `MC_gc_neg_hit_ignores_chain` | `BrokenLinkNeverReuseHit`, 10 | `BrokenLinkNeverReuseHit`, 10 | `BrokenLinkNeverReuseHit`, 10 |
| `MC_gc_neg_hit_ignores_chain_restore` | `RestoreOrRecapture`, 10 | (an `also` row) | |
| `MC_gc_neg_reroot_pre_mismatch` | `PrereqsSatisfiedByEarlierLinks`, 14 | `PrereqsSatisfiedByEarlierLinks`, 14 | `PrereqsSatisfiedByEarlierLinks`, 14 |
| `MC_gc_neg_reuse_after_record` (L7's run, 2026-10-07) | `ReuseManifestBeforeRecord`, 4 | `ReuseManifestBeforeRecord`, 4 | `ReuseManifestBeforeRecord`, 4 |

Every primary row stops first at its own verdict. Two counterexamples also
violate `RestoreOrRecapture` in their last state:

- `sidecar_after_record`'s: the record without its sidecar fails apply and
  the hit path with a bare IO;
- `hit_ignores_chain`'s: the reused record does not restore.

The same `formal-nv` run passed every BulkloadTransfer row again: both
presets, all 17 mutation rows and all 14 every-invariant runs. It also
passed `rows --check` (363 rows, unchanged) and the schema check (5
unions). 55 rows matched (33 BulkloadTransfer, 22 GitCarry); none
differed.

### Findings

- **A bundle rewritten in place blocks its item while the source holds
  still** (`MC_gc_live_rewritten` fails `ChainRecovery` in 12 states). A
  third party rewrites an item's bundle B at its CORPUS name. The record
  no longer restores (`DIGEST_MISMATCH`), and `retained_capture` sees the
  identity change and returns `Retained::None`. The recapture at the same
  tip produces the same bytes (the code's tests rely on that), so the same
  content name, `{identity}-{digest}.bundle`. `publish_bundle` finds the
  name taken, hashes the rewritten file and refuses `DIGEST_MISMATCH`, on
  every pass, until the source moves. The finding row checks a
  self-contained bundle on one item; the same rule stops a based bundle (a
  grouped item), and `prepare_base` for a plan base that was linked but
  never recorded. Every refusal is typed, so `RestoreOrRecapture` holds in
  every pass row; recovery does not. The fix is
  outside this lane: for example, `publish_bundle` and `prepare_base`
  could move a file whose digest differs from its name to a quarantine
  name that cannot collide, then link the fresh bytes.
- **A missing plan base restored as a bare IO; fixed by lane L6b (#181).**
  `MC_gc_base_missing_untyped` failed `RestoreOrRecapture` under
  `BaseMissingTyped = FALSE`: `estate::import_base` read `{bundle}.base`,
  then staged the base with `git_carry::stage_bundle`, whose
  `fs::canonicalize` fails `ENOENT` for a deleted base, so the apply
  refused `IO`. A bare IO never counts (S4, OI-1003-Q1). Since L6b
  `estate::stage_base` refuses a missing base `SEALED_OBJECT_MISSING`, as
  `apply_item` does for the head bundle and `chain_links` for a missing
  link, and so do `chain_links` and `chain::flatten` for a base a chain is
  bound to. The estate test `a_missing_plan_base_refuses_apply_by_name`
  pins it. The row keeps its name, sets `BaseMissingTyped = TRUE` as the
  positive grouped configs do, and is an expected pass. No config sets the
  constant `FALSE` any more.
- **A record written before its sidecar is stuck while the source holds
  still.** In `sidecar_after_record`'s 8-state counterexample, the record
  names a chained bundle with no `.prior`. Apply then reads the absent
  `.base` sidecar (`requires_base` is true for a chained bundle) and fails
  `IO`. The next capture's hit path fails `IO` on the same read. So the
  record neither restores nor is recaptured until the source moves and the
  capture leaves the hit path. The same state also violates
  `RestoreOrRecapture`. This is why `SidecarsBeforeRecord` is
  load-bearing.
- **v1 keeps a lost base as visible custody.** A group whose base is lost
  refuses `RECEIPT_BINDING_INVALID` on every pass, by design. Recovery is
  an operator act. With the first finding, `ChainRecovery` is claimed only
  where damage deletes bundles and cannot reach a base (`MC_gc_live`).

### What GitCarry does not prove

- **Pack contents and the object-set laws** (Q45). A bundle's content is
  its CORPUS name: its kind, item, the source tip it captured, its basis
  and the prerequisite tips its header declares, which fix its writer and
  inputs. Two exports that differ in any of those never share a name here,
  though their bytes could; two that agree always do. What a pack holds,
  and the `PackExcludesHeld` property, are P64 and P65's (lane L1).
- **A file's identity.** A bundle linked again under its name (after a
  delete or GC) keeps one identity in the model. The code gives it a new
  `StatIdentity`, so a capture-side reference recorded earlier
  (`LinkBinding::Custody`) sees a changed file and recaptures, as for a
  rewritten link; apply binds by digest and sees what the model sees.
- **A source returning to an earlier tip.** Source tips only move forward
  here. Returning re-exports an earlier name, whose outcomes are those of a
  same-tip re-export, which the model reaches (reuse, a fresh link, or
  `DIGEST_MISMATCH`), plus the identity effect above.
- **Two writers on one CORPUS.** `estate.lock` lives in STATE, and two
  STATE directories may share a CORPUS
  (`two_state_dirs_sharing_a_corpus_fail_closed_on_interleaved_records`).
  The model has one capture actor, and GC runs only between its passes;
  apply is a state function, so a GC racing an apply is not explored
  either. GC is safe only if L8 makes it take a CORPUS-level exclusive
  lock that every capture and every apply also take; that lock is a
  pending symbol of `GCNeverDeletesDepended`'s row (P71).
- **The order inside one GC deletion.** GC collects one bundle with its
  sidecars per step and recomputes what is garbage before the next, so a
  crash between two deletions is explored (as a stop between steps). The
  order of a bundle and its sidecars inside one deletion is not.
- **Drift, racy seats, shallow sources.** They are inputs of `decide`, and
  the pinned rows vary them. The model holds them fixed (no drift, settled,
  start recorded, not shallow).
- **Two capture jobs at once.** The code runs up to two jobs inside
  `estate.lock`, and the model runs one. The jobs share no custody state
  except the plan base, which is created once under the group's mutex.
- **STATE GC.** STATE attempt directories are never depended on, because
  `publish_bundle` and `prepare_base` hard-link into CORPUS. So collecting
  them under the lock cannot break custody, and they are not modelled.
- **Sidecar damage, torn writes.** Every durable write is atomic
  (`estate::write`). Only bundle files are damaged.
- **What a reuse manifest lists, and what a pass reuses from it** (L7).
  The manifest is a file that exists or not. The model checks its order
  against the record (`ReuseManifestBeforeRecord`) and nothing else: a
  manifest bound to another bundle, one that does not decode, the
  presence check on the source and the bytes a reuse reads are P70's and
  P69's (Rust). A manifest a crash leaves without a record is harmless
  there for the same reason: nothing reads it without the record that
  names its bundle.
- **Bare captures** (#172). `apply_item` refuses a bare capture planned with
  a workspace (`refuse_bare_capture`), before any chain or base step. That
  is a typed refusal outside custody.
- **The code beyond L6b.** P67 checks the Rust `decide` against all 363
  rows, so its L8 policy is checked too. The callers run only the L6b
  policy (`Policy::CODE`, fix 2) and refuse a plan it cannot carry out (a
  re-root), so what the code does with an L8 decision is unchecked until
  that lane lands its custody. P68 checks what the code does with an L6b
  decision. One estate rule is outside the reference: `chain_offer` never
  offers a based link whose bound base is lost, where `extend` does not
  read `prev_base` for a based link; the capture is then the plan base's
  delta, as under v1.

## SqliteCarry: the SQLite snapshot seat (#218, OI-1003-Q146)

[`SqliteCarry.tla`](SqliteCarry.tla) models the SQLite snapshot seat of
wire v6 (`--sqlite=snapshot`), as built on `feat/sqlite-carry-20261008`
([the design note](../agent-notes/2026-10-08-sqlite-carry-design.md),
sections 0 and 16). It is model first, per OI-1003-Q102's precedent. Its
configs are rendered from [`catalogue/SqliteCarry.dhall`](catalogue/SqliteCarry.dhall)
into `configs_sq.tsv` and `MC_sq_*.cfg`, and `just tla-check` runs them
(the third `Module`; its code symbols are grounded with the `code` match,
as BulkloadTransfer's). Until 2026-10-09 it lived in `docs/formal/sqlite/`
with hand-written configs outside the catalogue.

### What it models

One live store, one destination path, at most three runs and one crash.

- The store: a logical version `lv`, and stat identities of the main file and
  the `-wal` that only grow. A WAL commit moves the `-wal` (never blocked by a
  reader); a checkpoint moves the main file (it waits while a step reads); a
  WAL reset moves the `-wal`; a rollback-mode commit moves the main file (it
  waits while a step holds `SHARED`).
- A run: the walk stats the main file and the `-wal` in two separate steps,
  commits possible between (R11); Decide reuses from the row or adopts an
  unrowed output from its capture record, only for the output this store
  landed and nothing touched since; it refuses for a destination sidecar
  (R5), a non-owner (R4), a refuse-mode record only under the mutation (R8);
  a session as root opens nothing (OI-1003-Q76). The backup pre-stats, steps
  with the lock released between steps (OI-1003-Q16, D2), restarts on a
  commit between steps, ends within its step budget or is refused,
  post-stats and settles (design 5.4).
- Publish (OI-1003-Q146): beside a sidecar it is refused; into a free leaf
  it renames; over the same bytes it adopts; over this store's own,
  untouched output it takes WP0(d)'s intent (the old rows out), then
  `Exchange`, whose last look refuses a touched output (no rows back) or a
  sidecar that appeared since Decide (the old rows back), and otherwise
  trades the files in one step; `CommitRow` settles the intent. Over any
  other file it is refused. A crash at any step is followed by the sweep:
  an old output still in place and still owned gets its rows back.
- Third parties: `SidecarAppear` (a sidecar beside the published output, at
  any step of a run), `SidecarRemoved` (between runs), `DestTouch` (an
  in-place write of the output, at any step: it is then `Foreign` and no
  longer this store's).
- Session end: a sidecar is covered only by its base's snapshot (R2).

### Properties and mutations

| Property | Meaning | Mutation that fails it |
|---|---|---|
| `SqliteNeverTorn` | a published output is a committed version | `sqlite_raw_send` |
| `SqliteReuseSound` | a Reuse (row or adopted record) is of the version at the walk | `sqlite_key_main_only`, `sqlite_record_unsettled`, `sqlite_capture_record_main_only` (R1) |
| `WalWriterNeverBlocked` | in WAL mode a writer's commit is always enabled | `wal_lock_exclusive` |
| `S2_BackupLockBounded` | the lock is held inside one step; steps are bounded | `sqlite_unbounded_pinned` |
| `SqliteRootRefusedUpFront` | as root, nothing is opened | `sqlite_as_root` |
| `SqliteOwnerRefused` | another user's database is never opened (R4) | `sqlite_not_owner_opened` |
| `SqliteNoForeignSidecar` | never published, by a rename or an exchange, beside a sidecar (R5, Q146) | `sqlite_publish_beside_sidecar`, `sqlite_supersede_no_recheck` |
| `SqliteNoClobber` | only the output this store landed, untouched, is ever replaced (Q146) | `sqlite_supersede_unowned` |
| `SqliteOldOrNewWhole` | once published, the path always holds a whole database; during a supersede, exactly the old output or the new snapshot (Q146) | `sqlite_supersede_unlink_rename` |
| `SidecarCoverageSound` | a sidecar is covered only by its base's snapshot (R2) | `sidecar_covered_by_name` |
| `SnapshotModeIgnoresV5Record` | snapshot mode does not honour tag 1 (R8) | `sqlite_header_refusal_in_snapshot_mode` |
| `S3_UnchangedSqliteZero` | a run whose key the destination proves takes no backup | `sqlite_reuse_ignored` |

`SqliteReuseSound` is defined against `lv` when the main file was walked,
not "now": a commit after the walk is the next run's (review R11).
`SqliteNeverSupersede` (D7, "never supersede a snapshot output") and its
mutation `sqlite_supersede` are gone: OI-1003-Q146 replaced D7. The
reachability witness `Witness_Superseded` shows a supersede is reached and
its row commits (`MC_sq_reach_superseded`).

### Results (SqliteCarry)

`just tla-check` on 2026-10-09 (TLC 1.7.4, 3 workers, coverage on; the
shared host at a load average of about 200), every row of `configs_sq.tsv`
as expected. Total wall 816 s, peak RSS 1702 MiB.

| Config | Expect | Outcome | Violated | Distinct states | Wall |
|---|---|---|---|---|---|
| `MC_sq_budget_selftest` | inconclusive | INCONCLUSIVE | WithinBudget | ? | 15s |
| `MC_sq_wal` | pass | PASS | - | 827494 | 180s |
| `MC_sq_rollback` | pass | PASS | - | 30908 | 26s |
| `MC_sq_root` | pass | PASS | - | 63 | 12s |
| `MC_sq_not_owner` | pass | PASS | - | 1479 | 20s |
| `MC_sq_raw_base` | pass | PASS | - | 1479 | 15s |
| `MC_sq_v5_record` | pass | PASS | - | 373702 | 70s |
| `MC_sq_reach_reuse` | reach | REACHED | Witness_Reuse | 8742 | 35s |
| `MC_sq_reach_restart` | reach | REACHED | Witness_Restart | 1339 | 22s |
| `MC_sq_reach_superseded` | reach | REACHED | Witness_Superseded | 79781 | 65s |
| `MC_sq_neg_sqlite_raw_send` | fail | FAIL | SqliteNeverTorn | 3959 | 38s |
| `MC_sq_neg_sqlite_key_main_only` | fail | FAIL | SqliteReuseSound | 9512 | 27s |
| `MC_sq_neg_sqlite_record_unsettled` | fail | FAIL | SqliteReuseSound | 14304 | 21s |
| `MC_sq_neg_sqlite_capture_record_main_only` | fail | FAIL | SqliteReuseSound | 10745 | 15s |
| `MC_sq_neg_wal_lock_exclusive` | fail | FAIL | WalWriterNeverBlocked | 172 | 28s |
| `MC_sq_neg_sqlite_unbounded_pinned` | fail | FAIL | S2_BackupLockBounded | 540 | 17s |
| `MC_sq_neg_sqlite_as_root` | fail | FAIL | SqliteRootRefusedUpFront | 106 | 12s |
| `MC_sq_neg_sqlite_not_owner_opened` | fail | FAIL | SqliteOwnerRefused | 142 | 20s |
| `MC_sq_neg_sqlite_publish_beside_sidecar` | fail | FAIL | SqliteNoForeignSidecar | 57213 | 34s |
| `MC_sq_neg_sqlite_supersede_no_recheck` | fail | FAIL | SqliteNoForeignSidecar | 76204 | 32s |
| `MC_sq_neg_sqlite_supersede_unowned` | fail | FAIL | SqliteNoClobber | 113132 | 33s |
| `MC_sq_neg_sqlite_supersede_unlink_rename` | fail | FAIL | SqliteOldOrNewWhole | 63451 | 22s |
| `MC_sq_neg_sidecar_covered_by_name` | fail | FAIL | SidecarCoverageSound | 157 | 8s |
| `MC_sq_neg_sqlite_header_refusal_in_snapshot_mode` | fail | FAIL | SnapshotModeIgnoresV5Record | 58 | 13s |
| `MC_sq_neg_sqlite_reuse_ignored` | fail | FAIL | S3_UnchangedSqliteZero | 11610 | 20s |

The pass rows' never-enabled actions are the `never` column of
`configs_sq.tsv`; every pass row checks every safety invariant, and
`RenameAfterUnlink` (the unlink-then-rename mutation's second step) is
never enabled in any of them.

### What SqliteCarry does not prove

- SQLite's own internals: that a backup whose steps all saw one version
  copies that version is an axiom here, checked by P80, not proved.
- Clocks: the racy rule is folded into "settled"; same-tick rewrites are the
  racy rule's, as for files (#86).
- The path race of design section 7.5; chunks, credit and the wire; the file
  seat and every property `BulkloadTransfer.tla` already holds for it,
  WP0(d)'s exchange for files included (`MC_wp0d_exchange`).
- The window between the exchange's last sidecar `lstat` and the exchange:
  here the look and the exchange are one step. A `-journal` or `-wal`
  created in that window, and an application on SQLite before 3.8.3 (or a
  VFS whose files do not track their inode: `unix-none`, `unix-dotlock`,
  `unix-flock`, a custom one) whose next write after the exchange
  journals beside the new file, are the residuals OI-1003-Q146 accepts; a
  modern unix-VFS connection on the old inode gets
  `SQLITE_READONLY_DBMOVED` instead (design.md, "SQLite snapshot seats").
- The exclusive-mode opener and the checkpointer's busy handler beyond "waits
  while a step reads": their wait is the backup connection's life
  (`StepBudget` steps), which `S2_BackupLockBounded` bounds.
- The Haskell explorer does not cover this module.

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

GitCarry.tla's names (the module, its properties, mutations, constants and
configs) are new and not frozen; freezing them needs a ruling.

SqliteCarry.tla's names (the module, its properties, witnesses, mutations,
constants and configs, #218 and OI-1003-Q146) are new and not frozen;
freezing them needs a ruling.

The names of #187's records (2026-10-07, OI-1003-Q102) are not frozen
either: the properties `SupersedeAtomic`, `OwnershipNeverReuse`,
`RememberedRefusalSound` and `ExchangeRefusedUpFront`, the action
`BeginSupersede`, the constant `ExchangeSupported`, the mutations
`owned_ignores_identity`, `sweep_drops_ownership`,
`exchange_before_intent`, `own_is_reuse`, `refusal_unbound` and
`late_exchange_refusal`, and the configs `MC_supersede_*`,
`MC_reach_exchange_refused`, `MC_reach_remembered_refusal`,
`MC_reach_ownership_superseded`, `MC_reach_sweep_ownership`,
`MC_reach_sweep_restore` and the six `MC_neg_` rows of those mutations.

The names this revision adds are not frozen: `R25_StrictNoDurableReread`,
`AdoptOnlyUnrowed` (#169), the `Witness_` invariants, the constants
`StoreRootSealed`, `TrackStrictHeld` and `AdoptUnrowed` (#169), the
mutations `adopt_unkeyed`, `adopt_unverified`, `reuse_ignores_row` and
`adopt_unrecorded` (#169), the ghost `ledgerLost`, and the configs
`MC_nv_ledger`, `MC_nv_core_adopt`, `MC_nv_ledger_adopt`, `MC_reach_*`,
`MC_store_root_unsealed`,
`MC_r25_unrowed_bytes`, `MC_r25_unrowed_no_adopt`, `MC_r25_strict_deep`,
`MC_r25_strict_main`, `MC_r25_strict_unsealed`, `MC_r25_strict_authority`,
`MC_neg_adopt_unkeyed`, `MC_neg_adopt_unverified`,
`MC_neg_reuse_ignores_row`, `MC_neg_adopt_unrecorded`,
`MC_neg_reread_ignore_ledger` and `MC_neg_reread_exchange`. Freezing
`MC_nv_ledger` as the second N-version row needs a ruling.
