# 2026-10-05: Q42 lane L6a, the decision core (`git_carry/decide.rs`, P67)

Rulings:

- **OI-1003-Q42**: the Q15 git-carry push (lanes L1 to L9). This is L6a.
- **OI-1003-Q43**: the language roles. The decision core is a pure Rust
  `decide`, with a Haskell reference copy as a differential oracle and
  pinned rows checked by a Rust property test (P67).
- **OI-1003-Q46**: chain lifetime (re-root and GC). The Rust core carries
  the policy knobs; the code runs v1's policy only.
- **R-N13**: receipts and this note.
- Also followed: R-N11, R-N12, R-N92, R-N101, R-N104, R-N98 (no hook
  bypass), R33 (refusals are values), R34 (no new dependency) and R-N54
  (no `unsafe` was added, so no `// SAFETY:` comment was needed).

The brief came from the coordinator's workflow dispatch. The note keeps the
dispatch's file name (dated 2026-10-04); the work ran on 2026-10-05.

Branch `feat/q42-l6a-decide-20261005` in worktree
`bulkload.worktrees/q42-l6a-decide-20261005`, created from `origin/main`
at `40adca8` (#182 merged). This session was its only writer. No PR, as
the brief asked.

Shas:

- `5a44dac`: the decision core, its callers, P67, the catalogue grounding
  and the docs.
- This note: the commit after `5a44dac`, pushed with the branch.

## What was done

1. **`crates/bulkload-agent/src/git_carry/decide.rs`**: a pure, total
   `decide(&Inputs) -> Decision`, with no IO. It transliterates
   `docs/formal/hs/GitCarryCore.hs`'s `decide`, including the L6b (chain
   under a plan base) and Q46 (root window) policies, behind
   `Policy { depth_limit, root_window, chain_under_base }`. The code runs
   only `Policy::V1` (`CHAIN_DEPTH_LIMIT`, window 0, no chain under a
   base).
   - The variant names are the Dhall labels: `Basis` (`SelfContained`,
     `Base`, `Chain`, `BaseAndChain`), `Rebase` (`NoRebase`, `NewRoot`,
     `Reroot`), `ReuseEligibility` (`NoRetained`, `BlobReuse`,
     `PassStartUnrecorded`), `Refusal` (`ReceiptBindingInvalid`) and
     `Decision` (`Hit`, `Export(Plan)`, `Refuse`). The input enums use the
     rows' labels.
   - `From<Refusal> for BulkloadRefusal`, so a refusal is a value (R33).
   - `reads_prev_base` and `reads_tips_held` say whether a decision rests
     on an input that is read lazily. Each compares `decide` on two values
     of that input, so it cannot drift from `decide`.
2. **Callers act on the decision, in stages**, reading each input exactly
   where v1 read it:
   - `estate::prepare_base`: a lost base refuses through `decide` on the
     base alone (`lost_base`), before any item's record is read, as v1 did.
   - `estate::capture_item` → `decide_capture`: `retained_capture` now only
     reads the record into `Inputs`, and `chainable` only reads the shape,
     an intact chain and the depth. The `.base` sidecar of a retained
     bundle is read only when `reads_prev_base` says the decision rests on
     it, which is v1's based hit. Then `decide` gives a hit, a typed refusal
     or a `Plan`. `chain_offer` builds the link from the plan;
     `reuse_offer` maps the reuse offer; the plan base is offered when the
     basis is based. A plan v1 cannot carry out (`BaseAndChain`,
     `Reroot`, a depth that is not the link's plus one) refuses
     `CONTRACT_SELF_INCONSISTENT`; under `Policy::V1` none occurs.
   - `git_carry::export_pass` → `shared::write_capture`: the writer decides
     again on the offer (`Inputs::offered`, from `ExportOptions`'
     `prerequisite` and `chain`) once it has read the shallow frontier. It
     queries `chain::source_held_tips` only when `reads_tips_held` says so
     (a link, no base, not shallow), as v1 did. It then writes the decided
     basis: a shallow envelope, `write_full`, a base delta or a chained
     link.
   - This replaces `ExportOptions.chain`'s "Ignored when prerequisite is
     set" rule, `chain_offer`'s filter, `chainable`'s rules and
     `export_pass`'s match. `ExportOptions` keeps its five fields, because
     the tests build it by literal.
   - `write_chained` is gone (its only caller was that match).
     `write_chained_capped` stays as a `#[cfg(test)]` wrapper over
     `write_capture` for REFS-SCALE. `write_bundle` delegates to
     `write_capture` (its only caller is `export_base`).
   - One IO difference: the extend path no longer walks a chained record's
     chain twice (v1 walked it for `restorable` and again in `chainable`).
     The walk reads only CORPUS sidecars and stats, so no bundle, record,
     restore or counter changes.
3. **`Oid` newtype on the prerequisite path** (`shared.rs`).
   `prerequisite_commits` returns `BTreeSet<Oid>`; `thin_header` and the
   pack walk take `Oid`s. `write_excluding_tip_trees` keeps its
   `&BTreeSet<String>` signature, because REFS-SCALE calls it with strings,
   and checks each one into an `Oid` at entry (`GIT_INVENTORY_MALFORMED`
   otherwise). `chain::source_held_tips` still returns strings, because
   its own test compares a `BTreeSet<String>`. Header bytes and order are
   unchanged.
4. **P67 DECISION-CORE** (`src/git_carry/decide_tests.rs`, 5 tests):
   - the row counts per lane (v1 79, L6b 85, L8 199) and v1's policy is
     `Policy::V1`;
   - the label set of all 363 rows' outputs equals the Rust unions';
   - every v1 row (all 363 rows since the review round below) decides the
     reference's output, with 32 fixed-seed draws
     of each "-" input per row;
   - totality and well-formedness over any inputs and policies, with depth
     and age drawn at the policy's edges (512 cases), plus the lazy-input
     laws;
   - stages: under `Policy::V1`, the estate's decision, then the writer's,
     equals one decision in kind, basis, depth and reuse offer (512 cases).
     The rebase label is excluded: for a shallow source at the depth
     limit, the estate's request reads `NewRoot` and one decision reads
     `NoRebase`. The bundle is self-contained either way, and v1 acts on no
     rebase label (it refuses only `Reroot`).
   - Mutation evidence (scratch copies, restored byte for byte):
     - depth `<` → `<=`, a hit ignoring a broken chain, a chain under a
       base without fix 2, a shallow grouped capture based, reuse ignoring
       the pass start, a chain ignoring the source's tips, a lost base not
       refused and a hit ignoring a lost bound base: each fails the pinned
       rows and totality;
     - a re-root ignoring the root's tips fails totality only (no v1 row
       re-roots).
   - The 284 L6b and L8 rows are compared since the review round below.
5. **Grounding** (`docs/formal/catalogue/GitCarry.dhall`):
   - `ChainDepthBounded` cites `write_capture` (for `write_chained`),
     `decide` and `Rebase`, and drops the L6a pending symbol;
   - `BrokenLinkNeverReuseHit` cites `decide` and `Inputs` and has no
     pending symbol left.
   - The bare `decide` also matches transfer.rs's `Outbound::decide`, so
     each row also names a type only decide.rs defines.
   - `tla-check` greps tracked files only, so decide.rs had to be staged
     before its symbols grounded.
6. **Docs**:
   - `docs/formal/README.md`: the roles table, the decision-core section,
     the properties table, the results and what is not proven;
   - comment-only edits in `GitCarry.tla`'s code map, `GitCarryCore.hs`'s
     header and `Types.dhall`;
   - two lines in `docs/design.md`;
   - the P67 row in `docs/plans/2026-10-03-property-test-plan.md`.

## Validation (sting, scratch `$TMPDIR/q42-l6a-decide`, own `CARGO_TARGET_DIR`)

- `just tla-render` then `just tla-render --check`: all 68 committed
  files equal the rendering (the grounding lives only in the catalogue's
  JSON, so no rendered file changed).
- `just tla-check MC_gc_core`: grounding found GitCarry.tla's 31
  operators, 14 constants, 7 mutations, 5 label sets and 27 code symbols
  (24 before), and 7 pending symbols. BulkloadTransfer.tla's 19 symbols
  ground. The self-test was INCONCLUSIVE as expected; `MC_gc_core` PASS
  at 45,062 distinct, 81,520 generated, diameter 29.
- `just formal-nv` over the final tree: exit 0, **55 rows matched, 0
  differed**. `rows --check`: `decide_rows.tsv` current (363 rows),
  byte-identical (the file is unchanged). The schema matched (5 unions).
- Before the gate: the git carry and estate lib tests (295 passed),
  `git_capture_counters`, `git_carry_v2` (65), `git_estimate_dag`,
  `git_group_minimality` (18, P64/P65) and `refs_scale_distinct`, all
  green and unchanged.
- check-fast (the dispatch's `flock ... nice -n 10 nix develop .#default
  --command just check-fast`, the lane's `CARGO_TARGET_DIR`, 15:19 to
  15:30Z, load 6 to 20), over `5a44dac`'s tree: **green**, exit 0. 752
  passed, 0 failed and 9 ignored over 29 test binaries. The lib suite
  passed 473, P67's 5 among them, with 5 ignored. The fault harness (56),
  the power-loss proofs, the CI contract (22 tests), repo-manifest (PASS)
  and gitleaks (no leaks) all passed. The shell tool's 10-minute limit
  moved the run to the background at about 15:29Z, without stopping it;
  the lane waited for its exit before committing.

## Open

- **The over-cap fallback is outside `decide`.** A thin header over the
  cap is still written self-contained by `write_excluding_tip_trees`, and
  the reference has no input for it, so a `Base` or `Chain` decision can
  still produce a self-contained bundle (reported by `chained` and
  `PackStats`). Adding an input to the reference and the rows, or leaving
  it as a writer limit, needs a ruling. The recommended default is to
  leave it: it is a size limit, not custody.
- **The writer's view of an offered link** (`Inputs::offered`) is a
  retained, unchained bundle at depth 0. The writer acts on the basis
  only; the depth is the estate's. P67's stages test is what makes that
  sound under `Policy::V1`. L6b and L8 must revisit it once the writer
  can be offered a base and a link together, or a root.
- **L6b and L8**: P67 checks `decide` on their rows (review round below).
  What the callers do with an L6b or L8 decision stays unchecked until
  each lane lands its custody; today they refuse it
  (`CONTRACT_SELF_INCONSISTENT`).
- Carried from L4, untouched here: the `import_base` bare-IO finding, the
  content-name `DIGEST_MISMATCH` finding, Q46's window semantics.
- **Coordinator**: distil these facts onto TIN-4543 and the SSOT ledger.
  This lane posted nothing to Linear.
- Scratch: `/srv/scratch/jess/tmp/q42-l6a-decide` (logs, mutant logs and
  the lane's target dir) and the worktree's ignored `target/fault`
  (`just fault-harness` sets that target dir itself). Both are this lane's
  own and hold nothing durable.

## Review round 1 (2026-10-05, same worktree and branch)

The coordinator's dispatch carried two medium findings and seven low ones
on `5a44dac`. Both mediums are fixed in `64ae349`. The lows are listed for
the coordinator and were left unfixed, as the dispatch asked. Rulings:
OI-1003-Q42, OI-1003-Q43, OI-1003-Q46, R-N13.

- **P67 compares all 363 rows** (medium 1). The policy is a column of each
  row, so the L6b and L8 rows need no custody to check `decide`. The v1
  rows still pin `Policy::V1`. Scratch mutants that survived the old P67
  and now fail the pinned test: window `>=` → `>`, the window counting
  `age` not `age + 1`, a window that never ends, a window end labelled
  `NoRebase`, fix 2 dropped from the offer, and a based bundle never
  linkable under fix 2. A re-root that ignores the root's tips now fails
  the pinned test too, not only totality.
- **The staging is shared code** (medium 2). `decide.rs` gains
  `decide_recorded` (the estate stage: the bound base read through a
  callback only when `reads_prev_base`) and `decide_offered` (the writer
  stage: the source's tips read through a callback only when
  `reads_tips_held`). `estate::decide_capture` and
  `shared::write_capture` call them, and P67's stages test calls them
  instead of its own copy. Behaviour is unchanged.
- **Estate test** `a_retained_based_bundle_bound_to_a_lost_base_is_never_a_reuse_hit`:
  two worktrees of one repository capture twice (two reuse hits); then the
  group's base bundle is lost and its base record removed, so the next
  pass regenerates a retained base. Both items' retained based bundles
  still name the lost base, so both refuse `RECEIPT_BINDING_INVALID` and
  their records stay byte for byte. Scratch mutants: the estate stage never
  reading the bound base fails stages and this test; `decide_capture`'s
  read answered `Retained` fails this test; the writer never reading the
  tips fails stages. The unmutated tree passes; each mutant was restored
  and the diff compared byte for byte.
- Docs: the P67 row of the property-test plan, the formal README (the
  "pinned rows can" sentence, the stages paragraph, the "not proven"
  bullet), and line 10 of `GitCarryCore.hs`'s header. These are comment and
  prose edits only; `decide_rows.tsv` is unchanged.

Validation (sting, scratch `/srv/scratch/jess/tmp/q42-l6a-fix1`, own
`CARGO_TARGET_DIR` warmed from a copy of the lane's earlier target):

- `cargo fmt --check` and `cargo clippy -p bulkload-agent --all-targets
  -- -D warnings`: clean.
- The first check-fast over `64ae349` (16:01Z to 16:08Z) failed in
  `git_capture_counters`: its `estate-capture` verb runs under the CLI's
  default `--min-free-percent 25`, its corpus sits in `TMPDIR` on
  `/srv/scratch`, and that filesystem was at 24.7% free. The refusal was
  `DESTINATION_SPACE_INSUFFICIENT` on space, not this change. The lane
  removed its own 4.0G `target/debug/incremental` (27.3% free) and reran
  with `CARGO_INCREMENTAL=0`.
- The second check-fast over `64ae349` (16:09Z to 16:33Z,
  `CARGO_INCREMENTAL=0`) passed: exit 0, every tier green, including
  `git_capture_counters`, the fault harness, `resume-power-loss`, the repo
  manifest and the CI contract tests.
- Review round 1 close-out (2026-10-06): `/srv/scratch` had fallen to 22%
  free again from other lanes' growth, so the lane removed its own 4.1G
  round-0 scratch target (`/srv/scratch/jess/tmp/q42-l6a-decide/target`,
  27% free after) and committed this note as `b40bdf3`.
- Check-fast over `b40bdf3`, first run (16:15Z to 16:47Z): **failed**, exit
  101, on space again. `git_group_minimality` had 10 of 18 tests refuse
  `DESTINATION_SPACE_INSUFFICIENT` from the `estate-capture` and
  `estate-apply` verbs; nothing else failed. That session ended before a
  rerun finished, with `64ae349` and `b40bdf3` unpushed.
- Reattached session (2026-10-06, same worktree and branch, no code
  change): `/srv/scratch` at 27.9% free. Check-fast over `b40bdf3`
  (the dispatch's `flock ... nice -n 10 nix develop .#default --command
  just check-fast`, started 17:52Z, waited on the lanes' lock until about
  18:23Z, `CARGO_INCREMENTAL=0`, `TMPDIR`
  `/srv/scratch/jess/tmp/q42-l6a-fix2`): **passed, exit 0**, recorded at
  19:38:52Z. 29 test results, 753 passed, 0 failed, 9 ignored: the lib
  suite 474 (P67's 5 and the new estate test among them),
  `git_capture_counters` 1, `git_carry_v2` 65, `git_group_minimality` 18,
  the fault harness 56, the power-loss proofs 9, repo-manifest PASS and the
  CI contract's 22 tests OK. The last test output was at 18:35Z; the
  command returned an hour later, and the cause was not found. The shell
  tool's 10-minute limit moved the run to the background without stopping
  it, and the lane waited on its status file.
- `2b63b6e` and the commit after it change only this note. A second
  check-fast, queued over `2b63b6e` at 18:54Z, had not got the lanes' lock
  by 20:05Z and was not waited for: it has no result. The push rests on the
  exit 0 over `b40bdf3`.
- The branch is 4 commits behind `origin/main` and was not merged with it:
  the dispatch did not ask for a merge.

Low findings, not fixed (for the coordinator):

1. `decide_tests.rs` `staged()`: the stages property still composes the
   two stage functions itself (the record read, the offer from the basis,
   the depth and reuse of the result), not through `estate::decide_capture`,
   `chain_offer` or `shared::write_capture`. Medium 2's fix shares the
   stage functions; the composition stays a model.
2. `shared.rs` `write_capture`, `Basis::Chain` arm: `tips` is filled only
   when `reads_tips_held`; Chain implies it was read and non-empty only
   because `Inputs::offered` sets `root_held = false`. Nothing asserts it.
3. `docs/formal/README.md` roles table: "L6's fixed-seed property test"
   should say L6a.
4. `DRAWS_PER_ROW`: 32 random draws per "-" input, weaker than the
   reference's whole-domain enumeration in `rowDecision`.
5. The label test checks rows against the test's hand-written arrays, not
   the Rust enums themselves.
6. `GitCarry.dhall`: the bare `decide` code symbol also grounds on
   transfer.rs's `fn decide`; the qualified `git_carry::decide::decide`
   would not.
7. `GitCarryCore.hs` header (about lines 35 to 49) still cites
   `shared::write_chained` and `Retained::None`; `GitCarry.tla` line 9
   still says "P67, lane L6".

## Recheck and ship (2026-10-06, same worktree and branch)

Rulings: OI-1003-Q42, OI-1003-Q43, OI-1003-Q46, R-N13.

- Verdict **CLEAN** at `77d4e17`. Read the diff `856e7b9..77d4e17` and the
  callers it touches.
  - Medium 1 is fixed: `p67_every_row_decides_what_the_reference_decides`
    compares all 363 rows under each row's own policy.
  - Medium 2 is fixed: `estate::decide_capture` calls `decide_recorded`,
    `shared::write_capture` calls `decide_offered`, and P67's stages test
    calls the same two functions. The estate test reaches the lazy
    bound-base read through `capture`: without the read the third pass
    would report two reuse hits, not two refusals.
  - No new defect found. `decide_capture`'s new
    `CONTRACT_SELF_INCONSISTENT` arm is unreachable (`reads_prev_base`
    holds only for a held record, and a held record always has a retained
    capture). `write_capture` reads the tips exactly when it did before.
- The seven lows stay open, as listed above.
- Check-fast was **not** run again in this stage. `/srv/scratch` was at
  26% free, and a fresh target directory would have put it under the 25%
  floor the estate tests need, for this lane and the others. The verdict
  rests on the recorded exit 0 over `b40bdf3` (its log shows P67's 5 tests
  and the estate test passing) and on `b40bdf3..77d4e17` changing only
  this note.
- The branch is 4 commits behind `origin/main` (`2247ab8`: CI and docs
  only) and merges cleanly (`git merge-tree`). It was not merged here.
- **PR #191** opened against `main`:
  https://github.com/Jesssullivan/bulkload/pull/191. Not merged. Nothing
  posted to Linear; the coordinator still owes the TIN-4543 and SSOT
  ledger entries.
- An earlier session's check-fast queued over `2b63b6e`
  (`/srv/scratch/jess/tmp/q42-l6a-fix2/check-fast3.start`) has no exit
  record. This stage did not look for it.
- Scratch: `/srv/scratch/jess/tmp/q42-l6a-recheck` (the PR body and the
  commit message), nothing durable.
