# 2026-10-07: S1 batched group seal (OI-1003-Q107, Q112)

Lane: coordinator (sting), serial (the subagent limit is in force until Oct 12).
Branch `feat/s1-group-syncfs-20261007`.

## Why

The S1 sample of record (`docs/evidence/r23-2026-10-07-1410Z-mbp-13-record.md`)
failed initial copy: native 857 ms against rclone 361 ms. WP0(g)'s breakdown
(`docs/evidence/s1-seal-breakdown-2026-10-07.md`) puts 77% of native's wall time
in syncs. The 23 per-file destination `fsync`s alone take about 410 ms.

The microbenchmark (mbp-13, xfs, 23 files, 239.8 MB in total; log
`~/git-bulkload/logs/syncbench-20261007.log` on mbp-13):

| Mode | Time to durable |
| -- | -- |
| fsync each file, rename, fsync the directory (group mode today) | 429–502 ms |
| one `syncfs` | 173–190 ms |
| `sync_file_range(WRITE)` per file, then one `syncfs` | 21–43 ms at the end |

The microbenchmark renamed everything before its single `syncfs`. That
ordering is **unsafe**: a power loss can keep a name whose data it lost. The
built protocol syncs twice.

## What is built

Linux, group mode, groups of 2 or more outputs (`io::durable::batched`):

1. `start_writeback` (`sync_file_range(WRITE)`), as each output is queued
   (`transfer.rs` `publish`). It makes nothing durable, and its errors are
   ignored.
2. `seal_group_data`: one `syncfs` per device the group's files live on. Every
   temporary's data is then durable under its temporary name.
3. The renames.
4. `TouchedDevices::seal(.., batched = true)`: one `syncfs` per touched device,
   which makes the entries durable. The store's device is included.
5. The record commit.

If step 2 fails, nothing has been renamed: each temporary is removed, and each
output of the group carries the refusal. A full disk maps to the typed space
refusal.

Darwin, single-file groups and `--durability=strict` are unchanged. The new
counter pair is `flush_fs_count` / `flush_fs_ns`.

## Proof

- **Crash checker** (`io/crash_check.rs`): a new `SyncKind::FsSync`. Its rule
  1 extension: an `FsSync` on any node makes every earlier mutation on that
  node's drive durable. Three toy tests cover it:
  - `FsSync` before and after the rename passes;
  - `FsSync` only after the rename fails;
  - an `FsSync` on another drive makes nothing durable.
- **Real trace** (`materialize/adoption_power_loss.rs`,
  `a_batched_group_names_no_output_before_its_data_is_durable`): three outputs
  in one `PublishSink` group under the process recorder. `check_view` holds
  over every power-loss state. Two mutation checks:
  - dropping the pre-rename `syncfs` fails, on the 2-`syncfs` count;
  - moving both `syncfs` after the renames fails in the checker: output `a`
    keeps its name with empty data.
- **Model.** The batched protocol is a refinement of `BulkloadTransfer.tla`,
  so no new action was added. The argument is in `docs/formal/README.md`,
  "Seals":
  - each `Publish(s)` needs only `s`'s own seal;
  - the early durability `syncfs` adds is the `persist` crash choice the model
    always offers.

## Merge with main's WP0(d) superseding publish (2026-10-08)

- **Decision: superseding temporaries join `seal_group_data`.** On main a
  superseding temporary is sealed only by `StagedFile::seal` inside
  `prepare_supersede` (per-file `fsync`). In a batched group that would be
  one extra flush per changed seat, so `seal_group_data` now covers
  `Publication::Superseding` too. It counts the file's device in the first
  `syncfs` and marks the temporary `sealed`, so `prepare_supersede` skips
  its own seal. The order holds: device seal, then the intent commit
  (`begin_supersedes`), then `RENAME_EXCHANGE`, then
  `TouchedDevices::seal(store_device, batched)`, then the record commit. No
  name is exchanged or renamed in before its data is durable.
- The model allows it: `BeginSupersede(s)` needs only `dPub[s] = "sealed"`
  (`docs/formal/README.md`, "Seals" bullet extended).
- The seal-failure path (`PublishSink::refuse_unsealed`, extracted to keep
  `commit` under clippy's 100-line limit) matches every variant. A
  superseding item's temporary is discarded with its refusal. No intent was
  recorded and nothing was exchanged.
- `touched.seal(self.store_device, batched)` runs after
  `self.supersede(...)`, so the supersede step's touched directories are
  sealed too.
- The batched power-loss proof now also supersedes one output (`d`) in the
  same group. It asserts: an `Exchange` happened, the trace has no
  per-file `DataSync`/`Fsync`, and the trace has exactly 2 `FsSync`. In
  every power-loss state, `d` holds the old or the new bytes whole, and a
  committed `Output d` never sits beside the old bytes. The pin is still 4
  proofs.

## Not done / open

- **Merge is held** for the adversarial review when reviewers return (after
  Oct 12, Q112).
- An informational re-measure on mbp-13, then a gated sample.
- The rclone-plus-sync equal-durability arm of Q107 (the harness half) is
  its own PR, #209, merging on green under Q110.
- Cost on a busy shared file system: `syncfs` flushes unrelated dirty data
  too. Unmeasured; the rig is quiet by construction.
- **Kernel floor (OI-1003-Q113, ruled 2026-10-07).** Linux before 5.8 does
  not report write-back errors from `syncfs`. The kernel release is read
  once (`uname`), and below 5.8, or when it cannot be read, every group keeps
  the per-file seals. There is no refusal and no flag.
- **Two pins** moved with the new power-loss proof: `just resume-power-loss`
  now expects 4 passing proofs, and `tests/test_ci_contract.py` pins the
  recipe.

Rulings: OI-1003-Q107, OI-1003-Q112, OI-1003-Q110, R-N13.
