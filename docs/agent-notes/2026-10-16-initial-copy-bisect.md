# 2026-10-16: initial-copy rise 372 -> 440 ms is rig drift, not a commit (OI-1003-Q154)

Lane: workflow subagent, informational only. Every sample here is
`--under-load --coordinator-quiet`, never a gate sample (OI-1003-Q39,
OI-1003-Q50, OI-1003-Q143). Rulings: OI-1003-Q154, OI-1003-Q165 (copy
gate present in the harness), OI-1003-Q166 (unshared corpus), OI-1002-Q30,
R-N13.

Mutation receipt (R-N13): on mbp-13 only under `~/git-bulkload`: the
scripts `run-icbisect-series-20261016.sh` and
`run-icbisect2-series-20261016.sh`, logs `logs/series-icbisect*-20261016.log`,
`logs/r23-icbisect*-20261016-*`, `logs/build-icbisect*-20261016.log`, work
roots `runs/r23-ab-icbisect*-20261016-*` and `runs/build-icbisect*`, new
binaries `build/bin/bulkload-bench-{508eda79f8d0,1d826abd46ef,3ea34cc3b9e6}`,
and the rig checkout `~/git-bulkload/bulkload` moved from detached `19e4b36`
to detached `508eda79` (the harness revision). No process was signalled; no
other r23/series process ran (checked with `ps` before each launch). Nothing
was deleted. In this repo: this note only.

## Question

The S1 initial copy (B native, median of the bench medians) read about
372 ms in the fold series (B = `51d1d4c`, 07:12Z) and about 440 ms in the
delta-lever series (B = `19e4b36` = `508eda7` + an opt-in lever, 13:33Z);
rclone stayed near 357 ms and the pinned A (`8a24161`) rose 566 -> 600 ms.
Commits between (`git log 51d1d4c..508eda7`): #230's merge `1d826ab` (tree
identical to `51d1d4c`), #233 `3ea34cc` (estate adopt/estate-diff, CI
fault-harness recipe), #234 `508eda7` (docs only; `crates/` identical to
`3ea34cc`).

## 1. Head-to-head, both revisions on the same rig state

`run-icbisect-series-20261016.sh`, harness at `508eda79` (has
`plain_copy_tree`), `--build-only` first, load1 < 0.5 before every sample,
unshared corpus `~/git-bulkload/r23-corpus-v1-unshared/corpus`, V4 skipped
(`--rev-v4 ''`), 14:45Z to 15:09Z. Each round ran the tasked sample
**fwd** (`--rev-a 51d1d4c --rev-b 508eda7`) and a role-swapped **rev**
sample (`--rev-a 508eda7 --rev-b 51d1d4c`), order alternated per round,
because the B/A/B/A/B order gives B three slots including the cool rep 0.
Cells are the median over that revision's reps in the sample (bench
medians, ms). Binaries: `51d1d4c` sha256 `ed89fbef070a978a` (the same
binary the fold series ran), `508eda7` `378fdab783794436`.

**fwd (as tasked: A = 51d1d4c, B = 508eda7)**

| round | initial A 51d1d4c | initial B 508eda7 | B - A | delta A | delta B | B - A |
|---:|---:|---:|---:|---:|---:|---:|
| 1 | 449.2 | 467.6 | +18.4 | 135.1 | 130.1 | -5.0 |
| 2 | 469.5 | 459.1 | -10.4 | 142.3 | 132.0 | -10.3 |
| 3 | 458.0 | 440.4 | -17.6 | 135.9 | 139.3 | +3.4 |
| 4 | 506.6 | 472.2 | -34.4 | 132.9 | 139.8 | +6.9 |
| 5 | 472.8 | 484.1 | +11.3 | 135.1 | 147.1 | +12.0 |
| median | 469.5 | 467.6 | **-10.4** | 135.1 | 139.3 | **+3.4** |

**rev (A = 508eda7, B = 51d1d4c)**

| round | initial 508eda7 [A] | initial 51d1d4c [B] | 508eda7 - 51d1d4c | delta 508eda7 | delta 51d1d4c | diff |
|---:|---:|---:|---:|---:|---:|---:|
| 1 | 480.1 | 459.9 | +20.2 | 142.4 | 138.1 | +4.3 |
| 2 | 503.6 | 454.3 | +49.3 | 130.9 | 140.6 | -9.7 |
| 3 | 484.5 | 447.1 | +37.4 | 136.0 | 135.3 | +0.7 |
| 4 | 508.2 | 453.8 | +54.4 | 138.1 | 133.2 | +4.9 |
| 5 | 494.5 | 451.4 | +43.1 | 150.7 | 140.0 | +10.7 |
| median | 494.5 | 453.8 | +43.1 | 138.1 | 138.1 | +4.3 |

Reading:

- As tasked, B (`508eda7`) is not slower: paired median -10.4 ms initial,
  +3.4 ms delta, signs mixed.
- The swap shows a position effect: whichever revision sits in the A slots
  (reps 1 and 3) reads slower (pooled medians by rep: 456.9, 486.3, 457.6,
  481.6, 467.8 ms). Splitting the two arms per round, position = 0.8,
  29.8, 27.5, 44.4, 15.9 ms; revision (`508eda7` - `51d1d4c`) = +19.3,
  +19.4, +9.9, +10.0, +27.2 ms (median +19.3, about 4 %).
- Delta is flat (balanced per-round -10.0 to +11.4 ms; pooled 137.1 vs
  135.3 ms).
- rclone pooled 357.9 vs 357.5 ms.

The balanced +19 ms is in the same direction in five of five rounds, so it
was bisected (section 2), although even taken at face value it is under a
third of the 68 ms rise.

## 2. Bisect of the first-parent merges

`run-icbisect2-series-20261016.sh`, same method, 2 rounds (rig disk, see
section 4), 15:18Z to 15:35Z. Step **s233** = `1d826ab` vs `3ea34cc` (all
the code between the two revisions). Step **s234** = `3ea34cc` vs
`508eda7`: `crates/` identical, so the binaries differ only by the embedded
revision. It is the null control for "same code, different binary, same
rig". Values are newer - older, initial, ms.

| step | round | fwd (older in A) | rev (newer in A) | balanced (revision) |
|---|---:|---:|---:|---:|
| s233 #233 code | 1 | +16.1 (486.1 -> 502.2) | +30.4 (519.4 -> 549.8) | +23.3 |
| s233 #233 code | 2 | -23.6 (636.1 -> 612.4) | +63.6 (602.8 -> 666.4) | +20.0 |
| s234 null (docs) | 1 | -13.4 (533.5 -> 520.1) | +41.0 (522.2 -> 563.2) | +13.8 |
| s234 null (docs) | 2 | +54.6 (592.7 -> 647.4) | aborted (below) | n/a |

The null step gives +13.8 ms balanced and +54.6 ms in a single arm. It
moves as much as the code step, so a 15 to 25 ms revision difference is
inside this rig's noise floor today. No commit is named.

The code read agrees. `51d1d4c..508eda7` touches no file the bench copy
path compiles or calls: `transfer.rs`, `transfer_store.rs`, `materialize.rs`,
`io/`, `walk.rs`, `durable`, `counters.rs` and `extents` are unchanged.
`Cargo.lock`, `Cargo.toml` and `flake.lock` are also unchanged. The changed
agent files are `closure.rs`, `estate*`, `git_carry*` (only the
adopt/estate-diff/landing helpers; `racy`, `pass_start_ns` and
`RACY_GRANULARITY_NS`, which `transfer.rs` calls, are untouched), `main.rs`
(agent binary, not the bench) and `fault.rs`. In `fault.rs`, five new
`Point`s grow `ALL` from 38 to 43; it is read only when parsing
`BULKLOAD_FAULT`, and `hit_in` checks a `OnceLock`. `bulkload-proto`
gains one refusal enum arm. The fold is active on both revisions
(`directories_folded=4` on every initial copy, in the delta series too).

Aborted sample: `s234rev-2` rep 0 (B = `3ea34cc`) refused after its native
delta sample 2 with `benchmark refused: No such file or directory (os
error 2); private outputs retained`
(`runs/r23-ab-icbisect2-20261016-s234rev-2/logs/rep0-B.stderr`). It
happened once in 18 samples, on a build with the same crates as main. Not
chased here; it is worth a look if it recurs.

## 3. Verdict: rig drift, CPU-side

The same binary (`51d1d4c`, sha256 `ed89fbef...`) read 340 to 402 ms
(fold-on arm, 03:15 EDT) and reads 447 to 507 ms today (10:45 EDT). Per
sample over the whole day (median native initial of the non-`8a24161`
reps, rclone median, median `cdc_hash_ns` CPU sum per native initial):

| series (UTC) | /home used (est.) | native initial | rclone | cdc_hash CPU ms | recv_worker_place CPU ms |
|---|---:|---:|---:|---:|---:|
| s1fold 07:12-07:36 | ~46-55 % | 340-453 | 348-367 | 920-1130 | 325-421 |
| s1delta 13:33-13:58 | ~55-65 % | 406-463 | 352-367 | 943-1144 | 353-437 |
| icbisect 14:45-15:09 | 66-75 % (measured) | 440-488 | 353-362 | 976-1145 | 437-484 |
| icbisect2 15:18-15:35 | 75-82 % (measured) | 488-643 | 350-368 | 1086-1407 | 491-652 |

- **Unchanged CPU-bound work slows with the rig.** `cdc_hash_ns` hashes a
  page-cache-resident source (residency 1.0 every rep) and never touches
  the destination. Its sum rose from about 950 ms to about 1400 ms on code
  that did not change, and native elapsed follows it. rclone, which leaves
  the copy to the kernel, is flat at 350 to 368 ms throughout.
- **Thermal.** The i7-5557U (2 cores) runs hot under every sample:
  `x86_pkg_temp` 58 to 69 C at admission and 93 to 98 C at sample end.
  `package_throttle_count` grew 41927 -> 44883 over the 25-minute
  icbisect series and by 176 to 731 per sample in icbisect2 (log `thr=`).
  `intel_powerclamp` idle injection fired during builds at 13:30Z and
  15:10Z, but not at the fold build at 07:09Z (dmesg). That suggests a
  warmer machine later in the day (03:00 vs 09:30 to 11:10 local). The
  governor is `performance` and turbo is on. Historic temperatures are not
  recorded: mbp-13 is not in the lab Prometheus/Mimir, and the series logs
  before today's had no `obs` lines.
- **Disk fill** of the destination fs (`/dev/mapper/rl-home`, xfs, also
  the work root) rose with every sample, about 8.06 GiB each (df deltas in
  the `obs` lines). Earlier fills are reconstructed from the run-dir
  inventory. Fill and time of day rise together, so read-only observation
  cannot separate them. The CPU-sum signature and the flat rclone point to
  the CPU side, not the fs.
- **The 372 ms baseline was flattered.** In the fold series the fold-on
  arm always ran first in its round. Its `cdc_hash` median (948 ms over
  the five samples) sits below the fold-off arm's (1004 ms) although the
  fold does not touch hashing. The delta series alternated arm order and
  saw no consistent split. Part of the fold series' 65 ms on/off gap is therefore order
  and heat, and the fold lever's real initial-copy effect is smaller than
  that series reported.
- **Load** was at most 0.5 at every admission and up to 2.5 at sample end,
  the same pattern in every series. It is not a differentiator.

Implication for follow-up rig work: compare revisions only inside one
series, with roles swapped (fwd and rev). Log `x86_pkg_temp` and
`package_throttle_count` per sample. Treat cross-series absolute
initial-copy numbers on mbp-13 as drift-dominated.

## 4. Rig disk (operator decision)

These two series added about 150 GiB under `~/git-bulkload/runs` (18 work
roots, about 8 GiB each, mostly corpus copies and destinations). `/home` on
mbp-13 is now 82 % used, 168 GiB free. Nothing was reclaimed (AGENTS.md:
reclaim only proven redundant data). The logs that carry the numbers are in
`~/git-bulkload/logs`, and each work root's `logs/` and `r23-ab.json` hold
the per-rep data. Whether to reclaim the `r23-ab-*` corpus and destination
trees is the operator's call. Each later sample adds about 8 GiB more.
