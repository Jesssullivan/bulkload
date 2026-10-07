# 2026-10-06 WP3 PR 3: typed outcome ledger and dispositions (S4 backbone)

Lane `wp3-pr3-outcome-ledger`, branch `feat/wp3-pr3-outcome-ledger-20261006`,
cut from `origin/main` at `b6ecd50`. PR
[#195](https://github.com/Jesssullivan/bulkload/pull/195), not merged by this
lane.
Rulings: OI-1003-Q1 (S4), OI-1003-Q14, R33 (refusals are values), R-N13, and
the operator rulings OI-1003-Q73, Q74 and Q75 (see "Operator rulings" below).

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
  deleting a refusal variant (lane L5) cannot shift a recorded code. (What
  a deleted code does to a record that names it is in the review round
  below: it reads as `refusal-code-retired`.)
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
- `main.rs`: `closure-dispose` and `closure-report --dispositions LEDGER`
  (the `closure-dispose` arguments changed in the review round below).
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

Unmutated: 25 of 25 in `outcome:: disposition:: closure::` (at `a0d78df`).

## Review round 1 (2026-10-06)

Seven medium/high review findings, fixed in the commit that carries this
section, on top of the signed merge of `origin/main` `2247ab8` (#190) at
`0591c2b`. Rulings as above.

- **Bare `IO` attested away (high).** `attest()` decided "untyped" from the
  unaccounted reason, and `classify()` returns `record-source-mismatch`
  before it looks at the refusal, so a bare `IO` recorded under another
  source could be attested. The check now reads the decoded record
  (`AttestationRow::verdict(native)`): any `Refused` that is not typed, and
  any `Unreadable::RefusalUntyped`, is rejected whatever the reason. P73
  gained `BareIoElsewhere`.
- **Reviews bound to the refusal instance (medium).** An item row is
  `Scope::Item { item, instance }`; `closure::instance(entry)` is a blake3
  digest of the item's current capture and its outcome record. `review()`
  matches item, instance and code. `closure-dispose` now takes `CORPUS` and
  the state directories (and `--attest`), builds the report, and refuses
  `RECEIPT_BINDING_INVALID` unless the item holds that typed refusal now
  (`Report::reviewable`). Stale rows are listed
  (`refusal-instance-stale`). New arguments: `closure-dispose
  [--attest LEDGER.json] LEDGER PLAN CORPUS SOURCE ITEM|--policy CODE
  DECISION REVIEWER DATE [PRIVATE_STATE ...]`.
- **Ledger bound to plan bytes (medium).** `Body.plan_digest` (blake3 of
  the plan file); `decode` compares it; `record()` reads and validates the
  plan for policy rows too. The path binding stays as well (#133 shape).
- **Retired codes (two medium findings, one fix).** Write-time and
  read-time validation are split. Writers (`Refusal::new`, `Row::new`)
  need a current code. Readers accept any code token
  (`outcome::is_code_token`). A record naming a retired code is
  `Unaccounted("refusal-code-retired")` in both formats, not unreadable; a
  review row naming one disposes nothing and is listed; the ledger still
  decodes and appends. No closed list of retired codes is kept, so lane L5
  needs no change here and nothing names its two codes.
- **`design.md` (medium).** The "gate equals verdict" sentence is amended
  with a dated S4 note, `refused-pending-review` is in the disposition
  list, and `CLOSURE_UNACCOUNTED` is stated to cover pending review.
- **Report schema (medium).** Bumped to `bulkload.closure.v2`, with the
  changed and added fields listed in `design.md`. `refusal` is again
  printed for typed refusals only (others print `recorded_refusal`), and a
  legacy untyped refusal prints `"outcome":"refused"` again.

Red on mutation (scratch run on sting, 2026-10-06, same method as above):

| Mutant | Tests that turned red |
| --- | --- |
| attestation decides untyped from the reason string | both `p73::*`, `attestation_cannot_close_an_untyped_refusal_under_any_reason` |
| `review()` ignores the instance | both `p73::*`, 3 example tests |
| `decode` ignores the plan digest | `a_ledger_is_bound_to_the_plan_bytes_not_only_its_path`, `ledgers_decode_strictly_and_bind` |
| review row decode requires a current code | `a_retired_code_fails_closed_per_row_not_per_ledger`, `a_retired_review_row_is_listed_and_the_rest_still_dispose` |
| record decode requires a current code | `a_retired_code_still_decodes_and_is_not_typed` |
| retired code classified as typed | `p73_closure_is_green_iff_every_refusal_is_dispositioned`, `attestation_cannot_close_an_untyped_refusal_under_any_reason` |

Unmutated: 31 of 31 in `outcome:: disposition:: closure::`, 6 of 6 in
`tests/space_closure_cli.rs`.

## Open

- WP3 PR 4: transfer and SQLite provider outcomes into the same ledger.
  Until then S4 is proven for estate items only.
- `estate.rs` is lane L6a's file; this branch touches it (listed above). A
  merge with L6a will need those hunks reconciled.
- A refusal whose code leaves the taxonomy (lane L5, PR #189, deletes
  `GIT_DESTINATION_FILESYSTEM_UNSUPPORTED` and `JOURNAL_OWNERSHIP_CONFLICT`)
  reads as `refusal-code-retired`: unaccounted, not reviewable, not
  attestable, closed only by a verb recording a current outcome. A review
  already written for it stops counting. Ruled as built (OI-1003-Q75).
- A disposition ledger is bound to the plan's bytes, so `estate-add` on a
  plan that already has reviews starts a new ledger.
- The refusal instance has no time in it: the same capture and a
  byte-identical record are one instance, so a re-run that refuses
  identically keeps its review.
- `docs/slo.md`: PR #189 inserts its amendment at the same place; a merge
  with L5 will conflict there.
- Low review findings, not fixed: "strict" decode accepts postcard's
  overlong varints; P73 does not generate attested-`refused` rows,
  `Unreadable::Codec`, `OutcomeUnknown` or no-record items; the writer's
  outcome names are still string literals converted at run time;
  `Refusal.site` is the verb, not the raise site.
- A `re-carry` review counts as a disposition for the gate; nothing yet
  checks that the re-carry later happened.
- Reviewer names are free text; nothing authenticates them.

## Recheck and ship (2026-10-06)

Recheck stage over `f6490d9`, diff read since the build head `a0d78df`.
Rulings as above.

- All seven medium/high review findings verified fixed by reading the code
  and re-running the lane's tests. No new medium/high defect found in the
  fix round.
- `origin/main` had moved to `48bd697` (#191 lane L6a, #192 S3 properties).
  Merged with a signed merge commit (no rebase); `estate.rs` merged without
  conflict and the writer's ten outcome literals are unchanged, so every one
  still maps through `outcome::NAMED`.
- On the merged tree: 31 of 31 in `outcome:: disposition:: closure::`, 6 of 6
  in `tests/space_closure_cli.rs`, then `just check-fast` inside
  `nix develop`. The merge commit carrying this section was made only after
  check-fast exited 0.
- Verdict CLEAN; the PR is opened by this stage and not merged.

Still open, beyond the list above (all low, none fixed here):

- `disposition::plan_digest` hashes the bytes it read and then validates the
  plan by path with a second read; the two reads are not one snapshot.
- With several state directories the last one holding an `{item}.outcome`
  record wins (unchanged behaviour), so the refusal instance and the
  untyped-refusal check follow the directories the operator names.
- A legacy `refused` reason whose first word happens to be an upper-case
  token (not a taxonomy code) reads as `refusal-code-retired`, not
  `refusal-untyped`. Both are unaccounted and neither can be attested.

## Operator rulings (recorded 2026-10-06)

- **OI-1003-Q73:** PR #195 merges under Q29.
- **OI-1003-Q74:** standing-policy rows are open-ended in time, as built: a
  policy stands until the plan's bytes change. `docs/slo.md` now states it as
  ruled, in a new dated amendment ("2026-10-06 (later)"); the earlier
  amendment's "design statement" wording is left as written and superseded
  there.
- **OI-1003-Q75:** a retired refusal code fails closed, as built.

Neither ruling changes code.

## Merge of main after #193 (2026-10-06)

PR #195 conflicted with `origin/main` and update-branch was refused. Rulings
as above.

- State found: the branch head `b9a762f` already held the merge of `48bd697`
  (#191 lane L6a, #192), with `estate.rs`, `design.md` and the property plan
  merged without conflict. Only #193 (`34e945e`, S1 gate (b) harness) was
  missing.
- Signed merge commit `f57d3ae` (parents `b9a762f`, `34e945e`; no rebase).
- Conflict list: `docs/slo.md` only. Both sides added a 2026-10-06 amendment
  at the same place; both are kept, main's gate (b) amendment first, then
  this lane's S4 amendment. `justfile`, `gate_b.py`, `test_gate_b.py` and the
  gate (b) plan and note came in from main untouched.
- `estate.rs` on the merged tree: L6a's decide core is as on main, with this
  lane's hunks on top (`Receipt.refusal`, `emit` with a site and the typed
  record, `repair_missing_index`, `LedgerEntry.record`, `id` `pub(crate)`).
  `git diff origin/main -- crates/bulkload-agent/src/estate.rs` shows only
  those hunks.
- Receipts on the merged tree plus the Q74/Q75 `slo.md` amendment, sting,
  inside `nix develop`:
  - `cargo fmt --check` and `cargo clippy --workspace --all-targets -- -D
    warnings`: clean.
  - `cargo test -p bulkload-agent --lib -- outcome:: disposition:: closure::
    closure_lane git_carry:: estate::`: 327 passed, 0 failed, 4 ignored
    (includes the five P67 `git_carry::decide_tests` and the six
    `estate::wp2_chain` P-CHAIN tests).
  - `tests/space_closure_cli.rs`: 6 of 6. `tests/git_group_minimality.rs`
    (P64/P65): 18 of 18.
  - `just check-fast`: exit 0. The merge commit was made only after that.
- Seed guard (#194) was not on `origin/main` (`34e945e`) at push time, so
  `tests/prop_seed_guard.rs` does not exist here. The lane's two proptest
  blocks (`outcome.rs` P72, `closure.rs` P73) already route through
  `test_support::prop_config(256)`.

Still open after this round: the list under "Open" above, less the two items
ruled by Q74 and Q75. If #194 or #198 lands before #195, main is merged
again and the seed guard is run.
