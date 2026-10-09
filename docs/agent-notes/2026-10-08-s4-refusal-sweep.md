# 2026-10-08 S4 refusal sweep

Lane: S4 refusal sweep, for #219, #220, #162, #181, #183 and #126. Rulings
cited: OI-1003-Q1 (S4), OI-1003-Q5 and OI-1003-Q16 (S2), OI-1003-Q75 (a
retired code fails closed), R-N121 (no stderr carried), R-N13 (this note).

- Branch `fix/s4-refusal-sweep-20261008`, worktree
  `bulkload.worktrees/s4-sweep-20261008`, based on #221's head `d6a8b86`.
- Changes are staged, not committed, pushed or opened as a PR. That is left
  to the coordinator.
- Edits stay at refusal sites and on the capture side. The estate-converge
  lane is rewriting `restore_staged`, `restore_linked_staged` and the refs
  restore, so this lane did not touch them. Its edits in that area are
  guards and refusal mapping:
  - `import_verified` and `import_base`: `git_carry::destination_repository`
    refuses a missing or non-repository destination before `bundle verify`.
  - `apply_item`: the no-record refusal is now `CAPTURE_ABSENT`.
  - `apply`'s grouping loop: a destination `common_repository` cannot read
    keys by its own path. The estate-converge worktree makes the same
    change (`unwrap_or_else(|_| item.repository.clone())`) inside a
    rewritten condition, so the hunk will conflict textually; take theirs.
  - `apply`'s and `capture_in`'s operation closures map refusals
    (`charged_space`, `capture_refusal`).

## Taxonomy changes (closed set)

Three new codes. `CODES`, the unit-test list, the `refusal_taxonomy` scan,
`docs/design.md` and the `docs/slo.md` amendment of 2026-10-08 are updated
together. No code left the taxonomy, so no recorded refusal becomes
`refusal-code-retired`.

- `CAPTURE_ABSENT`: estate-apply meets an item the corpus holds no capture
  record for.
- `GIT_SOURCE_ALTERNATES(path)`: alternates a capture cannot follow.
- `SPACE_EXHAUSTED(Option<path>)`: `ENOSPC` or `EDQUOT` with no preflight.

`StderrClass` gains `no_space` ("no space left on device", "disk quota
exceeded"). A v1 child of that class refuses `SPACE_EXHAUSTED`, with no
path.

The `Io(None)` allowlist goes down by one: `git_carry.rs` drops from 1 to 0.

## Per issue

Every test was green after its fix. Each mutation was applied to a scratch
copy of one file, built, run against its test, and then reverted; the
mutation runs are under "Mutations" below.

### #219: never-captured apply, and alternates

- **Cause 1.** The corpus had no capture record because the capture refused
  `GIT_SOURCE_PARTIAL_CLONE`. `apply_item` refused that missing record with
  `SEALED_OBJECT_MISSING`, which names a lost sealed object.
- **Fix 1.** `apply_item` now refuses `CAPTURE_ABSENT`.
  - It does not carry the capture's own code forward. Capture and apply
    keep their outcome records in different PRIVATE_STATE directories (often
    on different hosts), so apply cannot read it.
  - The capture state's outcome record still names the code.
  - Carrying it forward would need a new corpus sidecar. That is open.
- **Cause 2.** Nothing in the code named alternates. In practice they are
  followed. The private repository borrows the source's store, and Git
  reads that store's alternates recursively. The bundle's pack-objects (no
  `--local`) packs every reachable object wherever it is stored.
- **What actually failed.** The depth limit. Git reads alternates files at
  most six levels down, and the private repository adds one level. The
  promisor walk refused a deeper chain, and a quoted entry, as
  `GIT_INVENTORY_MALFORMED`.
- **Fix 2.** `promisor_packs` became `alternates_walk`, which runs at
  capture before any export. It walks as Git 2.52 links stores: depth
  first, in file order, each store once by its real path (Git 2.52 prints
  "unable to normalize alternate object path" for a missing one, so it uses
  realpath). A self-reference, a cycle or a diamond costs no depth. An entry
  five stores down naming a store not yet linked, and a quoted entry, refuse
  `GIT_SOURCE_ALTERNATES` naming the file. Comment-only files are accepted.
  - The estimate probe (`PROBE_SCRIPT`) walks the same way (`cd -P`/`pwd -P`
    for the real path) and exits 6 (`PROBE_ALTERNATES`) with an
    `alternates <file>` line, which `run_probe` maps to the same refusal.
    Its other non-zero exits go through `v1_child_refusal`.
  - An entry naming a store Git cannot open (a lender moved after
    `clone --shared`) is skipped and recorded. A capture that then fails
    `GIT_CHILD_FAILED` of class `bad_object` or `other` refuses
    `GIT_SOURCE_ALTERNATES` naming that file (`capture_refusal` and
    `missing_alternate`). The real Git failure is class `other`, not
    `bad_object` as review assumed. A stale entry nobody needs still
    captures.
  - The drift check (`pack_listing`) hashes every followed store's `pack/`
    listing, so a lender's repack under the pass is drift custody.
- **Tests.** In `estate::s4_sweep_tests`:
  - `apply_of_a_never_captured_item_refuses_capture_absent`.
  - `alternates_are_followed_and_the_landing_borrows_nothing`. Objects wholly
    borrowed, partly borrowed, through two levels and through a relative
    entry, landed as a refs import and as a standalone checkout. Each landing
    passes `fsck --full --strict` and has no `objects/info/alternates`.
  - `an_alternates_chain_deeper_than_capture_reads_refuses_typed`. A source
    six stores down refuses and records no capture. The boundary source, five
    stores down, captures and lands clean.
  - `self_cyclic_and_diamond_alternates_capture_as_git_reads_them`,
    `a_missing_lender_refuses_naming_its_alternates_file`,
    `a_lender_rewrite_under_the_pass_is_drift_custody`, and
    `estimate::tests::the_probe_and_capture_walk_alternates_alike` (a
    parity test over six sources).

### #220: a full TMPDIR refused as a bare IO

- **Cause.** `source_index` copies the index bytes into
  `PrivateDir::create(None)`, which is under TMPDIR. On a full TMPDIR the
  mkdir failed. `PrivateDir::create` dropped the error and refused
  `Io(None)`, which printed as reason `IO` with neither an errno nor a path.
  Every non-bare item reads its index, which matches the 35 items in the
  yoga run.
- **Fix.** `refuse::io_in(error, dir, site)` maps `ENOSPC` and `EDQUOT` to
  `SPACE_EXHAUSTED path=<dir>` and any other error to `IO (errno N)`. It is
  used in four places:
  - `PrivateDir::create`, which reports a parent that ran out of space even
    when a later parent fails otherwise, else the last parent's error;
  - the scratch index write in `source_index` (names TMPDIR);
  - the bundle stage create and write in `copy_hashing` (name the directory
    the stage's private directory was made in, which outlives it).

  A v1 Git child whose stderr is classed `no_space` refuses
  `SPACE_EXHAUSTED` with no path; its verb then names it. estate-apply lifts
  it to `DESTINATION_SPACE_INSUFFICIENT` (`charged_space`: its children
  write into the preflight-charged destination and corpus stage);
  estate-capture names PRIVATE_STATE (`capture_refusal`: its children write
  only under attempt and plan-base directories there). The estimate probe
  keeps it unnamed.
- **Tests.**
  - `a_full_tmpdir_refuses_space_exhausted_naming_it` is a fault-injection
    test. A cfg(test) thread-local, `git_carry::scratch_fault`, is armed from
    the `IndexRead` mid-pass hook and fails the next private-directory mkdir
    with `ENOSPC`. The test checks the receipt, that no capture record
    exists, and that the durable outcome record is typed. The fault now has
    four sites (`Mkdir`, `IndexWrite`, `StageCreate`, `StageWrite`), each
    arming failing one write, in order.
  - `a_full_tmpdir_at_the_index_copy_refuses_space_exhausted_naming_it`,
    `a_full_stage_names_the_directory_that_outlives_it`,
    `a_child_out_of_space_is_named_by_its_verb`.
  - `refuse::tests::a_full_disk_names_its_directory`.
  - `estimate::tests::a_child_out_of_space_refuses_space_exhausted`.
  - The proto tests for the class and the path display.
- **Not done.**
  - A bare `IO` carries its errno but not a path. That needs a payload on
    the `IO` variant, a workspace-wide change.
  - No preflight charges TMPDIR (#134).

### #162: bare repositories

- **Already fixed.**
  - The S2 freshening, by #172 (merged 2026-10-04): the write store and
    P34 in `git_carry::source_inert_tests`.
  - The bare `IO` (errno 2), by the bare-capture work that is already on
    main.
- **Cause.** `read_authority` ran `git rev-parse --verify HEAD`, which fails
  when HEAD is unborn.
- **Fix.** `carried_head` treats a HEAD that does not resolve (exit 1) in a
  bare root with a symbolic HEAD as unborn. The carried head is then `""`
  and no `refs/carry-export/head` is exported. Any other unresolved HEAD,
  including an unborn HEAD in a non-bare repository, still refuses
  `GIT_CHILD_FAILED`
  (`a_non_bare_repository_with_an_unborn_head_still_refuses`).
- **Test.** `a_bare_repository_with_an_unborn_head_captures_refs_only` uses
  two sources: an unborn HEAD with one other branch, and an empty bare
  repository. Both capture, keep the source's stat census unchanged at
  nanosecond resolution (S2), import their refs with no `/head`, and land
  fsck clean.

### #181: a missing plan base refused as a bare IO

- Fixed on main by L6b (#199, commit `0ec447f`): `estate::stage_base`. Not
  re-fixed here.
- Re-verified: `l6b_grouped_chain::a_missing_plan_base_refuses_apply_by_name`
  passes, and the mutation below fails it.
- The issue can close; the coordinator decides.

### #183: misattributed `GIT_INVENTORY_MALFORMED`

- **Item 2** (a relative corpus path) was fixed by L6b (#199).
  `a_chain_in_a_relative_corpus_path_flattens` passes.
- **Item 1, cause.** A refs import into a missing destination repository
  reached `bundle verify` in that missing directory. `verify_bundle` reads
  any failure as `GIT_INVENTORY_MALFORMED`.
- **Item 1, fix.**
  - `git_carry::destination_repository` refuses `GIT_REPOSITORY_NOT_AT_PATH`
    for a missing path, a non-directory, or a directory where
    `rev-parse --absolute-git-dir` (ceiling at the parent) answers
    `not_a_repository`. `try_exists` and `metadata` go through `refuse_at`,
    so `EACCES` or `ELOOP` stays `IO` with its errno. Both
    `import_verified` and `import_base` call it.
  - On a plan base, `import_base` refused the missing case
    `GIT_DESTINATION_OCCUPIED`. It now refuses `GIT_REPOSITORY_NOT_AT_PATH`.
    A standalone workspace keeps `GIT_DESTINATION_OCCUPIED`.
  - `apply`'s grouping loop no longer propagates `common_repository`'s
    refusal for an existing destination, which aborted the whole apply with
    no item records (R33).
- **Tests.** `a_refs_import_into_a_missing_repository_refuses_by_name`
  (missing, empty, and partly cleaned destinations, one row each) and
  `a_plan_base_import_into_a_missing_repository_refuses_by_name` (a
  grouped capture on a plan base; and a standalone on the base still
  `GIT_DESTINATION_OCCUPIED`). Nothing is created at the destination.

### #126: `SQLITE_FULL` refused as a bare IO

- **Cause.** `transfer_store::sqlite_error` mapped `SQLITE_FULL` to
  `Io(Some(ENOSPC))`. Only `materialize::space_refusal` lifted that to a
  typed code, so `Store::open` and the source ledger refused a bare `IO`.
- **Fix.** `sqlite_refusal` returns `SPACE_EXHAUSTED` with no path for
  `SQLITE_FULL`, and for `SQLITE_IOERR_*` whose errno is `ENOSPC` or
  `EDQUOT`. The errno is read with `sqlite3_system_errno` at the `COMMIT`
  sites (`sqlite_failed`), before any other call on the connection. Any
  other I/O error is `IO` with its errno (`EIO` when the site cannot read
  it), never `SQLITE_INTEGRITY_CHECK_FAILED`. Then, by side:
  - `materialize::space_refusal` lifts the unnamed refusal to
    `DESTINATION_SPACE_INSUFFICIENT` for destination group commits, which
    the transfer's preflight charges;
  - `Store::open` and `LedgerSink::publish` name the state directory
    (`named_space`), so a full source ledger never reports the
    destination's space.
- **Tests.** `transfer_store::tests::a_full_store_refuses_the_typed_space_refusal`
  produces a real `SQLITE_FULL` (`PRAGMA max_page_count = 2`);
  `a_full_source_ledger_names_its_state_directory` fills a real source
  ledger under `LedgerSync::Full`;
  `an_io_error_is_never_the_store_s_corruption` pins the errno mapping;
  `materialize::tests::a_full_destination_store_is_the_destination_s_space`.

## Mutations

Each mutation was run with `cargo test -p bulkload-agent --lib <test>`. Every
one failed as shown, and every file was then restored.

| # | Issue | Mutation | Result |
|---|---|---|---|
| M1 | #219 | `apply_item` refuses `SEALED_OBJECT_MISSING` again | the never-captured test fails at the apply assertion |
| M2 | #219 | the alternates depth bound raised from 5 to 6 | the deep test fails: the deep source is no longer refused typed |
| M3 | #219 | the bound lowered from 5 to 4 | the deep test fails: the boundary source is refused |
| M4 | #219 | `GIT_INVENTORY_MALFORMED` again | the deep test fails: left `GIT_INVENTORY_MALFORMED`, right `GIT_SOURCE_ALTERNATES path=".../level-0.git/objects/info/alternates"` |
| M5 | #219 | every alternates entry refused (the refuse-only design) | the followed test fails at capture |
| M6 | #162 | `rev-parse --verify HEAD` again | the unborn test fails at capture |
| M7 | #220 | `PrivateDir::create` returns `Io(None)` again | left `IO`, which is the field symptom, right `SPACE_EXHAUSTED path="<TMPDIR>"` |
| M8 | #220 | `io_in` without its `ENOSPC`/`EDQUOT` arm | two tests fail, showing `IO (errno 28)` and `Io(Some(28))` |
| M9 | #183 | `import_verified` without the missing-repository guard | left `GIT_INVENTORY_MALFORMED` |
| M10 | #126 | `sqlite_error` returns `Io(Some(ENOSPC))` again | left `Io(Some(28))` |
| M11 | #220 | a no-space child refuses `GIT_CHILD_FAILED` again | left `GitChildFailed(NoSpace)` |
| M12 | #181 | `stage_base` without its existence check and `sealed` | left `IO (errno 2)` ×2; `a_chain_in_a_relative_corpus_path_flattens` still passes |

## Review round 1 (2026-10-08)

Sixteen reviewer findings on the staged sweep. Each was checked. Fourteen
were real and are fixed with a test. Two pinned behaviour that was
already correct but had no test (#183 `import_base`, #162 non-bare); those
got tests that fail under the mutation the reviewer named. Duplicates:
findings 1 and 14 (#183 non-repository), 3 and 16 (#126 side), 4 and 13
(the probe), 6 and 10 (cycles).

- **Failing first.** The new tests that compile against the staged
  (pre-review) tree were run against it, in a copy of the index
  (`git checkout-index`), with its own target directory. Results:
  - a refs import into an existing non-repository: left `[]`, so the whole
    apply aborted with no item record (R33);
  - the probe walk: left `Err(GitChildFailed(Other))`;
  - the self/cycle/diamond test: capture refused `GIT_SOURCE_ALTERNATES`;
  - the missing lender: left `GIT_CHILD_FAILED stderr_class=other`;
  - the lender rewrite: left `GIT_CHILD_FAILED stderr_class=bad_object`;
  - the full source ledger: left `Err(DestinationSpaceInsufficient)`;
  - `a_full_store_refuses_the_typed_space_refusal`: left
    `DestinationSpaceInsufficient`;
  - the materialize lift: left `SpaceExhausted(None)`.

  The plan-base test and the non-bare test pass there, as expected for
  pins. Tests that use the new seams (`scratch_fault::Site`,
  `sqlite_refusal`, `charged_space`) are proved by mutation instead.
- **Mutations (review round).** Each was applied to the working tree, run
  against its test, then restored:

| # | Mutation | Result |
|---|---|---|
| R1 | `destination_repository` skips its `rev-parse` check (a directory check only) | left `NOT_AT_PATH, GIT_INVENTORY_MALFORMED ×2` |
| R2 | `import_base`'s old guard (missing refuses `GIT_DESTINATION_OCCUPIED`) | left `GIT_DESTINATION_OCCUPIED ×2` |
| R3 | `carried_head` without `bare_root` | the non-bare unborn test fails (it captures) |
| R4 | the index copy through `refuse_at` | left `IO (errno 122)` |
| R5 | the stage names `destination.parent()` (the removed private directory) | left the private directory's path |
| R6 | the stage create through `refuse_at` | left `Io(Some(28))` |
| R7 | `PrivateDir::create`: the last parent's refusal wins | left `Io(Some(13))` |
| R8 | apply passes a child's unnamed space refusal through | left `SPACE_EXHAUSTED` |
| R9 | capture passes it through | left `SPACE_EXHAUSTED` |
| R10 | capture without the missing-alternate attribution | left `GIT_CHILD_FAILED stderr_class=other` |
| R11 | an unknown-errno IOERR is the integrity code again | left `SqliteIntegrityCheckFailed` |
| R12 | the walk without its linked set | the cycle test fails at capture |
| R13 | the drift listing of the source's own store only | the lender test fails (refused) |
| R14 | the source ledger does not name its root | left `Err(SpaceExhausted(None))` |
| R15 | `space_refusal` without the unnamed store refusal | left `SpaceExhausted(None)` |

  In the first run, three mutants survived: R4, R6 and R8. The fault
  seams mapped the injected error themselves, and the verb test called
  `charged_space` directly. Both were reshaped: the injected and the real
  error now share one `map_err`, and a `Site::Child` fault fails the next
  v1 Git child as out of space, so both verbs run end to end. All three
  are now killed.
- **Residual.**
  - The `sqlite3_system_errno` read is not exercised by a real `EDQUOT`.
    Only the pure mapping is pinned.
  - Reads only at `COMMIT` sites. Other store calls map an `IOERR` to
    `IO (errno EIO)`.
  - `EACCES` on a destination path now refuses `IO` with its errno. No
    test pins it.

## Validation

Run in `nix develop .#default`, with `CARGO_TARGET_DIR` at
`/srv/fast-local/jess/cache/cargo-target/s4sweep`. The host load average
was 240 to 360 throughout. These are the results after review round 1.

- `cargo fmt --all --check`: OK (`FMT=0`).
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: exit 0.
- The same with `--features bulkload-agent/fault-injection,bulkload-agent/io-trace`:
  exit 0.
- `cargo test -p bulkload-agent -p bulkload-proto --locked`: exit 0.
  - The agent lib: 581 passed, 5 ignored.
  - `refusal_taxonomy`: 5 passed.
  - The proto lib: 16 passed.
  - Every integration binary is green.
    `git_grouped_chain` took 1680 s under the load.
- After two clippy-only edits (a doc paragraph split and a `map_or`), the
  touched tests were run again:
  - the agent lib filter (the sweep, probe, store and space tests): 21
    passed;
  - `refusal_taxonomy`: 5 passed;
  - the proto lib: 16 passed.
- Not run here: `just fault-harness` and `just check-fast`.

## Open

- Carrying a capture's refusal code into apply needs a corpus sidecar. For
  now apply refuses `CAPTURE_ABSENT`.
- A bare `IO` does not carry its path.
- An unborn HEAD in a non-bare repository still refuses `GIT_CHILD_FAILED`
  (by design; now pinned).
- TMPDIR is not in the capture space preflight (#134).
- `docs/formal/GitCarry.tla` line 304 still comments a capture-less apply
  as `SEALED_OBJECT_MISSING`. Formal files were left to the formal lane.
- Issues #219, #220, #162, #181, #183 and #126 need comments, and the
  Linear SSOT ledger needs updating, once this lands.
