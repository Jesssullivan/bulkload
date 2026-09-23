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
