# 2026-10-06 s2-proof-closure (completion-bar audit lane 7)

Rulings: OI-1003-Q5, OI-1003-Q9, OI-1003-Q16, OI-1003-Q36, OI-1003-Q60,
R-N13. Branch `feat/s2-proof-closure-20261006`, worktree
`bulkload.worktrees/s2-proof-closure-20261006`, based on main `b6ecd50`.
No PR is opened, per the lane brief.

## What was done

- **P76 SOURCE-COMMAND-REGISTRY** is
  `crates/bulkload-agent/tests/source_command_registry.rs`.
  - A static scan registers every non-test `Command::new` in the agent. The
    sanctioned builder is `git_carry::git`, and four bypasses sit on an
    allowlist with a ceiling of 4 that may only shrink.
  - The contract of `git_carry::git` and `git_env` is checked statically.
  - No store-rewriting subcommand literal may appear, and no override of
    `GIT_OPTIONAL_LOCKS`.
  - A dynamic `git` wrapper checks every Git child of a real `git-export`
    and a real local `git-carry-estimate`.
- **P77 SOURCE-LOCK-TRACE** is
  `crates/bulkload-agent/tests/source_lock_trace.rs`. It is Linux only.
  - The `io-trace` recorder cannot see locks or opens, so P77 samples
    `/proc/locks` and `/proc/self/fdinfo` instead, with a sampler
    self-test.
  - Its legs are `copy` (a free run and a holder run), a v1
    `export_repository` with a holder, and a SQLite backup against a
    separate WAL writer process. The SQLite leg counts the Q16/Q36
    exception.
- The P76 and P77 rows were added to `docs/plans/2026-10-03-property-test-plan.md`.
  P73 to P75 were left for the concurrent lanes, per the brief.
- `docs/evidence/s2-proof-closure-2026-10-06.md` gives what is proved, the
  mutation table (M1 to M4) and what remains.
- Filed [#188](https://github.com/Jesssullivan/bulkload/issues/188) for the
  four bypass sites.
- Commented on #157 about a new finding: the SQLite backup holds the source
  `-wal` open `O_RDWR`. Its bytes are unchanged. The test counts it under
  `WAL_OPENED_READ_WRITE`.
- No production code, `Cargo.toml`, `justfile`, `flake.*` or `.github/`
  changed. Cargo auto-discovers both tests, so no `[[test]]` hunk was
  needed.

## Gotchas found on the way

- **`/proc/locks` is read one page per `read()`.** On a busy host, a
  one-shot baseline read can tear and drop a holder line, which then looks
  like a new lock in every later sample. The sampler therefore excludes the
  holder by the shape of its lines (granted `FLOCK` WRITE from this process,
  or granted `OFDLCK` WRITE, on a held inode), not by a baseline.
- **libtest prints `test NAME ... ` before a `--nocapture` test's output.**
  The writer helper's ready marker therefore ends a line rather than being
  one. The reader has a deadline, and closing the writer's stdin ends it.
  The first version waited forever. That background run stayed blocked on
  its own pipe, and it was not signalled (R-N11).
- **Compare the database bytes while the writer is still open.** The
  writer's own close checkpoints the `-wal` into the database.

## Open

- #165 (the S2 budget run on neo), the load1-lag estimator ruling, FADV,
  #188, the `-wal` open on #157, the no-signals scan, and the Darwin
  channel. Details are in the evidence doc, section 2.
- Shas: see the commit on the branch. The structured lane result carries
  the head sha.
