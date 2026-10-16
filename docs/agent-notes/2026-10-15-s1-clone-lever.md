# 2026-10-15: the clone lever: semantics, durability, verify, fairness (OI-1003-Q164)

Lane: S1 clone-study subagent, design only, on `docs/s1-clone-study-20261015`
(from `origin/feat/s1-levers-20261015`, `59e5e60`). Only this note is written
and staged. No engine code changed. The rig runs are informational
micro-benchmarks, not R23 samples, gate verdicts or S1 results.

Rulings: OI-1003-Q164 (this study), OI-1003-Q154 and Q155, R-N57 and R-N63
(the clone fast path never counts toward a gate), S2 (OI-1003-Q5, Q9), the
S2 exceptions OI-1003-Q16, Q36 and Q72 and the refusal OI-1003-Q76, R25 and
R-N58 with OI-1003-Q40 and #169, OI-1003-Q105 (the pinned rclone), R-N13.

Mutation receipt (R-N13, OI-1003-Q164). On mbp-13 this lane wrote only under
`~/git-bulkload/clone-study/`: the tool `clonestudy.py`,
`run-clone-study.sh` (`aa22d1f0c8214d7c…`), logs, a plain-copied source
`src/` and the scratch trees `s2-rig-1/` and `stall-rig-1/`. Its bulky
outputs (fresh sources, clone trees and the bench work root) were removed
when the runs ended. The sealed corpus was opened read-only for `FIEMAP`
(no `FIEMAP_FLAG_SYNC`) and nothing else. `sota/src` was read as the copy
source. On sting, scratch files went only under this session's scratchpad
on `/srv/fast-local`. No process was signalled.

- **Tool hash attribution.** `clonestudy.py`'s sha256 `90fbbb7548b21a96…`
  is its final revision, last modified 15:49:54Z. It identifies only the
  code behind `firstB.log` (written 3 s later) and `stall.log`. The s2 run
  (15:46:5xZ), `first.log` (15:47:42Z), `time-bg.log`, `time-normal.log`
  and `native-59e5e60.log` (by 15:48:18Z) were written by earlier,
  unhashed revisions of the same file.
- **Numbers with no rig log.** The §3.1 s2 table, the 8.9 / 0.14 ms dirty
  and clean clone times, the copy-on-write p50/p95, the 1 → 503 extents and
  the "+24 MiB" were printed to the console and never logged on the rig.
  The sting runs were console-only too. `run-clone-study.sh` does not run
  `s2`, `stall` or `firstB`, so it does not reproduce them (§9). Where a
  later log backs one of these claims, the text cites it.
- **Review re-measurement.** The adversarial review of 2026-10-15 wrote only
  `~/git-bulkload/clone-study/review-20261015/`: `review.py` (sha256
  `377b59985c69e42b…`) and `logs/` (`s2.log` `796fd1cc86eda1dc`,
  `durable.log` `b9b0929fe78ec327`, `durable.meta`, `corpus.log`
  `8523175ce1eb6b96`, `s2-rig-1-leftovers.log` `057ab0f33ed22141`), 36 KB,
  and removed its large trees. The correction pass read those logs over
  `ssh` and wrote nothing on the rig.

## 1. Result

- **The clone is not a faster copy. It is a different operation.** On the
  rig, cloning corpus v1 and making it durable takes **15 to 18 ms**
  (`FICLONE` or `copy_file_range`, then #210's `syncfs` order). These are
  lower bounds for a product mode: they leave out the engine's SQLite FULL
  commit (about 8 ms each on this rig [C18]). The kicked durable floor of a
  real copy is 255 to 284 ms by median in this session (10-rep range
  191.8 to 367.9 ms); the SOTA note puts it at about 235 ms [C18]. Plain
  rclone is 329 to 341 ms, and native `59e5e60` is 381 ms in the same
  session. The clone writes no data and reads no source bytes, and it
  gives no second physical copy.
- **It writes to the source, while every stat field stays the same.**
  Measured on the rig (Linux 6.19; the review's `s2.log`, since this
  lane's own s2 run left no log) and on sting (6.12, console only): the
  source's ino, dev, size, blocks, mode, nlink, uid, atime, mtime, ctime,
  fsxattr flags, `GETFLAGS`, inode generation and bytes are identical
  before and after. Only `FIEMAP`'s `shared` count moves, 0 → 1. That
  holds for a first `FICLONE`, a `copy_file_range` and a second `FICLONE`.
  A 4 KiB read as a control did move atime, so the probe sees changes. The
  kernel source shows what does change:
  - XFS sets `XFS_DIFLAG2_REFLINK` on the source inode in a logged
    transaction, and bumps the source's `i_version` [C1, C2]. Nothing
    outside the kernel can see that bump (§3.1);
  - the source's extents gain references in the refcount btree, so
    `FIEMAP` reports them `SHARED` (measured) [C1];
  - the clone flushes the source's dirty pages, and writers to the source
    are blocked for the whole remap, not only the prep (`XFS_IREMAPPING`,
    §3.1) [C1, C3, C4, C5]. Measured once (n=1 per condition): a writer on
    the source stalled **141 ms** while a 128 MiB dirty source was cloned,
    against 0.02 ms at most at baseline;
  - every later write to a shared block is copy-on-write. Measured, but not
    logged on the rig: a 4 KiB overwrite plus `fdatasync` has a p50 of
    **7.9 ms against 3.8 ms** and a p95 of **11.6 ms against 4.0 ms**.
    The file went from 1 extent to 503 (the leftover `P.bin` still has 503
    extents, 252 shared).

  This lane reads that as a source write under S2 as written ("never
  writes to the source"). The OI-1003-Q76 refusal is not settled precedent
  for it: Q76 refused a write that moved a visible ctime, and this one is
  invisible. Whether an invisible inode flag and refcount write counts is
  the operator's reading, folded into §8 Q-a. The S2 property checks and
  the bench's source-row check cannot see it: they compare stat rows and
  bytes, and all of those are unchanged.
- **The sealed corpus v1 on the rig is already reflinked.** All 23 of its
  files share their extents with `~/git-bulkload/sota/src`; each first
  extent sits at the same physical address in both (re-confirmed by the
  review's `corpus.log`, `O_NOATIME`, stat unchanged by the probe). No
  receipt records how `sota/src` was made. It was made by a cloning copy,
  most likely GNU cp 9.5 (the rig's `cp`), which defaults to
  `--reflink=auto` since 9.0 [C15]. The ctimes of the sealed files are
  unchanged (2026-10-07), and the s1-levers series' working copy of them
  verified its content identity at 08:14Z. Still, their inodes now carry
  the reflink flag. That is the S2 effect above, applied to a corpus that
  is meant to be sealed.
  `r23_ab.py`'s working copy (`shutil.copytree`) does not share extents
  (measured), but only because the rig's Python is 3.12.14, whose
  `shutil` copies with `sendfile`. Python 3.14's `shutil` uses
  `copy_file_range` on Linux [C22], which clones on reflink xfs.
- **Verify.** A clone moves no bytes, so "verify every byte" has two honest
  meanings, and each has a price:
  - hash the source, at memory speed while it is hot (37 ms to read it, plus
    BLAKE3 CPU);
  - or hash the clone. That reads the shared blocks from the device:
    **181 to 183 ms** on the rig, about the same as a real durable copy.
  Neither one checks a second physical copy, because no second copy exists.
- **Fairness.** rclone 1.74.4 never clones on Linux. It clones only on
  macOS with cgo, through `copyfile(COPYFILE_CLONE)` [C12, C13]. A native
  clone against rclone on Linux therefore compares a metadata operation
  with a byte copy. On macOS the asymmetry is reversed: rclone as shipped
  clones, as measured on neo in M0 [C17], so a gate (a) field confirmation
  on one APFS volume pits native's copy against rclone's clone.
- **Gate (b) and the migration.** Cloning across hosts or file systems is
  impossible: `FICLONE` and `copy_file_range` both return `EXDEV` (measured
  on sting, xfs to tmpfs). Gate (b) and the neo→sting run gain nothing
  under any ruling.
- **Recommendation:** keep the clone excluded (option A, §7), and add two
  guards: S2's typed source access should forbid remap calls on a source
  descriptor, and rig copies of the sealed corpus should never clone (an
  explicit read/write copy, checked unshared with `FIEMAP`).

## 2. What a gate (a) sample measures (`crates/bulkload-bench`)

- **The timed window** of `run_sample` is `transfer::copy(source,
  destination, …)` for native and an `rclone copy` child for rclone. The
  rclone flags are `--config /dev/null --create-empty-src-dirs --links
  --metadata --transfers 4 --checkers 4`. The rclone arm's
  `sync_after_ms` (one `syncfs` of the destination, OI-1003-Q107) is
  measured after the window closes, outside the timed window; it feeds
  only the informational `rclone_synced` medians.
- **The source is a private fixture,** seeded by a native transfer from the
  corpus root (`seed_fixture`), so it is a real copy and not a clone. It
  sits on the same file system as every destination. On the rig that is
  one xfs with reflink on, so a native clone path would fire there.
- **After each arm, outside the window,** the bench checks:
  - `rows(source) == expected`. The rows are `RowSchema`: dev, ino, size,
    mtime_ns, ctime_ns, mode, nlink and BLAKE3;
  - that the destination matches by path, kind, size, BLAKE3 and mode.

  A clone passes the source check, because no field in it moves (§3.1). So
  the harness cannot tell a clone from a non-mutating copy.
- **The phases are** initial, warm resume, interrupted resume and a 1 %
  delta. The verdict needs native's initial and delta medians to beat
  rclone's, zero source reads on both resumes, and RSS below 2 GiB.
- **The clone exclusion (R-N57, R-N63, design.md "M2 gates") is enforced
  today only because no clone path exists.** No agent source calls
  `FICLONE` or `copy_file_range` directly, and no counter records one.

## 3. Semantics by file system (study item 1)

### 3.1 XFS (the rig: 6.19.5, reflink=1; sting: 6.12, reflink=1 on `/srv/fast-local` and `/home`)

Path, Linux v6.19. `ioctl(FICLONE)` and `copy_file_range` both arrive at
`xfs_file_remap_range` [C4]. `vfs_copy_file_range` calls the
file system's `remap_file_range` first when both files are on one
superblock, and falls back to an in-kernel splice copy only if the remap
fails [C6, `fs/read_write.c` L1552–1640]. On reflink xfs, therefore,
`copy_file_range` *is* a clone, and the caller cannot ask for a byte copy.
That applies to Rust's `std::fs::copy` too, which uses `copy_file_range`
on Linux [C14].

What happens to the **source** inode and file system:

| Step | Code | Effect on the source |
|---|---|---|
| Lock | `xfs_reflink_remap_prep` → `xfs_ilock2_io_mmap` [C1 L1667, C3 L2786]; `XFS_IREMAPPING` [C1 L1722–1724, C4 L217–222, L1846–1849, C3 `xfs_iunlock2_remapping` L2836] | The source's `i_rwsem` (IOLOCK) and invalidate lock (MMAPLOCK) are taken **exclusive**, and pNFS layouts on it are broken (`xfs_iolock_two_inodes_and_break_layout`). After the prep, the source gets `XFS_IREMAPPING` and both locks are demoted to shared. While that flag is set, direct-I/O writers upgrade to the exclusive IOLOCK, write faults take `MMAPLOCK_EXCL`, and buffered writes need the exclusive IOLOCK anyway. The flag is cleared only when the remap completes. So **writers are blocked for the whole remap** (write-back plus one transaction per extent); the demotion only lets readers through. A clean but fragmented source stalls writers too. |
| Flush | `__generic_remap_file_range_prep` → `inode_dio_wait`, `filemap_write_and_wait_range(inode_in…)` [C5 L320–327] | The source's dirty pages in the range are written back, under the exclusive lock. No device cache flush. |
| Flag | `xfs_reflink_set_inode_flag` [C1 L1175–1229] | First clone only: `src->i_diflags2 |= XFS_DIFLAG2_REFLINK`, `xfs_trans_log_inode(tp, src, XFS_ILOG_CORE)`, committed in a transaction. |
| Change counter | `xfs_trans_log_inode` [C2 L86] | With `XFS_ILOG_CORE`, `inode_maybe_inc_iversion(inode, true)` bumps the source's `i_version`. Nothing outside the kernel can see it: `statx` strips `STATX_CHANGE_COOKIE` (kernel-only [C16, C21 L714–715]), and on v6.19 `xfs_vn_getattr` never sets it [C19], so `nfsd4_change_attribute` falls back to ctime [C20 L878–893]. It is a logged write with no externally visible effect. |
| Sharing | `xfs_reflink_remap_extent` [C1 L1313–1520] | One transaction per extent: `xfs_refcount_increase_extent` on the source's blocks and a mapping into the destination. The destination's `i_disk_size` grows extent by extent (L1507), and each extent commits on its own (L1512). |
| Timestamps | `file_modified(file_out)` [C5 L362] | The destination only. No timestamp update of the source in this path. |
| Notify | `fsnotify_access(file_in)` [C5 L409, C6] | An `IN_ACCESS` event on the source, as a read makes. |
| Clear | `xfs_reflink_unshare` → `xfs_reflink_try_clear_inode_flag` [C1 L1827, L1895]; `xfs_itruncate_clear_reflink_flags` [C3 L998–1012] | The flag stays after every clone is deleted. It is cleared by an explicit unshare (`FALLOC_FL_UNSHARE_RANGE`), or when a truncate empties the data fork. Both need write access, so a read-only caller cannot undo it. |

**Measured** (`clonestudy.py s2`, an 8 MiB + 1234 B file at mode 0444, so
the source is read-only like a sealed file, written, `fsync`ed, then left
1.2 s). Identical on the rig and on sting. This lane's runs printed to the
console only (no rig log; see the receipt). The rig record is the review's
`review-20261015/logs/s2.log`: an 8 MiB + 101 B file at 0444 with atime set
so that any read would move it, through `FICLONE`, `copy_file_range`, a
second `FICLONE`, deletion of the clones and a 4 KiB read as a control. It
shows the same result, field for field, plus fsxattr, `GETFLAGS` and
generation:

| Probe | Source stat fields that changed | Source `FIEMAP` |
|---|---|---|
| after the 1st `FICLONE` | none (ino, size, blocks, mode, atime, mtime and ctime all equal) | 1 extent, `SHARED` |
| after the 2nd `FICLONE` | none | `SHARED` |
| after `copy_file_range` | none | `SHARED`, and the destination shares the same physical address |
| after all three clones are deleted and `syncfs` | none | not shared |
| content (sha256, read after everything) | equals the writer's bytes | – |

A read-only mode does not protect a file: a clone needs only `O_RDONLY` on
the source. From the code, not measured (it needs root): the immutable
check in `__generic_remap_file_range_prep` applies to the destination only
(`IS_IMMUTABLE(inode_out)` [C5 L288]), so even a `chattr +i` source can be
cloned and flagged.

**Corpus scale, first clone against later clones.** The record is the
review's interleaved run (`review-20261015/logs/durable.log`, 16:03–16:04Z,
nice 19 / ionice idle, 6 reps, arm order alternated per rep): each rep made
a fresh, never-cloned plain copy of corpus v1 (23 files) and cloned it
durably, against a tree that had already been cloned once.

| Arm, clone + syncfs2 | Median | Range | n |
|---|---:|---:|---:|
| First clone of a never-cloned tree (sets 23 flags) | 14.36 ms | 14.06–18.34 | 6 |
| Tree already cloned once (flags set) | 14.61 ms | 14.34–17.80 | 6 |

On all 23 fresh sources in every rep, `shared` (0 → 1) was the only field
the first clone changed. Shared extents went from 0 to 23. The flag
transactions cost nothing measurable. This lane's own `firstB.log` is
dropped as evidence: no log records how its "never-cloned" trees were
made, and if it reused `first.log`'s trees their flags were already set.

**Source-side costs** (`clonestudy.py s2` and `stall`). Only the stall row
has a rig log (`stall.log`). The other rig cells were console output with
no log; the s2 run's leftover files (review `s2-rig-1-leftovers.log`)
agree with them where they can: `P.bin` 503 extents with 252 shared,
`DP.bin` 252 shared, `F.bin` unshared, `G`/`DG` shared. Every sting cell
is console output only. Each stall cell is one sample (n=1).

| Probe | Rig (6.19, load1 ≈ 0.1) | sting (6.12, load1 ≈ 43) |
|---|---|---|
| Clone of a 32 MiB source with dirty pages / clean (no log) | 8.9 ms / 0.14 ms. The source went from delalloc to allocated, so the clone forced its write-back. | 114 ms / 0.24 ms |
| A writer's longest `pwrite` stall during a clone of a 128 MiB source, dirty / clean (n=1 each) | **141 ms** (the whole clone) / 3.5 ms. Baseline max 0.02 ms. | 421 ms / 97 ms. The writer itself had re-dirtied the "clean" file. |
| 256 × (4 KiB overwrite + `fdatasync`) on a 64 MiB file cloned once, against a never-cloned control, interleaved (no log) | p50 **7.9 / 3.8 ms**, p95 **11.6 / 4.0 ms**. Extents 1 → 503 against 1 → 1. Blocks: the run reported +24 MiB against the control; unverified, with no log. The leftover `P.bin` now has 24 more 512 B blocks than `Q.bin`, i.e. 12 KiB, so the +24 MiB was most likely COW-fork preallocation since reclaimed (a unit error cannot be ruled out). | p50 14.8 / 3.7 ms, p95 22.4 / 6.8 ms. Extents 1 → 503. |
| The clone after the source's overwrites | Unchanged content, 252 extents still shared. COW keeps the copies private [C9]. | same |

Each source block pays the copy-on-write once, on its first write after the
clone. The source's fragmentation stays. The blocks stay shared for as long
as the destination exists, and the destination is the migrated copy, so it
lasts, after bulkload has exited. It matters for sources rewritten in place
(SQLite databases); it largely does not for write-once files
such as git objects.

### 3.2 btrfs (not measured: no btrfs host on hand)

`btrfs_remap_file_range` [C7 L868–930]:

- It takes both inodes' `i_rwsem` exclusive (`lock_two_nondirectories`)
  and both mmap locks for the **whole** clone, not just the prep.
- It calls `filemap_flush` on the **whole** source inode, then waits for
  its ordered extents (L842–849). That is source write-back, as on xfs,
  but for the whole file.
- `btrfs_clone_files` adds extent back-references in the extent tree. The
  inode update with times and `iversion` (`clone_finish_inode_update`, L22)
  is the destination's. Reading the code, no flag is written to the
  source's inode item. That is not measured.
- btrfs checksums data, so a later read of either file *detects* a bad
  shared block. On a single device it cannot repair it.
- Clones across subvolumes of one file system are allowed, though their
  `st_dev` differ. The device-number logic in design.md ("Device numbers
  are `st_dev`…") would see two devices.

### 3.3 ext4 (no reflink)

ext4's `file_operations` define neither `remap_file_range` nor
`copy_file_range` [C8]:

- `FICLONE` returns `EOPNOTSUPP`;
- a same-filesystem `copy_file_range` falls back to `do_splice_direct`
  [C6]: an in-kernel copy through the page cache. It reads the source as
  `read(2)` does, writes nothing to it, and copies every byte to new
  blocks. bulkload could not hash those bytes without reading them. The
  saving is a user-space copy, which the SOTA study measured at about 48 ms
  for the whole buffered copy: it is not a lever.

### 3.4 APFS (neo: `clonefile`, `copyfile(COPYFILE_CLONE)`; not measured, neo is not an authorized rig)

- **The source inode is written.** The Apple File System Reference defines
  `INODE_WAS_EVER_CLONED = 0x400`, "The inode has been cloned at least
  once. If this flag is set, the blocks on disk that store this inode might
  also be in use with another inode" [C11]. Separately, `INODE_WAS_CLONED`
  marks the clone itself.
- Whether a clone moves the source's ctime on APFS is not measured. It is
  the APFS proof obligation in §7.
- `copyfile(…, COPYFILE_CLONE)` is a best try: across volumes it falls
  back to a copy [C13].

### 3.5 Across file systems and hosts

- `FICLONE` checks the superblock and returns `EXDEV` [C5 L384].
- `copy_file_range` crosses file systems only when both sides share one
  `->copy_file_range`, as NFS, CIFS, FUSE and Ceph do. Otherwise it returns
  `EXDEV` (since 5.19) [C6, C10].
- Measured on sting: xfs → tmpfs gives `EXDEV` for both.

What this means for each gate and copy:

- **Gate (b)** is `pull`/`serve` over `ssh -T` between two hosts. There is
  no shared file system, so no clone is possible. The destination hashes
  every received byte (design.md, Wire v6). Gate (b) and the neo→sting
  migration are unaffected by any ruling here.
- **The bundle stage** (design.md: "a full copy, never a clone") is
  consistent with this.
- **Two destination-side `std::fs::copy` calls do clone on xfs/btrfs
  today.** They are `git_carry.rs` L5599 (`prepare_attachment`: bundle →
  receipt) and `git_carry/registered.rs` L208 (staged bundle → receipt).
  Both copy bulkload's own destination files, not source files, so this is
  not S2. It does mean a retained receipt shares media with its input.

## 4. Durability after a clone (study item 2)

**What has to reach stable storage.** A clone copies no data. On xfs,
everything that makes the destination exist is in the log:

- the destination's mapping;
- its size;
- the refcount increments;
- the flags, on a first clone.

The shared data blocks are the source's.

- **When the source was dirty,** the clone wrote its pages back (§3.1), but
  issued no device-cache flush.
- **So the destination is durable once a log force with a pre-flush
  covers its last transaction,** and the new name is durable once its
  directory is sealed.
- **#210's order provides both:**
  - `syncfs` plus an `fsync` of one file (that `fsync` also covers the
    idle-log case of design.md);
  - the no-replace renames;
  - `syncfs` plus a directory `fsync`.
- **The per-file order** (`fsync` each clone, rename, `fsync` its
  directory) does the same 23 times.

**Crash shape.** The remap commits one transaction per extent, and the
destination's size grows with them (§3.1). A crash in the middle of a
clone can therefore leave a temporary holding a prefix of the source. The
existing order stages under a tagged temporary name and renames only after
the first seal, so a partial clone is never published (I2). A
`copy_file_range` remap may also return short (`REMAP_FILE_CAN_SHORTEN`),
so a caller has to loop and check the size.

**Measured on the rig**, 2026-10-15 15:47–15:50Z, corpus v1 content (a
plain copy, `clone-study/src`, 23 files, 239,819,837 B), the source hot,
`os.sync()` before each run, arms interleaved and reversed on alternate
reps. "bg" is nice 19 with ionice idle, as the SOTA series ran; load1 was
0.04 to 0.38 (bg) and 0.38 to 0.63 (normal). Medians in ms, range in
brackets:

| Arm | bg, 10 reps | normal, 5 reps |
|---|---:|---:|
| clone (`FICLONE`), no sync | 2.1 [2.1–2.5] | 2.2 [2.1–2.3] |
| **clone + syncfs2** (#210's order) | **18.2 [14.1–21.6]** | **18.3 [14.6–19.3]** |
| `copy_file_range` (clones here) + syncfs2 | 18.1 [14.1–22.6] | 15.0 [14.5–18.6] |
| clone + per-file `fsync`, rename, directory `fsync` | 184.5 [167.6–204.6] | 180.1 [167.7–183.0] |
| floor: real copy, 4 threads, 8 MiB kicks, syncfs2 (SOTA's `placebench`) | 283.5 [198.3–367.9] | 255.0 [191.8–277.4] |
| rclone 1.74.4, the pinned store path, the bench's flags, no sync | 340.5 [323.3–394.8] | 328.5 [324.5–338.9] |
| native `59e5e60`, `bulkload-bench --only native --reps 5 --informational`, initial copy (background class, same corpus, right after) | 381.1 [345.0–422.5] | – |
| native `59e5e60`, same run, 1 % delta | 118.5 [111.0–145.5] | – |

SOTA's C bench measured clone + syncfs2 at 14.7 ms. This Python driver
agrees within its overhead. The s1-levers series earlier the same day
(`logs/r23-s1lev-20261015-underload-{1..5}.md`, under load, not of record)
put native B at a median 402.9 ms against rclone at 356.8 ms over its five
rounds (1.13×). This session's 381.1 against 340.5 ms is 1.12×.

**Two caveats on these numbers.**

- **The clone rows are lower bounds for a product mode.** They time the
  clone and #210's `syncfs` order only. A product mode would also pay the
  engine's SQLite FULL commit (about 8 ms each [C18]) and the rest of its
  critical path.
- **The floor is not one number.** This session's floor medians are 255.0
  and 283.5 ms, and its 10-rep range is 191.8 to 367.9 ms. The SOTA note
  puts the floor at about 235 ms (1 barrier group) [C18]. Any comparison
  of "clone + V-dest" against "a real durable copy" (§5, §7) depends on
  which floor is taken, and this note does not reconcile the two.

## 5. Verification, R25 and the reuse ledger (study item 3)

What "verify every byte" protects today: the destination checks every
received chunk against the source's digest. Since OI-1003-Q154's hand-off,
in loopback the source's digest of the very buffer handed over is that
check. The check proves that the bytes **written** are the bytes **read**.
A clone writes no bytes, so that property is vacuous. The remaining
failures are:

- (a) the source changing between hashing and cloning;
- (b) the file system mapping the wrong extents;
- (c) media errors.

The options:

| Verify | What it proves | Cost on the rig | Notes |
|---|---|---|---|
| V-src: hash the source (hot), with stat identity checked before the hash and after the clone | The manifest describes the source as it was. With an unchanged identity, the clone has the same content. | A 37 ms read from the page cache, plus BLAKE3 at 3.27 GB/s per thread (SOTA §2.3): about 73 ms of CPU on 1 thread, about 37 ms on 2. Python's blake2b took 379 ms, which is not representative. | The same trust model as lever 1's "verify once" in loopback. It does not detect (b). The racy rule (#86) applies as it does for a read capture, including its blind spot for mmap writes that move no timestamp. |
| V-dest: clone first, then hash the clone | The digest is of exactly what was published. The clone is a point-in-time snapshot (writers to the source are blocked for the whole remap by `XFS_IREMAPPING`, §3.1), so this is *stronger* than a read capture against concurrent writers. | **181 to 183 ms** to read the clone. The blocks come from the device, because page cache is per inode and the hot source's pages do not serve the clone. BLAKE3 overlaps the read. | It catches (b), which needs a kernel bug. 15–18 + 181 ms (a lower bound, without the FULL commit) is close to the floor of a real durable copy (255 to 284 ms by median here, about 235 ms in the SOTA note; §4), so most of the clone's speed is spent again. |
| V-none: trust the remap | Nothing about content. | 0 | It violates the destination-verifies rule outright. |

**Shared extents and later divergence.**

- COW isolates later writes, on either side, from the other file [C9].
  A source write after the clone leaves the destination's bytes alone, as
  measured (§3.1). It shows up as a changed stat identity, as any change
  does.
- **No independent copy exists.** The two files are one physical copy.
- **xfs has no data checksums.** A latent or silent media error in a
  shared block corrupts the source and the destination together,
  undetected.
- **btrfs detects such an error on read, but on a single device cannot
  repair it.**
- **Reclaim** ("reclaim only proven redundant data") frees nothing by
  deleting a cloned source, because the destination keeps the blocks.

**R25 and the ledgers.**

- **R25 itself is safe.** R25 (OI-1003-Q40) forbids re-reading source
  bytes that a committed destination row proves durable. A clone reads 0
  source bytes for the copy, so R25 is unaffected on the copy path.
- **The reuse key survives.**
  - On an unchanged rerun, `Reuse` comes from the destination row, keyed by
    the source's stat identity (device, inode, size, mtime_ns and ctime_ns).
  - On xfs the clone moves none of those (measured, §3.1). An unchanged
    rerun therefore still reads 0 bytes, and S3's unchanged-estate clause
    holds.
  - **This is the load-bearing measurement.** If a clone moved the
    source's ctime, every rerun would see every cloned seat as changed and
    capture it again, which would break S3.
  - APFS is unmeasured, so this is the first APFS proof obligation.
- **Under V-dest, `source_bytes_read` is 0 even on the initial copy.**
  - The source ledger's row (digest-only, R-N58) would then carry a
    manifest computed from the destination, not the source. Its provenance
    is the remap.
  - Both the ledger model and the counters' meaning would have to say so.
  - S3's inequality 1 (source bytes ≤ the sizes of changed seats) holds
    trivially.
- **#169's adoption is unchanged.** It hashes the destination's own bytes
  after a crash. With a clone that is a device read of shared blocks.
- **The delta.** A changed seat would be re-cloned whole, or with
  `FICLONERANGE` per unchanged range, so wire bytes are 0. The superseding
  publish (`RENAME_EXCHANGE`, OI-1003-Q100) is unaffected.

## 6. Fairness of the S1 comparison (study item 4)

- **rclone on Linux does not clone.**
  - The local backend's server-side `Copy` exists only in
    `backend/local/clone_darwin.go`, under build tag `darwin && cgo`
    [C13]. Its docs say cloning is "currently only supported when using
    APFS on macOS" [C12].
  - SOTA's `strace` of a rig copy found 0 `copy_file_range` and 0
    `FICLONE` calls.
  - The relevant flags are `--local-no-clone` (macOS only),
    `--local-no-preallocate`, `--multi-thread-streams`,
    `--multi-thread-cutoff` (256 MiB by default, above every corpus file),
    `--transfers` and `--checkers`. None of them makes rclone reflink on
    Linux.
- **On Linux, a native clone against rclone is not like for like.** The
  clone's win (2 ms, or 15 to 18 ms durable as a lower bound) is the file system's, not
  bulkload's. The like-for-like Linux comparator for a clone mode is
  `cp --reflink=always` plus a sync, not rclone.
- **On macOS the asymmetry runs the other way.** rclone as shipped clones
  (M0 on neo: `isCloned: true`, 1404 ms including its MD5 check, against
  1488 ms with `--local-no-clone`) [C17].
  - A gate (a) field confirmation on neo, with the corpus and the work root
    on one APFS volume, compares native's real copy against rclone's clone.
  - Whether the pinned `aarch64-darwin` rclone (OI-1003-Q105) is built with
    cgo, and so clones at all, is unverified. Its `rclone version` output
    shows it.
  - Under every option in §7, the evidence should record the rclone arm's
    clone state per platform.
- **What R23 would need to say if a clone mode exists.**
  - (i) Every native gate sample asserts `source_remap_calls == 0`, a new
    counter, and is refused otherwise. R-N57/R-N63 would then hold by
    construction, not by absence.
  - (ii) The header prints `clone=off` and `rclone_clone=never|apfs`.
  - (iii) An informational `clone` arm may be reported beside
    `rclone_synced`, never in the verdict.
  - (iv) Whether gate (a) on macOS runs rclone with `--local-no-clone` is
    the operator's to rule (§8 Q-b).

## 7. Ruling options for the operator (study item 6)

| | **A. Keep excluded (recommended)** | **B. Non-gate local fast path** | **C. Amend S2 to permit the reflink metadata touch** |
|---|---|---|---|
| What | No clone in the engine. Record it as a rejected lever with this evidence. | `copy --clone`: an explicit opt-in, off by default, `copy` only (never `serve`/`pull`/`receive`), same file system only. It falls back to the normal stream on `EXDEV`/`EOPNOTSUPP`, typed and counted, and never by silent splice. | A general S2 text change: "never changes source content, stat identity or timestamps; a clone's sharing metadata is a counted exception", like Q36/Q72. |
| S1 | No effect. S1 is still won or lost by levers 1–2 against the real-copy floor (255–284 ms by median here, about 235 ms in the SOTA note; §4). | None. It never counts (R-N57/R-N63). | None by itself. Gates stay clone-free. |
| S2 cost | None | The source writes of §3.1 (flag, `i_version`, refcounts), a writer stall for the whole remap, and the COW tax on later source writes (p95 2.9× on the rig, no log). B needs C's exception, so **B implies a narrow C**. The +25 % p95 S2 budget does not capture these: `s2_budget.py`'s reference workload runs in a sibling directory, never inside the corpus, so it cannot observe the clone's lock stall (one sample, 141 ms) or the later COW cost. The COW cost is also a lasting effect that outlives the run, outside "with bulkload running". It matters for sources rewritten in place (SQLite databases); it largely does not for write-once git objects. | The same, for every caller: a lasting effect on cloned sources that the budget instrument cannot observe. An amendment would have to say so, and measure it with a reference workload that writes into cloned files. |
| Proof obligations | (1) Add "no remap call (`FICLONE`, `FICLONERANGE`, `copy_file_range`) on a source descriptor" to Q16's typed source access, with a property test (strace or a seccomp filter in P-tests). (2) Rig hygiene: copies of the sealed corpus use an explicit read/write copy (never `cp` defaults or `shutil`, since Python 3.14's `shutil` uses `copy_file_range` [C22]), followed by a `FIEMAP` check that the result shares no extents, and every R23 evidence header records the corpus's `FIEMAP`-shared extent count. (3) The operator's call on the rig's already-reflinked corpus (§8 Q-c). | All of A(2), plus: counters `source_remap_calls` and `source_remap_bytes`, and the gate refusal of §6(i). A verify policy is chosen and modelled (V-src with the racy rule, or V-dest with `dest_clone_verify_bytes`). A `Clone` action in `BulkloadTransfer.tla`: destination content = source content at the clone instant, no source read, `R25_NoDurableReread` and `NoClobber` re-checked. The crash checker gains the remap as a write whose content is the source's (a partial clone is never published). P-tests: source rows (ctime_ns included) and bytes identical after a clone on xfs and btrfs, and APFS measured on PZM. The S2 budget is measured with a reference agent writing into cloned files. Reclaim never counts a cloned source as freeable. | All of B's measurements on every supported platform (xfs: this note; btrfs: owed; APFS: ctime and the `INODE_WAS_EVER_CLONED` effect on PZM). A formal statement of which source metadata is permitted, and a counter for it (the flag itself is invisible to userspace, so the count is of remap calls). An explicit treatment of the writer stall: writers are blocked for the whole clone (write-back plus one transaction per extent, so a clean but fragmented source stalls them too); 141 ms for a dirty 128 MiB source, measured once. A dated amendment in docs/slo.md, as Q36/Q72 were. |
| Value | Keeps S2's strongest form. Under this lane's reading, that is the form the Q76 refusal relied on (Q-a). | A same-volume local copy at 15–18 ms durable (about 200 ms with V-dest); both are lower bounds that leave out the FULL commit. That is not the migration, which crosses hosts. | Only what B needs. |

## 8. Open

- **Q-a.** A, B or C? This lane recommends A. B has product value only for
  same-file-system copies, which the neo→sting migration never is. Folded
  into this question: does a clone's invisible source write (the reflink
  flag, the `i_version` bump and the refcounts, with no stat field moved)
  count as "writes to the source" under S2? This lane reads it as yes.
  OI-1003-Q76 is not settled precedent for it, since Q76 refused a write
  that moved a visible ctime. The reading is the operator's to make.
- **Q-b.** On macOS, should gate (a)'s rclone arm run with
  `--local-no-clone`, so that both arms copy bytes? Or should "rclone as
  shipped" stay, with the evidence recording that it cloned? M0 measured
  both.
- **Q-c.** The rig's sealed corpus v1 now shares all 23 extents with
  `sota/src`, and its inodes carry the reflink flag. Accept it as is
  (content identity unchanged, ctime unchanged), or re-seal a fresh,
  unshared copy? A re-seal changes inode and ctime, so the stat-based
  fixture identity starts a new series. This lane changed neither the
  sealed corpus nor `sota/src`.
- **Q-d.** Should the two destination-side `std::fs::copy` calls (§3.5)
  become explicit byte copies, so that a receipt never shares media with
  its input?
- **Owed measurements:** btrfs (no host here); APFS source ctime on a
  clone (PZM); whether the pinned darwin rclone has cgo.
- **Linear:** §1 and §7 belong on TIN-4543 as a dated comment. They were
  not posted from this lane.

## 9. Method and reproducibility

- Tool: `~/git-bulkload/clone-study/clonestudy.py`, Python 3.12.14,
  standard library only. Its final revision (sha256 `90fbbb7548b21a96…`,
  modified 15:49:54Z) is the code behind `firstB.log` and `stall.log`
  only; the other logs came from earlier, unhashed revisions (see the
  receipt). Its subcommands:
  - `fiemap`: read-only;
  - `s2`: §3.1 semantics;
  - `stall`: the writer-stall probe;
  - `mksrc`: a plain `read`/`write` copy, never a clone, verified unshared
    by `FIEMAP`;
  - `first`: first against second clone;
  - `time`: §4.
- The floor arm runs the SOTA lane's `~/git-bulkload/sota/placebench` as
  `rw syncfs2 4 0 0 8`. rclone is
  `/nix/store/v5xbkynmfg8ml23d82m09s802nmj2r6f-rclone-1.74.4`. The bench
  binary is `build/bin/bulkload-bench-59e5e60076da`.
- Logs under `~/git-bulkload/clone-study/logs/` (sha256 prefixes):

  | Log | sha256 prefix |
  |---|---|
  | `run.log` | `b9b3d57fd3f17e5f` |
  | `first.log` (its atime diffs come from the warm-up read; not cited) | `c688fe46e60f4eee` |
  | `firstB.log` (tree provenance unrecorded; not cited, §3.1) | `7f22a4b438ff356f` |
  | `time-bg.log` | `6e8f14c03837966d` |
  | `time-normal.log` | `dbff111021d30ca4` |
  | `native-59e5e60.log` | `64299643ea54858f` |
  | `stall.log` | `552b746f73d0662b` |
- Review logs under `~/git-bulkload/clone-study/review-20261015/logs/`
  (tool `review.py`, sha256 `377b59985c69e42b…`): `s2.log`
  (`796fd1cc86eda1dc`, the §3.1 semantics record), `durable.log`
  (`b9b0929fe78ec327`, the first-clone record), `corpus.log`
  (`8523175ce1eb6b96`, the sealed corpus's sharing),
  `s2-rig-1-leftovers.log` (`057ab0f33ed22141`, the s2 run's leftover
  files).
- No log on the rig: the s2 table, the 8.9 / 0.14 ms clone times, the COW
  p50/p95, 1 → 503 extents and "+24 MiB" (§3.1).
- Rerun: `cd ~/git-bulkload/clone-study && ./run-clone-study.sh`. It needs
  `src`, `fresh` and the work directories absent. It runs only `mksrc`,
  `fiemap`, `first`, `time` and the native bench: it does **not** run
  `s2`, `stall` or `firstB`, so it does not reproduce §3.1's semantics,
  stall or first-clone numbers. Those come from `clonestudy.py s2` /
  `stall` run by hand, and from the review's `review.py`.
- sting: the same tool, with `s2` and `stall` under this session's
  scratchpad on `/srv/fast-local` (xfs, kernel 6.12.0-211.51.1.el10_2,
  load1 ≈ 43), plus the cross-file-system probe to `/dev/shm`. Console
  output only; no log was kept.
- Not done:
  - btrfs, APFS and neo;
  - page-cache drops (no root);
  - `xfs_db` inspection of `di_flags2`. That needs root, so the flag is
    shown from source [C1] and its effects from `FIEMAP` and COW
    behaviour.

## 10. Review corrections applied (2026-10-15)

An adversarial review re-measured the study on mbp-13
(`review-20261015/`, receipt above) and found its main conclusions sound.
Recommendation A is unchanged. This pass changed:

1. **Receipt.** The tool hash identifies only `firstB.log` and
   `stall.log`. The numbers with no rig log are marked, and so is the fact
   that `run-clone-study.sh` does not run `s2`, `stall` or `firstB`. "+24 MiB"
   is marked unverified: the leftover `P.bin` is 12 KiB over its control
   now, most likely reclaimed COW preallocation.
2. **First clone.** It now cites the review's interleaved `durable.log`
   (median 14.36 ms first against 14.61 ms already cloned, n=6 each). The
   `firstB.log` table is dropped, since its trees' provenance is
   unrecorded.
3. **S2 budget.** `s2_budget.py`'s reference workload runs in a sibling
   directory, so it cannot observe the lock stall or the later COW cost.
   That cost is a lasting effect beyond "with bulkload running". It matters
   for sources rewritten in place (SQLite), and largely not for write-once
   git objects. The stall is n=1.
4. **A(2) rig hygiene.** "`cp --reflink=never` or `shutil`" is replaced
   with an explicit read/write copy plus a `FIEMAP` check, because Python
   3.14's `shutil` uses `copy_file_range`.
5. **Lock.** `XFS_IREMAPPING` blocks writers for the whole remap, not only
   the prep. The stall is the whole clone's duration, and it applies to
   clean but fragmented sources too.
6. **Change counter.** The NFS change-attribute claim is removed. On XFS
   v6.19 the `i_version` bump is invisible outside the kernel.
7. **Clear.** A truncate that empties the data fork also clears the flag.
   Added from the code, not measured: `IS_IMMUTABLE` is checked only on the
   destination.
8. **Corpus provenance.** The sealed corpus was "made by a cloning copy,
   most likely GNU cp 9.5". There is no receipt for `cp -a`.
9. **Q76.** The S2 reading is this lane's interpretation, folded into
   Q-a, not settled precedent.
10. **Smaller points.**
    - The floor (255–284 ms by median here, 10-rep range 191.8–367.9 ms)
      is set beside the SOTA note's about 235 ms.
    - The 15–18 ms figures (and about 200 ms with V-dest) are lower bounds
      that leave out the SQLite FULL commit.
    - rclone's `sync_after_ms` falls outside the timed window.

## 11. Follow-up: OI-1003-Q165 and Q166 ruled; Q165 built (2026-10-15)

**Rulings.**

- **OI-1003-Q165.** The clone stays excluded. That answers §8 Q-a with
  option A, and the clone's invisible source write counts as a write under
  S2.
  - (a) S2 gains a typed rule: no clone call on a source file.
  - (b) Rig corpus copies are explicit read/write copies, verified unshared
    by `FIEMAP`; a shared extent refuses the sample.
  - The two destination-side `std::fs::copy` calls may stay (§3.5). This
    answers §8 Q-d for now; the receipt may still share media with its
    input.
- **OI-1003-Q166** answers §8 Q-c. Corpus v1 on mbp-13 is re-sealed as an
  unshared copy:
  - made by an explicit read/write copy;
  - verified to the same content identity and `FIEMAP`-unshared;
  - with the rig's default corpus path pointed at it.

  The old reflinked copy is kept. §8 Q-b (rclone `--local-no-clone` on
  macOS) is still open.

**Built by the follow-up lane** (staged on `docs/s1-clone-study-20261015`,
not committed; docs/slo.md "Amendment 2026-10-15" is the rule text):

- **(a) In the typed source access layer.**
  - `io/mod.rs` states the rule ("Source access never clones").
  - `io::sys::extent_sharing` is a read-only `FS_IOC_FIEMAP` probe, with no
    `FIEMAP_FLAG_SYNC`. `bulkload_agent::extents` exposes it per file and
    per tree.
  - **P84 SOURCE-NO-CLONE** is static, in `tests/source_command_registry.rs`.
    No non-test code in any crate names a remap call (`copy_file_range`,
    `FICLONE`, `FICLONERANGE`, `FIDEDUPERANGE`, `clonefile`/`fclonefileat`,
    `copyfile`/`fcopyfile`, the btrfs clone ioctls) or a clone ioctl number,
    and no `use` hides `fs::copy` or `io::copy`.
  - Every `std::fs::copy` and `std::io::copy` is one of six registered
    sites, each with the reason its input is not a source file. The raw-tree
    capture's `io::copy` reads a source file, so its writer is pinned to
    `git fast-import`'s stdin pipe, for which std uses `splice`.
  - **P84's kernel leg** is in `tests/source_lock_trace.rs`. A self-test
    proves that `FIEMAP` sees a `FICLONE` and a `std::fs::copy` that the
    `lstat` census misses. Then a real `copy` and a `serve` in its own
    process must leave every source extent unshared.
- **(b) The rig.**
  - `r23_corpus.py` gains:
    - `plain_copy_tree`, an `os.read`/`os.write` copy;
    - `tree_sharing`, `FIEMAP` through `fcntl.ioctl`;
    - `require_unshared`;
    - the subcommands `copy` (a read/write, verified, unshared copy, for
      Q166) and `fiemap`;
    - a `seal` that refuses a shared copy.
  - `r23_ab.py`'s working copy uses `plain_copy_tree` and is checked before
    any build: a shared extent refuses with exit 2. A gated sample on Linux
    without `FIEMAP` is refused too.
  - After every rep, `r23_ab.py` checks the working copy and the rep's
    fixture again; a shared extent aborts with exit 3.
  - `bulkload-bench` checks its seeded fixture after the seed and after
    every arm. Its header records `fixture_shared_extents` and
    `sealed_corpus_shared_extents`, and each sample row records
    `source_shared_extents`.
  - `gate_b.py`'s source working copy is a read/write copy in the remote
    helper, and a shared extent refuses SOURCE.

**Failing-first evidence** (sting, `/srv/fast-local` xfs `reflink=1`, kernel
6.12):

- **P84 static, with an empty registry.** It failed and named exactly six
  sites, and no remap call:
  - `io::copy` in `batch_objects::close`, `batch_objects::read_body` and
    twice in `raw_tree::regular`;
  - `fs::copy` in `registered::restore` and `prepare_attachment`.

  It passes with the six registered.
- **Mutations in `transfer::open_source`.**
  - M1, a File-to-File `std::io::copy` of the source: the scan names
    `agent::transfer::open_source`, and the kernel leg fails with 16 of 29
    source extents shared.
  - M2, `std::fs::copy` of the source through `/proc/self/fd`: the scan
    names it, and the kernel leg fails the same way.
- **Mutation M3,** a raw `libc::copy_file_range` in `io::sys`: the scan
  names `sys_posix.rs`.
- **The kernel leg unmutated.** `FICLONE` shared 1 extent and
  `std::fs::copy` 1 more, with the census unchanged. `copy` and `serve`
  over 30 files and 29 extents left 0 shared. Under the default tmpfs
  `TMPDIR`, the leg prints that it did not run and returns.
- **Bench.** With the after-arm check removed, the reflinked-fixture test
  fails because the sample was not refused.
- **Harness tests against HEAD's scripts.**
  - `test_r23_ab.py` adds 5 tests, and all 5 fail on HEAD:
    - the shutil path is used;
    - a shared copy is not refused;
    - no `FIEMAP` is not refused;
    - a clone during a rep is not refused;
    - the real reflinked copy is not refused.

    The pin test also failed there, only because the relocated copy had
    no `flake.lock`.
  - `test_r23_corpus.py` adds 4 tests, which error on HEAD. Its b3sum round
    trip on xfs ran `generate`, `copy`, `seal` and `verify` to the
    committed identity.
  - `test_gate_b.py` adds 1 test, which fails on HEAD: `shutil.copytree`
    is used.

**Receipt (R-N13, OI-1003-Q165).** Sting only:

- edits in this worktree, staged;
- the cargo target `/srv/fast-local/jess/cache/cargo-target/clone`;
- `TMPDIR` `/dev/shm/clone-tmp`;
- reflink scratch under `/srv/fast-local/jess/cache/clone-tests`, removed
  at the end.

No mbp-13 access, no Linear post, and no process signalled.

**Open** (as this lane left them; §12 closes or restates each).

- Q166 was not done by this lane. It was done on mbp-13 afterwards (§12).
- The sota tooling on the rig was not changed by this lane (§12 does it).
- **Darwin has no `FIEMAP`.** The bench and the harnesses record `n/a`
  there, and APFS clone detection is unmeasured.
- **The bench's seed-time check** cannot be mutation-tested without a
  cloning seed; the after-arm check is tested.

## 12. Q166 done, and the review of the Q165 build fixed (2026-10-15)

**Q166 (done on mbp-13 by a rig lane, receipted there).**

- **Paths.** Old: `~/git-bulkload/r23-corpus-v1/corpus`, kept, 23 of 25
  extents shared. New: `~/git-bulkload/r23-corpus-v1-unshared/corpus`.
- **Copy.** Made by `~/git-bulkload/reseal-20261015/reseal_copy.py`
  (sha256 `523a06458c47cbfa73e156fb1adc3736a22bb804d76a93fd39f358fc7a3d55d1`).
  It is an `os.read`/`os.write` copy, not `r23_corpus.py copy`, which was
  not on the rig's checkout. `strace` showed no `copy_file_range`,
  `sendfile`, `splice` or clone ioctl. The copy was sealed 0444/0555 like
  the original.
- **Identity.** `f4a7619f7b88f2e0e1eeadb5995c79a809eafa8b4a8cf3f82d1ff6f29b5b07c7`,
  by `r23_corpus.py verify` at 59e5e60.
- **FIEMAP.** 0 of 25 extents shared, by
  `~/git-bulkload/reseal-20261015/fiemap_check.py` (sha256
  `0ff568b6d8203baed3110586359b07b2b0706de31fec13fde36066412f26af15`).
- **Receipt.** `~/git-bulkload/logs/reseal-corpus-v1-20261015.receipt`,
  sha256 `5e716ee5988a710724c413d6740cf102d4b611790d9ae30e0f4f96f1c8ae1c68`.
- **Default.** The canonical default is now the runbook
  (`docs/plans/2026-10-07-s1-hermetic-rig.md` §2 and §5), which names the
  new path. The four dated `run-*.sh` the rig lane repointed are records of
  past runs, not defaults.

**Review findings and what changed** (staged, with failing-first evidence
on sting `/srv/fast-local`, xfs `reflink=1`, unless noted):

1. **The reflink legs passed without running.** Under the lane's tmpfs
   `TMPDIR`, P84's kernel leg and the reflinked halves of the bench and
   harness tests returned early and still reported `ok`.
   - The kernel leg now falls back to `CARGO_TARGET_TMPDIR` (the build's
     file system), so it ran under `TMPDIR=/dev/shm` here.
   - `just p84-reflink` (on demand, R-N122) sets
     `BULKLOAD_P84_REQUIRE_REFLINK` and requires each leg's own line. It
     runs the bench and harness reflinked halves the same way; the Python
     tests fail instead of skipping under it.
   - It ran here, with all legs. Under a tmpfs `P84_REFLINK_DIR` it fails
     (exit 101). `tests/test_ci_contract.py` pins its lines.
   - Running the reflinked r23_ab half for the first time exposed a stale
     assertion; it is corrected for item 6.
   - Not done: a required CI job. The CI host's file system is unknown,
     and a PR gate needs a ruling.
2. **The static scan missed spellings.** The review's six bypasses (M1–M6)
   now each fail `p84_no_source_file_is_ever_the_input_of_a_clone`; each
   was planted in `extents.rs` and reverted.
   - Numbers match by value, in any radix.
   - `self as` renames and globs of `fs`/`io` are refused.
   - Raw `syscall`s are a registry of two, `SYS_ioprio_set` and
     `SYS_ioprio_get`.
   - The new self-test failed first with no breach found.
3. **The registry pinned text, not types.** `read_body`'s caller is now
   pinned (`p84_the_batch_reader_reads_only_git_s_pipe`). The review's
   `clone_by_registered_site` makes it fail.
   - The kernel leg gained a v1 Git capture and a `SQLite` snapshot. A
     `std::fs::copy` of every seat in `raw_tree::capture` makes the Git leg
     fail with 30 extents shared.
4. **What `FIEMAP` can say.** Both the docs and the code now say "shares
   nothing now", not "never cloned".
   - Sharing is trusted only on xfs, btrfs, bcachefs and ocfs2.
   - ext2/3/4 and tmpfs count 0 by construction.
   - Any other file system is unsupported, never a silent 0. The Rust
     `extents::basis` and the Python `fs_magic` both read it with
     `fstatfs`.
   - Tests failed first: tmpfs was `EOPNOTSUPP`, and a CIFS magic gave a
     number.
   - Not done: tracing arms with `strace`/seccomp. It would perturb the
     timed window, and the static scan is the exact half for bulkload's
     own code.
5. **Unreadable directories.** The Python walks (`tree_sharing`,
   `plain_copy_tree`, gate B's `tree_shared` and `plain_copy`) now raise.
   The tests failed first. Rust already failed closed, and now has a
   test.
6. **Rig sota and diagnostic tooling, and a shared seal.**
   - A shared sealed corpus now refuses a gated sample in the bench and in
     `r23_ab.py`; an informational one records it. Tests failed first.
   - On mbp-13 (receipt `~/git-bulkload/logs/q165b-rig-tooling-20261015.receipt`,
     sha256 `5cc030df4a6ae8b12d673cafc5fc99b1476114033d98baefb4ba83955617456d`):
     - `sota/src-unshared` was made by `q165b-20261015/rigcopy.py` (sha256
       `c263121aed604800…`, a verbatim copy of the staged `r23_corpus.py`
       functions). It has identity `f4a7619f…` and 0 of 23 extents shared.
       `strace` showed no `copy_file_range`, `sendfile` or `splice`. The
       old `sota/src` (23 shared) is kept, unused.
     - `placedrive.py` uses the new copy. It checks before and after every
       sample, and refuses its `cfr` and `clone` configurations (smoke
       logs in `q165b-20261015/logs/`).
     - `diag.sh` and `pin-diag.sh` copy with `rigcopy.py`.
     - All six run and gate scripts refuse a checkout without the Q165 (b)
       copy gate (exit 5). Each original is kept as `*.pre-q165`.
7. **Reconciliation.** The runbook, `docs/slo.md` (b) and (c), the P84
   row and `docs/design.md` now record what was done. The TIN-4543 comment
   is posted (`61ccc412-75c8-42d1-9237-0f623072f3a3`).

**Receipt (R-N13; OI-1003-Q165, Q166).**

- **Sting:** edits in this worktree, staged; the cargo target
  `/srv/fast-local/jess/cache/cargo-target/clone` and its `p84-reflink`
  directory; `/srv/fast-local/jess/cache/clone-tests` as a reflink
  `TMPDIR`.
- **mbp-13:** only what the q165b receipt lists. There was no deletion of
  pre-existing files and no process signalled.

**Still open.**

- A CI job that runs `just p84-reflink` (needs a reflink runner and a
  ruling).
- The rig checkout gets the gate when the branch lands; until then every
  rig run script refuses.
- Darwin and APFS.
- §8 Q-b and Q-d.
- Not this lane's: `transfer::tests::a_copy_moves_the_hand_off_timers` fails
  at HEAD (9806d36, from 59e5e60's loopback verify-once lever:
  `recv_verify_ns=0`), so `just rust-test` is red on this branch for that
  one unit test. Every other group passed.

## References (each fetched or checked on 2026-10-15)

- [C1] Linux v6.19, `fs/xfs/xfs_reflink.c`: `xfs_reflink_set_inode_flag` L1175–1229, `xfs_reflink_remap_extent` L1313–1520, `xfs_reflink_remap_prep` L1652–1730, `xfs_reflink_try_clear_inode_flag` L1827, `xfs_reflink_unshare` L1895. <https://github.com/torvalds/linux/blob/v6.19/fs/xfs/xfs_reflink.c>
- [C2] Linux v6.19, `fs/xfs/libxfs/xfs_trans_inode.c`, `xfs_trans_log_inode` L86 (`inode_maybe_inc_iversion`). <https://github.com/torvalds/linux/blob/v6.19/fs/xfs/libxfs/xfs_trans_inode.c>
- [C3] Linux v6.19, `fs/xfs/xfs_inode.c`: `xfs_itruncate_clear_reflink_flags` L998–1012, `xfs_ilock2_io_mmap` L2786, `xfs_iunlock2_remapping` L2836 (clears `XFS_IREMAPPING`). <https://github.com/torvalds/linux/blob/v6.19/fs/xfs/xfs_inode.c>
- [C4] Linux v6.19, `fs/xfs/xfs_file.c`: IOLOCK upgrade to exclusive while `XFS_IREMAPPING` L217–222, `xfs_file_remap_range` L1529, write fault `MMAPLOCK_EXCL` while `XFS_IREMAPPING` L1846–1849. <https://github.com/torvalds/linux/blob/v6.19/fs/xfs/xfs_file.c>
- [C5] Linux v6.19, `fs/remap_range.c`: `__generic_remap_file_range_prep` L276–365 (`IS_IMMUTABLE(inode_out)` L288), `vfs_clone_file_range` L376–413. <https://github.com/torvalds/linux/blob/v6.19/fs/remap_range.c>
- [C6] Linux v6.19, `fs/read_write.c`: `generic_copy_file_checks` L1481, `vfs_copy_file_range` L1552 (remap first on the same superblock, then splice). <https://github.com/torvalds/linux/blob/v6.19/fs/read_write.c>
- [C7] Linux v6.19, `fs/btrfs/reflink.c`: `clone_finish_inode_update` L22, `btrfs_remap_file_range_prep`, `btrfs_remap_file_range` L868. <https://github.com/torvalds/linux/blob/v6.19/fs/btrfs/reflink.c>
- [C8] Linux v6.19, `fs/ext4/file.c` (`ext4_file_operations` sets neither `remap_file_range` nor `copy_file_range`). <https://github.com/torvalds/linux/blob/v6.19/fs/ext4/file.c>
- [C9] ioctl_ficlone(2), Linux man-pages ("the filesystem must ensure that the changes remain private to the file being written"; `EXDEV`). <https://man7.org/linux/man-pages/man2/ioctl_ficlone.2.html>
- [C10] copy_file_range(2), Linux man-pages ("copy acceleration … reflinks"; `EXDEV` since 5.19). <https://man7.org/linux/man-pages/man2/copy_file_range.2.html>
- [C11] Apple, *Apple File System Reference*, 2020-06-22, `j_inode_flags`: `INODE_WAS_CLONED`, `INODE_WAS_EVER_CLONED`. <https://developer.apple.com/support/downloads/Apple-File-System-Reference.pdf>
- [C12] rclone, "Local Filesystem", `--local-no-clone`, `--local-no-preallocate`. <https://rclone.org/local/>
- [C13] rclone v1.74.4, `backend/local/clone_darwin.go` (`//go:build darwin && cgo`, `apfs.CopyFile(…, COPYFILE_CLONE)`; no Linux counterpart in `backend/local`). <https://github.com/rclone/rclone/blob/v1.74.4/backend/local/clone_darwin.go>
- [C14] Rust standard library, `std::fs::copy`, "Platform-specific behavior" (`copy_file_range(2)` on Linux). <https://doc.rust-lang.org/std/fs/fn.copy.html>
- [C15] GNU coreutils NEWS, 9.0 (2021-09-24): "cp now uses --reflink=auto mode by default". <https://git.savannah.gnu.org/cgit/coreutils.git/tree/NEWS>
- [C16] Linux v6.19, `include/uapi/linux/stat.h` (no change cookie among the exported `STATX_*` masks). <https://github.com/torvalds/linux/blob/v6.19/include/uapi/linux/stat.h>
- [C17] `docs/evidence/m0-2026-09-23.md` (rclone as shipped clones on neo's TinylandState; `--local-no-clone` arm).
- [C18] `docs/agent-notes/2026-10-15-s1-sota-study.md` (floor, about 235 ms; FULL commit, about 8 ms; BLAKE3 and socket costs; rclone `strace`).
- [C19] Linux v6.19, `fs/xfs/xfs_iops.c`, `xfs_vn_getattr` L682 (never sets `STATX_CHANGE_COOKIE`). <https://github.com/torvalds/linux/blob/v6.19/fs/xfs/xfs_iops.c>
- [C20] Linux v6.19, `fs/nfsd/nfsfh.c`, `nfsd4_change_attribute` L878–893 (falls back to ctime without a change cookie). Checked by the review. <https://github.com/torvalds/linux/blob/v6.19/fs/nfsd/nfsfh.c>
- [C21] Linux v6.19, `fs/stat.c` L714–715 ("STATX_CHANGE_COOKIE is kernel-only for now"; stripped from `stx_mask`). <https://github.com/torvalds/linux/blob/v6.19/fs/stat.c>
- [C22] CPython 3.14.7, `Lib/shutil.py` L59 and L137 (`copy_file_range` fast copy on Linux). Checked by the review. <https://github.com/python/cpython/blob/v3.14.7/Lib/shutil.py>
