# Formal model: EstateConverge only

[2026-10-17: rewritten under R-C843 (Linear TIN-4543, ace8649c). The
previous README is in git history: `git show e3522007:docs/formal/README.md`.]

R-C843 deleted the formal model, the whitepaper and the paper-proof package,
citing [docs/SPEC.md](../SPEC.md). R-C844 says done means the R7 resume
test plus a hash manifest; no paper or formal model is required (this
supersedes OI-1003-Q4, Q7, Q32 and Q155). Runnable property tests stay as
tests.

What remains here, and why:

| File | Why it stays |
|---|---|
| [`EstateConverge.tla`](EstateConverge.tla) | The estate convergence model (OI-1003-Q144). It is the reproducer for bulkload#237 (a `NoLocalWorkLost` counterexample at `MaxSrc=2`: a deletion made during an apply is undone by the post-crash settle). It stays until #237 is fixed. |
| [`configs_ec.tsv`](configs_ec.tsv) | The module's run order and expected outcome per config. `just tla-check` reads it. |
| `MC_ec_*.cfg` | The module's TLC configurations. |

Deleted under R-C843 on 2026-10-17: `BulkloadTransfer.tla`, `GitCarry.tla`,
`SqliteCarry.tla` and `DirectoryRecords.tla` with their configs, the Dhall
catalogue (`catalogue/`), which rendered every config, and the Haskell
explorer and reference decision core (`hs/`). The `just tla-render` and
`just formal-nv` recipes went with them. Code comments that still name
those modules' actions (for example `DirectoryRecords.tla` `GroupFinish`)
point into that history.

Because the catalogue is gone, `configs_ec.tsv` and `MC_ec_*.cfg` are now
edited by hand; their "Rendered from catalogue/EstateConverge.dhall" headers
are historical. `EstateConverge.tla`'s comments cite sections of the
previous README ("EstateConverge", "What EstateConverge does not prove",
"The budget is state-level") and the catalogue's grounding; read those in
the history above.

## Running it

`just tla-check` is standalone and on demand: no tier and no CI job runs it.
TLC comes from the flake's pinned nixpkgs (`tlaplus`).

```sh
just tla-check                  # every row of configs_ec.tsv, in order
just tla-check MC_ec_refs       # the budget self-test, then the named configs
```

The first row of `configs_ec.tsv`, `MC_ec_budget_selftest`, must finish
with `WithinBudget` alone violated, or nothing else runs. Every other row's
outcome must equal its `expect` column (`pass`, `fail`, `reach`, `simulate`
or `inconclusive`); the recipe's comment in the `justfile` defines the
outcomes. The catalogue's staleness and grounding checks no longer run.

One config by hand, without the recipe:

```sh
cd docs/formal
nix shell --inputs-from ../.. nixpkgs#tlaplus --command \
  tlc -workers 3 -metadir "$(mktemp -d)" -config MC_ec_refs.cfg EstateConverge.tla
```

Add a row's `flags` column, when it is not `-`, to the `tlc` arguments.
