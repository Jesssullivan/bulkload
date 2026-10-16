# R23 gate (a) S1 delta-lever series - 2026-10-16 - rig mbp-13 (INFORMATIONAL UNDER LOAD - NOT A GATE SAMPLE)

> **Informational, under load, not a gate sample.** Ten `r23_ab.py` runs in
> `--under-load` mode (OI-1003-Q39, OI-1003-Q50), order `BABAB`, on the rig
> of record `mbp-13`, on the unshared re-seal of corpus v1 (OI-1003-Q166).
> Five rounds. Each round has one run with the delta lever on and one with
> it off, and the arm that goes first alternates by round. Every run's
> harness status is `complete-under-load-informational` with
> `of_record=false`. No number here is an R23 gate verdict or an S1 result
> in either direction. S1 stays gated against plain rclone, and a gated
> sample needs an informational median at or under 0.90x plain rclone first
> (OI-1003-Q143). The R-N81 load gate and R-N91 quiet lanes were set aside
> by ruling; load1 is recorded before and after every rep.

Rulings: OI-1003-Q39, OI-1003-Q50, OI-1003-Q143, OI-1003-Q154 (the delta
lever), OI-1003-Q155, OI-1003-Q173 (review before merge), OI-1003-Q165 and
Q166 (no clone; the unshared corpus), OI-1002-Q30 (A never decides), R23,
R-N57, R-N13. Harness: `crates/bulkload-bench/scripts/r23_ab.py`. The lever
and its diagnosis are in `docs/agent-notes/2026-10-16-delta-stream.md` and
`docs/agent-notes/2026-10-16-delta-regression.md`.

- **B** = `19e4b36` (`19e4b36573cd703352e6dc2991207aa0d33fe446`, bench
  binary sha256 `a28aca543f6589ce`): the head of `perf/delta-stream-20261016`
  (PR #235) when the series ran, on `main` `508eda7`. It holds the fold
  (on by default) and the opt-in delta lever. It was compiled once on the
  rig before the series (`~/git-bulkload/logs/build-s1delta-20261016.log`,
  `release`, 13:27:49Z to 13:31:20Z). Nothing was compiled inside any
  round.
- **Arms.** The lever is opt-in: `transfer.rs` turns it on only when
  `BULKLOAD_LOCAL_NEW_PATH_STREAM` is `1`. So both arms name the knob, and
  the run with no `--b-env` was not used, because it would have measured
  the lever off. *stream on*: `--b-env BULKLOAD_LOCAL_NEW_PATH_STREAM=1`.
  *stream off*: `--b-env BULKLOAD_LOCAL_NEW_PATH_STREAM=0`. The knob
  reaches the B reps only: every A and V4 rep's `env` is empty. Both arms
  run the same B binary, and the fold is at its default (on) in both: B's
  `directories_folded_total` is 12 in every rep of both arms.
- **Order.** Odd rounds ran stream on first; even rounds ran stream off
  first. This removes the order confound of the fold series
  (`r23-2026-10-16-mbp-13-s1fold-underload.md`).
- **A** = `8a24161` (`8a241614ba96`, sha256 `9fa36c3c38130863`): the rig's
  pinned control. It predates the levers and never decides anything
  (OI-1002-Q30).
- **V4** = `41bf9a4` (`41bf9a4fa233`, sha256 `a5acdebb85ef9374`,
  `pmset-shim`): diagnostic only, the dedup reference.
- **rclone**: `rclone v1.74.4`,
  `/nix/store/v5xbkynmfg8ml23d82m09s802nmj2r6f-rclone-1.74.4`, binary
  sha256 `41b54044d938cadf392f53cb65560efff7c03e915adf809e8008b2c653b76e0e`,
  `matches_committed_pin=true` (flake.lock nixpkgs
  `241313f4e8e508cb9b13278c2b0fa25b9ca27163`, OI-1003-Q105). It ran with the
  r23-2026-09-18 flags and no sync flag.
- **Corpus**: `~/git-bulkload/r23-corpus-v1-unshared/corpus` (the
  OI-1003-Q166 re-seal), 23 regular files, 239,819,837 bytes, content
  identity
  `f4a7619f7b88f2e0e1eeadb5995c79a809eafa8b4a8cf3f82d1ff6f29b5b07c7`. It was
  verified before the first rep and after the last rep of every run. The
  working copy was made by `read-write (os.read/os.write; OI-1003-Q165)`.
  FIEMAP-shared extents were 0 in the sealed corpus, in the working copy,
  and in every rep's fixture after every rep. The bench's
  `sealed_corpus_blake3` matched in every rep of its run.
- **Delta copy**: one changed file, 33,555,209 bytes. The bench removes the
  mutated private target before timing, so on B the file is a path new to
  a non-empty destination. That is the case the diagnosis found and the one
  the lever changes.
- **Rig**: `mbp-13`, MacBookPro12,1, i7-5557U (2 cores, 4 threads),
  15.3 GiB, Linux 6.19.5-11.xr.el10, work root on `xfs`, AC on every row.
  The page cache was never dropped, so every timed arm started source-hot.
  Native seal `fsync`, durability `group`. Loopback hand-off: B `on` in
  every run.
- **Window**: the first rep was admitted at 13:33:53Z and the series was
  done at 13:58:25Z. Before each run the script waited for load1 < 0.5,
  and each run started at load1 0.39 to 0.48. The series log,
  `~/git-bulkload/logs/series-s1delta-20261016.log`, shows ten runs, all
  `exit=0`.
- **Sources** (read-only, on `mbp-13`):
  - the script, `~/git-bulkload/run-s1delta-series-20261016.sh`;
  - `~/git-bulkload/logs/r23-s1delta-20261016-{on,off}-underload-{1..5}.md`;
  - `r23-s1delta-20261016-{on,off}-{1..5}.log`;
  - `series-s1delta-20261016.log`;
  - every counter, in
    `~/git-bulkload/runs/r23-ab-s1delta-20261016-{on,off}-{1..5}/r23-ab.json`.

Every value is the bench's own per-rep median in ms. A ratio is native
over rclone, either for the same rep or for the same round's B medians. A
round's "B median" is the median of its three B reps' bench medians. That
is the figure each run's evidence file reports as "median of the bench's
informational medians".

## Per-round B medians

| round | order | arm | initial native | initial rclone | init / rclone | delta native | delta rclone | delta / rclone | delta first data | B bench statuses |
|---:|---|---|---:|---:|---:|---:|---:|---:|---:|---|
| 1 | on first | on | 408.501 | 355.729 | 1.148 | 84.351 | 105.467 | 0.800 | 1.065 | fail x3 |
| 1 | on first | off | 405.902 | 356.170 | 1.140 | 121.766 | 114.822 | 1.060 | 52.718 | fail x3 |
| 2 | off first | off | 415.186 | 357.143 | 1.163 | 122.661 | 103.610 | 1.184 | 52.426 | fail x3 |
| 2 | off first | on | 462.488 | 359.605 | 1.286 | 88.578 | 103.270 | 0.858 | 0.946 | fail x3 |
| 3 | on first | on | 419.155 | 357.382 | 1.173 | 84.269 | 104.901 | 0.803 | 1.078 | fail x3 |
| 3 | on first | off | 427.415 | 358.625 | 1.192 | 126.437 | 105.313 | 1.201 | 52.563 | fail x3 |
| 4 | off first | off | 440.657 | 351.850 | 1.252 | 132.360 | 103.905 | 1.274 | 51.978 | fail x3 |
| 4 | off first | on | 440.079 | 349.582 | 1.259 | 84.479 | 105.387 | 0.802 | 0.981 | fail x3 |
| 5 | on first | on | 445.362 | 358.346 | 1.243 | 89.928 | 105.613 | 0.851 | 1.167 | fail x3 |
| 5 | on first | off | 440.382 | 360.380 | 1.222 | 124.207 | 105.403 | 1.178 | 52.263 | fail x3 |

"Delta first data" is the median, over the round's B reps, of each rep's
`recv_first_data_ms` median for the delta copy.

Median of the five round medians:

| arm | initial native | initial rclone | init / rclone | delta native | delta rclone | delta / rclone | delta first data |
|---|---:|---:|---:|---:|---:|---:|---:|
| stream on (fold on) | 440.079 | 357.382 | **1.231** | 84.479 | 105.387 | **0.802** | 1.065 |
| stream off (fold on) | 427.415 | 357.143 | 1.197 | 124.207 | 105.313 | 1.179 | 52.426 |

The medians of the per-round ratios give the same picture: stream on is
1.243 for the initial copy and 0.803 for the delta, and stream off is
1.192 and 1.184. Over all fifteen B reps of each arm, the initial medians
are 439.575 ms on and 427.415 ms off, and the delta medians are 84.835 ms
on and 124.342 ms off.

### Paired rounds (off minus on), with the A control

A runs the same binary in both arms of a round, so its movement between the
round's two runs is the rig's own drift over those few minutes.

| round | order | B initial, off - on | A initial, on / off | A initial, off - on | B delta, off - on | A delta, on / off | A delta, off - on | B delta less A drift | B delta first data, off - on |
|---:|---|---:|---|---:|---:|---|---:|---:|---:|
| 1 | on first | -2.599 | 563.495 / 582.507 | 19.012 | 37.415 | 135.141 / 130.734 | -4.407 | 41.822 | 51.653 |
| 2 | off first | -47.302 | 623.663 / 551.635 | -72.029 | 34.083 | 137.461 / 133.430 | -4.030 | 38.113 | 51.480 |
| 3 | on first | 8.260 | 600.393 / 588.328 | -12.065 | 42.168 | 139.325 / 135.696 | -3.629 | 45.797 | 51.485 |
| 4 | off first | 0.578 | 594.176 / 609.458 | 15.283 | 47.881 | 134.113 / 133.114 | -0.999 | 48.880 | 50.998 |
| 5 | on first | -4.980 | 627.678 / 628.282 | 0.604 | 34.279 | 141.916 / 138.673 | -3.243 | 37.522 | 51.095 |

Medians of the paired differences: initial -2.599 ms, delta 37.415 ms,
delta less A's drift 41.822 ms, delta first data 51.480 ms.

## Counters (every B native sample, per copy)

The lever's counters are exact and identical in every B rep of an arm. A's
build predates them (absent).

| counter (delta copy) | stream on | stream off |
|---|---:|---:|
| `transfer_manifests_asked` (`WantManifest`) | 0 | 1 |
| `transfer_new_paths_streamed` (`Send`, lever) | 1 | 0 |
| `read_source_file_bytes` | 33,555,209 | 33,555,209 |
| `read_dest_local_reuse_bytes` | 0 | 0 |
| `blake3_dest_local_reuse_bytes` | 0 | 0 |
| `blake3_total_bytes` | 33,555,285 to 33,555,286 | 33,555,285 to 33,555,286 |
| `wire_handed_bytes` | 33,555,209 | 33,555,209 |
| `wire_bytes_received` (control frames and chunk headers) | 30,123 to 30,131 | 44,848 to 44,856 |
| `files_materialized` | 1 | 1 |

The decision runs one way in each arm. With the lever on, the one new path
is streamed and no manifest is asked for. With it off, the destination asks
for the manifest (`WantManifest`), as `main` does. In both arms the source
reads the same bytes, the destination's local reuse reads and hashes
nothing, and the same single file is placed. What the lever removes is the
wait before the first byte, not a read.

- **Initial copy, warm resume and interrupted resume:** both counters are
  0 in both arms. The initial copy goes into an empty destination, and the
  two resumes place nothing. In the initial copy, `directories_folded` is
  4 per copy in both arms (fold on).
- **Receive timers:** each figure is the median over the B reps' per-rep
  medians, for the delta copy.
  - `recv_first_data_ms`: 1.065 with the lever on, 52.426 off.
  - `recv_stream_ms`: 82.218 on, 122.016 off.
  - `recv_drain_ms`: 38.206 on, 35.419 off.
  - `recv_tail_ms`: 0.259 on, 0.273 off.

  For the initial copy, `recv_tail_ms` is 0.698 on and 0.709 off (the
  fold).
- **R25:** in every B and A rep, the warm resume and the interrupted
  resume read zero (`r25_warm_zero` and `r25_interrupted_zero` true), and
  peak memory stayed under 2 GiB.
- **Bytes received:** 239,819,837 for B and A (the whole payload), and
  219,946,368 for V4. So the #88 duplicate share is 0.0829, as before.

## Reading

- **Delta copy: the lever reaches Q143's margin.** The median of the
  round medians is **0.802x** plain rclone with stream on (84.479 against
  105.387 ms), against 1.179x with stream off.
  - Every stream-on round median is between 0.800 and 0.858.
  - 14 of the 15 stream-on B reps are at or under 0.90x (0.784 to 0.860),
    and the 15th is 0.924 (round 5, rep 0).
  - No stream-off B rep is under 1.00x (1.023 to 1.317).
  - The diagnosis predicted about 0.89x (`docs/slo.md`). The bench shows
    more than that.
- **The delta gain is the lever's, and order does not explain it.** Stream
  on was faster in every paired round, by 34.1 to 47.9 ms (median 37.4).
  That holds in the two off-first rounds (34.1, 47.9) as in the three
  on-first rounds (37.4, 42.2, 34.3).
  - A's delta moved by only -1.0 to -4.4 ms between a round's two runs.
    With that drift taken off, the gain is 37.5 to 48.9 ms.
  - First data fell by 51.0 to 51.7 ms in every round, which matches the
    diagnosis (51 ms against 0.8 ms; on the rig, 51.5 against 0.9 ms).
- **Initial copy: Q143's 0.90x is not reached.** With fold on and stream
  on, the median of the round medians is **1.231x** plain rclone (440.079
  against 357.382 ms). The round medians run from 1.148 to 1.286, and no B
  rep in either arm is under 1.00x: the stream-on per-rep ratios run from
  1.108 to 1.310.
  - The lever does not act on the initial copy: both counters are 0, and
    the destination is empty.
  - The paired initial differences, -47.3 to 8.3 ms (median -2.6), follow
    A's own swings (-72.0 to 19.0 ms). They are noise.
- **The initial copy here is slower than in the morning's fold series,
  for both B and A.** That series ran at 07:12Z to 07:36Z with B
  `51d1d4c`.
  - All fifteen fold-on B reps: 382.472 ms then, about 434 ms now (439.575
    on, 427.415 off). The receive-stream timer accounts for it: 346.8 ms
    then, 366 to 389 ms now.
  - A, the same binary in both series: 562.8 ms then, 588 to 603 ms now.
  - The two series are about six hours apart and have different B commits,
    so this series cannot split B's rise between rig drift and the commits
    `main` gained between `51d1d4c` and `508eda7`. A comparison of the same
    commits at the same time would answer it. This series only reads the
    lever.
- **Bench status:** every B rep's status is `fail` in both arms, because
  the initial copy loses to rclone. Under load that status is
  informational and decides nothing.
- **Load:** load1 ran from 0.44 to 2.17 over the B and A reps, under
  R-N81's 2.5 throughout. The R-N91 quiet lanes were not held, so the
  series stays under-load.
- **What this does not settle:** whether handed bytes count as wire bytes
  for WP0(c) inequality 2 is an open operator ruling. Until it is ruled,
  the lever is A/B only, and none of these numbers is a gate result.

## Per-rep results

### Round 1, stream on (2026-10-16T13:33:53Z, `r23-ab-s1delta-20261016-on-1`) (`BULKLOAD_LOCAL_NEW_PATH_STREAM=1` on B)

| # | label | bench status | load1 before/after | initial native | initial rclone | init / rclone | delta native | delta rclone | delta / rclone | delta first data ms | delta manifests asked / new paths streamed | R25 warm / interrupted zero |
|---:|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| 0 | B | fail | 0.5/0.78 | 408.501 | 325.212 | 1.256 | 80.519 | 100.532 | 0.801 | 1.082 | 0 / 1 | True / True |
| 1 | A | fail | 0.78/1.45 | 520.369 | 350.206 | 1.486 | 131.518 | 105.920 | 1.242 | 57.376 | absent / absent | True / True |
| 2 | B | fail | 1.45/1.55 | 394.211 | 355.729 | 1.108 | 84.669 | 105.467 | 0.803 | 1.065 | 0 / 1 | True / True |
| 3 | A | fail | 1.55/1.55 | 606.621 | 368.480 | 1.646 | 138.764 | 108.163 | 1.283 | 51.401 | absent / absent | True / True |
| 4 | B | fail | 1.55/1.87 | 447.203 | 375.526 | 1.191 | 84.351 | 107.165 | 0.787 | 1.013 | 0 / 1 | True / True |
| 5 | V4 | diagnostic-only | 1.87/1.88 | 1,366.884 | n/a | | n/a | n/a | | n/a | n/a | n/a |

### Round 1, stream off (2026-10-16T13:36:20Z, `r23-ab-s1delta-20261016-off-1`) (`BULKLOAD_LOCAL_NEW_PATH_STREAM=0` on B)

| # | label | bench status | load1 before/after | initial native | initial rclone | init / rclone | delta native | delta rclone | delta / rclone | delta first data ms | delta manifests asked / new paths streamed | R25 warm / interrupted zero |
|---:|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| 0 | B | fail | 0.53/0.91 | 405.902 | 365.144 | 1.112 | 119.032 | 105.216 | 1.131 | 52.718 | 1 / 0 | True / True |
| 1 | A | fail | 0.91/1.15 | 592.837 | 359.796 | 1.648 | 131.041 | 107.721 | 1.216 | 52.577 | absent / absent | True / True |
| 2 | B | fail | 1.15/1.42 | 460.108 | 346.397 | 1.328 | 121.766 | 114.822 | 1.060 | 51.051 | 1 / 0 | True / True |
| 3 | A | fail | 1.42/1.43 | 572.177 | 355.836 | 1.608 | 130.428 | 104.293 | 1.251 | 52.085 | absent / absent | True / True |
| 4 | B | fail | 1.43/1.8 | 403.206 | 356.170 | 1.132 | 132.618 | 120.693 | 1.099 | 52.754 | 1 / 0 | True / True |
| 5 | V4 | diagnostic-only | 1.8/1.82 | 1,418.113 | n/a | | n/a | n/a | | n/a | n/a | n/a |

### Round 2, stream off (2026-10-16T13:41:45Z, `r23-ab-s1delta-20261016-off-2`) (`BULKLOAD_LOCAL_NEW_PATH_STREAM=0` on B)

| # | label | bench status | load1 before/after | initial native | initial rclone | init / rclone | delta native | delta rclone | delta / rclone | delta first data ms | delta manifests asked / new paths streamed | R25 warm / interrupted zero |
|---:|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| 0 | B | fail | 1.19/1.24 | 451.900 | 364.250 | 1.241 | 133.988 | 103.417 | 1.296 | 52.426 | 1 / 0 | True / True |
| 1 | A | fail | 1.24/1.27 | 566.117 | 357.521 | 1.583 | 130.504 | 103.388 | 1.262 | 51.818 | absent / absent | True / True |
| 2 | B | fail | 1.27/1.23 | 415.186 | 350.683 | 1.184 | 117.216 | 103.610 | 1.131 | 51.848 | 1 / 0 | True / True |
| 3 | A | fail | 1.23/1.45 | 537.152 | 351.526 | 1.528 | 136.357 | 104.385 | 1.306 | 52.955 | absent / absent | True / True |
| 4 | B | fail | 1.45/1.46 | 408.806 | 357.143 | 1.145 | 122.661 | 107.986 | 1.136 | 52.588 | 1 / 0 | True / True |
| 5 | V4 | diagnostic-only | 1.46/1.67 | 1,285.012 | n/a | | n/a | n/a | | n/a | n/a | n/a |

### Round 2, stream on (2026-10-16T13:44:12Z, `r23-ab-s1delta-20261016-on-2`) (`BULKLOAD_LOCAL_NEW_PATH_STREAM=1` on B)

| # | label | bench status | load1 before/after | initial native | initial rclone | init / rclone | delta native | delta rclone | delta / rclone | delta first data ms | delta manifests asked / new paths streamed | R25 warm / interrupted zero |
|---:|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| 0 | B | fail | 0.45/0.62 | 469.419 | 370.986 | 1.265 | 84.835 | 102.378 | 0.829 | 0.927 | 0 / 1 | True / True |
| 1 | A | fail | 0.62/0.61 | 667.031 | 348.290 | 1.915 | 145.567 | 106.103 | 1.372 | 52.683 | absent / absent | True / True |
| 2 | B | fail | 0.61/0.83 | 462.488 | 353.127 | 1.310 | 90.566 | 105.371 | 0.859 | 0.946 | 0 / 1 | True / True |
| 3 | A | fail | 0.83/1.03 | 580.296 | 344.148 | 1.686 | 129.354 | 102.374 | 1.264 | 52.596 | absent / absent | True / True |
| 4 | B | fail | 1.03/0.95 | 416.503 | 359.605 | 1.158 | 88.578 | 103.270 | 0.858 | 1.124 | 0 / 1 | True / True |
| 5 | V4 | diagnostic-only | 0.95/0.95 | 1,292.844 | n/a | | n/a | n/a | | n/a | n/a | n/a |

### Round 3, stream on (2026-10-16T13:45:55Z, `r23-ab-s1delta-20261016-on-3`) (`BULKLOAD_LOCAL_NEW_PATH_STREAM=1` on B)

| # | label | bench status | load1 before/after | initial native | initial rclone | init / rclone | delta native | delta rclone | delta / rclone | delta first data ms | delta manifests asked / new paths streamed | R25 warm / interrupted zero |
|---:|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| 0 | B | fail | 0.49/0.81 | 394.076 | 346.452 | 1.137 | 85.299 | 106.830 | 0.798 | 1.151 | 0 / 1 | True / True |
| 1 | A | fail | 0.81/0.99 | 630.875 | 357.411 | 1.765 | 139.134 | 105.644 | 1.317 | 52.014 | absent / absent | True / True |
| 2 | B | fail | 0.99/1.14 | 419.155 | 357.382 | 1.173 | 84.269 | 104.901 | 0.803 | 0.898 | 0 / 1 | True / True |
| 3 | A | fail | 1.14/1.64 | 569.911 | 356.313 | 1.599 | 139.515 | 100.640 | 1.386 | 51.459 | absent / absent | True / True |
| 4 | B | fail | 1.64/2.07 | 439.575 | 377.528 | 1.164 | 80.646 | 101.850 | 0.792 | 1.078 | 0 / 1 | True / True |
| 5 | V4 | diagnostic-only | 2.07/2.12 | 1,310.739 | n/a | | n/a | n/a | | n/a | n/a | n/a |

### Round 3, stream off (2026-10-16T13:48:23Z, `r23-ab-s1delta-20261016-off-3`) (`BULKLOAD_LOCAL_NEW_PATH_STREAM=0` on B)

| # | label | bench status | load1 before/after | initial native | initial rclone | init / rclone | delta native | delta rclone | delta / rclone | delta first data ms | delta manifests asked / new paths streamed | R25 warm / interrupted zero |
|---:|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| 0 | B | fail | 0.47/1.19 | 427.415 | 358.625 | 1.192 | 126.437 | 110.330 | 1.146 | 51.289 | 1 / 0 | True / True |
| 1 | A | fail | 1.19/1.16 | 583.397 | 370.961 | 1.573 | 137.579 | 107.766 | 1.277 | 52.784 | absent / absent | True / True |
| 2 | B | fail | 1.16/1.07 | 429.790 | 344.959 | 1.246 | 111.704 | 105.313 | 1.061 | 52.563 | 1 / 0 | True / True |
| 3 | A | fail | 1.07/0.98 | 593.258 | 377.769 | 1.570 | 133.812 | 102.406 | 1.307 | 53.594 | absent / absent | True / True |
| 4 | B | fail | 0.98/0.91 | 415.109 | 367.364 | 1.130 | 134.916 | 102.454 | 1.317 | 53.350 | 1 / 0 | True / True |
| 5 | V4 | diagnostic-only | 0.91/1.08 | 1,288.214 | n/a | | n/a | n/a | | n/a | n/a | n/a |

### Round 4, stream off (2026-10-16T13:50:21Z, `r23-ab-s1delta-20261016-off-4`) (`BULKLOAD_LOCAL_NEW_PATH_STREAM=0` on B)

| # | label | bench status | load1 before/after | initial native | initial rclone | init / rclone | delta native | delta rclone | delta / rclone | delta first data ms | delta manifests asked / new paths streamed | R25 warm / interrupted zero |
|---:|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| 0 | B | fail | 0.44/0.46 | 440.657 | 348.893 | 1.263 | 124.342 | 99.577 | 1.249 | 51.033 | 1 / 0 | True / True |
| 1 | A | fail | 0.46/0.7 | 600.987 | 355.484 | 1.691 | 141.203 | 103.487 | 1.364 | 58.430 | absent / absent | True / True |
| 2 | B | fail | 0.7/0.9 | 454.259 | 351.850 | 1.291 | 136.046 | 106.335 | 1.279 | 51.978 | 1 / 0 | True / True |
| 3 | A | fail | 0.9/1.16 | 617.930 | 362.704 | 1.704 | 125.025 | 103.801 | 1.204 | 51.295 | absent / absent | True / True |
| 4 | B | fail | 1.16/1.5 | 424.555 | 356.918 | 1.190 | 132.360 | 103.905 | 1.274 | 52.805 | 1 / 0 | True / True |
| 5 | V4 | diagnostic-only | 1.5/1.62 | 1,351.928 | n/a | | n/a | n/a | | n/a | n/a | n/a |

### Round 4, stream on (2026-10-16T13:52:34Z, `r23-ab-s1delta-20261016-on-4`) (`BULKLOAD_LOCAL_NEW_PATH_STREAM=1` on B)

| # | label | bench status | load1 before/after | initial native | initial rclone | init / rclone | delta native | delta rclone | delta / rclone | delta first data ms | delta manifests asked / new paths streamed | R25 warm / interrupted zero |
|---:|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| 0 | B | fail | 0.66/1.17 | 405.409 | 343.370 | 1.181 | 84.479 | 105.387 | 0.802 | 1.149 | 0 / 1 | True / True |
| 1 | A | fail | 1.17/1.95 | 599.211 | 351.556 | 1.704 | 130.774 | 110.393 | 1.185 | 51.917 | absent / absent | True / True |
| 2 | B | fail | 1.95/2.12 | 440.127 | 351.610 | 1.252 | 86.867 | 106.239 | 0.818 | 0.927 | 0 / 1 | True / True |
| 3 | A | fail | 2.12/2.17 | 589.141 | 356.643 | 1.652 | 137.451 | 106.007 | 1.297 | 52.659 | absent / absent | True / True |
| 4 | B | fail | 2.17/2.15 | 440.079 | 349.582 | 1.259 | 82.159 | 104.844 | 0.784 | 0.981 | 0 / 1 | True / True |
| 5 | V4 | diagnostic-only | 2.15/2.06 | 1,347.077 | n/a | | n/a | n/a | | n/a | n/a | n/a |

### Round 5, stream on (2026-10-16T13:55:02Z, `r23-ab-s1delta-20261016-on-5`) (`BULKLOAD_LOCAL_NEW_PATH_STREAM=1` on B)

| # | label | bench status | load1 before/after | initial native | initial rclone | init / rclone | delta native | delta rclone | delta / rclone | delta first data ms | delta manifests asked / new paths streamed | R25 warm / interrupted zero |
|---:|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| 0 | B | fail | 0.5/0.82 | 408.310 | 358.346 | 1.139 | 96.056 | 103.919 | 0.924 | 0.922 | 0 / 1 | True / True |
| 1 | A | fail | 0.82/1.02 | 642.532 | 370.097 | 1.736 | 146.966 | 105.456 | 1.394 | 57.915 | absent / absent | True / True |
| 2 | B | fail | 1.02/0.94 | 470.011 | 382.798 | 1.228 | 88.442 | 105.913 | 0.835 | 1.167 | 0 / 1 | True / True |
| 3 | A | fail | 0.94/1.5 | 612.823 | 354.874 | 1.727 | 136.866 | 105.626 | 1.296 | 52.009 | absent / absent | True / True |
| 4 | B | fail | 1.5/2.03 | 445.362 | 352.402 | 1.264 | 89.928 | 105.613 | 0.851 | 1.204 | 0 / 1 | True / True |
| 5 | V4 | diagnostic-only | 2.03/2.19 | 1,356.621 | n/a | | n/a | n/a | | n/a | n/a | n/a |

### Round 5, stream off (2026-10-16T13:57:30Z, `r23-ab-s1delta-20261016-off-5`) (`BULKLOAD_LOCAL_NEW_PATH_STREAM=0` on B)

| # | label | bench status | load1 before/after | initial native | initial rclone | init / rclone | delta native | delta rclone | delta / rclone | delta first data ms | delta manifests asked / new paths streamed | R25 warm / interrupted zero |
|---:|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| 0 | B | fail | 0.53/0.97 | 399.093 | 347.581 | 1.148 | 129.752 | 106.905 | 1.214 | 52.263 | 1 / 0 | True / True |
| 1 | A | fail | 0.97/1.13 | 673.300 | 353.240 | 1.906 | 136.029 | 107.527 | 1.265 | 52.329 | absent / absent | True / True |
| 2 | B | fail | 1.13/1.34 | 450.552 | 360.380 | 1.250 | 124.207 | 102.942 | 1.207 | 52.255 | 1 / 0 | True / True |
| 3 | A | fail | 1.34/1.51 | 583.264 | 354.858 | 1.644 | 141.317 | 106.766 | 1.324 | 51.759 | absent / absent | True / True |
| 4 | B | fail | 1.51/1.58 | 440.382 | 361.767 | 1.217 | 107.864 | 105.403 | 1.023 | 52.788 | 1 / 0 | True / True |
| 5 | V4 | diagnostic-only | 1.58/1.61 | 1,369.877 | n/a | | n/a | n/a | | n/a | n/a | n/a |
