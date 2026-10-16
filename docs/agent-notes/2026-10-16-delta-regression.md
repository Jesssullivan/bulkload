# 2026-10-16 delta-regression lane: why the 1 % delta is slower than rclone

Lane: diagnose the "1 % delta regression" reported from the fold series
(Linear TIN-4543, coordinator comment `0840c52b`). Branch
`docs/evidence-20261016`. No product code changed. Rulings cited:
OI-1003-Q39 and Q50 (under-load runs are informational, never of record),
OI-1003-Q143 (the 0.90x margin), OI-1003-Q165 and Q166 (no clone; the
unshared corpus), OI-1002-Q30 (A never decides), R23, R25 / R-N58, R-N13.
Nothing was signalled on either host (R-N11).

## Verdict

**There is no commit regression.** On the rig, B's 1 % delta has improved
since the first rig sample, not worsened. The 2026-09-18 "win" was
measured on another host, by another engine, on another corpus, where the
delta moved a file one tenth the size. The gap to rclone on the rig has
one main structural cause, measured below:

- **The destination asks for a manifest first, for a file it can't fill
  locally.** The source therefore reads and hashes the whole 33.5 MB file
  before the first byte is placed, with no overlap between the two.
- On the rig this costs about **26 to 29 ms per delta copy**: 136.9 ms
  against 110.7 ms in-process medians, same binary and same bytes.

## 1. What the delta workload is on the rig

`add_one_percent_delta` (bench `main.rs`) XORs the first
`ceil(total / 100)` bytes of the first regular files in row order. On
corpus v1 that is 2,398,199 bytes at the head of `big/archive.log`, a
33,555,209-byte file. `remove_delta_targets` then deletes that file from
each arm's destination, outside timing. So both arms copy one whole new
33.5 MB file into a destination that holds the other 22:

- every B delta sample has `source_bytes_read`,
  `transferred_content_bytes`, `wire_handed_bytes` and
  `write_dest_materialize_bytes` all equal to 33,555,209;
- `read_dest_local_reuse_bytes` is 0, `files_materialized` is 1 and
  `transfer_racy_captures` is 1.

The bench's "delta" is therefore not native's changed-seat path: the
seat's old output is gone before the copy starts. It measures adding a new
33.5 MB file to a populated destination.

## 2. Across series: no phase grew

Medians over every native delta sample of each label, from
`runs/*/logs/rep*-{B,A,V4}.stdout` on mbp-13, read only. The rclone column
is the same reps' rclone delta samples. Timers are in ms;
`stream - first_data` is the receive loop after its first data frame,
including the drain.

| series | label | rev | n | native | rclone | ratio | cdc_hash | first_data | stream - first_data | drain | file barrier | dir barrier | sqlite commit |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 10-07 gate-a (B/B/B) | B | `3e7b5bf` | 9 | 133.8 | 106.0 | 1.262 | 48.4 | - | - | - | 36.3 | 3.8 | 8.5 |
| 10-07 1410Z of record | B | `adfdf57` | 9 | 140.5 | 107.1 | 1.311 | 48.5 | - | - | - | 44.6 | 3.8 | 8.7 |
| 10-07 1410Z of record | A | `3931471` | 6 | 130.6 | 106.9 | 1.222 | 48.7 | - | - | - | 35.4 | 3.9 | 8.7 |
| 10-07 2140Z #210 | B | `5b4cbe4` | 9 | 130.8 | 99.6 | 1.314 | 47.9 | - | - | - | 23.5 | 3.8 | 8.8 |
| 10-08 0233Z handoff | B | `ec53448` | 9 | 136.2 | 102.5 | 1.328 | 48.1 | - | - | - | 34.8 | 3.8 | 8.6 |
| 10-09 s1tp | B | `8a24161` | 45 | 120.6 | 103.9 | 1.161 | 48.6 | 52.1 | 65.9 | 34.2 | 19.7 | 3.8 | 8.6 |
| 10-09 s1tp | A | `91d4294` | 30 | 139.7 | 104.5 | 1.337 | 48.3 | - | - | - | 42.5 | 3.9 | 8.7 |
| 10-15 s1lev | B | `59e5e60` | 45 | 116.6 | 106.1 | 1.099 | 48.3 | 52.0 | 61.7 | 31.0 | 16.8 | 3.8 | 8.6 |
| 10-15 s1lev | A | `8a24161` | 30 | 120.9 | 106.6 | 1.135 | 48.5 | 51.9 | 66.7 | 36.1 | 21.8 | 3.8 | 8.7 |
| 10-16 s1fold on | B | `51d1d4c` | 45 | 120.0 | 105.4 | 1.139 | 48.7 | 52.3 | 63.9 | 28.7 | 13.7 | 3.8 | 8.7 |
| 10-16 s1fold on | A | `8a24161` | 30 | 126.8 | 105.4 | 1.204 | 48.7 | 52.3 | 71.1 | 36.7 | 22.2 | 3.9 | 8.7 |
| 10-16 s1fold off | B | `51d1d4c` | 45 | 124.0 | 105.4 | 1.176 | 48.4 | 52.3 | 69.7 | 34.6 | 18.9 | 3.9 | 8.7 |
| any series | V4 | `41bf9a4` | 1-5 | 188-214 | - | - | 54-60 | - | - | - | 46-64 | 3.8-5.7 | 15-19 |
| 10-07 dry-run corpus | B | `9d01b68` | 3 | 41.8 | 53.0 | **0.790** | 4.9 | - | - | - | 18.8 | 3.8 | 7.6 |

`recv_first_data_ns` first appears in `8a24161`. Older builds print no
receive timers.

What the table shows:

- **Walk and decide are not the cost.** `walk_ns` is about 0.1 ms and
  `reuse_census_ns` (the decide) about 0.8 ms in every series.
  `recv_setup_ns` is 1.5 to 2.4 ms. `recv_tail_ns` is 0.3 ms.
- **Hashing is flat at 48 ms in every rig build since `3931471`.** That is
  FastCDC plus BLAKE3 over 33.5 MB on one capture worker, about 700 MB/s.
- **The ramp is flat at 52 ms.** `recv_first_data_ns` is about 52 ms in
  every build that has the timer, which is `cdc_hash` plus setup. The
  receiver sees no data until the whole file has been hashed.
- **The flush did improve.** The file barrier fell from 35 to 45 ms
  (10-07 and 08) to 14 to 20 ms (`8a24161` onwards), from #210's
  write-back kicks and the levers. The directory barrier (3.8 ms) and the
  one SQLite FULL commit (8.6 ms) never moved.
- **Durable commits did not grow.** `sqlite_commits_total` is 1,
  `durable_groups` is 1 and `flush_dir_barrier_count` is 1 in every B
  delta sample.
- **From 10-09 on, B beat A in every series** (by 4 to 19 ms). On 10-07
  and 10-08, B and A were within 10 ms of each other in either direction:
  1410Z A `3931471` 130.6 against B 140.5; 2140Z A 136.9; 0233Z A 136.3.
  B's delta went from 1.31x rclone (10-07) to 1.10 to 1.18x (10-15 and
  10-16). So there is no commit to bisect: every phase is flat or
  falling.
- **At equal durability native wins every rep.** Native's delta beat
  `rclone_synced_ms` (rclone plus `syncfs`, informational) in all 63 B
  reps on the rig that print it. rclone's untimed delta `syncfs` median is 50.6 ms.
- **On the same rig with a smaller changed file, native wins outright.**
  The 2026-10-07 dry-run corpus's delta file is 3,145,728 bytes: native
  41.8 ms against rclone 53.0 ms.

## 3. Why 2026-09-18 is not a baseline

`docs/evidence/r23-2026-09-18.md` differs from the rig series in five
ways:

- **Engine.** It ran tummycrypt's `tcfs-bulkload-bench` at `c6cead96`,
  not this repository's engine.
- **Host.** neo: macOS, APFS, the `F_BARRIERFSYNC` seal.
- **Corpus.** A different corpus (`9b52260a…`, 242,605,606 bytes).
- **Changed file.** It was about 3.3 MB (`source_read` 3,280,458), and
  0.84 MB of it was filled locally (transferred 2,444,912).
- **rclone.** v1.75.0, with an rclone "median" of 127.064 ms that is the
  mean of two samples: 203.212 ms and 50.916 ms.

Native's 58.8 ms there corresponds to the rig's dry-run-corpus row
(41.8 ms native), not to corpus v1's 33.5 MB file. Against 09-18, the
bytes the delta hashes went up about 10x (4.8 ms to 48 ms `cdc_hash`)
and the bytes it seals did too. No product change made that happen.

## 4. Root cause: manifest-first for a file nothing local can fill

At `51d1d4c`, `transfer.rs`:

- `Inbound` decide (line 5098):
  `if identity.is_some() || self.fill_locally || self.target.salvaged() > 0 { Decision::WantManifest } else { Decision::Send }`.
- `fill_locally = store.has_output_hints()?` (line 4645). It is true as
  soon as the destination store has any output hint, which means any
  earlier non-empty output.

In the bench delta the target was removed, so `identity` is `None`. But
the other 22 outputs have hints, so the decision is `WantManifest`. Then:

1. The source runs `manifest_capture` (line 2662). It counts
   `source_ledger_miss_reads=1`, which every B delta sample shows.
2. It reads and chunks the whole file into retained memory, then offers
   the manifest.
3. `plan_file` probes every chunk locally and finds nothing.
   `read_dest_local_reuse_bytes` is 0, partly because the old output's
   hints name a path that the bench removed.
4. `NeedChunks` asks for all of them, and only then does placement start.

A `Send` decision would instead stream through `send_capture` (line
2554), which places each chunk while the next one is hashed. The initial
copy, which runs into an empty store and therefore always `Send`s, shows
`source_ledger_miss_reads=0`.

**Responsible commit:** `c2f8b663` (2026-10-01, "port the agent and bench
to wire v5 with a digest-only ledger"). It introduced `fill_locally` as
`has_output_hints()` and the "manifest first when the destination could
fill locally" rule. It is in the A pin `3931471` and in every rig build,
and not in V4 `41bf9a4`, so it predates every rig sample. That is why no
rig series shows a step.

## 5. Informational A/B on the rig: Send against WantManifest

This run is not a gate sample and not an R23 run (OI-1003-Q39). Setup:

- **Binary.** `bulkload-agent` built on sting from `git archive 51d1d4c`
  (`release`, sha256 `c7975eae…bbedd`), staged at
  `~/git-bulkload/runs/delta-diag-20261016-tools/`. Its runtime deps are
  the rig's own store paths.
- **Corpus.** `big/archive.log` copied by plain read and write from the
  unshared corpus (Q165, Q166).
- **Arms.** Each timed `bulkload-agent copy` lands that one 33,555,209-byte
  file into a destination already seeded, outside timing, with one file
  `y.bin`:
  - *send*: `y.bin` is empty, so the store has no hints, so `Send`;
  - *manifest*: `y.bin` is 1 MiB, so the store has hints, so
    `WantManifest`.
- **Controls.** The seed is past the 2 s racy allowance, so `y.bin` is
  reused in both arms (`completed=1 reused=1`). Each arm runs 10 reps in
  ABBA order, at load1 0.04 to 0.15 (the rig was idle; it waited for load1
  under 0.5 first). The output was verified with `cmp` and was identical.
- **Logs.** `run-2.log`, and `~/git-bulkload/runs/delta-diag-20261016-2/results.json`.

| arm (10 reps) | in-process wall | first_data | cdc_hash | stream | drain | sqlite commit | source_bytes_read | bytes_received | ledger miss reads |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| send | **110.72** (96.6-133.1) | 0.79 | 40.65 | 78.14 | 36.15 | 8.40 | 33,555,209 | 33,555,209 | 0 |
| manifest | **136.89** (123.8-164.1) | 50.97 | 47.87 | 107.68 | 33.30 | 8.13 | 33,555,209 | 33,555,209 | 1 |

The bytes read, the bytes received and the commits are identical. The
only change is the decision, and the decision alone costs **26.2 ms**
(subprocess wall 138.3 against 112.1 ms). The first run (`run-1.log`, in
`delta-diag-20261016-1`) had a racy seed that re-read `y.bin` in the
manifest arm. It gave 138.1 against 109.4 ms, in the same direction.

The CLI path's absolute numbers are not the bench's: it records closure
rows and its flush counters differ (`flush_full_ns` about 19 ms). Read the
difference, not the levels.

## 6. Proposed fix and expected effect (product change for a later lane)

1. **Primary.** Under the loopback hand-off (`copy`, both halves in one
   process), answer `Send` when only `fill_locally` would justify a
   manifest: no output at the path (`identity` is `None`) and no salvage.
   Keep `WantManifest` for `identity.is_some()` (the real changed-seat
   delta, superseding publish from the old output's chunks) and for
   salvage.
   - **R25 and S3 hold.** The source reads the file exactly once either
     way (measured above). In a local copy, local fill buys no wire bytes:
     it re-reads and re-verifies a destination chunk instead of taking the
     handed buffer. The initial copy already `Send`s into an empty store.
   - **The cost is cross-file dedup on new paths.** For `pull` over a real
     wire, that dedup saves wire bytes, so keep the rule there or
     replace it with item 2.
   - **Expected effect.** About −26 ms on the bench delta: B's fold-on
     median falls from about 120 to about 94 ms, against rclone's about
     105 ms, so about **0.89x**. That is at Q143's 0.90x margin, not
     comfortably under it.
   - **Validate** with an informational `r23_ab.py --under-load` series
     on the unshared corpus. The decision test belongs in `transfer.rs`'s
     decide tests, plus a counter assertion
     (`source_ledger_miss_reads=0` for a new file into a populated
     store).
2. **Structural, for `pull`.** Pipeline the manifest: send per-chunk
   digests as they are hashed and answer `NeedChunks` incrementally, so
   placement overlaps capture and dedup is kept. This needs a wire
   version and a model update (`BulkloadTransfer.tla`). Expected: the
   same ~26 ms locally, and the full 48 ms ramp on a wire.
3. **The remaining fixed cost is the drain**, about 26 to 29 ms on the
   bench: file barrier 14 to 19 ms, directory barrier 3.8 ms, one FULL
   SQLite commit 8.7 ms. rclone pays none of it. Parallel BLAKE3 over a
   single large file's chunks on the second core could cut the 40 to
   48 ms hash-bound stream by up to half.
   - Items 1 and 3 together are the realistic route to a clear sub-0.90x
     delta on this 2-core rig.
4. **The bench's delta is not a delta.** `remove_delta_targets` turns it
   into a new-file copy, so native's changed-seat path (superseding
   publish, local fill) is never timed. A gate that wants to measure R25's
   delta path needs a ruling on the preconditioning. It is the operator's
   call, and not assumed here.

## Corrections to earlier notes

`2026-10-08-s1-throughput.md` read the delta ramp ("first-data 54 % of
wall") as "the walk and census of the 22 unchanged files". The timers say
otherwise: the walk is 0.1 ms and the census 0.8 ms. The ramp is the
manifest capture of the changed file (section 4).

## Open

- Coordinator: carry the verdict and section 6 to Linear TIN-4543. This
  lane posted nothing there.
- Fix 1 is a product change. It needs its own lane and PR, and an
  informational series before any gated sample (Q143).
- Rig files written by this lane, all under `~/git-bulkload/runs/`:
  `delta-diag-20261016-tools/` (binary, script, `run-{1,2}.log`) and
  `delta-diag-20261016-{1,2}/` (`results.json`, `x.bin` and the seeds).
