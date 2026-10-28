# 2026-10-28: BL2-F5 (hermetic test floor)

Authority: operator interview 2026-10-28, Linear TIN-3692 comment
`952e2fb6`. BL2-F5, verbatim: "Hermetic test floor". Related: BL2-Q3
(`36925e2d`), BL2-F1 and BL2-F4 (`c10e9bec`). R-N13.

| stream | owner | repo/branch | state/evidence | next |
| --- | --- | --- | --- | --- |
| BL2-F5 test seam | sting lab seat | #342 | honey and PZM gates in the PR comments | merge |

Facts a later lane needs:

- `space::probe` is the only free-space reader. In a debug build it
  returns the `BULKLOAD_TEST_SPACE` reading (`TOTAL:AVAILABLE`, bytes)
  when the variable is set. Every item of the seam is
  `cfg(debug_assertions)`. On honey the release binary of #342 had the
  same sha256 as main's (`3281a711`).
- Two defaults exist, and #342 changes neither. The engine floor is
  `space::DEFAULT_MIN_FREE_PERCENT` = 25. The fold floor is
  `git_carry::fold::DEFAULT_MIN_FREE_PERCENT` = 10 (BL2-Q3), with
  `from=default` or `from=flag` in its receipt rows. The ruling's
  "product default floor (10%)" is the fold's. The 25% the tests hit is
  the engine's.
- A test that runs the binary sets `TEST_SPACE_ROOMY` through its helper.
  A new helper that omits it reads the host disk again and refuses on
  PZM, whose `/nix` is under 25% free.
- The integration tests name `cfg(debug_assertions)` items, so they do not
  compile with `cargo test --release`. No justfile recipe does that:
  `bench-io-chunker` builds `--lib` only.
- After #342, PZM `cargo test --workspace` failed 12 tests, all also
  failing on main, none reading free space. Causes are in the PR comment.
