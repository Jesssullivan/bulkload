# 2026-10-06 WP3 PR 3: typed outcome ledger and dispositions (S4 backbone)

Lane `wp3-pr3-outcome-ledger`, branch `feat/wp3-pr3-outcome-ledger-20261006`,
cut from `origin/main` at `b6ecd50`. No PR opened (the coordinator opens it).
Rulings: OI-1003-Q1 (S4), OI-1003-Q14, R33 (refusals are values), R-N13.

## What was done

Architecture review WP3 PR 3 (`docs/plans/2026-10-03-architecture-review.md`).
Before this, closure passed with undispositioned refusals and matched on
outcome strings.

- `crates/bulkload-agent/src/outcome.rs` (new): `Outcome` (10 named outcomes
  and `Refused(Refusal)`), `Refusal{code, site, errno}`, `OutcomeRecord`.
  Persisted as an 8-byte magic plus postcard; decode is strict (the whole
  input must be consumed). A record without the magic is read as the legacy
  `(source, outcome, reason)` tuple and mapped; what cannot be mapped is an
  `Unreadable` value. Refusal codes are stored as their stable strings, so
  deleting a refusal variant (lane L5) cannot shift a recorded code.
- `crates/bulkload-agent/src/disposition.rs` (new): the disposition ledger
  (`bulkload.dispositions.v1`, postcard, strict, bound to plan and SOURCE
  label). Rows are accept / re-carry / abandon with reviewer and date, per
  item or as a standing policy. No row can name `IO` or `FRAME_CODEC`.
- `closure.rs`: classification matches on the enum. New
  `Disposition::RefusedPendingReview`; `Refused` now means a reviewed typed
  refusal. `gate` (S4) passes only when nothing is unaccounted and nothing is
  pending review. An attestation row cannot close an item whose own record
  is an untyped refusal (`native-refusal-untyped`); an attested `refused`
  row needs a review like a native one. The native `verdict` is unchanged.
- `main.rs`: `closure-dispose LEDGER PLAN SOURCE ITEM|--policy CODE
  accept|re-carry|abandon REVIEWER YYYY-MM-DD`, and
  `closure-report --dispositions LEDGER`.
- `estate.rs` (lane L6a's file, minimal hunks): `Receipt.refusal`, `emit`
  writes the typed record with the verb as site, `repair_missing_index`
  writes typed records, `LedgerEntry.record` is
  `Option<Result<OutcomeRecord, Unreadable>>`, `id` is `pub(crate)`.
- `closure::Json` stays. `serde_json` is not in the agent's dependency
  graph and the R34 wall is a closed list; the new ledger is postcard, so
  nothing new needs JSON. The reason is recorded on the type; it goes with
  the attestation ledger under WP4.
- Fixture `crates/bulkload-agent/tests/fixtures/outcome-legacy/`: 15 legacy
  records, asserted byte for byte against the legacy encoding.
- Docs: `docs/slo.md` amendment 2026-10-06 (S4 proof status),
  `docs/design.md`, property plan rows P72 and P73.
- WIRE_SCHEMA is untouched (v5). No new crates.

## Existing tests deliberately updated

- `estate.rs` `closure_lane_20261002`: a typed refusal now classifies as
  `RefusedPendingReview`, not `Refused` (no review exists in those tests).
- `estate.rs` drift capture test: reads the durable outcome through
  `outcome::read_record` instead of decoding the legacy tuple.
- `closure.rs` example tests: built on typed records; passing reports now
  carry reviews for their typed refusals.

## Properties

- P72 OUTCOME-ROUNDTRIP: `outcome.rs` `p72_*`,
  `checked_in_legacy_fixture_reads`, `golden_encodings_are_stable`.
- P73 CLOSURE-DISPOSITION: `closure.rs` `tests::p73::*` (256 fixed-seed
  cases each).

Red on mutation (scratch run on sting, 2026-10-06, each mutant applied to
one file, tests run, file restored byte-identical):

| Mutant | Tests that turned red |
| --- | --- |
| lax decode (trailing bytes accepted) | `p72_typed_records_round_trip_strictly`, `p72_every_legacy_string_maps`, `disposition::ledgers_decode_strictly_and_bind` |
| legacy reader drops `refs-imported` | `p72_typed_records_round_trip_strictly`, `p72_every_legacy_string_maps`, `checked_in_legacy_fixture_reads` |
| gate ignores pending review | both `p73::*`, 2 example tests |
| bare `IO` classified as typed | both `p73::*`, 3 example tests |
| attestation untyped check removed | both `p73::*`, `attestation_cannot_close_an_untyped_refusal` |
| `is_typed_code` admits `IO` | both `p73::*`, 4 others |

Unmutated: 25 of 25 in `outcome:: disposition:: closure::`.

## Open

- WP3 PR 4: transfer and SQLite provider outcomes into the same ledger.
  Until then S4 is proven for estate items only.
- `estate.rs` is lane L6a's file; this branch touches it (listed above). A
  merge with L6a will need those hunks reconciled.
- A legacy `refused` record whose code lane L5 deletes
  (`GIT_DESTINATION_FILESYSTEM_UNSUPPORTED`, `JOURNAL_OWNERSHIP_CONFLICT`)
  will read as `refusal-untyped` once the code leaves the taxonomy, which
  is unaccounted and fails closed. Nothing here depends on either code.
- A `re-carry` review counts as a disposition for the gate; nothing yet
  checks that the re-carry later happened.
- Reviewer names are free text; nothing authenticates them.
