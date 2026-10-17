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

## Item 2: an unowned narrower directory is accepted, its files proved by content

Finding: a pull into a copy of a staging dir refused 160 entries
`DESTINATION_OCCUPIED`. The logs (read-only) show all 160 were
directories (0700 copies of an earlier landing's unfinished directories
under a 0755 source; `finish_directories` runs only after a session with
no refusal). Two refuters established that a refused directory refuses
only its own row: the files below were decided all the same, so the file
rule the operator asked for (equal adopted, differing refused, never
overwritten) already held. The change (PR #247): an existing directory
with no owning record whose mode differs is accepted as a container, mode
kept and never adopted, when it is this user's, gives its owner rwx, and
grants group/other nothing the source's mode does not (owner bits not
compared, so a 0700 copy of a read-only source directory passes).
Counter `dest_directory_mode_kept`, stderr `directory-mode-kept`. Tests
that encoded the old refusal now force it with a wider mode and keep
their hazard assertions; `three_runs` covers a kept directory at batch
sizes 1/2/3/64.

Open (operator): kept directories never converge to the source's mode and
are not in the closure row; an equal file whose mode differs is still
refused; the pre-existing gap that a refused (wider) directory still has
its entries published inside it is #248.

## Item 5: `estate-retarget`, re-pointable captures

Design: `docs/design.md`, "Re-pointable captures". Finding: item IDs hash
the destination, so re-pointing a plan forced a full re-capture. The
capture does not depend on the destination (source key, opaque bundle
name, sidecars keyed by the bundle name, base groups by the source's
common directory), so the verb rewrites the plan under its lock and writes
one new `{item}.capture` per moved item with the old record's bytes, after
checking the new plan as `estate-add-batch` would; the old plan is kept as
`PLAN.before-retarget-<digest>`. Built because it was clean (PR on
`feat/estate-retarget-20261016`). Tests: `retarget::tests` (component
prefix), `estate_retarget` P108 (re-point, then the capture is a reuse hit
with `read_source_file_bytes=0`, the apply lands at the new paths and none
at the old, the rerun is a no-op, re-pointing back finds the records
`present`) and P109 (an overlapping target refuses with plan and corpus
unchanged). Not re-keyed by design: apply STATE and the capture's private
STATE.
||||||| a283db5
