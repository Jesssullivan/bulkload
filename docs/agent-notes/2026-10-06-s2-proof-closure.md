# 2026-10-06 s2-proof-closure (completion-bar audit lane 7)

Rulings: OI-1003-Q5, OI-1003-Q9, OI-1003-Q16, OI-1003-Q36, OI-1003-Q60,
R-N13. Branch `feat/s2-proof-closure-20261006`, worktree
`bulkload.worktrees/s2-proof-closure-20261006`. Main `48bd697` (#191, #192)
is merged in. The recheck stage found no medium or high finding left and
opens the pull request; see "Recheck stage" below.

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

## Open

- **The `-wal` `O_RDWR` open (#157).** The criterion "no write-mode open
  but the `-shm`" is not met. It needs a provider fix or a Q36 amendment.
- **A typed source/private Git builder (#188).** The writer table is a
  census until it exists.
- **Two unhardened Git children in `bulkload-handoff` (#188).**
- **No P77 leg** for `estate::apply`, the estimate over ssh,
  `export_repository_with_policy`, the prerequisite and drift paths,
  `hydrate-state`, a real `pull`, or Darwin.
- **Cross-lane friction.** The registry pins text and counts in files other
  lanes own. A lane that adds a writer subcommand or a `-c` in the
  `git_carry` tree, or changes `main::pull_command`'s ssh arguments or a
  handoff Git probe, fails P76 until the registry is updated. Deleting
  `carry_v2` is tolerated: ceilings only shrink.
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
- The commit that carries this section holds the recheck stage's edits.
  `just check-fast` ran on that tree before the commit; its receipt is in
  the pull request, because a commit cannot carry its own receipt.
