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
- **Kernel floor (OI-1003-Q113, ruled 2026-10-07, set 5.8; moved to
  5.17-rc3 by OI-1003-Q141, ruled 2026-10-08 (Linear TIN-4543 comment
  7734678e), on upstream evidence; the review lane could not find the ruling
  because it was posted minutes before the lane started).** The
  kernel release is read once
  (`uname`), and below the floor, or when it cannot be read, every group
  keeps the per-file seals. There is no refusal and no flag.
- **Two pins** moved with the new power-loss proof: `just resume-power-loss`
  now expects 4 passing proofs, and `tests/test_ci_contract.py` pins the
  recipe.

## Adversarial review fixes (2026-10-08)

The adversarial review of #210 (two refute lanes, seven findings, all
confirmed real by a fix pass that changed nothing) is now addressed in the
worktree. The changes are staged and not committed; the merge stays held
under Q112.

1. **Each file's own write-back error (findings 1 and 2).** `syncfs`
   checks write-back errors against the cursor of the one descriptor it is
   called on. An error that an earlier `syncfs` elsewhere already consumed
   can therefore hide another member's lost data.
   - `seal_group_data` now runs `io::durable::check_writeback` on every
     member after the device seals: staged, superseding and adopted files.
     The call is `sync_file_range(WAIT_BEFORE|WRITE|WAIT_AFTER)`. It returns
     `file_check_and_advance_wb_err` against that file's own descriptor and
     sends no cache flush.
   - One failure refuses the whole group through `refuse_unsealed`, before
     any rename.
   - Test hook: `fail_writeback_of(Some((dev, ino)))`.
   - Test: `a_write_back_error_of_a_later_member_refuses_the_batched_group`.
     Three outputs; the second member fails. All three are refused with
     `EIO`, nothing is left in the destination, and the store has 0
     outputs.
   - Verified upstream (v5.17): `sync_file_range` calls
     `file_fdatawait_range`, which returns
     `file_check_and_advance_wb_err(file)`. The errseq conversion is
     6454568d961b.
2. **File-system allowlist (findings 3 and 5).** `io::durable::syncfs_seals`
   allows xfs, btrfs and tmpfs. It allows the ext magic 0xEF53 only when the
   mount's type in `/proc/self/mountinfo` is `ext4` or `ext3`: the ext2
   driver shares the magic, and its `ext2_sync_fs` returns 0 with no device
   flush (checked at v6.12).
   - Excluded: FUSE except virtiofs (`fuse_sync_fs` returns 0 unless
     `fc->sync_fs`; checked at v6.12), CIFS/SMB, NFS, 9p, vfat, exFAT and
     overlayfs. Also f2fs: `f2fs_sync_fs` returns 0 when checkpointing is
     disabled or has failed (checked at v6.12).
   - Including tmpfs is justified because nothing on it is durable either
     way: its `fsync` is a no-op too.
   - `PublishSink::devices_batch` runs `fstatfs` on every member file and
     its parent directory and caches the answer per `st_dev` for the sink's
     life. If any device is off the list, or any `fstat` fails, the whole
     group keeps the per-file path.
   - `design.md`'s "at least as strong" sentence is replaced by the three
     bounds. The `crash_check.rs` rule 1, the `trace.rs` `FsSync` doc, the
     `sys_linux::sync_fs` doc and `docs/formal/README.md` "Seals" are scoped
     to the allowlist.
   - Tests: `a_group_off_the_syncfs_allowlist_keeps_the_per_file_seals`
     (forced FUSE: 0 device seals; forced tmpfs: 2) and
     `the_syncfs_allowlist_names_ext4_xfs_btrfs_and_tmpfs_only` (the table
     and the mountinfo parser).
3. **Kernel floor 5.17 (finding 3 of lane 1; OI-1003-Q141).** `SYNCFS_REPORTS_ERRORS_SINCE = (5, 17)`.
   - Verified against upstream with the GitHub API: torvalds/linux
     5679897eb104cec9e99609c3f045a0c20603da4c, "vfs: make sync_filesystem
     return errors from ->sync_fs", Darrick J. Wong, 2022-01-30. It is not
     in v5.17-rc1. It is first in v5.17-rc3 and is in v5.17.
   - The commit is cited in `durable.rs` and `design.md`.
   - A distribution backport below 5.17 (for example RHEL 9's 5.14) is not
     detected and keeps the per-file path.
   - `the_syncfs_floor_reads_the_release_major_and_minor` now pins (5, 17),
     with 5.8, 5.10, 5.14, 5.15 and 5.16 below the floor.
4. **A directory whose `fstat` fails (finding 6).** `TouchedDevices` keeps
   such a directory in `unstated`, and a batched `seal` gives it its own
   `seal_dir`.
   - Test: `a_touched_directory_without_a_device_is_sealed_in_a_batched_group`.
5. **No vacuous batched proof (finding 7).**
   `a_batched_group_names_no_output_before_its_data_is_durable` is now
   `#[cfg(target_os = "linux")]`.
   - On Linux it asserts `batched()` instead of returning, so a kernel below
     the floor fails it.
   - It forces the tmpfs magic around the recorded commit, so the scratch
     directory's file system cannot switch the proof off. The 2-`FsSync`
     assertion still checks that the group was batched.
   - `just resume-power-loss` derives its expected count per platform: 4
     proofs on Linux, 3 elsewhere. `tests/test_ci_contract.py` is re-pinned
     to match.

Failing-first runs, before the fixes were wired in:

- The floor test failed on the (5, 17) pin.
- The write-back test failed: 2 device seals, not 1, and the records
  committed.
- The allowlist test failed with `[2, 2]` against `[0, 2]`.
- The directory test failed with `Ok(0)` against `Err(Io(Some(5)))`.

All of these pass after the fixes.

Results (sting, Linux 6.12, xfs; TMPDIR on tmpfs):

| Check | Result |
| -- | -- |
| fmt | clean |
| clippy, three ways (default, io-trace, fault-injection+io-trace) | clean |
| lib `materialize durable crash_check` | 58 passed |
| full lib | 554 passed, 5 ignored |
| `w3_engine` | 1 passed |
| `power_loss` | 12 passed |
| lib `io::` (fault build) | 68 passed |
| `just resume-power-loss` | 4 passed, exit 0 |
| `tests/test_ci_contract.py` | 24 OK |
| `just python-lint` (after `ruff format` of the new pin line) | clean |
| shellcheck of the rendered `resume-power-loss` body | clean |

Not verified here:

- The Darwin build. The Linux-only tests, the proof's constants and the
  `SyncKind` import are now cfg-gated so that Darwin clippy has no dead
  code, but no Darwin compile ran in this pass.
- An end-to-end run on a real FUSE, CIFS or ext2 mount. The allowlist is
  exercised through the `fstatfs` override hook.
- An informational S1 re-measure. The extra per-file `sync_file_range` wait
  runs after the data is already written, so it should cost little, but
  that is unmeasured.

## Second review round fixes (2026-10-08)

Eight findings on the staged fixes; all verified. Staged, not committed;
the merge stays held under Q112.

1. **xfs idle log (major).** Confirmed at v6.12: `sync_filesystem` sends no
   flush of its own (`sync_blockdev` writes and waits only),
   `xfs_fs_sync_fs` is `xfs_log_force(SYNC)`, and `xfs_log_force` returns 0
   with no I/O when the head iclog is active-and-empty or dirty and the
   previous one is too. `xfs_file_fsync` sends `blkdev_issue_flush` when
   its log force was a no-op (not for a realtime inode).
   - `sys::sync_fs` is now `syncfs`, then `fsync` of the same descriptor;
     the `FsSync` event is recorded after both, with the `fsync` traced just
     before it. Both steps of a batched group get it (one member file per
     device number; one touched directory per device number).
   - xfs realtime: `syncfs_seals_handle` reads `FS_IOC_FSGETXATTR` on every
     xfs handle; `FS_XFLAG_REALTIME`, or a failed ioctl, keeps the group on
     the per-file path.
   - Tests: `an_adopted_only_batched_group_flushes_its_device_before_it_commits`
     (io-trace; forced xfs; failed first: no member flush before the
     commit) and `a_realtime_xfs_member_keeps_the_per_file_seals` (failed
     first: `[2, 2]` against `[0, 2]`). The batched proof's "no flush of its
     own" check now allows only each seal's own `fsync`, immediately
     followed by that seal's `FsSync` of the same node.
2. **Recycled device number (minor).** The sink-lifetime `fs_batches` cache
   is gone; every handle is `fstatfs`ed for every group, and
   `/proc/self/mountinfo` is read at most once per group, for the ext magic
   only. Test: `a_device_number_reused_by_another_file_system_is_read_again`
   (failed first: the cached tmpfs answer batched the "FUSE" device).
3. **btrfs subvolumes (minor, perf).** Confirmed by construction: each
   subvolume has its own `st_dev`, and both steps deduplicate by `st_dev`.
   Not fixed in code: deduplication by the btrfs fsid
   (`BTRFS_IOC_FS_INFO`) cannot be exercised here (no btrfs on sting). The
   docs no longer say "one syncfs per device", name the per-subvolume cost,
   and claim no S1 gain for a multi-subvolume btrfs destination. Follow-up:
   dedupe by fsid and benchmark on a multi-subvolume btrfs.
4. **5.17-rc1/rc2 (minor) and 7 (same, plus xfs commit).** Confirmed with
   the GitHub compare API: 5679897eb104 and 2d86293c7075 are both
   "diverged" from v5.17-rc2 and "behind" v5.17-rc3. New
   `release_meets_floor` reads an `rcN` after `-` or `.` for 5.17 exactly.
   The floor test lists `5.17.0-rc1`, `5.17.0-rc2` and Fedora's
   `5.17.0-0.rc2...` as below (failed first on `5.17.0-rc1`).
   2d86293c7075 is cited in `durable.rs`, `sys_linux.rs` and `design.md`.
5. **ext2 exclusion untested in the running code (major).**
   `syncfs_seals_device` is replaced by
   `syncfs_seals_handle(handle, &OnceCell<Option<String>>)`, which takes the
   mountinfo text. Tests: `the_ext_magic_off_an_ext4_mount_keeps_the_per_file_seals`
   (forced ext magic on the scratch directory: 0 device seals) and
   `the_ext_magic_reads_its_mount_type_by_major_and_minor` (synthetic
   ext4/ext3/ext2/absent/swapped lines keyed to the real `st_dev`). Both
   pass on the fixed code; the reviewer's mutant M3 fails both, and a
   major/minor swap fails the second ("an ext4 mount of this device
   batches").
6. **OI-1003-Q141 citation (major in review).** The reviewer could not find
   the ruling and the citation was removed; the coordinator confirmed it
   (Linear TIN-4543 comment 7734678e, 2026-10-08 20:53Z) and restored it in
   `durable.rs`, `design.md` and this note, beside the upstream commits.
8. **Over-claiming docs (minor).** Confirmed: `ext4_sync_fs` returns 0 on a
   forced shutdown at v5.17 and v6.12; `ext4_sync_file` returns `EIO`. The
   seal's `fsync` (item 1) now reports it, so it is fixed in code as well
   as reworded. "Reports a failure of any of it" and "at least as strong"
   are gone from `design.md`, `durable.rs` and `materialize.rs`.

Results of the rerun (sting, Linux 6.12, xfs; TMPDIR on tmpfs):

| Check | Result |
| -- | -- |
| fmt | clean |
| clippy, three ways (default, io-trace, fault-injection+io-trace) | clean |
| lib `materialize durable crash_check` | 62 passed |
| full lib | 558 passed, 5 ignored |
| `w3_engine` | 1 passed |
| `power_loss` | 12 passed |
| lib `io::` (fault build) | 68 passed, 2 ignored |
| lib io-trace `an_adopted_only_batched_group_...` | 1 passed |
| `just resume-power-loss` | 4 passed, exit 0 |
| `tests/test_ci_contract.py` | 24 OK |
| `just python-lint` | clean |
| shellcheck of the rendered `resume-power-loss` body | clean |

Not re-audited: btrfs NOCOW overwrites under an idle transaction (the
seal's `fsync` should cover them as `btrfs_sync_file` does for the
per-file path, unverified). No Darwin compile. No S1 re-measure of the
extra `fsync` per device per step.

Rulings: OI-1003-Q107, OI-1003-Q112, OI-1003-Q110, OI-1003-Q113, R-N13
OI-1003-Q141.
