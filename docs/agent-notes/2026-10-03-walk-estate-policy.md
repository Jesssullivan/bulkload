# 2026-10-03 walk-estate-policy (#129, #131, #89)

Lane: walk-estate-policy, branch `feat/walk-estate-policy-20261003` from
`origin/main` (c65dee5). Rulings: OI-1002-Q24 (bulkload completes before the
single migration run), OI-1002-Q29 (ultracode, full steam), OI-1002-Q34
(wave 2), OI-1002-Q33 (#124 salvage bound; R25 strict for anything the
destination durably held), R-N13 (receipts cite rulings; this note).

## Done

- **#129 walk caps.** The issue's three false refusals are fixed in
  `crates/bulkload-agent/src/walk.rs`:
  1. a directory at the depth cap is opened, listed and closed, and refuses
     `PATH_DEPTH_EXCEEDED` only when it holds something the walk would
     carry (an empty one, or one holding only tagged file temporaries or
     other-device seats in a same-device walk, is carried whole);
  2. the tagged temporary-directory check runs before the depth cap;
  3. `PATH_TOO_LONG` is applied after `fstatat`, so an other-device seat is
     skipped and a tagged engine temporary (file, or directory of them) is
     recorded as one.
  The caps are `WalkLimits` (defaults 256 / 4095, configurable only below
  them; out-of-range refuses `FIELD_DOMAIN_VIOLATION`). A capped subtree
  stays a typed, path-attributed refusal; `TransferStats::capped_subtrees`
  and the `capped_subtrees=` receipt field count them, and `walk` prints
  every refused seat by path and code. Never counted as carried: any
  refusal still skips `finish_directories`.
- **#131 intent-to-add.** Capture refuses `GIT_INVENTORY_INTENT_TO_ADD` when
  an entry's mode is not the mode `git add -N` would read from its seat
  (`git add -N f; chmod +x f`), before any destination write. Restore checks
  the custody against the worktree tree before any worktree byte (standalone:
  after init and object import; linked: before `worktree add`). Entry flags
  are parsed as hex and `CE_FSMONITOR_VALID` (0x200000) is masked; every
  other flag still refuses `GIT_INVENTORY_MALFORMED`. Probe on git 2.52.0:
  with the hardened `core.fsmonitor=false` Git does not surface the bit, so
  the mask is defence in depth; a test builds a real FSMN index with a hook
  fsmonitor and proves capture and restore work, the hook never runs, and
  the restored index has no FSMN extension.
- **#89 caller retry contract.** `carry_v2/retry.rs`: `FenceRetry` (12
  attempts, 500 ms base, 30 s ceiling, equal jitter via `RandomState`, no new
  dependency), `retryable` (`repository_fenced`, `quarantine_held`,
  `journal_held`), and the final typed refusal
  `JOURNAL_OWNERSHIP_CONFLICT` / `*_retries_exhausted`.
  `Ingest::finish_retrying` resumes between attempts. A held journal lock
  now refuses with reason `journal_held` (a journal holding another plan
  stays bare and is not retried). Documented in the module and in
  `docs/design.md`.

## Open

- #89: no wire-side caller exists yet (W4/W5 Git sub-stream receiver); it
  must finish through `Ingest::finish_retrying`. The PR says Refs #89.
- #131: neo's git version was not checked from sting; the mask makes the
  answer irrelevant to classification, but cohort4 should still note it.
- A legacy standalone capture whose custody fails the restore-side check
  leaves an initialized `.git` at the destination (no worktree bytes).
  Capture-side refusal prevents new ones, and no migration run has used
  #106 custody yet (OI-1002-Q24).
