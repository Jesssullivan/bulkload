# 2026-10-02 — M2 W4 PR 3: streaming source walk, component-wise openat source

Lane: W4 PR 3, host sting. Worktree
`bulkload.worktrees/m2-w4-pr3-20261002`, branch
`feat/m2-w4-pr3-streaming-walk`, cut from origin/main 9115ffd (#75, #77,
#80, #81 merged).

Rulings cited:
- OI-1002-Q25 ("W4 PR 3 lane"): this lane.
- R-N13: receipts cite rulings; this note.
- R-N58, OI-1001-Q15: R25 stays strict.
- R-N59, R-N118: no dual stack; the walk has one implementation.
- R-N54: unsafe-first. This PR adds no `unsafe` block; it reuses the
  annotated `io::sys` wrappers.
- R-N127: W4 sequencing (PR 3 of 4).

## Scope

No issue or Linear comment wrote PR 3 down beyond the phrase "streaming
walk and component-wise `openat` source" (2026-10-01 W4 PR 2 note; #46
description, D1 "Source" and "Protocol"). Derived from those:

1. **Streaming walk, parents before children, no global sort** (#46 D1).
2. **Component-wise `openat` source, with identity checked before and
   after the read** (#46 D1 "Source"; it fixes the D1 finding that the
   source opened `root.join(rel)` with `O_NOFOLLOW` on the last component
   only).
3. `pread` reads, no mmap (#46 D1).

Out of scope, left for a follow-up: pooled 4 MiB slabs and the fused
`io::chunker` on the source capture path (window-parallel CDC for files
≥ 32 MiB, QoS for the largest file). The capture keeps `StreamCDC`.

## What changed

- **`walk.rs`.** One walk engine, `Walker`, an iterator of `WalkItem`
  (`Row`, `Refused`, `Engine`) beneath one root descriptor. Each directory
  is opened with `openat(O_DIRECTORY | O_NOFOLLOW)` beneath its parent's
  descriptor, its row is taken from `fstat` of that descriptor and it is
  listed through it, so the row and the listing are one inode. Seats are
  statted with `fstatat(AT_SYMLINK_NOFOLLOW)`. Names within a directory
  are in byte order; there is no global sort. One descriptor is held per
  level of depth. `walk()` collects the stream and hashes as before,
  through `hash::hash_beneath_observed` (component-wise open).
  - The `ignore` crate is gone from the workspace (one fewer dependency).
  - Unlistable directory: the directory is still a row, and its contents
    are refused under the directory's own path (was an empty path).
  - A directory swapped for a symlink or file between `fstatat` and the
    open refuses `SOURCE_CHANGED_AFTER_SNAPSHOT`.
  - Engine temporaries: a tagged directory temporary is excluded when it
    holds only tagged file temporaries (it and they are recorded). This is
    decided from its listing before its row is emitted, so no buffering is
    needed; it matches the old post-walk pass, including that a tagged
    directory holding an empty tagged directory is kept.
- **`transfer.rs` source.**
  - `serve` opens the canonical source root once (`sys::open_root`) and
    takes the authority's device and inode from that descriptor.
  - A walk thread runs `Walker` and hands items to the sending thread over
    the existing event channel, at most `WALK_AHEAD` (4096) items ahead of
    the wire (`WalkGate`). The sending thread offers rows as the 1024-entry
    window allows, writes walk refusals and engine temporaries as they
    arrive, and sends `WalkDone` once the walk has ended and every row is
    offered. `SourceDone` counts the offered entries.
  - Jobs carry their row (`Arc<RowSchema>`); a row is dropped once its
    entry is retired, so the source holds only small per-entry slots.
  - `open_source` opens every capture and chunk re-read with
    `sys::openat_beneath` (`O_NOFOLLOW` at each component). Reads go
    through `sys::pread_full`; a truncation is a short count.
- **docs/design.md.** Wire v5 gains a "Walk" bullet.
- The "wired in by W4 PR 2/3" dead-code allowances on `open_root`,
  `openat_beneath` and `pread_full` are removed; `OpenMode` keeps a
  narrower one (only `Read` is used outside tests).

No wire change: `wire_id` and every frame are unchanged. The destination
already accepted walk refusals and engine temporaries at any point before
`WalkDone`.

## R25 and durability

- The walk reads metadata only. No content read was added or moved: a
  `Reuse` still reads nothing, `Send` reads once, `WantManifest` reads at
  most once (ledger manifest or retained chunks), exactly as on main.
- The ledger commit still follows `Held{true}`, which follows the
  destination group commit; `SourceDone` still follows `committer.sync()`.
  Streaming changes when an entry is offered, never when anything is
  committed.
- `WalkDone` is sent only after the walk has ended and every row is
  offered, so the destination's `settle_held` sees the same condition.

## Tests

- `walk::tests`: parents before children without a global sort; the first
  item needs one listing; a directory replaced by a symlink mid-walk is
  walked through its held descriptor and never followed out; a symlinked
  directory is a row, not a descent; a directory temporary of file
  temporaries is recorded whole; an unlistable directory is a seat with
  its contents refused.
- `transfer::tests::a_source_read_never_follows_a_swapped_directory`: the
  intermediate directory is replaced by a symlink to a directory that holds
  a hard link of the same inode, so a path open would pass the stat-identity
  check. The source refuses with `ENOTDIR`/`ELOOP` and reads 0 bytes.
  Mutant (path-based open restored): caught.
- `transfer::tests::a_walk_longer_than_the_walk_ahead_bound_completes`:
  4160 walk items through the gate.

## Gates (local, sting)

Every recipe ran through `nix develop .#default` (rustc 1.96.1) with its own
`CARGO_TARGET_DIR`, `CARGO_BUILD_JOBS=4` and `RUST_TEST_THREADS=2`. Host load
was about 25–40 (not a benchmark sample).

| Gate | Result |
|---|---|
| `just check-fast` | exit 0. Lib 388 passed (6 ignored); io-trace io:: 69; fault_harness 56/56; power_loss 5/5; resume 2/2; contract tests pass; gitleaks clean |
| `just resume-power-loss` (standalone) | exit 0, 2 passed |

## Open

- Slabs and the fused chunker on the source capture path (D1 "Source").
- The walk holds one descriptor per level of depth; a tree deeper than the
  descriptor limit refuses the subtree with `IO`.
- No gated benchmark sample (R-N81); gate (a) waits for the #88
  measurement.
