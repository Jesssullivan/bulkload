# 2026-10-07 — Q42 lane L6b: grouped items chain under their plan base (P68)

Lane `q42-l6b-grouped-chain`, branch `feat/q42-l6b-grouped-chain-20261007`,
worktree `bulkload.worktrees/q42-l6b-grouped-chain-20261007`, from
`origin/main` `34e945e`, merged with `8083675` (#194) at the ship stage.
Pushed; the ship stage opens the PR from the head named in "Recheck and
ship" below.

Rulings: OI-1003-Q42, OI-1003-Q46, OI-1003-Q62, OI-1003-Q63, R-N72, R-N13.
Started under OI-1003-Q81 ("L6b starts now"); validation under OI-1003-Q85.
The design is the TIN-4543 comment "Q42 L6b design: grouped items chain
under their plan base (P68)", written against L6a; every line reference in
it was re-read against main.

## Commits

| Sha | What |
| -- | -- |
| `690b080` | P68 lands red: test only (`tests/git_grouped_chain.rs`, the shared `tests/git_group/mod.rs`) |
| `0ec447f` | The fix: `decide.rs`, `shared.rs`, `chain.rs`, `estate.rs`, and the tests |
| `089e071` | `design.md`, the formal catalogue and its rendering, the property plan, this note |
| `3a36f82` | Review round 1: `publish_prior` and `bound_base` in `estate.rs`, four custody tests, the P68 restore pin and the A → B → A row |
| `98621bd` | Review round 1 docs: `design.md`, the property plan, this note |
| `c9ea1ce` | Ship stage: the restore-cost pin cites #147, not #148 (docs and two test comments) |
| `4dae1b8` | Ship stage: merge of `origin/main` `8083675` (#194, the seeded proptests and the no-fuzz guard) |
| the commit after it | Ship stage: this note |

All are signed. No force-push, no rebase; `origin/main` had not moved
from `34e945e` at the first two pushes and was `8083675` at the third.

## What changed

Before this lane a changed capture of a grouped item was a delta on the
plan base alone, so pass `k` re-packed everything committed since the base.
Now it also declares the source-held tips of its own prior capture
(`Basis::BaseAndChain`), so it packs one pass's work. This is the code's
only policy (D4: no flag; a reader from before it fails closed).

- `decide.rs`: `Policy::CODE` (depth 8, root window 0, chain under base),
  equal to the L6b rows' policy. `decide` and `extend` are unchanged.
  `Policy::V1` stays for the v1 rows and the back-compat tests.
- `shared.rs` `write_capture`: the `BaseAndChain` arm writes one thin bundle
  over the base's commits and the link's held tips. Over the header cap the
  existing fallback writes it self-contained. The writer always decides
  under `Policy::CODE`; an estate under `Policy::V1` never offers a link
  beside a base, so the writer decides the same.
- `chain.rs` `flatten(head, bases, links)`: canonicalizes the corpus path
  once (#183 item 2), stages and digest-checks each bound base, requires it
  self-contained, imports it before the oldest link, and lets the oldest
  link declare prerequisites only when a base is bound. A missing base or
  link is `SEALED_OBJECT_MISSING`.
- `estate.rs`:
  - `chain_links` returns the links and the set of bound bases
    (`Chain { links, bases }`). The head and every link with a `.prior` is
    bound to its `.base`; the root only when its header declares
    prerequisites. Under `Custody` a bound base that is gone or replaced
    breaks the chain.
  - `bound_base`: a bundle with no `.prior` and no `.base` is
    `PrevBase::None` only when its header declares no prerequisites; a
    based bundle whose `.base` sidecar is gone is `PrevBase::Lost` (review
    round 1). A chained bundle combines its own base with every link's.
  - `chain_offer`: the `BaseAndChain` refusal is gone; `Reroot` still
    refuses (L8).
  - `prepare_base`: the record is written with `write_new` (a hard link, so
    an existing record is never replaced); a record that appears
    concurrently refuses `RECEIPT_BINDING_INVALID` and stands.
  - `stage_base` maps a missing base to `SEALED_OBJECT_MISSING` (#181), and
    so does a based bundle whose `.base` sidecar is gone.
  - `apply_item` flattens over the chain's bound bases; `item_space` counts
    their bytes.
  - `capture_in` takes the decision policy (crate-private; every caller
    outside the tests passes `Policy::CODE`).
- Docs and formal: `design.md` gains the L6b paragraph, the R-N72
  restore-contract amendment (D2) and the drift-link line (D1, #149). The
  two L6b pending symbols are grounded (`bind_base`, `bound_base`,
  `stage_base`, `write_new`, with `write_capture` and `chain_links`).
  `MC_gc_base_missing_untyped` is an expected pass (`BaseMissingTyped =
  TRUE`, never `Advance, Crash, GC, Rewrite`). "No code yet" is gone for
  L6b in the README, `GitCarry.tla`, the `MC_gc_fix2*` configs and
  `GitCarryCore.hs`. The property plan has the P68 row.
  `tests/data/decide_rows.tsv` is unchanged.

## Three things the design did not say

1. **A capture that reproduces its link byte for byte.** When only another
   item's worktree moved (P64's grouped head-move rows), an item's key
   changes but its bundle does not: under a plan base the prior's held tips
   are the base's own commits, so the header and the pack are the prior's.
   The bundle then has the prior's name. `publish_prior` used to refuse a
   self-link `CONTRACT_SELF_INCONSISTENT`; five P64 grouped rows failed on
   it. It now leaves the link's recorded custody as it is and writes no
   sidecar. The bundle is the link. Review round 1 found this guard one
   step too short (below): the same holds for any bundle already in the
   link's chain.
2. **A based link whose bound base is lost is never offered**
   (`chain_offer`). The reference's `extend` does not read `prev_base` for
   a based link, so the plan says `BaseAndChain`; the estate offers no link
   and the writer writes the plan base's delta, as v1 did. Nothing could
   restore a capture chained on that bundle. This is an estate rule outside
   the reference (see Open).
3. **The root's `.base` binds only when the root declares prerequisites.**
   A root written self-contained (a shallow source, a header over the cap)
   still gets a `.base` sidecar from `capture_item`; binding it would make
   a lost base break a chain that never needed it.

## P68, red then green

P68 is `tests/git_grouped_chain.rs`: fixtures Probe-2 (main plus `wt`) and
P64-3 (P64's three items), 12 passes after the first, the four clauses per
recaptured item, re-base captures excluded and pinned (Q62), restores at
`k ∈ {1, 2, 9, 10, 12}`. P64's helpers moved to `tests/git_group/mod.rs`.

**Red** on the test-only commit `690b080` (run on that commit's tree, main's
code): both tables failed, 90 violations (Probe-2) and 136 (P64-3). At
`k = 1` no item had a `.prior`. Bytes per item bundle, Probe-2 main:

| k | bytes (red) | bytes (green) |
| --: | --: | --: |
| 1 | 529,554 | 529,553 |
| 2 | 1,054,568 | 529,917 |
| 3 | 1,579,370 | – |
| 8 | – | 529,984 |
| 9 | 4,729,140 | 4,728,951 (the re-base) |
| 10 | – | 529,988 |
| 12 | 6,304,245 | 530,002 |

Red grows about `k × 512 KiB`; from `k = 2` each bundle also packed 4 to 21
objects its prior already held. The restores passed on red (v1 restored
correctly; it only re-packed).

**Green** on the fix: 55 chained captures (22 Probe-2, 33 P64-3), each
under the bound 589,824 B; the largest is 530,002 B. Every item re-bases
once, at `k = 9`: 4,728,951 and 4,728,956 B (Probe-2), 4,728,950, 4,728,957
and 4,728,783 B (P64-3). That is about 9.46 MB per pass for Probe-2 and
14.19 MB for P64-3, the cost L8's re-root removes (Q62). The pin is
`[9 × 512 KiB, 9 × 576 KiB]` per item.

Two test fixes rode in the fix commit, not the red one: the sidecar name
reader decodes a two-byte length (item bundle names are 136 bytes; the red
run never read a `.prior`), and two clippy rewrites.

## Re-pinned, one by one

- **P64 grouped rows.** No numeric assert moved; all 18 rows pass
  unchanged. What changed is the shape of pass 2: the moved item's bundle
  now declares 16 prerequisites, not 15 (its prior's held tip, the detached
  HEAD, beside the base's 15 commits; header 3,098 B, 54 B more), and has a
  `.prior`. `later_pass` now asserts it: every later capture's moved-item
  bundle has `.prior`, and `.base` exactly in a group. The thin-reuse row's
  `.prior` check, which ran only for the chained layout, runs for both.
  Measured grouped pass 2: head move 4,742 B, worktree large edit 4,916 B,
  revert to older content 70,417 B (the 64 KiB blob whole, as before),
  untracked payload 529,409 B, thin reuse 4,925 B then 4,981 B at pass 3.
- **REFS-SCALE.** New row
  `a_chained_pass_under_a_plan_base_reaches_the_thin_cap_sooner_and_falls_back`
  in `src/git_carry/refs_scale_tests.rs`: the `BaseAndChain` header is
  exactly one 54-byte prerequisite line longer than the base's delta per
  held tip the base does not name; one byte under its length the pass is
  written self-contained and imports exactly; at its length it stays thin
  and flattens on the base and the prior. That file belongs to the
  `ci-slim-source-gate` lane: this lane added the row and changed one call
  (`chain::flatten` gained its `bases` argument). Nothing else there moved.
- **P67.** The row-count test also pins the L6b rows' policy to
  `Policy::CODE`; the staged test runs under `Policy::CODE` and
  `Policy::V1`.
- **Unchanged:** `git_capture_counters` (inequality 1 bytes and
  `census_walks`: 4 per changed item, 1 per reuse), P-CHAIN (`wp2_chain`),
  P40/P42/P43/P45, `source_inert_tests`, `refs_scale_distinct`.

## Custody tests (`estate.rs`, `l6b_grouped_chain`)

Back-compat corpora are written under `Policy::V1` through `capture_in`,
then captured under `Policy::CODE` in the same corpus, then applied.

- A v1 grouped based record becomes the depth-0 root of the next changed
  capture's chain; `item_space` charges head, link and base.
- A v1 ungrouped chain is still a hit, and extends as a plain chain.
- v1 records bound to a lost base refuse `RECEIPT_BINDING_INVALID` and are
  never chained on.
- `prepare_base` never replaces a record that appears concurrently.
- A missing plan base refuses apply `SEALED_OBJECT_MISSING`, based and
  chained (#181).
- A chain bound to two bases restores from both (D5); either base alone
  refuses `GIT_INVENTORY_MISSING_PREREQUISITE`; once the old base is lost
  the chain is broken and the next capture is the new base's delta.
- A drift-marked bundle is a chain link under the plan base (D1, #149).
- A chain in a relative corpus path flattens (#183 item 2).
- Review round 1: a capture that reproduces its chain's root stands as the
  root; one that reproduces a link of its own chain keeps that link's
  custody; a recapture that reproduces a broken chain's head drops its
  stale `.prior`; a based bundle whose `.base` sidecar is gone is never a
  hit or a link.

Mutants, in a scratch copy under `/srv/cache/jess/q42-l6b-grouped-chain-mut`
(never the lane worktree), each of which fails a test:

| Mutant | Fails |
| -- | -- |
| `flatten` imports only the first base | two-bases test |
| `bound_base` ignores the links | two-bases test |
| `chain_links` skips a link's base | two-bases test |
| `prepare_base` replaces the record | no-replace test |
| a based link with a lost base is offered | lost-base test |
| `stage_base` leaves the missing base untyped | missing-base test |
| the writer drops the link's tips | both P68 tables |
| `publish_prior` without the chain-membership guard | root custody test, P68 A → B → A row |
| `publish_prior` keeps a stale `.prior` on an unchained export | broken-head test |
| `bound_base` answers `None` for a sidecar-less based bundle | sidecar test |

The first mutant survived the first draft of the two-bases test, which
never flattened the two-base chain; the test now does.

## Review round 1 (2026-10-07)

Five medium/high findings, all judged valid. Four are fixed in code; the
fifth is measured and pinned, and its design choice is an open ruling.

1. **A cycle on a reproduced root (high).** Grouped, untracked file added
   then removed: pass 3's chained export has the first capture's bytes
   (its link's held tips are the base's commits), and `publish_prior` wrote
   `.prior` onto the root: root → link → root. `chain_links` refused
   `RECEIPT_BINDING_INVALID` at depth 0, so the item was `captured` and
   unrestorable. Fix: a name that is the link or is already in the link's
   own chain (`chain_links(link)`) gets no `.prior`. Not the simpler rule
   the review offered (never onto an existing bundle without a `.prior`):
   a pass that crashed after publishing its bundle and before its `.prior`
   leaves exactly that state, and the next pass must still write the link.
2. **A stale `.prior` on a reproduced broken head (high).** Old base lost,
   recapture as the new base's delta has the head's bytes when the head's
   link tips are all in the new base; the record again named a bundle with
   a broken chain. Fix: an export that declared no link removes a `.prior`
   whose chain is not intact under `Custody`, and syncs the corpus
   directory, before the record is written. An intact one stands. The
   earlier two-bases test passed only because it deletes a branch; the new
   test deletes none, and the claim in "Custody tests" above now holds for
   both.
3. **A sidecar-less based bundle was a clean hit (medium and high, one
   fix).** `bound_base` now answers `Lost` for a bundle with no `.prior`,
   no `.base` and declared prerequisites: the hit refuses
   `RECEIPT_BINDING_INVALID` and `chain_offer` never offers it. On main
   that state refused through a failed sidecar read (read from main's
   code, not run).
4. **Restore cost (medium): measured and pinned, not changed.** P68 now
   reads `write_bundle_stage_bytes` at each restore and asserts
   `copied + n × base ≤ staged ≤ 2 × copied` for `n` chained items
   (`copied`: the corpus bytes the apply reads). Base 1,329,889 B:

   | k | Probe-2 staged | chained | P64-3 staged | chained |
   | --: | --: | --: | --: | --: |
   | 1 | 7,440,717 | 2 | 11,160,834 | 3 |
   | 2 | 9,550,591 | 2 | 14,325,718 | 3 |
   | 9 (re-base) | 10,787,796 | 0 | 15,516,593 | 0 |
   | 10 | 26,338,822 | 2 | 39,507,859 | 3 |
   | 12 | 30,557,853 | 2 | 45,836,366 | 3 |

   At `k = 1` the corpus bytes with the base read once are 2.40 MB
   (Probe-2) and 2.93 MB (P64-3): the stage is about 3.1× and 3.8× that,
   and the factor grows with the group. `item_space` charges every chained
   item its base, so a destination that fit under v1 can refuse
   `DESTINATION_SPACE_INSUFFICIENT`. `apply_item`, `item_space` and
   `flatten` are unchanged: importing each base once per destination
   repository means the flat bundle is no longer self-contained, which
   changes the restore verbs' contract (R-N72 D2 says "restores from the
   one flattened bundle") and the standalone-destination case. The
   staging footprint is #147; the choice needs a ruling (Open).

The low findings were not fixed, by instruction: the head-versus-root
`.base` asymmetry in `chain_links` against `import_base`; the thin
verb-level coverage of `bound_base`'s chained branch and the missing
join/leave/re-key tests; P68 never restoring the depth-8 chain; the red
commit's `named()` decoder; and the half-checked shape of the re-base
capture.

## Validation

- `just tla-render --check`: all 68 files current.
- `GitCarryCore.hs rows --check`: 363 rows current; the TSV is untouched.
- `just tla-check MC_gc_base_missing_untyped`: self-test INCONCLUSIVE as
  expected, the row PASS at 137 distinct states, 31 code symbols grounded,
  5 pending (L7, L8). The other GitCarry rows were not run again (their
  constants did not change); `just formal-nv` was not run.
- `just check-fast` (CI toolchain, detached and polled, OI-1003-Q85), over
  the tree of `0ec447f` plus the docs of the note's commit, 02:01Z to
  02:10Z on 2026-10-07: **exit 0**. 32 test results, 778 passed, 0 failed,
  13 ignored: `git_grouped_chain` 2, `git_group_minimality` 18,
  `git_capture_counters` 1, the fault harness, the power-loss proofs,
  repo-manifest PASS and the CI contract's 24 tests OK. This note was added
  to the tree after that run; nothing else changed. The test binaries ran with `TMPDIR` on
  `/dev/shm`: the estate verbs refuse `DESTINATION_SPACE_INSUFFICIENT`
  under their 25% floor, and `/srv/scratch` (22.7% free), `/srv/cache/jess`
  (13.4%) and `/srv/fast-local/jess` (4.9%) were all under it when the
  lane started. No code or test was changed for this.

- **Review round 1.** `just check-fast` (CI toolchain, detached and
  polled, OI-1003-Q85) over the tree of `3a36f82` plus the round's
  docs, 02:46Z to 02:54Z on 2026-10-07: **exit 0**, 32 test results, 0
  failed; `l6b_grouped_chain` 12 tests, `git_grouped_chain` 3. Test
  binaries ran with `TMPDIR` on `/dev/shm`, as before. After that run only
  this note changed (the fix commit's sha was filled in). The three review
  mutants ran in a scratch copy under
  `/srv/cache/jess/q42-l6b-grouped-chain-mut` with its own target
  directory, both since removed; each failed the test the table names and
  no other. `just tla-check` and `formal-nv` were not run again: no
  formal file, catalogue symbol or `decide` row changed. `origin/main`
  was still `34e945e`.

## Recheck and ship (2026-10-07)

The ship stage re-read the round's diff (`089e071..98621bd`) against each
medium and high finding and ran its own checks. Verdict: clean.

- **Cycle on a reproduced root (high): fixed.** A cycle needs the name to
  be the link or in the link's chain; `publish_prior` now tests exactly
  that. Scratch runs beyond the lane's tests: A → B → C → A (the root three
  captures back) stands as the root; a capture that reproduces a root from
  before a re-base (not in the link's chain, no `.prior`) is chained on the
  link at the link's depth and restores, and seventeen further passes over
  it (a second re-base among them) each keep an intact chain under both
  bindings and restore.
- **Stale `.prior` on a reproduced broken head (high): fixed.** Also with
  the head two links deep and bound to both bases: after the old base is
  lost the recapture has no stale link, hits on the next pass, extends on
  the one after, and restores each time.
- **Sidecar-less based bundle (medium and high): fixed.** `Lost`, so the
  hit refuses and `chain_offer` drops it, as main refused.
- **Restore cost (medium): deferred, soundly.** The per-item flatten is the
  ratified restore contract (OI-1003-Q63 D2), so changing it is a ruling,
  not this lane's choice. The cost is now a pinned number in P68 and a
  line in `design.md`. The round cited #148 for it; that is the side-door
  verbs issue. The apply-side flatten footprint is #147, which now carries
  the numbers and the question; the citation is corrected in `c9ea1ce`.
- **Mutants, re-run independently** in a scratch copy under
  `/srv/cache/jess/q42-l6b-grouped-chain-mut` with its own target
  directory (both since removed): `publish_prior` without the
  chain-membership guard, without the stale-link removal, and `bound_base`
  without the `Lost` arm each fail the test the table above names.
- **Merge.** `origin/main` moved to `8083675` (#194). One conflict, in the
  property plan's table: main's P66 row, this lane's P67 and P68 rows. This
  lane has no proptest, so the new seed guard has nothing to check here.
- **`just check-fast`** (CI toolchain, detached and polled, OI-1003-Q85)
  over the merged tree `4dae1b8`, 03:10Z to 03:17Z: **exit 0**, 33 test
  results, 789 passed, 0 failed, 13 ignored; `l6b_grouped_chain` 12,
  `git_grouped_chain` 3, `prop_seed_guard` green. Test binaries ran with
  `TMPDIR` on `/dev/shm`, as before (`/srv/scratch` is under the 25%
  floor). Only this note changed after that run. A run over `98621bd`
  plus the citation fix, before the merge, was also exit 0.

Not done here: the low findings stay as listed above, and the two rulings
under Open are for the operator.

## Open

- **A ruling for item 2 above.** Should a based link whose bound base is
  lost be dropped from the offer (as built: the capture recovers as the new
  base's delta), or refuse `RECEIPT_BINDING_INVALID` on the export path too
  (the missing custody stays visible, the item is stuck until the operator
  acts)? Either way the reference's `extend` and the TLA+ model do not
  cover the case.
- **`BaseMissingTyped = FALSE` has no config now.** The row was flipped as
  the design says. A regression row for the untyped behaviour would be a
  mutation row; none was added.
- **The re-base cost** (about 4.73 MB per item at `k = 9` here) stays until
  L8 (Q62).
- **Restore cost, a ruling (#147).** Apply stages the base twice per
  chained item (the numbers are in "Review round 1"). Either accept
  `N × base` on restore, or import each bound base once per destination
  repository
  and flatten only the links, which amends the R-N72 restore contract
  (D2). P68's restore pin goes red on the day that changes, by design.
  #148 is a different issue (the side-door verbs refuse a chained
  capture); it matters more now that grouped captures chain, and is
  untouched here.
- **The space floor on sting.** `git_group_minimality`,
  `git_grouped_chain` and `git_capture_counters` refuse under the default
  floor wherever `TMPDIR` has under 25% free. Every lane's check-fast on
  this host depends on which volume its `TMPDIR` is on.
- **Merge order.** #189 touches `git_carry.rs` at `pub mod carry_v2;` only.
  #195 changes `estate.rs` (`Receipt.refusal`, `emit`'s site argument, the
  outcome records): this branch's new tests read `row.outcome` and
  `row.reason`, and `capture_in`'s signature changed, so whichever lands
  second merges main and re-runs check-fast.
- Issues #149, #181 and #183 carry a comment each with what landed; none
  was closed. #183 item 1 (a missing destination repository) is not fixed.
