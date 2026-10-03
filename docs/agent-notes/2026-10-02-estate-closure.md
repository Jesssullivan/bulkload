# 2026-10-02 — estate-closure lane: #95, #101, #106

Lane: estate-closure, host sting. Worktree
`bulkload.worktrees/estate-closure-20261002`, branch
`feat/estate-closure-20261002`, cut from origin/main 41c8892 (#114).

Rulings cited:
- OI-1002-Q24: bulkload completes before the single migration run.
- OI-1002-Q25: gate plan, pre-migration fixes (#95, #101, #106).
- OI-1002-Q29: operator "ultracode, full steam ahead" (2026-10-02).
- OI-1002-Q5 (#95), OI-1002-Q11 (#101): the issues' own rulings.
- R-N13: receipts cite rulings; this note.

## What changed

- **#95, repair records.** `git-repair-missing-index BUNDLE REPOSITORY
  SOURCE NEW_RECEIPT [PLAN CORPUS PRIVATE_STATE]`. With the three extra
  arguments the bundle is bound, before anything is written, to the one
  planned item whose current CORPUS capture has the bundle's digest and whose
  repository (or workspace) is REPOSITORY; else `RECEIPT_BINDING_INVALID`.
  After the index is published and synced it writes the exact
  current-capture journal (`refs-imported`), then `{item}.outcome` =
  `index-repaired`. A refusal after binding records `refused` with its code
  unless the item is already journaled. `closure.rs` reads `index-repaired`
  + `refs-imported` as `referenced-only`.
- **#95, attestation.** `closure-report ... --attest LEDGER.json` joins a
  `bulkload.closure-ledger.v1` ledger in a separate `attested` block. A row
  closes an item only if the native ledger leaves it unaccounted, the source
  matches, the disposition is known, the basis is not
  `native-closure-report`, evidence is non-empty, and a `refused` row names a
  typed code. Native totals are never changed. A ledger naming another plan or
  label, or listing an item twice, is refused. JSON is parsed by hand (no
  dependency added).
- **#101.** `estate-capture` reserves each item's estimated bundle (the
  larger of its census's regular-file bytes and the retained bundle it
  extends) against a fresh `statvfs` of CORPUS plus in-flight reservations,
  before the export writes. An item that does not fit refuses
  `DESTINATION_SPACE_INSUFFICIENT` as its own receipt; the rest of the pass
  runs. A reuse hit reserves nothing.
- **#106.** Intent-to-add index entries are carried as custody
  (`refs/carry-export/intent-to-add-v1`: path, mode, empty-blob flag),
  written only when present, and re-marked after `read-tree` by
  `git-restore`, `git-restore-linked`, estate apply and
  `git-repair-missing-index`, then read back. `GIT_INVENTORY_MALFORMED` is
  split: `GIT_INVENTORY_MISSING_PREREQUISITE` (the bundle's prerequisites
  are absent from the receiving repository) and `GIT_INVENTORY_INTENT_TO_ADD`
  (the seat is gone, or the entry is in a nest, whose index is not carried).
  `git-attach-*` refuses a capture that carries intent-to-add entries.
  Skip-worktree and assume-unchanged entries are still refused as
  `GIT_INVENTORY_MALFORMED`.

## Validation

- `nix develop .#default --command just check-fast`: green (all new tests
  included). `just resume-power-loss`: green.
- Read-only `closure-report --attest` on sting (no estate apply was run):
  - cohort3: native 73 unaccounted, attested 73 (71 referenced-only, 2
    refused), 0 left.
  - cohort1: native 4 unaccounted, attested 4, 0 left.
  - cohort2a: native 9 unaccounted, attested 9, 0 left.
  - All three exit 0. The input directories' mtimes are unchanged.

## Open

- #106 item 1 (cohort4 breakdown by cause): this needs a read-only probe on
  neo. That is outside this lane's sting-only reach, so the breakdown is not
  posted and #106 stays open. The code half (carry intent-to-add, split codes)
  is in this PR.
- The lane picked up uncommitted work from an earlier interrupted run in the
  same worktree. It was reviewed and validated here before commit.
