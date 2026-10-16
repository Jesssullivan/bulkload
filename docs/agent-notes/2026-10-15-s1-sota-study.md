# 2026-10-15: S1 state-of-the-art study: substrate, placement, topology (OI-1003-Q154)

Lane: S1 SOTA subagent, run by the coordinator's workflow on
`feat/s1-levers-20261015` (S1 throughput `8a24161`, stacked on #210, with
a `git merge origin/main` in progress that this lane does not touch). Only
this note is written and staged in the repository. No engine code changed
and nothing was built in the repository. The rig runs are informational
micro-benchmarks. They are not R23 samples, gate verdicts or S1 results.
S1 stays gated against plain rclone (OI-1003-Q154).

Rulings: OI-1003-Q154 (2026-10-15: the three authorized levers plus this
study), OI-1003-Q155 (publishable quality), R-N57 and R-N63 (the clone fast
path never counts toward a gate), R25 and R-N58, R-N102, WP0(g)
(OI-1003-Q37, Q104), OI-1003-Q107 and Q143 (built: #210's batched seal,
write-back kicks, receive workers, directory batching), OI-1003-Q39 and Q50
(informational runs), R-N13. Mutation receipt: the rig directory
`~/git-bulkload/sota/` on mbp-13 and two logs under `~/git-bulkload/logs/`
(R-N13, OI-1003-Q154 "SOTA study"). Nothing else on mbp-13 was changed.

## 1. Result

On the rig of record (mbp-13), **a fully durable copy of corpus v1 cannot
take less than about 230 to 240 ms**. That is the time to get 240 MB onto
this SSD (about 1.0 GB/s) behind one barrier group. Plain rclone takes
**about 330 ms** and syncs nothing. A buffered copy with no sync takes
48 ms. So rclone's time is its own overhead and never the device's. The
headroom between the durable floor and rclone is **about 90 ms at 1.0x and
about 60 ms at the 0.90x admission margin**. Native B (`8a24161`) is at
555 ms (median of 15 reps on 2026-10-09, under load), so it spends about
**320 ms above the floor**.

The largest lever this study measured is **how many barrier groups lie on
the critical path**. On the rig the same bytes cost 238 ms as 1 group,
319 ms as 4 serial groups, 393 ms as 9 and 539 ms as 23. That is about
14 to 27 ms for each extra group. Native's initial copy ran 10 to 18
durable groups on sting (2026-10-08 trace). Q154 lever (3) covers this.

The other authorized levers are smaller on this rig:

- Each FULL WAL commit costs about 8 ms.
- Verify-once frees about 73 ms of CPU.
- The wire hand-off frees about 60 ms of CPU and about 30 ms of wall time.
- Creating stores in WAL saves about 1.5 ms per store.

Nothing in the substrate list beats the floor: io_uring was not measured,
and O_DIRECT, fallocate and intra-file striping were. The one exception is
the clone. On this xfs, reflink and `copy_file_range` both clone, and make
the copy durable in 15 ms. R-N57 and R-N63 exclude the clone from every
gate, and on XFS it writes to the source inode (S2). For gate (b), compare
multi-stream against the calibrated link ceiling before building it. zstd
level 1 removes 18.6 % of the corpus's wire bytes. QUIC is not a candidate.

## 2. The rig budget (measured, informational)

Rig: mbp-13, MacBookPro12,1, i7-5557U (2 cores, 4 threads), 15.3 GiB,
Linux 6.19.5-11.xr.el10, `/home` on xfs over LVM on `APPLE SSD SM1024G`
(SATA, mq-deadline, volatile write cache: `write_cache` = `write back`).
xfs reflink is enabled: FICLONE succeeds. `vm.dirty_ratio` 40,
`dirty_background_ratio` 10, `dirty_expire_centisecs` 3000. Load1 was 0.06
to 0.76 throughout. Source: a writable copy of sealed corpus v1, 23 files,
239,819,837 bytes, content-identical to the sealed corpus (sha256 over the
sorted per-file sha256 list: `56c71a39…ac02` for both). It is on the same
xfs as the destinations and stays hot in the page cache: every run reads
it first, as r23_ab.py does.

### 2.1 Placement floor: `placebench` (C), 23 files, a fresh destination each run

Each output is written as `NAME.tmp` and renamed. `syncfs2` is #210's
batched order: `syncfs`, renames, `syncfs`. `perfile` is the order before
#210: `fsync` and rename per file, then `fsync` each directory. Times run
from the first `mkdir` to durable. Both series ran at the bench's
background priority (nice 19, ionice idle). Series 1 had 5 reps
(05:53Z). Series 2 had 10 reps and added serial groups (06:11Z). Configs
were interleaved and reversed on alternate reps, with `os.sync()` before
each run. Values are medians in ms, with the range in brackets.

| Config | Series 1 (5 reps) | Series 2 (10 reps) |
|---|---:|---:|
| buffered `pread`/`pwrite`, 1 thread, **no sync** | 48.2 [48–48] | – |
| buffered, 4 threads, no sync | 54.5 [51–59] | – |
| buffered, 4 threads, `fsync` per file (pre-#210 order) | 515.0 [492–551] | – |
| buffered, 1 thread, `syncfs2`, no write-back kick | 306.8 [256–319] | 281.0 [236–349] |
| + `sync_file_range(WRITE)` every 8 MiB (as built in 1a), 1 thread | 275.5 [209–293] | 251.6 [179–293] |
| same, 2 threads (W = 2) | 270.4 [241–293] | 234.6 [169–300] |
| same, 4 threads, a file per job | 267.1 [187–375] | 237.9 [176–286] |
| same, 4 threads, 8 MiB stripes within files | 214.8 [187–252] | 234.4 [174–326] |
| same + `fallocate(KEEP_SIZE)` before writing | 292.9 [235–296] | 239.4 [176–308] |
| `O_DIRECT`, 4 threads, 8 MiB stripes, `syncfs2` | 256.8 [210–287] | 233.0 [178–341] |
| `O_DIRECT`, 1 thread | 284.2 [279–296] | – |
| **serial groups** (4 threads, kicks): 4 groups, each `syncfs`, renames, `syncfs` | – | 318.6 [236–375] |
| serial groups: 9 groups | – | 393.2 [301–437] |
| serial groups: 23 groups (one per file) | – | 538.5 [504–622] |
| `copy_file_range` (clones on this xfs), `syncfs2` | 16.6 [14–21] | – |
| `FICLONE`, `syncfs2` | 14.7 [14–18] | – |
| **rclone 1.74.4** (pinned store path, the bench's flags) | 325.8 [321–330] | 330.5 [309–345] |
| rclone + one `syncfs` after it | 569.3 [568–605] | 582.0 [521–628] |

A third series ran at normal priority (5 reps, key arms only). Every value
fell within the spread above: kicked 4 threads 258.2, stripes 296.2,
`O_DIRECT` 261.4, no kick 339.4, `copy_file_range` 15.4, rclone 335.4 and
rclone + syncfs 554.2.

Reading:

- **Device floor.** No sync takes 48 ms, `syncfs2` with no kick takes 281
  to 307 ms, and the medians with kicks converge at **230 to 240 ms** (the
  best single reps are 165 to 180 ms). The same bytes, waited on by one
  `syncfs` after rclone, take another 230 to 250 ms.
- **Write-back kicks help.** Kicking saves 30 to 45 ms against an
  unkicked `syncfs2`, which confirms OI-1003-Q143's item 1a.
- **Within the noise.** More placement threads, stripes within a file,
  `O_DIRECT` and preallocation all land within ±10 ms of the kicked
  floor. The device is the bound on this rig, not the submission path.
- **Why rclone + syncfs is so slow.** Dirty pages stay below
  `dirty_background_ratio` (10 % of 15.3 GiB ≈ 1.5 GiB), so nothing
  reaches the device during the copy until something forces it. rclone's
  330 ms therefore hold 0 device writes, and its `syncfs` pays the whole
  floor afterwards.
- **Serial barrier groups cost 14 to 27 ms each,** and the cost grows
  sublinearly. Each group drains the queue, then pays two `syncfs`
  (about 9 ms for the first, with almost nothing dirty, and about 4 ms for
  the second; see the clone rows). The 23-group row (539 ms) is
  numerically close to native B (555 ms). That is suggestive, not proof:
  native pipelines its groups against the stream, and this bench does
  not.

### 2.2 rclone's syscalls on this rig (`strace -f -c`, one copy)

| Syscalls | Count |
|---|---:|
| `write` | 7,366 |
| `read` | 468 |
| `futex` | 1,116 |
| `nanosleep` | 1,686 |
| `fallocate(FALLOC_FL_KEEP_SIZE, 0, size)`, one per file | 23 |
| `renameat` (`.partial` to the final name) | 23 |
| `fsync`, `fdatasync`, `syncfs`, `sync_file_range` | 0 |
| `copy_file_range`, `FICLONE` | 0 |

This matches the s1-profile evidence (2026-10-07). rclone's Linux local
backend does not clone: its docs say cloning is "currently only supported
when using APFS on macOS" [R1]. The 256 MiB `--multi-thread-cutoff`
(default, `rclone help flags`) is above every corpus file, so each file is
one stream. rclone does preallocate by default (`--local-no-preallocate`
turns it off [R1]), and our rows show that preallocation gains nothing
here.

### 2.3 Costs of the authorized levers' targets

| Quantity (rig) | Measured | Source |
|---|---|---|
| SQLite WAL commit, `synchronous=FULL` (8 rows) | median 7.7–8.9 ms per commit (5 reps × 12 commits) | `sqlitebench.py` |
| same, `synchronous=NORMAL` | 0.03–0.04 ms | same |
| store create + first commit, rollback journal then WAL | 34.6–40.9 ms (median 36.2) | same |
| store create directly in WAL | 32.7–38.0 ms (median 34.7) | same |
| BLAKE3, 1 thread (`bench micro blake3`, blob-a.bin) | 3.27 GB/s, so **73 ms of CPU per 240 MB** | bench `8a24161` |
| BLAKE3, 4 threads (2 cores, HT) | 7.85 GB/s aggregate, 1.97 GB/s per thread | same |
| FastCDC slice-cut, 1 / 4 threads | 2.29 / 5.19 GB/s | same |
| loopback socket, 256 MiB, default and 4 MiB buffers | 7.45 / 7.66 GB/s; about 68 ms of CPU on both threads per 256 MiB | same |
| zstd -1 of the corpus as one tar stream | 195,261,917 B out (−18.6 %), 269 ms on 1 thread (about 890 MB/s) | `zstd` 1.5.5 (system) |
| zstd -3 | 195,740,712 B, 655 ms: no better than -1 and 2.4× slower | same |
| zstd -1 per file | `archive.log` 0.58, `disk.img` 0.50, `m02.txt` 0.58; `blob-a.bin` and `m01.bin` 1.00 (incompressible) | same |

### 2.4 Gate (b)-shaped probe (sting ← mbp-13, informational)

Path: a direct Tailscale path, RTT 54 to 56 ms. sting's load1 was about 42
on 32 CPUs. mbp-13's SSH server here is Tailscale SSH, not OpenSSH. Each
run moved 8 MiB of `/dev/urandom` to `wc -c`:

| Run | 1 stream (ms) | 4 streams × 2 MiB (ms) |
|---|---:|---:|
| rep 1 | 2,442 | 2,489 |
| rep 2 | 2,290 | 2,203 |

Both arms move about 3.5 MB/s. The path is bound by link capacity, so 4
streams gain nothing. A first attempt at 512 MiB did not finish in 600 s
and was stopped by this lane, which had started it. Neither neo nor the
neo→sting path was measured. This probe says nothing about the LAN case.

## 3. Substrate (study item a)

- **io_uring** [R5, R6, R7, R8, R9]. It batches submissions and
  completions through shared rings, and it can chain operations, for
  example a write followed by `fsync`. Haas and Leis show that a storage
  engine needs asynchronous, batched I/O to reach NVMe arrays' millions of
  IOPS [R7]. Ren and Trivedi find that at low I/O load "all APIs perform
  competitively", and that polling buys 1.7× performance for 2.3× the
  CPU instructions [R9]. bulkload's placement issues about one `pwrite` per
  MiB (about 240 per copy) and a few hundred metadata calls. On this rig
  the placement path takes 48 ms with no sync, against a 230 to 240 ms
  device floor. **Expected gain: under 10 ms.** Not measured. It needs no
  invariant change, but it is a new Linux-only I/O path, and the crash
  checker would have to model linked SQEs.
- **splice, vmsplice, sendfile** [R10, R11, R12]. These move bytes between
  descriptors without user-space copies, and splice needs a pipe at one
  end. bulkload must hash and frame every chunk, so the bytes have to pass
  through user space at least once on the source. In loopback the halves
  share an address space, and handing over an immutable buffer (Q154
  lever 2) removes the copy more simply than splice would. Cross-host, the
  transport is `ssh -T`, whose pipe already belongs to ssh.
- **copy_file_range and FICLONE** [R2, R3, R4, R13]. With both files on
  one filesystem, `vfs_copy_file_range` first tries the filesystem's
  `remap_file_range`, which is a clone. Only when that fails does it fall
  back to an in-kernel splice copy (`fs/read_write.c`, [R13]). On this
  rig's xfs (reflink on), `copy_file_range` is therefore the clone: 15 to
  17 ms durable, the same as FICLONE.
  - GNU cp has defaulted to `--reflink=auto` and used `copy_file_range`
    since coreutils 9.0 (2021-09-24) [R14]. rclone clones on APFS only
    [R1].
  - **Whether bulkload may use them.** For gate (a): no. Gates measure
    "the native engine, running its full protocol over a local socket with
    no clone fast path" (design.md, R-N57 and R-N63).
  - As a product feature for same-filesystem copies, it needs a ruling on
    three points:
    - **S2.** `xfs_reflink_set_inode_flag` sets `XFS_DIFLAG2_REFLINK` on
      the source inode in a logged transaction under `XFS_ILOCK_EXCL`
      [R15]. That is a metadata write to the source, and it makes later
      writes by agents on that file copy-on-write.
    - **Verify.** No bytes are read, so the destination's digest would
      come from trusting the source's capture.
    - **Redundancy.** A media error in a shared extent hits both copies.
  - The neo→sting migration crosses hosts, so a clone is never available
    there.
- **O_DIRECT against buffered with write-behind** [R16, R17, R18].
  - `O_DIRECT` "makes an effort to transfer data synchronously, but does
    not give the guarantees of the O_SYNC flag" [R16]. A barrier is still
    owed after it.
  - `sync_file_range(SYNC_FILE_RANGE_WRITE)` starts write-back without
    waiting, and "None of these operations writes out the file's
    metadata". Its man page
    calls it "extremely dangerous" as a durability primitive [R17].
    bulkload uses it only as a hint and keeps every seal, which is the
    safe use.
  - Measured: `O_DIRECT` lands at the buffered-with-kicks floor (233
    against 234 to 238 ms). It also costs alignment handling and a
    trailing `ftruncate`. **Do not build.**
- **fallocate and preallocation** [R18]. Preallocated space is "unwritten"
  extents, and converting them at write-back costs transactions.
  Measured: neutral to worse (239 against 234 ms; series 1 put it 78 ms
  worse). rclone preallocates by default. **Do not build.**

## 4. The read-to-placement asymmetry (study item b)

- **Why placement dominates.** A hot source reads at memory speed. The
  rig's buffered copy runs at about 5 GB/s, while its SSD sustains about
  1.0 GB/s of durable writes. A durable copy is therefore device-bound
  unless the destination's queue stays full from the first byte to the
  last. Every moment the device idles (a stream ramp, a barrier, a commit,
  a `Held` round trip) adds directly to wall time. The serial-group rows
  measure that cost.
- **Page-cache dirty limits** [R19, R20]. On this rig 240 MB never
  crosses the background threshold (about 1.5 GiB) or the 30 s expiry. So
  without kicks no write-back starts until the barrier, and the barrier
  then pays the whole floor; this is also why rclone + syncfs costs 580 ms.
  Balancing dirty pages through write throttling [R20] matters only above
  the threshold. Kicking every 8 MiB brings the device in early: −30 to
  −45 ms, already built.
- **Parallel placement across and within files.** XFS spreads
  allocation and inode work across allocation groups so that independent
  files can allocate concurrently [R21, R22]. On a single SATA SSD the
  device saturates first: 1, 2 and 4 threads, and stripes within a file,
  all land at 234 to 252 ms. On a multi-device or NVMe destination,
  sting's for example, that can change, as Haas and Leis show for NVMe
  arrays [R7]. **Not a lever on the rig of record.**
- **Write ordering for durability** [R23, R24, R25].
  - Chidambaram et al. separate ordering from durability (`osync` against
    `dsync`) [R23]. The barrier itself is not the cost. The cost is
    letting a barrier stall the next unit of work.
  - ALICE shows that real applications depend on unstated ordering
    between persistence operations [R24]. bulkload's crash checker and
    TLA models (R-N88, R-N119) exist to make that ordering explicit.
  - #210's order (`syncfs` data, rename, `syncfs` entries, then commit)
    is an `osync`/`dsync` split already. The open question is how many
    such pairs land on the critical path.
  - SQLite in WAL mode with `synchronous=FULL` syncs the WAL on every
    commit [R25]. That costs about 8 ms here.
- **Implication.** The floor is 1 barrier group at about 235 ms, against
  rclone's 330 ms (0.71×). Native can pass gate (a) only if everything
  else on its critical path stays within about 60 ms (0.90×) or about
  90 ms (1.0×). That budget must cover setup, the walk ramp, store
  creation (about 35 ms each, both opened in parallel), FULL commits
  (about 8 ms each), barrier pairs (about 13 ms each) and the tail.

## 5. Topology for gate (b) (study item c)

- **What each arm uses.**
  - The native arm is one `ssh -T` stream (gate (b) harness, 2026-10-06).
  - The rclone sftp backend defaults to unlimited connections, a 32 KiB
    chunk and 64 outstanding requests per file, about 2 MiB in flight per
    file [R26]. With the bench's `--transfers 4`, rclone runs at least 4
    SSH connections in parallel.
  - An OpenSSH session channel's default window is
    `CHAN_SES_WINDOW_DEFAULT = 64 × 32 KiB = 2 MiB` [R27]. One stream is
    therefore capped near 2 MiB / RTT: about 2 GB/s at 1 ms, about
    37 MB/s at 56 ms. Each stream also gets one core for its cipher.
- **HPN-SSH** matches the application receive buffer to TCP's and
  parallelizes the cipher. It reports up to 100× over OpenSSH on some
  paths and about 30 % from the parallel ciphers [R28]. It needs a
  different ssh on the receiving host: a host change.
- **Multiple streams** (GridFTP's parallel and striped transfers [R29],
  bbcp [R30], FDT [R31], Facebook WDT [R32], mscp [R33]). They help when a
  single stream is limited by its window, its cipher or loss, and do
  nothing when the link is full. Our WAN probe is link-bound and shows
  1 stream ≈ 4 streams. For neo→sting the harness's calibrated link
  ceiling (design.md W5) decides it. **Measure a single native stream's
  share of that ceiling before building multi-stream.**
- **zstd on the wire** [R34]. Level 1 removes 18.6 % of the corpus's
  bytes at about 890 MB/s per thread, and level 3 gains nothing more. The
  incompressible files (`blob-a.bin` is 38 % of the corpus) should be sent
  raw, per chunk, after a cheap trial. zstd pays only below about
  7 Gbit/s per compressing thread, and it costs source CPU (S2: +2.0
  load1 budget).
- **Congestion control.** BBR models the bottleneck bandwidth and RTT
  instead of reacting to loss [R35, R36], and it helps on long-RTT or
  lossy paths. CUBIC [R37] is the Linux default. Changing it is a host
  setting on the sending side, sshd's socket, and is outside bulkload's
  code. On a LAN it changes little.
- **QUIC** [R38]. Over fast networks, UDP + QUIC + HTTP/3 loses up to
  45.2 % of data rate against TCP + TLS + HTTP/2, and the gap grows with
  bandwidth. The cause is receive-side processing [R39]. bulkload rides
  ssh, and QUIC would mean a new transport and authentication. **Reject.**

## 6. Prior art (study item d)

| System | Mechanism relevant here | Published or measured number |
|---|---|---|
| rsync [R40] | rolling checksum delta, a single stream | Algorithm reference only, no number used |
| rclone 1.74.4 (rig) | buffered copy, preallocates, no sync; clones only on APFS [R1] | **330 ms** for corpus v1 on mbp-13 (this note); 0.85 s user CPU (s1-profile) |
| GNU cp ≥ 9.0 [R14] | `--reflink=auto` by default, `copy_file_range` | Rig, same xfs: the clone path, 15 ms durable |
| casync [R41] | content-defined chunks, a chunk store | Design reference |
| restic [R42] | content-defined chunks, encrypted packs | Design reference |
| FastCDC [R43], BLAKE3 [R44] | bulkload's chunker and hash | Rig: CDC 2.29 GB/s and BLAKE3 3.27 GB/s per thread |
| GridFTP [R29] | parallel TCP streams, striped servers | Design reference (no number taken) |
| bbcp [R30] | multi-stream, peer-to-peer copy | Design reference |
| FDT [R31] | Java, multi-stream, disk-to-disk | Design reference |
| Facebook WDT [R32] | many threads and ports, flash-optimized | "easily saturate a 40 Gbit/s NIC … above 4 Gbytes/sec" |
| mscp [R33] | many SSH (SFTP) connections, files and large-file ranges in parallel | 44 Gbps memory-to-memory locally; −95 % transfer time against scp on long-distance paths |
| HPN-SSH [R28] | larger SSH windows, parallel cipher | up to 100× OpenSSH on some paths; +30 % from the cipher |

## 7. Ranked menu

"Expected" is a saving on mbp-13's initial copy of corpus v1. The source
of each number is given in the evidence column. Authorization is under
OI-1003-Q154. Each item still owes the review and model checks that the
existing durability work had.

| # | Lever | Mechanism | Expected saving on the rig | Evidence | Invariant risk | Authorized? |
|---:|---|---|---|---|---|---|
| 1 | **Fewer barrier groups on the critical path** | Grow durable groups so that one copy runs 1 to 2 `syncfs` pairs instead of 10 to 18. Do not let a group's barrier stall later groups' placement: overlap the barrier with the stream, the `osync`/`dsync` split [R23]. | **−80 to −250 ms.** Serial groups 9→1 save 155 ms, and 23→1 save 300 ms (upper bound). Lower bound: about 13 ms of barrier floor per removed group if native already overlaps. | §2.1 serial-group rows; `durable_groups` 10–18 and `flush_fs_count` 2–8 (2026-10-08 trace) | **R25:** holds in its committed-row reading if `Held` still follows the barrier and the FULL commit (as `MC_wp0g`). **Durability:** a larger group holds `Held` back longer and leaves a larger unrowed window after a crash, which #169's adoption bounds. Directory seals must still precede their records (`MC_neg_commit_before_dirseal`). **S2:** none. | **Yes**, Q154 (3), "batch/relax commits where R25 holds", reviewed against WP0(g). It needs a TLA config with grown groups, and the crash checker over a group of every size. **Done in the build (review, 2026-10-15):** `MC_r25_strict_three` (three seats, so groups of one, two and three commit; strict R25 with a crash), and the power-loss copy checked at every destination group size of its tree, 1 to 7 files (`2026-10-15-s1-levers.md`, lever 3b). Timing-based closes (idle, age) are outside the model: `Commit` may take any nonempty set of ready seats at any step, so every close rule is one of its choices. |
| 2 | **Fewer FULL commits** | Fold the directory records (R-N102, now 1 + 1) and the group records into one FULL commit per barrier group. | **−8 ms per commit** removed from the critical path, up to about −70 ms if the roughly 12 destination commits are serialized | §2.3, 7.7–8.9 ms per FULL commit; 18 FULL commits before batching (2026-10-08 trace) | **R25:** unchanged if each commit still follows its barrier. **R-N102:** a directory record becomes durable later. | **Yes**, Q154 (3). It needs the `DirectoryRecords` model to cover the delay. **Built 2026-10-16 for the completion** (`GroupFinish`; `2026-10-16-s1-fold.md`); the pending record cannot fold, since it must precede the directory's name. |
| 3 | Verify once in loopback | The destination skips re-hashing what the in-process source already hashed. Cross-host keeps verifying. | **0 to −40 ms wall**: 73 ms of CPU at 3.27 GB/s, split over W = 2. It matters most when the 2-core rig runs CDC, BLAKE3 and the copy together. | §2.3 BLAKE3; s1-profile: verify was 85 % of the receive thread | **Verify:** a fault between the source's hash and the destination's write goes unseen in loopback. Adoption after a crash (#169) must still hash the destination's bytes. Needs the mutation test that proves cross-host still verifies. | **Yes**, Q154 (1) |
| 4 | Loosen the in-process wire | Bigger frames and batches, and an immutable buffer handed across (no socket copy) in loopback | **0 to −30 ms wall**, about 60 ms of CPU (socket at 7.5 GB/s, 68 ms of CPU per 256 MiB) | §2.3 socket | Does not affect R25. The handed buffer must stay immutable until the source's hash and the destination's write are both done (it interacts with 3). | **Yes**, Q154 (2) |
| 5 | Create stores directly in WAL | Avoid the rollback-journal commit made before WAL mode is set | **−1.5 to −3 ms per store.** Both stores open in parallel, so about −2 ms of wall time. It also removes 6 fsyncs. | §2.3 sqlite | None. The authority commit stays FULL. | **Yes**, Q154 (3). Low value. |
| 6 | (built) Write-back kicks every 8 MiB | `sync_file_range(WRITE)` as data lands | Already in `8a24161`: −30 to −45 ms against no kick | §2.1 | None; a hint only [R17] | Built (OI-1003-Q143 item 1a) |
| 7 | Do not build: `O_DIRECT`, preallocation, striping within a file, more placement threads, io_uring | – | **±10 ms**, all within noise; io_uring is expected under 10 ms | §2.1; [R9] | A new I/O path for the crash checker to model, for no gain | n/a |
| 8 | Clone fast path (FICLONE, or `copy_file_range` on reflink xfs) | Metadata-only copy | −220 ms (15 ms durable), same filesystem only | §2.1 clone rows | **Gate:** excluded (R-N57, R-N63). **S2:** XFS writes the source inode's reflink flag [R15] and makes later source writes copy-on-write. **Verify:** no bytes read. **Redundancy:** extents are shared. | **Needs a ruling**, as a non-gate product mode only. Never for S1. |
| 9 | Gate (b): several streams per pull | k ssh connections, files striped (as rclone sftp and mscp do) | Unknown for neo→sting. It helps only if one stream sits below the link ceiling. WAN probe: 0. | §2.4; [R26, R27, R33] | `Held` order across streams. S2: more ssh processes on neo (the +2.0 load1 budget). | **Needs a ruling** (W5). Measure one stream's share of the link ceiling first. |
| 10 | Gate (b): zstd level 1, per chunk and adaptive | Compress compressible chunks; send the rest raw | −18.6 % of wire bytes on corpus v1. Worth it below about 7 Gbit/s per thread. | §2.3 zstd; [R34] | Verify stays over the plaintext digest. Decompression is bounded by `MAX_FRAME_BYTES` (8 MiB). S2: source CPU. Wire v7 is a hard cut (R-N59). | **Needs a ruling** (a wire change) |
| 11 | Gate (b): BBR or HPN-SSH on the hosts | Host congestion control or SSH windows | Long-RTT or lossy paths only; about 0 on a LAN | [R28, R35] | None in bulkload; it is a host change | **Needs an operator ruling** (host configuration) |
| 12 | QUIC | – | Negative over fast paths | [R39] | A new transport and authentication | **Reject** |

**Recommended order.**

1. Read `durable_groups`, `flush_fs_count`, `recv_drain_ns` and the
   commit count at `8a24161` on the rig. One informational run is enough,
   and the counters already exist.
2. Build lever 1 with lever 2, because they share the review against
   WP0(g) and the TLA work.
3. Build levers 3 and 4 together. Both are CPU work on the 2-core rig,
   and lever 4's buffer hand-off underlies lever 3's trust in the
   source's hash.
4. Lever 5 can ride along.

Levers 1 and 2 together are the only path the numbers show could put
native below 300 ms. Levers 3 to 5 alone cannot close the 258 ms gap
to 0.9× (555 ms against 297 ms).

## 8. Method and reproducibility

- Rig files (mbp-13, `~/git-bulkload/`), sha256 prefixes:
  - `sota/placebench.c`: `afd4f9deca6712a7`, the final version with serial
    groups. Series 1 ran an earlier build without the group loop; its
    other paths are identical.
  - `sota/placedrive.py`: `8f710a23293ec793`.
  - `sota/sqlitebench.py`: `81d5c3e4409ae805`.
  - `sota/run-sota.sh`: `20dcc72f0cbda9ea`.
  - `sota/run-sota2.sh`: `9f1899a6a007fa20`.
  - Logs: `logs/sota-s1-20261015.log` (`fb5171f9a88c093d`) and
    `logs/sota-s1-20261015-2.log` (`db2320a036357ce0`).
  - `sota/src` is the writable corpus copy, 229 MB, kept for reruns.
    `sota/work` was removed.
- Build: `gcc -O2 -Wall -pthread` (GCC 14.3.1, the Rocky Linux 10.2
  system compiler). Bench binary: `build/bin/bulkload-bench-8a241614ba96`. rclone:
  `/nix/store/v5xbkynmfg8ml23d82m09s802nmj2r6f-rclone-1.74.4`, the flake
  pin (OI-1003-Q105).
- Timings run from the first `mkdir` to the last durability call, in one
  process (`placebench`), or around the subprocess (rclone). Process
  start is included for rclone and excluded for `placebench`, which
  favours rclone by its startup.
- Rerun: `cd ~/git-bulkload/sota && nice -n 19 ionice -c 3 python3
  placedrive.py 10 [config,...]`.
- Not done: page-cache drops (no root; the bench never drops either),
  neo, the neo→sting path, io_uring, NVMe, and off-CPU tracing (no
  privileges).

## 9. Open

- Q154 lever (1) requires a mutation test proving cross-host verifies.
  It is owed when lever 3 is built, not here.
- The bench's `--only rclone` defect (2026-10-08) is still open.
- Linear: the distilled table (§1 and §7) belongs on TIN-4543 as a dated
  comment. It was not posted from this lane.
- Questions for the operator:
  - **Q-a.** Do lever 1's grown groups pass under Q154 (3) with a TLA
    config, or does growing a group change `Held` enough to need its own
    ruling?
  - **Q-b.** Is a non-gate clone mode for same-filesystem copies wanted
    at all, given S2 (the source inode write)?
  - **Q-c.** Should gate (b)'s multi-stream and zstd questions wait for a
    single-stream share of the neo→sting link ceiling?

## References (each checked to resolve on 2026-10-15)

- [R1] rclone, "Local Filesystem" (`--local-no-clone`, `--local-no-preallocate`). <https://rclone.org/local/>
- [R2] copy_file_range(2), Linux man-pages. <https://man7.org/linux/man-pages/man2/copy_file_range.2.html>
- [R3] ioctl_ficlone(2), Linux man-pages. <https://man7.org/linux/man-pages/man2/ioctl_ficlone.2.html>
- [R4] syncfs(2), Linux man-pages (errors are reported since 5.8). <https://man7.org/linux/man-pages/man2/syncfs.2.html>
- [R5] io_uring(7), Linux man-pages. <https://man7.org/linux/man-pages/man7/io_uring.7.html>
- [R6] J. Corbet, "Ringing in a new asynchronous I/O API", LWN, 2019. <https://lwn.net/Articles/776703/>
- [R7] G. Haas, V. Leis, "What Modern NVMe Storage Can Do, and How to Exploit It", PVLDB 16(9), 2023. doi:10.14778/3598581.3598584
- [R8] D. Didona et al., "Understanding modern storage APIs: a systematic study of libaio, SPDK, and io_uring", SYSTOR 2022. doi:10.1145/3534056.3534945
- [R9] Z. Ren, A. Trivedi, "Performance Characterization of Modern Storage Stacks: POSIX I/O, libaio, SPDK, and io_uring", CHEOPS 2023. doi:10.1145/3578353.3589545
- [R10] splice(2). <https://man7.org/linux/man-pages/man2/splice.2.html>
- [R11] vmsplice(2). <https://man7.org/linux/man-pages/man2/vmsplice.2.html>
- [R12] sendfile(2). <https://man7.org/linux/man-pages/man2/sendfile.2.html>
- [R13] Linux, `fs/read_write.c`, `vfs_copy_file_range` (tries `remap_file_range` on the same superblock, then falls back to splice). <https://github.com/torvalds/linux/blob/master/fs/read_write.c>
- [R14] GNU coreutils NEWS, release 9.0 (2021-09-24): "cp now uses --reflink=auto mode by default", and `copy_file_range`. <https://git.savannah.gnu.org/cgit/coreutils.git/tree/NEWS>
- [R15] Linux, `fs/xfs/xfs_reflink.c`, `xfs_reflink_set_inode_flag` (sets `XFS_DIFLAG2_REFLINK` on the source). <https://github.com/torvalds/linux/blob/master/fs/xfs/xfs_reflink.c>
- [R16] open(2), `O_DIRECT`. <https://man7.org/linux/man-pages/man2/open.2.html>
- [R17] sync_file_range(2). <https://man7.org/linux/man-pages/man2/sync_file_range.2.html>
- [R18] fallocate(2). <https://man7.org/linux/man-pages/man2/fallocate.2.html>
- [R19] Linux kernel documentation, "Documentation for /proc/sys/vm/" (`dirty_*`). <https://docs.kernel.org/admin-guide/sysctl/vm.html>
- [R20] J. Corbet, "No-I/O dirty throttling", LWN, 2011. <https://lwn.net/Articles/456904/>
- [R21] A. Sweeney et al., "Scalability in the XFS File System", USENIX ATC 1996. <https://www.usenix.org/legacy/publications/library/proceedings/sd96/sweeney.html>
- [R22] Linux kernel documentation, "The SGI XFS Filesystem". <https://docs.kernel.org/admin-guide/xfs.html>
- [R23] V. Chidambaram, T. S. Pillai, A. C. Arpaci-Dusseau, R. H. Arpaci-Dusseau, "Optimistic Crash Consistency", SOSP 2013. doi:10.1145/2517349.2522726
- [R24] T. S. Pillai et al., "All File Systems Are Not Created Equal: On the Complexity of Crafting Crash-Consistent Applications", OSDI 2014. <https://www.usenix.org/conference/osdi14/technical-sessions/presentation/pillai>
- [R25] SQLite, "Write-Ahead Logging" and "PRAGMA synchronous". <https://www.sqlite.org/wal.html>, <https://www.sqlite.org/pragma.html>
- [R26] rclone, `backend/sftp/sftp.go` (`chunk_size` 32 KiB, `concurrency` 64, `connections` 0 = unlimited). <https://github.com/rclone/rclone/blob/master/backend/sftp/sftp.go>
- [R27] OpenSSH portable, `channels.h` (`CHAN_SES_PACKET_DEFAULT`, `CHAN_SES_WINDOW_DEFAULT`). <https://github.com/openssh/openssh-portable/blob/master/channels.h>
- [R28] HPN-SSH, README. <https://github.com/rapier1/hpn-ssh>; <https://www.psc.edu/hpn-ssh-home/>
- [R29] W. Allcock et al., "The Globus Striped GridFTP Framework and Server", SC 2005. doi:10.1109/SC.2005.72
- [R30] A. Hanushevsky et al., "Peer-to-Peer Computing for Secure High Performance Data Copying" (bbcp), SLAC/OSTI, 2002. doi:10.2172/799054
- [R31] FDT, Fast Data Transfer. <https://github.com/fast-data-transfer/fdt>
- [R32] Facebook WDT, README. <https://github.com/facebook/wdt>
- [R33] R. Nakamura, Y. Kuga, "Multi-threaded scp: Easy and Fast File Transfer over SSH", PEARC 2023. doi:10.1145/3569951.3597582; <https://github.com/upa/mscp>
- [R34] Y. Collet, M. Kucherawy, "Zstandard Compression and the 'application/zstd' Media Type", RFC 8878, 2021. doi:10.17487/RFC8878
- [R35] N. Cardwell, Y. Cheng, C. S. Gunn, S. H. Yeganeh, V. Jacobson, "BBR: Congestion-Based Congestion Control", ACM Queue 14(5), 2016. doi:10.1145/3012426.3022184
- [R36] The same, Communications of the ACM 60(2), 2017. doi:10.1145/3009824
- [R37] L. Xu et al., "CUBIC for Fast and Long-Distance Networks", RFC 9438, 2023. <https://www.rfc-editor.org/rfc/rfc9438>
- [R38] J. Iyengar, M. Thomson (eds.), "QUIC: A UDP-Based Multiplexed and Secure Transport", RFC 9000, 2021. doi:10.17487/RFC9000
- [R39] X. Zhang, S. Jin, Y. He, A. Hassan et al., "QUIC is not Quick Enough over Fast Internet", WWW 2024. doi:10.1145/3589334.3645323
- [R40] A. Tridgell, P. Mackerras, "The rsync algorithm", TR-CS-96-05, ANU, 1996. <https://www.andrew.cmu.edu/course/15-749/READINGS/required/cas/tridgell96.pdf>
- [R41] L. Poettering, "casync — A tool for distributing file system images", 2017. <https://0pointer.net/blog/casync-a-tool-for-distributing-file-system-images.html>
- [R42] restic documentation, "References" (design). <https://restic.readthedocs.io/en/stable/100_references.html>
- [R43] W. Xia et al., "FastCDC: a Fast and Efficient Content-Defined Chunking Approach for Data Deduplication", USENIX ATC 2016. <https://www.usenix.org/conference/atc16/technical-sessions/presentation/xia>
- [R44] J. O'Connor, J.-P. Aumasson, S. Neves, Z. Wilcox-O'Hearn, "BLAKE3" specification. <https://github.com/BLAKE3-team/BLAKE3-specs/blob/master/blake3.pdf>
