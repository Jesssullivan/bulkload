# 2026-10-19: a clean source lands no tree beside; redundant beside trees retire by proof

Lane: code lane on `bulkload`, branch `feat/clean-source-no-beside-20261019`,
stacked on #289 (`feat/adopt-ancestor-20261019` at `9990aeb`, the advance
lane, in flight), which is stacked on `origin/main` `9854236` (#282 merged).
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
   bulkload's provenance (`refs/carry*`), or from a captured branch or tag**
   (which refs-in brings in). A detached HEAD on a commit no branch or tag
   holds lands beside.
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
   created under `refs/<ns>/` on `--delete`; remote-tracking names are not
   (the refs-in rule carries branches and tags). Its `config` and
   `info/exclude` bytes go into the ledger record instead of being required
   equal (the advance retirement requires remote URLs ⊆ live; this one does
   not, since configuration is not content and is ledgered).
7. **Rebuildable roots in a beside tree are counted, not reconciled**, as a
   capture omits them (the fixed `REBUILDABLE_DIRECTORIES`, nothing tracked
   below).
8. **An existing beside landing is left alone by apply**: the clean pass
   names it (`beside-existing=<path>`) for `estate-beside-retire`; it is
   never removed by apply.

## Open

- `estate-diff` / `estate-move --plan-only` still preview `beside=landing`
  for a clean diverged item (estate-diff takes no `--refs-in`); the apply
  then lands no tree.
- The clean assessment builds a second scratch repository (the adopt proof
  built one), one more index of the bundle for a diverged item.
- No fault-harness rows for the reconcile (unit rows prove the write-ahead
  settle; a crash leaves only recorded temporaries the next pass removes).
- A session whose cwd is inside a retired tree is left in a deleted
  directory (no process inspection, as for the advance retirement).
- The ruling's intent-ledger entry (above).
