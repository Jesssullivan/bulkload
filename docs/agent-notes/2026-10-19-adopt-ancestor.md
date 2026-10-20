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
2. *(Changed by the coordinator, see "Coordinator decisions" below.)*
   **Ignored entries where the capture lands a seat it does not track** are
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
6. **The collision rule, read for the coordinator's own examples.** An
   untracked entry is taken as Git lists it (`?? .serena/`, one entry for a
   directory that holds nothing tracked). It collides with a captured path
   that is the same, with any captured path under it, and with a captured
   *file* above it; a captured *directory* above it is no collision. The
   literal "one is a parent directory of the other" would refuse the
   MassageIthaca shape (`docs/agent-notes/2026-08-28-convergence-review/`
   sits under the tracked `docs/agent-notes/`), which the coordinator wants
   advanced; this reading needs the coordinator's sign-off. Gitlinks are
   never a collision (a submodule's directory is the live checkout's).
7. *(Withdrawn, see "Coordinator decisions" below.)*
   **A live ignored directory takes only the capture's tracked files.**
   Where the capture holds a directory the live checkout holds as an
   ignored one, the capture's tracked files under it land (the checkout is
   wrong without them) and what the capture does not track there is left,
   never mixed into the live checkout's ignored tree.
8. **Ownership by proof.** After the #282 beside pass, a beside landing of
   another item (re-keyed by A11b) or a standalone checkout no ledger named
   (adopted by A11a) is this item's by proof, as #282 defines it, and so
   retirable once proved equal; a foreign one (`GIT_BESIDE_FOREIGN`) is
   kept (`refused:GIT_BESIDE_FOREIGN`).
9. *(Changed by the coordinator, see "Coordinator decisions" below.)*
   **A beside kept after an advance does not hold the closure gate.** It is
   closure-visible (`advance.beside=kept`, `reason`) but the row stays
   `applied`: the ruling says to keep it until proved equal, which this
   does; whether a kept beside should be pending review (R4, duplicates)
   is the operator's call.
10. **The finish of an advance a flagged pass began needs no flag.** A
   rerun without `--advance-ancestor` settles a pending advance (as any
   pending converge) and retires its beside landing; a STATE retirement
   record or an advance intent ends no journal.

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
- Fault harness, 14 rows: `estate_advance_after_{intent,branch,seat_first,seat_mid,index,ledger}`
  and `estate_advance_retire_{after_intent,after_rename,mid}`,
  `estate_advance_retire_standalone_{after_rename,mid}`: after each crash
  every live seat is the live HEAD's or the capture's, sting's own state is
  as it was and the beside bytes are whole at their path or private name;
  the rerun finishes, closure 0 unaccounted, a second rerun a no-op. Then
  `estate_advance_after_seat_rerun_without_flag` and
  `estate_advance_retire_after_rename_rerun_without_flag` (the rerun has no
  flag and still finishes), and `estate_advance_after_branch_then_edited`
  (an edit after the crash is refused and kept, never ledgered as
  bulkload's).
- Library rows added after the review:
  `converge::tests::an_advance_whose_head_switched_puts_the_branch_back`.
- `estate_adopt` P131's "read by an item's scratch" filter matched any
  `/adopt-` in the path, so it failed under this lane's TMPDIR
  (`.../adopt-ancestor-20261019/...`); it now reads the scratch's own
  directory name (`<stage>/adopt-<n>/repo`). Test-only.
- After the review of #289 and the "Switch when clean" ruling:
  - lib (`git_carry::converge::tests`): `a_held_git_lock_refuses_the_advance_with_nothing_moved`,
    `a_commit_cannot_interleave_with_an_advance`,
    `an_edit_met_after_the_branch_moved_rolls_the_advance_back`,
    `a_switch_lands_the_capture_on_its_branch_and_leaves_the_live_one`,
    `a_switch_advances_an_ancestor_branch_and_refuses_a_diverged_one`,
    `a_switch_refuses_each_condition_typed_with_nothing_written` (dirty,
    merge, lock, busy, collides, a process inside),
    `a_switch_met_by_an_edit_is_rolled_back`; `advance::tests` (markers,
    reachability, switch typing); `procs::tests`. The two older race tests
    move their branch or HEAD by `update-ref`/`symbolic-ref`, since a
    `git commit` or `git switch` now cannot run under bulkload's lock.
  - `estate_advance`: A12 (held `index.lock`/`HEAD.lock`), S1 (switch,
    beside retired, `main` kept), S2 (switch dirty, in use, diverged), R1
    (beside in use kept, retired once free), R2 (standalone beside with its
    own branch upstreams kept, live config untouched); A4-A6's mismatch is
    now a detached capture.
  - fault harness: `estate_advance_after_branch_rolled_back`,
    `_after_seat_rolled_back`, `_rollback_after_intent`, `_rollback_mid`,
    `_switch_after_intent`, `_switch_head_branch`, `_switch_after_branch`,
    `_switch_after_seat`, `_switch_after_index`, `_switch_after_ledger`,
    `_retire_after_rename_live_edited`, `_retire_after_rename_written`,
    `_retire_mid_written`.
- The full `cargo test -p bulkload-agent --locked` and `just rust-check` on
  the pushed head are in the PR body with host, head and TMPDIR.

## Review

One adversarial read-only review (a refuter subagent, nothing run) at
`eef6599`; its findings and what was done (fixes in the second commit):

| # | Finding | Disposition |
| --- | --- | --- |
| 1 | a rerun without the flag ended on the beside journal, leaving a half-advanced checkout (or a renamed beside) as `applied-beside` | fixed: an advance intent counts as a pending workspace intent and a `.retire` record ends no journal; the advance and its retirement are finished without the flag (rows `..._rerun_without_flag`) |
| 2 | a settle recorded current prints for seats no operation touched, so an edit made after a crash became "bulkload's output" a later converge would replace | fixed: the intent carries the proof's prints and the settle records those; the edit is refused `GIT_DESTINATION_MODIFIED` and kept (row `estate_advance_after_branch_then_edited`) |
| 3 | the retirement took its prints after the (long) proof, so a write during the proof could be deleted | fixed: prints before and after the proof, kept `raced` on any change; the intent stores the earlier digest. Residual: a write into the renamed tree during its deletion is lost (narrow; documented) |
| 4 | a crash inside the admin removal (gitdir gone) or a read-only captured directory wedged the retirement on a bare `IO` | fixed: a gone `gitdir` is the record's own once deleting; directories are made writable before deletion; a failed deletion is kept `delete-failed`, the record left for the next pass |
| 5 | gitlinks in the tracked set made every repository with a submodule collide | fixed: gitlinks are never a collision |
| 6 | HEAD switched between the proof and the branch's compare-and-swap let the advance move a branch no longer checked out | fixed: the import now precedes the under-lock proof (the window is the intent write), and HEAD is checked after the compare-and-swap; a switch puts the branch back and refuses `GIT_ADVANCE_RACED` (unit row). Git cannot verify HEAD's target in the transaction that moves its branch |
| 7 | a collision fixed by removing an ignored entry was never retried for the same capture | fixed: the record lists the colliding paths; the item is proved again once one is gone |
| 8 | later converges of an advanced checkout did not import new stash entries | fixed: refs-in carries the stash for any advanced checkout |
| 9 | the collision rule differs from the coordinator's text | decision 6 above, for sign-off |
| 10 | files added into a live ignored directory | fixed: only the capture's tracked files land there (decision 7) |
| 11 | "each refusal writes nothing" was inaccurate after the import | wording fixed (worktree, index and refs untouched; the additive import aside) |
| 12 | a standalone beside's refs by name, hooks, remotes, other reflogs and annotated tags were not checked | fixed: every ref by name and value (own, namespace or provenance), every reflog, no own hooks, no remote URL the live repository lacks; `info/exclude` and other configuration are left to the capture's provenance (open) |
| 13 | a kept beside counts `applied` | decision 9 above (operator's call) |
| 14 | the admin removal compared a spelling of the path | fixed: bound to the `gitdir` bytes recorded at the proof |

## Coordinator decisions on the sign-offs (2026-10-19, on #289)

From the standing rulings (no duplicate bytes on sting; nothing excluded;
every outcome visible):

1. **Accepted**: an untracked directory under a directory the capture
   tracks does not collide (decision 6).
2. **Changed** (decision 2 above): an ignored-vs-untracked *file* conflict
   no longer keeps the whole beside tree. The capture's file (or link) at
   that path lands as a file-level beside `<path>SUFFIX`, create-only
   (through the seat engine, numbered after the delta so a settle names it
   the same; present when it already holds the captured version; left
   kept when something else holds it), counted (`advance-file-besides`,
   closure `file_besides`), and the retirement proof accepts it in place
   of the path. A kept *directory* (a live ignored file where the capture
   holds a directory) still keeps the beside tree: that subtree reconcile
   is the hook for the beside-only-unique lane (`Left.kept` minus
   `Left.besides` is exactly what is left for it).
3. **Accepted**: live-only untracked files the advance left alone are not a
   difference (decision 3).
4. **Changed** (decision 9 above): a beside kept after an advance is a
   duplicate: closure holds the item `refused-pending-review` with the new
   code `GIT_ADVANCE_BESIDE_KEPT` (carrying the reason), dispositionable.

Also done: `estate-diff --advance-ancestor` (and `estate-move --plan-only`
with the flag) previews the advance read-only (`adopt:"advance"`, or
`diverged` with `advance-refused=<CODE>`), so a plan never reports an
advance as a beside landing. And decision 7 above is withdrawn under
"nothing excluded": what the capture holds under a live ignored directory
lands entry by entry where the path is free (never over a live file; an
occupied one is a kept file, landing as a file-level beside).

## Operator ruling 2026-10-19 (interview): "Switch when clean"

Relayed by the coordinator after the review found 4 of the 5 named sting
repositories on another branch than neo's capture (`BRANCH_MISMATCH`).
Verbatim from the relay:

> when the live checkout is on a different branch than neo's capture, and
> (a) live is clean (index + tracked files equal its HEAD, no
> skip-worktree/assume-unchanged, submodules clean), (b) no git operation
> in progress (rebase/merge/cherry-pick/bisect/am, index.lock, HEAD.lock),
> (c) no process has its cwd or an open fd inside the checkout (/proc
> scan, Linux; on Darwin refuse unless proven), (d) neo's branch is not
> checked out in another worktree, (e) neo's commit is reachable after
> refs-in, and (f) no untracked file collides with the capture — then
> switch the live checkout to neo's branch at neo's commit (create the
> branch create-only if absent; if it exists at an ancestor, CAS-advance
> it; if it exists diverged, refuse typed), land the captured seats/index
> as in the advance, and leave sting's previous branch ref exactly where
> it was (record it in the receipt). Otherwise refuse typed (GIT_SWITCH_*
> codes, pending review) and write nothing. Same rollback guarantee as the
> advance. Then the beside retirement applies as for an advance.

Implemented through the advance's own machinery (`advance.rs`): each
condition maps to `GIT_SWITCH_NOT_CLEAN`, `_IN_PROGRESS`, `_IN_USE`,
`_BRANCH_BUSY`, `_UNREACHABLE`, `_UNTRACKED_COLLIDES`, plus
`_BRANCH_DIVERGED`, `_RACED`, `_ROLLED_BACK`. Readings taken:

- "Different branch" is two branch names. One side detached stays
  `GIT_ADVANCE_BRANCH_MISMATCH` (attaching or detaching the live checkout
  was never ruled).
- The live HEAD need not be an ancestor of neo's: its branch stays where
  it was, so nothing of sting's is lost.
- (e) is asked under the lock after the capture's import (its provenance
  makes the commit reachable); `refs-in` itself runs after the workspace
  step, as for every item.
- (c) is asked in the read-only proof and again under the lock right
  before the intent; this process itself is not counted.
- The receipt names the branch left: `advance-switched-from=<ref>@<oid>`;
  closure's `advance` block says `switched`.

## Review of #289 at 9990aeb (coordinator's adversarial pass) and fixes

| # | Finding | Fix |
| --- | --- | --- |
| 1 HIGH | No rollback after the branch CAS; a settle only went forward, so an edit after a crash wedged the checkout with HEAD advanced over the old index (a `git commit -a` then dropped neo's file). Held `index.lock` unchecked. | `index.lock`/`HEAD.lock` are in-progress markers. Bulkload holds its own marked `index.lock` from the under-lock proof through the index publish (a crash leaves it held; a stale marker of this source with no intent is removed under the lock). A foreign edit (or index change) after the branch moved rolls everything back (`rolling_back` durable first): seats by content to the live HEAD's bytes, the edit left, refs by CAS, lock released, `GIT_ADVANCE_ROLLED_BACK`, beside fallback. Settle: published index writes only records; HEAD back at the proof's → rollback; HEAD elsewhere → raced, intent dropped. Fault points `rollback_after_intent`, `rollback_mid`. |
| 2 MED | A resumed `deleting`/`delete-failed` retirement deleted without re-proof; no cwd/fd check. | Re-proved before every deletion step: prints (admin dir included), `/proc/*/cwd` and `/proc/*/fd` (before the rename and before deletion; `in-use`), and a deletion cut short resumes only while no file's ctime is newer than the proof's start (`written-since`), else the tree goes back. The tree is deleted before its administration, so a put-back is whole. |
| 3 MED | A pending `.retire` record only finished on `workspace-restored`; a refusal hid neo's copy at `.bulkload-retire-<hex>`, closure silent. | Pending retirements are finished or put back before the item's workspace step; transient put-backs are `pending` and retried; closure prints `retire_pending` and holds any beside not retired `GIT_ADVANCE_BESIDE_KEPT` pending review. |
| 4 LOW-MED | Admin dir outside the prints; `remove_admin` never checked its location. | Prints include the admin dir; the proof requires it under `<common>/worktrees/` with `gitdir` naming `<beside>/.git` (`admin-foreign`); removal requires `worktrees/` and the proved `gitdir`. |
| 5 LOW | Standalone retirement skipped config, `info/exclude`, `modules`, in-progress state; `refs/carry*` skipped both checks. | Kept typed (`configuration`, `exclude`, `modules`, `in-progress`), never carried into the live config (the advance never touches config). `refs/carry*` held by name and value, else must be reachable commits. softconnect's four branch upstreams will keep its beside, pending review. |
| 6 LOW | A replaced seat's print came from a later `lstat`. | Printed from the written descriptor (`fstat`); a size/mtime moved since the rename records the pre-rename print (never matches). |

## Coordinator decisions on the readings (2026-10-20, at 9116e01)

1. Accepted: a detached side stays `GIT_ADVANCE_BRANCH_MISMATCH`
   (switching off a detached HEAD could orphan a commit).
2. Accepted: a switch does not need the live HEAD to be an ancestor
   (sting's branch stays put).
3. Changed: the no-process check applies to the plain advance too (C19: a
   live agent must not have files change under it; the operator: work is
   "topped up as agents exit organically"). `GIT_ADVANCE_IN_USE`,
   retryable (never in the decided list a journal rerun keeps), pending
   review, and no beside written for that pass when one exists (the item
   refuses typed instead of landing beside).
4. Changed: a standalone beside's configuration entries whose key the live
   configuration lacks (softconnect's branch upstreams) are added to the
   live configuration create-only (`git config --local --add` only while
   the key is absent; never an existing value changed; only values
   `safe_configuration_value` allows), counted in the receipt
   (`advance-config-added=N`), and the beside is then retirable; a key the
   live configuration holds with another value keeps it
   (`configuration:<key>`). `info/exclude` patterns the live exclude lacks
   are appended by the exclude union's rule (its lock, STATE record and
   settle; `advance-exclude-appended=N`); a negation on either side, or an
   exclude another converge proves, keeps the beside (`exclude`). The
   source command registry's `config (writing form)` pin goes 3 to 4,
   argued in its entry and recorded on bulkload#188.

Tests: lib `an_advance_refuses_while_a_process_is_inside`; `estate_advance`
A13 (in use: refused, no beside written, advanced once free) and R2 (two
absent upstreams and an exclude pattern carried, then retired; a key held
with another value kept, live config untouched).

## Verification of #289 at 1b2e1f5 (coordinator, 2026-10-20)

All six earlier findings confirmed fixed by probe; the switch and the
rollback hold. Two configuration-carry defects, fixed with tests:

| # | Defect | Fix |
| --- | --- | --- |
| r6 | A key with two values (`branch.main.merge`) the live repository lacked: the first value went in, then the carry read its own write as a conflict; the beside was kept, the live configuration left with one of two values, uncounted. | Decided per key against the proof's snapshot: every value of an absent key goes in, or none. The plan is in the retirement's STATE record (`config`) before the first write, so a crash mid-key is finished by the rerun (`settle`; fault point `estate.advance.retire_carried`, row `estate_advance_retire_carried`); a key holding anything but planned values keeps the beside. Counted per value (`advance-config-added`). |
| r7 | `user.email` was carried into live's local configuration: absence was read from `--local` only (no includes, no global), and `safe_configuration_value` allows any `user.name`/`user.email`. | Identity and signing keys (`user.*`, `gpg.*`, `commit.gpgsign`, `tag.gpgsign`, `push.gpgsign`, `tag.forceSignAnnotated`) are never carried; one the live configuration does not hold keeps the beside (`configuration:user.email`). Absence is tested against the effective configuration: local with includes, plus the global (`$XDG_CONFIG_HOME`/`~/.config` `git/config`, `~/.gitconfig`) and system (`/etc/gitconfig`) files, each with includes, read by name (bulkload's own Git children run without them). |

R2 covers both (two merge values carried and counted; a key held differently
in the live file or only in the global file; a differing local identity),
with `HOME` pointed at a fixture.

## Open

- **A beside kept after an advance stays** until the operator disposes it
  (`GIT_ADVANCE_BESIDE_KEPT`, pending review): later passes converge the
  live checkout and never revisit the beside.
- **Kept directories** (a live ignored file or link where the capture holds
  a directory) still keep the beside tree: the file-level reconcile of such
  subtrees is the beside-only-unique lane's.
- **The `/proc` check is best effort**: it sees only this user's
  processes (other uids' links are unreadable) and only this mount
  namespace's paths (a process in another mount namespace, a container,
  resolves its cwd and fds to paths that do not compare); a process that
  enters the tree between the last scan and the write is not seen; and a
  link whose target name itself ends in ` (deleted)` is skipped as a
  deleted file. On Darwin nothing is proved: an advance or switch refuses
  `_IN_USE` and a retirement keeps the beside (`in-use`).
- **A hand-deleted `index.lock`** (advance.rs, the settle's "someone's
  HEAD since" branch): after a crash the advance's marked lock stays held
  so the operator's Git refuses; an operator who deletes it by hand and
  then moves HEAD (or commits over the half-advanced worktree) owns that
  state: the settle drops the intent as `GIT_ADVANCE_RACED` and touches
  nothing, so seats the advance had written stay as they are.
- **Darwin**: with the process check on every advance, nothing advances or
  switches on Darwin (unprovable, refused `GIT_ADVANCE_IN_USE` /
  `GIT_SWITCH_IN_USE`); sting (Linux) is the target.
- **A beside's configuration values are carried only for the keys
  `safe_configuration_value` allows** (user identity, push/pull modes,
  branch upstreams to `origin` or `.`, origin's default fetch refspec); any
  other absent key keeps the beside (`configuration:<key>`).
- **Gate codes on a journal rerun**: the metadata-only gate cannot see the
  capture, so a rerun whose live checkout became dirty records
  `GIT_ADVANCE_NOT_CLEAN` even where the item would be a switch.
- **Rollback leaves gitlink directories** an advance created for nested
  repositories (empty directories), and a directory the advance widened
  that is not tracked keeps the widened mode.
- **Global excludes**: bulkload's Git runs with no global configuration, so
  a file ignored only through a custom `core.excludesFile` reads as
  untracked; it collides (refusal, beside) only where the capture holds
  that path.
- A ledger over the cap after an advance is written without a workspace
  ledger (`not-convergeable`), as a landing's is; the next pass then adopts
  rather than converges. Not tested (200k seats).
- The live run (sting) was not run from this lane; it needs this build
  deployed first, and `estate-move --advance-ancestor` given.
