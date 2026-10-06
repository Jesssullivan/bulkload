# S2 proof closure, sting, 2026-10-06 (property tests, not the budget)

Rulings: OI-1003-Q5 and OI-1003-Q9 (S2: never interrupt the source; no
locks, no writes, no signals), OI-1003-Q16 (WP0(b) typed source access and
the bounded SQLite exception), OI-1003-Q36 (the `-shm` wal-index
exception), OI-1003-Q60 (completion-bar audit), R-N13.

**Scope.** This is lane 7 of the completion-bar audit. It adds two property
tests, P76 and P77 in the
[property-test plan](../plans/2026-10-03-property-test-plan.md). It is not
S2's measured budget: that is #165 and is still open (see below). No
production code was changed. Bypass sites are reported in
[#188](https://github.com/Jesssullivan/bulkload/issues/188), and the one new
SQLite finding is on
[#157](https://github.com/Jesssullivan/bulkload/issues/157#issuecomment-6021861734).

Measured on branch `feat/s2-proof-closure-20261006`, based on main
`b6ecd50`, on sting (Linux, xfs scratch). The host was heavily shared:
load1 was about 90 to 130 throughout. Tests were run with
`cargo test -p bulkload-agent --test source_command_registry` and
`--test source_lock_trace` inside `nix develop`, and the whole lane passed
`just check-fast`.

**check-fast receipt (commit `f3df6dc`).** It ran under the shared
`.check-fast.lock` in `nix develop .#default` and exited 0, with 31
`test result: ok` lines and 0 `FAILED`. Within it,
`source_command_registry` reported 8 passed and `source_lock_trace`
reported 6 passed.

## 1. What is now proved, and how strongly

### P76 SOURCE-COMMAND-REGISTRY (`tests/source_command_registry.rs`)

A source scan of `crates/bulkload-agent/src` removes comments, string and
char literals, `tests.rs` files, modules declared behind `cfg(test)` and
items behind `cfg(test)`. It then registers every `Command::new` that
remains. Each site is named `module::fn(program)`, never by line.

- **Six non-test child builders exist.** One is the sanctioned
  `git_carry::git("git")`. Four are allowlisted bypasses (bulkload#188):
  - `git_carry::estimate::local_probe("bash")`;
  - `git_carry::estimate::ssh_command("ssh")`;
  - `main::pull_command("ssh")`;
  - `provider_sqlite::hydrate::hydrate_one(program)`.

  The allowlist has a ceiling of 4, may only shrink, and fails the test
  when an entry goes stale.
- **`git_carry::git` holds the WP1 (#145) contract, checked statically.**
  - It uses `git_env::CLEARED`, `CONFIG` and `SET`, and adds
    `--no-optional-locks` and `GIT_CEILING_DIRECTORIES`.
  - `CONFIG` carries `core.hooksPath=/dev/null`, `core.fsmonitor=false`,
    `gc.auto=0` and `maintenance.auto=false`.
  - `SET` carries `GIT_OPTIONAL_LOCKS=0`, `GIT_NO_LAZY_FETCH=1`,
    `GIT_TERMINAL_PROMPT=0`, `GIT_CONFIG_NOSYSTEM=1`,
    `GIT_CONFIG_GLOBAL=/dev/null` and `GIT_NO_REPLACE_OBJECTS=1`.
- **No non-test literal names a store-rewriting subcommand.** The checked
  names are `gc`, `maintenance`, `repack`, `prune`, `prune-packed`,
  `commit-graph`, `multi-pack-index` and `pack-refs`. Only `git_env` names
  `GIT_OPTIONAL_LOCKS`.
- **Every Git child of a real run carries the contract, checked
  dynamically.** A `git` wrapper first on `PATH` recorded every Git child
  that a real `git-export` and a real local `git-carry-estimate` started.
  Each one carried the contract. `git version` is exempt from the `-c`
  flags but still carries the environment.
- **Limit.** The dynamic leg covers only the two verbs it runs. Children of
  other verbs (`estate-capture`, `estate-apply`, `restore`) are covered
  only by the static registry.

### P77 SOURCE-LOCK-TRACE (`tests/source_lock_trace.rs`, Linux only)

The `io-trace` recorder cannot answer the lock question. It records only the
agent's own mutating `io::sys` calls. It never sees an `flock`, an `fcntl`
lock, an open of any mode, a Git child, or SQLite's VFS. P77 therefore
watches the kernel instead, with no `LD_PRELOAD` and no new crate:

- `/proc/locks` is sampled during the verb. It lists `flock`, POSIX and OFD
  locks for every process, by device and inode.
- `/proc/self/fdinfo` is sampled at the same time, to read the access mode
  of each in-process descriptor.
- A **holder** keeps an exclusive `flock` and a whole-file OFD write lock on
  every source node, and the copy's tree is made read-only (the run was not
  as root).
- An **`lstat` census** of the source is compared before and after. It
  covers every field except atime.

| Leg | Result |
|---|---|
| Sampler self-test | A shared `flock` and a write-mode open, each held across one sample, are both seen. The holder's own lines and descriptors are not reported. |
| `copy`, free run | 0 lock lines on source inodes and 0 write-mode source descriptors over 265 to 320 samples. The census is unchanged and all 30 files are carried byte-equal. |
| `copy`, holder run | The copy neither waited (120 s deadline) nor refused. 0 non-holder lock lines, 0 write-mode opens, census unchanged, every byte carried. |
| `git_carry::export_repository`, holder run, all Git children | No process took or waited for a lock on any inode of the repository (worktree and `.git`). 0 write-mode opens in-process, census unchanged. |
| `provider_sqlite::snapshot`, WAL database held open by another writer process | This process's source locks were only READ on the db and locks on the `-shm`, as Q16/Q36 allow. Write-mode opens were on the `-shm` and also the **`-wal`** (see below). The db and `-wal` bytes were identical before and after. |

**Exception counts, from one run.** Each number counts samples that saw the
item, not calls: `db-read-lock=18 shm-lock=18 shm-write-open=19
wal-write-open=19`, out of 21 samples at a mean period of about 64 ms.
Other runs gave 32/32/31/31 of 34 and 51/51/51/51 of 53.

**Sampling period.** Under this load the mean period was about 6 to 107 ms
per sample, varying by run and leg. A free-run lock shorter than one period
can be missed. The holder run does not depend on the period: any blocking
lock attempt on a held node waits until the deadline.

**New finding (#157).** The SQLite backup's read-only connection holds the
source `-wal` open `O_RDWR`. Q16/Q36 name only a shared read lock on the
database and the `-shm`. The bytes did not change. The test counts the open
under `WAL_OPENED_READ_WRITE = true`, which cites #157. Any other
write-mode open still fails it. This lane does not own the provider, so
the fix or a Q36 amendment is left to #157.

### Mutation results (scratch edits, all reverted before commit)

| Mutant | Edit | Result |
|---|---|---|
| M1 | Inside `git_carry::partial_clone`, add `Command::new("git").arg("status")`, never spawned | **Red.** `every_child_goes_through_the_source_safe_builder` names `git_carry::partial_clone("git")` at `git_carry.rs:174`. |
| M2 | In `git_carry::git`, delete `command.arg("--no-optional-locks")` | **Red,** twice. `the_source_safe_builder_holds_the_s2_contract` fails ("lacks --no-optional-locks"), and `every_git_child_of_a_capture_and_an_estimate_is_hardened` fails (no leading `--no-optional-locks` on the export's children). |
| M3 | In `transfer::open_source`, take a blocking `flock(LOCK_SH)` on every source file opened | **Red,** twice. The free run sees `FLOCK READ` lines on source inodes. The holder run's copy waits until the 120 s deadline. |
| M4 | In `transfer::open_source`, take a non-blocking POSIX `F_SETLK` read lock and ignore failure | **Red** in the free run (`POSIX READ` lines, up to 58 samples per inode). **Green** in the holder run. This is the stated blind spot: under a holder the attempt fails, the failure is ignored, and nothing waits. |

## 2. What remains open

- **#165, the S2 measured budget (OI-1003-Q34).** This is the proof-grade
  run of the reference agent workload on neo: at most +25 % p95 and
  +2.0 load1, in a quiet window held by the coordinator. It has not run.
  This lane did not run it, by its brief. Until it runs, S2's budget half
  is unmeasured, and these property tests do not stand in for it.
- **The load1-lag estimator ruling.** `s2_budget.py` decides d_load1 from a
  lag-corrected level: the mean of the settled rows plus tau times their
  slope (`docs/agent-notes/2026-10-04-s2-budget.md`, 7ad8861). The lane
  brief reports that this estimator is awaiting an operator ruling. This
  lane did not verify that state, and its tests do not touch the
  estimator.
- **FADV.** Architecture-review WP1 PR 5 called for `FADV_DONTNEED` /
  `F_NOCACHE` after each consumed range on capture descriptors. That
  limits source page-cache pressure, which bears on the budget and not on
  the lock and write properties. It was scoped out of WP1, and no code does
  it.
- **The four bypass sites (#188).** The remote estimate probe runs at the
  remote host's default priority. `local_probe` hand-copies a subset of
  `git_env::CLEARED`. Neither a hydrate decompressor nor the pull ssh is a
  typed SourceAccess kind.
- **The `-wal` `O_RDWR` open (#157).** Described above.
- **No-signals half of S2.** These tests do not scan for signal calls.
  Under R-N92, lanes keep process-control words out of their commands, so
  a text scan for them was not written. It stays covered by review and the
  repo's pre-commit process-safety audit, not by a property test.
- **Darwin.** P77 compiles to nothing on Darwin, which has neither
  `/proc/locks` nor `fdinfo`. On neo the lock properties need another
  channel, such as `lsof`-free `proc_pidinfo` or `fs_usage` under the
  gated run.
- **What P77 cannot see.**
  - A non-blocking lock attempt whose failure is ignored, made under the
    holder, and missed by sampling in the free run (M4).
  - Write-mode opens by Git children. Their descriptors are in their own
    `/proc/<pid>`, which this lane does not read, so they are left to the
    `lstat` census.
