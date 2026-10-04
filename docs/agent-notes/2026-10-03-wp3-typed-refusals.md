# 2026-10-03 — WP3 typed refusals (lane wp3-typed-refusals)

Architecture review WP3 (docs/plans/2026-10-03-architecture-review.md, on
PR #143), PRs 1 and 2. Rulings: OI-1003-Q15..Q21 (WP0 (a)–(g)), OI-1002-Q33
(#124), R-N13; R33 (refusals are values), R-N121 (stderr classified, never
echoed). carry_v2 stays frozen (OI-1003-Q15).

## PR 1 — taxonomy (branch feat/wp3-typed-refusals-20261003)

- Deleted the 9 refusal variants with zero constructors anywhere outside
  `refusal.rs` (verified by grep over all crates, including tests, docs and
  scripts, before deleting): `SnapshotCustodyUnavailable`,
  `SnapshotCustodyEscape`, `SnapshotOwnershipUnproven`, `SealedObjectChanged`,
  `PathMapDetached`, `RollbackSnapshotMissing`, `RollbackEndStateDiverged`,
  `JournalAlreadyRolledBack`, `TransportAuthorityMismatch`.
- Added `ProtocolStateViolation`, `WorkerLost`, `SqliteBackupFailed(Option<i32>)`
  (SQLite extended result code) and `GitChildFailed(StderrClass)`.
  `StderrClass` moved from `git_carry::estimate` to `bulkload-proto` (it is
  now a refusal payload); `estimate` re-exports it, so paths are unchanged.
- First constructors:
  - `ProtocolStateViolation`: transfer's well-formed-but-unexpected frames and
    its own bookkeeping invariants (were `FRAME_CODEC` or bare `IO`).
    `FRAME_CODEC` stays for bytes that do not decode and a proto/wire_id
    mismatch.
  - `WorkerLost`: thread joins, closed worker channels and the closed credit
    gate (were bare `IO`).
  - `SqliteBackupFailed`: `provider_sqlite::snapshot`'s open, step and
    journal-mode calls (were bare `IO`).
  - `GitChildFailed`: estimate's probe on an unexpected non-zero exit (was
    `GIT_INVENTORY_MALFORMED`).
- `tests/refusal_taxonomy.rs`: every variant has a non-test constructor
  (source scan that drops comments, literals and `cfg(test)` items, and
  ignores patterns and comparisons), plus a fixed-seed proptest that a
  `GitChildFailed` refusal never shows a stderr byte (R-N121). The local
  `prop_config` should fold into the shared `test_support::prop_config` once
  PR #145 / #146 land it.

Wire: no bump. Codes cross the wire only as strings in `Control::Refused` and
`Decision::Refuse`, whose schema is unchanged; the deleted codes were never
constructed, so never sent; the new codes are session-ending or local-verb
refusals, not per-entry wire codes.

Validation: `nix develop .#default --command just check-fast` green (rustc
1.96.1).

## Open

- Merged main 4a7b86b (#146 v1 auto-prerequisite chains, #154 bounded
  salvage) on 2026-10-04 (R-N71, OI-1003-Q23). `refusal.rs` keeps main's
  live `SalvageBoundExceeded` before `FrameCodec` in the enum, `code()`,
  `CODES` and the test list, and still drops the dead
  `TransportAuthorityMismatch` (no constructor on main either). #146 folded
  estimate's `feed` into `git_carry::input`; the writer-thread join there
  now refuses `WORKER_LOST`, as `feed`'s did on this branch.
- `refusal_taxonomy.rs` keeps its local `prop_config`; folding it into
  `test_support::prop_config` (now on main) is a follow-up.
