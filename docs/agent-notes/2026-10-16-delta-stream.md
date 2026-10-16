# 2026-10-16: the delta lever, a new seat streamed in a local copy (OI-1003-Q154)

Lane: delta-stream, run by the coordinator's workflow in
`bulkload.worktrees/delta-stream-20261016`, branch
`perf/delta-stream-20261016` off `origin/main` `3ea34cc`. Rulings:
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
  key), else `Send`, counted `transfer_new_paths_streamed`.

Why a per-key row lookup, not a per-path hint lookup: hints at a path
outlive its output. The bench's `remove_delta_targets` deletes the old
`archive.log` and leaves its hints, so a per-path hint test would keep the
regression. And a hint at an absent path can fill nothing; only the source
ledger can save a read, and it holds a row only for an exact key this
destination answered `Held` for, which follows the reuse row here. So the
per-key row test keeps every case where the old rule saved a source read
(P79's removed twin: manifest from the ledger, filled from another output,
0 bytes) and streams every seat the destination never held.

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
the default in `local_new_path_stream` and run inequality 2's added-seat
clauses on the transport arm (`serve` against `receive`, which the harness
already has).

One earlier test was edited and then restored to main's text once the
default flipped (`a_lost_output_does_not_lose_reuse_of_its_chunks`).

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

## Open

- Operator: rule whether handed bytes count as wire bytes for WP0(c)
  inequality 2 (section 2). Until then the lever is A/B only.
- Coordinator: an informational `r23_ab.py --under-load` series with
  `BULKLOAD_LOCAL_NEW_PATH_STREAM=1` for B (Q143), then carry this to
  TIN-4543. The bench's per-sample counters carry the two new counters.
- Independent review before merge (OI-1003-Q173). Darwin: no Darwin code
  changed; `just darwin-check` was not run.
