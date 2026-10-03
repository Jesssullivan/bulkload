# 2026-10-03 closure-integrity lane (#133, #132)

Rulings: OI-1002-Q24 (bulkload completes before the single migration run),
OI-1002-Q29 (full steam), OI-1002-Q34 (wave 2), OI-1002-Q33 (salvage only
for byte-touching refusals; R25 strict for anything the destination durably
held), R-N13. Branch `feat/closure-integrity-20261003` from `origin/main`
727493a.

## Done

- **#133, closure gate integrity** (`closure.rs`, `main.rs`).
  - The top-level `verdict` of `bulkload.closure.v1` is always native
    (`unaccounted == 0`). A new top-level `gate` field carries the
    native-or-attested result and is the exit status. Without `--attest`,
    `gate == verdict`. The attested block drops `native_verdict`, because
    the top-level `verdict` now says it.
  - An attestation ledger must name both `plan` and `source_label`. A
    missing field is `REQUIRED_FIELD_MISSING`; a different value is
    `RECEIPT_BINDING_INVALID`.
  - Every attestation row must carry `capture`: the item's current capture
    digest, as the native item row now prints it. Use `null` only when the
    corpus holds no capture record. New rejection reasons:
    `capture-missing`, `capture-mismatch`, `capture-record-unreadable`. A
    malformed `capture` refuses the whole ledger (`FIELD_DOMAIN_VIOLATION`).
  - A native applied, refused or referenced-only record is never overridden
    (test `attestation_never_overrides_a_native_record`). An untyped IO
    refusal is still natively unaccounted, so an attestation may still close
    it, per OI-1002-Q13.
  - `--attest LEDGER.json` is recognised only as the first argument after
    the verb. After PLAN, `--attest` is a positional PRIVATE_STATE.
- **#132, repair vs apply** (`estate.rs`). `git-repair-missing-index` with
  a ledger refuses `RECEIPT_BINDING_INVALID` for an item that plans a
  workspace. It refuses before it writes anything (no receipt, journal or
  outcome). Test `a_repair_never_binds_a_workspace_item_and_apply_still_restores_it`:
  repair on a linked-worktree item's main repository refuses, and a later
  estate-apply restores the workspace (`workspace-restored`, closure
  `applied`).

## Validation

- `nix develop .#default --command just check-fast`: green (lib 413 passed,
  fault harness and power-loss proofs green, contract tests OK).
  CARGO_TARGET_DIR `/srv/fast-local/jess/cargo-target/closure-integrity`.
- No durability ordering changed: the #132 refusal happens before the first
  write. `just resume-power-loss` was therefore not required; the
  power-loss proofs ran inside check-fast's fault harness.
- Read-only `closure-report --attest` on sting (no estate apply was run;
  the input directories' mtimes are unchanged):
  - cohort1 with its 2026-10-02 ledger: `verdict` fail, `gate` fail, exit
    nonzero. 22 rows superseded and 4 rejected as `capture-missing`. Before
    this change, `verdict` printed pass.
  - cohort3 with its 2026-10-02 ledger: refused `REQUIRED_FIELD_MISSING`
    (no `source_label`).

## Open

- The existing sting ledgers (`receipts/cohort{1,2a,3}-*.ledger.json`) have
  no per-row `capture`, and the cohort3 ledger has no `source_label`. Under
  #133 they no longer close items. They must be regenerated with the
  builder scripts (outside the repo) from a fresh native report, which now
  prints each item's `capture`.
- State directories written by an earlier repair on a workspace item (if
  any) still carry the `refs-imported` journal. This lane does not migrate
  them. An operator ruling is needed before deleting any such journal.
