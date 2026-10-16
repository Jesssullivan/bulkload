# 2026-10-16: the delta lever, a new seat streamed in a local copy (OI-1003-Q154)

Lane: delta-stream, run by the coordinator's workflow in
`bulkload.worktrees/delta-stream-20261016`, branch
`perf/delta-stream-20261016` off `origin/main` `3ea34cc`, rebased onto
`508eda7` (PR #234 merged) for the review round (section 6). Rulings:
OI-1003-Q154 (authorized S1 levers where R25 holds), OI-1003-Q155
(publishable quality), OI-1003-Q173 (merge on an independent review),
OI-1003-Q39 and Q143 (informational before gated), OI-1003-Q18 (WP0(c)),
R25 and R-N58, R-N13. Diagnosis: `2026-10-16-delta-regression.md` on
`docs/evidence-20261016` (PR #234). Nothing was signalled on either host
(R-N11). Nothing was posted to Linear.

Mutation receipt (R-N13): this worktree; build output under
`/srv/fast-local/jess/cache/cargo-target/deltastream/` (logs in `logs/`,
the rig binary in `rig/`); on mbp-13, only
`~/git-bulkload/runs/delta-stream-20261016-tools/` (two binaries, two
scripts, `run-{1,2}.log`) and `~/git-bulkload/runs/delta-stream-20261016-{1,2}/`
(`results.json`, `x.bin`, `y1.bin`). The corpus was read, never written.

## 1. The predicate, from the code

At main, `Inbound::entry` decides `WantManifest` when
`identity.is_some() || fill_locally || salvaged() > 0`, and `fill_locally`
is `store.has_output_hints()`: store-wide. Built (`Inbound::wants_manifest`,
`NewPath`, `Store::holds_output_row`):

- `WantManifest` when a file is at the path, or the sweep salvaged any
  temporary. Salvage stays session-wide: `Salvage` indexes temporaries by
  digest, and a temporary's name does not say which path it was staged
  for, so any salvaged temporary can fill any entry (resume reads 0).
- Else, with no output hint in the store: `Send` (as before).
- Else, without the lever (`pull`, `serve`/`receive`, `copy` with the
  hand-off off, or the lever off): `WantManifest` (as before).
- Else, the lever (a `copy` with the hand-off and
  `BULKLOAD_LOCAL_NEW_PATH_STREAM=1`): `WantManifest` if the destination
  store holds a reuse row under the entry's exact key (or its snapshot
  key), or the source's ledger holds a capture of it that it would offer
  without a read (`ledger_manifest_fits`, shared with the source's
  `manifest_capture`), else `Send`, counted `transfer_new_paths_streamed`.
  The destination opens the source's ledger read-only
  (`Store::open_reader(source_state)`) after `Start`, only with the lever
  on; one it cannot open leaves the session on the store-wide rule, and a
  lookup that fails keeps that entry's manifest.

Why a per-key row lookup, not a per-path hint lookup: hints at a path
outlive its output. The bench's `remove_delta_targets` deletes the old
`archive.log` and leaves its hints, so a per-path hint test would keep the
regression. And a hint at an absent path can fill nothing; only the source
ledger can save a read. The first build assumed the source ledger holds a
row only for a key this destination answered `Held` for. That is wrong
(review round, section 6): the ledger key is `row_key(source authority,
row)`, independent of the destination, so a source state fanned out to two
destinations holds rows the second destination never held. The source
ledger check closes it: the lever keeps every case where the old rule
saved a source read (P79's removed twin, and the fan-out twin: manifest
from the ledger, filled from another output, 0 bytes) and streams only a
seat neither store has carried.

`transfer_manifests_asked` counts every `WantManifest` decision.

## 2. Why the lever ships off (a decision for the operator)

The first build had the lever on by default. `cargo test -p bulkload-agent`
then failed eight properties in `tests/s3_transfer_resume.rs` (log
`agent-test-2.log`): P18 (`p18_wire_bytes_are_the_size_less_verified_survivors`,
`p18_pinned_a_lost_newest_holder_falls_back_to_an_older_one`) and every P21
case over added seats (`p21_added_seats_…`, `p21_changed_seats_…`,
`p21_with_refused_seats_…`, `p21_pinned_an_added_copy_…`,
`p21_pinned_copies_slices_and_twins_…`,
`p21_pinned_a_seat_moved_out_of_a_changed_file_…`). Each is WP0(c)'s
inequality 2 (wire content bytes ≤ absent chunks), measured as
`bytes_received` of `copy`: a streamed added copy counts its handed bytes
whole (for example `bytes_received=660756`, `absent_bytes=352770`).

In a `copy` with the hand-off those bytes never cross the socket, and the
source reads each seat once either way, so whether handed bytes are wire
bytes is a reading of a ruled inequality (OI-1003-Q18), not mine to change.
The lever is opt-in (`BULKLOAD_LOCAL_NEW_PATH_STREAM=1`), every ruled proof
runs unchanged, and docs/slo.md's 2026-10-16 amendment states the
question. If the operator rules that handed bytes are not wire bytes: flip
the default in `local_new_path_stream` and move each clause listed below
to an arm where a new path still asks for its manifest (the transport arm,
`serve` against `receive`, which the harness already has, or the hand-off
off).

The first report named only the eight `s3_transfer_resume` properties; it
missed two (review round, section 6). The full inventory, from
`BULKLOAD_LOCAL_NEW_PATH_STREAM=1 cargo test -p bulkload-agent
--no-fail-fast` on the review-round code (`agent-test-lever-on-inventory.log`:
947 passed, 10 failed), every failure on the `bytes_received` bound:

- the eight P18 and P21 properties above;
- `tests/w3_engine.rs` `w3_engine_properties`, W3's cross-file prefix fill
  (`grown.bytes_received < extended.len()`; passes with the lever on and the
  hand-off off);
- the lib test `transfer::tests::a_lost_output_does_not_lose_reuse_of_its_chunks`
  (#59 review: hints keep every holder, and an older holder serves when the
  newest is gone). With the default flipped, that fallback would run on
  `copy` only through a removed output's manifest, so it moves too. (The
  first build edited this test and restored it to main's text when the
  default went back off.)

## 3. Proofs

- Failing first: on main's code a probe of `a_new_path_in_a_populated_local_destination_is_streamed`'s
  first assertion failed, `source_ledger_miss_reads` 1 against 0
  (`failing-first-main.log`); with the lever off the full test fails the
  same way, 2 against 0 (`lever-tests-knob-off.log`).
- `tests/loopback_handoff.rs` (+4 tests, serial, process counters):
  - lever on, new seats `fresh` and `twin` (a copy of `big`) into a
    populated destination: 0 manifests, 2 streamed, 0 miss reads, each
    read once, 0 destination reuse bytes; the unchanged rerun reads 0
    (S3); `twin`'s output removed by a third party: one manifest, 0 source
    bytes, 0 received;
  - lever on, an existing changed path: one manifest, filled from the old
    output and superseded, read once; rerun 0;
  - the default, the lever off, and the hand-off off: a new path asks for
    its manifest, read once;
  - cross-process (`serve` child, `receive` here: `pull` without ssh): two
    manifests, `twin` filled entirely from the destination's own output,
    `bytes_received` = `fresh`, every received byte hashed.
- Final run (opt-in code): `fmt-clippy-5.log`, `agent-test-3.log`,
  `fault-harness-3.log` (fault_harness, power_loss, io::, P5 alone,
  resume-power-loss, handoff-off), `contract-test-3.log`,
  `tla-check-2.log`, all exit 0: fmt and the three clippy passes (plain,
  `io-trace`, the fault-harness feature union) clean; `cargo test -p
  bulkload-agent` 954 passed, 0 failed (P18 and P21 green, the four new
  tests green); the fault harness green in both hand-off arms
  (`fault_harness` 111, `power_loss` 18, `resume-power-loss` 8 of 8,
  `w3_engine`, `s3_transfer_resume`, `sqlite_carry`); `contract-test`
  green.
- Final run after the review round (rebased on `508eda7`):
  `review-fmt-clippy-2.log` (fmt and the three clippy passes clean),
  `review-agent-test-1.log` (`cargo test -p bulkload-agent` 957 passed, 0
  failed, 7 ignored), `review-fault-harness-1.log` (`just fault-harness`
  exit 0: fault_harness 111, power_loss 18, `io::` 72, P5 alone,
  resume-power-loss 8 of 8; `handoff-off` w3_engine 1, fault_harness 111,
  power_loss 18, s3_transfer_resume 20, sqlite_carry 3, resume-power-loss 8;
  `delta-lever` loopback_handoff 10, estate_converge 23, fault_harness 111,
  power_loss 18, sqlite_carry 3, resume-power-loss 8),
  `review-contract-test-1.log` (24 OK), `review-tla-render-check.log` (257
  files equal). The model rows were not re-run: the `.tla` change is a
  comment, and the lever still only narrows `RecvEntry`'s free choice
  (toward `manifest`).

- Review round additions (section 6), each failing first or under the
  mutation it guards: `a_new_path_the_source_ledger_holds_keeps_its_ledger_manifest`
  (fan-out; failed on the first build's code, `transfer_manifests_asked` 0
  against 1, twin read whole: `failing-first-review-1.log`); the
  cross-process test now runs with the lever on for its destination
  (mutant M1, the lever reaching `pull`, fails it: `mutant-M1.log`);
  lib `levers::the_delta_lever_keeps_the_manifest_when_a_temporary_was_salvaged`
  (M2, salvage skipped under the lever: `mutant-M2.log`); lib
  `sqlite_carry::the_delta_lever_finds_a_snapshot_seats_row_under_its_snapshot_key`
  (M3, snapshot key dropped: `mutant-M3.log`); M4 (ledger check dropped)
  fails the fan-out test (`mutant-M4.log`). The default arm of
  `a_new_path_asks_for_its_manifest_with_the_lever_or_the_hand_off_off`
  follows the variable; its old text failed with it exported
  (`failing-first-env-default.log`). The lib tests read a per-thread tap
  (`NEW_PATH_DECISIONS`, `cfg(test)`), since lib tests share the process
  counters.

## 4. Model

`BulkloadTransfer.tla` `RecvEntry` already chooses freely between `send`
and `manifest` for an absent output, so the lever narrows a modelled
choice: no step, constant or row changes, and the catalogue renders
unchanged (`tla-render --check`, 257 files). The `RecvEntry` comment and
README's "Code and design disagreements" now name `Inbound::wants_manifest`.
Re-run, all as expected: `MC_main`, `MC_r25_strict_main`,
`MC_supersede_main`, `MC_wp0g_strict` PASS; `MC_reach_ledger_manifest`,
`MC_reach_ledger_chunks` REACHED; `MC_neg_reread_durable` fails
`R25_NoDurableReread`, `MC_neg_double_read` fails `ReadOnce`. The model
abstracts chunks, so inequality 2 is not a model property.

## 5. Measured, informational (mbp-13, OI-1003-Q39, not a sample)

`delta_stream_ab_optin.py` with the opt-in build (sha256 `a8f46e59…c744`),
one 33,555,209-byte new file (`r23-corpus-v1/corpus/big/archive.log`,
copied by plain read and write) into a destination seeded outside timing
with one 1 MiB output (so the store has hints), 10 reps per arm, ABBA,
load1 0.0, outputs `cmp`-identical:

| arm | median ms | range | first data ms | stream ms | manifests | streamed | miss reads | source bytes |
|---|---:|---|---:|---:|---:|---:|---:|---:|
| on (`=1`) | 107.6 | 94.3-113.5 | 0.9 | 78.5 | 0 | 1 | 0 | 33,555,209 |
| off (default) | 153.4 | 126.2-161.8 | 51.5 | 123.7 | 1 | 0 | 1 | 33,555,209 |

`run-1.log` (the default-on build, `30ea46c3…`, arms default and `=0`):
110.7 against 153.3 ms, first data 1.0 against 51.0 ms. The off arm sits
above the diagnosis's manifest arm (136.9 ms); read the difference.

## 6. Review round (OI-1003-Q173), 2026-10-16

Two independent reviews of the first build (`80f03d8`, rebased onto
`origin/main` `508eda7` as `f7571c6`). Outcomes:

- **Inventory incomplete** (both reviews): verified, 10 failures not 8
  (section 2). Fixed in docs/slo.md's amendment, design.md and this note.
- **Fan-out source ledger read** (review 1, medium): verified (the new
  test failed on the first build). Fixed in code: the lever also keeps a
  seat's manifest when the source's ledger holds a capture of it
  (`Inbound::source_ledger_holds`, `SourceLedger`, `ledger_manifest_fits`).
  The four false statements (transfer.rs doc, design.md, section 1,
  slo.md) are corrected and the amendment's "one residual" is gone.
- **No pull-ignores-lever proof** (review 1, medium): verified (mutant M1
  survived the committed tests). Fixed: the cross-process test turns the
  lever on for its destination.
- **No lever-on crash coverage; salvage and snapshot-key branches
  unguarded** (review 0 low, review 1 low): verified. Fixed: `just
  delta-lever` in `just fault-harness` (pinned in `tests/test_ci_contract.py`
  with two mutation variants), and the two lib tests above.
- **Default arm reads the environment** (review 0, low): verified
  (failed with the variable exported). Fixed: the default arm follows it.
- **Mid-session arrival refused, not adopted** (review 0, info): verified
  by reading; documented as a residual in slo.md and design.md, no code
  change. Passing `adopt = true` to a lever-streamed seat would trip
  `sqlite_snapshot`'s `!streaming.adopt` guard for a database seat in
  `snapshot` mode, and the same behavior holds for an empty destination on
  main.
- **Counter description** (review 1, low): fixed in `counters.rs`.
- **Formal README map pin** (review 1, low): fixed: the map says which
  names were added on 2026-10-16.

## Open

- Operator: rule whether handed bytes count as wire bytes for WP0(c)
  inequality 2 (section 2). Until then the lever is A/B only.
- Coordinator: carry the informational series to TIN-4543. The series
  ran on 2026-10-16 (`docs/evidence/r23-2026-10-16-mbp-13-s1delta-underload.md`):
  B `19e4b36`, five rounds, with the arm order alternating each round.
  - Delta copy: the median of the round medians is 0.802x plain rclone
    with stream on, against 1.179x with it off. That is at Q143's 0.90x
    margin. Stream on was faster in every paired round, by 34 to 48 ms,
    and first data fell from 52.4 ms to 1.1 ms.
  - Initial copy (fold on, stream on): 1.231x, not at the margin. The
    lever does not act on it.
  - Counters, per delta copy: with the lever on, `transfer_manifests_asked`
    is 0 and `transfer_new_paths_streamed` is 1. With it off, they are 1
    and 0.
- Independent review before merge (OI-1003-Q173). Darwin: no Darwin code
  changed; `just darwin-check` was not run.
