# 2026-10-03 — whitepaper and bibliography (proof package item one)

**Lane:** `whitepaper` (Lane C), a workflow subagent on sting. Branch
`docs/whitepaper-20261003`. It was first cut from `origin/main` `57030e1`,
then rebased onto `04ea9cb` in the review-fix pass below. That base holds
WP1 (#145) and WP2 PR 1 (#144). Worktree
`bulkload.worktrees/whitepaper-20261003`. No PR yet.
**Rulings:** OI-1003-Q7 (proof package: whitepaper and bibliography),
OI-1003-Q23 (lane fan-out; C is the whitepaper and bibliography, per
Linear TIN-4543), R-N13 (receipts and this note).

## Done

- `docs/whitepaper/bulkload.md` covers the problem, the design, the
  invariants (R25, durability ordering, S2), how each SLO is proven, results,
  related work, future work, and limits. It cites code paths under
  `crates/` and evidence files with their dates. It is pinned to `04ea9cb`.
  - Section 5 quotes only `docs/evidence/`.
  - S1 is stated as not met. Gate (a) has no passing verdict, and the wire
    v5 engine has no gated sample.
  - S1 gate (b), the S2 budget and the S3 ratio are marked "pending gate".
  - SLO numbers are linked to `docs/slo.md`, not copied.
  - The formal model is described generically. Following OI-1003-Q32
    (TIN-4543), TLA+/TLC is the checker of record, and the Dhall catalogue
    and the Haskell N-version explorer are stated as planned work. It names
    no model invariant or mutation. It says the model cannot
    prove S2 at the level of the code and does not cover the Git carry, and
    that S3's Git half is not modelled. No model results are reported.
- `docs/whitepaper/bibliography.md` has 34 entries, each with a "verified
  via" note. The sources are Crossref DOI records, USENIX pages with ISBNs
  or BibTeX, the ANU and UPenn handles, official documentation pages and
  repositories. Every key cited in the paper is defined, and every defined
  key is cited (checked by script).
- `docs/slo.md` and `AGENTS.md` are **not** changed. The first draft's
  `docs/slo.md` pointer line was dropped by coordinator direction: the
  coordinator's docs PR carries that pointer, with rulings Q24 onward. The
  whitepaper is not a source of truth, so it is not listed in `AGENTS.md`.

## Review-fix pass (2026-10-03, 25 findings)

Two agents wrote this worktree during the pass: the workflow's fix agent
and a copy resumed by the coordinator's first redirect. By coordinator
ruling, lane C has one owner, the fix agent. Its edits were reviewed and
kept where correct; one example is Borg's `LocalCache.commit`.

- **S2 and WP1 (sections 2.7, 3.3, 4, 7).** The paper now describes what WP1
  delivered:
  - the `git_env` table, which removes eleven named variables (not every
    `GIT_*` variable);
  - `GIT_SOURCE_PARTIAL_CLONE`;
  - background priority for six verbs;
  - the overlap-first check;
  - the P-S2 proptest, which makes four `proptest!` sites;
  - `ObjectStoreRewritten` drift.

  Only these stay open: WP7, the S2 sampler, P34 and P35, and the model.
  `the_source_writes_no_content_bytes` is described as the R-N58
  no-source-pack check.
- **WP2 PR 1 (#144).** It merged while this pass ran, so the paper was
  re-pinned. Pack-child reads are counted as a lower bound and
  `census_walks` exists. The auto-prerequisite (PR 2) is not on `main`, so
  v1 still re-packs the whole history. The Git carry remains unmeasured in
  any run.
- **Code-state corrections:**
  - the source-ledger path is `LedgerSink::publish`, then
    `StorePublisher::commit_captures`;
  - R-N54 buffer reuse is not wired;
  - `carry_v2` has no feature gate yet;
  - the salvage bound is in #154, not on `main`;
  - the SQLite lock is bounded by step count, with no counter;
  - untyped refusal sites are an S4 gap;
  - S5 lists every class that still refuses.
- **Results:**
  - the W3 bullet gives native against rclone for each run, and the
    baseline's `status=fail`;
  - S3 is scoped to the pre-wire-v5 file path;
  - the three 2026-10-03 gate (a) attempts are in section 7 only, cited to
    Linear TIN-4543;
  - the link rate applies to that session only;
  - the R36 and rsync claims are softened.
- **Related work and bibliography:**
  - ALICE is described as constructing selected states. This was read from
    the paper's PDF, sections 3.3 and 3.6.
  - The power-loss bound (`exhaustive_limit` 12, `accept_bounded`) is
    stated, and added to section 8.
  - Borg's default `ctime,size,inode` and its newest-cmtime guard are
    stated.
  - The casync digest is SHA-512/256, per the README.
  - The Bup title is corrected.
  - rsync `--checksum` and Git's racy rule are stated precisely.
  - [B3-18] gains pp. 33–50, from the USENIX BibTeX.
  - [Unison04] cites its handle, verified through the repository API.
  - [AppleDiskWrites] was read through its JSON endpoint, and is cited only
    for its guidance.
  - A new [AppleFcntl] entry (XNU `fcntl(2)`, dated 2021-08-12) carries
    the drain and barrier semantics.

## Validation

`flock bulkload.worktrees/.check-fast.lock nice -n 10 nix develop .#default
--command just check-fast`, with
`CARGO_TARGET_DIR=/srv/fast-local/jess/cargo-target/whitepaper`. When the
fix commit was made, the run was queued behind other lanes on the shared
lock; its result is recorded in a follow-up commit to this note. The first
draft's run, on `57030e1`, exited 0. This lane changed documentation only.

## Open

- **No evidence file for the 2026-10-03 gate (a) attempts.** The paper cites
  them through Linear TIN-4543 in section 7 only. Once an
  `r23-2026-10-03-*` file lands in `docs/evidence/`, cite that instead.
- **OI-1003-Q24 and Q25 are not in `docs/slo.md`.** The paper cites them
  through TIN-4543 as pending amendments. Point at `docs/slo.md` once the
  coordinator's docs PR merges.
- **Formal model.** Name the model's invariants only after Lane B fixes them
  in `docs/formal/README.md` and they are on `origin`. Report only TLC
  results committed on `docs/tla-model-20261003`. Describe the Dhall and
  Haskell parts as done only once they land; add bibliography entries for
  them then, if cited.
- **Next sprint (OI-1003-Q34):** this lane's sprint 2 is the S2 budget
  instrument, using the v0 synthetic workload. Then refresh section 5.3.
- **S5 classes with no owner.** Configuration, shallow-frontier, nest
  custody and rebuildable-root movement, and directory-shape drift, still
  refuse. No ruling or work package names them.
- **Bibliography field not verified by tool:** the LNCS volume number for
  [TLC99], which is omitted.
- Refresh sections 2.7 and 5 when any of these lands: WP2 PR 2, #154, a
  gated gate (a) sample on corpus v1, the estate corpus (WP0(e)), or the
  model.
