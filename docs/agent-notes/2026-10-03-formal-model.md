# 2026-10-03 — formal-model (lane B: proof package, formal model)

Rulings:

- OI-1003-Q7: proof package, formal model; CI stays slim.
- OI-1003-Q20: WP0(g), the relaxed source ledger, "only if proven in the
  formal model".
- OI-1003-Q23.
- OI-1003-Q32: the model is a hybrid. TLA+ with TLC is the checker of record;
  sprint 2 adds a Dhall config catalogue and a Haskell N-version explorer on
  `MC_nv_core`. The workflow coordinator relayed this ruling mid-lane.
- R-N13.
- Also cited: OI-1003-Q16 (WP0(b)), OI-1003-Q18 (WP0(c), (d)), OI-1001-Q15
  (`Held` after the commit), R-N92 and R-N101 (wording), and R-N11, R-N12 and
  R-N104 (no signals, refusals stop, no PID checks).

Branch `docs/tla-model-20261003`, worktree
`bulkload.worktrees/tla-model-20261003`. It was cut from `origin/main` 57030e1
and fast-forwarded to adb9c66 (#145). The model describes the code at
adb9c66; every cited symbol was re-checked with `git grep` at adb9c66 and at
`origin/main` 6268175. No PR yet.

Shas:

- `1da332c`: signed snapshot (one of the duplicate copies, 22:08).
- `3760263`: this session's reconcile commit (spec, generator, configs,
  runner).
- The commit that adds this note, on top of `3760263`: the TLC results, the
  README and this note.

## The duplicate-agent incident

- The workflow coordinator's `SendMessage` to this lane started resumed
  copies of the lane rather than reaching the running one. Two copies then
  wrote in this worktree at the same time.
- One copy made the signed snapshot `1da332c` from the files as they stood
  at 22:08, then rewrote the configs, the recipe and the budget. The other
  added `MC_nv_core` and found the WP0(g) authority result. At about 22:15
  the coordinator named one of them the owner and stopped the other. Both
  were gone before this session started (reported by the coordinator, not
  verified here).
- They left about 42 uncommitted changes (35 files, +450/−266) on top of
  `1da332c`, and an untracked draft note (`2026-10-03-tla-model.md`). Their
  reported numbers were unverified when this session started; see
  Validation for what was then verified.
- This session was the only writer. It read every changed file, reconciled
  them into one design, deleted the draft note (this note replaces it), and
  committed on top of `1da332c` without a force push.
- Reported by the draft note, not verified: one copy's first TLC baseline was
  started under a coreutils wall-clock wrapper that ended that copy's own JVM
  (exit 124). That is why the budget now lives inside TLC, and why this
  session ran TLC with no outside clock at all. Lane A reported a similar
  duplicate in its own worktree.

Lesson for coordinators: a `SendMessage` to a lane that may be resumed as a
new copy needs a single-writer guard (one worktree, one owner) before the
copy starts.

## Done

- **Reconciled design** (`3760263`).
  - `gen_cfgs.py` renders `configs.tsv` (columns `name`, `expect`,
    `named-property`, `flags`) and every `MC_*.cfg`, deleting stale ones.
    `MC_deep.cfg` became `MC_main_deep.cfg`.
  - Every fail config checks `TypeOK` beside its one named property, so a
    type error or a different invariant shows up as WRONG.
  - No `SYMMETRY` in a liveness config (asserted by the generator).
- **Budget.** `WithinBudget == run \in 0..MaxRuns => TLCGet("duration") <
  BudgetSeconds` is state-level. A probe spec on TLC 2.19 reproduced the
  copies' finding: the clock-only form explored 2,253,001 states in 7 s past
  a 2 s budget, and the state-level form tripped at 2 s.
  `MC_budget_selftest` (MC_main's constants, 5 s budget) is the first row of
  `configs.tsv` and came out INCONCLUSIVE in every run; the runner stops if
  it does not.
- **Bounds.** As the coordinator set them: `MC_main` two seats, two runs,
  one crash, faults off; `MC_main_deep` one seat, three runs, two crashes,
  every fault; `MC_main_sim` simulation only; `MC_nv_core` and its three
  mutations unchanged. No reduction was needed: `MC_s2`, `MC_wp0g`,
  `MC_wp0g_deep`, `MC_wp0d_exchange` and `MC_live` all finish well inside
  their 600 s budgets at the bounds the copies left (the slowest, `MC_wp0g`,
  in under 90 s).
- **Runner** (`just tla-check`, last recipe in the justfile). One JVM at a
  time, `-Xmx4g`, `-workers 3`, `nice -n 10`, `-coverage 1`, flags as
  separate argv elements, scratch in a private `mktemp -d` under `$TMPDIR`.
  Outcomes PASS, FAIL (exactly the named property), SIMULATION, INCONCLUSIVE
  (budget trip or no `Finished` line) and WRONG. The `check-optional` block
  and its comment are byte-identical to `origin/main` (lane A owns them);
  `check-full` and CI never run TLC.
- **`docs/formal/README.md`**: running it, the budget, results, coverage,
  the N-version core, the `WF_vars` liveness assumption, the WP0(g) verdict
  with its caveat, WP0(d), the abstraction map, the property ↔ SLO ↔ ruling ↔
  P-test map, mutations, the not-proven list and the frozen names.

## Validation

On host sting, TLC 2.19, one JVM at a time, `-Xmx4g`, 3 workers, `nice -n
10`, every metadir under a private `$TMPDIR/tla-owner-*` directory, no outside
clock.

- **Calibration** (the self-test, then `MC_main`): both matched.
- **Full run 1**, during which this session's comment-only spec edit landed:
  35/35 rows matched, in 345 s with a peak RSS of 1,869 MiB.
- **Full run 2, the run of record**, on the spec and configs of `3760263`:
  35/35 rows matched (9 PASS, 1 SIMULATION, 1 INCONCLUSIVE, 24 FAIL, 0 WRONG),
  in 379 s with a peak RSS of 1,901 MiB. The limits were 40 min and 4.5 GiB.
  Every positive config's distinct and generated counts and diameter equal
  run 1's.

What the copies reported, and what this session verified:

| Reported by the copies | Verified here |
|---|---|
| `MC_budget_selftest` INCONCLUSIVE at 6 s (drafted constants) | INCONCLUSIVE at 7–8 s in three runs, now on `MC_main`'s bounded constants |
| `MC_main` 869,296 distinct, depth 51, pass | Yes, in both runs (68–69 s) |
| `MC_wp0g` 878,950 distinct, pass | Yes, in both runs |
| `MC_main_sim` clean (simulation only) | Yes: SIMULATION, 1,147,363 states checked |
| `MC_nv_core` 15,834 distinct, 44,312 generated, depth 45, the same at 1 and 4 workers | 15,834 / 44,312 / 45 at 3 workers in both runs. The 1- and 4-worker runs were not repeated (a completed BFS count does not depend on workers). |
| The 3 core mutations fail as required | Yes. Each fails on exactly its named property, with `TypeOK` checked alongside. |
| Relaxed `captures` rows are safe; a relaxed authority breaks R25 in 15 states | Yes. `MC_wp0g` and `MC_wp0g_deep` pass. `MC_wp0g_authority` fails exactly `R25_NoDurableReread` with a 15-state counterexample (run 1). |

Coverage: `MC_main` never enables 12 of 37 actions and `MC_main_deep` 9.
Each one is switched off by that config's constants (faults off, no
superseding publish, no estate reads) and is enabled elsewhere (README,
Coverage).

`just check-fast` (CI toolchain, under the shared lock, cargo target under
`$TMPDIR`): queued on the shared lock when the results commit was made; its
result is recorded in the follow-up commit.

## Findings

- **WP0(g) holds only with a condition.** Losing any subset of the source
  `captures` rows keeps every property, because R25 is carried by the
  destination's committed rows (`MC_wp0g`, `MC_wp0g_deep` pass). Losing the
  store-creation commit, which holds the authority, does not: every key
  changes, and the next run re-reads durably held bytes
  (`MC_wp0g_authority` fails `R25_NoDurableReread`). So keep the creation
  commit `synchronous=FULL`, relax only `commit_captures` on the source, and
  treat a corrupt or absent source ledger as empty, never as a reason to
  mint a new authority. Subset loss over-approximates WAL `NORMAL`'s suffix
  loss; corruption and reordering are outside the model.
- **WP0(d).** Check-then-rename clobbers a concurrent third-party write. The
  exchange design (`RENAME_EXCHANGE`, check the displaced file, a restoring
  recovery) passes.
- **R25 is carried by `Reuse`, never by the source ledger.** A config that
  also needs the source row fails, even with a strict ledger.
- **WP0(c) inequality 1 needs a qualifier.** It must also cover seats the
  destination stopped holding.
- **The pre-commit audit caught a word.** The copies wrote "kill" (in the
  mutation-testing sense) in a spec comment. The repo's process-safety audit,
  the authoritative wording check (R-N92), refused the first commit attempt
  on it. The comment now says "caught mutant", and the runner and generator
  use the same word.

## Open

- An operator ruling on the WP0(g) condition: the creation commit stays
  `FULL`, or it is checkpointed and synced before `Start`. Then implement it in
  `Store::open` / `configure_sqlite`, source side only, keeping
  `checkpoint_fullfsync=ON`, with a corrupt or absent ledger read as empty.
- WP0(d) (WP5 PR 3): use the exchange shape. Decide what happens on a
  filesystem without exchange (refuse, keep no-clobber).
- Fix design.md's WantManifest condition: it omits salvaged temporaries
  (not done here; design.md is out of this lane's scope).
- P34: state whether the backup's WAL read touches the source's `-shm`.
  `S2_BackupLockBounded` has no dedicated test.
- Sprint 2 (OI-1003-Q32): the Dhall catalogue replaces `gen_cfgs.py`, and the
  Haskell explorer must reproduce `MC_nv_core`'s 15,834 distinct states and
  find the three core mutations.
- Linear: post the WP0(g) verdict on TIN-4543 and reconcile the SSOT
  ledger's proof-package row. Not done from this lane; the coordinator owns
  the ledger.
- Open the PR for this branch when the coordinator asks.
