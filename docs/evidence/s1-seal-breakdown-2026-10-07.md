# S1 seal breakdown and WP0(g) before/after: 2026-10-07 (INFORMATIONAL)

> **Informational only. Not a gate sample.** Every run here passed
> `--informational` to the bench, ran outside the gated harness
> (`r23_ab.py` was not used), and has no A control. Nothing in this file is
> an R23 verdict. The gate of record is the gated B/A/B/A/B sample
> (OI-1002-Q30); its new A pin is another lane's.

Rulings: OI-1003-Q104 (build WP0(g) now, measure first), OI-1003-Q96
(mbp-13, measurement only), OI-1003-Q105 (flake-pinned rclone), OI-1003-Q20
and OI-1003-Q37 (WP0(g)), R25, R-N58, R-N81 (power and load recorded),
R-N13.

## Why this file exists

The first gated gate (a) sample on the hermetic rig
(`docs/evidence/r23-2026-10-07-0652Z-no-a-control.md`, on PR #204's branch)
failed: native initial copy 873 ms against rclone 362 ms. The operator ruled
that the first response is to build WP0(g), the relaxed source ledger
(OI-1003-Q104), and to measure first where the time goes. This file is that
measurement, taken before any code changed, and the same measurement after.

## Rig and method

- Host `mbp-13`: Linux 6.19.5-11.xr.el10 x86_64, `MacBookPro12,1`,
  Intel i7-5557U, 4 cores, 15.3 GiB, work root on `xfs`
  (`/dev/mapper/rl-home`), AC power. The rig of the 06:52Z sample.
- Corpus: R23 corpus v1, a private copy of the sealed
  `~/git-bulkload/r23-corpus-v1/corpus` (`cp -a`, then `chmod -R u+w`), 23
  regular files, 239,819,837 bytes. `r23_corpus.py verify` on the copy:
  identity `f4a7619f7b88f2e0e1eeadb5995c79a809eafa8b4a8cf3f82d1ff6f29b5b07c7`,
  `manifest_match=True`.
- rclone: `/nix/store/v5xbkynmfg8ml23d82m09s802nmj2r6f-rclone-1.74.4/bin/rclone`
  (v1.74.4, the flake's pin, the binary the 06:52Z sample used).
- Bench flags, the 06:52Z sample's plus `--informational`:
  `bulkload-bench --corpus-root <copy> --work-root <new> --rclone <pin>
  --reps 3 --revision <sha> --informational`. Defaults: `durability=group`,
  `priority=background`. One invocation is three native initial samples
  alternating with two rclone ones, then warm resume, the 1 % delta and
  the interrupted resume.
- Builds: `cargo build --release --locked -p bulkload-bench` inside
  `nix develop .#default`, at nice 10.
- The page cache is never dropped; every timed arm starts source-hot, as in
  the gated harness.
- Every sample row carries the bench's own `power=` and `load1=`. The
  series script waits for a 1-minute load under 1 before each invocation.
  Another lane may use this host; load is recorded, not controlled.
- Everything ran under `~/git-bulkload/wp0g/` on the host. Raw bench output:
  `~/git-bulkload/logs/wp0g-*` there.

All counters below are the bench's own (`native_counters`,
`native_timing`). They are sums over worker threads, so they overlap in
time with each other and with the transfer, and their sum is not a share of
wall time on its own. "Share" below means summed counter time over the
median wall time, an upper bound on what removing that work could save.

## 1. Where the time goes (before, at this branch's base `521f12d`)

Series `base-1`, 2026-10-07 12:27Z to 12:28Z, three invocations, nine native
initial samples; load1 0.83 to 1.19, AC.

| | median | min | max |
|---|---:|---:|---:|
| native initial wall, ms | 851.7 | 794.8 | 1,372.7 |
| rclone initial wall, ms (6 samples) | 352.3 | 337.9 | 372.6 |
| native 1 % delta wall, ms | 140.5 | 130.5 | 155.2 |
| rclone 1 % delta wall, ms | 104.9 | 101.1 | 108.7 |
| native warm resume wall, ms (0 source bytes) | 4.2 | 3.8 | 6.6 |

The base reproduces the 06:52Z sample (873 ms against 362 ms). The 1,372.7
ms sample ran at load1 1.19 with 946 ms of file syncs; it is in the median.

### Sync calls of one native initial copy, by side

The bench's `copy` runs both halves in one process, so the counters are
split by what each call site can only belong to. On Linux a file seal and a
directory seal are each one `fsync` (`io::durable::seal_file`, `seal_dir`);
a `synchronous=FULL` `SQLite` commit is one sync of the WAL. `SQLite`'s own
syscalls are not in the io trace, so the "one sync a commit" line is
`SQLite`'s documented behaviour, not a traced count.

| call | side | count | summed ms (median) | share of wall |
|---|---|---:|---:|---:|
| file data sync (`flush_barrier`, `fsync` of each output) | destination | 23 | 412.4 | 48.4 % |
| directory seal (`flush_dir_barrier`) | destination 18, plus one state-root parent a store | 20 | 69.6 | 8.2 % |
| state-root seal (`flush_dir`, #161) | one a store | 2 | 7.5 | 0.9 % |
| `SQLite` commits, all | both | 15 | 166.9 | 19.6 % |
| of which source ledger ROW commits (`sqlite_group_source_commits`, timed by `native_timing sqlite_commit_ns`) | source | 2 (3 in 4 of 9 samples) | 26.6 | 3.1 % |
| of which everything else: 2 store creations (one a store), 3 destination output groups, 4 + 4 directory records | destination 12, source 1 | 13 | 126.9 | 14.9 % |
| all seals (`flush_*_ns`) | | 45 | 490.4 | 57.6 % |
| all seals and commits | | 60 | 656.4 | 77.2 % |

The source writes no file content (the digest-only ledger, R-N58), so it
makes no file data sync at all. Its whole sync budget in one initial copy
is: one parent-directory seal and one state-root flush (#161), one creation
commit (schema and authority), and the ledger's row commits. On this rig a
`synchronous=FULL` commit costs 8 to 13 ms.

The other counters, for scale (worker sums, they overlap): chunk and hash
1,104.8 ms over the capture workers, queue wait 563.9 ms, transfer 196.8
ms, reuse census 63.3 ms, walk 0.1 ms. Peak RSS 26 MiB.

### The 1 % delta and the resumes

- 1 % delta (one 33.6 MB file changed): one file sync 42.0 ms, one
  directory seal 3.8 ms, one destination commit 8.5 ms, and **no source
  ledger commit**: the bench changes the file just before the copy, so its
  capture is racy and is never recorded (#86). WP0(g) cannot affect this
  phase.
- Warm resume and interrupted resume: 0 source bytes, no source ledger
  commit (nothing new is held). WP0(g) cannot affect them either.

## 2. What WP0(g) can and cannot affect

WP0(g) relaxes the SOURCE ledger's ROW commits, and nothing else
(OI-1003-Q37).

- **Can affect: 26.6 ms of summed commit time a copy (3.1 % of the median
  wall), in 2 or 3 commits.** That is the ceiling. The ledger commits on
  its own thread while the transfer runs; the session waits for it once,
  at `committer.sync()` before `SourceDone`. So the wall-clock effect is at
  most the last commit (about 13 ms, 1.6 %), and less whenever the
  destination's own last group is still committing at that moment.
- **Cannot affect: everything else, 629.8 ms of summed sync time (74 % of
  wall).** Every file data sync, every directory seal, every destination
  commit, the two creation commits and the state-root seals are unchanged
  by ruling: the destination's durable rows carry R25 and its `Held` comes
  after its commit (R-N58), and the authority's commit stays FULL.
- The gap to rclone is 499 ms. rclone syncs nothing: its 352 ms is the copy
  into the page cache. WP0(g) could close at most 5 % of that gap.

So the expectation, before building it: WP0(g) is correct to build as
ruled, and it will not move the S1 number visibly.

## 3. After: WP0(g) built

Series `ab-final`, 2026-10-07 12:48Z to 12:54Z: five rounds, each round one
invocation of three arms in the order base, relaxed, full; 15 native
initial samples and 10 rclone initial samples an arm; load1 0.76 to 1.75,
AC. Same host, corpus copy, rclone and flags as section 1.

- **base**: this branch's base `521f12d`, the binary of section 1.
- **relaxed**: `521f12d` plus this lane's change, the default
  (`LedgerSync::Relaxed`). The binary was built from a tree whose `crates/`
  git tree is `97a96836ce7b`, the `crates/` tree of this lane's commit (the
  agent note records the sha and the check); the bench's `--revision` is
  `t97a96836ce7b`.
- **full**: the same binary with `--source-ledger-sync=full`, the code
  path before WP0(g) in the new build.

| | base | relaxed (WP0(g)) | full |
|---|---:|---:|---:|
| native initial wall, median ms | 880.3 | 880.0 | 880.9 |
| min to max | 787.3 to 1,363.9 | 768.5 to 1,853.4 | 811.2 to 1,336.8 |
| rclone initial wall, median ms (reference) | 345.8 | 358.3 | 357.8 |
| native 1 % delta wall, median ms | 148.6 | 134.3 | 139.9 |
| rclone 1 % delta wall, median ms | 105.5 | 104.1 | 104.3 |
| native warm resume, median ms (0 source bytes) | 4.0 | 4.1 | 4.2 |
| native interrupted resume, median ms (5 samples; 0 source bytes) | 68.2 | 63.1 | 62.8 |
| peak RSS, median MiB | 26.2 | 26.9 | 26.1 |

**WP0(g) did not move the S1 number.** The three medians are within 1 ms
of each other, and each arm's own spread is several hundred ms. Native
initial copy is still about 2.5 times rclone's.

The breakdown of the initial copy, medians of 15 samples (summed counter
time, as in section 1):

| | base | relaxed | full |
|---|---:|---:|---:|
| source ledger row commits, count (min to max) | 2 (2 to 4) | 3 (2 to 5) | 3 (1 to 3) |
| source ledger row commits, summed ms | 34.9 | **0.2** | 37.9 |
| `source_ledger_relaxed_commits` | 0 | 3 (equal to the row commits in every sample) | 0 |
| `source_ledger_commit_failed`, `source_ledger_rows_dropped` | 0 | 0 | 0 |
| all `SQLite` commits, summed ms | 168.3 | 131.6 | 172.7 |
| of which not source rows (creation, destination groups, directories) | 127.8 | 131.3 | 130.4 |
| file data syncs, 23, summed ms | 406.8 | 440.3 | 411.1 |
| directory seals (`flush_dir_barrier`), summed ms | 68.6 | 70.2 | 72.8 |
| state-root seals (`flush_dir`), 2, summed ms | 7.7 | 7.6 | 7.7 |
| all seals (`flush_*_ns`), summed ms | 485.6 | 520.4 | 491.6 |
| all seals and commits, share of the median wall | 74.3 % | 74.1 % | 75.4 % |

What changed is what the ruling said would: a source row commit went from
8 to 30 ms (a WAL sync) to about 0.1 ms (no sync), and nothing else moved
outside its noise. The ledger commits on its own thread, so the 35 ms it
no longer spends was never on the session's critical path except for the
last commit before `SourceDone`.

The 1 % delta and both resumes make no source ledger commit in any arm
(section 1), so their differences between arms (148.6, 134.3 and 139.9 ms)
are run-to-run noise, not an effect of this change.

The two slowest relaxed samples (1,820 and 1,853 ms, load1 0.86 and 0.81)
each spent over 1.3 s in file data syncs; a base sample did the same at
1,364 ms. That is the destination's `fsync` latency on this disk, the
term WP0(g) does not touch.

## 4. What would close the gap (for the operator; none of it is built)

From the breakdown, ranked by how much summed sync time each could remove
from an initial copy on this rig. Each changes what is durable when, so
each needs a ruling. Nothing here was tried.

1. **The 23 file data syncs (about 410 ms summed, 47 % of wall).** One
   `fsync` a file, issued by the materialize workers. Options: sync a
   group's files together (one `syncfs`, or `sync_file_range` writeback
   started early and one barrier a group), or drop the per-file sync and
   rely on the group's commit ordering. Durability: `Held` today means the
   file's bytes are durable and its row committed (R25, R-N58). A group
   barrier keeps that if `Held` still waits for the barrier and the
   commit; `syncfs` flushes the whole file system, so its cost depends on
   what else is dirty there. Dropping the sync breaks "a record is never
   committed before the bytes it describes are durable".
2. **The 8 directory-record commits (about 80 ms summed, 9 %).** Four
   `directory_pending` and four `directory_complete` commits in every
   sample, each a WAL sync of about 10 ms (the counters time all commits
   together; this is the count times the mean). Options: fold the
   directory records into the output group's transaction. Durability: a
   directory's record would become durable with the next output group,
   not at once; R-N102's directory records and their resume reading would
   have to hold with that delay.
3. **The 18 to 20 directory seals (about 70 ms summed, 8 %).** One `fsync`
   a touched directory a group. Options: seal each directory once a
   session, or once a group for all its new directories. Durability: a
   rename is durable only after its directory's seal; the row commit must
   still follow the seal (`MC_neg_commit_before_dirseal`), so coalescing
   delays `Held` for the files of that directory.
4. **Fewer, larger destination groups (3 or 4 group commits, about 30
   ms).** Small by itself; it matters only with 1, because the group is
   the unit a batched file sync would cover. Durability: a larger group
   holds `Held` back longer, and a crash loses more unrowed work (bounded
   by the capture record's adoption, #169).
5. **The capture side is not the gap.** Chunk and hash time (about 1.1 s
   summed over workers) overlaps the transfer and is CPU the 1 % delta and
   the resumes do not pay. rclone hashes nothing on this arm; whether S1
   compares like with like there is PR #204's question, not this lane's.

What rclone does for durability on this arm is nothing: it syncs no file
and no directory. So items 1 to 3 are the whole of the measured
difference that is durability, and closing it fully means matching
rclone's durability, which R25 and R-N58 forbid. The ruling needed is how
much of items 1 to 3 may be batched while `Held` keeps its meaning.
