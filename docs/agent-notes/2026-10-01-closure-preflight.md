# 2026-10-01: Closure report and destination space preflight

**Lane:** closure-preflight. Ratified by operator interview 2026-10-01 Q2
(OI-1001-Q2), with signing per OI-1001-Q6. The review fix round is
OI-1002-Q11. Receipts cite R-N13.

- Issue bulkload#79, product bar #34, PR bulkload#80. Follow-up: #101
  (`estate-capture` space check).
- Worktree `bulkload.worktrees/closure-preflight-20261001`, branch
  `feat/closure-report-space-preflight`.
- Built with its own
  `CARGO_TARGET_DIR=/srv/fast-local/jess/cargo-target/bulkload-closure-preflight`
  on sting. Every gate runs through `nix develop .#default` (cargo 1.96).

## Done

### Round 1 (573c7f7)

- Added the refusal codes `CLOSURE_UNACCOUNTED` and
  `DESTINATION_SPACE_INSUFFICIENT`, plus `BulkloadRefusal::CODES`.
- Added `space.rs`, the `closure-report` verb and the estate-apply
  preflight.

### Round 2 (#80 review BLOCK, OI-1002-Q11)

Merged origin/main (#75, #77 wire v5) as a merge commit (R-N125). The
transfer.rs conflict was resolved to main.

- **H1, space preflight on v5.** The old v4 batch hunk and the special
  case in `copy()` are gone. `Inbound::entry` now reserves the size of
  every entry decided `Send` or `WantManifest` until its `Held`. It checks
  the reserved total against a cached `statvfs`, refreshed on each group
  commit and every 256 MiB. An entry that does not fit is refused as a
  value (`Decision::Refuse`), and the session continues.
- **H2, untyped refusals.** A bare `IO` or `FRAME_CODEC` reason is now
  unaccounted (`refusal-untyped-io` / `-frame-codec`), not refused.
- **M3, exact journals.** `closure-report PLAN CORPUS SOURCE STATE...`
  requires the exact journal for the item's current capture and SOURCE
  label. It also checks that the outcome record names the plan's source,
  and that the outcome matches whether the item plans a workspace.
  Refs-only for a workspace item is unaccounted. Stale and foreign
  journals are listed.
- **M4, R33 in `space_plan`.** An item the space plan cannot read is
  skipped, so it refuses on its own in apply. A test covers this.
- **L5, the cheap fixes.** A linked worktree is charged to both its
  repository and its workspace. The corpus staging peak is now twice the
  `jobs` largest bundles. Darwin probes with `statfs` (64-bit counts).
- `CODES` gained `GIT_DESTINATION_FILESYSTEM_UNSUPPORTED`, which the sync
  test caught after #75.
- **Gates under nix, at the round-2 head:**
  - `just rust-check`: 573 passed, 0 failed.
  - `just fault-harness`: 63 passed, 0 failed.

## Closure re-run (read-only, sting, 2026-10-02; no estate operation)

SOURCE labels come from the apply receipts and are confirmed by zero
unmatched journals:

| Cohort | SOURCE | Verdict | Planned | Applied | Refused (typed) | Referenced-only | Unaccounted |
|---|---|---|---|---|---|---|---|
| cohort1 | `neo-20260922-c1` | fail | 26 | 0 | 0 | 22 | 4 |
| cohort2a | `neo-20260922-c2a` | fail | 169 | 147 | 13 (`GIT_DESTINATION_OCCUPIED`) | 0 | 9 |
| cohort2b | `neo-20260922-c2b` | pass | 5 | 3 | 0 | 2 | 0 |

- cohort2a's run includes the item7955 states `private-4` and `-5`; that
  item is now applied.
- Every unaccounted item is `refused` with `IO (errno 2)` (ENOENT), so
  each one is `refusal-untyped-io`.
- **cohort1:** asfirewire-legalab, crs310-8g-2s-in,
  medical-massage-specialists-infra, printstack.
- **cohort2a:**
  - Fuzzy.worktrees: docs-ox-alpha-sota, fuzzy-dev-disposition,
    harness-contract, ox-alpha-main-line, rescue-huihui-clean.
  - tinyland-auth.worktrees/tin-4182-file-enrollment-20260919.
  - tinyland-content.worktrees: tin-4177-owner-durability-20260919,
    tin-4177-owner-scoped-posts-20260915.
  - tinyland-invitation.worktrees/tin-2716-durable-invitation-20260919.

## Open

- The 13 ENOENT items need cause attribution and resolution. This lane
  does not fix them.
- #101: `estate-capture` per-item space check.
- `estate-apply` charges bundle size, which is a lower bound on a checkout.
