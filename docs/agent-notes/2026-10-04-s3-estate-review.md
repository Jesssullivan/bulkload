# 2026-10-04 S3 estate lane: review steps (main merge, then review-round fixes)

Rulings: OI-1003-Q15, OI-1003-Q18, OI-1003-Q35, OI-1003-Q38 (cited by the
dispatch; its text is in no repo branch, and not on main `cd4ffad`
either), R-N13.

- **Branch:** `feat/s3-estate-20261004`.
- **Worktree:** `bulkload.worktrees/s3-estate-20261004`. Each session below
  was its only writer.
- **PR:** none opened (by dispatch).
- **Earlier note:** [2026-10-04-s3-estate.md](2026-10-04-s3-estate.md)
  covers the harness, the runs and the Q15 packet.

## Step 1: merge main (earlier session)

- Merged origin/main `dfb9604` (#159, #160, #150, #151) as signed merge
  `2136c72`, with no conflicts.
- Recorded in `b5a583c` that the numbers come from a build of main
  `4a10bb8` and were not re-measured on `dfb9604`.
- check-fast exited 0 on that tree (11:00:07Z to 11:24:51Z). It ran as a
  background task, which the later review flagged; see Open.

## Step 2: review-round fixes (this session)

### Starting state

- HEAD was `b5a583c`, equal to `origin/feat/s3-estate-20261004`.
- The worktree held uncommitted harness changes from an earlier attempt at
  this review round in this lane. Its scratch is
  `$TMPDIR/s3-estate-fix-6eYBLn` (07:39 to 07:51 local), with a re-run of
  the evaluation in `$TMPDIR/s3-estate-fix2-oddLbt`. Neither committed.
  This session reviewed those changes line by line and adopted them. It
  then ran the tests and ruff, and re-evaluated the recorded runs into its
  own scratch.

### Changes since the branch last moved

- origin/main moved from `dfb9604` to `cd4ffad`: #166 (store root seal),
  #167 (SLO amendments OI-1003-Q24..Q26, Q34, Q36; coordinator note) and
  #168 (S2 budget instrument).
- None of them touches the harness, `shared.rs`, `chain.rs` or the
  `estate.rs` chain code that the findings cite. #166's change to
  `estate.rs` is confined to `private_directory`, where it adds a seal of
  the parent directory.
- The branch was not merged with `cd4ffad`, since this round did not need
  it. The branch still carries `dfb9604`.
- New worktrees exist beside this lane: `fix-source-odb-20261004` (likely
  evidence finding 10) and `formal-hybrid-20261004`. They were not
  inspected and not written.

### What was verified before fixing

- **Code at `4a10bb8` (and unchanged at HEAD):**
  - `CHAIN_DEPTH_LIMIT = 8` (`chain.rs`);
  - `chain_offer`'s `link.filter(|_| unbased)`, and `prepare_base` reusing
    `shared-{group}.base` (`estate.rs`);
  - `write_with_prerequisites` using plain `bundle create --all --stdin`,
    against `write_excluding_tip_trees` using
    `rev-list --objects-edge-aggressive` (`shared.rs`);
  - `raw_tree.rs`'s "Metadata censuses, symlink reads and Git repacking
    are separate";
  - `counters.rs`'s "every object in a capture pack was read from an
    object store".
- **The review's probes.** Their recorded outputs were re-read, not
  re-run:
  - the chained sequence: 4,197,421 B, then 526,413 to 526,727 B, then
    8,919,284 B (no `.prior`), then 526,818 B;
  - the grouped sequence: 526,696 B up to 5,249,800 B per item;
  - the exclusion pair: 4,300,866 B against 3,507 B.
- **The estate run's shared bases.** A pack scan of the two 68 MB bases
  found blobs `f34c244b…` and `c8fbd764…`, the 64 MiB blobs that the eight
  `r00` and `r12` item bundles carry again.
- **The recorded runs.** `run-estate-1` and `run-small-2` still hash as the
  evidence doc says (`1b0ed6f10e25…`, `ce1601940a1b…`).

### Fixes, by review finding (all medium and high findings accepted)

| Finding | Fix | Where |
|---|---|---|
| File inequality 1 counted SQLite seats (high) | Each seat counts in one half. Estate and small `mutate-10` now read "fail (sniff only)", +176 B and +64 B. Unit test fixed. | harness `delta_bounds`; evidence Delta table, findings 6 and 7; both notes |
| Git inequality 1 is worktree-only (medium) | Finding 2 restated. Object-store reads reported beside it (readback, pack bytes, changed object-store seats). Ruling asked. | harness `git_ineq1.object_store`; evidence Bounds, finding 2, a new table; packet row label and Q18 question 2 |
| CPU ratio never judged (medium) | `cpu_ratio_le_10pct` verdict added. Estate passes (6.9 % to 8.7 %); small fails (15.1 % to 16.2 %), because the first pass is a small denominator. Wall ratio kept as informational. | harness `evaluate`; evidence Verdicts |
| File inequality 2 passed only because seats were refused (medium) | `n/a: blocked by WP0(d)` when a changed seat was refused, with the carried-only bound beside it | harness; evidence Delta table, findings 7 and 8 |
| Git granularity and per-item double count (medium) | New objects counted once per repository. Bound reported at chunk and object granularity. Ruling asked. | harness `git_wire`, `per_item_pack`; evidence per-repository table; packet Q18 question 1 |
| Re-base cost ignored (high) | Amortised re-base row (about 19.9 M and 20.1 M), Reasons, follow-up 3, and the "would change it" list | packet; evidence "Probes after the run" |
| Grouped items never chain (high) | Stated with the probe. Follow-up 2 has a target over several rounds. | packet; evidence probes |
| Excess blamed on the snapshot layer (medium) | Findings 3 and 4 re-attributed to the group-base exclusion. Old follow-ups 1 and 2 merged. The hybrid row and the shares were recomputed or labelled "unfixed". | evidence findings 3 and 4, v2 projection; packet |
| Flat metadata allowance (medium) | Reframed as a formula or a deferral, and decoupled from Q15. Item 2 renamed (not the #48 case), and "constant" dropped. | packet item 2 and Q18 question 3 |
| v1 refuses bare mirrors; coverage unequal (medium) | Coverage gap stated, v2 shown without mirrors (343,676,638 B), and v1 follow-up 5 added | packet; evidence v2 projection |

Smaller corrections from the re-evaluation:

- Small `mutate-10` git inequality 2 is now 1,066,625 B (object). It was
  1,069,932 B, which counted `r01-row`'s 3,307 B of new objects twice.
- The estate object figure is unchanged, at 67,129,040 B.

### Key numbers (re-evaluated; same counters)

- **Estate `mutate-10`, file half.**
  - Inequality 1: 674,467 B read against 674,291 B, a fail by the sniff
    only.
  - Inequality 2: n/a, blocked by WP0(d). The carried seats were
    1,217 ≤ 1,217.
- **Estate `mutate-10`, git half.**
  - Inequality 1: 67,115,992 = 67,115,992 over worktree seats, with
    readback of at least 16,859,136 B outside it.
  - Inequality 2: 70,619,800 B against 806,672 B (chunk) or 67,129,040 B
    (object).
- **Unchanged reruns, CPU ratio.** Estate 6.9 % to 8.7 %, a pass. Small
  15.1 % to 16.2 %, a fail.
- **v2 first pass.** 350,103,767 B; 343,676,638 B without mirrors. v1,
  unfixed, was 932,119,799 B. Derived with follow-up 1: about 395 MB.

### Commits (signed; hooks ran; process-safety audit passed)

- `9581384`: the harness, its tests and the justfile line (`evaluate`
  subcommand).
- The commit after it: the evidence doc, the packet, the earlier note's
  corrections, and this note.

### Validation

- **Bench scripts, run directly.** `python3 -m unittest test_s3_estate`
  ran 15 tests, all ok. `ruff check` and `ruff format --check` were clean
  on `s3_estate.py` and `test_s3_estate.py`. check-fast's ruff does not
  cover these two files.
- **Re-evaluation.** `s3_estate.py evaluate` re-ran the evaluation of
  `run-estate-1` and `run-small-2` into this session's scratch, and every
  figure above comes from it.
- **check-fast.** `flock …/.check-fast.lock nice -n 10 nix develop
  .#default --command just check-fast` exited 0, on the tree with every
  change in this step except this paragraph.
  - Timing: it was started in the foreground at 18:48:08Z and was queued on
    the shared lock until 19:06Z. At the tool's 10-minute cap, still
    queued, it was moved to the background. It was not stopped. The
    session then blocked on its end marker (19:10:17Z) and on its exit
    status before committing.
  - Gates passed: ruff ("All checks passed!"), gitleaks ("no leaks
    found"), cargo fmt, and both clippy runs with `-D warnings`.
  - Tests: 26 test-result lines, all ok and none failed. That includes the
    agent's 437 unit tests, the fault harness (56) and the power-loss
    proofs (5, and 2 for resume). The 22 contract tests passed.

## Open

- **Operator rulings needed:**
  - Q15 on the revised packet, including whether to hold PR 3 until
    follow-ups 2 and 3 meet their targets over several rounds;
  - the three OI-1003-Q18 questions: git-half granularity, object-store
    reads in inequality 1, and the metadata allowance as a formula or
    deferred.
- **Low review findings, not fixed (by dispatch).**
  - Mirrors and copy exit codes: the harness drops mirrors from the census
    expectations and the Q18 bounds. copy exits 1
    `CONTRACT_SELF_INCONSISTENT` for `.codex`, `.config` and `.local`
    every pass, and for `projects` at `mutate-10`. Neither is recorded in
    the evidence doc. The medium fix stated v1's mirror gap.
  - Packet item 2: the 243 MB "before #146" figure is unmeasured and is
    the on-disk store, not the 179,409,341 B pack. 1.4 ms is the whole
    bundle; the metadata alone is about 1.06 ms. Only `4a10bb8` was run of
    the three planned builds.
  - check-fast ran as a background task in earlier sessions, and its ruff
    covers only `scripts tests`, not the bench scripts. `test_s3_estate.py`
    is optional tier. This session ran the tests and ruff on the bench
    scripts directly.
  - The raw JSON and logs exist only in scratch, and the Linear ledger was
    not reconciled.
  - Harness gaps: the v2 model fetch runs outside every S2 window, and
    `maxrss_kib` is a running maximum (`RUSAGE_CHILDREN`).
    `read_source_file_bytes` is 0 for estate-capture and snapshot.
  - Packet Reasons omit the architecture review's re-open condition ("only
    if measured bundle cost still breaks S3"), which these measurements
    meet.
  - The deletion inventory is stale at `dfb9604` (#151: carry_v2 is 4,883
    lines; `estimate.rs` is 3,363). It also misses five `IO_NONE_ALLOWLIST`
    rows in `tests/refusal_taxonomy.rs` that PR 3 must edit.
  - "Saved on refs and history" contradicts item 2's attribution to
    capture metadata. The Reasons bullet was rewritten for the re-base
    finding.
- **Re-measurement.** Nobody has requested a re-measure on `dfb9604` or
  `cd4ffad`, and none was made. A run after follow-ups 1 to 3 would test
  the derived 395 MB first pass and the targets over several rounds.
- **Scratch.** The earlier run's scratch (`$TMPDIR/s3-estate-20261004`) and
  the earlier attempts' scratch (`s3-estate-fix-6eYBLn`,
  `s3-estate-fix2-oddLbt`) were left alone. This session's scratch,
  `$TMPDIR/s3-estate-fix3-8Sbq5g`, holds the re-evaluated JSON and the
  helper scripts. It is kept as the re-evaluation's working copy.
- **Linear.** No distilled facts are on Linear, because no Linear write was
  in scope.
