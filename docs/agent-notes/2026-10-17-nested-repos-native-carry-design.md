# 2026-10-17: carrying custody-only nested repositories natively (M2r2 item 5)

Design only; nothing here is built. Lane: M2 round 2, item 5. Rulings
cited: OI-1003-Q185 (nothing is dropped from a carry), R-N73, R-N83, R-N89,
R-N114 (nested custody and planned nests, PR #53), R-N13 (this note).

## The finding

`estate-capture` records a foreign repository nested in a captured checkout
(a `.git` directory, or a `.git` pointer file into another administration)
as `NestedRepository` custody when it is provably clean (R-N73, R-N83):
the nest's HEAD, admin, `unpushed` count and its ignored files (R-N89). Its
tracked content and history are not in the outer bundle. Unless the nest is
also planned as its own item (R-N114, `carried-by=<item>`), it never lands
as a repository.

The in-place run carried these nests by hand. A lane script read each
capture's `nested-repository` receipt lines and planned one file pull per
nest into the outer's landed workspace. It also planned one pull of the
outer's `.git/modules` for every nest whose `.git` is a pointer file into
it. Tonight that was 31 rows: 27 nests and 4 admin directories. Several
nests held commits no remote has (`unpushed` from 29 to 703), so they were
unique state, not rebuildable state. The script skipped:

- gitlink-only rows (an unpopulated submodule, landed by the outer as an
  empty directory, B2/N7);
- nests already planned as items;
- nests inside an item that was itself carried as files.

It stopped for review (exit 8) when a pointer file's admin directory was
not under a `.git/modules`, or was under a linked-worktree outer's
`.git/modules`, which it could not map mechanically.

A file carry keeps the bytes. It loses everything the Git carry proves:

- no ref, index or HEAD proof;
- no capture key or reuse;
- no drift custody;
- no `estate-diff` row;
- no closure-report entry;
- no converge on a rerun.

A nest the files carry lands is bulkload's in name only.

## What already exists

R-N114 does most of the work for a nest that is planned:

- capture runs nested items first, deepest first (`capture_in` levels);
- the outer names the nest `carried-by=<item>` and carries none of its
  seats;
- a carrier that refused makes the outer refuse `GIT_NEST_CARRIER_REFUSED`;
- apply restores outer items before the items nested in them;
- every target collision is `GIT_DESTINATION_OCCUPIED`.

The only gap for a directory nest is that nobody plans it. Planning it by
hand needs two passes: one capture to learn the nests, then a plan edit
and a second capture.

## Design: derived nest items

### 1. Opt-in at capture

Add `estate-capture --nests=custody|carry`. The default, `custody`, is
today's behaviour. Under `carry`, every custody nest whose outer item
captures becomes a **derived item**:

```text
source     = <outer source>/<nest path>
repository = <outer target>/<nest path>
workspace  = <outer target>/<nest path>
```

Here `<outer target>` is the outer's workspace, or its repository for a
standalone item. The derived item's id comes from the same `id(&Item)` as a
reviewed `estate-add`. Planning the nest explicitly later is therefore a
duplicate the plan drops, not a second item. Each ledger is keyed the same
either way.

Derived items are **not written into the plan**. The plan stays the
reviewed list ("no automatic worktree proliferation"). They live in the
outer's capture record: the existing `{bundle}.nested` sidecar gains
`derived=<item id>` per nest. The corpus also gains a `derived.v1` index
(outer item to derived ids) under the corpus lock.

`estate-show PLAN CORPUS` lists derived items with
`derived-from=<outer item>`. Apply, diff, handoff, retarget and
closure-report enumerate the plan plus the derived items. See open
question 1.

### 2. Detection and ordering in capture

Derivation needs the nests before the deepest-first capture runs. The
census already finds directory nests and decides custody before any byte
is read. Under `carry`, capture therefore runs in two steps:

1. A census-only pass over each outer derives its custody nests. This is
   the read-only discovery the census already does before the byte pass.
2. The existing deepest-first capture runs over the plan plus the derived
   items.

The outer's capture then sees its nests as planned. It names them
`carried-by` and carries none of their seats (R-N114: the nest item owns
them), so no seat is carried twice. If a nest's census answer changes
between the two steps, the outer refuses `GIT_AUTHORITY_CHANGED`, as any
nest moving under a pass does today.

Derived items may nest further. A custody nest inside a derived nest is
derived again at the next level, up to the census depth. The nest's own
R-N73/R-N83 refusals stay strict:

- `GIT_NEST_INNER_REPOSITORY` keeps refusing an ignored inner repository;
- a dirty nest still refuses the outer.

Relaxing the clean-nest rule for a derived nest is the open question #53
left (open question 3). It is not part of this design.

### 3. Nest kinds

- **`gitdir=Directory`** (a `.git` directory): a standalone item. The
  existing standalone carry applies unchanged.
- **`gitdir=PointerFile` into the outer's own `.git/modules/<name>`** (an
  absorbed or submodule-style checkout that the outer does not register as
  a gitlink): the derived item's repository is the admin directory.
  - Its destination is `<outer destination common dir>/modules/<name>`,
    inside the outer's administration.
  - Apply needs one new landing shape, "absorbed nest":
    - land the admin directory there;
    - write the nest's `.git` pointer file (relative, as Git writes it);
    - set `core.worktree` in the admin's config to the nest's destination
      path (Git writes it absolute; it is re-spelled, and the respell is
      recorded).
  - This replaces the lane's whole-`.git/modules` file pull, which also
    swept in admin directories of unpopulated submodules no item owns.
  - Those admin directories stay a files carry until gitlink submodules
    are designed (out of scope).
- **`PointerFile` whose admin is elsewhere** (not under the outer's common
  dir, or under another repository): refuse with a new typed
  `GIT_NEST_ADMIN_FOREIGN`, carrying the nest path. The lane stopped by
  hand on these.
- **A linked-worktree outer**: the admin is under the common dir's
  `modules`, which the main item's repository owns. The derived item
  belongs to the main item's family, keyed by the outer item. If the main
  is not planned, it refuses `GIT_NEST_ADMIN_FOREIGN` as above.
- **Gitlink-only rows**: unchanged (an empty directory the outer lands).
- **A nest inside an omitted rebuildable root**: unchanged. It is omitted
  with its root and counted there; only nests the census records as custody
  are derived.

### 4. Apply

Derived items land after their outer (the existing outer-before-nested
order), each into the outer's already-landed workspace.

- **Outer landed in this pass, or earlier by bulkload:** the nest path is
  absent, because the outer carried none of its seats. The derived item
  lands as a first landing (item 1's absent-parents rule covers any
  levels).
- **Outer refused or unlanded:** the derived item refuses
  `GIT_DESTINATION_PARENT_MISSING`, pending its outer. It never creates the
  outer's directories itself, since that would lock the outer out with
  `GIT_DESTINATION_OCCUPIED`, the item 1 P107 hazard.
- **Nest path already holds a checkout** (tonight's files carry, or an
  operator's clone): `--adopt` applies per item as today. Equal is adopted
  in place (HEAD, index entries, seats), differing refuses
  `GIT_DESTINATION_DIVERGED`, and an unrelated repository refuses
  `GIT_DESTINATION_OCCUPIED`. This is the migration path for the nests
  carried as files tonight: a `--nests=carry` capture plus `--adopt` makes
  them bulkload's without moving a byte.
- The outer's workspace ledger records the nest path as `carried-by`, so
  the outer's converge and adopt proofs leave it alone. Today's
  planned-nest exclusion ("extra destination seats ... planned nests are
  left alone") is extended to derived items.

### 5. Ignore rules and the files carry

The outer's ignore rules decide nothing about the nest. Git already stops
at a nested repository (an untracked `nest/` entry, or an ignored path).
The census treats the nest root as a boundary whichever it is.

What must hold:

- **Nothing carried twice.** A nest's ignored files belong to the derived
  item once it exists, never to the outer. The census emits them under
  exactly one owner. A test asserts each seat's owner count is 1 (sum of
  outer seats plus nest seats equals the files under both).
- **Nothing dropped (OI-1003-Q185).** Each custody nest has exactly one
  outcome:
  - derived and captured;
  - derived and refused (the outer then refuses `GIT_NEST_CARRIER_REFUSED`
    and both appear in the closure ledger as refused-pending-review);
  - or, under `--nests=custody`, custody exactly as today.

  closure-report gains a `nests` check: every `nested-repository` custody
  row of a `carry` capture has a derived item with a closure outcome.
- **The plan-level file carry (`pull`).** A derived item's subtree joins
  the set of paths a Git item owns. A `pull` of the outer's tree in the
  same estate skips it, as it skips other Git items, so the two carries
  never race for the same seats.

### 6. estate-diff rows

Each derived item gets a row like a planned item's, plus
`derived_from=<outer item>` and `nest_kind=directory|absorbed`. The
classes are the usual ones. New hints:

- `outer-unlanded`: the outer has not landed, and apply would refuse
  `GIT_DESTINATION_PARENT_MISSING`.
- `files-carried`: a checkout at the nest path that no ledger names, which
  `--adopt` would prove. Without `--adopt` it shows as `differs` with that
  hint.

The diff shares its scratch import per repository (M2r2 item 3), so a
derived nest costs one import.

### 7. Retarget, handoff, reseat

- **estate-retarget (#252):** re-points derived items with their outer,
  since they derive from its target.
- **estate-handoff:** writes no block in a derived nest by default, because
  the nest is not a workspace an operator chose. Open question 4.

## Tests to build

1. A clean directory nest with commits no remote has lands as a
   repository. HEAD, refs, index and ignored files equal; `unpushed` refs
   present.
2. Seat ownership: every file under the outer and the nest is carried
   exactly once (count assertion, R-N114).
3. A dirty nest still refuses the outer under `carry` (R-N83 unchanged). A
   stashed nest refuses `GIT_NEST_STASHED`.
4. A derived capture that refuses makes the outer refuse
   `GIT_NEST_CARRIER_REFUSED`. closure-report lists both.
5. An absorbed nest (`PointerFile` into the outer's `.git/modules`) lands
   with the pointer file and a re-spelled `core.worktree`. `git -C nest
   status` is clean at the destination.
6. A `PointerFile` nest with an admin outside the outer refuses
   `GIT_NEST_ADMIN_FOREIGN` before any write.
7. A nest of a linked-worktree outer, with the main planned and unplanned.
8. A nest in an omitted rebuildable root stays omitted and counted.
9. Apply ordering: outer then nest. Outer refused gives the nest
   `GIT_DESTINATION_PARENT_MISSING` with nothing written.
10. A files-carried nest at the destination: `--adopt` adopts an equal one
    and refuses a differing one `GIT_DESTINATION_DIVERGED`.
11. An explicit `estate-add` of a derived nest dedupes (same id). A rerun
    reuses both captures.
12. `estate-diff` rows for each class and hint. Read-only (no write under
    the destination).
13. Fault-harness rows: a crash between the outer's landing and the nest's,
    and inside the absorbed-nest config respell.
14. Two levels of derived nests.

## Open questions for the operator

1. Should derived items live only in the corpus (this design), or should
   `estate-capture --nests=carry` append them to the plan as reviewed items
   (simpler for every verb, but capture would write the plan)?
2. Should `carry` be the default once built? Today's default drops nests'
   history from the Git carry (the lane had to make up for it). Q185 points
   to `carry`.
3. #53's open question: relax the outer-side clean-nest refusals for a
   derived nest, which carries its own index and worktree? It would let a
   dirty nest be carried instead of refusing the outer.
4. Handoff blocks in derived nests: none (proposed), or the same block as
   their outer?
5. Unpopulated submodules' admin directories under `.git/modules` that no
   checkout uses: keep carrying them as files, or design gitlink submodules
   now?
