# 2026-10-04 — Q42 lane L2: P66 ESTIMATE-DAG (lane q42-l2-estimate-dag)

Lane: Q42 lane L2, host sting. Worktree
`bulkload.worktrees/q42-l2-estimate-dag-20261004`, branch
`feat/q42-l2-estimate-dag-20261004`, cut from origin/main 8dc26c1. The branch
was pushed and no PR was opened, as the dispatch asked.

Rulings cited:
- OI-1003-Q42 and OI-1003-Q44: the lane's dispatch. P46 must survive the
  carry_v2 deletion (WP2 PR 3) as the property "estimate == upload-pack
  oracle over random DAGs". The ruling text is not recorded in this repo
  (see Open).
- OI-1003-Q7: local property tests with a fixed seed and a bounded CI corpus
  (the plan's conventions).
- R-N74, R-N97, R-N113, R-N116: the gate metric, the exactness oracle and the
  ancestors-first have order the property checks.
- R-N98: fixture repositories run with hooks off, and only those the test
  creates and removes.
- R-N13: receipts cite rulings; this note.

## What was done

- **2b4b94d** `test(git-carry): P66 ESTIMATE-DAG, estimate == upload-pack over random DAGs`
  - New `crates/bulkload-agent/tests/git_estimate_dag.rs`.
    - It imports only `bulkload_agent::git_carry::estimate::{estimate,
      Destination, Tally, ThinPack}` and `proptest`. Nothing from carry_v2,
      and nothing WP2 PR 3 deletes.
    - It derives the oracle request from the two repositories with plain
      `git`, not from v2's `first_round`. The wants are the source's tips
      the destination does not hold. The haves are every destination tip
      the source holds, ancestors first (`rev-list --topo-order --reverse`,
      with non-commit tips last).
    - The oracle is `upload-pack --stateless-rpc`, protocol v2, under the
      estimate's pack pins plus `pack.useSparse=false` and
      `pack.useBitmaps=false`.
  - `check()` asserts, on every DAG:
    - `thin_pack == oracle`, in header count **and** bytes. A zero-object
      oracle pack counts as the estimate's "no pack". Measured: upload-pack
      streams 32 bytes when every want is reachable from a have, and nothing
      when there is no want.
    - The oracle pack indexes on its own against the destination
      (`index-pack --fix-thin` into a throwaway object directory).
    - `missing ==` the per-type count and `%(objectsize:disk)` tally of
      exactly the oids the oracle pack carries.
    - `source ==` the same tally over the closure of the source's tips.
    - `haves_used`, `destination_tip_count` and the shallow, partial and
      stash fields match.
  - The generator is `dag()` from `tests/git_carry_v2.rs` (L4072–4108 at
    8dc26c1) without its `small_cap` dimension: the estimate has no segment
    cap. `build()` is the same fast-import builder.
  - PINNED rows (`pinned_dags_equal_upload_pack`), as of round 1. Round 2
    (below) adds two rows and makes each row require its deltas:
    - parent-held;
    - want-behind-a-have, where the oracle sends an empty 32-byte pack;
    - all-held, where there is no want;
    - nothing-held-stranger;
    - merge-over-held-branches.
  - Corpus. In CI: 12 cases at the fixed seed `0x0B01_C0AD_2026_1003`.
    Under `BULKLOAD_PROPTEST_DEEP=1`: 240 cases with random seeds.
    `test_support::prop_config` is `#[cfg(test)] pub(crate)`, which an
    integration test cannot reach. The file therefore mirrors it (same seed,
    same switch, same ×20), as `tests/refusal_taxonomy.rs` already does. No
    library API or visibility changed.
  - `docs/plans/2026-10-03-property-test-plan.md`: the P66 ESTIMATE-DAG row,
    in the git_carry table after P59.

## Evidence

- Round 1 numbers, superseded by round 2's content model. CI tier, in the
  worktree (`cargo test -p bulkload-agent --test git_estimate_dag`): 2
  passed in 3.7–5.2 s. Of the 12 fixed-seed cases, 8 have a non-empty pack.
  Estimate bytes equal oracle bytes in every case, for example 834/834 on
  parent-held (3 whole objects: the row was not thin, review finding 1) and
  7159/7159 on a 15-object random case.
- Deep tier, run twice (`BULKLOAD_PROPTEST_DEEP=1`): 240/240 passed in
  136.5 s and 240/240 in 117.7 s. 170 and 175 of the cases had a non-empty
  pack, and the largest had 26 objects. Bytes matched exactly in all 480
  cases. Thread-dependent delta search (`pack.threads=2`) has not caused a
  byte flake at this DAG size.
- Survives the deletion. Scratch copies under
  `$TMPDIR/q42-l2-estimate-dag/`, not in the branch:
  - (a) Only `tests/git_carry_v2.rs` removed: 2 passed.
  - (b) The whole carry_v2 module removed as well (`src/git_carry/carry_v2*`,
    `pub mod carry_v2;`, `tests/fault_harness/git_ingest.rs`): 2 passed.
    The library then warns about two items that PR 3 must clean up:
    - the `pub(in crate::git_carry) use stderr_store::{create_private,
      cstring, open_existing, private_file, private_subdirectory,
      PrivateState}` at `estimate.rs:98-100`;
    - `io/sys_linux.rs:135` `rename_noreplace_at`.

    Both fail `-D warnings` once v2 is gone.
- Checks: `cargo fmt --check` and `cargo clippy --workspace --all-targets
  -- -D warnings` are clean. `just check-fast` passed (exit 0) through the
  flock, on 2b4b94d plus this note:
  - 27 cargo `test result` lines, none with a failure;
  - `git_estimate_dag`: 2 passed in 3.95 s;
  - v2's `git_carry_v2` passed too, so P46 is still green on main;
  - fault_harness and power_loss passed;
  - contract tests: 22 OK.

  The only later change is this check-fast entry in the note.

## For lane L7: v2's object-list helper and preferred-base feeding

Paths and line ranges at origin/main 8dc26c1. **Not ported** (the
dispatch): unused code fails `-D warnings`.

- **`--missing=print` object list:**
  `crates/bulkload-agent/src/git_carry/carry_v2/plan.rs`.
  - L320–321: the `Listed` type, (edge lines, object lines).
  - L323–390: `object_list()`.
    - It runs `rev-list --objects-edge --missing=print --stdin`, with
      `--objects-edge-aggressive` for a shallow destination, over
      `wants --not haves`.
    - A `?<oid>` line refuses `source_lacks_reachable_objects`.
    - `-<oid>` lines are collected as edges and cleared when there is no
      object.
  - L310–318: `object_line()`, the shape check for `<oid>[ <path>]`.
  - L392–456: `sizes()` (`cat-file --batch-check` type and
    `%(objectsize:disk)`).
  - L474–517: `cut()`, segments in (type, basename, path) order.
  - Caller: `PackPlan::build`, L34–65 (the `object_list` call is L54).
- **Preferred-base feeding:**
  - `plan.rs` L155–165, `PackPlan::segment_input()`: every `-<oid>` edge
    line, then the segment's object lines, as `pack-objects` stdin.
  - `crates/bulkload-agent/src/git_carry/carry_v2/send.rs` L40–92,
    `send_segment()`.
    - L71–77: `pack-objects --stdout --delta-base-offset -q` in list mode,
      with no `--thin`; the edge lines make the pack thin (spike Q1
      caveat 1).
    - L78–84: the header count must equal the segment's lines.
  - L94–132: `stream()`.
- **Shared plumbing they rely on**, in
  `crates/bulkload-agent/src/git_carry/carry_v2.rs`:
  - L18–35: the module docs on the object list and segments;
  - L88–91: `PACK_PINS`;
  - L245–257: `pinned()`, which is `estimate::hardened` plus
    `pack.useSparse=false` and `pack.useBitmaps=false`, with
    `GIT_QUARANTINE_PATH` stripped;
  - L259–328: `run_child()`;
  - L330–335: `is_oid()`;
  - L337–358: `lines()`.
- **Have order (R-N116)**, in `carry_v2/negotiate.rs`: L99–132,
  `first_round()`, and L134–199, `ancestors_first()`.

## Open

- OI-1003-Q42 and OI-1003-Q44: their text is not in this repo (`docs/slo.md`
  stops before them). This note cites them as the dispatch gave them.
- P64 and P65 exist neither on main nor on any remote branch. P66 was
  numbered as the dispatch said; the two gaps are presumably reserved by
  other Q42 lanes (unverified).
- P66 covers only P46's estimate leg, over P46's generator plus round 2's
  derived edits, subtrees and renames. P46's extra dimensions are still open
  in the plan: shallow frontiers, annotated tags and tag-of-tag, non-commit
  tips, gitlinks and raw-byte paths. After round 2, 2 of the 12 CI cases
  have an empty pack, because the wants are reachable from the haves.
- The P46 row and retire-list row B still describe strengthening v2's
  `check()`. After PR 3 they should point at P66. That is left for the PR 3
  lane, to avoid a conflicting plan edit here.
- Distilled facts are not yet on the owning Linear issue: this lane had no
  issue ID. The Linear SSOT ledger is not reconciled.

## Live workstreams (as seen from this lane, 2026-10-04)

Reported, not verified, from `git worktree list` and the open PRs:
- open PRs #173 (s3-estate), #172 (source-odb-freshen), #171 (slo-q37-q40),
  #164 (whitepaper) and #136 (ingest-token);
- worktrees for `q42-l1-thin-base`, `r23-under-load`, `s2-budget`,
  `fix-store-root-seal` and the wp0e, wp1, wp2, wp3 and wp10 lanes.

The owners and states of those lanes are unknown from here. The coordinator
note holds the ledger.

## Round 2: review fixes (2026-10-04)

The dispatch: fix the review's two medium findings with signed commits,
re-run check-fast in the foreground, and push. The low findings are left as
they are; they are listed under "Open after round 2". Rulings cited:
OI-1003-Q42, OI-1003-Q44 and R-N13; OI-1003-Q7 for the corpus; R-N98 for
fixture hooks.

- **c8aa4a6** `test(git-carry): P66 makes thin and in-pack deltas observable`
  - Finding 1. The parent-held row was not thin, and one tree delta in one
    random case was all the thin coverage CI had.
    - `carried()` now reads each carried object's type from its header in
      the received bytes. A `REF_DELTA` counts as thin when `--fix-thin`
      appended its base past the received bytes. `check()` returns this
      `Encoding` and prints it on the `p66` line.
    - Each PINNED row now states the minimum number of thin and in-pack
      deltas its oracle pack must hold. parent-held requires a thin delta.
      Its comment is corrected: 4 objects, 376 B. The commit, root tree and
      tree `a` are whole, and the rewritten `a/f1.txt` is a `REF_DELTA` on
      the held version.
    - Cross-check, in a scratch copy: on every non-empty pack of the CI
      tier, `git verify-pack -v` on the `--fix-thin` index gives the same
      in-pack and thin delta counts as the header parse.
  - Finding 2. Every edit wrote fresh random content into four flat files,
    so there were no deltas, subtrees or renames.
    - Content model. A file's first write is a fresh body of 48–99 lines
      (over 1 KB). Every later write rewrites 1–3 lines of the file's
      current version, and an odd seed also appends a line.
    - The files are `f0.txt`, `a/f1.txt`, `a/sub/f2.txt` and `doc/f3.txt`.
      `Edit::Rename` (1 edit in 6) moves `a/` to `b/` and back.
    - fast-import writes each commit's whole tree: `deleteall`, then every
      file inline.
    - Two new PINNED rows:
      - rewrites-over-a-have holds both thin and in-pack deltas.
      - directory-rename-over-a-have moves an unchanged subtree the have
        holds to a path the have lacks. This is the `pack.useSparse` shape.
    - nothing-held-stranger now requires an in-pack `OFS_DELTA`, and
      merge-over-held-branches requires a thin delta.
  - The P66 plan row now records:
    - the content model;
    - the 7 PINNED rows and what each requires;
    - the mutants the tier catches;
    - that an in-crate `estimate.rs` fixture retires against P66 only once
      a PINNED row reproduces its shape and every one of its assertions.

### Round 2 evidence

The CI toolchain is `nix develop .#default` (cargo 1.96.1, git 2.54.0).
Runs were repeated with cargo 1.93 and git 2.52 from `PATH`, and the
PINNED bytes were the same on both.

- CI tier: 2 passed in about 3.8 s.
  - Of the 12 fixed-seed cases, 10 have a non-empty pack (round 1: 8).
  - 5 cases hold thin deltas (8 in all), and 4 hold in-pack `OFS_DELTA`s
    (16 in all).
  - PINNED rows (objects/bytes and the deltas each holds):
    - parent-held: 4/376, 1 thin;
    - nothing-held-stranger: 10/2034, 1 OFS;
    - merge-over-held-branches: 9/1408, 2 thin;
    - rewrites-over-a-have: 12/1004, 2 thin and 1 OFS;
    - directory-rename-over-a-have: 4/1305;
    - want-behind-a-have and all-held: empty, as before.
- Deep tier (`BULKLOAD_PROPTEST_DEEP=1`): 3 runs of 240 cases, two on git
  2.52 and one on git 2.54.
  - All passed with exact bytes, in 67.8–100.4 s.
  - Per run, 170–179 cases had a non-empty pack, 89–94 a thin delta and
    51–60 an in-pack delta.
  - No in-pack `REF_DELTA` appeared. The largest pack held 41 objects.
- Mutants of `estimate::thin_pack`, run on the CI tier with the CI
  toolchain in a scratch copy under `$TMPDIR/q42-l2-estimate-dag-fix/`.
  The copy is not in the branch and was deleted afterwards. Each mutant
  fails both tests:

  | Mutant | First PINNED row to fail | Caught in round 1 by |
  |---|---|---|
  | `--thin` removed | parent-held: 955 B vs 376 B | one random case only |
  | `--delta-base-offset` removed | nothing-held-stranger: 2052 B vs 2034 B | the deep tier only |
  | `pack.useSparse=true` | directory-rename-over-a-have: refuses `CONTRACT_SELF_INCONSISTENT` | nothing |
  | `--window=0` added | parent-held: 955 B vs 376 B | one random case only |

- `just check-fast` ran in the foreground and was waited for, through the
  flock and inside `nix develop .#default`. It ran on c8aa4a6, with this
  note not yet committed.
  - Exit 0, 9 min 50 s wall time (20:53:16Z to 21:03:06Z).
  - 27 cargo `test result` lines, none with a failure.
  - `git_estimate_dag`: 2 passed in 4.21 s. `git_carry_v2`: 65 passed.
  - fault_harness: 56 passed. power_loss: 9 passed.
  - Contract tests: 22 OK.

  The only change after the run is this check-fast entry.

### Open after round 2

- The review's low findings are not fixed, as the dispatch said:
  - The CI corpus is a fixed seed (OI-1003-Q7), and no `just` recipe runs
    the 240-case deep tier.
  - Round 1's check-fast ran as a background task.
  - The P66 "Replaces" column lists fewer missing P46 dimensions than P46
    does: bitmaps, gitlinks and raw-byte paths are left out. Round 2 added
    renames and subtrees, and a `pack.useSparse=true` mutant is now caught.
  - The mirrored `prop_config` doc says a failing deep run prints its seed.
    With `failure_persistence: None`, proptest 1.11 prints the minimal
    input instead. The `tests/refusal_taxonomy.rs` precedent uses a
    different seed and has no deep switch.
  - Four of the L7 pointers are imprecise:
    - `peel()` is not mentioned;
    - `sizes()` starts at L398;
    - the shallow flag replaces `--objects-edge`; it is not added to it;
    - the no-wants early return is not mentioned.
  - About 450 lines of fixture plumbing are copied from
    `tests/git_carry_v2.rs`.
  - Round 1's workstream report is incomplete. The round 2 list below is
    current.
- The branch is 29 commits behind origin/main (cdfe5f4). It merges
  cleanly, and main does not touch the plan or this test. Main was not
  merged in.
- No PR is open. Nothing is on a Linear issue yet, because this lane has no
  issue ID.

### Live workstreams (round 2, 2026-10-04, about 21:05Z)

Sources: `git worktree list`, `gh pr list` and the branches, read from
this lane. Owners and states beyond those are unknown here. The
coordinator note holds the ledger.

- Verified:
  - The open PRs are #164 (whitepaper) and #136 (ingest-token).
  - Q42 sibling lanes:
    - L1, `feat/q42-l1-thin-base-20261004`: at c822544, pushed, 3 commits
      ahead of main.
    - L3, `feat/q42-l3-probes-20261004`: at a479560, pushed, 2 commits
      ahead.
    - L4, `docs/q42-l4-formal-custody-20261004`: at 8bb9921, 2 commits
      ahead (it merged main), not pushed.
  - This lane, L2: c8aa4a6 plus this note, pushed. No PR.
- Worktrees whose owners and states are unknown from here:
  - coordinator-20261003, formal-hybrid, tla-model and whitepaper;
  - fix-source-odb, fix-store-root-seal, ingest-token,
    transfer-salvage-racy-finish, r23-under-load, s2-budget, s3-estate and
    slo-q37-q40;
  - wp0e, wp1, wp2 (two), wp3 (two) and wp10 (two);
  - eight detached review worktrees.
