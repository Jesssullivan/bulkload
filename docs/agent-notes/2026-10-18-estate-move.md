# 2026-10-18: `estate-move` built (M2 item 7)

Lane: M2 fix 7, operator ruling A-20261018-110 (asks ledger, Linear doc
2d0eee49). Linear TIN-6656 (this verb), TIN-6657 (second-machine repeat),
TIN-6658 (the 26 hand-handled i02 items). Design: PR #251,
`2026-10-16-estate-move-design.md`. Branch `feat/estate-move-20261018`.
Edited on sting; every build and test ran on honey in this lane's own
directories (`~/git-build/wt-estate-move`, target
`~/git-build/target-estate-move`, `TMPDIR=/dev/shm/estate-move-dev`).
R-N13.

## What was built

Two verbs and one option, composed of the verbs already on main. Nothing in
them writes a repository, seat or session by itself.

- `estate-move-source [--map SRC=LIVE]... [--jobs N] [--include-rebuildable]
  MOVE_STATE SOURCE_ROOT...` runs on the source and replaces
  `neo-gen-items-inplace.sh` and `neo-inplace-capture.sh`:
  - an inventory of every Git repository under the roots, grouped as Git
    groups it. That is the main and every worktree it registers, wherever
    they are. A linked worktree under a root brings its main, and a bare
    repository is refs-only.
  - The lane skipped worktrees outside `~/git` (`OUTSIDE`); this
    inventory plans them.
  - Everything the inventory cannot plan is a note: symlinks, plain files,
    prunable worktrees, unreadable repositories or entries, directories
    their repository does not list, and nests a capture holds only as
    custody. Notes are printed, counted and written to
    `carry/move-source.notes`. `estate-move` reports them as `not_moved=`,
    and they hold the gate.
  - `estate-add-batch` of the new items. A source already planned at
    another live path refuses `RECEIPT_BINDING_INVALID`.
  - `estate-capture` at auto priority. Item refusals go to
    `move-source.refusals`. Pass marks (`move-source.started`,
    `move-source.done`) let the destination refuse a corpus pulled
    mid-pass with `SOURCE_CHANGED_AFTER_SNAPSHOT`.
- `estate-move --label LABEL --source SOURCE ... HOST|- SOURCE_MOVE_STATE
  MOVE_STATE` runs on the destination and replaces the `sting-apply*.sh`,
  `i04-pipeline-run.sh`, `refs-in*.py`, `sting-carry*.sh`, `home3-run.sh`
  and `reseat.sh` chain. Its phases, each recorded in `move.ledger`:
  1. pull;
  2. plan: presence and capture give one action per item. Only a lasting,
     recorded capture refusal is carried as files; a transient or unreached
     one waits for the next source pass;
  3. `estate-apply --adopt --beside .LABEL --refs-in`;
  4. `estate-carry-files`: live while the path is still absent or empty,
     else beside. A carry is recorded as started before it runs, and
     stays where it went;
  5. `estate-handoff` for the items this pass landed or adopted;
  6. with `--sessions`, the session pulls, then (only over a complete
     landing) `estate-reseat --lanes-from-plan`, with `--map` and
     `--reseat-arg` passed on;
  7. closure, with `--dispositions` joined.

  The verb exits with the first phase's refusal, else the gate. The gate
  passes only when every item is accounted and reviewed and nothing is
  noted not moved.
- `estate-apply --refs-in`: `refs-in.py` made native. Each captured branch
  and tag that the live repository lacks, or holds at another value, goes
  in create-only as `refs/LABEL/heads/NAME`. It is recorded write-ahead
  like `--beside`'s refs.
  - A name already held at another value, from an earlier pass, is kept
    and counted (`refs-in-kept=N`), as the design's step 7 reports
    `exists`; it is never moved.
  - A ref named `refs/LABEL` itself refuses the item before it writes.

## Where it differs from the design sketch

- **Two halves, not one command over SSH (design question 4).** The S2
  source-command registry (`tests/source_command_registry.rs`, P76)
  freezes the set of child-process builders: "never add to this one". A
  destination that ran the source's capture over SSH needs a new `ssh`
  child. The destination reaches the source only through `pull`'s
  existing `serve` session. One command per host is the result.
  Collapsing it to one needs an operator ruling on the registry.
- **`--label` instead of `--date` (question 1).** The beside suffix is
  `.LABEL` and the ref namespace `refs/LABEL/`. The operator names it
  (`neo-20261018`, `yoga-20261020`); no default is guessed. The label is
  bound in the ledger at the first run, and another label refuses
  `RECEIPT_BINDING_INVALID`.
- **Refs namespace spelling.** `refs/LABEL/heads/NAME` follows merged
  `--beside` (#266), not the lane's `refs/heads/neo-YYYYMMDD/NAME`.
- **Beside worktrees (question 2).** They are whatever `estate-apply
  --beside` (#266) does: a detached linked worktree of the live
  repository.
- **Tags (question 3).** Tags are included, as the lane did.
- **Beside landings do not converge (#266).** When the source changed, a
  later pass refuses a beside item `GIT_DESTINATION_OCCUPIED`, pending
  review: `estate-apply --beside` never converges a beside checkout to a
  newer capture. That contract belongs to #266's verb, so this lane did
  not change it. A converge loop over a live source therefore needs a
  beside converge (follow-up).
- **Not built.** These are left for review:
  - The non-Git "beside group" (a live path holding a non-Git tree): the
    plan says `refuse`, and the apply refuses it typed with nothing
    written, so it stays pending review. Building it needs `estate-retarget`
    on a plan that `pull` keeps re-syncing, which would collide.
  - The converge loop (`--passes`): a rerun is the next pass.
  - Nested repositories as their own items: that is M2r2 item 5
    (`2026-10-17-nested-repos-native-carry-design.md`). Such nests are now
    notes, counted as not moved, and they hold the gate.
  - Symlinked live paths resolved to their real paths. They are refused
    (`symlinked-path`) unless `--side-root` seats them. Map to real paths.
  - Bare repositories whose capture refused, carried as files.
  - Readiness and notify (`readiness.py`, `notify-ready.sh`) per group.
- **Refs-in on items already journaled** for their current capture (the
  in-place lane's applied items) runs only after their next capture: an
  unchanged journaled item is a no-op.

## Proofs

- `estate::moving` unit tests:
  - the action table never lands an uncaptured item, never adopts a
    non-Git tree and never acts through a symlink;
  - presence reads each shape;
  - the inventory groups worktrees inside, outside and across roots and
    notes the rest;
  - the ledger binds its labels.
- A fixed-seed property: decisions are total and carry decisions are
  sticky.
- `tests/estate_move.rs` (P-MOVE-1/2/3 on this host). One pass:
  - lands two items and adopts one with refs-in;
  - puts a diverged checkout beside;
  - carries two refused captures, one live and one beside;
  - refuses a non-Git tree, leaving it untouched;
  - notes a plain file in a root.

  Closure shows 0 unaccounted, 1 pending review and 1 not moved, and the
  gate fails. A rerun moves no ref and keeps both carries where they went.
  A converge pass after the source moved a branch keeps the earlier
  refs-in ref (`refs-in-kept=1`), with the native refs and the beside
  checkout as they were. Another label is refused, as is a reserved one.
  `--plan-only` writes nothing at a live path.

## Review

Two opus refuters reviewed `c4b0687`: one on the correctness axioms and
one on operator UX. Confirmed findings and what this branch did:

| Finding | Disposition |
| --- | --- |
| A later pass after the source moved a branch refused refs-in forever | Fixed: kept and counted, never refused |
| Refs-in namespace checked after the apply wrote | Fixed: checked before any write |
| No check that the source pass finished; a corpus pulled mid-capture is carried as files | Fixed: pass marks, plus per-item refusal codes. Only a lasting refusal is carried |
| Item-versus-verb refusal decided by a counter, so a global refusal is masked | Fixed: the verb refuses unless every planned item answered (source and apply) |
| A sticky carry-live could write into a path occupied since | Fixed: sticky only once a carry started. Presence is checked again just before a carry. `--plan-only` records nothing |
| Inventory drops content silently (notes on stdout only; nests; unlisted directories; one unreadable entry aborts) | Fixed: notes persisted and counted, and they fail the gate. Unlisted directories and unreadable entries are notes. Carrying them is not built (above) |
| Hand-off, session and reseat failures did not reach the exit code; reseat ran over a partial landing | Fixed: first phase refusal is the exit; reseat only after both pulls completed; hand-off only for landed items |
| No way to join reviews, so the gate never passed | Fixed: `--dispositions` |
| Gate message unclear | Fixed: `estate-move gate=pass\|fail next=...` |
| Reseat flags the lane used are dropped | Fixed: `--reseat-arg` passthrough. Session options need `--sessions` |
| A label could name `heads` or `tags` | Fixed: reserved names refused |
| A changed `--map` plans a source twice | Fixed: `RECEIPT_BINDING_INVALID` |
| Non-Git beside group, symlink resolution, bare carry, converge loop, readiness, beside converge | Not built; reported (above) |
| Apply options not bound in the ledger; busy lock is a bare IO; no ledger reader | Open (minor) |

## Receipts

- Linear: TIN-6656, TIN-6657 and TIN-6658 were created under the P-TIN-177
  milestone "M2 bulkload is one command", and the fixes 1–6 PR list was
  commented on TIN-6656 (A-20261018-110; descriptions are new, none
  rewritten).
- honey: worktree `~/git-build/wt-estate-move` was added from the shared
  `bulkload-m2r2` clone (the other lanes' pattern), with this lane's own
  target and TMPDIR; builds ran at `nice -n 19`.
