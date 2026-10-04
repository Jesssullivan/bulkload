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
  - The state root is opened `O_DIRECTORY|O_NOFOLLOW` and, when absent,
    created with `io::sys::mkdirat`. A symlink, a non-directory, or
    group/other bits give `PATH_ESCAPES_ROOT`. The first version of this
    claimed the refusals were unchanged; that was wrong, and review round 1
    below corrects it and lists the remaining differences from origin/main.
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

## Review round 1 (three medium findings, all fixed)

Same lane, same day. Rulings: R-N58, OI-1003-Q37, OI-1003-Q38, R-N13.

- **The seal-before-marker order had no proof.** The schema transaction
  that commits `root_sealed` traced no `Event::Commit`, so a refactor that
  sealed after `COMMIT` kept every store proof green.
  - New `CommitRecord::RootSealed`. `Store::open` traces the marker's
    commit inside the `COMMIT` step, as it returns, under the trace's serial
    lock (taken before `BEGIN`, D5 order).
  - `store_kept` requires the state root and its database once the marker
    commit has returned, with its own message.
  - The positive proofs assert that both seals come before the marker. The
    pre-#161 model (`before_the_seal`) drops the marker as well as the
    seals.
  - New teeth proof `a_store_marker_committed_before_its_seal_can_lose_the_store`
    moves the seals after the marker in the trace. It reports violations
    at ops 3 and 4.
  - New unit test `a_store_whose_seal_fails_records_no_seal` uses
    `fail_dir_seals`: the open refuses `IO(EIO)` and leaves no marker, and
    the next open seals and records it.
  - Mutation run: seal moved after `COMMIT` in `Store::open`. Both positive
    store proofs and the unit test FAILED (`the state root is None after the
    root_sealed marker committed`). Restored afterwards.
- **Every open read-opened the parent (regression).** `StateRoot::open`
  first opens the root by path, `O_NOFOLLOW|O_DIRECTORY`, which needs only
  search permission on the parent. The parent is opened only to create the
  root (for `mkdirat`) or for a seal that is needed (`StateRoot::seal`). A
  parent opened late must still hold this root's inode under the root's
  name, or the open refuses `PATH_ESCAPES_ROOT`. A seal the agent cannot do
  because it cannot read the parent refuses `IO` (`EACCES`). That happens
  only when a seal is needed, not on every open, and no marker is recorded.
  Unit test
  `a_sealed_store_opens_under_a_parent_it_cannot_list` covers a 0311 parent
  with a sealed root, an unsealed root and a fresh name. The asserts that
  depend on permission bits are skipped for euid 0. A mutation that opens
  the parent eagerly fails it.
- **Trailing `/` or `/.` on a symlinked root (changed acceptance).** Option 1
  was taken: the acceptance origin/main had is kept. `resolve_leaf`
  canonicalizes a path ending in `/` or `/.` the same way it already
  canonicalizes `.` and `..` leaves. The store then lives in, and seals,
  the directory the link names. A trailing-slash name that does not exist
  yet is created as written. Unit test
  `a_state_root_named_with_a_trailing_slash_follows_its_leaf` covers
  `link/`, `link/.`, `link//`, `link/./`, a fresh `fresh/`, and a link to a
  0750 directory, which is still refused. A mutation without the
  trailing-slash case fails it. The bare `link` is still refused, as on
  main.
- Differences from origin/main that remain, all deliberate:
  - creating a root under a parent the agent can write and search but not
    read now refuses `IO` (`EACCES`), because the new root could never be
    sealed;
  - an existing root without its marker under such a parent refuses the
    same way until the parent is readable;
  - a dangling symlink named with a trailing slash refuses
    `PATH_ESCAPES_ROOT` instead of `IO` (`EEXIST`).
- `docs/design.md` Durability: added that the marker commits only after the
  seal, and the parent-access rule.
- Evidence: see the review round 1 entries under Shas.

## Shas

- Fix commit: f3d0449 (`fix(store): seal the state root and its parent before
  Store::open returns (#161)`). The first version of this note is in d649f7c.
- Review round 1: 217deca (`fix(store): prove the seal precedes root_sealed;
  open the parent only to seal (#161 review)`). This note's update is in the
  commit after it.
- Review round 1 check-fast (`flock … nix develop .#default --command just
  check-fast`, sting): exit 0. The run outlasted the harness's 600 s
  foreground limit, so the harness moved it to the background. It was waited
  on to completion, and nothing ran in parallel. Results:
  - workspace lib: 439 passed, 5 ignored;
  - `power_loss`: 9 passed (the 5 copy proofs and 4 store proofs, including
    the new marker-order teeth);
  - `fault_harness`: 56 passed;
  - `adoption_power_loss`: 2 passed;
  - contract tests: 22 OK.
- Store proofs after the fix:
  - fresh open plus commit: `crash_check events=6 ops=6 crash_points=7
    states=11 exhaustive=true violations=0`;
  - marker-first mutation: `events=5 … violations=3`;
  - pre-#161 model: `events=3 … violations=4`.

## Recheck and ship

Same lane, same day, recheck-and-ship stage. Rulings: R-N58,
OI-1003-Q37, OI-1003-Q38, R-N13.

- **What was checked.** The diff from d649f7c to e5216be, read against the
  three medium findings, plus the rest of the branch for any new defect.
  Verdict: CLEAN. No medium or high finding remains.
  - Seal before marker: fixed. The marker's `Event::Commit` is recorded
    once `COMMIT` returns, under the reentrant trace serial lock taken
    before `BEGIN`; `record` takes no lock of its own. `store_kept` reads
    the marker's commit from the trace indices in `StateInfo::commits`. The
    positive proof asserts both syncs fall in `opening[..marker]`, so a seal
    moved after `COMMIT` fails that proof deterministically. The trace-level
    teeth proof is independent of the code. `fail_dir_seals` is
    thread-local, so the unit test cannot disturb parallel tests.
  - Parent read-open: fixed. An existing root is opened by path, which
    needs only search permission on the parent. The parent is opened only
    in the create branch or in `StateRoot::seal`, and `open_parent` checks
    the parent's entry against the root's inode.
  - Trailing `/` or `/.`: fixed. `resolve_leaf` canonicalizes such paths.
    A name that does not exist yet keeps its leaf (Rust's
    `Path::file_name` drops the trailing `/` or `/.`). `link/..` and `.`
    resolve as they did before.
  - No new medium or high defect. `CommitRecord::RootSealed` is ignored by
    the other commit consumers: the copy invariant, which handles it
    explicitly, and `adoption_power_loss`, which has a `_` arm.
- **Small differences from main, rated below low, not filed:**
  - an existing root without its owner's read bit (a private 0300 root)
    now refuses `IO` (`EACCES`);
  - a non-directory earlier component now refuses `PATH_ESCAPES_ROOT`
    instead of `IO` (`ENOTDIR`).
- **Evidence.** check-fast (`flock … nix develop .#default --command just
  check-fast`, sting, head e5216be): exit 0. It outlasted the 600 s
  foreground limit, so the harness moved it to the background, where it was
  waited on. Results:
  - workspace lib: 439 passed, 5 ignored;
  - io-trace lib: 61 passed;
  - `fault_harness`: 56 passed;
  - `power_loss`: 9 passed, including all four store proofs;
  - `w3_engine`: 1 passed;
  - contract tests: 22 OK.
  The three new unit tests passed.
- The PR, ready for review and not merged, is recorded below once it opens.

## Open

- An operator rename or restore of a state dir is outside the engine's
  model. A marker that is already present is trusted, so that dir is not
  re-sealed.
- The stderr store's `key` file has a separate gap: a creator that died
  between create and write leaves a short key, which refuses later opens.
  Review round 1 points out that it is also a power-loss gap of the #161
  kind: a process that did not create the key never syncs the key file.
  This was not fixed (rated low).
- The estate shared-base hard link into the corpus was not audited for its
  corpus-directory seal.
- The state dir's own ancestors (operator paths) are not sealed.
- Review round 1 low findings, not fixed (by instruction), each still
  open:
  - On Darwin the parent's barrier must come before the root's full flush
    in `seal_state_root`, and nothing guards that order.
  - SQLite opens `transfer.sqlite` again by path, so a root swapped between
    `StateRoot::open` and `Connection::open` defeats the descriptor seal.
  - A foreign read-write connection (the sqlite3 CLI, a backup tool) deletes
    the WAL on close. The next WAL is then never sealed, because the marker
    is trusted.
  - The estate, journal and stderr seals have no proofs or `fail_dir_seals`
    pins.
  - The Darwin trace shape (Barrier, then FullFlush) is never exercised.
  - The four sealing sites use three different primitives. Estate
    full-flushes its parent on every call.
  - The low finding about the parent `O_RDONLY` open repeats medium finding
    2, and is fixed with it.
- A dedicated refusal code for "state root cannot be sealed" was not
  added. It would change the wire refusal taxonomy, and `IO` (`EACCES`) is
  what the open returns now.
- origin/main moved past this branch's base (#159, #160, #150; 8d1edd3 at
  recheck). A trial merge with `git merge-tree` has no conflicts. The
  branch was not merged.
- Linear: the distilled facts belong on the owning issue. Not posted from
  this lane.
