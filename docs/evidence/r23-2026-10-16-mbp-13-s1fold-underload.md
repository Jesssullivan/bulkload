# R23 gate (a) S1 fold series - 2026-10-16 - rig mbp-13 (INFORMATIONAL UNDER LOAD - NOT A GATE SAMPLE)

> **Informational, under load, not a gate sample.** Ten `r23_ab.py` runs in
> `--under-load` mode (OI-1003-Q39, OI-1003-Q50), order `BABAB`, on the rig
> of record `mbp-13`, on the unshared re-seal of corpus v1 (OI-1003-Q166).
> Five rounds, each one run with the directory fold on and one with it off
> (`--b-env BULKLOAD_DIRECTORY_FOLD=0`). Every run's harness status is
> `complete-under-load-informational` with `of_record=false`. No number here
> is an R23 gate verdict or an S1 result in either direction; S1 stays
> gated against plain rclone, and a gated sample needs an informational
> median at or under 0.90x plain rclone first (OI-1003-Q143). The R-N81 load
> gate and R-N91 quiet lanes were set aside by ruling; load1 is recorded
> before and after every rep.

Rulings: OI-1003-Q39, OI-1003-Q50, OI-1003-Q143, OI-1003-Q154 (the fold,
lever 2), OI-1003-Q165 and Q166 (no clone; the unshared corpus),
OI-1002-Q30 (A never decides), R23, R-N57, R-N13. Harness:
`crates/bulkload-bench/scripts/r23_ab.py`. The arm design is the fold
note's (`docs/agent-notes/2026-10-16-s1-fold.md`, section 5).

- **B** = `51d1d4c` (`51d1d4c90a43f40b3f6b7edb28822d6c62b21414`, bench
  binary sha256 `ed89fbef070a978a`): the S1 stack with the fold, the
  merge of `main` `5beb031` (#228) into `feat/s1-stack-20261015`. It is in
  `main`'s history since #230 (`1d826ab`). Compiled once on the rig before
  the series (`~/git-bulkload/logs/build-s1fold-20261016.log`, `release`,
  3 min 09 s, 07:12Z); no compile inside any round.
- **Arms.** *fold on*: no `--b-env`. *fold off*: `--b-env
  BULKLOAD_DIRECTORY_FOLD=0`, which reaches the B reps only (every A and
  V4 rep's `env` is empty). Same B binary in both arms.
- **A** = `8a24161` (`8a241614ba96`, sha256 `9fa36c3c38130863`), the rig's
  pinned control: it predates the fold and the levers, and never decides
  anything (OI-1002-Q30).
- **V4** = `41bf9a4` (`41bf9a4fa233`, sha256 `a5acdebb85ef9374`,
  `pmset-shim`): diagnostic only, the dedup reference.
- **rclone**: `rclone v1.74.4`,
  `/nix/store/v5xbkynmfg8ml23d82m09s802nmj2r6f-rclone-1.74.4`, binary
  sha256 `41b54044d938cadf392f53cb65560efff7c03e915adf809e8008b2c653b76e0e`,
  `matches_committed_pin=true` (flake.lock nixpkgs
  `241313f4e8e508cb9b13278c2b0fa25b9ca27163`, OI-1003-Q105), the
  r23-2026-09-18 flags, no sync flag.
- **Corpus**: `~/git-bulkload/r23-corpus-v1-unshared/corpus` (the
  OI-1003-Q166 re-seal, 2026-10-15), 23 regular files, 239,819,837 bytes,
  content identity
  `f4a7619f7b88f2e0e1eeadb5995c79a809eafa8b4a8cf3f82d1ff6f29b5b07c7`,
  verified before the first and after the last rep of every run. Working
  copy by `read-write (os.read/os.write; OI-1003-Q165)`; FIEMAP-shared
  extents 0 in the sealed corpus, the working copy and every rep's fixture,
  before and after every rep. The bench's `sealed_corpus_blake3`
  (content and stat fields of each run's working copy) matched in every
  rep of its run.
- **Rig**: `mbp-13`, MacBookPro12,1, i7-5557U (2 cores, 4 threads),
  15.3 GiB, Linux 6.19.5-11.xr.el10, work root on `xfs`, AC on every row.
  Page cache never dropped (every timed arm starts source-hot). Native
  seal `fsync`, durability `group`. Loopback hand-off: B `on` in every run.
- **Window**: 2026-10-16 07:12Z (build) to 07:36:55Z (series done); the
  first rep was admitted at 07:14:29Z. In every round the fold-on run went
  first and the fold-off run second
  (`~/git-bulkload/logs/series-s1fold-20261016.log`: ten runs, every one
  `exit=0`).
- **Sources** (read-only, on `mbp-13`):
  `~/git-bulkload/logs/r23-s1fold-20261016-{on,off}-underload-{1..5}.md`,
  `r23-s1fold-20261016-{on,off}-{1..5}.log` and
  `series-s1fold-20261016.log`; every counter is in
  `~/git-bulkload/runs/r23-ab-s1fold-20261016-{on,off}-{1..5}/r23-ab.json`.

Every value is the bench's own per-rep median in ms. Ratios are native /
rclone of the same rep, or of the same round's B medians. A round's "B
median" is the median of its three B reps' bench medians, the figure each
run's evidence file reports as "median of the bench's informational
medians".

## Per-round B medians

| round | arm | initial native | initial rclone | init / rclone | delta native | delta rclone | delta / rclone | B bench statuses |
|---:|---|---:|---:|---:|---:|---:|---:|---|
| 1 | on | 357.373 | 359.182 | 0.995 | 121.610 | 106.972 | 1.137 | fail x2, pass x1 |
| 1 | off | 437.291 | 366.532 | 1.193 | 113.284 | 105.000 | 1.079 | fail x3 |
| 2 | on | 400.055 | 347.686 | 1.151 | 105.883 | 109.969 | 0.963 | fail x3 |
| 2 | off | 409.395 | 355.843 | 1.150 | 115.994 | 107.665 | 1.077 | fail x3 |
| 3 | on | 339.786 | 358.310 | 0.948 | 120.140 | 104.320 | 1.152 | fail x3 |
| 3 | off | 419.626 | 364.679 | 1.151 | 125.254 | 104.267 | 1.201 | fail x3 |
| 4 | on | 372.025 | 352.861 | 1.054 | 126.113 | 104.823 | 1.203 | fail x3 |
| 4 | off | 448.544 | 355.715 | 1.261 | 130.997 | 108.747 | 1.205 | fail x3 |
| 5 | on | 402.179 | 358.841 | 1.121 | 113.914 | 105.676 | 1.078 | fail x3 |
| 5 | off | 453.078 | 352.012 | 1.287 | 113.314 | 106.204 | 1.067 | fail x3 |

Median of the five round medians:

| arm | initial native | initial rclone | init / rclone | delta native | delta rclone | delta / rclone |
|---|---:|---:|---:|---:|---:|---:|
| fold on | 372.025 | 358.310 | 1.038 | 120.140 | 105.676 | 1.137 |
| fold off | 437.291 | 355.843 | 1.229 | 115.994 | 106.204 | 1.092 |

### Paired rounds (off minus on), with the A control

A runs the same binary in both arms, so its movement between the on and
off run of a round is the rig's own drift over those two minutes.

| round | B initial, off - on | A initial median, on / off | A initial, off - on | B delta, off - on |
|---:|---:|---|---:|---:|
| 1 | 79.918 | 567.981 / 599.747 | 31.766 | -8.326 |
| 2 | 9.340 | 508.696 / 558.790 | 50.094 | 10.111 |
| 3 | 79.840 | 541.300 / 588.136 | 46.836 | 5.114 |
| 4 | 76.519 | 566.279 / 578.565 | 12.286 | 4.884 |
| 5 | 50.899 | 573.690 / 596.545 | 22.855 | -0.600 |

## Counters (every B native sample, per copy)

The fold's counters are exact and identical in every B rep of an arm.
`directories_folded_total` is the rep's sum over its native samples: 12
in every fold-on B rep (4 per initial copy, three initial samples; 0 in
the warm, interrupted and delta phases) and 0 in every fold-off B rep.
A's build predates the counter (absent).

| counter (initial copy) | fold on | fold off | 2026-10-15 levers series (`59e5e60`) |
|---|---:|---:|---:|
| `directories_folded` | 4 | 0 | (absent) |
| `directories_finished` | 4 | 4 | 4 |
| `durable_groups` | 2 | 2 | 2 |
| `flush_fs_count` | 2 | 2 | 2 |
| `flush_dir_count` | 2 | 2 | 2 |
| `flush_dir_barrier_count` | 4 | 8 | 8 |
| `flush_full_count` | 0 | 0 | 0 |
| `sqlite_directory_complete_commits` | 0 | 1 | 1 |
| `sqlite_commits_total` | 5 | 6 | 6 |
| `full_flushes_total` | 6 | 7 | 7 |
| `source_bytes_read` | 239,819,837 | 239,819,837 | 239,819,837 |

The 1 % delta copy is the same in both arms: `durable_groups` 1,
`flush_dir_barrier_count` 1, `sqlite_commits_total` 1,
`directories_folded` 0, `source_bytes_read` 33,555,209. These are the
values the fold note predicted (section 5), arm for arm.

Receive timers (median over the B reps' per-rep medians, initial copy):
`recv_tail_ms` 0.72 (range 0.66 to 0.82) fold on against 28.37 (27.83 to
31.83) fold off; `recv_drain_ms` 48.31 (41.67 to 64.02) against 49.86
(46.76 to 57.73). The delta copy's tail was 0.29 ms in both arms.

R25 in every B and A rep: the warm resume and the interrupted resume read
zero (`r25_warm_zero` and `r25_interrupted_zero` true), and peak memory
stayed under 2 GiB. Bytes received: B and A 239,819,837 (the whole
payload), V4 219,946,368, so the #88 duplicate share is 0.0829, as before.

## Reading

- **Fold on was faster than fold off in every paired round, by 9.3 to
  79.9 ms** of B's initial median (median of the paired differences
  76.5 ms). The medians of the five round medians differ by **65.3 ms**
  (372.025 against 437.291 ms); over all fifteen B reps of each arm, by
  46.3 ms (382.472 against 428.736 ms).
- **That gap is not all the fold's.** In every round the off run went
  second, and A, which is the same binary in both arms, was also slower in
  the off run of every round, by 12.3 to 50.1 ms (median 31.8 ms). Taking
  A's drift off round by round leaves 48.2, -40.8, 33.0, 64.2 and 28.0 ms;
  round 2 reverses. The timers attribute about 27.6 ms to the fold
  directly: the receive tail falls from about 28.4 ms to 0.7 ms, and this
  time, unlike the sting census, the drain does not grow to absorb it. The
  series cannot separate the rest of the gap from order and drift.
- **Against plain rclone, the gate's comparison, fold on is not at Q143's
  margin.** Its round medians for the initial copy ran 0.948 to 1.151
  (median of round medians 1.038); two rounds were under 1.00x and none at
  or under 0.90x. Per rep, 4 of 15 fold-on B reps were under 1.00x and one
  reached 0.90x (round 3 rep 2: 337.835 against 375.501 ms, 0.8997); 0 of
  15 fold-off reps were under 1.00x (1.054 to 1.351).
- **The 1 % delta is worse than rclone in 9 of the 10 round medians**
  (1.067x to 1.205x); the exception is round 2 fold on, 0.963x. The fold
  does not touch the delta copy (no directory is created), and the
  per-round differences between the arms, -8.3 to 10.1 ms, are noise.
- One B rep had bench status `pass`: round 1 fold on, rep 4 (initial
  349.297 against 384.797 ms, delta 106.251 against 106.972 ms). Under load
  that status is informational and decides nothing.
- **Load**: load1 ran from 0.45 to 1.90 over the B and A reps, under
  R-N81's 2.5 throughout, but the R-N91 quiet lanes were not held, so the
  series stays under-load.

## Per-rep results

### Round 1, fold on (2026-10-16-0714Z, `r23-ab-s1fold-20261016-on-1`)

| # | label | bench status | load1 before/after | initial native | initial rclone | init / rclone | delta native | delta rclone | delta / rclone | `directories_folded_total` | R25 warm / interrupted zero |
|---:|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|
| 0 | B | fail | 0.46/0.5 | 357.373 | 340.786 | 1.049 | 121.610 | 105.415 | 1.154 | 12 | True / True |
| 1 | A | fail | 0.5/1.32 | 563.192 | 370.725 | 1.519 | 130.117 | 108.250 | 1.202 | absent | True / True |
| 2 | B | fail | 1.32/1.52 | 382.472 | 359.182 | 1.065 | 127.198 | 109.488 | 1.162 | 12 | True / True |
| 3 | A | fail | 1.52/1.45 | 572.771 | 358.687 | 1.597 | 117.112 | 102.936 | 1.138 | absent | True / True |
| 4 | B | pass | 1.45/1.3 | 349.297 | 384.797 | 0.908 | 106.251 | 106.972 | 0.993 | 12 | True / True |
| 5 | V4 | diagnostic-only | 1.3/1.44 | 1,236.460 | n/a | | n/a | n/a | | n/a | n/a |

### Round 1, fold off (2026-10-16-0716Z, `r23-ab-s1fold-20261016-off-1`) (`BULKLOAD_DIRECTORY_FOLD=0` on B)

| # | label | bench status | load1 before/after | initial native | initial rclone | init / rclone | delta native | delta rclone | delta / rclone | `directories_folded_total` | R25 warm / interrupted zero |
|---:|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|
| 0 | B | fail | 0.46/0.61 | 437.291 | 380.697 | 1.149 | 130.176 | 105.000 | 1.240 | 0 | True / True |
| 1 | A | fail | 0.61/1.14 | 588.638 | 374.321 | 1.573 | 135.210 | 106.003 | 1.276 | absent | True / True |
| 2 | B | fail | 1.14/1.19 | 440.718 | 366.532 | 1.202 | 106.065 | 108.251 | 0.980 | 0 | True / True |
| 3 | A | fail | 1.19/1.82 | 610.856 | 356.684 | 1.713 | 121.864 | 108.368 | 1.125 | absent | True / True |
| 4 | B | fail | 1.82/1.76 | 415.862 | 355.779 | 1.169 | 113.284 | 104.050 | 1.089 | 0 | True / True |
| 5 | V4 | diagnostic-only | 1.76/1.93 | 1,323.941 | n/a | | n/a | n/a | | n/a | n/a |

### Round 2, fold on (2026-10-16-0719Z, `r23-ab-s1fold-20261016-on-2`)

| # | label | bench status | load1 before/after | initial native | initial rclone | init / rclone | delta native | delta rclone | delta / rclone | `directories_folded_total` | R25 warm / interrupted zero |
|---:|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|
| 0 | B | fail | 0.45/0.61 | 408.886 | 345.499 | 1.183 | 107.080 | 110.697 | 0.967 | 12 | True / True |
| 1 | A | fail | 0.61/0.83 | 524.784 | 350.518 | 1.497 | 110.766 | 104.307 | 1.062 | absent | True / True |
| 2 | B | fail | 0.83/0.95 | 400.055 | 347.686 | 1.151 | 105.883 | 109.969 | 0.963 | 12 | True / True |
| 3 | A | fail | 0.95/1.5 | 492.608 | 369.593 | 1.333 | 132.088 | 105.605 | 1.251 | absent | True / True |
| 4 | B | fail | 1.5/1.57 | 373.753 | 348.208 | 1.073 | 103.602 | 103.380 | 1.002 | 12 | True / True |
| 5 | V4 | diagnostic-only | 1.57/1.53 | 1,270.283 | n/a | | n/a | n/a | | n/a | n/a |

### Round 2, fold off (2026-10-16-0721Z, `r23-ab-s1fold-20261016-off-2`) (`BULKLOAD_DIRECTORY_FOLD=0` on B)

| # | label | bench status | load1 before/after | initial native | initial rclone | init / rclone | delta native | delta rclone | delta / rclone | `directories_folded_total` | R25 warm / interrupted zero |
|---:|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|
| 0 | B | fail | 0.53/0.65 | 383.786 | 364.041 | 1.054 | 104.445 | 104.121 | 1.003 | 0 | True / True |
| 1 | A | fail | 0.65/0.71 | 559.961 | 355.482 | 1.575 | 111.919 | 101.343 | 1.104 | absent | True / True |
| 2 | B | fail | 0.71/0.84 | 409.395 | 355.738 | 1.151 | 115.994 | 107.665 | 1.077 | 0 | True / True |
| 3 | A | fail | 0.84/0.79 | 557.618 | 356.599 | 1.564 | 107.072 | 106.474 | 1.006 | absent | True / True |
| 4 | B | fail | 0.79/0.9 | 428.736 | 355.843 | 1.205 | 126.169 | 108.592 | 1.162 | 0 | True / True |
| 5 | V4 | diagnostic-only | 0.9/0.91 | 1,240.030 | n/a | | n/a | n/a | | n/a | n/a |

### Round 3, fold on (2026-10-16-0723Z, `r23-ab-s1fold-20261016-on-3`)

| # | label | bench status | load1 before/after | initial native | initial rclone | init / rclone | delta native | delta rclone | delta / rclone | `directories_folded_total` | R25 warm / interrupted zero |
|---:|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|
| 0 | B | fail | 0.47/0.6 | 339.786 | 358.146 | 0.949 | 120.140 | 104.320 | 1.152 | 12 | True / True |
| 1 | A | fail | 0.6/0.98 | 571.556 | 386.005 | 1.481 | 127.265 | 106.841 | 1.191 | absent | True / True |
| 2 | B | fail | 0.98/1.38 | 337.835 | 375.501 | 0.900 | 135.060 | 107.052 | 1.262 | 12 | True / True |
| 3 | A | fail | 1.38/1.89 | 511.043 | 345.115 | 1.481 | 130.791 | 107.746 | 1.214 | absent | True / True |
| 4 | B | fail | 1.89/1.68 | 402.292 | 358.310 | 1.123 | 110.249 | 104.252 | 1.058 | 12 | True / True |
| 5 | V4 | diagnostic-only | 1.68/1.86 | 1,296.718 | n/a | | n/a | n/a | | n/a | n/a |

### Round 3, fold off (2026-10-16-0725Z, `r23-ab-s1fold-20261016-off-3`) (`BULKLOAD_DIRECTORY_FOLD=0` on B)

| # | label | bench status | load1 before/after | initial native | initial rclone | init / rclone | delta native | delta rclone | delta / rclone | `directories_folded_total` | R25 warm / interrupted zero |
|---:|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|
| 0 | B | fail | 0.46/0.47 | 454.272 | 354.570 | 1.281 | 125.254 | 102.540 | 1.222 | 0 | True / True |
| 1 | A | fail | 0.47/0.94 | 584.302 | 354.653 | 1.648 | 112.869 | 108.549 | 1.040 | absent | True / True |
| 2 | B | fail | 0.94/1.39 | 393.309 | 364.679 | 1.079 | 115.370 | 104.267 | 1.106 | 0 | True / True |
| 3 | A | fail | 1.39/1.56 | 591.970 | 349.234 | 1.695 | 130.192 | 107.471 | 1.211 | absent | True / True |
| 4 | B | fail | 1.56/1.47 | 419.626 | 373.147 | 1.125 | 138.860 | 106.329 | 1.306 | 0 | True / True |
| 5 | V4 | diagnostic-only | 1.47/1.59 | 1,346.287 | n/a | | n/a | n/a | | n/a | n/a |

### Round 4, fold on (2026-10-16-0727Z, `r23-ab-s1fold-20261016-on-4`)

| # | label | bench status | load1 before/after | initial native | initial rclone | init / rclone | delta native | delta rclone | delta / rclone | `directories_folded_total` | R25 warm / interrupted zero |
|---:|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|
| 0 | B | fail | 0.45/0.91 | 367.097 | 352.861 | 1.040 | 141.823 | 104.823 | 1.353 | 12 | True / True |
| 1 | A | fail | 0.91/1.7 | 525.305 | 343.717 | 1.528 | 113.133 | 106.349 | 1.064 | absent | True / True |
| 2 | B | fail | 1.7/1.6 | 388.549 | 350.780 | 1.108 | 122.687 | 106.175 | 1.156 | 12 | True / True |
| 3 | A | fail | 1.6/1.67 | 607.253 | 364.869 | 1.664 | 139.414 | 112.229 | 1.242 | absent | True / True |
| 4 | B | fail | 1.67/1.9 | 372.025 | 396.007 | 0.939 | 126.113 | 102.748 | 1.227 | 12 | True / True |
| 5 | V4 | diagnostic-only | 1.9/1.83 | 1,138.218 | n/a | | n/a | n/a | | n/a | n/a |

### Round 4, fold off (2026-10-16-0730Z, `r23-ab-s1fold-20261016-off-4`) (`BULKLOAD_DIRECTORY_FOLD=0` on B)

| # | label | bench status | load1 before/after | initial native | initial rclone | init / rclone | delta native | delta rclone | delta / rclone | `directories_folded_total` | R25 warm / interrupted zero |
|---:|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|
| 0 | B | fail | 0.45/0.76 | 448.544 | 355.715 | 1.261 | 116.519 | 108.747 | 1.071 | 0 | True / True |
| 1 | A | fail | 0.76/0.87 | 577.327 | 355.420 | 1.624 | 122.644 | 119.802 | 1.024 | absent | True / True |
| 2 | B | fail | 0.87/1.2 | 452.450 | 350.089 | 1.292 | 130.997 | 109.555 | 1.196 | 0 | True / True |
| 3 | A | fail | 1.2/1.41 | 579.804 | 354.072 | 1.638 | 133.930 | 103.158 | 1.298 | absent | True / True |
| 4 | B | fail | 1.41/1.45 | 419.762 | 357.864 | 1.173 | 136.104 | 102.729 | 1.325 | 0 | True / True |
| 5 | V4 | diagnostic-only | 1.45/1.75 | 1,279.904 | n/a | | n/a | n/a | | n/a | n/a |

### Round 5, fold on (2026-10-16-0732Z, `r23-ab-s1fold-20261016-on-5`)

| # | label | bench status | load1 before/after | initial native | initial rclone | init / rclone | delta native | delta rclone | delta / rclone | `directories_folded_total` | R25 warm / interrupted zero |
|---:|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|
| 0 | B | fail | 0.46/0.69 | 385.105 | 354.958 | 1.085 | 108.789 | 105.676 | 1.029 | 12 | True / True |
| 1 | A | fail | 0.69/0.67 | 562.451 | 366.292 | 1.536 | 132.603 | 110.023 | 1.205 | absent | True / True |
| 2 | B | fail | 0.67/0.65 | 402.179 | 359.941 | 1.117 | 124.374 | 105.685 | 1.177 | 12 | True / True |
| 3 | A | fail | 0.65/0.63 | 584.929 | 360.118 | 1.624 | 138.702 | 103.115 | 1.345 | absent | True / True |
| 4 | B | fail | 0.63/0.61 | 411.311 | 358.841 | 1.146 | 113.914 | 104.099 | 1.094 | 12 | True / True |
| 5 | V4 | diagnostic-only | 0.61/0.64 | 1,283.440 | n/a | | n/a | n/a | | n/a | n/a |

### Round 5, fold off (2026-10-16-0736Z, `r23-ab-s1fold-20261016-off-5`) (`BULKLOAD_DIRECTORY_FOLD=0` on B)

| # | label | bench status | load1 before/after | initial native | initial rclone | init / rclone | delta native | delta rclone | delta / rclone | `directories_folded_total` | R25 warm / interrupted zero |
|---:|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|
| 0 | B | fail | 0.6/0.74 | 404.886 | 337.663 | 1.199 | 101.944 | 103.900 | 0.981 | 0 | True / True |
| 1 | A | fail | 0.74/0.85 | 634.809 | 359.766 | 1.765 | 139.565 | 107.232 | 1.302 | absent | True / True |
| 2 | B | fail | 0.85/1.17 | 453.078 | 357.511 | 1.267 | 113.314 | 106.204 | 1.067 | 0 | True / True |
| 3 | A | fail | 1.17/1.45 | 558.281 | 347.447 | 1.607 | 135.894 | 104.917 | 1.295 | absent | True / True |
| 4 | B | fail | 1.45/1.66 | 475.623 | 352.012 | 1.351 | 127.109 | 111.246 | 1.143 | 0 | True / True |
| 5 | V4 | diagnostic-only | 1.66/1.8 | 1,317.380 | n/a | | n/a | n/a | | n/a | n/a |
