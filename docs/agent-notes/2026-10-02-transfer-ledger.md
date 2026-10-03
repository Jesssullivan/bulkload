# 2026-10-02 — transfer-ledger lane (#86, #87, #97, #100)

Rulings cited: OI-1002-Q24 (bulkload completes before the single migration
run), OI-1002-Q25 (gate plan: these four are pre-migration fixes),
OI-1002-Q29 (operator "ultracode, full steam ahead", 2026-10-02), R-N13.
Engine rulings honoured: R25/R-N58 strict per OI-1001-Q15, R-N76 (racy rule),
R-N86, R-N88, R-N118 (wire v5, no dual stack), R33.

Branch `feat/transfer-ledger-20261002` from `origin/main` 84242b8.

## What changed

- **#86 racy-capture guard.** `capture_file` reads the wall clock before it
  opens a seat and after the final stat check, and applies the Git carry's
  own predicate (`git_carry::racy`, 2 s allowance, future stamps racy). A
  racy capture is sent and published but never recorded: no source ledger
  row, and `End{racy: true}` makes the destination delete or skip the output
  row under the row key (chunk hints are still kept). Wire change inside v5:
  `End` gained `racy`, `WIRE_SCHEMA` is dated 2026-10-02 and the `wire_id`
  pin moved (070bb548…); both peers must run the same build, as for every
  schema change. New counter `transfer_racy_captures`.
- **#87 strict decode.** `bulkload_proto::frame::decode_exact` uses
  `take_from_bytes` and refuses a remainder (`FRAME_CODEC`). Control frame
  bodies and source ledger manifests decode through it (a padded ledger row
  is a miss). Output and directory records were already compared raw or
  decoded exactly.
- **#97 salvage.** Salvaged orphans are removed at the end of every session
  that reaches `finish_receive`, refusals or not. Salvage only saves wire
  bytes since captures commit after `Held`; `keep_salvaged` and the
  `destination_refused` plumbing are gone.
- **#100 Held{false} fault test.** A keyed test hook fails the destination
  store commit with `ENOSPC`; the group answers `Held{false}` for each entry,
  no ledger or output row commits, the session ends with
  `DESTINATION_SPACE_INSUFFICIENT` per entry, the next run reads each file
  once and a warm run reads 0. `ENOSPC` from a seal or a `SQLite` full disk
  is now mapped to that typed code in the destination group commit.
- Fixtures written just before a copy are settled past the racy window with
  `transfer::settle_racy_window` (bench fixture outside timing, w3_engine,
  power_loss, fault harness). Unit tests read a clock one hour ahead unless
  a test pins it per source root.

## Validation

`nix develop .#default --command just check-fast` green (fmt, clippy -D
warnings incl. feature passes, workspace tests, fault harness 56/56,
power_loss 5/5, resume-power-loss 2/2, contract tests); `just
resume-power-loss` green standalone.

## Open / known limits

- Ledger and output rows written before this change are not proven
  non-racy; pre-migration state only (OI-1002-Q24), so no invalidation pass
  was added.
- A true same-tick rewrite cannot be produced in a test (ctime moves); the
  test pins the capture clock inside the tick and asserts no row is kept.
- `estate.rs` sidecar reads, `git_carry` row/custody decodes and
  `shallow.rs` still use `postcard::from_bytes`; they are corpus and Git
  carry records, outside the transfer wire/ledger scope of #87.
- The fault harness gains about 2 s per populated fixture for the settle.
