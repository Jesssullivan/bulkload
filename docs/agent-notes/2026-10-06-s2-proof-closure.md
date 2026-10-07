# 2026-10-06 s2-proof-closure (completion-bar audit lane 7)

Rulings: OI-1003-Q5, OI-1003-Q9, OI-1003-Q16, OI-1003-Q36, OI-1003-Q60,
R-N13. Branch `feat/s2-proof-closure-20261006`, worktree
`bulkload.worktrees/s2-proof-closure-20261006`. Main `600c765` is merged in. PR #197 is open. Its CI failed as root on `b385f08`; the
second fix round (below, 2026-10-07, adding OI-1003-Q72 and OI-1003-Q76)
answers that.

**State: S2's lock and write properties are NOT closed.** Two property
tests exist and are red on mutation leg by leg. What they do not prove is
listed in the evidence doc, section 3, and under "Open" below.

## What was done

Four sessions worked this lane: a builder, a fixer that was cut off
mid-run, the session that finished the fix round, and a recheck stage. No production code,
`Cargo.toml`, `justfile`, `flake.*` or `.github/` file was changed by the
lane.

- **P76 SOURCE-COMMAND-REGISTRY**,
  `crates/bulkload-agent/tests/source_command_registry.rs`.
  - Round 1 scanned the agent crate only, compared sites as a set and
    checked only the body of `git_carry::git`. A review showed that a new
    bypass could pass all three.
  - The fix round rescans every crate under `crates/*/src`, counts each
    child, pins its text and its callers, freezes the id list at 17,
    refuses renamed `Command`s and raw spawns, and checks what callers do
    with the builder. Details are in the evidence doc, section 1.
- **P77 SOURCE-LOCK-TRACE**,
  `crates/bulkload-agent/tests/source_lock_trace.rs`. Linux only.
  - Round 1 had `copy`, a v1 export and a SQLite leg, and watched only the
    lock table and this process's descriptors.
  - The fix round adds an inotify write watch, which is the channel that
    sees Git's `*.lock` files. It adds legs for `serve` as its own process,
    `estate::capture`, a local `git-carry-estimate` and a SQLite backup
    under a committing writer. The SQLite exception is now exact, observed,
    bounded and released.
- The P76 and P77 rows in `docs/plans/2026-10-03-property-test-plan.md` and
  `docs/evidence/s2-proof-closure-2026-10-06.md` were rewritten to say what
  the tests prove now, and what they do not.
- [#188](https://github.com/Jesssullivan/bulkload/issues/188) holds the four
  agent bypass sites and, in a dated comment, the production work the fix
  round found. The `-wal` finding is on #157.

## Mutation testing

- Mutants are applied to a copy of the tree under
  `/srv/cache/jess/s2-proof-closure-mut`, never to the lane worktree. The
  interrupted fixer mutated the worktree in place and left a mutant in
  `provider_sqlite.rs`; the coordinator restored it. No mutant was ever
  staged or committed.
- 16 static mutants and 12 library mutants ran. The tables are in the
  evidence doc, section 2.
- Three results are weaker than "red" suggests, and the evidence doc says
  so:
  - stripping the optional-lock hardening alone (G1) leaves P77 green,
    because the v1 capture runs no index-refreshing subcommand on a source;
  - the estate leg goes red under G3 because the capture refuses, not
    because the leg's write assertion fires;
  - the journal-file mutant (S2) is red only because the backup fails.

## Recheck stage

- Merged main `48bd697` (#191 and #192) with a signed merge commit,
  `c8c1783`. #191 adds `git_carry/decide.rs` and changes
  `git_carry/shared.rs`, both inside the tree the registry pins. Both lane
  tests passed on the merge with no registry entry changed.
- Read the diff against main and ran 40 static mutants and two library
  mutants of its own, on a copy under `/srv/cache`. 37 static mutants were
  red, and every writer ceiling is tight. Three deliberate spellings pass
  the scan: a macro that takes the type as an argument, an ungated
  `#[path]` module that names a gated test file, and an `include!` of a
  file that is not `.rs`. They are low: none is in the tree, and each is
  plain in review. They are now listed in the evidence doc, section 3, and
  in the test's header.
- One test line changed: `the_bypass_list_only_shrinks` asserted that
  `FROZEN` holds exactly 17 ids, which would have failed the lane that
  removes a bypass, against the list's own comment. It now asserts at most
  17. Nothing else in either test changed.
- Disposition of the eight medium and high findings: five fixed (the scan,
  the identity of an entry, the registry's scope, monotonicity, the P77
  mutation evidence); three fixed as far as this lane can and the rest
  deferred to filed issues. The typed source/private builder is on #188,
  the `-wal` `O_RDWR` open is on #157, and the P77 verbs with no leg are
  item 5 of the #188 comment.

## Gotchas found on the way

- **`/proc/locks` is read one page per `read()`.** On a busy host a
  one-shot baseline can tear and drop a holder line. The sampler excludes
  the holder by the shape of its lines, not by a baseline.
- **libtest prints `test NAME ... ` before a `--nocapture` test's output.**
  The writer helper's ready marker therefore ends a line. The reader has a
  deadline.
- **Compare the database bytes while the writer is still open.** The
  writer's own close checkpoints the `-wal` into the database.
- **This host sets a cargo build directory apart from
  `CARGO_TARGET_DIR`.** Test binaries land under `/srv/cache/jess/cargo/build/`.
  Take the path from cargo's `Executable` line, not from the target
  directory.

## Second fix round, 2026-10-07 (root, the empty `-wal`, exact tables)

Rulings: OI-1003-Q76, OI-1003-Q72, OI-1003-Q5, OI-1003-Q9, OI-1003-Q16,
OI-1003-Q60, R-N13. No production code changed. Detail and receipts are in
section 5 of the evidence document.

- **Why.** PR CI failed on `b385f08` (run 37540759845): as root, SQLite
  re-applies ownership to the `-wal` it opens, and P77's write watch
  reported `attrib` on the source `-wal`. The operator ruled OI-1003-Q76
  (the provider refuses a source read as root; #196). Main had also moved
  to `a80c63b` (#189, #194, #195, #196, #198).
- **Merge.** `fdc5fac` merges main `a80c63b`. One conflict, the property
  plan; both sides' rows kept in order (P74, P75, P76, P77).
- **P77.** The SQLite legs print which half they run. As root: the five
  provider verbs refuse `SQLITE_SOURCE_AS_ROOT` on an idle-writer, a closed
  and a live-writer source, with no write event, no lock, and the directory
  byte- and metadata-identical. As a user: the Q16/Q36/Q72 exception, now
  with both counters, and a new closed-database leg for the empty `-wal`.
  `WAL_OPENED_READ_WRITE` became `RESIDUAL_WAL_OPEN_READ_WRITE`, documented
  against Q72's wording.
- **P76.** The id list was already exact and is unchanged (17). The other
  tables were upper bounds and passed with carry_v2's leftovers: three
  callers, three builders, four `-c` entries and five slack numbers. All
  removed or lowered; `stale_entries` now fails on any of them.
- **A flake found and fixed.** With the scratch on tmpfs the 2 MiB backup
  can fall between two `/proc/locks` samples, and the idle-writer leg
  failed "the sampler never saw the database READ lock" in one of the
  first two runs.
  It now repeats until one snapshot is seen (at most 40; each held to every
  limit); the live leg waits for a sampled lock too.
- **Main moved during the round** to `600c765` (#199, #164), merged
  again. #199 adds a fifth `fetch` (`chain::flatten`, each bound plan base
  into the chain's scratch repository). P76 failed on it (`5 uses, ceiling
  4`); the call site was read, the number raised to 5 with its argument in
  the entry, and the fact recorded on #188. It is the only number raised.
- **Mutants** (scratch copy, all red): S1 to S5 in the evidence document.
  S2 reproduces the CI failure under `unshare -r`. S5 passed before this
  round.
- **Root run recorded:** `unshare -r <source_lock_trace binary> --nocapture
  --test-threads=1`, 13 passed; the lines are in the evidence document.

## Open

- **The `O_RDWR` open of an existing `-wal` (#157).** A named residual
  (`RESIDUAL_WAL_OPEN_READ_WRITE`): Q16, Q36 and Q72 neither grant nor
  refuse it. The criterion "no write-mode open but the `-shm`" is not met.
  It needs a provider change or a ruling.
- **CI proves the refusal half only.** PR CI runs as root, so P77's SQLite
  legs assert `SQLITE_SOURCE_AS_ROOT` there and never run a backup. The
  Q16/Q36/Q72 exception is proved by local runs as a user. The local root
  run is `unshare -r`, a user-namespace root, not real root.
- **A typed source/private Git builder (#188).** The writer table is a
  census until it exists.
- **Two unhardened Git children in `bulkload-handoff` (#188).**
- **No P77 leg** for `estate::apply`, the estimate over ssh,
  `export_repository_with_policy`, the prerequisite and drift paths,
  `hydrate-state`, a real `pull`, or Darwin.
- **Cross-lane friction.** The registry pins text and counts in files other
  lanes own. A lane that adds a writer subcommand or a `-c` in the
  `git_carry` tree, or changes `main::pull_command`'s ssh arguments or a
  handoff Git probe, fails P76 until the registry is updated. Since the
  second fix round the tables are exact, so *removing* a writer use, a
  builder, a registered caller or an extra `-c` also fails P76 until its
  entry is lowered or deleted.
- #165 (the S2 budget run on neo), the load1-lag estimator ruling, FADV and
  the no-signals scan are untouched.

## Shas

- `f3df6dc`, `fd6595b`: round 1 (tests, plan rows, evidence, note).
- `205bee0`, `0a93a8a`: the fix round's test changes.
- `a7b7ccc`: merge of main `2247ab8`.
- `959ac6c`: the local `git-carry-estimate` leg.
- `ad80d52`: the evidence doc, the plan rows and this note after the fix
  round. `just check-fast` exited 0 on that head; the receipt is on #188.
- `c8c1783`: merge of main `48bd697`.
- `b385f08`: the recheck stage's edits. CI failed on it as root (run
  37540759845).
- `fdc5fac`: merge of main `a80c63b` (second fix round).
- `cd45b49`: the second fix round's tests, plan rows, evidence and this
  note. `just check-fast` exited 0 on that tree (second run; the first
  failed only on the known #200 `EAGAIN` flake).
- The merge of main `600c765` carries this line, the `fetch` number and
  its documentation. `just check-fast` ran on that tree before the commit;
  its receipt is in the pull request, because a commit cannot carry its own
  receipt.
