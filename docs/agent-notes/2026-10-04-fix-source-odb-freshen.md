# 2026-10-04 — v1 capture freshened the source object store; bare sources refused untyped (lane fix-source-odb-freshen)

- **Lane:** fix-source-odb-freshen, on host sting.
- **Issue:** [#162](https://github.com/Jesssullivan/bulkload/issues/162).
- **Worktree:** `bulkload.worktrees/fix-source-odb-20261004`.
- **Branch:** `fix/source-odb-freshen-20261004`, cut from origin/main 4a10bb8.
- **Pushed, no PR:** opening a PR is the coordinator's call.

Rulings cited:

- OI-1003-Q5 and OI-1003-Q16 (S2): bulkload never writes to the source, and
  every source access is typed.
- OI-1003-Q1 (S4): a bare `IO` never counts as accounted.
- OI-1003-Q38: cited by the lane brief.
- R-N13: receipts cite rulings; this note.

## S2: what freshened the source

- **The private repository already borrowed the source.** The capture's
  private repository read the source through `objects/info/alternates` and
  wrote only into itself.
- **That was not enough.** Git freshens (`utime(NULL)`) any existing copy of
  an object it is asked to write, in its own store or in **any alternate**.
  - A loose copy: `freshen_loose_object`.
  - A packed copy: `freshen_packed_object`, which re-stamps the whole
    `.pack`.
- **Probes** (git 2.52 on the host, 2.54 in the devShell) re-stamped source
  objects through alternates for each of these:
  - `write-tree` with an invalidated cache-tree, which rebuilds a tree the
    source holds;
  - `hash-object -w` of bytes the source holds, which re-stamped a pack;
  - `fast-import`. Below `fastimport.unpackLimit` (100 by default) it
    explodes its pack through the object writer, re-stamping every loose
    source copy. Its own pack store never freshens.
- **No-freshen readers.** `index-pack`, reached by a bundle fetch (thin, with
  prerequisites), and `fast-import` with `unpackLimit=0` re-stamped nothing,
  each with the source visible.
- **Writers in the v1 capture path.**
  - `metadata()`: `hash-object -w` and `mktree`.
  - `commit_tree`: `commit-tree`.
  - `export_pass`: `write-tree`.
  - `raw_tree`: `fast-import`, twice.
  - No `stash create` or `update-index` runs against the source.
  - The shallow envelope and `chain::flatten` repositories borrow nothing.

## S2: the fix (`git_carry.rs`)

- **Write store.**
  - `git_env::WRITE_STORE` (`objects-written`) is a write store beside the
    private repository's `objects`.
  - `prepare_private` creates it and lists it first in
    `objects/info/alternates` (relative), then the source.
  - Readers (bundle, pack, fetch, fast-import, update-ref) see both stores.
- **Writer builder.** `git_writer` / `writing_privately` is the one builder
  that adds `GIT_OBJECT_DIRECTORY`. It points at the write store, which has
  no alternates.
  - `hash-object -w` and `mktree` in `metadata()` use it.
  - The capture's `write-tree` uses it, now with `--missing-ok`, because
    the write store cannot see the index's blobs. Packing the bundle still
    needs every blob it carries.
- **Archival commits.**
  - Archival commits are now `hash-object -t commit` in the write store
    (`commit_object`), byte for byte what `commit-tree -m` wrote under the
    archival identity.
  - A test pins this for sha1 and sha256 and five labels.
  - `commit-tree` needs its tree in the store, and fast-import does not
    store a raw tree that a source pack already holds (a clean checkout).
- **fast-import.** `fastimport.unpackLimit=0` is in `git_env::CONFIG`, so
  fast-import never loosens its pack. The estimate probe's `g()` preamble
  carries it too, as the pin test requires; it is inert there.
- **The source store is read-only.** No writer can see it, and the readers
  that can do not freshen.

## S4: bare repositories

- **Before.** Estate-capture of a bare repository refused with
  `Io(Some(2))` in `source_index`, which read the absent index.
- **Now it is carried as ref custody.**
  - `source_index` returns empty index bytes when the index is absent and
    Git says the repository is bare (`rev-parse --is-bare-repository`).
  - `capture_census_planned` returns an empty census when the root is its
    own common dir and bare. A non-bare `.git` given as a root is
    unchanged.
  - `export_pass` writes no index file, so the staged and worktree trees
    are the empty tree.
  - Refs, HEAD, head-symbolic, exclude and configuration are carried as
    before.
  - Apply of a no-workspace item imports them (`refs-imported`).

## Tests and evidence

- **New module `git_carry/source_inert_tests.rs`.**
  - `p34_a_capture_leaves_the_source_lstat_census_unchanged` (P34, S2):
    - Over generated repositories: packed and loose objects, 0–2 stashes
      (the last optionally `-u`), a reverted staged change, a staged edit
      with an unstaged edit on top, untracked copies of tracked bytes, and
      the empty blob.
    - Three captures run: a plain one, then two that reuse and chain on the
      one before.
    - Each leaves the whole source root's lstat census unchanged. The
      census is mode, size, mtime, mtime_nsec, ctime, ctime_nsec, ino and
      nlink, over the worktree and all of `.git`. Object files are
      backdated first.
    - R25 holds across the split stores. The retained pass is presented as
      started 10 s later, so seats are not racy, without sleeping.
      - The second pass read exactly the 8-byte new seat.
      - The third read 0 bytes. It reused by name blobs that only the
        source holds, because the chained bundle excluded them, through
        fast-import's `M <oid>`.
    - The three bundles fetch into a store-less repository with a clean
      `fsck --connectivity-only`, and the carried staged tree equals the
      source's index.
  - `private_commits_are_exactly_what_commit_tree_writes`.
  - `a_private_writer_is_the_hardened_child_in_the_write_store`.
  - `s4_a_bare_repository_is_carried_as_ref_custody` covers mirror refs,
    tags and HEAD in the bundle, a chained second pass, the mirror's census
    unchanged, empty staged and worktree trees, and the import.
- **Estate test.** `estate::tests::a_bare_repository_item_captures_and_applies_as_ref_custody`
  checks for receipt `captured`, then `refs-imported`.
- **Fails on the old code** (the tests with production code at 4a10bb8,
  `PROPTEST_MAX_SHRINK_ITERS=0`):
  - P34, first capture: 4 loose objects and the pack moved. mtime and
    ctime changed (978307200 → now); mode, size, ino and nlink did not.
    These are the issue's symptoms.
  - S4: `Io(Some(2))` from `capture_key_parts`. The estate pass returned
    `ContractSelfInconsistent`, with the item refused.
- **Passes on the fix:** CI seed, 6 cases. The deep tier
  (`BULKLOAD_PROPTEST_DEEP=1`, random seed, 120 cases) passed in 228 s.
- **`just check-fast` receipt** (in `nix develop .#default`, under the
  shared `.check-fast.lock`, nice 10):
  - Exit 0 at bd74e09 (the merge below), 2026-10-04T10:12Z.
  - 25 cargo result lines: 687 passed, 0 failed, 8 ignored. The lib suite
    alone is 439 passed.
  - The fault harness passed: `fault_harness`, `power_loss` (both copy
    proofs), `resume-power-loss` (both adoption proofs) and
    `io-partial-write-alone`.
  - The CI contract passed: 22 tests OK.

## Shas

- c4c168e: fix, tests, design.md and this note (signed).
- bd74e09: merge of origin/main cb681d3 (#159, estate corpus), with no
  overlap (signed).
- The follow-up commit records this receipt (signed). It is note-only, so
  the code tree is bd74e09's.

## Open

- **A workspace planned for a bare source.** It would restore an empty
  checkout: HEAD set, the index empty. Consider refusing at `estate-add`
  when the source is bare and a workspace is planned.
- **Linked worktrees inside a bare repository.** They are not named as
  nested custody by the bare capture, because its census is empty. Each is
  its own item, but an unplanned one is unnamed until #102/#103 coverage.
- **PR #151 merge** (WP3 PR 2, no blanket `From`). This diff keeps `?` on
  io errors in four places, which need `.refuse_at(site)` there:
  - `source_index`'s match arm;
  - `prepare_private`'s `DirBuilder` and alternates write;
  - `export_pass`'s index write.

  Expected conflicts: `source_index`, `prepare_private`, `commit_tree`
  (#151 reroutes `output`/`text`) and `shallow.rs`.
- **The staged-blob check moved.** With `--missing-ok`, the
  "staged blob exists" check moves from `write-tree` to bundle packing. In
  a chained pass, a staged blob that a prerequisite commit's tree also
  names is not re-checked; it is the source's own history.
- **Out of scope here.** Restore and import (destination stores) and the
  frozen `carry_v2` were not audited for freshening.
- **Not reconciled.** Distilled facts were not posted to Linear or #162 by
  this lane, and the Linear SSOT workstream ledger was not reconciled by
  this lane. Both are for the coordinator.

## Workstreams touched (reported, not verified here)

- **PR #151** (WP3 PR 2): reported as merging into main tonight; it was open
  at 4a10bb8 when this lane fetched.
- **PR #159** (estate corpus): found #162 with its carriability smoke.
- **This lane:** branch pushed, no PR. The next action is review and PR by
  the coordinator.
