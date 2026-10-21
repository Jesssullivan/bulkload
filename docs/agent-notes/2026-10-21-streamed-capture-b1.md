# 2026-10-21: streamed capture B1 (objects by id, seats as files)

Lane: SPEC rule 2 reset (2026-10-20, ratified #298). Coordinator decisions:
- (a) seats land as files through the transfer engine;
- B1 uses Git's pack protocol inside bulkload, with per-batch resume.

Branch `feat/streamed-capture-b1-20261020`, stacked on PR A (#301).
Edited on sting. Built and tested on honey (`nice -n 19`, jobs 4, gate
devShell). R-N13.

## What B1 does

- **Source half** (`estate-move-source`). Per planned item, one ledger:
  `carry/ITEM.snapshot` (`estate::streamed::ItemSnapshot`). It holds:
  - the object ids to carry (`git_carry::stream::snapshot`): every ref,
    each worktree's HEAD and per-worktree refs, every reflog entry's old
    and new ids, the stash, notes and replace refs, operation files
    (`ORIG_HEAD`, a merge, a rebase, a sequencer) and each worktree's index
    blobs;
  - counts of ids the source no longer holds, and of unreachable objects
    (`fsck --unreachable --connectivity-only`), never carried;
  - the paths the seat transfer leaves out: `objects`, rebuildable
    directories and nested repositories;
  - the host-path files (config, `worktrees/*/gitdir`, a linked
    worktree's `.git`).

  A partial, shallow or alternates source is a typed hold
  (`GIT_SOURCE_PARTIAL_CLONE`, `GIT_HAVES_UNPROVABLE`,
  `GIT_SOURCE_ALTERNATES`). Nothing is bundled, copied or written to the
  source repository. A test checks its administration is byte-identical
  after both halves.
- **Destination half** (`estate-move`). The pull lands only the source
  half's ledgers; the corpus is excluded. For each item whose live path is
  absent, empty or its own earlier landing:
  1. its objects are fetched by id into the live object store, in parallel
     across items (`--jobs`). The source end is `bulkload-agent upload-pack
     REPO` (`git upload-pack` through the sanctioned builder, with
     `uploadpack.allowAnySHA1InWant` on the `-c` command line only).
     - It is reached locally, or relayed over ssh through the one
       registered ssh builder (`pull_command`, mode `UploadPack`).
     - Fetches run in a hand-written scratch bare repository, with objects
       written to the live store (`GIT_OBJECT_DIRECTORY`): no refs,
       no `FETCH_HEAD`.
     - Batches of 4096 ids; each finished batch is a resume mark,
       `private/ITEM.objects`.
  2. its seats (tracked, dirty, untracked, ignored, and the
     administration's refs, reflogs, HEAD, index, hooks and `info/`) land
     through the engine straight into the live path. The engine has a new
     per-destination excluded-paths list (`transfer::set_excluded`):
     excluded entries are `Skip`, never landed or read.
  3. the host-path files are written with every planned source path
     mapped to its live path.
  4. any object the landed administration names that the store lacks is
     fetched by id (a top-up).
  5. the proof: nothing the landed repository names is missing
     (otherwise `CAPTURE_DRIFTED`, the next pass completes it), and no
     quarantine is left in the store (otherwise `STATE_PAYLOAD_LEFT`).

  Each item's receipt line carries `object_bytes`, `fetch_ms` and
  `land_ms` (evidence for goal 2). The closure line carries
  `objects_unreachable_not_carried=` and `objects_missing_at_source=`.

  A live path holding another version is held:
  `GIT_DESTINATION_OCCUPIED`, typed, nothing written there. B2's
  consolidate path (#297 `union_of(Class)`) takes these over.
- **Closure, hand-off and decide** read the item snapshot where the corpus
  capture record was. The journal is bound to the snapshot's digest.

## Tests (honey, gate devShell)

fmt is clean, and all three clippy passes are clean with `-D warnings`.

- `git_carry::stream` and `estate::streamed` unit tests:
  - the snapshot wants every named id and counts the missing;
  - a fetch carries every want, with no ref, no `FETCH_HEAD` and no
    quarantine;
  - a mark skips a batch, and with the mark gone a rerun carries 0 bytes;
  - marks bound to another snapshot are not reused;
  - quarantine leftovers are named;
  - host-path mapping;
  - the plan's path map.
- `transfer::tests::an_excluded_subtree_is_never_landed_or_read`.
- `tests/estate_move.rs` (6 passed, 4 ignored):
  - a streamed pass lands every absent item: refs, stash, status, dirty,
    untracked and ignored bytes exact; the linked worktree linked; fsck
    clean; no custody refs; no payload in either state; the source's
    administration byte-identical. Its rerun fetches 0 batches and moves
    no ref.
  - a live path holding another version is held and untouched;
  - a shallow source is held typed;
  - the sessions-only homes pass, retired formats, stray payload.
- `tests/estate_move_source_roots.rs` (5): a remote pass over an ssh
  stand-in lands the item through the relayed `upload-pack`.
- `source_command_registry` (22) passes with the registry entries argued:
  - `upload-pack` added as a read;
  - `fetch` at 6;
  - `uploadpack.allowAnySHA1InWant` in `EXTRA_CONFIG`;
  - `pull_command`'s shape and reason updated.

## Open (B2 and after)

- Converge and consolidate a live path holding another version (#297).
- The index refresh a destination `git status` makes is seen as a
  destination change on the next pass (the tests read with
  `GIT_OPTIONAL_LOCKS=0`); B2's converge must treat a stat-only index
  refresh as unchanged.
- Nested repositories and submodules as their own items (they are
  excluded and noted).
- The four #280 `--sessions` tests still drive the old corpus pipeline
  (adopt and beside), so they stay ignored until B2.
- `darwin-touched`: yes (estate.rs, main.rs, transfer.rs). `just
  darwin-check` on pzm is owed before merge.
