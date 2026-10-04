# 2026-10-03 — whitepaper and bibliography (proof package item one)

**Lane:** `whitepaper` (Lane C). Branch `docs/whitepaper-20261003`, worktree
`bulkload.worktrees/whitepaper-20261003`. No PR yet.
**Rulings:** OI-1003-Q7 (proof package: whitepaper and bibliography),
OI-1003-Q23 (lane fan-out; C is the whitepaper and bibliography),
OI-1003-Q32 (the formal model is a TLA+/Dhall/Haskell hybrid), R-N13
(receipts and this note). All are recorded on Linear TIN-4543.

## Commits on the branch

| sha | what |
|---|---|
| `d6f6432` | First draft of the paper and the bibliography. |
| `e7b12fd` | Dropped the `docs/slo.md` pointer line; the coordinator's docs PR carries it. |
| `ba6e487` | Lane C review fixes (25 findings); pinned to `04ea9cb`. |
| `7e2921a` | Merge of `origin/main` at `6268175` (#152). |
| `6ab4ce0` | Finish pass, part 1 (pushed): re-pin to `6268175`, the formal-model section with `3dbfbdb`'s TLC results, bibliography re-resolution, this note. |
| `cb4469b` | Merge of `origin/main` at `46587af` (#146 merged while part 1 was being committed), so the branch diff lists only lane C files. |
| part 2 commit | Finish pass, part 2: re-pin to `46587af` and describe #146's auto-prerequisite chains. |

The branch was first cut from `57030e1`. The review-fix pass rebased it onto
`04ea9cb`. This pass merged `main` instead and did not rewrite history.

## Done

- **Paper** (`docs/whitepaper/bulkload.md`), pinned to `origin/main`
  `46587af`. It covers the problem, the design, the invariants (R25,
  durability ordering, S2), how each SLO is proven, results, related work,
  future work and limits. Section 5 quotes only `docs/evidence/`. SLO
  numbers are linked to `docs/slo.md`, not copied.
- **Bibliography** (`docs/whitepaper/bibliography.md`): 34 entries, each
  with a "verified via" note. Every cited key is defined, and every defined
  key is cited (checked by script).
- `docs/slo.md`, `docs/design.md` and `AGENTS.md` are not changed by this
  lane. `AGENTS.md` differs from `ba6e487` only through the merge of
  `main`.

## Finish pass (2026-10-03, after the duplicate-agent incident)

Each item was checked against `origin/main` (`6268175`, then `46587af`
for what #146 touched), `docs/evidence/`, the GitHub PR list and
TIN-4543.

- **S1 is stated as not met.** The 2026-09-18 R23 sample (pre-wire-v5)
  failed the initial copy at 3015.294 ms against rclone's 601.010 ms. It won
  the 1 % delta, 58.842 ms against 127.064 ms. The abstract now gives these
  exact numbers, matching `docs/evidence/r23-2026-09-18.md`.
- **Wire v5 has no gated sample.** The three 2026-10-03 gate (a) attempts
  (01:32Z, 02:01Z, 19:30Z) are cited only through TIN-4543, in section 7,
  because `docs/evidence/` has no file for them.
- **Nothing stronger than `docs/slo.md`.** The abstract, section 1.4 and
  section 3.3 now state the S3 and S2 goals as requirements or aims, not as
  facts. Section 8 says WP1 made "more", not "most", of S2 structural.
- **WP1 is on `main`** (#145, `adb9c66`). The hardening table
  (`git_carry::git_env`, eleven cleared variables) and background priority
  for six verbs were checked in the code at `6268175`, and the table again
  at `46587af`. No text says they are off `main`.
- **#152 (WP10 PR 1) folded in.** Four crates, with `bulkload-handoff`
  described. Adds `drain_bounded`'s proptests in `src/child.rs`.
- **#146 (WP2 PR 2) folded in.** It merged as `46587af` during this pass.
  - Section 2.7 now describes auto-prerequisite chains: source-held tips
    only, the `.prior` sidecar, depth cap 8 with re-base, and a verified,
    flattened restore. It notes the side doors (#148).
  - Its evidence is fixtures only: the counters test, which writes less
    than an eighth of the 256 KiB history, and P-CHAIN. No estate run has
    measured it.
  - Sections 5.4, 6 and 7 and the status table were updated. There are
    now six `proptest!` sites. The durability row cites #146's four
    passing checks.
  - `estimate::hardened` still builds through `git()`, so the
    one-hardening-table claim holds.
- **Section 4, formal model (OI-1003-Q32).** TLA+/TLC is the checker of
  record. The Dhall catalogue and the Haskell N-version explorer are stated
  as planned for sprint 2.
  - Lane B's origin head moved during this pass, from `1da332c` (a
    snapshot, with no results) through `3760263` and `3dbfbdb` to
    `d7589e0`. `d7589e0` changes only lane B's note. The first
    draft of this pass said "pending". After a re-check of origin before
    committing, section 4 instead reports the results committed in
    `docs/formal/README.md` at `3dbfbdb`.
  - Those results: the run of record over `3760263`'s spec, which is
    unchanged at `3dbfbdb`. All 35 rows matched their expectation:
    - 9 PASS, 24 FAIL each on its named property, 1 SIMULATION and the
      INCONCLUSIVE budget self-test;
    - `MC_main` has 869,296 distinct states and `MC_nv_core` 15,834;
    - the WP0(g) verdict is conditional, and WP0(d)'s exchange passes
      while check-then-rename fails.

    The paper states the small bounds, and that the branch is not on
    `main`.
  - The paper names only invariants defined in `BulkloadTransfer.tla` at
    `3dbfbdb`. All 20 names were checked, and they match the README's
    frozen list.
  - The paper says the model cannot prove code-level S2, does not cover
    the Git carry, and does not model S3's Git half.
- **Lane A's SQLite `-shm` finding.** It is reported in sections 2.8, 3.3
  and 8, and in the status table. The source is lane A's evidence file on
  `feat/wp0e-estate-corpus-20261003` (`7b75b26`) and TIN-4543. A ruling is
  open.
- **Open PRs named.** #150 and #151 (WP3), #153 (WP10 PR 2) and #154
  (salvage) are marked open and not on `main`.
- **Bibliography re-resolution:**
  - [TLC99] gains LNCS 1703 and its editors, from Lamport's publications
    page and Crossref's book record. Crossref, OpenAlex and Springer gave
    no volume number.
  - [FastCDC20]: `fastcdc` 3.2.1's `v2020` module names IEEE Xplore
    document 9055082, the DOI's primary resource.
  - [AppleDiskWrites] and [RacyGit]: re-fetched, and their bodies were read
    again.
  - [Proptest]: the pin is stated as declared `1` and locked at 1.11.0.

## Duplicate-agent incident (recorded on TIN-4543)

- About 22:05 EDT on 2026-10-03, the coordinator used SendMessage to send
  redirects to the running lane workflow agents.
- The first send to each agent started a resumed copy of it, instead of
  reaching the live agent. Lanes A, B and C then each had two agents
  writing one worktree.
- In this lane, the workflow's fix agent and the resumed copy both edited
  this worktree during the review-fix pass. By coordinator ruling, the fix
  agent owned the lane. The earlier version of this note records that the
  edits were reviewed and kept where correct, for example Borg's
  `LocalCache.commit`.
- At 22:23 the session reached its rate limit and every lane agent ended.
- The coordinator's rule since then: running workflow agents are never
  messaged, and a redirect relaunches the lane as a fresh workflow with one
  owner.
- This pass is that relaunch (`lane-c-whitepaper-finish`), the only writer
  in this worktree. Its task text, from the coordinator's workflow, says
  the earlier copies were gone before it began.
- Sibling agents share the session scratchpad, so this pass kept its
  scratch files in its own subdirectory.

## Validation

`flock bulkload.worktrees/.check-fast.lock nice -n 10 nix develop .#default
--command just check-fast`, with
`CARGO_TARGET_DIR=/srv/fast-local/jess/cargo-target/whitepaper`. Every run
exited 0, and the Python contract suite ran 22 tests, OK, each time.

| run | tree | `cargo test` result lines |
|---|---|---|
| 1 | `7e2921a` plus part 1's edits, before the `3dbfbdb` update | 683 passed, 0 failed |
| 2 | `7e2921a` plus part 1's final edits (committed as `6ab4ce0`) | 683 passed, 0 failed |
| 3 | `cb4469b` (with #146's code) plus part 2's edits | 690 passed, 0 failed |

During run 3, only documentation lines were edited: a rewrap in section
2.7 and this paragraph. This lane changes documentation only.

- The draft's run, on `57030e1`, exited 0.
- The review-fix pass's run was queued when its session ended. No commit
  recorded that run's result.

## Open

- **Formal model.** Section 4 cites `3dbfbdb`, with the head at
  `d7589e0`. If lane B's branch moves
  or merges, re-pin to the `main` path and its sha. Describe the Dhall
  catalogue and the Haskell explorer as done only once they land, and add
  bibliography entries for them if they are cited. Adopting WP0(g) under
  the model's conditions has no ruling yet.
- **SQLite `-shm` ruling** (S2, OI-1003-Q16). Rewrite sections 2.8, 3.3
  and 8 when it is ruled. Cite lane A's evidence file by its `main` path
  once it merges.
- **Gate (a) on wire v5.** There is no evidence file for the 2026-10-03
  attempts. Cite an `r23-2026-10-03-*` file once one lands.
  OI-1003-Q33 holds the next attempt for the full post-train `main`.
- **OI-1003-Q24 and Q25 are not in `docs/slo.md`.** The paper cites them
  through TIN-4543. Point at `docs/slo.md` once the coordinator's docs PR
  merges.
- **Refresh on merge** of #150, #151, #153 or #154, of the estate corpus
  (WP0(e)), or of a gated gate (a) sample on corpus v1. When WP0(a)'s S3
  measurement of the chains lands, cite it in sections 2.7 and 5.4.
- **S5 classes with no owner.** Configuration, shallow-frontier, nest
  custody and rebuildable-root movement, and directory-shape drift, still
  refuse. No ruling or work package names them.
- **Next sprint (OI-1003-Q34).** This lane's sprint 2 is the S2 budget
  instrument, using the v0 synthetic workload; then refresh section 5.3.
- **PR.** None is open for this branch. Under OI-1003-Q29 it merges once
  its R-N71 review is clean and CI is green.
