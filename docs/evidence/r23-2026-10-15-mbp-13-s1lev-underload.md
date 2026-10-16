# R23 gate (a) S1 levers series - 2026-10-15 - rig mbp-13 (INFORMATIONAL UNDER LOAD - NOT A GATE SAMPLE)

> **Informational, under load, not a gate sample.** Five `r23_ab.py`
> rounds in `--under-load` mode (OI-1003-Q39, OI-1003-Q50), order `BABAB`,
> on the rig of record `mbp-13`. Every round's harness status is
> `complete-under-load-informational` with `of_record=false`. No number
> here is an R23 gate verdict or an S1 result in either direction
> (OI-1003-Q143, OI-1003-Q154). The R-N81 load gate and R-N91 quiet lanes
> were set aside by ruling; load1 is recorded before and after every rep.
>
> **It ran on the old reflinked corpus.** The sealed corpus these rounds
> read from was `~/git-bulkload/r23-corpus-v1/corpus`, whose 23 corpus
> files all shared their extents with another tree on the rig
> (`~/git-bulkload/sota/src`): 25 extents, 23 shared, by the FIEMAP probe
> in `~/git-bulkload/logs/reseal-corpus-v1-20261015.receipt`. That corpus
> was re-sealed unshared later the same day (OI-1003-Q166, 16:56Z), after
> this series. The bench timed reads of each round's working copy, which
> `r23_ab.py` then made with `shutil.copytree` (no clone on the rig's
> Python 3.12, per the same receipt); this version of the harness did not
> record the working copy's shared extents. Content identity was the same
> before and after. Compare this series with later ones only with that
> difference stated; the 2026-10-16 fold series
> ([evidence](r23-2026-10-16-mbp-13-s1fold-underload.md)) is on the
> unshared corpus.

Rulings: OI-1003-Q39, OI-1003-Q50, OI-1003-Q143, OI-1003-Q154 (the
levers), OI-1002-Q30 (A never decides), R23, R-N57, R-N134, R-N13.
Harness: `crates/bulkload-bench/scripts/r23_ab.py`.

- **B** = `59e5e60` (`59e5e60076da717ede9632c99750a0a564e03b70`, bench
  binary sha256 `46ba77422006b33c`): the S1 levers (verify once in
  loopback, the handed in-process wire, WAL-born stores, larger groups;
  OI-1003-Q154), `feat/s1-levers-20261015`. Compiled on the rig before the
  series (`~/git-bulkload/logs/build-s1lev-20261015.log`); no compile
  inside any round.
- **A** = `8a24161` (`8a241614ba96`, sha256 `9fa36c3c38130863`), the rig's
  pinned control; it never decides anything (OI-1002-Q30).
- **V4** = `41bf9a4` (`41bf9a4fa233`, sha256 `a5acdebb85ef9374`,
  `pmset-shim`): diagnostic only.
- **rclone**: `rclone v1.74.4`,
  `/nix/store/v5xbkynmfg8ml23d82m09s802nmj2r6f-rclone-1.74.4`, binary
  sha256 `41b54044d938cadf392f53cb65560efff7c03e915adf809e8008b2c653b76e0e`,
  `matches_committed_pin=true` (flake.lock nixpkgs
  `241313f4e8e508cb9b13278c2b0fa25b9ca27163`, OI-1003-Q105), the
  r23-2026-09-18 flags, no sync flag.
- **Corpus**: `r23-corpus-v1`, 23 regular files, 239,819,837 bytes,
  content identity
  `f4a7619f7b88f2e0e1eeadb5995c79a809eafa8b4a8cf3f82d1ff6f29b5b07c7`,
  verified before the first and after the last rep of every round.
- **Rig**: `mbp-13`, MacBookPro12,1, i7-5557U (2 cores, 4 threads),
  15.3 GiB, Linux 6.19.5-11.xr.el10, work root on `xfs`, AC on every row.
  Page cache never dropped. Native seal `fsync`, durability `group`.
- **Window**: 2026-10-15 08:13:32Z (first rep) to 08:24:36Z (series done;
  `~/git-bulkload/logs/series-s1lev-20261015.log`, five rounds, every one
  `exit=0`).
- **Sources** (read-only, on `mbp-13`):
  `~/git-bulkload/logs/r23-s1lev-20261015-underload-{1..5}.md` and
  `r23-s1lev-20261015-{1..5}.log`; every counter is in
  `~/git-bulkload/runs/r23-ab-s1lev-20261015-{1..5}/r23-ab.json`.

Every value is the bench's own per-rep median in ms. A round's "B median"
is the median of its three B reps' bench medians.

## Per-round B medians

| round | initial native | initial rclone | init / rclone | delta native | delta rclone | delta / rclone | B bench statuses |
|---:|---:|---:|---:|---:|---:|---:|---|
| 1 | 360.783 | 347.433 | 1.038 | 105.532 | 104.434 | 1.011 | fail x3 |
| 2 | 419.913 | 355.548 | 1.181 | 123.973 | 110.599 | 1.121 | fail x3 |
| 3 | 402.913 | 362.676 | 1.111 | 111.162 | 106.514 | 1.044 | fail x3 |
| 4 | 402.360 | 356.847 | 1.128 | 115.804 | 104.739 | 1.106 | fail x3 |
| 5 | 406.341 | 358.118 | 1.135 | 119.594 | 107.208 | 1.116 | fail x3 |
| median of rounds | 402.913 | 356.847 | 1.129 | 115.804 | 106.514 | 1.087 | |

Over all fifteen B reps: initial native 402.360 against rclone 356.847
(per-rep ratio 0.953 to 1.285, 1 of 15 under 1.00x, none at or under
0.90x); delta 116.638 against 107.208 (0.919 to 1.216, 4 of 15 under
1.00x). A over its ten reps: initial 514.791 against 356.493, delta
122.128 against 107.562.

## Counters (every B native sample, per copy)

| counter | initial copy | 1 % delta copy |
|---|---:|---:|
| `durable_groups` | 2 | 1 |
| `flush_fs_count` | 2 | 0 |
| `flush_dir_count` | 2 | 0 |
| `flush_dir_barrier_count` | 8 | 1 |
| `flush_full_count` | 0 | 0 |
| `sqlite_directory_complete_commits` | 1 | 0 |
| `sqlite_commits_total` | 6 | 1 |
| `full_flushes_total` | 7 | 1 |
| `directories_finished` | 4 | 0 |
| `directories_folded` | (absent: before the fold) | (absent) |
| `source_bytes_read` | 239,819,837 | 33,555,209 |

Receive tail (median over the B reps' per-rep medians, initial copy):
29.65 ms (28.22 to 32.29); drain 49.96 ms. R25 in every B and A rep: warm
and interrupted resume read zero.

## Reading

- **B was slower than plain rclone in every round median**, initial
  1.038x to 1.181x (median of round medians 402.913 against 356.847 ms,
  1.13x), delta 1.011x to 1.121x. No round reaches Q143's 0.90x margin.
- **The rounds spread by about 59 ms** (360.783 to 419.913 ms), more than
  the fold could remove; the fold note (section 5) reads it that way.
- **B against A**: initial 402.360 against 514.791 ms over all reps
  (0.78x); the spreads do not overlap (B 358.224 to 456.912, A 467.249 to
  559.220).
- **Load**: load1 0.40 to 2.40 over the B and A reps; round 4's last B
  rep ended at 2.40.

## Per-rep results

### Round 1 (2026-10-15-0813Z, `r23-ab-s1lev-20261015-1`)

| # | label | bench status | load1 before/after | initial native | initial rclone | init / rclone | delta native | delta rclone | delta / rclone | R25 warm / interrupted zero |
|---:|---|---|---|---:|---:|---:|---:|---:|---:|---|
| 0 | B | fail | 0.44/0.57 | 390.822 | 332.976 | 1.174 | 105.215 | 100.191 | 1.050 | True / True |
| 1 | A | fail | 0.57/0.64 | 467.249 | 343.062 | 1.362 | 121.641 | 103.572 | 1.174 | True / True |
| 2 | B | fail | 0.64/0.7 | 360.783 | 347.433 | 1.038 | 105.532 | 107.439 | 0.982 | True / True |
| 3 | A | fail | 0.7/0.99 | 512.010 | 354.844 | 1.443 | 112.405 | 112.626 | 0.998 | True / True |
| 4 | B | fail | 0.99/1.28 | 358.224 | 375.817 | 0.953 | 127.014 | 104.434 | 1.216 | True / True |
| 5 | V4 | diagnostic-only | 1.28/1.42 | 1,168.249 | n/a | | n/a | n/a | | n/a |

### Round 2 (2026-10-15-0815Z, `r23-ab-s1lev-20261015-2`)

| # | label | bench status | load1 before/after | initial native | initial rclone | init / rclone | delta native | delta rclone | delta / rclone | R25 warm / interrupted zero |
|---:|---|---|---|---:|---:|---:|---:|---:|---:|---|
| 0 | B | fail | 0.4/1.57 | 388.302 | 363.998 | 1.067 | 116.638 | 112.127 | 1.040 | True / True |
| 1 | A | fail | 1.57/1.57 | 496.501 | 367.223 | 1.352 | 124.109 | 108.987 | 1.139 | True / True |
| 2 | B | fail | 1.57/1.41 | 419.913 | 353.910 | 1.186 | 123.973 | 108.383 | 1.144 | True / True |
| 3 | A | fail | 1.41/1.74 | 517.572 | 356.876 | 1.450 | 136.790 | 106.339 | 1.286 | True / True |
| 4 | B | fail | 1.74/1.77 | 456.912 | 355.548 | 1.285 | 128.290 | 110.599 | 1.160 | True / True |
| 5 | V4 | diagnostic-only | 1.77/1.87 | 1,100.717 | n/a | | n/a | n/a | | n/a |

### Round 3 (2026-10-15-0818Z, `r23-ab-s1lev-20261015-3`)

| # | label | bench status | load1 before/after | initial native | initial rclone | init / rclone | delta native | delta rclone | delta / rclone | R25 warm / interrupted zero |
|---:|---|---|---|---:|---:|---:|---:|---:|---:|---|
| 0 | B | fail | 0.56/0.84 | 421.400 | 362.676 | 1.162 | 120.211 | 102.410 | 1.174 | True / True |
| 1 | A | fail | 0.84/1.58 | 529.934 | 375.056 | 1.413 | 110.018 | 107.752 | 1.021 | True / True |
| 2 | B | fail | 1.58/1.5 | 385.063 | 355.015 | 1.085 | 109.689 | 119.337 | 0.919 | True / True |
| 3 | A | fail | 1.5/1.35 | 501.865 | 358.750 | 1.399 | 122.615 | 109.959 | 1.115 | True / True |
| 4 | B | fail | 1.35/1.67 | 402.913 | 362.696 | 1.111 | 111.162 | 106.514 | 1.044 | True / True |
| 5 | V4 | diagnostic-only | 1.67/1.78 | 1,121.695 | n/a | | n/a | n/a | | n/a |

### Round 4 (2026-10-15-0821Z, `r23-ab-s1lev-20261015-4`)

| # | label | bench status | load1 before/after | initial native | initial rclone | init / rclone | delta native | delta rclone | delta / rclone | R25 warm / interrupted zero |
|---:|---|---|---|---:|---:|---:|---:|---:|---:|---|
| 0 | B | fail | 0.76/0.94 | 406.939 | 364.281 | 1.117 | 106.549 | 108.354 | 0.983 | True / True |
| 1 | A | fail | 0.94/1.76 | 483.270 | 360.517 | 1.340 | 140.224 | 106.325 | 1.319 | True / True |
| 2 | B | fail | 1.76/1.89 | 396.428 | 356.847 | 1.111 | 121.384 | 104.739 | 1.159 | True / True |
| 3 | A | fail | 1.89/1.99 | 551.952 | 356.111 | 1.550 | 142.351 | 113.543 | 1.254 | True / True |
| 4 | B | fail | 1.99/2.4 | 402.360 | 345.368 | 1.165 | 115.804 | 104.342 | 1.110 | True / True |
| 5 | V4 | diagnostic-only | 2.4/2.29 | 1,124.157 | n/a | | n/a | n/a | | n/a |

### Round 5 (2026-10-15-0823Z, `r23-ab-s1lev-20261015-5`)

| # | label | bench status | load1 before/after | initial native | initial rclone | init / rclone | delta native | delta rclone | delta / rclone | R25 warm / interrupted zero |
|---:|---|---|---|---:|---:|---:|---:|---:|---:|---|
| 0 | B | fail | 0.59/0.73 | 421.441 | 367.952 | 1.145 | 119.594 | 106.130 | 1.127 | True / True |
| 1 | A | fail | 0.73/1.47 | 526.794 | 350.142 | 1.505 | 111.879 | 105.580 | 1.060 | True / True |
| 2 | B | fail | 1.43/1.48 | 406.341 | 356.748 | 1.139 | 106.773 | 109.543 | 0.975 | True / True |
| 3 | A | fail | 1.48/1.8 | 559.220 | 352.753 | 1.585 | 111.471 | 107.373 | 1.038 | True / True |
| 4 | B | fail | 1.8/1.77 | 388.981 | 358.118 | 1.086 | 127.675 | 107.208 | 1.191 | True / True |
| 5 | V4 | diagnostic-only | 1.77/2.03 | 1,283.458 | n/a | | n/a | n/a | | n/a |
