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
  - One predicate, `bare_root`, drives both the index read and the census
    (review round 1). A repository is bare-rooted only when Git says bare
    and its canonical path is `rev-parse --absolute-git-dir`, in one child.
  - `source_index` returns empty index bytes for an absent index of a
    bare-rooted repository.
  - `capture_census_planned` returns an empty census for a bare-rooted
    repository. A non-bare `.git` given as a root is unchanged.
  - `export_pass` writes no index file, so the staged and worktree trees
    are the empty tree.
  - Refs, HEAD, head-symbolic, exclude and configuration are carried as
    before.
  - The capture marks itself with `refs/carry-export/bare-repository-v1`,
    written only for a bare source. A shallow envelope lifts it into its
    headers as `shallow-bare-v1`, and `unpack` checks the lift against the
    inner inventory.
  - Apply of a no-workspace item imports them (`refs-imported`).
- **Refused, typed (review round 1).**
  - A bare git dir reached through a `.git` gitfile (the
    bare-plus-worktrees layout) refuses `GIT_REPOSITORY_NOT_AT_PATH`, as the
    estimate probe does. Before, it reported `captured` with an empty index
    and the `.bare` administration carried as worktree seats.
  - A non-bare repository with no index file (`clone --no-checkout`,
    `worktree add --no-checkout`) refuses the new
    `GIT_INVENTORY_INDEX_ABSENT`, not `Io(Some(2))`.
    - It is not carried as an empty index. Git reads an absent index as
      unborn, and a plain `git checkout` then populates the worktree.
    - With an empty index file, every HEAD path stays a staged deletion.
      This was probed on git 2.52.
  - Every verb that lays down a workspace, an index or a payload
    attachment refuses a bare capture with the new
    `GIT_BARE_CAPTURE_WORKSPACE`, before writing anything. The verbs are
    `restore_staged`, `restore_linked_staged`, `repair_missing_index`,
    `attach_payload` and `registered::restore`.
    - `StagedBundle` reads the marker from the headers `stage_bundle`
      already lists, and `chain::flatten` keeps it.
    - An estate item planned with a workspace for a bare source therefore
      refuses typed at apply. It leaves no partial destination, no
      imported ref and no journal.
    - `apply_item` checks this before any chain flatten or plan-base
      import (661928f). A bare hub and its linked worktree share a plan
      base. Without the early check, the base import's own guard refused
      `GIT_DESTINATION_OCCUPIED` first and masked the typed cause.
- **Census.** `filesystem_census` skips the common dir when it lies below
  the root (a gitfile into `--separate-git-dir`). The repository's own
  administration is never a seat.

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
  - Review round 1 added four S4 tests:
    - `s4_a_gitfile_naming_a_bare_git_dir_is_not_a_repository_root`:
      capture, export and estimate all refuse `GIT_REPOSITORY_NOT_AT_PATH`,
      while `.bare` and its linked worktree each capture.
    - `s4_a_separate_git_dir_below_the_root_is_not_a_seat`.
    - `s4_a_non_bare_repository_without_an_index_refuses_typed`: a
      no-checkout clone and a no-checkout linked worktree, whose indexes
      stay absent.
    - `s4_a_bare_capture_never_lays_down_a_workspace`: plain and shallow
      bare captures carry their marker, all five workspace verbs refuse
      with nothing written, and import still succeeds.
- **Estate tests.**
  - `estate::tests::a_bare_repository_item_captures_and_applies_as_ref_custody`
    checks for receipt `captured`, then `refs-imported`.
  - `estate::tests::a_bare_repository_item_with_a_workspace_refuses_typed_at_apply`
    covers a standalone and a linked workspace: each is `captured`, then
    `refused` with `GIT_BARE_CAPTURE_WORKSPACE`. No workspace exists after,
    and the repository holds no ref.
  - `estate::tests::a_bare_item_on_a_plan_base_refuses_its_workspace_before_the_base`
    covers a bare hub that shares a plan base with its linked worktree's
    item. It fails without the early check, with `GIT_DESTINATION_OCCUPIED`.
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
- 0ca1b68: this receipt (signed). It is note-only, so the code tree is
  bd74e09's.
- 0708629: merge of origin/main edf6120 (#160, TLA+ model) (signed).
  - It is docs plus a standalone `tla-check` recipe, with no Rust and no
    change to the check-fast tier.
  - The non-Rust check-fast gates were re-run green on it: manifest, Python,
    shell and workflow lint, the secrets scan and the CI contract.
- 8d7c9e5: note (signed).
- c7c2849: merge of origin/main 8d1edd3 (#150, WP3 PR 1, typed refusals),
  clean (signed).
  - `just check-fast` was re-run on it and exited 0 at 2026-10-04T10:38Z.
  - 26 cargo result lines: 691 passed, 0 failed, 8 ignored, with
    `refusal_taxonomy` included.
  - The fault harness, the power-loss and adoption proofs and the CI
    contract (22 OK) passed.
  - **This is the code tree that was pushed.**
- 915af22: note (signed). The first push ended here.
- **Review round 1** (2026-10-04):
  - ae9c569: signed merge of origin/main dfb9604 (#151, WP3 PR 2, no
    blanket `From`). It had three conflicts in `git_carry.rs`, each
    resolved to this branch's behaviour with a named site:
    - `export_pass`: `.refuse_at("git_carry::export_pass")`;
    - `source_index`: `crate::refuse::io(.., "git_carry::source_index")`;
    - `prepare_private`: the write store `DirBuilder` and the alternates
      write, both `.refuse_at("git_carry::prepare_private")`.

    `bare_root`, `metadata` and `commit_object` run through `text` and
    `input`, which on main are the classified `estimate::run_git`.
  - 1ab5454: the S4 review fixes and their tests (signed).
  - 661928f: refuses a bare capture's workspace in `apply_item` before
    the plan base (signed).
  - `just check-fast` exited 0 on 1ab5454's tree (2026-10-04T11:50Z).
  - 157f3b9: signed merge of origin/main 1f4c214 (#166 store-root seal,
    #167 coordinator notes), clean. `just check-fast` exited 0 on it
    (2026-10-04T13:49Z).
  - 78709af: signed merge of origin/main cd4ffad (#168, S2 budget
    instrument), clean. It adds bench Python, a justfile recipe and a
    lane note; no Rust.
  - **`just check-fast` receipt on 78709af** (in `nix develop .#default`,
    under the shared `.check-fast.lock`, nice 10):
    - Exit 0, 2026-10-04T18:36:29Z to 18:40:58Z.
    - 26 cargo result lines: 710 passed, 0 failed, 9 ignored. The lib
      suite alone is 453 passed.
    - All five `source_inert_tests::s4_*` tests, the three estate bare-item
      tests and `refusal_taxonomy::bare_io_none_sites_only_shrink` passed.
    - The fault harness, the power-loss and adoption proofs,
      `io-partial-write-alone` and the CI contract (22 OK) passed.
    - **This is the code tree pushed in review round 1.**
  - The last commit is note-only: it records the lines above.

## Open

- **Linked worktrees inside a bare repository.** They are not named as
  nested custody by the bare capture, because its census is empty. Each is
  its own item, but an unplanned one is unnamed until #102/#103 coverage.
- **The staged-blob check moved.** With `--missing-ok`, the
  "staged blob exists" check moves from `write-tree` to bundle packing. In
  a chained pass, a staged blob that a prerequisite commit's tree also
  names is not re-checked; it is the source's own history.
- **Out of scope here.** Restore and import (destination stores) and the
  frozen `carry_v2` were not audited for freshening.
- **Low review findings, not fixed** (the brief scoped them out):
  - A bare repository whose HEAD names an unborn branch refuses
    `GIT_INVENTORY_MALFORMED` from `read_authority`'s `rev-parse --verify
    HEAD`. That covers a `git init --bare` hub on `master` that was pushed
    only `main`, and an empty bare repository. The refusal is typed, but
    it names the wrong cause, and no test covers it.
  - The design text says a bare repository has no index. A bare git dir
    can hold one (the dotfiles `--git-dir`/`--work-tree` pattern). That
    index is carried as the staged tree, but such a capture still refuses
    a workspace restore.
  - With a valid cache-tree, `write-tree --missing-ok` writes nothing, so
    its root tree is checked only at bundle packing. This is benign and
    undocumented.
  - With an invalid cache-tree, `write-tree --missing-ok` in the write
    store writes every index tree as a loose object on each pass with a
    staged change. Attempt state directories are never removed, so their
    inode count grows. This was not measured on a large repository.
- **Not reconciled.** Distilled facts were not posted to Linear or #162 by
  this lane, and the Linear SSOT workstream ledger was not reconciled by
  this lane. Both are for the coordinator.

## Workstreams touched (reported, not verified here)

- **PR #151** (WP3 PR 2): merged into main as dfb9604. It was merged into
  this branch at ae9c569 (verified).
- **PR #159** (estate corpus): found #162 with its carriability smoke.
- **Main since review round 1** (verified with `gh pr list` at
  2026-10-04T18:35Z):
  - #166 (store-root seal) and #167 (coordinator notes) merged and are in
    this branch at 157f3b9.
  - #168 (S2 budget instrument) merged and is in this branch at 78709af.
  - #164 (whitepaper) is still open. It does not touch this branch.
- **Issue #162** is still open, with one comment.
- **This lane:** branch pushed, no PR. The next action is review and PR by
  the coordinator.
