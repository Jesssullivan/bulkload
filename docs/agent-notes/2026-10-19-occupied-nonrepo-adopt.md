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
- Results: in the PR body with host, head and TMPDIR.

## Open

- `estate-move`'s plan table still prints `action=refuse reason=not-git`
  for these items (`estate/moving.rs` `action`; `Presence::NotGit` does not
  tell a directory from a file, and its property test pins "never adopt a
  non-Git tree" for the plan). The apply runs on every planned item anyway,
  so the outcome is the apply's; the preview line is stale. Changing the
  plan table is a separate change (it touches a persisted `Presence` enum).
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
