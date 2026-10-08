# 2026-10-07: wp0g-ledger-sync (WP0(g), relaxed source-ledger row commits)

Rulings: OI-1003-Q104 (build WP0(g) now, measure first), OI-1003-Q37
(WP0(g) adopted with conditions), OI-1003-Q20 (raised it), OI-1003-Q96 and
OI-1003-Q105 (mbp-13 for measurement only; the flake-pinned rclone),
OI-1003-Q40 (the R25 reading), R25, R-N58, R-N54, R-N81, R-N13.

## Workstreams (as this lane saw them; "reported" is the task brief's word)

| stream | owner | repo/branch | state/evidence | next |
|---|---|---|---|---|
| WP0(g) relaxed source ledger | this lane | `feat/wp0g-ledger-sync-20261007`, stacked on #203 | verified: built, check-fast green, pushed; no PR | coordinator opens the PR against main after #203 merges |
| #203 S3 sniff and supersede | another lane | `fix/s3-sniff-supersede-20261007` at `521f12d` | verified open, no new commit at 14:00Z; reported in its TLA model round | merges before this branch |
| #204 hermetic S1 rig | another lane | `feat/s1-hermetic-rig-20261007` | reported: the 06:52Z sample FAILED S1; the new A pin is its | gated sample, not this lane's |
| #205 git carry L7 | another lane | `feat/q42-l7-manifest-reuse-20261007` | reported open | unknown |
| #197, #202 | merged | main `8006085` | verified merged at 14:00Z | none |

## What was done

1. **Measured first**, at this branch's base `521f12d`, on mbp-13
   (informational): `docs/evidence/s1-seal-breakdown-2026-10-07.md`,
   sections 1 and 2. Native initial copy 851.7 ms median (9 samples)
   against rclone 352.3 ms. Summed sync time is 77 % of wall: 23 file data
   syncs 48 %, directory seals 8 %, state-root seals 1 %, `SQLite` commits
   20 %, of which the source ledger's row commits are 3 % (2 or 3 commits,
   26.6 ms). WP0(g)'s ceiling was stated before the code changed: at most
   3 % summed, about 13 ms on the critical path.
2. **Built WP0(g) as ratified** (commit `db52898`):
   - `LedgerSync` in `crates/bulkload-agent/src/io/durable.rs`, default
     `Relaxed`; `relax_ledger_rows` sets `synchronous=NORMAL`,
     `fullfsync=OFF` on the source publisher's connection and reads the
     settings back, with `checkpoint_fullfsync` still ON.
   - `StorePublisher::relax_ledger_rows` and `LedgerSink::with_sync`
     (`transfer_store.rs`); a destination publisher refuses
     `PROTOCOL_STATE_VIOLATION`. `serve` relaxes only its second
     connection, after `Store::open` committed the authority FULL.
   - #163: `LedgerSink::publish` counts a failed relaxed commit
     (`source_ledger_commit_failed`, `source_ledger_rows_dropped`) and
     returns `Ok`. `ledger_read` (`transfer.rs`) answers a failed ledger
     read as a miss (`source_ledger_unreadable`).
   - Counters `source_ledger_relaxed_commits` and
     `source_ledger_miss_reads`; `full_flushes_total` leaves the relaxed
     commits out.
   - `--source-ledger-sync=relaxed|full` on the agent and on
     `bulkload-bench`; `pull` passes `full` to `serve`.
   - **No destination durability change.** `git diff 521f12d db52898 --
     crates/bulkload-agent/src/materialize.rs` is empty; in
     `transfer_store.rs` and `transfer.rs` no hunk touches
     `configure_sqlite`, `Store::open`, `commit_outputs`, `PublishSink` or
     `Inbound::answer_held`. The one hunk inside `commit_captures`, which
     only a `LedgerSink` calls, adds a counter bump.
3. **Proof.**
   - Model: `LedgerCommit` gains the counted failure under
     `RelaxedSourceLedger`; `ledgerLost` records how a row was lost; new
     rows `MC_wp0g_strict` (pass) and `MC_reach_wp0g_failed_commit`
     (reach). Code sites are cited in the spec header and in
     `docs/formal/README.md`, "WP0(g) as built".
   - Which R25 reading holds: the committed-row reading (OI-1003-Q40) in
     `MC_wp0g` and `MC_wp0g_deep`; the strict reading (#169, within its
     limits) in `MC_wp0g_strict`.
   - Code: P79 RELAXED-LEDGER-LOSS (`transfer/tests/wp0g.rs`, 16 fixed
     seeds) and `tests/power_loss.rs`
     `every_power_loss_state_of_a_relaxed_ledger_costs_at_most_its_lost_seats`.
4. **Measured again** at the final tree (same evidence file, section 3):
   base 880.3 ms, relaxed 880.0 ms, full 880.9 ms (15 samples an arm);
   rclone 345.8 to 358.3 ms. Source row commits 34.9 ms to 0.2 ms summed.
   **WP0(g) did not move the S1 number.** Nothing else was tuned and no
   gated sample was run.
5. **Docs**: `docs/slo.md` (new amendment 2026-10-07), `docs/design.md`,
   the plan's WP1 item 5, P79 in the property-test plan, the formal README.

## Shas

- Base: `521f12d` (`origin/fix/s3-sniff-supersede-20261007`, PR #203, open).
- `db52898da2f1c2628881dac078c9d54180f81299`: the change (signed). Its
  `crates/` tree is `97a96836ce7bba1165734500e86eeec2d6329cae`.
- This note is the commit after it.
- main was `8006085` at 14:00Z (#197 and #202 merged since the base). It
  is not merged here: #203 has not merged, and the brief ties the merge
  of main to that.

## Receipts

- **check-fast** (`nix develop .#default --command just check-fast`, nice
  10, sting, 2026-10-07 14:01Z to 14:09Z, on the tree committed as
  `db52898`): `exit=0`; 34 `test result: ok` lines, 812 passed, 0 failed,
  10 ignored; contract tests 24 OK. One run, no rerun; neither known
  flake (#200, #201) appeared. Log sha256 `2ab64ec7…1cb1b5e` (scratch,
  removed).
- **TLC** (`just tla-check`, sting, tlaplus from the flake's nixpkgs):
  - all of `configs.tsv` and `configs_gc.tsv` with the changed spec,
    13:07Z: every row matched its expectation (1,687 s), before
    `MC_wp0g_strict` existed;
  - the WP0(g) rows again with `MC_wp0g_strict`, 13:53Z to 13:57Z, every
    row as expected:

    | config | expect | outcome | violated | distinct | generated | depth |
    |---|---|---|---|---:|---:|---:|
    | `MC_wp0g` | pass | PASS | – | 945,031 | 2,586,810 | 75 |
    | `MC_wp0g_deep` | pass | PASS | – | 711,661 | 2,776,377 | 49 |
    | `MC_wp0g_strict` | pass | PASS | – | 717,633 | 2,794,980 | 49 |
    | `MC_r25_strict_authority` | pass | PASS | – | 1,188 | 2,303 | 30 |
    | `MC_reach_wp0g_lost_row` | reach | REACHED | `Witness_LostRowRead` | 65,936 | 224,719 | 19 |
    | `MC_reach_wp0g_failed_commit` | reach | REACHED | `Witness_FailedRowRead` | 263,432 | 790,086 | 29 |
    | `MC_wp0g_authority` | fail | FAIL | `R25_NoDurableReread` | 742 | 1,377 | 17 |
    | `MC_store_root_unsealed` | fail | FAIL | `R25_NoDurableReread` | 705 | 1,313 | 18 |
    | `MC_neg_src_ledger_carries_r25` | fail | FAIL | `R25_NoDurableReread` | 3,693 | 9,491 | 15 |

    The relaxed rows' counts grew with the failing branch of
    `LedgerCommit` (`MC_wp0g` was 646,491 distinct); the rows that are not
    relaxed kept theirs (`MC_main` 963,314, `MC_main_deep` 457,464).
  - `just tla-render --check`: all 83 committed files equal the
    catalogue's rendering.
  - The authority mutation of the brief is `MC_wp0g_authority`
    (`RelaxedAuthority = TRUE`): it fails `R25_NoDurableReread`, its named
    property.
- **Mutation** (scratch copy `/srv/cache/jess/wp0g-mut/src`, whose
  `crates/` equals `db52898`'s; never this worktree). Baseline green;
  every mutant red:

  | mutant | red |
  |---|---|
  | M1 the creation commit `synchronous=NORMAL` | P79 (the authority is lost with every row), `only_a_source_ledgers_row_commits_are_relaxed`, `stores_commit_through_wal_with_full_flushes` |
  | M2 a destination publisher may be relaxed | `only_a_source_ledgers_row_commits_are_relaxed` |
  | M3 a failed relaxed commit is fatal | `a_failed_relaxed_ledger_commit_is_counted_not_fatal` |
  | M4 the sink never relaxes | P79, the power-loss test, three store tests |
  | M5 an unreadable ledger is not a miss | `an_unreadable_relaxed_ledger_reads_as_a_miss` |
  | M6 the miss read is not counted | P79 |
  | M7 the authority is minted again on open | P79, the power-loss test, two store tests |
  | M8 relaxing drops `checkpoint_fullfsync` | P79, the power-loss test, four store tests |

- **mbp-13** (OI-1003-Q96): everything under `~/git-bulkload/wp0g/` and
  `~/git-bulkload/logs/wp0g-*`; no sudo; builds at nice 10; nothing
  outside `~/git-bulkload/`. Series `base-1` (12:27Z) and `ab-final`
  (12:48Z to 12:54Z). The `relaxed` and `full` arms ran the binary
  `bulkload-bench-t97a96836ce7b` (sha256 `0da7650e…cd544c0d`), built from
  `521f12d` plus the patch whose `crates/` tree is `db52898`'s. rclone:
  `/nix/store/v5xbkynmfg8ml23d82m09s802nmj2r6f-rclone-1.74.4/bin/rclone`.
  The lane's directory and logs are left there for review.

## Open

For the operator. None of this is built.

1. **What would close the S1 gap**, ranked by the breakdown (evidence
   file, section 4): the 23 per-file data syncs (about 410 ms summed,
   47 %); the 8 directory-record commits (about 80 ms, 9 %); the
   directory seals (about 70 ms, 8 %); fewer, larger destination groups
   (matters only with the first). Each changes when `Held` can be sent,
   so each needs a ruling on R25 and R-N58. rclone syncs nothing on this
   arm.
2. **#163's policy beyond OI-1003-Q37.** Q37 does not mention a failed
   commit; the formal verdict's conditions and #163 do. It is built as
   counted, not fatal, behind `LedgerSync::Relaxed`, and fatal under
   `Full`. Ratify, or say otherwise. #163 can close with this change if
   ratified.
3. **The flag and its default.** `--source-ledger-sync` and `relaxed` as
   the default are this change's reading of Q37 ("may run with"). `pull`
   forwards `full` to the remote `serve`.
4. **A source store that cannot be opened** is still a refusal, not an
   empty ledger: treating it as empty would mint a new authority
   (`MC_wp0g_authority`). Q37's "corrupt or absent ledger is treated as
   empty" is met for the ledger's rows (a failed read is a miss), not for
   a store whose database will not open.
5. **A failed ledger read also hides a remembered refusal** (#186): the
   seat is sniffed again, 16 bytes, counted.
6. **The model's loss is any subset; `SQLite`'s is a suffix.** P79 checks
   the suffix on the real WAL. Torn pages in a checkpointed database, and
   Darwin's reordered writes under `fullfsync=OFF`, are argued from WAL
   checksums and `checkpoint_fullfsync=ON`, and tested only as a garbled
   WAL tail on Linux. No Darwin run was made.
7. **Merge train.** `docs/slo.md` (a new amendment before "Priority"),
   `docs/formal/README.md` (result rows, a new subsection),
   `Catalogue.dhall`, `Types.dhall`, `BulkloadTransfer.tla` and
   `crates/bulkload-bench/src/main.rs` (one CLI field and one call; not
   the preflight) will meet #203's model round and #204. Keep both sides,
   then `just tla-render` and `--check`.
8. **Linear** (TIN-4543, the SSOT ledger) was not updated by this lane.
