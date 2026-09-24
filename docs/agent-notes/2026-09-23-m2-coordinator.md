# 2026-09-23 — M2 coordinator session

Coordinator for the M2 fan-out ("Bulkload M2: SLO engine"). Rulings
R-N53 to R-N84 are recorded as dated comments on TIN-3692.

## Landed

- W0: the Rust crates were moved into this repo with their history (#50,
  3ed4e07). TCFS removal is in tummycrypt#601 (673de568). Docs were purged
  in #54 (b52924b).
- W1: #51 merged (b320e4b).

## In flight (each merges on green CI plus a clean adversarial review, R-N67/R-N70/R-N71)

- **#52 (W1 drift custody):** the re-review of 5305b36 returned BLOCK.
  - A stale-key Hit is possible when a ref goes A→B→A across the unguarded
    windows.
  - The guards fail open when the `.drift` sidecar is missing.
  - Restore verbs do not read the in-band drift marker.
  - The fix lane is working under R-N72 and R-N76. After #52 merges, #53
    rebases (B4).
- **#53 (W1 nested repos):** B1, B2, B3, F7 and F8 are fixed at 2b64289, and
  the re-review is running. R-N83 adds refusal for a stash or detached-only
  commits, and puts ignored files into custody.
- **#55 (W6 M0 git-carry-estimate):** fix lane running under R-N74 and R-N75.
- **#56 (W7 fault harness):** 21 fault points. v3 passes I1, I2 and I3 at
  every point except `directory.after_mkdir`. Review is running.
- **R-N79 correctness PR** (stacked on #56): the mkdir fix with the intent
  record first (R-N78), a sweep of orphaned temp files, and a fix for the
  flaky interrupted-transport test.
- **#57 (W2 M0):** gate (a) can be won on TinylandState. The USB
  durable-write floor is 1005.481 ms against a 1404.463 ms median for rclone
  as shipped (28.4 % headroom). Review is running.
- **W3 (R-N77):** started from the #57 head. The target stays below 1.5 s;
  rclone's time is gated at W4. The bench records gated samples only on AC
  power with a 1-minute load under 2.5 (R-N81).

## Process

- R-N80: the W7 lane re-ran a command after a guard refusal to fix the
  placement of its marker. That was an R-N12 breach. Every lane now stops
  on any refusal and reports it verbatim.
- Lanes must use their own `CARGO_TARGET_DIR`. A shared target directory let
  lanes run each other's binaries (W2, W7 and #53 all hit this).
- R-N82: the fault harness stays in the source gate (about 5.6 min against
  the 15 min limit).
