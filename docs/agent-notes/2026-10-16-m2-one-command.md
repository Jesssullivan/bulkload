# 2026-10-16: M2 lane, "bulkload is one command"

Lane: M2, approved by the operator to run alongside the live neo→sting
in-place move. Worktrees under `bulkload.worktrees/m2-*-20261016`, one
branch and PR per item, builds and tests on sting only inside `nix develop`
(`nice -n 19`, private `CARGO_TARGET_DIR` under
`cache/cargo-target/m2-*`). Merges go through the interim local gate
(OI-1003-Q199). The live move's tree, processes and neo were not touched.
Rulings cited: R-N13 (receipts, this note), R-N11 (no signals), OI-1003-Q199
(local gate), OI-1003-Q185 (nothing dropped from a carry).

The work items are the findings of the in-place run's first night: what the
lane scripts under `inplace-20261016/scripts/` did by hand.

## Item 1: `estate-apply` makes absent parents

Finding: 42 of 104 in-place items refused `GIT_DESTINATION_PARENT_MISSING`
(`estate-diff` said `adopt:"parent-missing"`); the lane pre-made the
directories by hand (`plan/i01-premake-dirs.txt`).

Built (branch `feat/estate-apply-parents-20261016`):
`git_carry::parents` (`FirstTarget`, `absent_parents`, `CreatedParents`).
A first landing (standalone, linked, bare) makes each absent level 0755
exactly (`fchmod`, whatever the umask) as its first write, after every
write-free refusal (`check_linked` included), syncs it and its parent, then
appends it to `{item}-{blake3(SOURCE)}.parents` in STATE and counts
`parent-created=<n>` in the receipt. Policy: levels are made only inside a
real directory, the boundary opened `O_NOFOLLOW` and each level made with
`mkdirat` and reopened `O_NOFOLLOW`; a symlink or a non-directory at the
boundary (or swapped in), or `ENOTDIR` above it, keeps the refusal. An
existing parent that is a file is now typed too. A planned path under a
file (`ENOTDIR`) is absent to the apply and the diff, so it reaches the
typed refusal rather than a bare `IO`. CLI restore verbs keep the refusal
(`Landing::parents` is `None`). `estate-diff`: `landing`/`bare-landing`
with `parents=<n>`, `parent-missing` only where the policy refuses.

Tests: `git_carry::parents::tests` (7: create and record, policy, swap
race, raced level, existing parent, `..` and unwritable boundary, no
temporary left); `estate_adopt` P105 (standalone,
linked and bare under absent parents, diff read-only, modes, records,
no-op rerun), P106 (never through a link) and P107 (bare below a planned
workspace lands after it); P103/P104 now refuse through
a regular file; fault point `estate.apply.parents_made` with the
`EstateAdopt::Parents` crash row.

Refuter round (2 opus reviewers): confirmed and fixed: a `..` in the
planned path made a level then refused with nothing recorded (now refused
up front, and any levels made before a refusal are recorded); a bare item
planned below a not-yet-landed standalone workspace made that workspace's
directory and locked it out `GIT_DESTINATION_OCCUPIED` (refs-only items now
ordered by their repository path, P107); a crash between `mkdirat` and
`fchmod` could leave a final-named level at the umask mode, and a level
another job made was used unsynced (levels now made under a temporary,
0755, synced, renamed no-replace; every parent synced); an unwritable
boundary made the diff say `landing` and the apply `IO` (now
`parent-missing` in both). Docs: `parents=<n>` counts levels absent now.
`rv4_case_folded_nest_target_is_refused_or_typed` updated (its
case-sensitive branch now lands both items).

Open: a crash between the levels and the record leaves unrecorded empty
directories or an empty `.bulkload-parent-*` temporary (documented; the
rerun lands into them).
