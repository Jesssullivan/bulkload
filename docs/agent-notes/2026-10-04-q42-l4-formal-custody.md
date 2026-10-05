# 2026-10-04: Q42 lane L4, formal custody (GitCarry.tla, Dhall unions, Haskell decide)

Rulings:

- **OI-1003-Q42**: the Q15 git-carry push (lanes L1 to L9).
- **OI-1003-Q43**: the language roles. The decision core is a pure Rust
  `decide` (L6), with a Haskell reference copy as a differential oracle
  and pinned rows checked by a Rust proptest (P67). Dhall types the Basis
  and Decision unions. TLC checks only chain and base custody and crash
  order.
- **OI-1003-Q46**: chain lifetime. A re-root policy, plus STATE and CORPUS
  GC that never deletes a link something depends on (P71).
- **OI-1003-Q32**: the hybrid formal model. TLA+ is the checker of record,
  with a Dhall catalogue and a Haskell explorer.
- **R-N13**: receipts and this note.
- Also followed: R-N11, R-N12, R-N92, R-N101, R-N104 and R-N98 (no hook
  bypass).

The ruling text is from the coordinator's Linear comment on TIN-4543
(2026-10-04, about 15:25 EDT). The lane brief came from the Q42 plan's L4
section.

Branch `docs/q42-l4-formal-custody-20261004` in worktree
`bulkload.worktrees/q42-l4-formal-custody-20261004`. It was created from
`origin/main` at `8dc26c1`, and this session was its only writer. No PR,
as the brief asked.

Shas:

- `1740c53`: GitCarry.tla, the catalogue's Module union and GitCarry rows,
  `Lib.dhall`, `GitCarryCore.hs`, `decide_rows.tsv`, the three recipes and
  the first README edits.
- `8bb9921`: merge of `origin/main` at `cdfe5f4` (#171, #172, #173). It
  merged cleanly. #172's `refuse_bare_capture` runs in `apply_item` before
  any custody step, and every cited symbol is unchanged.
- `a418448`: the README results, parity tables and findings, and a fix to
  `tla-check`'s label-set extractor. It matched only `NAME ==` with one
  space, so the aligned `Bases     ==` read as empty. Grounding failed
  closed, with 11 labels reported missing, and no TLC ran.
- This note: the commit after `a418448`, pushed with the branch.

## What was done

1. **`docs/formal/GitCarry.tla`**: a custody-only module. It models the
   plan base, chain links and depth (`CHAIN_DEPTH_LIMIT`), Q46's re-root
   window and CORPUS GC, L6b's chain under a base, crashes between the
   bundle, its sidecars and the record, third-party damage (a delete or an
   in-place rewrite), source tips moving and being rewritten, and what
   apply and the next capture would do with every record.
   - Seven safety invariants: `ChainDepthBounded`,
     `PrereqsSatisfiedByEarlierLinks`, `BrokenLinkNeverReuseHit`,
     `BaseNotReplacedWhileDepended`, `GCNeverDeletesDepended`,
     `SidecarsBeforeRecord` and `RestoreOrRecapture`. Liveness:
     `ChainRecovery`. There is no `PackExcludesHeld` (P64 owns it).
   - Grounded in the v1 code at `8dc26c1` and re-checked at `cdfe5f4`:
     `estate.rs` (`prepare_base`, `retained_capture`, `chainable`,
     `chain_offer`, `chain_links`, `publish_prior`, `apply_item`,
     `import_base`), `git_carry.rs` (`ExportOptions`, `export_pass`,
     `stage_bundle`, `write_chained`, `write_bundle`), `chain.rs`
     (`CHAIN_DEPTH_LIMIT`, `source_held_tips`, `flatten`) and `shared.rs`
     (`requires_base`, `prerequisites`). That is 24 symbols, each found
     by `git grep -w`. `decide.rs` and the L6b, L7 and L8 symbols are
     listed as pending, with the lane that lands each.
2. **Mutations.** Six, each failing exactly its named property:
   - `chain_ignores_depth` fails `ChainDepthBounded`;
   - `gc_deletes_depended` fails `GCNeverDeletesDepended`;
   - `base_replaced_live` fails `BaseNotReplacedWhileDepended`;
   - `sidecar_after_record` fails `SidecarsBeforeRecord`;
   - `skip_flatten_verify` fails `PrereqsSatisfiedByEarlierLinks`;
   - `hit_ignores_chain` fails `BrokenLinkNeverReuseHit`, and again
     `RestoreOrRecapture` in an `also` row.

   `hit_ignores_chain` is a sixth, beyond the plan's five: none of the five
   reaches `BrokenLinkNeverReuseHit`, and the catalogue asserts that every
   safety invariant has a fail row.
3. **Dhall.**
   - A `Module` union with a module table (spec, run order), so the
     catalogue holds a second spec.
   - `catalogue/GitCarry.dhall` renders `configs_gc.tsv` and 15
     `MC_gc_*.cfg`, in the existing format.
   - Typed P-ids (`T.PId`) and typed pending symbols (`T.Lane`), on the new
     rows only.
   - Closed unions `Basis`, `Rebase`, `ReuseEligibility`, `Refusal` and
     `Decision`, each listed from a total table and asserted label by label.
   - The list and text helpers moved to `catalogue/Lib.dhall`.
   - BulkloadTransfer's 44 rendered files are byte-identical to
     `8dc26c1`'s, by sha256 manifest.
4. **`docs/formal/hs/GitCarryCore.hs`** (base and containers):
   - `decide :: Inputs -> Decision`, derived from the v1 rules and Q46's
     re-root policy;
   - `rows`, which writes 363 pinned rows (v1 79, L6b 85, L8 199) to
     `crates/bulkload-agent/tests/data/decide_rows.tsv`. Each `-` input is
     checked to be ignored over its whole domain, and every label of every
     closed union is reached;
   - `rows --check FILE`, which is byte-identical or fails;
   - `schema`;
   - `explore`, a BFS of GitCarry.tla whose capture step calls `decide`,
     with all six mutations, JSON counterexamples, and the capture
     decisions each search took.
5. **justfile** (standalone recipes, no tier, no CI):
   - `tla-render --check`;
   - `tla-check` runs each module's rows against that module, after its own
     budget self-test, with per-module grounding, including the
     closed-union label sets in both directions and the pending symbols;
   - `formal-nv` adds `rows --check`, the schema check, four GitCarry
     presets with their required decisions, and every `MC_gc_neg_` row on
     the explorer and, for primary rows, the every-invariant comparison
     with TLC.
6. **README**: the Q43 roles; the decision core and Q46's reading; the
   properties, mutations, results and parity tables; the findings; what
   GitCarry does not prove. Also an updated file table, running notes,
   catalogue checks and frozen-names note.

Key numbers (sting; load 20 to 48 for the TLC run, near 500 for formal-nv):

- `just tla-check` (both modules) **matched all 58 rows**; total 934 s,
  peak RSS 2,117 MiB.
  - BulkloadTransfer's 43 rows had unchanged counts. **`MC_nv_core` is
    still 15,834.**
  - GitCarry's 15 rows took 533 s, and the core `MC_gc_core` 14 s:
    - `MC_gc_core`: 111,680 distinct;
    - `MC_gc_q46`: 943,611;
    - `MC_gc_grouped`: 149,749;
    - `MC_gc_fix2`: 770,065;
    - `MC_gc_live`: 232,067, `ChainRecovery` under `LiveSpec`;
    - the self-test is INCONCLUSIVE (`WithinBudget` alone), and the nine
      fail rows fail only their named property.
- `just tla-render --check`: all 60 committed files equal the rendering.
- `rows --check`: 363 rows, byte-identical.
- Explorer parity: the four presets equal TLC's distinct and generated
  counts and diameter exactly (`gc_core`, `gc_q46`, `gc_grouped`,
  `gc_fix2`). Each also took its required decisions: v1's re-base, Q46's
  `Reroot`, the base refusal, and fix 2's `BaseAndChain` re-root. `just
  formal-nv` (21:30 to 22:04 EDT) **matched all 50 rows, with 0
  differing**. That covers BulkloadTransfer's presets, its 17 mutation
  rows and 14 every-invariant runs, again, and for GitCarry `rows --check`,
  the schema, the 4 presets, 7 mutation rows and 6 every-invariant runs
  against TLC with one worker.
- check-fast (`nix develop .#default --command just check-fast`, under the
  shared lock, 22:04 to 22:19 EDT, over `a418448`'s tree): **green**.
  - 710 tests passed, 0 failed.
  - The CI contract (22 tests) and repo manifest passed.
  - The fault harness and the power-loss proofs passed.
  - An earlier attempt (17:03 to 21:30, load near 600) passed all of
    check-source. The harness's background time limit then stopped it
    inside fault-harness's first feature build, so it is not counted.

Finding (code, not fixed here): `estate::import_base` stages a plan base
with `stage_bundle`. That fails `ENOENT` for a deleted base, so the apply
refuses with a bare `IO`, while `apply_item` maps the same error to
`SEALED_OBJECT_MISSING` for the head bundle. S4 says a bare IO never
counts. `MC_gc_base_missing_untyped` shows it, and the positive grouped
configs assume the typed refusal. It still holds at `cdfe5f4`.

## Open

- **Q46's window semantics (L8, operator).** The model's reading: a
  capture whose root would reach `RootWindow` starts a new root, and at the
  depth limit a capture re-roots on its chain's root. The window's value,
  and whether the age rule applies below the limit, are unruled. The rows
  use window 27 as a parameter.
- **The import_base finding** needs an issue and a fix lane: map the
  base's `ENOENT` to `SEALED_OBJECT_MISSING`, as for the head bundle.
- **The sixth mutation** (`hit_ignores_chain`) goes beyond the plan's five.
  Keep it (recommended: it is what makes `BrokenLinkNeverReuseHit`
  falsifiable) or drop it.
- **Frozen names.** Freezing GitCarry's names, and `MC_gc_core` as the
  custody N-version core, needs a ruling.
- **L6 hand-off.** `decide.rs` should use the Dhall labels. P67 reads
  `crates/bulkload-agent/tests/data/decide_rows.tsv`: `v1` rows in L6a,
  `L6b` rows in L6b, `L8` rows in L8. It draws `-` inputs. The input
  enums' labels are GitCarryCore's. Code-symbol grounding for `decide.rs`
  lands then: move the pending symbols into `codeSymbol`.
- **Bounds.** `MC_gc_fix2` runs at `L1 W3 C2`, because `L2 W4 C3` on two
  items tripped the 600 s budget at 2,462,299 states (a scratch run).
  Larger fix-2 bounds would need simulation.
- **Not modelled:** two capture jobs at once, sidecar damage, torn
  writes, drift and racy seats in the TLA (they are `decide`'s inputs, and
  the rows vary them), shallow sources in the TLA, and STATE GC (never
  depended on).
- **Coordinator:** distil these facts onto TIN-4543 and the SSOT ledger.
  This lane posted nothing to Linear.
- Scratch: `/srv/scratch/jess/tmp/q42-l4-formal` (this lane's own; logs of
  the run of record are kept there until the coordinator no longer needs
  them).
