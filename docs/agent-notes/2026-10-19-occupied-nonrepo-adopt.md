# 2026-10-19: an occupied non-repo directory is adopted or set beside

Lane: coordinator follow-up after #282 (merged as `9854236`), the td01
batch on sting. Branch `feat/occupied-nonrepo-adopt-20261019` from
`origin/main` `9854236`. Edited on sting; every build and test ran on
honey (`~/git-build/occupied-nonrepo-adopt-20261019`, target
`~/git-build/target-beside-adopt-converge` reused from this seat's merged
lane, `TMPDIR=/var/tmp/jess-bulkload-tests/occupied-nonrepo-adopt-20261019`,
removed after). Nothing ran against the live estate. R-N13
(lab R-HOOK-CONVERGENCE-20261004).

Requirement served (docs/SPEC.md R0.2, verbatim): "The work lane is intact
at its DEST path. [...] linked worktrees still linked, under their names
(RES-14)"; R2.2: "a diverged DEST copy is set aside intact, with its
unpushed branches imported create-only (LIVE-32)" and "A run never
overwrites, resets or deletes DEST work that SOURCE does not hold".

## The failure

`estate-move` batch td01 (closure
`/srv/fast-local/jess/estate/inplace-stage/move/td01/closure.json`,
label `neo-20261016`) refused six linked worktrees of tinyland.dev
`GIT_DESTINATION_OCCUPIED` (`estate::apply`), e.g.
`tinyland.dev.worktrees/tin-1721-drop-vendored` and
`tinyland.dev/.worktrees/tin-1932-as2-single-source`: on sting each is a
directory with its files (about 60 top-level entries, thousands in all) and
no `.git` at all, the 2026-08-29 ceremony having stripped it; neo has them
as real worktrees, captured fine. `apply_workspace` refused any planned
path with no `.git` (`worktree_git_dir(..).ok_or(OCCUPIED)`), so neither
adoption nor beside was reached, and refs-in never ran for them. Each also
already has a beside landing `<path>.neo-20261016` from the 10-17 hand
plans (`tinyland.dev/.git/worktrees/<name>.neo-20261016`), which #282's
re-key path takes over when these go beside.

## What changed

Under `--adopt`, a linked worktree item whose planned path is a directory
with no `.git` entry:

- **(a) proves equal → adopted by registering it again.**
  `converge::check_nonrepo` (read-only): the path spelled canonically; no
  registration of the repository naming it unless this item's landing
  intent does (its own registration a crash cut short); the first linked
  landing's checks (`check_linked` less its absent-path rule); and
  `adopt::prove_seats`, every carried seat (tracked, untracked, ignored)
  held with its kind, mode and bytes; extra files there are left alone.
  `git_carry::adopt_nonrepo_linked` then does what a first linked landing
  does, except write the files: under the lock import, native ref step,
  HEAD plan; re-check (no `.git`, prints unchanged); landing intent; the
  branch `worktree add -b` would create in a foreign repository (CAS
  create); `converge::register` (the administration directory
  `worktrees/<id>`: `commondir`, `HEAD`, `gitdir` last; `<id>` the
  directory's name, numbered when taken, as Git numbers it); the `.git`
  gitfile created new (the one entry added); the index from the capture
  (`read-tree`, intent-to-add); the ledger from the proof's prints once they
  still hold (`converge::record_adopted`). Receipt `adopted-nonrepo=1
  adopted-seats=N`; outcome `workspace-restored`, closure `applied`; later
  passes converge it like any landing.
- **(b) differs, or a standalone item's → never written.**
  `GIT_DESTINATION_NONREPO_DIFFERS` (new code). Under `--beside` a linked
  item's capture lands beside it exactly as a diverged checkout's does
  (#266/#282: refs in under `refs/<ns>/`, the directory untouched), and the
  beside landing records why: `beside-reason=<CODE>` in the receipt,
  `{item}-{blake3(SOURCE)}.beside-reason` in STATE, `beside_reason` on
  closure's `applied-beside` row (a diverged checkout's says
  `GIT_DESTINATION_DIVERGED`). A standalone item's non-repo directory has
  no registration to re-create and no repository to land beside into: it
  refuses typed (pending review, accounted), nothing written there or
  beside it. Without `--beside` the linked case refuses typed too.
- **Crash safety.** New fault point `estate.apply.nonrepo_registered`
  (administration directory written, no gitfile yet): the rerun's adoption
  reuses that directory, never makes a second. After the gitfile
  (`estate.converge.linked_added`) the rerun's `recover_linked_landing`
  finishes it; `restore_filesystem_rows` now sets a mode only where the
  seat does not hold it already, so the recovery changes no file's
  metadata (ctime included). `plan_linked` no longer counts the
  destination's own registration as another worktree holding its branch
  (else the rerun detached HEAD).
- `estate-diff`: `nonrepo-adoptable` (class `identical`) or
  `nonrepo-differs` with the differing seats and, with `--beside`, the
  beside component.
- No wire change, no new dependency; closure stays `bulkload.closure.v5`
  with one added row key.

## Tests

- `tests/estate_adopt.rs` P126 (adopt: every entry keeps inode, times,
  size and mode; the destination-only file stays; on its branch with the
  capture's status; `git worktree list` names it; diff
  `nonrepo-adoptable`; the next source commit converges into it), P127
  (differs: refused typed without `--beside`; with `--beside --refs-in` the
  capture lands beside, refs in, `beside_reason` in receipt and closure,
  the directory untouched; diff `nonrepo-differs`, `seat=`, `beside=landing`),
  P128 (standalone: refused typed with `--beside --refs-in`, nothing written,
  nothing beside).
- Fault harness `estate_apply_nonrepo_registered`,
  `estate_apply_nonrepo_linked_added` (crash, rerun finishes; no file
  written; one registration; branch and status right; second rerun a
  journal no-op).
- `tests/estate_move.rs`: the standalone `occupied` item now refuses
  `GIT_DESTINATION_NONREPO_DIFFERS` (was `GIT_DESTINATION_OCCUPIED`); and
  the converge-pass check #282 added (`beside-converged=1`) only holds
  when the pass recaptured `diverged`, which depends on the source
  census's timing (it did not under honey's load on 10-19), so it now
  accepts either that converge or the journal no-op.
- The S2 source-command registry caps `read-tree` call sites
  (`source_command_registry.rs`, "a ceiling only shrinks"): the adoption
  uses the existing `read_tree` helper, adding no site.
- Results: in the PR body with host, head and TMPDIR.

## Review of #286 (coordinator comment, 2026-10-19T23:38Z, at `f70dd0c`)

| # | Finding | Disposition |
| --- | --- | --- |
| 1 (blocker) | five of the six td01 items already have a bulkload `<path>.neo-20261016` linked landing holding their branch: adopting the August directory landed it detached beside it, two registered copies under a passing closure | fixed (refusal half): an equal directory whose captured branch another worktree holds, or whose item's beside landing exists (`--beside`), refuses `GIT_DESTINATION_NONREPO_DUPLICATE` before any write, pending review; P129. Open: when that beside is this seat's and proven equal, moving the branch to the adopted path and retiring the beside through #289's `git_carry::beside::retire` (unmerged); until #289 lands the item waits for review with both copies as they are |
| 2 (high) | crash recovery past the gitfile rewrote missing seats and reset modes; its ledger was a later census | fixed: a marker beside the landing intent sends the recovery to `finish_nonrepo_adoption`, which proves every seat (content and mode) and writes none (`GIT_DESTINATION_MODIFIED` otherwise) and records that proof's prints; fault row `estate_apply_nonrepo_tampered` |
| 3 (med) | `registration_of` compared `gitdir` literally, so a relative stale one let the path be registered twice | fixed: a relative `gitdir` is resolved against its administration directory; P134 |
| 4 (low) | no `locked` marker while the registration is made | fixed: `locked` "initializing" until the gitfile, then removed (only that exact content); asserted in `estate_apply_nonrepo_registered` |
| 5 (low) | the under-lock re-check ran after the import and an exclude append | fixed: `plan_linked` takes a re-check run under the lock before them (and the check runs after too) |
| 6 (low) | a crash before the gitfile, then a pass that does not adopt, left the administration directory and intent | fixed: `abandon_registration` removes them under the lock when they are exactly this item's; fault row `estate_apply_nonrepo_abandon` |
| preview | `estate-move` printed `action=refuse reason=not-git` | fixed: `moving::nonrepo_preview` says `adopt` (`nonrepo-adopt-or-beside`) for a linked item's non-repo directory with a held capture; a standalone one's and a file stay `refuse`; unit test |
| Darwin | P126-P128 refuse `DESTINATION_SPACE_INSUFFICIENT` on pzm's free-space floor (the 38 baseline failures' cause) | no Darwin defect (coordinator's A/B at `f70dd0c`) |

## Verification of #286 (coordinator, at `11ea7c2`)

| # | Defect | Disposition |
| --- | --- | --- |
| V1 | bare `--adopt` (no `--beside`) adopted the equal directory while the item's `<path>.neo-YYYYMMDD` copy existed detached (tin2835 shape): the duplicate check saw `<path>SUFFIX` only when the suffix was passed | fixed: `converge::registered_copy` refuses `GIT_DESTINATION_NONREPO_DUPLICATE` whenever a registered worktree other than the path is bulkload's landing of this item (its workspace ledger names the item) or a `<path>.<suffix>` sibling with a bulkload workspace ledger under any source, independent of `--beside` and HEAD; P129b (hand-plan copy, detached, bare `--adopt`) |
| V2 | `locked` "initializing" stayed after a crash at `nonrepo_registered` and a pass without `--adopt` (OCCUPIED), and after a failed `.git` create | fixed: the pass without `--adopt` runs `git_carry::abandon_nonrepo` before refusing OCCUPIED; every refusal of `check_nonrepo` does; the registration steps (branch create, `register`, gitfile) run in one closure whose failure removes a partial gitfile it created and then abandons; `abandon_registration`'s guard is now "no `.git` names this administration directory" (a stranger's `.git` no longer protects our half-made one); fault row `estate_apply_nonrepo_no_adopt` |

## Open

- (superseded by the review fix above: the preview now says `adopt` for a
  linked item's non-repo directory.)
- Finding 1's equal-beside path waits for #289: this lane refuses the
  duplicate; it does not yet move the branch or retire the beside.
- V1's sibling rule is conservative: a different item whose worktree sits
  at `<path>.<anything>` with a bulkload ledger also makes the directory
  refuse DUPLICATE (a refusal, never a write); none of td01's six has one.
- V2: a branch the adoption created before a later registration step
  failed stays (a ref, not a lock); the rerun reuses it.
- A stale registration of the repository that names the path (an admin
  directory left by a pruned worktree whose `gitdir` still names
  `<path>/.git`) makes the directory `GIT_DESTINATION_NONREPO_DIFFERS`
  rather than reusing or replacing that registration (its HEAD, index and
  logs are not this capture's). On sting the six have none (the beside
  ones are `<name>.neo-20261016`).
- A non-repo directory at a standalone item's path is refused typed, not
  adopted or set beside (no repository exists to register it in or land
  beside into); none of td01's six is standalone.
- The live td01 pass is not re-run from this lane; it needs this build
  deployed.
