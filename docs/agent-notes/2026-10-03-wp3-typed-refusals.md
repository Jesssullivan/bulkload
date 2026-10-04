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

## PR 2 — no blanket conversions (branch feat/wp3-typed-refusals-20261003-pr2, stacked on PR 1)

- Removed `From<std::io::Error>` and `From<postcard::Error>` for
  `BulkloadRefusal` (bulkload-proto) and `From<std::io::Error>` for
  `estimate::Refused`. Added `crate::refuse::RefuseAt::refuse_at(site)`
  (plus `refuse::io` / `refuse::codec` for non-`?` sites). The change is
  compiler-driven: about 600 `?` sites were rewritten by a script fed by rustc's
  JSON diagnostics, with `site = module::function`. The macOS-only blocks,
  which do not compile on sting, were audited by hand (`space::counts`,
  `stderr_store::extended_acl`). carry_v2 changed only mechanically, because
  the removal forced it.
- `tests/refusal_taxonomy.rs::bare_io_none_sites_only_shrink`: an allowlist
  of non-test `Io(None)` sites per file, 70 in total. A rise fails, and a fall
  fails until the table is lowered.
- v1 Git children are routed through the estimate classifier. New helpers:
  `estimate::run_git` (stdin and stdout piped, stderr drained into
  `StderrClass`), `estimate::StderrTap` for streaming children and
  `estimate::child_failed` for `Command::output` sites. A non-zero exit
  refuses `GIT_CHILD_FAILED(class)` instead of `GIT_INVENTORY_MALFORMED`.
  The routed sites:
  - `git_carry::{output,input,text}` and `estimate::{feed,run,walk,thin_pack}`
  - `raw_tree::{capture,prune}`, `shallow::{write_bundle,unpack}` and
    `batch_objects`
  - the status catch-alls in `read_authority`, `nested_head`,
    `nest_index_hides_changes`, `filter_drivers`, `nest_has_stash`,
    `nest_detached_unreachable`, `collapsed_gitlinks` and `retained_blobs`
  - `nest_has_stash` and `shallow::write_bundle` no longer inherit the
    agent's stderr.
- design.md: one paragraph on the refusal contract.
- Wire: no bump. Transfer refusals keep their codes: `.refuse_at` maps to
  the same `IO(errno)` / `FRAME_CODEC`. `GIT_CHILD_FAILED` is raised only
  by local Git verbs and their ledgers, never inside a frame.

Validation passed: `just check-fast` and `just resume-power-loss`, both run with
`nix develop .#default`.

### PR 2 merge of PR 1 (28ec2d7) and main (2026-10-04, R-N71, OI-1003-Q23)

PR 1's head 28ec2d7 already carries main through 4a7b86b (#145 WP1 S2,
#144 v1 counters, #152 handoff crate, #146 chains, #154 salvage). Merged
here, with main's new code brought under PR 2's rules:

- WP1 custody (`git_carry::export_repository_inner`): a failed child under
  a moved pack listing now arrives as `GIT_CHILD_FAILED(_)`, so the custody
  match takes `GitInventoryMalformed | GitChildFailed(_)`. Without that a
  `gc`/`prune` racing the pass would refuse instead of deferring (S5).
  `estate::tests::a_git_child_failure_without_a_rewrite_still_refuses` now
  expects `GIT_CHILD_FAILED stderr_class=bad_object`.
- #144's `pack_child` drains stderr through `estimate::StderrTap` (it was
  `Stdio::null`) and returns only the storage reads: a failed packing child
  refuses `GIT_CHILD_FAILED` with its class, ahead of the write it cut short.
  Its two `Io(None)` sites are gone: a missing stdin pipe is `WORKER_LOST`,
  and a pid that does not fit `pid_t` is `IO(ECHILD)` via `refuse::io`. The
  `Io(None)` allowlist is unchanged (git_carry.rs stays at 1).
- #146's chained export (`shared::write_chained`, the `rev-list --stdin`
  and `pack-objects` pair) and `create_bundle`/`write_full`/
  `write_with_prerequisites` go through `pack_child`, so the old
  `Option`/`!success` catch-alls to `GIT_INVENTORY_MALFORMED` are gone.
  `chain.rs` reuses `input`/`output`/`text`, so it is classified already.
- #145's `partial_clone` classifies a `git config` failure other than exit 1
  with `estimate::child_failed`; `promisor_packs` and `pack_listing` name
  their sites.
- Every other bare `?` the merge brought in (estate chain links, chain.rs,
  `estimate::Repository::local`, `priority::*`, the handoff binary's
  receipt) now goes through `.refuse_at(site)`.

## Open

- Deferred: `verify_bundle`'s residual `GIT_INVENTORY_MALFORMED` (the
  documented #106 split), and the `.status()` probes whose exit code is the
  answer (`show-ref --verify --quiet`, `diff --cached --quiet`). They
  inherit or discard stderr but are not catch-alls over a failed child.
  They belong to WP7's `SourceRepo::git(ReadCmd)`.
- The `refuse_at` site is carried only at the call site today. WP3 PR 3
  persists it in `Refusal{code, site, errno}`.
- Merged main 4a7b86b (#146 v1 auto-prerequisite chains, #154 bounded
  salvage) on 2026-10-04 (R-N71, OI-1003-Q23). `refusal.rs` keeps main's
  live `SalvageBoundExceeded` before `FrameCodec` in the enum, `code()`,
  `CODES` and the test list, and still drops the dead
  `TransportAuthorityMismatch` (no constructor on main either). #146 folded
  estimate's `feed` into `git_carry::input`; the writer-thread join there
  now refuses `WORKER_LOST`, as `feed`'s did on this branch.
- `refusal_taxonomy.rs` keeps its local `prop_config`; folding it into
  `test_support::prop_config` (now on main) is a follow-up.
