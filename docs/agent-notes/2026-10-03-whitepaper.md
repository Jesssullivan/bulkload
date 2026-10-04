# 2026-10-03 — whitepaper and bibliography (proof package item one)

**Lane:** `whitepaper` (Lane C). Branch `docs/whitepaper-20261003`, worktree
`bulkload.worktrees/whitepaper-20261003`. PR #164 (open, not merged).
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
| `1fda837` | Finish pass, part 2: re-pin to `46587af` and describe #146's auto-prerequisite chains. |
| `7205183` | Merge of `origin/main` at `4a7b86b` (#154 merged 7 minutes after `1fda837` was pushed). |
| `e737547` | Refuter fixes to the paper: re-pin to `4a7b86b`; bounded salvage and #125 in section 2.6; OI-1003-Q36 for the `-shm` write; the code's WantManifest condition. |
| `3fdfc46` | Refuter fixes to this note (pushed with `e737547`). |
| `11a32b8` | Merge of `origin/main` at `4a10bb8` (#153 merged at 05:41Z, after `7205183`). |
| `1e481b1` | Re-pin to `4a10bb8`: section 2.1's dead_code reason, WP10 PR 2 on `main`, the durability row. |
| `dd30add` | Lanes A and B under review (#159, #160): section 4 describes `3dbfbdb` only and notes that `27581be` supersedes its run of record; sections 3.3, 4, 7 and 8 add lane A's reported Git-carry source write and its `snapshot` smoke. |
| `3e41784` | This note: the second re-pin, the relaunch pass and check-fast run 5. |
| next commit | This note: the ship pass (PR #164) and check-fast run 6. |

The branch was first cut from `57030e1`. The review-fix pass rebased it onto
`04ea9cb`. This pass merged `main` instead and did not rewrite history.

## Done

- **Paper** (`docs/whitepaper/bulkload.md`), pinned to `origin/main`
  `4a10bb8`. It covers the problem, the design, the invariants (R25,
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

Each item was meant to be checked against `origin/main` (`6268175`, then
`46587af` for what #146 touched), `docs/evidence/`, the GitHub PR list and
TIN-4543. The `-shm` item missed TIN-4543's Q36 ruling; the refuter-fix
pass below corrects it.

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
  - Sections 5.4, 6 and 7 and the status table were updated. There were
    then six `proptest!` sites (seven since #154). The durability row then
    cited #146's four passing checks.
  - `estimate::hardened` still builds through `git()`, so the
    one-hardening-table claim holds.
- **Section 4, formal model (OI-1003-Q32).** TLA+/TLC is the checker of
  record. The Dhall catalogue and the Haskell N-version explorer are stated
  as planned for sprint 2.
  - Lane B's origin head moved during this pass, from `1da332c` (a
    snapshot, with no results) through `3760263` and `3dbfbdb` to
    `d7589e0`. `d7589e0` changes only lane B's note (the branch has
    moved again since; see the relaunch pass). The first
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
  `feat/wp0e-estate-corpus-20261003` (`7b75b26`) and TIN-4543. The finish
  pass wrongly called the ruling open; see the refuter-fix pass below.
- **Open PRs named.** #150 and #151 (WP3) are marked open and not on
  `main`. #153 (WP10 PR 2) was too, until it merged (second re-pin below).
- **Bibliography re-resolution:**
  - [TLC99] gains LNCS 1703 and its editors, from Lamport's publications
    page and Crossref's book record. Crossref, OpenAlex and Springer gave
    no volume number.
  - [FastCDC20]: `fastcdc` 3.2.1's `v2020` module names IEEE Xplore
    document 9055082, the DOI's primary resource.
  - [AppleDiskWrites] and [RacyGit]: re-fetched, and their bodies were read
    again.
  - [Proptest]: the pin is stated as declared `1` and locked at 1.11.0.

## Refuter-fix pass (2026-10-04)

The refuter review of `1fda837` raised four medium or high findings, two
of them on the same `-shm` error. All were valid. Each was checked against
the code at `4a7b86b`, TIN-4543, GitHub and the coordinator branch before
it was fixed. This pass ran as the lane's only writer, per its task text.

- **OI-1003-Q36 was already ruled.** TIN-4543's comment of
  2026-10-04T03:47Z (headed about 23:55 EDT on 10-03) extends the Q16
  exception to the backup read's `<db>-shm` creation or touch. The
  conditions: the effect is counted and recorded in S2 evidence, the main
  file and `-wal` stay byte-identical, and a property test asserts no
  other source write. The `docs/slo.md` text is `8dd4546` on
  `docs/coordinator-20261003`; the implementation is #157 (open). Both came
  before `6ab4ce0` and `1fda837`, so the finish pass's claim that every
  item was checked against TIN-4543 was wrong for this one.
  - Sections 2.8, 3.3, 4 (model list and status table), 7 and 8 now give
    the ruling and its conditions, say that #157 is unbuilt, and say that
    the model at `3dbfbdb` has no `-shm` write (its README leaves it to
    P34). Section 3.3's statement now lists two SQLite exceptions.
- **Re-pinned to `4a7b86b`.** #154 merged at 2026-10-04T05:18Z, 7 minutes
  after `1fda837` was pushed. Merged as `7205183`.
  - Section 2.6 describes bounded salvage as on `main`: only temporaries
    staged by a byte-touching refusal are kept, capped at 1024 files and
    4 GiB (`transfer.rs`), past which `SALVAGE_BOUND_EXCEEDED` is refused.
  - It adds #125's legacy-row pass (`racy_guard`,
    `transfer_legacy_rows_invalidated`; OI-1003-Q26).
  - `proptest!` now appears in seven places (the salvage-bound property is a
    second block in `transfer/tests.rs`).
  - Section 3.1 cites `docs/design.md`'s new "Known limit: clocks".
  - Section 7 and the status table were updated. The durability row cites
    #154's four passing checks (and, since the second re-pin, #153's).
- **WantManifest.** Section 2.2 now gives the code's condition
  (`Inbound::entry`): an existing output, any output hint in the store
  (`has_output_hints`, once per session), or any salvaged temporary. It
  notes that `docs/design.md` states it more narrowly.
- **For the coordinator:** `docs/design.md` on `main` still calls the
  1024 / 4 GiB salvage values "unruled engineering defaults", while the
  comment at the constants and TIN-4543 record OI-1003-Q24. This lane does
  not edit `docs/design.md`.
- **Second re-pin, to `4a10bb8`.** #153 (WP10 PR 2) merged at
  2026-10-04T05:41Z, found when this pass pushed. Merged as `11a32b8`.
  Section 2.1 now gives the io allowance's new reason (gate (a) evidence,
  #88, WP10 PR 3) in place of the "W4 PR 2/3" quote. Section 7 has WP10
  PR 2 on `main`. The durability row cites #153's checks and its
  crash-check retarget at the production no-replace publish. `proptest!`
  still appears in seven places. `transfer.rs`, `provider_sqlite.rs`,
  `closure.rs`, `crash_check.rs` and `tests/` are unchanged from
  `4a7b86b`.
- That pass committed `11a32b8` and `1e481b1` but did not push them, and
  left this note's update uncommitted. The relaunch pass below found both,
  checked them and kept them.

## Relaunch pass (2026-10-04)

The coordinator relaunched this lane with the same four refuter findings,
as the only writer; its task text says the earlier copies are gone.

- **All four findings were already fixed** in `e737547` (pushed) and
  `1e481b1`. Each was re-checked before anything was pushed:
  - the WantManifest condition against `Inbound::entry` in `transfer.rs`
    (`has_output_hints` once per session; any salvaged temporary);
  - bounded salvage against `SALVAGE_KEEP_FILES`, `SALVAGE_KEEP_BYTES`,
    `salvage_to_keep` and `bound_salvage`, and #125 against `racy_guard`
    in `transfer_store.rs`;
  - seven `proptest!` sites, counted with grep at `4a10bb8`;
  - Q24, Q26 and Q36 against their TIN-4543 comments (Q36 at
    2026-10-04T03:47Z; Q24 and Q26 at 00:46Z), and #157 open on GitHub
    (created 03:47Z).
- **New drift: lanes A and B moved under review.** Fixed in `dd30add`.
  - Lane B is open as #160 at `27581be`. Its README there supersedes the
    `3dbfbdb` run of record with a 43-row run over `8bc6672`. It says the
    old `MC_wp0g` row used `MC_main`'s bound and never read the relaxed
    ledger, so it was no evidence. It also widens the WP0(g) condition to
    the store's whole creation, its state root's directory entry included,
    and notes that the model assumes a ledger commit never fails. Section
    4 now says it describes `3dbfbdb` only and records these points; it was
    not re-pinned to an unmerged, still-moving branch.
  - Lane A is open as #159 at `633ff72` (evidence at `378b634`). Its
    evidence file no longer gives the 3.51.2 and 3.53.1 `-shm` probes. It
    records a smoke at `46587af` instead: bulkload's own `snapshot` created
    `storage.db-shm`, and `estate-capture` moved the mtime and ctime of
    source loose objects and packs, which the lane reads as Git freshening
    through alternates and leaves to S2 for a ruling. TIN-4543 has no
    ruling on it. Sections 3.3, 4 (S2 row) and 8 report it as unruled;
    sections 4 and 7 name #159 and #160.
- **Workstreams (verified on GitHub and origin, 2026-10-04 about 08:40Z):**
  `main` is `4a10bb8`. Open PRs are #136 (#120 ingest token; the refuter
  reports it held by WP0(a), which GitHub does not show), #150 and #151
  (WP3), #159 (lane A) and #160 (lane B). #153
  and #154 are merged. #157 (Q36 implementation) is an open issue. The
  coordinator's `docs/slo.md` amendments are `9784467` and `8dd4546` on
  `docs/coordinator-20261003`, with no PR open. This branch has no PR.

## Ship pass (2026-10-04)

Relaunched by the coordinator as the only writer, to open the PR.

- Before opening, the worktree was clean. `HEAD` was `3e41784`, equal to
  `origin`, and 0 commits behind `main` (`4a10bb8`). All 14 branch commits
  are GPG-signed. The diff against `main` touches only the paper, the
  bibliography and this note.
- The cite and definition check was re-run at `3e41784`: 34 of 34.
- Opened #164 at 2026-10-04T09:05Z, not merged. Its body covers scope and
  structure, pending gate evidence, how the references were verified, and
  the review summary, and it lists the open low findings below.
- The refuter's low findings on the probe script, the `proptest!` seeds,
  `StreamCDC`, `KeyParts::carries` and S4's #40 were re-checked at
  `4a10bb8` and still hold. This pass did not change the paper.

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
- The finish pass was that relaunch (`lane-c-whitepaper-finish`), the only
  writer in this worktree. Its task text, from the coordinator's workflow,
  said the earlier copies were gone before it began. The 2026-10-04
  relaunch pass above was relaunched the same way.
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
| 4 | `7205183` (with #154's code) plus the refuter-fix edits | 696 passed, 0 failed |
| 5 | `dd30add` (with #153's code, which deleted stale io tests) | 682 passed, 0 failed |
| 6 | `3e41784` plus the ship pass's edit to this note | 682 passed, 0 failed |

During run 3, only documentation lines were edited: a rewrap in section
2.7 and this paragraph. During run 4, only documentation was edited: two
paraphrases and one sentence in the paper, and this note. Run 5 waited
about 15 minutes for the shared lock; during it only this note was
edited. Run 6 waited about 20 minutes for the lock; the tree was
`3e41784` plus this note's ship-pass edit, and only that edit's result
row was filled in afterwards. This lane changes documentation only.

- The draft's run, on `57030e1`, exited 0.
- The review-fix pass's run was queued when its session ended. No commit
  recorded that run's result.

## Open

- **Formal model (#160).** Section 4 describes `3dbfbdb` and says the
  README at `27581be` supersedes its run of record. When #160 merges,
  re-pin section 4 to the `main` path and sha: the 43-row run, the
  reach rows, the widened WP0(g) condition, and the README's coverage and
  "Code and design disagreements" sections. Describe the Dhall catalogue
  and the Haskell explorer as done only once they land, and add
  bibliography entries for them if they are cited. Adopting WP0(g) under
  the model's conditions has no ruling yet.
- **SQLite wal-index (OI-1003-Q36).** Ruled; implementation open as #157.
  When #157 lands, cite its counter and property test in sections 2.8,
  3.3, 4 and 8. When #159 merges, rewrite section 8's probe text to cite
  the evidence file's `main` path and its `snapshot` smoke (the 3.51.2 and
  3.53.1 probes are no longer in it). Point at `docs/slo.md` once
  `8dd4546` reaches `main`.
- **Git-carry source write (reported on #159).** `estate-capture` moved
  the mtime and ctime of source loose objects and packs in lane A's smoke.
  No ruling yet. When TIN-4543 records one, or #159 merges, update sections
  3.3 and 8 and the S2 row.
- **Gate (a) on wire v5.** There is no evidence file for the 2026-10-03
  attempts. Cite an `r23-2026-10-03-*` file once one lands.
  OI-1003-Q33 holds the next attempt for the full post-train `main`.
- **OI-1003-Q24, Q25, Q26 and Q36 are not in `docs/slo.md` on `main`.**
  Their text is on `docs/coordinator-20261003` (`9784467`, `8dd4546`). The
  paper cites them through TIN-4543. Point at `docs/slo.md` once the
  coordinator's docs PR merges.
- **Refresh on merge** of #136, #150, #151, #159 (estate corpus, WP0(e))
  or #160, or of a gated gate (a) sample on corpus v1. When WP0(a)'s S3
  measurement of the chains lands, cite it in sections 2.7 and 5.4.
- **S5 classes with no owner.** Configuration, shallow-frontier, nest
  custody and rebuildable-root movement, and directory-shape drift, still
  refuse. No ruling or work package names them.
- **Next sprint (OI-1003-Q34).** This lane's sprint 2 is the S2 budget
  instrument, using the v0 synthetic workload; then refresh section 5.3.
- **PR #164.** Under OI-1003-Q29 it merges once its R-N71 review is clean
  and CI is green.
- **Nine low refuter findings on `1fda837` are not yet fixed.** #164's body
  lists them:
  - §2.7: the estimate's probe script does not build its children through
    `git()`;
  - "last completed" R23 sample;
  - three `proptest!` sites draw a random seed;
  - verbatim runs from `docs/design.md`, and copied live PR state;
  - §6 FastCDC rate (the engine uses `StreamCDC`);
  - S4's #40 is omitted;
  - §2.10 omits the exclude file, the stash reflog and the symbolic HEAD;
  - the §4 mutation count includes three non-mutation rows;
  - `MC_main`'s edit bound is per seat.
