# 2026-09-23: PR #55 git-carry-estimate review fixes (R-N74, R-N75, TIN-4545)

- **Scope:** fix the adversarial-review findings F1–F13 on the read-only
  `git-carry-estimate` verb. Each fix has a test that failed on the PR head
  and passes after it. The PR was not merged.
- **Rulings applied:**
  - R-N74: `missing_thin_pack_bytes` is the gate metric.
  - R-N75: refuse a partial or differently-shallow destination.
  - R-N69: local build exception.
  - R-N11, R-N12, R-N80: no process was signalled, and no guard hook refused
    anything.
- **Found during the re-run:** Git's default `pack.useSparse=true` packed one
  object the haves already reach (bulkload: 659 packed vs 658 walked). The
  estimate now pins `pack.useSparse=false` and refuses on any count mismatch.
  The M1 sender must build its pack the same way.
- **Shared target dir hazard:** another M2 lane's worktree builds into the
  same `CARGO_TARGET_DIR`, and workspace members hash identically across
  worktrees. Cargo then reported that lane's newer artifacts as fresh and
  ran its test binary. Touch this worktree's sources before each gate, and
  check the test list or the binary's strings.
- **Evidence:** `docs/evidence/git-carry-estimate-2026-09-23.md`, re-measured
  at `f65313b`. Nothing was written on sting or in any cohort repository.

## r2 (same day): re-review BLOCK at `5313c32`

- **Rulings applied:**
  - R-N74 and R-N75, fail closed.
  - R-N97: the gate is sent bytes ≤ 1.1× `missing_thin_pack_bytes` and sent
    objects ≤ `missing_objects`. The estimate is an upper bound, and exactness
    is checked against `git fetch`.
  - R-N80: no guard refusals occurred.
- **Findings fixed:** N1–N6 and N9, with the reviewer's artifacts ported as
  regression tests (see PR #55). N7 and N8 are deferred to a follow-up PR
  comment; the "lower bound" wording is corrected to "upper bound" in the
  module doc.
- **Found during the fix (N3):** upload-pack drops a have that is a parent of
  an earlier-processed have. For a shallow client (`--shallow`,
  edge-aggressive over the command-line haves), that changes the pack. The
  estimate drops every have that is a parent of another have, which keeps it an
  upper bound whatever order the client offers them in. It equals a real fetch
  whenever commit dates order the haves child-first.
- **Build:** own `CARGO_TARGET_DIR`
  (`cargo-target-bulkload-m2-w6-estimate`), so there is no shared-target
  staleness.
