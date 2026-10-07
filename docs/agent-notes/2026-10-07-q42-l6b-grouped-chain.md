# 2026-10-07 — Q42 lane L6b: grouped items chain under their plan base (P68)

Lane `q42-l6b-grouped-chain`, branch `feat/q42-l6b-grouped-chain-20261007`,
worktree `bulkload.worktrees/q42-l6b-grouped-chain-20261007`, from
`origin/main` `34e945e`. Pushed, no PR opened (the coordinator opens it).

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
| the commit that carries this note | `design.md`, the formal catalogue and its rendering, the property plan, this note |

All three are signed. No force-push, no rebase; `origin/main` had not moved
from `34e945e` when the branch was pushed.

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
  - `bound_base`: a missing `.base` is `PrevBase::None`; a chained bundle
    combines its own base with every link's.
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
   sidecar. The bundle is the link.
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

The first mutant survived the first draft of the two-bases test, which
never flattened the two-base chain; the test now does.

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
- **#148** (apply fetches the base for every chained item) matters more
  now: most grouped captures are chained, and each flatten stages and
  fetches the base. Not touched here.
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
