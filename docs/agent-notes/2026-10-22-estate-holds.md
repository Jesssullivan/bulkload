# 2026-10-22: the estate pass's eight new hold kinds

Lane `feat/estate-holds-20261022` (from main e1d3905). The operator,
2026-10-22: "Hold all 22, run now (Recommended)". This follow-up handles
the kinds it can without writing to neo's repositories.

- **`GIT_NEST_CONVERSION_ATTRIBUTE`, `GIT_NEST_POINTER_STALE`, and likely
  `GIT_INVENTORY_INDEX_ABSENT` and `GIT_INVENTORY_MALFORMED`.** These came
  from the old capture census, which judges every nest (its attributes, its
  pointer, its index) and refuses a stray `.git`. The streamed capture now
  finds nests and rebuildable roots by one plain walk
  (`git_carry::streamed_omissions`). A directory holding a `.git` entry of
  any kind is its own item and is never entered or judged. Symlinks are not
  followed. The registered-worktree census is gone from this path too.
- **`GIT_SOURCE_ALTERNATES`.** The refusal is removed. The fetch by id is
  served from the source's alternates, so the live store holds every object
  itself. No alternates file is written at the destination.
- **`SOURCE_ROOT_UNREADABLE`.** It came from the plan's `negotiate`. It is
  now typed with its errno: `detail="SourceRootUnreadable(Some(N))"` on the
  item line. Any other status refusal carries its own code and detail.
- **`GIT_CHILD_FAILED` (3).** The source line now prints
  `detail=GitChildFailed(<stderr class>)`, so the next pass names the
  class. No fix without knowing it.
- **`DESTINATION_UNMAPPED` (1).** A temporary checkout under neo's
  `/private/var/folders/…/T/` is correctly held. No map places a temp
  directory.

Tests:
- `git_carry::tests::nests_are_found_by_a_plain_walk_and_never_judged`
- `git_carry::preview::tests::an_unreadable_worktree_is_typed_with_its_errno`
- `estate_move::alternates_are_carried_and_nests_never_refuse_the_parent`
