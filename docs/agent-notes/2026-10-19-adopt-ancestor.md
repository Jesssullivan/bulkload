# 2026-10-19: a clean live ancestor is advanced in place (`--advance-ancestor`)

Lane: code lane on `bulkload`, branch `feat/adopt-ancestor-20261019` from
`origin/main` `9854236` (#282 merged: beside landings converge and adopt
by proof). Edited on sting, in
`/srv/fast-local/jess/git/bulkload.worktrees/adopt-ancestor-20261019`;
every build and test ran on honey (Q196: never on sting or neo), in
`~/git-build/adopt-ancestor-20261019` with
`CARGO_HOME`/`CARGO_TARGET_DIR` under `~/git-build/adopt-ancestor-20261019-cargo`
and `TMPDIR=/var/tmp/jess-bulkload-tests/adopt-ancestor-20261019`.
Nothing ran against the live estate. R-N13 (lab
R-HOOK-CONVERGENCE-20261004).

## The ruling

Operator ruling 2026-10-19 (interview, verbatim choice "Yes, advance
those"), as the lane brief quotes it:

> For a live sting checkout that is clean and whose HEAD is an ancestor of
> neo's (GloriousFlywheel, glorious-build-infra, and any others like them),
> bulkload advances it to neo's HEAD plus neo's working-tree changes, so
> sessions resume on neo's actual work. Sting's own branches, stashes and
> worktrees stay untouched, and the beside copy is retired only after
> bulkload proves the live tree equals it.

Coordinator refinement (mid-lane message, 2026-10-19), followed as given:
"tracked/staged changes still refuse (GIT_ADVANCE_NOT_CLEAN). Untracked-not-ignored
live paths are allowed and left byte-for-byte untouched ONLY if none of them
collides with any path in the captured tree, the captured index, or the
captured working-tree changes (a collision = same path, or one is a parent
directory of the other). Any collision refuses with a new typed code (e.g.
GIT_ADVANCE_UNTRACKED_COLLIDES) whose refusal_detail lists the colliding
paths, and falls back to beside in the same pass. [...] Ignored files: never
touched, never a collision unless the capture tracks that path."

The ruling is not yet an entry in `docs/intent-ledger.md`: its entries carry
the interview's timestamp, session and question, which this lane does not
have. The coordinator should append it (with A-115 of 2026-10-18, also
absent) from the interview record.

Requirement served (docs/SPEC.md R0.2, verbatim): "The work lane is intact
at its DEST path. [...] SOURCE's state is fully present and restorable on
DEST, 'reporting the correct state as on neo' (RES-17)"; R0.1 "Conversations
resume" (the sessions resume in the live cwd on neo's work); R2.2: "a
diverged DEST copy is set aside intact, with its unpushed branches imported
create-only (LIVE-32), or diffed and preserved before it moves forward
(LIVE-33)" and "a rerun replaces a DEST file only when it is bulkload's own
output, unchanged since it wrote it (LIVE-30)"; R4: "On DEST each tree
lands once, at its live path (FOOT-18, PUR-20). There are no duplicate
landing archives [...] left behind without an operator ruling".

## What changed

`estate-apply --adopt --beside SUFFIX --advance-ancestor` (and
`estate-move --advance-ancestor`, which passes it). The flag needs `--adopt`
and `--beside` (`REQUIRED_FIELD_MISSING`); `estate-diff` refuses it.

- **When.** Only where the adopt proof of a live checkout bulkload did not
  land finds it differs (`GIT_DESTINATION_DIVERGED` without the flag).
- **Preconditions** (`git_carry/converge/advance.rs`), each read before any
  write and proved again under the existing destination lock:
  `GIT_ADVANCE_NOT_ANCESTOR` (unborn, or not an ancestor of the captured
  HEAD), `GIT_ADVANCE_BRANCH_MISMATCH` (another branch, or one detached:
  never detached instead), `GIT_ADVANCE_BRANCH_BUSY` (checked out by
  another worktree or held by a rebase or bisect there, named by a pending
  bulkload intent, a symbolic ref, or a tip that is not the live HEAD),
  `GIT_ADVANCE_NOT_CLEAN` (a staged or unstaged change, an operation in
  progress, an assume-unchanged or skip-worktree entry, or a seat to be
  replaced that does not hold its HEAD's bytes),
  `GIT_ADVANCE_UNTRACKED_COLLIDES` (the coordinator's rule; carries the
  paths), `GIT_ADVANCE_RACED` (the proof does not hold under the lock, or
  the branch's compare-and-swap lost). Each falls back to the beside
  landing in the same pass, is recorded in STATE
  (`{item}-{blake3(SOURCE)}.advance`) and is closure-visible and
  dispositionable: the `applied-beside` row becomes
  `refused-pending-review` with that code until `closure-dispose`.
- **The advance.** Intent (`advance.intent` in the checkout's ledger
  directory), then the branch (or detached HEAD) by compare-and-swap from
  the proved tip, then the seats from the live HEAD tree to the captured
  worktree through the converge's seat engine, then the captured index,
  then the advance record (`advanced`, the swap record) and the workspace
  ledger, then the intent removed. No repository marker, no native ref
  step: other branches, the stash, other worktrees, configuration and
  `info/exclude` are never touched. `--refs-in` imports the captured
  branches, tags and (for an advanced item) stash entries under the
  namespace. Outcome and journal `workspace-restored` (closure `applied`).
- **Ignored and untracked entries.** Never touched. A captured seat the
  capture does not track whose path the live checkout holds with an
  ignored entry is left to it (`advance-kept`), its subtree with it; a
  captured directory held as an untracked directory keeps its mode;
  the capture's bytes for both stay in the capture's provenance. Sting's
  own untracked files stay as they are.
- **Later passes.** The advanced checkout converges as any landing, with
  three rules from its `advanced` record: what the advance left is never
  touched, its `info/exclude` is never replaced or proved, and its branch
  moves with HEAD by compare-and-swap from bulkload's own value
  (`advance::branch_plan`), never detached for a foreign repository's.
- **Rerun of a beside-journalled item.** A beside journal of the current
  capture no longer ends an item when the flag is given and the live
  checkout may advance now (`advance::gate`, a metadata walk); a recorded
  `NOT_ANCESTOR`, `BRANCH_MISMATCH` or `UNTRACKED_COLLIDES` with the same
  live HEAD and untracked list keeps the journal (no re-proof).
- **Beside retirement** (`git_carry/beside/retire.rs`). After an advance,
  the item's beside landing is brought to the capture as every beside pass
  brings it (#282: converged, re-keyed, adopted by proof), must name this
  item and capture in its own ledger (else kept `not-owned`), and is
  proved equal to the live tree (HEAD, index entries, every entry
  including untracked and ignored, a sealed hand-off block the one
  difference allowed, live-only non-ignored files only where they are the
  live checkout's own untracked files the advance left; nothing its Git
  holds unreachable from the live repository). Then a crash-safe removal:
  STATE record (`.retire`), rename to a private name, re-proof by prints,
  record marked deleting, a linked landing's administration directory
  removed, the tree deleted, the record removed. Any difference keeps it
  with a typed reason. Receipt and closure: `advance-beside=retired|kept|absent`.
- No new crate or dependency; no wire change (codes travel as strings);
  closure stays `bulkload.closure.v5` with one added row key (`advance`)
  and the advance-refused classification.

Files: `crates/bulkload-proto/src/refusal.rs`,
`crates/bulkload-agent/src/git_carry/converge/advance.rs` (new),
`crates/bulkload-agent/src/git_carry/beside/retire.rs` (new),
`git_carry/converge.rs` (`Applied.advanced`, `Policy.advance`,
`Place.advanced`, `publish_index`'s before-tree, the pending-intent settle,
the advanced checkout's filters), `converge/adopt.rs`, `beside.rs`,
`converge/diff.rs`, `estate.rs`, `closure.rs`, `estate_move.rs`, `main.rs`,
`fault.rs`, `git_carry.rs` (test hooks), `docs/design.md`.

## Decisions taken in the lane (for the operator to see)

1. **One ordered seat pass.** The brief's "check out the captured tree and
   index, then materialize the captured working-tree changes [...] exactly
   as a landing does" is done as a landing does it: one seat pass from the
   live HEAD tree to the captured worktree (tracked content, dirty and
   untracked files together), then the index. The fault rows name the
   brief's points: after the branch CAS (`after_branch`), mid-materialize
   (`after_seat:3`), after checkout (`after_index`: tree, working changes
   and index all in place, no ledger).
2. **Ignored entries where the capture lands a seat it does not track** are
   left to the live checkout (counted, recorded, never in the ledger),
   rather than refusing the advance. Host-specific ignored entries
   (`bazel-*` and `result` links, `.claude/settings.local.json`, `.env`)
   are likely on GloriousFlywheel; refusing would never advance it. The
   capture's bytes stay in its provenance in the live repository and in the
   beside landing, which is then kept (`differs:seat=<path>`), so nothing
   is dropped. Where the capture tracks the path, it collides instead (the
   coordinator's rule).
3. **Retirement equality** exempts the live checkout's own untracked files
   the advance left (proved apart from every captured path): they were
   never the beside landing's, and retiring the beside touches none of
   them. Every entry of the beside landing must still be in the live tree
   byte for byte, ignored ones included. Without the exemption the
   MassageIthaca shape would keep its beside landing for good.
4. **The beside landing is converged before it is compared** (it usually
   holds an older capture than the advance's), by the #282 beside pass,
   which proves it unchanged against its ledger first; an edit there keeps
   it (`refused:GIT_DESTINATION_MODIFIED`).
5. **The `.beside` STATE record is kept after a retirement**, so a later
   beside pass treats the path as retired (#282: never laid down again).

## Tests

Run on honey (rows created and destroyed by the tests, R-N98):

- `tests/estate_advance.rs`, 8 rows: A1 the GloriousFlywheel shape (beside
  first, then advanced: sting's stash, branches, worktree and ignored files
  untouched; neo's stash entry in under the namespace; the linked beside
  retired, its administration gone; closure `applied` with `advance`
  block; a rerun a journal no-op; a later capture converges with the
  branch attached), A2 a standalone beside adopted by proof then retired,
  A3 the glorious.build shape (`GIT_ADVANCE_NOT_ANCESTOR`, beside, closure
  pending review, `closure-dispose` accept, an unchanged rerun keeps the
  instance), A4 to A6 dirty unstaged and staged (`NOT_CLEAN`), branch
  checked out elsewhere (`BRANCH_BUSY`), branch-name mismatch
  (`BRANCH_MISMATCH`), A7 ignored files kept and a differing beside kept,
  A8 non-colliding untracked kept and advanced (beside still retired), A9
  untracked colliding with a newly tracked path and A10 an untracked
  `.serena` the capture also holds (`UNTRACKED_COLLIDES`, closure
  `refusal_detail` naming it), the flag's prerequisites.
- Library rows: `converge::tests::an_advance_whose_branch_moved_refuses_raced_with_nothing_written`
  (the live branch moved before the lock, and between the intent and the
  compare-and-swap: `GIT_ADVANCE_RACED`, nothing written),
  `an_advance_lands_the_capture_on_the_live_branch`, `advance::tests`
  (kept/left rules, the collision rule), `beside::retire::tests`.
- Fault harness, 11 rows: `estate_advance_after_{intent,branch,seat_first,seat_mid,index,ledger}`
  and `estate_advance_retire_{after_intent,after_rename,mid}`,
  `estate_advance_retire_standalone_{after_rename,mid}`: after each crash
  every live seat is the live HEAD's or the capture's, sting's own state is
  as it was and the beside bytes are whole at their path or private name;
  the rerun finishes, closure 0 unaccounted, a second rerun a no-op.
- The full `cargo test -p bulkload-agent --locked` and `just rust-check` on
  the pushed head are in the PR body with host, head and TMPDIR.

## Open

- **`estate-diff` does not preview the advance**: a live ancestor still
  reads `adopt=diverged` (and `beside=...`) in `estate-move --plan-only`.
  The proof is read-only and could be run there; not built.
- **A beside kept after an advance stays** until the operator removes it:
  later passes converge the live checkout and never revisit the beside.
- **Sessions with their cwd in a retired beside**: the retirement renames
  and deletes the directory once it is proved equal (no write there since
  the proof, checked by prints after the rename); a shell sitting there is
  left in a deleted directory. Not detected (no process inspection).
- **Global excludes**: bulkload's Git runs with no global configuration, so
  a file ignored only through a custom `core.excludesFile` reads as
  untracked; it collides (refusal, beside) only where the capture holds
  that path.
- A ledger over the cap after an advance is written without a workspace
  ledger (`not-convergeable`), as a landing's is; the next pass then adopts
  rather than converges. Not tested (200k seats).
- The live run (sting) was not run from this lane; it needs this build
  deployed first, and `estate-move --advance-ancestor` given.
