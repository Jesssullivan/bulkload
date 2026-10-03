# 2026-10-02 walk-depth (#110)

Rulings: OI-1002-Q24 (bulkload completes before the single migration run),
OI-1002-Q25 (gate plan / pre-migration fixes), OI-1002-Q29 (operator
"ultracode, full steam ahead"), R-N13.

## Done

- `bulkload-proto` refusal taxonomy: `PathDepthExceeded`
  (`PATH_DEPTH_EXCEEDED`) and `PathTooLong` (`PATH_TOO_LONG`), added to the
  path family and to the `CODES` sync list.
- `crates/bulkload-agent/src/walk.rs`:
  - `MAX_WALK_DEPTH = 256`: a directory 256 components below the root is a
    row and its contents are one refused subtree (never opened), so the walk
    holds at most 256 directory descriptors.
  - `MAX_REL_PATH_BYTES = 4095` (Linux `PATH_MAX` less its NUL): a longer
    seat is refused before it is statted; a directory refused so is never
    entered.
  - Levels no longer each own their full path prefix: one shared path
    buffer, each level keeps only its prefix length. Path memory is linear
    in depth, not quadratic.
  - Tests: a chain deeper than the cap and a 21-level chain of 200-byte names
    each refuse only the over-cap subtree as a value; siblings are carried.
- `docs/design.md` Walk bullet records both caps.

## Open

- The caps are fixed constants, not options; a deeper estate tree would need
  an operator ruling to raise them.
- Refused subtrees surface as `Control::Refused` frames (code string on the
  wire), so no wire schema change was needed.
