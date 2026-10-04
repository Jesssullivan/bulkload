# 2026-10-04 — Seal the store's state root and its parent (lane fix-store-root-seal)

Lane: fix for [#161](https://github.com/Jesssullivan/bulkload/issues/161), host
sting. Worktree `bulkload.worktrees/fix-store-root-seal-20261004`, branch
`fix/store-root-seal-20261004`, cut from origin/main 4a10bb8. Pushed, no PR
opened (by instruction).

Rulings cited:
- R-N58 (R25): never re-read a byte the destination holds durably. A lost
  state root loses the durable rows and, on the source, the authority every
  row key carries.
- OI-1003-Q37, OI-1003-Q38: the dispatch rulings for this lane.
- R-N13: receipts cite rulings; this note.
- Context: the formal model's `StoreRootSealed` assumption and its
  `MC_store_root_unsealed` counterexample (PR #160, finding F1), and WP0(g)
  (OI-1003-Q20), for which this is a precondition.

## What changed

- **`Store::open` (source ledger store and destination store).**
  - `private_dir` now opens the parent, creates the root with
    `io::sys::mkdirat`, and opens the root `O_DIRECTORY|O_NOFOLLOW`. The same
    refusals as before apply: a symlink, a non-directory, or group/other bits
    give `PATH_ESCAPES_ROOT`.
  - `transfer.sqlite` is created with `io::sys::create_excl_at`. Both
    creations are now traced, so the R-N88 checker sees them.
  - New `io::durable::seal_state_root(parent, root)`: `seal_dir(parent)`,
    then a full flush of the root (`flush_dir`). The root's full flush drains
    the drive on Darwin, which takes the parent's barrier with it. A parent
    on another device is fully flushed too. Durable at return in either
    `--durability` mode, because the authority leaves the host at Start.
  - The seal runs before the schema transaction and outside SQLite's write
    lock (D5 lock order). The first read opens the WAL, so the seal covers
    the database and its WAL. The transaction then adds a `root_sealed`
    settings row.
  - A store without `root_sealed` is sealed again on open. That covers a new
    store, a creator that died before its seal, and a store from before #161.
    A sealed store reopens with no flush. Measured by `w3_engine`:
    `flush_dir` is 2 on a fresh copy (one per store) and 0 on the warm rerun.
- **Audit of the other private state directories.**
  - Ingest journal: its directories were already sealed. Same gap in another
    form: the journal file's entry was sealed only by its creator. `claim`
    now seals `ingest/` on every claim.
  - Quarantine: no gap. `quarantine(true)` seals the quarantine and
    `objects/`, and so does the resume path while no segment is journaled.
  - List store and Git receipts: no gap. Every persist syncs `lists`,
    `git-carry-v2` and the state dir; receipts sync their parent.
  - stderr store: gap. `stderr/` was never sealed in the state dir. It is
    now sealed when the store first opens it in a process.
  - Estate state and corpus (`estate::private_directory`): gap. Their
    entries were never sealed. The parent is now sealed after create or
    check.
- **Proofs.** `tests/power_loss.rs`, run by the `fault-harness` gate:
  - `every_power_loss_state_of_a_store_open_keeps_the_store`: checks
    `Store::open` plus one record commit, exhaustively, from the root's
    parent. In every crash state after the open returned (before Start) or
    after a commit returned, the root and its database are named. A
    reopen's trace is empty.
  - `a_store_open_without_its_seals_can_lose_the_store`: the teeth, a
    trace-level mutation. The same trace with the open's directory syncs
    cut out has violations.
  - `a_store_an_earlier_run_left_unsealed_is_sealed_by_the_next_open`: an
    earlier open with its syncs cut and its marker deleted, then a real
    open. Passes. With the second open's syncs cut too, it fails.
  - The copy proof's `foreign == 0` became `foreign ==` the 4 store creations
    (two roots, two databases). There are still no writes.
- **Docs.** `docs/design.md` Durability (two sentences); the crash_check
  rule 5 doc; the `power_loss.rs` module doc.

## Evidence

- Fixed: store proofs `crash_check events=5 ops=5 crash_points=6 states=10
  exhaustive=true violations=0`, for both the fresh open and the open after
  an unsealed earlier run.
- Unfixed (one run with the `seal_state_root` call removed from
  `Store::open`, tracing kept): both positive proofs FAILED with
  `events=3 ops=3 crash_points=4 states=9 violations=4`, for example
  `violation after op 2: the state root is None after Store::open returned
  (lost: #0 mkdir state; #1 create transfer.sqlite)` and `violation after op
  3: ... after a store commit returned`. The teeth test passes in both runs.
- check-fast (`flock … nix develop .#default --command just check-fast`,
  foreground, sting): exit 0. Highlights: workspace lib 436 passed; io-trace
  `io::` 61 passed; `power_loss` 8 passed (the 5 copy proofs and the 3 new
  store proofs); `resume-power-loss` 2 passed; `w3_engine` 1 passed; contract
  tests 22 OK.
- `w3_engine` fresh copy before the budget update: `flush_dir_count=2
  flush_dir_ns=42691006 full_flushes_total=9 durable_groups=5`. That is one
  state-root full flush per fresh store, about 21 ms each on sting.

## Shas

- Fix commit: f3d0449 (`fix(store): seal the state root and its parent before
  Store::open returns (#161)`). This note is in the commit after it.

## Open

- An operator rename or restore of a state dir is outside the engine's
  model. A marker that is already present is trusted, so that dir is not
  re-sealed.
- The stderr store's `key` file has a separate gap: a creator that died
  between create and write leaves a short key, which refuses later opens.
  This is a different gap from #161 and was not fixed.
- The estate shared-base hard link into the corpus was not audited for its
  corpus-directory seal.
- The state dir's own ancestors (operator paths) are not sealed.
- Linear: the distilled facts belong on the owning issue. Not posted from
  this lane.
