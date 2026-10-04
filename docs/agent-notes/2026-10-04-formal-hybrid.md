# 2026-10-04 — formal hybrid (lane B, sprint 2: Dhall catalogue and Haskell N-version explorer)

Rulings:

- OI-1003-Q7: proof package, formal model; CI stays slim.
- OI-1003-Q32: TLA+ with TLC is the checker of record; a hybrid with a
  typed catalogue and an N-version core.
- OI-1003-Q37 and OI-1003-Q38: cited by the lane brief. Their text is not
  in `docs/slo.md`, nor on `docs/coordinator-20261003` at `8dd4546`, so this
  note does not restate them (open).
- R-N13.
- Also followed: R-N11, R-N12, R-N92 and R-N101 (wording), R-N104, R-N98
  (no hook bypass).

Branch `docs/formal-hybrid-20261004`, worktree
`bulkload.worktrees/formal-hybrid-20261004`, created from
`origin/docs/tla-model-20261003` at `27581be` (PR #160, stacked). This
session was its only writer. No PR was opened, as the brief asked.

Shas:

- `0781bd6`: the Dhall catalogue (`docs/formal/catalogue/Types.dhall`,
  `Catalogue.dhall`), `just tla-render`, and `tla-check`'s staleness and
  grounding steps. `gen_cfgs.py` is still present here, so the byte-identity
  proof can be re-run at this sha.
- `efde8ac`: the provenance switch, `gen_cfgs.py` deleted,
  `docs/formal/hs/Explorer.hs`, `just formal-nv`, and the README's Hybrid
  roles section with the parity results.
- `40363ff`: merge of `origin/docs/tla-model-20261003` at `94c3eb8`
  (#160's head moved during the session: it merged main through #159). No
  conflicts; `docs/formal` untouched; all 19 code symbols still found under
  `crates/` after it.
- `65ad448`: this note, with the README and explorer comment corrections
  made after `efde8ac`.
- Round 2 (review fixes, below): `c94d2f9` (catalogue tables, grounding
  both ways), `2eb7ef3` (formal-nv over every in-domain mutation row; the
  explorer described as a second encoding), and the commit that adds the
  round 2 section of this note.

## What was done

1. **Dhall catalogue.** Every TLC config as typed `Constants`, an `Expect`
   union that carries only what each row kind needs, a total `merge` from
   `Mutation` to its verdict, an assert on the primary `MC_neg_` rows (at
   `65ad448` it compared them with the literal `[0..18]`, so it did not
   hold for a newly added mutation; round 2 fixes that),
   `gen_cfgs.py`'s two checks as asserts, and 18
   traceability rows `{tla, slo, ruling, codeSymbol, ptest}` (the 16 frozen
   safety invariants other than `TypeOK`, plus `RunsClose` and
   `AllRunsFinish`). Union labels are the TLA+ names (`showConstructor`).
   No Prelude import, so it evaluates offline.
2. **Rendering.** `just tla-render [out] [json]`: dhall-to-json, then jq
   writes one file per entry (names checked unique and safe). Dhall 1.42.3,
   dhall-json 1.7.12 and jq 1.8.2 from the pinned nixpkgs.
3. **`tla-check` gates.** Before TLC: staleness (rendering equals the
   committed 44 files byte for byte, same file set) and grounding (66
   operators defined in the spec, 16 constants declared, 19 mutations in
   its `Mutations` set, 19 distinct code symbols found by `git grep -w`
   under `crates/`).
4. **Haskell explorer.** `docs/formal/hs/Explorer.hs`, base and containers
   only, GHC 9.10.3. An explicit-state BFS of the 27 actions TLC's
   coverage shows enabled in `MC_nv_ledger`, with the 17 safety
   invariants, deadlock detection, presets `nv_core` and `nv_ledger`,
   14 in-core mutations, and JSON counterexamples. It reads no TLA+, no
   `.cfg` and no rendered file. It is a second encoding of the spec
   (independent code, shared design), not an independent implementation:
   it was transliterated from the spec's action definitions (round 2).
5. **`just formal-nv`.** Standalone and last in the justfile; in no tier
   and not in CI. Builds with `ghc -O1 -Wall -Werror` into a private
   `mktemp` directory. At `65ad448` it failed unless both presets matched
   TLC's distinct counts and the three core mutations violated their
   `configs.tsv` properties; round 2 widens it to every in-domain row.
6. **README.** The Hybrid roles section (roles, the catalogue's
   guarantees, the replacement proof, the explorer and the parity table),
   plus the file table, running notes and Mutations note. No frozen name
   changed.

## Evidence

- **Byte identity (before deleting `gen_cfgs.py`).** At `27581be`'s tree
  plus the catalogue, three sha256 manifests over the 44 files
  (`configs.tsv` and 43 `MC_*.cfg`) were identical, `259bd98c…f340`: the
  committed files, `python3 docs/formal/gen_cfgs.py` into scratch, and the
  catalogue rendered by dhall-to-json and jq. `just tla-render` into
  scratch gave the same manifest, and into `docs/formal` it left
  `git status` clean for those files. After the provenance switch, `git
  diff --numstat` showed exactly 1 line in and 1 line out in each of the
  44 files: 43 `\*` lines and one `#` line.
- **The catalogue rejects bad edits** (scratch copies):
  - a new mutation with an index but no verdict: `Error: Missing handler:
    new_break` at the verdict `merge`;
  - `double_read`'s primary row removed: `Error: Assertion failed` (the
    primary-row assert);
  - `MC_live` under `SYMMETRY`: `Error: Assertion failed` (the liveness
    assert).
- **`tla-check` gates, negative runs** (`just tla-check MC_nv_core`, each
  edit undone after; `docs/formal` restored byte for byte, checked by a
  tree hash). Each stopped with exit 1 before any TLC run:
  - a stray line appended to `MC_nv_core.cfg`: "MC_nv_core.cfg differs
    from the catalogue's rendering";
  - an extra `MC_extra.cfg`: "differ in their file set: < MC_extra.cfg";
  - code symbol `max_steps` misspelt in the catalogue: "code symbol
    max_stepz is not found under crates/";
  - action label `Tick` renamed: "Tock is not defined in
    BulkloadTransfer.tla".

  A gate failure now removes its scratch; the four scratch directories
  these runs left (before that change) were mine and are deleted.
- **TLC with the gates on.** `just tla-check MC_nv_core
  MC_neg_held_before_commit MC_neg_commit_before_fsync
  MC_neg_src_ledger_carries_r25` (before the provenance switch): the
  catalogue line, then INCONCLUSIVE, PASS 15,834/44,312/45, and FAIL on
  `HeldAfterCommit`, `RecordImpliesBytes` and `R25_NoDurableReread`; 22 s.
  Then a full `just tla-check` over the re-rendered configs (the
  `docs/formal` tree committed as `efde8ac`, load near 35): gates
  passed (44 files current; 66 operators, 16 constants, 19 mutations and
  19 code symbols grounded), and **43/43 rows matched: 10 PASS, 3 REACHED,
  28 FAIL, 1 SIMULATION, 1 INCONCLUSIVE**. Every pass row's distinct and
  generated counts and diameter equal the run of record's; 529 s, peak RSS
  1,865 MiB.
- **Replay from the commit object.** `git archive 0781bd6 docs/formal`,
  then `gen_cfgs.py` and the archived catalogue rendered into scratch:
  the committed files, `gen_cfgs.py`'s output and the catalogue's output
  all hash to `259bd98c…f340`.
- **Explorer parity** (`ghc -O1`, host sting, load near 45):

  | Row | TLC | Explorer |
  |---|---|---|
  | `MC_nv_core` | PASS 15,834 distinct, 44,312 generated, diameter 45 | pass 15,834, 44,312, 45 levels; 0.7 s |
  | `MC_nv_ledger` | PASS 142,450, 497,089, 49 | pass 142,450, 497,089, 49 levels; 7.6 s |
  | `MC_neg_held_before_commit` | FAIL `HeldAfterCommit`, 7 states | violation `HeldAfterCommit`, 7 states |
  | `MC_neg_commit_before_fsync` | FAIL `RecordImpliesBytes`, 9 states | violation `RecordImpliesBytes`, 9 states |
  | `MC_neg_src_ledger_carries_r25` | FAIL `R25_NoDurableReread`, 15 states | violation `R25_NoDurableReread`, 15 states |

  TLC's one-worker hand runs of the three mutation configs gave the same
  action sequences as the explorer's counterexamples. Under `runghc`,
  `MC_nv_core` took 17 s. `just formal-nv` printed every row `match yes`.
  No count mismatch, so no divergence to triage.
- **check-fast** (CI toolchain: `flock .check-fast.lock nice -n 10 nix
  develop .#default --command just check-fast`, foreground; cargo target in
  this worktree): **pass**, 2026-10-04 05:44–05:55 EDT after about 27
  minutes queued for the lock. It ran on the tree of `40363ff` (the merge)
  plus this note and the README and explorer comment corrections. Every
  stage ran: the repo manifest, ruff, shellcheck, actionlint; gitleaks
  ("no leaks found"); fmt, the three clippy runs and 25 cargo test
  results, all ok, none failed; the fault harness, the power-loss proofs
  and `resume-power-loss`; the 22 contract tests. No stage reads
  `docs/formal`'s Dhall or Haskell, so `tla-check` and `formal-nv` above
  are their validation.

## Open

- OI-1003-Q37 and OI-1003-Q38: record their text in `docs/slo.md` (or the
  coordinator branch) so this lane's citations can be checked.
- Freezing `MC_nv_ledger` as the second N-version row still needs a ruling;
  the explorer already matches it.
- The explorer is not independent of the spec (round 2). An independent
  check of the spec against the code needs a second author deriving each
  action from the cited Rust function without the spec, or property-test
  or fault-harness traces replayed as explorer behaviours. Neither is done.
- The low findings of the round 2 review are not fixed (listed below).
- Carried over, unchanged by this lane: the model's re-check against PR #154
  (`Store::open`'s `racy_guard` row deletion, `SALVAGE_BOUND_EXCEEDED`); the
  code fixes for the unsealed state root and the failing ledger commit; the
  rulings on R25's "held durably" and slo.md's R25 wording; Linear
  (TIN-4543 and the SSOT ledger, owned by the coordinator). #160 is open;
  this branch has no PR yet (the brief said not to open one).

## Round 2: review fixes (2026-10-04)

Five medium findings (two of them the same defect), all fixed. Rulings
as above (OI-1003-Q7, OI-1003-Q32, OI-1003-Q37, OI-1003-Q38, R-N13);
every commit is signed and ends with that line.

1. **formal-nv tested 3 of 17 invariants in a failing case** (and two of
   them hold by construction in the core). `2eb7ef3`:
   - The catalogue has a new output, `nversion`. It lists every `MC_neg_`
     row inside the explorer's domain (`explorerModels`): 17 rows, the 14
     supported mutations' primary rows plus `reread_unchanged`,
     `reread_changed_only` and `record_racy_ledger`.
   - The explorer gained bound flags (`--seats`, `--runs`, `--crashes`,
     `--edits`, `--foreign`, each a count from 0 to 9, `keyBase`'s digit).
   - `formal-nv` runs each row at its own bound and requires `violation
     <named property>`. For each primary row it also runs TLC with one
     worker and the explorer with every safety invariant checked (a
     scratch config the catalogue renders). Both must report the same
     first violated invariant after the same number of states.
2. **The primary-row assert held only for today's 19 mutations**, and
   `allMutations` and `safety` were hand-written lists (two findings, the
   same defect). `c94d2f9`:
   - The primary rows are a record keyed by mutation label (`primary`),
     merged over `T.Mutation`. A missing field is `Missing handler` and an
     extra one `Unused handler`.
   - The primary-row assert compares positions with `range n`, where `n`
     is the record's field count.
   - `Types.dhall` has label-keyed tables for Property (with a class),
     Action and Witness. `allProperties`, `allActions`, `allWitnesses`,
     `allMutations`, `safety` and the traceability assert's expected list
     all derive from them.
3. **Explorer independence overstated.** `2eb7ef3`: the explorer header,
   the README's Hybrid roles table and explorer section, the N-version core
   section and this note now say "second encoding of the spec: independent
   code, shared design". They say what that cannot catch: a misreading of
   the Rust code that the spec makes. They also list what parity does not
   show: `TypeOK`, `NoClobber`, `S2_BackupLockBounded` and
   `ClosureAccounted` never fire on the explorer inside the domain.
4. **Grounding was one-directional for mutations.** `c94d2f9`: tla-check
   compares the catalogue's mutations with the spec's `Mutations` set (less
   `"none"`) as sets in both directions, and does the same for the
   constants against the spec's `CONSTANTS` block.

Evidence (host sting, `TMPDIR` under a private lane directory):

- **Rendering unchanged.** `just tla-render` into scratch gives the 44
  files byte-identical to the committed ones (sha256 manifest, `cmp`). The
  evaluated catalogue's `files`, `grounding` (as sets) and `invariants`
  equal `65ad448`'s.
- **Catalogue negatives** (scratch copies of `catalogue/`):
  - a new mutation with an index and a verdict but no `primary` field:
    `Missing handler: new_break`, at `primaryOf`'s merge (and through
    `just tla-check MC_nv_core` in a scratch clone, which stops before TLC);
  - the field present but no row in `negRows`: `Assertion failed` (the
    primary-row assert);
  - a new safety-class property with no fail row: `Assertion failed` (the
    falsifiability assert);
  - a new temporal property with no traceability row: `Assertion failed`
    (the traceability assert);
  - a new property label with no table field: `Missing handler: NewInv`;
  - a table field holding another label's value: `Assertion failed` (the
    label assert).
- **Grounding negatives** (`just tla-check MC_nv_core` in a scratch clone
  at `2eb7ef3`, each stopped with exit 1 before TLC):
  - `"new_break"` appended to the spec's `Mutations` and a constant
    `ExtraKnob` declared: "constant ExtraKnob is declared in
    BulkloadTransfer.tla but the catalogue does not set it", "the spec's
    mutation new_break has no catalogue entry (no verdict, no MC_neg_
    row)";
  - `"double_read"` removed from the spec's set: "mutation double_read is
    not in the spec's Mutations set".
- **tla-check with the gates on**, at the final tree: `just tla-check
  MC_nv_core` printed "catalogue: 44 files current; grounded 66 operators,
  16 constants, 19 mutations, 19 code symbols", then INCONCLUSIVE for the
  budget self-test and PASS 15,834/44,312/45 for `MC_nv_core`.
- **formal-nv**, at the final tree, 06:40 EDT, load 21-30, 48 s: both
  presets matched (15,834 and 142,450 distinct), and all 31 mutation runs
  matched (17 named, 14 every-invariant). Every-invariant results:
  - 12 primary rows stop first at their verdict;
  - `reread_ignore_ledger` stops at `R25_NoDurableReread` (19 states);
  - `skip_output_row` stops at `LedgerAfterHeld` (12 states);
  - TLC and the explorer agree on all 14.
- **The review's scenario now fails the gate.** In a scratch clone, 11
  explorer invariants were replaced by `\_ _ -> True` (the same 11 as the
  review). `just formal-nv` reported "18 row(s) disagree with TLC", exit 1.
- **check-fast** (CI toolchain: `flock .check-fast.lock nice -n 10 nix
  develop .#default --command just check-fast`; cargo target in this
  worktree): **pass**, exit 0. It ran on the tree of `2eb7ef3` plus this
  note's draft. The shared lock was held by other lanes for about 38
  minutes (acquired 07:33:21 EDT), longer than one foreground tool call
  allows (10 minutes). So the exact command ran as my own background job,
  holding a private lock file, and I waited on that file in the
  foreground until it finished at 07:37:26 (R-N104: a wait on a file). It
  was not left running. Every stage ran:
  - the repo manifest, ruff, shellcheck and actionlint;
  - gitleaks ("no leaks found");
  - fmt, the three clippy runs, and 25 cargo test results, all ok with none
    failed;
  - the fault harness, the power-loss proofs and `resume-power-loss`;
  - the 22 contract tests.

  No stage reads `docs/formal`, so `tla-check` and `formal-nv` above are
  this round's validation.

Scratch: everything under my private `$TMPDIR/formal-hybrid-r2`, deleted at
the end. One `tla-check.*` directory leaked by the low finding below (a
catalogue that fails to evaluate aborts tla-check before its cleanup). It
was mine and is deleted.

Low findings, not fixed (as the brief asked):

- formal-nv exercises only one-seat bounds. With two seats the explorer's
  generated counts differ from TLC's, which counts one `Tick` successor per
  witness. So the README's generated-count paragraph is not true in
  general.
- formal-nv compares the presets' distinct count and outcome only, and
  TLC's counts sit as constants in the justfile. tla-check never compares
  pass rows' counts.
- The README's run-of-record table labels `MC_nv_core` and `MC_nv_ledger`
  "sym", but neither config has `SYMMETRY`.
- Code-symbol grounding is a whole-word `git grep` that also matches
  comments, so it is close to vacuous for generic words (`racy`).
  `SourceDone` is never defined as an fn or a const.
- `gen_cfgs.py` checks the catalogue's types do not imply were dropped:
  `WithinBudget` exactly once, a fail row never naming `TypeOK`, temporal
  properties only under `PROPERTY`. The `SYMMETRY` assert reads only
  `p.properties`.
- Traceability: `ruling` and `ptest` are free text and nothing grounds
  them. The README table is a second hand-written copy. `RecordImpliesBytes`
  is tagged S4, which slo.md does not support.
- tla-check leaks its scratch when the catalogue fails to evaluate or
  tla-render's name check refuses (`set -e` exits before `rm -rf`).
