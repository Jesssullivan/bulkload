# R23 gate (a) S1 throughput series - 2026-10-09 - rig mbp-13 (INFORMATIONAL UNDER LOAD - NEVER OF RECORD)

> **Informational, under load, never of record.** Five `r23_ab.py` rounds in
> `--under-load` mode (OI-1003-Q39, OI-1003-Q50), order `BABAB`, on the
> rig of record `mbp-13`, sealed corpus v1. Each round's harness status is
> `complete-under-load-informational` with `of_record=false`. No number here
> is an R23 gate verdict or an S1 result, in either direction; S1 stays
> gated against plain rclone (OI-1003-Q154). The R-N81 load gate and R-N91
> quiet lanes were set aside by ruling; load1 is recorded before and after
> every rep.

- **B** = `8a24161` (`8a241614ba96`, bench binary sha256 `9fa36c3c38130863`):
  S1 throughput (parallel store open, streaming write-back, directory
  batching, receive workers; OI-1003-Q143), stacked on #210.
- **A** = main `91d4294` (`91d429418c2d`, sha256 `4e957405af3d5fd8`): the
  drift control between the B reps; it never decides anything (OI-1002-Q30).
- **V4** = `41bf9a4` (`41bf9a4fa233`, sha256 `a5acdebb85ef9374`, `pmset-shim`):
  diagnostic only, the dedup reference.
- rclone: `rclone v1.74.4`, `/nix/store/v5xbkynmfg8ml23d82m09s802nmj2r6f-rclone-1.74.4`,
  sha256 `41b54044d938cadf`, the flake-pinned build (OI-1003-Q105), the
  r23-2026-09-18 flags, no sync flag.
- Corpus: `r23-corpus-v1`, 23 regular files, 239,819,837 bytes, content
  identity `f4a7619f7b88f2e0e1eeadb5995c79a809eafa8b4a8cf3f82d1ff6f29b5b07c7`,
  verified before the first and after the last rep of every round.
- Rig: `mbp-13`, MacBookPro12,1, i7-5557U (2 cores, 4 threads), 15.3 GiB,
  Linux 6.19.5-11.xr.el10, work root on `xfs`, AC on every row. Page cache
  never dropped (every timed arm starts source-hot). Native seal: `fsync`,
  durability `group`.
- Window: 2026-10-09 21:24Z to 21:36Z, every binary prebuilt (no compile
  inside a round). A first attempt at 15:55Z exited 2 in all five rounds
  before any rep ran; it left no report and is not part of this series.
- Sources: `~/git-bulkload/logs/r23-s1tp-20261009-underload-{1..5}.md` on
  `mbp-13` (every counter in `~/git-bulkload/runs/r23-ab-s1tp-20261009-{1..5}/r23-ab.json`
  there). Harness: `crates/bulkload-bench/scripts/r23_ab.py`.

Every value is the bench's own per-rep median in ms. "rclone+syncfs" adds
one `syncfs` of rclone's destination, run outside the timed window, back
in (equal durability, OI-1003-Q107); it is informational, and the bench's
verdict stays against rclone as shipped. Ratios are native / rclone of the
same rep.

## Per-round results

### Round 1 (2026-10-09-2124Z, work root `runs/r23-ab-s1tp-20261009-1`)

B bench statuses: fail x2, informational x1. load1 when rep 0 was admitted: 0.46.

| # | label | bench status | load1 before/after | initial native | initial rclone | initial rclone+syncfs | init / rclone | init / rclone+syncfs | delta native | delta rclone | delta rclone+syncfs | delta / rclone | delta / rclone+syncfs |
|---:|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | B | fail | 0.46/1.19 | 590.719 | 362.766 | 557.503 | 1.628 | 1.060 | 120.641 | 102.663 | 146.397 | 1.175 | 0.824 |
| 1 | A | fail | 1.19/1.93 | 853.067 | 375.487 | 615.721 | 2.272 | 1.385 | 131.379 | 103.996 | 138.172 | 1.263 | 0.951 |
| 2 | B | fail | 1.93/2.33 | 605.559 | 396.777 | 526.130 | 1.526 | 1.151 | 140.636 | 103.874 | 142.395 | 1.354 | 0.988 |
| 3 | A | fail | 2.33/2.28 | 923.159 | 361.928 | 631.715 | 2.551 | 1.461 | 145.019 | 105.256 | 160.724 | 1.378 | 0.902 |
| 4 | B | informational | 2.28/2.55 | 534.942 | 374.609 | 611.082 | 1.428 | 0.875 | 109.012 | 104.609 | 148.032 | 1.042 | 0.736 |
| 5 | V4 | diagnostic-only | 2.55/2.82 | 1,236.638 | n/a | n/a | | | n/a | n/a | n/a | | |

B medians this round: initial native 590.719 vs rclone 374.609; delta native 120.641 vs rclone 103.874.

### Round 2 (2026-10-09-2127Z, work root `runs/r23-ab-s1tp-20261009-2`)

B bench statuses: fail x3. load1 when rep 0 was admitted: 0.53.

| # | label | bench status | load1 before/after | initial native | initial rclone | initial rclone+syncfs | init / rclone | init / rclone+syncfs | delta native | delta rclone | delta rclone+syncfs | delta / rclone | delta / rclone+syncfs |
|---:|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | B | fail | 0.53/1.26 | 564.466 | 355.738 | 590.566 | 1.587 | 0.956 | 128.697 | 106.434 | 154.161 | 1.209 | 0.835 |
| 1 | A | fail | 1.26/1.67 | 900.279 | 355.494 | 562.116 | 2.532 | 1.602 | 127.963 | 102.613 | 153.446 | 1.247 | 0.834 |
| 2 | B | fail | 1.67/1.73 | 549.107 | 355.016 | 645.591 | 1.547 | 0.851 | 115.592 | 105.301 | 143.318 | 1.098 | 0.807 |
| 3 | A | fail | 1.73/2.19 | 926.834 | 360.390 | 636.050 | 2.572 | 1.457 | 136.243 | 104.651 | 148.470 | 1.302 | 0.918 |
| 4 | B | fail | 2.19/2.09 | 513.639 | 361.486 | 563.431 | 1.421 | 0.912 | 137.833 | 107.712 | 151.727 | 1.280 | 0.908 |
| 5 | V4 | diagnostic-only | 2.09/2.16 | 1,205.618 | n/a | n/a | | | n/a | n/a | n/a | | |

B medians this round: initial native 549.107 vs rclone 355.738; delta native 128.697 vs rclone 106.434.

### Round 3 (2026-10-09-2129Z, work root `runs/r23-ab-s1tp-20261009-3`)

B bench statuses: fail x3. load1 when rep 0 was admitted: 0.49.

| # | label | bench status | load1 before/after | initial native | initial rclone | initial rclone+syncfs | init / rclone | init / rclone+syncfs | delta native | delta rclone | delta rclone+syncfs | delta / rclone | delta / rclone+syncfs |
|---:|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | B | fail | 0.49/0.73 | 580.558 | 370.680 | 628.453 | 1.566 | 0.924 | 117.326 | 103.393 | 150.843 | 1.135 | 0.778 |
| 1 | A | fail | 0.73/0.79 | 876.622 | 349.228 | 568.606 | 2.510 | 1.542 | 135.181 | 113.200 | 168.224 | 1.194 | 0.804 |
| 2 | B | fail | 0.79/1.42 | 519.309 | 361.783 | 549.998 | 1.435 | 0.944 | 130.230 | 104.443 | 157.366 | 1.247 | 0.828 |
| 3 | A | fail | 1.42/1.59 | 871.962 | 361.383 | 567.697 | 2.413 | 1.536 | 133.240 | 105.425 | 142.705 | 1.264 | 0.934 |
| 4 | B | fail | 1.59/1.9 | 576.159 | 358.509 | 608.699 | 1.607 | 0.947 | 114.773 | 106.589 | 146.400 | 1.077 | 0.784 |
| 5 | V4 | diagnostic-only | 1.9/2.06 | 1,201.494 | n/a | n/a | | | n/a | n/a | n/a | | |

B medians this round: initial native 576.159 vs rclone 361.783; delta native 117.326 vs rclone 104.443.

### Round 4 (2026-10-09-2132Z, work root `runs/r23-ab-s1tp-20261009-4`)

B bench statuses: fail x3. load1 when rep 0 was admitted: 0.50.

| # | label | bench status | load1 before/after | initial native | initial rclone | initial rclone+syncfs | init / rclone | init / rclone+syncfs | delta native | delta rclone | delta rclone+syncfs | delta / rclone | delta / rclone+syncfs |
|---:|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | B | fail | 0.5/0.5 | 615.453 | 349.690 | 621.549 | 1.760 | 0.990 | 117.601 | 102.075 | 160.371 | 1.152 | 0.733 |
| 1 | A | fail | 0.5/0.75 | 927.045 | 357.366 | 621.859 | 2.594 | 1.491 | 139.527 | 102.014 | 150.461 | 1.368 | 0.927 |
| 2 | B | fail | 0.75/1.35 | 508.106 | 355.681 | 602.621 | 1.429 | 0.843 | 129.460 | 103.150 | 151.863 | 1.255 | 0.852 |
| 3 | A | fail | 1.35/1.5 | 905.938 | 354.273 | 551.765 | 2.557 | 1.642 | 160.235 | 104.302 | 153.692 | 1.536 | 1.043 |
| 4 | B | fail | 1.5/1.7 | 546.793 | 403.669 | 563.888 | 1.355 | 0.970 | 117.090 | 104.495 | 140.062 | 1.121 | 0.836 |
| 5 | V4 | diagnostic-only | 1.7/1.75 | 1,218.741 | n/a | n/a | | | n/a | n/a | n/a | | |

B medians this round: initial native 546.793 vs rclone 355.681; delta native 117.601 vs rclone 103.150.

### Round 5 (2026-10-09-2135Z, work root `runs/r23-ab-s1tp-20261009-5`)

B bench statuses: fail x3. load1 when rep 0 was admitted: 0.44.

| # | label | bench status | load1 before/after | initial native | initial rclone | initial rclone+syncfs | init / rclone | init / rclone+syncfs | delta native | delta rclone | delta rclone+syncfs | delta / rclone | delta / rclone+syncfs |
|---:|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | B | fail | 0.44/0.84 | 555.110 | 343.613 | 614.137 | 1.616 | 0.904 | 110.817 | 104.134 | 162.686 | 1.064 | 0.681 |
| 1 | A | fail | 0.84/1.16 | 904.810 | 350.521 | 593.739 | 2.581 | 1.524 | 145.160 | 104.733 | 160.545 | 1.386 | 0.904 |
| 2 | B | fail | 1.16/1.88 | 531.189 | 351.058 | 565.622 | 1.513 | 0.939 | 120.186 | 105.024 | 152.532 | 1.144 | 0.788 |
| 3 | A | fail | 1.88/2.12 | 862.154 | 357.072 | 665.945 | 2.415 | 1.295 | 150.870 | 103.475 | 154.219 | 1.458 | 0.978 |
| 4 | B | fail | 2.12/1.87 | 595.109 | 348.791 | 553.639 | 1.706 | 1.075 | 135.947 | 102.884 | 146.960 | 1.321 | 0.925 |
| 5 | V4 | diagnostic-only | 1.87/2.28 | 1,221.261 | n/a | n/a | | | n/a | n/a | n/a | | |

B medians this round: initial native 555.110 vs rclone 348.791; delta native 120.186 vs rclone 104.134.

## Combined summary (5 rounds: 15 B reps, 10 A reps)

Medians over the per-rep medians of every round. The rclone columns are
the rclone arms run inside that label's reps.

| arm | reps | initial native | initial rclone | initial rclone+syncfs | delta native | delta rclone | delta rclone+syncfs |
|---|---:|---:|---:|---:|---:|---:|---:|
| B | 15 | 555.110 | 358.509 | 590.566 | 120.186 | 104.443 | 150.843 |
| A | 10 | 902.544 | 357.219 | 604.730 | 137.885 | 104.477 | 153.569 |
| rclone, all 25 reps | 25 | | 357.366 | 593.739 | | 104.443 | 151.727 |

V4 (diagnostic, one rep per round): initial native median 1,218.741 ms, range 1,201.494 to 1,236.638.

### Ratios

"Median ratio" is the arm's native median over the rclone median of the
same reps. "Per rep" is native / rclone within each rep: its median, its
range, and how many reps were at or under 0.90x (the admission margin of
the S1 fixed-cost plan, `docs/agent-notes/2026-10-08-s1-fixed-cost-plan.md`,
step 2) and under 1.00x (the bench's win).

| arm | phase | against | median ratio | per-rep median | per-rep min to max | reps at or under 0.90x | reps under 1.00x |
|---|---|---|---:|---:|---|---:|---:|
| B | initial | rclone | 1.548 | 1.547 | 1.355 to 1.760 | 0/15 | 0/15 |
| B | initial | rclone+syncfs | 0.940 | 0.944 | 0.843 to 1.151 | 3/15 | 12/15 |
| B | delta | rclone | 1.151 | 1.152 | 1.042 to 1.354 | 0/15 | 0/15 |
| B | delta | rclone+syncfs | 0.797 | 0.824 | 0.681 to 0.988 | 12/15 | 15/15 |
| A | initial | rclone | 2.527 | 2.542 | 2.272 to 2.594 | 0/10 | 0/10 |
| A | initial | rclone+syncfs | 1.492 | 1.507 | 1.295 to 1.642 | 0/10 | 0/10 |
| A | delta | rclone | 1.320 | 1.335 | 1.194 to 1.536 | 0/10 | 0/10 |
| A | delta | rclone+syncfs | 0.898 | 0.922 | 0.804 to 1.043 | 2/10 | 9/10 |

### Reading

- **Against plain rclone, the gate's comparison, B is still slower in every
  rep:** initial 1.548x (0/15 reps under 1.00x), delta
  1.151x (0/15). B does not meet the fixed-cost plan's
  admission margin (both medians at or under 0.90x rclone), so this series
  admits no gated sample; S1 stays gated (OI-1003-Q154).
- **B against A (same rig, same window):** initial native
  555.110 vs 902.544 ms (0.615x, 38.5% less
  time), delta 120.186 vs 137.885 ms (0.872x). The B spread
  (initial 508.106 to 615.453) does not
  overlap A's (853.067 to 927.045).
- **At equal durability (informational only):** B's initial median is
  0.940x rclone+syncfs (12/15 reps under 1.00x, 3/15 at or under 0.90x) and
  its delta median 0.797x (15/15 under 1.00x, 12/15 at or under 0.90x). This
  is not the gate's comparison: rclone ships with no sync, and the gate
  is against rclone as shipped.
- Bytes received: B and A both 239,819,837 (the whole payload); V4
  219,946,368 (unique chunk bytes), so the #88 duplicate share is 0.0829
  for both, unchanged by B.
- Load: load1 rose within every round, from 0.44 to 0.53 at rep 0 to 1.75
  to 2.82 after the V4 rep. In round 1 it crossed 2.5 during the last B
  rep (2.28 before, 2.55 after), so the bench ran that rep `gated=false`
  and gave it status `informational` rather than `fail`; its numbers are
  kept. A and B alternate, so the drift falls on both.
