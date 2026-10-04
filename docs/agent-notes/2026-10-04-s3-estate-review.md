# 2026-10-04 S3 estate lane: merge main after #159 (review step)

Rulings: OI-1003-Q15, OI-1003-Q18, OI-1003-Q35, OI-1003-Q38 (cited by the
dispatch; its text is in no repo branch), R-N13.

- **Branch:** `feat/s3-estate-20261004`.
- **Worktree:** `bulkload.worktrees/s3-estate-20261004`. This session was
  its only writer.
- **PR:** none opened (by dispatch).
- **Earlier note:** [2026-10-04-s3-estate.md](2026-10-04-s3-estate.md)
  covers the harness, the runs and the Q15 packet.

## Done

1. **Checked the starting state.** The worktree was clean, and HEAD equalled
   `origin/feat/s3-estate-20261004` at `be7acf8`. Its commits are
   `041c871` (harness, tests, justfile) and `be7acf8` (evidence, plan,
   note).
2. **Merged origin/main.** A signed merge commit, `2136c72`, brings in main
   `dfb9604`. That main carries #159 (the base of this branch, WP0(e)
   estate corpus), #160 (TLA+ model), and #150 and #151 (WP3 typed
   refusals). The merge had no conflicts.
3. **Recorded the build provenance.** It is in the evidence doc, the Q15
   packet and the earlier note:
   - The measurement build was main `4a10bb8`. That is before #151, and
     before #150. Both merged after the 08:49Z build.
   - The numbers were **not re-measured** on `dfb9604`.
   - What may differ there: #151 removed the blanket I/O refusal
     conversions and routes v1 git children through the estimate
     classifier (`GIT_CHILD_FAILED`). Refusal text may therefore read
     differently, such as finding 11's `IO (errno 2)` for bare mirrors.
   - What was checked: the `ExportOptions::chain` doc comment that
     finding 3 cites is unchanged at `dfb9604`.
4. **Fixed a stale line.** The earlier note's "PR #159 is not merged yet"
   was no longer true, so it now points here (R-N55).

Nothing was re-measured, and no scratch was deleted. The earlier run's
scratch (`$TMPDIR/s3-estate-20261004`) awaits an operator ruling under
R-N12. This session's own scratch holds only the commit messages.

## Key numbers (unchanged; estate scale, build `4a10bb8`)

- **Unchanged reruns.**
  - Git half: 0 source bytes, 0 pack bytes, and 54 of 54 census walks.
  - File half: 0 bytes received, but 176 B read: the 16 B SQLite magic
    sniff, once per refused database.
  - SQLite half: snapshot re-reads all 180,581,128 B every pass.
  - Wall time was 0.6 % to 1.7 % of the first pass, and CPU 6.9 % to
    7.9 %.
- **Deltas.**
  - Git inequality 1 holds with equality (3,209 B; 67,115,992 B).
  - Git inequality 2 fails: 39,352 B against 9,782 B at `mutate-1`, and
    70,619,800 B against 67,129,040 B at `mutate-10`.
  - The file half passes both inequalities at `mutate-10`: 674,467 B read
    against a bound of 4,095,275 B, and 1,217 B received against 263,623 B.
- **v2 projection against v1.**

  | Pass | v2 thin pack | v1 pack |
  |---|---:|---:|
  | first | 350,103,767 B | 932,119,799 B |
  | reruns | 0 | 0 |
  | `mutate-1` | 499 B | 39,352 B |
  | `mutate-10` | 1,065 B | 70,619,800 B |

## Validation

- **Command.** `flock …/.check-fast.lock nice -n 10 nix develop .#default
  --command just check-fast` exited 0. It ran on the merged tree plus these
  doc edits, from 11:00:07Z to 11:24:51Z, most of it queued on the shared
  lock.
- **Gates passed.**
  - ruff: "All checks passed!"
  - gitleaks: "no leaks found"
  - cargo fmt, then clippy with `-D warnings`
  - cargo tests: 26 test-result lines, all ok, none failed. The agent's
    unit tests were 437 passed.
  - the fault harness: 56 passed
  - the power-loss proofs: 5 passed, and `resume-power-loss` 2 passed
  - the 22 contract tests
- **After the run.** Only this paragraph changed.
- **How it ran.** check-fast runs past the 10-minute foreground limit, so
  it ran as this session's own background task. The lane blocked on a
  lane-private flock until the run's end marker appeared, and only then
  committed.

## Commits (signed; the hooks ran)

- `2136c72`: merge of origin/main `dfb9604`.
- The commit after it: these doc updates and this note.

## Open

- The operator's Q15 ruling on the packet, and the OI-1003-Q18
  metadata-allowance ruling. Both are carried from the earlier note.
- Whether to re-measure on `dfb9604` or later. Nobody has requested it.
  #151's commit message describes a refusal-typing change and says errno
  values and transfer codes are unchanged. No run has checked the counters
  on `dfb9604`.
- Cleanup of the earlier run's scratch, which is held for an operator ruling
  under R-N12.
- OI-1003-Q38's text is still in no repo doc.
- Distilled facts are not on Linear, because no Linear write was in scope.
