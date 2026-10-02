# 2026-10-02: cohort3 closure

**Lane:** cohort3-closure. Ratified by operator interview 2026-10-02:
Q3 (OI-1002-Q3) closes the cohort, Q4 (OI-1002-Q4) covers the plain-git
fixes on sting, and Q5 (OI-1002-Q5) covers the follow-up issue. The
procedure follows OI-1001-Q12 (the item7955 precedent). One-item carries
for the 10 unrestored items are exempt from the R-N56 freeze. Cohort4 is
deferred to the M2 engine and was not touched. Receipts cite R-N13.
Linear TIN-3692. Receipts are on sting under
`/srv/fast-local/jess/bulkload/receipts/cohort3-closure-20261002.*`.

## Done

- Probed the 10 unrestored items on neo and sting without writes
  (`GIT_OPTIONAL_LOCKS=0`). Compared refs, working bytes (tracked and
  untracked, not ignored, including nested repos) and index state.
- 3 jesssullivan-infra items (refused `GIT_INVENTORY_MALFORMED` on 09-23):
  the missing bundle prerequisites were `80e2620` (origin
  `feat/mail-accounts-runner-set`) and `584095c` (PR #98 head, merged).
  Fetched both from origin on sting as objects only, with no ref writes.
  Re-ran `git-repair-missing-index` (8976397) on the 09-22 bundles, and all 3
  were repaired. Local heads and remotes did not change. 264 carry refs were added.
- 5 items have no unique content on neo (8311, caldera, cmux,
  dollhouse-farm, k8sy-windows): recorded as referenced-only based on a parity audit.
- 2 Scruff.jl worktrees: their only state unique to neo is intent-to-add index
  entries (7 and 2). A one-item `estate-capture` on neo
  (`tinyland-state/bulkload-cohort3-20260922-item-{dd07b398,1a75a0ec}`)
  refused `GIT_INVENTORY_MALFORMED` again, so both are recorded as typed refusals.
- Payload sample of the 63 repaired items: 10 present and compared, 8
  byte-identical. FinanceBro: every neo path is identical; sting has 726 extra
  untracked files, and neo has one branch from 09-30 that is pushed. chapel: 5 tracked
  `llvm-src/bindings/ocaml/target/*` files were missing on sting, and 4 paths
  differ only in case (`readline` vs `readLine`).
- Closure ledger `cohort3-closure-20261002.ledger.json`
  (`bulkload.closure-ledger.v1`): 73 planned, 71 referenced-only, 2 refused,
  0 unaccounted. Native `closure-report` still reports 73 unaccounted
  (`no-outcome-record`), because cohort3 was never estate-applied.
- OI-1002-Q4 on sting (receipts `gitfix.{before,after,log}`):
  - Ran `git reset -q` on 8311, cmux, dollhouse-farm and k8sy-windows.
  - Ran `git reset -q` and `git add -N` on both Scruff worktrees; their
    status now hashes equal to neo's.
  - Restored chapel's 5 files with `git checkout --`. The 4 case-only paths
    were left as they are.
  - caldera was not touched.
- OI-1002-Q5: opened bulkload#95. It asks for the repair verb to write
  apply-style records into a state dir, and for `closure-report --attest`.

## Open

- bulkload#95 (M2).
- cmux and dollhouse-farm on sting show their submodule gitlinks as
  modified, because the nested `.git` directories on sting have no refs or
  index.
- caldera on sting is on a different branch from neo.
