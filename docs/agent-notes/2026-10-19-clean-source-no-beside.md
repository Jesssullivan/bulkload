# 2026-10-19: a clean source lands no tree beside; redundant beside trees retire by proof

Lane: code lane on `bulkload`, branch `feat/clean-source-no-beside-20261019`
(PR #291), stacked on #289 (`feat/adopt-ancestor-20261019`, the advance
lane, in flight: started at `9990aeb`, its `0b5fb1d` merged in), which is
stacked on `origin/main` `9854236` (#282 merged).
Edited on sting in
`/srv/fast-local/jess/git/bulkload.worktrees/clean-source-no-beside-20261019`;
every build and test ran on honey (Q196: never on sting or neo), in
`~/git-build/clean-source-20261019` with `CARGO_HOME`/`CARGO_TARGET_DIR`
under `~/git-build/clean-source-20261019-cargo` and
`TMPDIR=/var/tmp/jess-bulkload-tests/clean-source-20261019`, `nice -n 19`,
`CARGO_BUILD_JOBS=4`, `RUST_TEST_THREADS=4`. Nothing ran against the live
estate. R-N13 (lab R-HOOK-CONVERGENCE-20261004).

## The ruling

Operator standing ruling, 2026-10-16 21:24Z, as the lane brief quotes it:
"refs in always; a tree beside ONLY where neo has uncommitted work". The
brief's gap: estate apply landed a whole second checkout beside the live one
(`<path>.neo-20261016`) even when neo had nothing the live repo lacked. On
sting, 173 beside trees held 61 GB; of the 14 that
`estate-move --adopt --beside .neo-20261016 --refs-in --exclude-union` made
on 2026-10-19, at least 7 were pure duplicates (beside HEAD == live HEAD,
the beside tree clean), two more clean with another HEAD, and two empty
directories left behind. The operator called it continued duplicate copying.

The ruling is not yet an entry in `docs/intent-ledger.md` (its entries carry
the interview's timestamp, session and question, which this lane does not
have); the coordinator should append it.

Requirement served (docs/SPEC.md R4, verbatim): "On DEST each tree lands
once, at its live path (FOOT-18, PUR-20). There are no duplicate landing
archives, stale run directories or rollback copies left behind without an
operator ruling (FOOT-05, FOOT-06, FOOT-07, FOOT-11, FOOT-20). Residue that
may be the only copy is never deleted to make room (KEEP-08, FOOT-13)." And
R0.2 (SOURCE's state fully present and restorable on DEST): every source
file is counted, nothing excluded.

## What changed

1. **No tree beside for a clean source** (`git_carry/converge/clean.rs`,
   `git_carry/converge/files.rs`, the diverged branch of
   `estate::apply_item`). Under `--adopt --beside SUFFIX --refs-in` (so every
   `estate-move`), where the adopt proof refused `GIT_DESTINATION_DIVERGED`
   (or a `GIT_ADVANCE_*` precondition), the capture is read in a scratch
   repository first: tracked-clean (captured index == HEAD tree, no
   intent-to-add, every tracked path a seat with the HEAD's kind, blob and
   executable bit) and its HEAD reachable from a live ref (provenance aside)
   or a captured branch or tag. Then no tree lands beside: the refs go in
   (stash entries too) and the untracked and ignored entries are reconciled
   file by file into the live checkout: absent placed (create-only:
   temporary, fsync, `RENAME_NOREPLACE`), equal adopted, different landed
   as `<path>SUFFIX` (counted), a `<path>SUFFIX` held by something else
   refusing `GIT_BESIDE_FOREIGN` naming it after the rest. Write-ahead
   record `{item}-{blake3(SOURCE)}.files`. Outcome and journal
   `workspace-refs-in` (outcome variants 14, 15), closure `applied` with a
   `files` block and `refs_in`. The live checkout's HEAD, index, tracked
   files, refs and `info/exclude` are never touched.
2. **A beside tree only for tracked changes or a HEAD no ref brings in**:
   anything else lands beside as before; so does every capture without
   `--refs-in`.
3. **No empty beside directory**: the two empty `.neo-20261016` directories
   came from `estate-carry-files --beside` (r03, items `4c38c659...` and
   `6e1f5952...`, `CAPTURE_ABSENT`, the carry refused): the carry made the
   destination, then the session refused. It now removes a directory it
   made when the item ends not carried and the directory is still empty.
4. **`estate-beside-retire`** (`git_carry/beside/subset.rs`,
   `estate/beside_retire.rs`, `main.rs`): proves each existing beside tree a
   subset of its live checkout by rule 1 (module docs), report only by
   default, `--delete` to reconcile, ledger and remove (the advance lane's
   crash-safe removal, factored out of `retire.rs` as `remove_proved`).

## Decisions taken in the lane (for the operator to see)

1. **The rule is the default under `--refs-in`, not a new flag.** It is a
   standing ruling, and `estate-move` always passes `--refs-in`. Without
   `--refs-in` the source's refs would not go in, so a clean source still
   lands beside there.
2. **"HEAD reachable" means from a ref of the live repository other than
   bulkload's provenance (`refs/carry*`), remote-tracking refs and
   `refs/stash`, checked after refs-in ran.** A detached HEAD on a commit
   no captured ref holds, or one whose namespace ref is held at another
   value or was removed by the operator, lands beside.
3. **File-level beside suffix is the `--beside` suffix** (`.neo-20261016`),
   not today's date: reruns on later days find their own earlier landings
   (`beside-present`) instead of making new copies.
4. **Equality is kind, bytes (or link target) and the executable bit**, as
   the advance lane's retirement compares; a mode difference lands beside.
5. **A source directory whose live path is a file (or a link) lands its
   subtree under `<dir>SUFFIX/`**; nothing is ever written through a link.
6. **Retire, standalone trees**: every commit its refs, stash and reflogs
   name must be in the live repository and reachable there (a
   remote-tracking ref's own word, `unreachable-remote:`, since a fetch may
   rebuild it). Branch, tag and other names the live repository lacks are
   created under `refs/<ns>/` on `--delete`; remote-tracking names are not.
   Its raw `config` and `info/exclude` bytes go into the ledger record
   instead of being required equal (the advance retirement requires remote
   URLs ⊆ live; this one does not, since configuration is not content and
   is ledgered).
7. **Rebuildable roots in a beside tree** first refused by default
   (`rebuildable:<path>`, `--drop-rebuildable` to drop them); superseded by
   the operator's ruling of 2026-10-20 ("Ledger as rebuildable", below):
   they are derived output, ledgered and gone with the tree, no flag.
8. **The clean path's refs follow the source.** Its namespace refs are the
   only holder of neo's work (no tree beside), so this item's own earlier
   refs move by compare-and-swap, as a beside landing's do; elsewhere
   refs-in stays create-only.
9. **An existing beside landing is left alone by apply**: the clean pass
   names it (`beside-existing=<path>`) for `estate-beside-retire`; it is
   never removed by apply.

## Review

One adversarial read-only review (a refuter subagent, nothing run) at
`1230c44`; findings and what was done (`0802d25`):

| # | Finding | Disposition |
| --- | --- | --- |
| M1 | "HEAD reachable after refs-in" assumed: create-only refs-in keeps this item's own namespace ref at an old value, so a source that moved on lands no tree and its HEAD is in by no ref | fixed: the clean path runs refs-in first in follow mode (this item's own namespace refs move by compare-and-swap, as a beside landing's do), then checks HEAD reachable from a live ref, else lands beside in the same pass (C7, both cases) |
| M2 | a linked beside never checked to own its administration: a `cp -a` of a live linked worktree would be retired and the live worktree's admin deleted | fixed: the admin's `gitdir` must name the tree back (`worktree-link`), and the live checkout's own admin refuses `shared-admin` (R2 "copy") |
| M3 | a standalone tree's `.git` deleted without proving `modules/`, `lfs/`, `git-crypt/`, `info/*`, includes; config ledgered lossily | fixed: an allow-list of `.git` entries (`unknown-git:<name>`), `info/` limited to exclude/refs/packs, `borrowed-by-live`, `config-include`, raw config bytes ledgered (refused when unreadable) (R4) |
| M4 | rebuildable roots deleted unproved | fixed: refused `rebuildable:<path>` by default; `--drop-rebuildable` drops them as a capture omits them, counted (R4) |
| m1 | a commit in a linked tree between proof and removal (admin not in the prints) | fixed: the admin's prints are in the proof's raced check, and HEAD and admin are looked at again just before the rename; the rename-to-delete window is the advance retirement's (documented there) |
| m2 | ledger keyed by path; `retired` written after the intent's removal | fixed: one ledger per retirement (`beside-retire-<tag>-<ns>.retired`), marked `deleting` before the removal; `settle_all` marks it retired when tree and intent are gone |
| m3 | a live tracked file deleted in the worktree would be re-placed | fixed: live index paths are passed to the plan; the source's entry lands beside (C9, unit row) |
| m4 | a gitlink typechange judged clean | fixed: a file or link at a gitlink path is a tracked change |
| m5 | a crash leaves made directories at `0700` or empty | fixed: made directories are recorded write-ahead; settle removes empty ones, gives the rest their mode (unit row) |
| m6 | one tree's IO error aborts the run; an assess error became a refusal | fixed: per-tree `reason=error:<CODE>`, summary `errors=`; an assess error lands beside as before |
| m7 | the clean path imported fewer refs than the beside landing it replaces | fixed: every captured ref but the stash under the namespace (C8) |
| m8 | live tips counted remote-tracking refs and the stash | fixed: excluded |
| m9 | carry-files: a crash after the mkdir leaves an empty directory forever; a partial transfer leaves a non-empty beside | first fixed (an empty beside destination is removed on refusal); the second left: a partial transfer's bytes are its resume state, recorded in its transfer rows |

## Coordinator decisions and the independent review (2026-10-20)

- Reflog-only commits (operator ruling 2026-10-16, "full history. drop
  nothign. ever."): carried into the namespace, `refs/<ns>/reflog/<tree>-<tag>/<oid>`,
  create-only, ledgered and counted, objects packed from the beside tree
  when the live store lacks them (`894c0e3`).
- `estate-move --plan-only` previews `adopt:refs-in` with `beside=none`
  (`estate-diff --refs-in`).
- estate-handoff into a `workspace-refs-in` live checkout: first left
  undone (hand-off is bound to a workspace ledger; a refs-in live checkout
  is sting's own, its `AGENTS.md`/`CLAUDE.md` sting's tracked files);
  decided 2026-10-20, below.

## Operator ruling 2026-10-20 (interview): "Ledger as rebuildable"

Relayed by the coordinator, on neo's untracked build output when a beside
tree retires. Implemented:

- Derived entries (`build/`, `dist/`, `.svelte-kit/`, `.artifacts/`,
  `bazel-*` links, `mktemp`-named directories, `.serena/`, the rebuildable
  roots: the classes held before) are **never placed into the live
  checkout and never compared**. Each entry's path, kind, mode, size and
  BLAKE3 (a link's target; whether the live checkout holds something at
  its path, by `lstat` only) is ledgered as `rebuildable`: in the
  retirement's ledger record (`beside-retire-<tag>-<ns>.retired`, and its
  `.files` record), hashed under the destination lock before anything is
  written, and the tree retires only when its prints at the second proof
  are still the first proof's (so the hashes name the bytes removed).
  Every other entry must still prove contained.
- The same rule in the apply path: a clean source's derived seats are
  ledgered in its `.files` record (BLAKE3 of the blob), never written; the
  bytes stay in the capture's provenance.
- Counted everywhere: receipt `files-rebuildable=N
  files-rebuildable-bytes=B`; the retire report's `rebuildable=`
  `rebuildable-bytes=` per tree and in the summary; `estate-placements`;
  closure's `files` block (`rebuildable`, `rebuildable_bytes`); and a new
  closure `beside_retirements` block, from any PRIVATE_STATE holding
  retirement ledger records (outside the gate, as the omissions are).
- `--place-derived` and `--drop-rebuildable` are **removed** (a run that
  passes either is refused `FIELD_DOMAIN_VIOLATION`); the
  `derived-held:` and `rebuildable:` refusals are gone.
- One narrowing, mine: a derived name whose directory has a tracked file
  at or under it (in the source's index or the live one) is a source
  directory (`build/` holding tracked scripts), so an untracked file there
  stays source-like and is placed create-only, never ledgered away
  (`class_in`).
- Source-like untracked files keep the create-only placement with its
  manifest (`estate-placements [--undo]`).

Hand-off for a `workspace-refs-in` item (the coordinator, 2026-10-20):
estate-handoff never writes into such an item's live checkout (sting's
own tracked `AGENTS.md`/`CLAUDE.md`). It records the hand-off facts in
bulkload's STATE as a typed note instead, `{item}-{blake3(SOURCE)}.handoff-note`
(`refs-in-live-checkout`: capture, live and source paths, PLAN, the refs
its refs-in created and kept, the file tally, each file landed beside),
its line's outcome `noted` (`unchanged`, `planned` on `--dry-run`,
`checked`, `cleared` by `--clear`, which removes the note), and closure
prints it on the item's row as `handoff_note` (`null` until recorded).
The apply's H step leaves a refs-in item alone (C14).

Independent review of #291 at `d310063` (relayed by the coordinator):

| # | Finding | Disposition |
| --- | --- | --- |
| 1 | rebuild on #289's head; use its in-use checks and admin re-proof | merged `583c0b9` (`7c34e60`) and `ae09525` (`44ccc8c`); `remove_proved` is rebuilt on #289's retire (its `in_use` before the rename and in `finish`, its admin-inclusive prints); the proof also refuses `in-use` up front |
| 2 | record every placement; undo or list; hold placements pending the build-output ruling; classify source-like vs derived | every directory made and every file placed or landed beside is recorded by identity as it goes; `estate-placements [--undo]`; `class_of` (`build/`, `dist/`, `.svelte-kit/`, `.artifacts/`, `bazel-*`, `mktemp`-named, `.serena/`, rebuildable roots); derived writes held by default (`--place-derived`), counted per class in receipts and the retire report |
| 3 | refs-in `kept` ignored by closure | a `workspace-refs-in` row with `refs_in.kept > 0` is `refused-pending-review` `GIT_DESTINATION_OCCUPIED` (C11) |
| 4 | reachability counted remotes, stash, provenance | one ref set for HEAD and every commit: local branches, tags and this item's namespace |
| 5 | settle touched directories it never made; EEXIST counted placed; no total check | directories recorded by inode as made; settle and undo touch only those; EEXIST is adopted; the tally must equal the planned entries |
| 6 | scan took any `*SUFFIX` directory | a tree must carry a bulkload workspace ledger for SOURCE (`not-bulkload-landed`), R6 |
| 7 | path-based writes; gitlinks; dropped rebuildables only counted | writes through `openat` descriptors (`O_NOFOLLOW` per component, `mkdirat`, `create_excl_at`, `renameat2`); files under a gitlink whose checkout is missing refuse `GIT_DESTINATION_PARENT_MISSING`; each dropped rebuildable path ledgered with `equal`/`differs`/`absent` against the live checkout |

## Report-only run on sting (read-only)

2026-10-20 ~08:30Z, `estate-beside-retire --scan /srv/fast-local/jess/git
.neo-20261016 neo-inplace-20261016 <absent STATE>` (the SOURCE label the
beside landings' ledgers carry) with the honey-built agent at `2064bc7`
(sha256 `ef718f31...e7d6`, copied to sting, `nice -n 19 ionice -c3`; no
STATE made, nothing written). Report:
`/srv/fast-local/jess/bulkload/receipts/beside-retire-report-sting-20261020-2064bc7.txt`
(earlier runs at `3e44c8b` and `b6d12e3` beside it, superseded).

- 139 candidates (24 more `*.neo-20261016` directories have no live path).
- **45 retirable, 4.31 GB allocated**: 38 standalone (4.09 GB), 5 linked
  (0.22 GB), 2 empty. Retiring them writes 24 entries, all source-like (10
  placed, 11 landed as `<file>.neo-20261016`, the rest directories), adopts
  1,108 derived entries already equal in the live checkouts, creates 132
  refs under the namespace and 2,873 reflog refs (`refs/neo-20261016/reflog/`),
  and carries the objects of 1 commit the live store lacks.
- 94 refused, typed: `live-not-a-checkout` 22, `tracked-changes` 18 (neo's
  uncommitted work), `not-bulkload-landed` 17 (no bulkload ledger: hand
  copies), **`derived-held` 17** (34,685 derived entries, mostly
  `bazel-bin`, `.svelte-kit`, `.serena`, `dist`, `build`, waiting for the
  build-output ruling), `rebuildable` 4, `live-absent` 4,
  `unreachable:head` 3 (DarwinNicUtil, both `l54-pr*`), `refs` 3,
  `unknown-git` 2, `nested` 2, `other-repository` 1, `not-a-checkout` 1.
- The narrower ref set (review item 4) first refused 28 trees
  `unreachable-remote` (`63068ab`); `2064bc7` carries a tree's every ref the
  live repository lacks (remote-tracking ones too) under the namespace, so
  none is dropped.

## Verification of the 2026-10-20 change (stopped by priority change)

On honey before the coordinator paused this lane (only #280, #294, #278
move; nothing to run on honey or pzm): `cargo fmt`, `cargo check
--all-targets` ok; unit rows `converge::files` 7/7 and `closure` 37/37,
`source_command_registry` 22/22, `estate_clean_source` 22/22 (C12, C14,
R4, R8 new or rewritten). Clippy found two doc lints, fixed in text only,
not re-run. Not run yet: clippy after that fix, clippy with
`fault-injection,io-trace`, `estate_handoff`, `estate_adopt`,
`estate_move`, `estate_carried`, the fault harness, the full suite, and
the regenerated read-only sting report. A last honey run's ssh session
dropped (exit 255); its cargo target under
`~/git-build/clean-source-20261019-cargo` is still to delete.

## Open

- A retired standalone tree whose objects another repository than the
  live one borrows (alternates elsewhere) is not detected.
- The clean assessment builds a second scratch repository (the adopt proof
  built one), one more index of the bundle for a diverged item.
- No fault-harness rows for the reconcile (unit rows prove the write-ahead
  settle; a crash leaves only recorded temporaries the next pass removes).
- The ruling's intent-ledger entry (above).
- Closed since: the plan-only preview (`894c0e3`), reflog-only commits
  carried (`894c0e3`), a process inside a tree refuses `in-use` (#289's
  check), the refs-in hand-off (a note in STATE, 2026-10-20).
