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
7. **Rebuildable roots in a beside tree refuse by default**
   (`rebuildable:<path>`); `--drop-rebuildable` applies the capture's own
   policy (the fixed `REBUILDABLE_DIRECTORIES`, nothing tracked below):
   counted, dropped with the tree. That is the operator's call per run.
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

## Report-only run on sting (read-only)

2026-10-20 ~05:55Z, `estate-beside-retire --scan /srv/fast-local/jess/git
.neo-20261016 neo <absent STATE>` with the honey-built agent at `3e44c8b`
(sha256 `813f3c98...68b1`, copied to sting, run `nice -n 19 ionice -c3`;
no STATE made, nothing written). Full report:
`/srv/fast-local/jess/bulkload/receipts/beside-retire-report-sting-20261020-3e44c8b.txt`.

- 137 candidates (a `*.neo-20261016` directory with a live path; 24 more
  have no live path, and the `.worktrees` directories inside checkouts were
  not yet scanned by that binary).
- **62 retirable, 16.8 GB allocated**: 53 standalone (16.0 GB), 7 linked
  (0.79 GB), 2 empty (the two `oauth-mux` leftovers). Retiring them would
  place 29,243 untracked/ignored files into their live checkouts, land
  3,043 as `<file>.neo-20261016`, and create 10 refs under the namespace.
- 75 refused, typed: `tracked-changes` 25 (neo's uncommitted work: what a
  beside tree is for), `live-not-a-checkout` 20, `unreachable:logged` 11
  (a reflog names a commit the live repository lacks; telefreeze, A-nix-src,
  B-nixos-nix among them), `unreachable:head` 3 (DarwinNicUtil and the two
  `l54-pr*` worktrees: their HEADs are not in sting's refs yet; a clean
  pass of this build brings them in), `unknown-git` 5 (`modules` 2,
  `lost-found`, `COMMIT_MSG_A/B` in `upstream/nix{,-src}`), `live-absent` 4,
  `refs` 2, `nested` 2, `unborn`, `other-repository`, `not-a-checkout` 1
  each.
- The first run (`46cdaa6`) refused 57 standalone bulkload landings
  `unknown-git:carry-config` and 9 `worktrees` (prunable copies of neo's
  worktree administration); `067c6b0` ledgers the receipts and proves a
  gone worktree's HEAD and reflogs instead.

## Open

- `estate-diff` / `estate-move --plan-only` still preview `beside=landing`
  for a clean diverged item (estate-diff takes no `--refs-in`); the apply
  then lands no tree.
- estate-handoff writes no block into a `workspace-refs-in` live checkout
  (`HANDOFF_NOT_LANDED`, as for a beside item's live path).
- A retired standalone tree whose objects another repository than the
  live one borrows (alternates elsewhere) is not detected.
- The clean assessment builds a second scratch repository (the adopt proof
  built one), one more index of the bundle for a diverged item.
- No fault-harness rows for the reconcile (unit rows prove the write-ahead
  settle; a crash leaves only recorded temporaries the next pass removes).
- A session whose cwd is inside a retired tree is left in a deleted
  directory (no process inspection, as for the advance retirement).
- The ruling's intent-ledger entry (above).
- **Reflog-only commits** (11 trees on sting): the proof requires every
  commit a beside tree's reflogs name to be in the live repository. They
  are history Git itself expires; whether to drop them, or to carry them
  in under `refs/<ns>-logged/` before a retirement, is the operator's call.
