# S2 proof closure, sting, 2026-10-06 (property tests, not the budget)

Rulings: OI-1003-Q5 and OI-1003-Q9 (S2: never interrupt the source; no
locks, no writes, no signals), OI-1003-Q16 (WP0(b) typed source access and
the bounded SQLite exception), OI-1003-Q36 (the `-shm` wal-index
exception), OI-1003-Q72 (the empty `-wal`), OI-1003-Q76 (no SQLite source
read as root), OI-1003-Q60 (completion-bar audit), R-N13.

**Scope.** This is lane 7 of the completion-bar audit. It adds two property
tests, P76 and P77 in the
[property-test plan](../plans/2026-10-03-property-test-plan.md). It is not
S2's measured budget: that is #165 and is still open. No production code
was changed. Bypass sites and the production work the tests point at are on
[#188](https://github.com/Jesssullivan/bulkload/issues/188). The SQLite
`-wal` finding is on
[#157](https://github.com/Jesssullivan/bulkload/issues/157#issuecomment-6021861734).

**This document was rewritten after a review round.** The first version
claimed more than the tests proved. Section 1 says what each test proves
now, section 2 gives the mutation evidence leg by leg, and section 3 lists
what is **not** proved. S2's lock and write properties are not closed: see
section 3.

Measured on branch `feat/s2-proof-closure-20261006` on sting (Linux, xfs
scratch), with main `2247ab8` merged in (`a7b7ccc`). The recheck stage then
merged main `48bd697` (#191, #192) as `c8c1783` and reran both tests there;
see sections 2 and 4. A second fix round on 2026-10-07 merged main `a80c63b`
(#189, #194, #195, #196, #198) as `fdc5fac`, after PR CI failed as root; it
is section 5, and sections 1, 3 and 4 are updated to match. The host was
heavily shared. Tests ran inside `nix develop` with
`cargo test -p bulkload-agent --test source_command_registry --test
source_lock_trace -- --test-threads=1`.

## 1. What is proved, and how strongly

### P76 SOURCE-COMMAND-REGISTRY (`tests/source_command_registry.rs`)

**The scan.** It reads every `crates/*/src` tree as text: the agent, the
bench and the handoff tool. It drops comments, literals, items behind
`#[cfg(test)]`, and files that a parent declares as a module behind that
attribute. A file is never dropped for being named `tests.rs`. A
`#[cfg(test)]` on an enum variant, a field, a statement or a parameter
hides only that thing. The scan then finds every `Command` token followed
by `::new`, under any path.

**What the scan refuses outright**, because it could not name the child:

- a renamed `Command` (`use .. as`, a `type` alias, an `impl .. for
  Command`);
- a child started without `Command`: `system`, `popen`, `fork`, `vfork`,
  the `exec*` family, `posix_spawn*`, `clone`, the raw syscall numbers,
  `CommandExt`, and `link_name`.

**The registry.** There are 19 non-test children in the workspace:

| Crate | Children | Standing |
|---|---|---|
| agent | `git_carry::git("git")` | The one sanctioned builder. |
| agent | `git_carry::estimate::local_probe("bash")`, `git_carry::estimate::ssh_command("ssh")`, `main::pull_command("ssh")`, `provider_sqlite::hydrate::hydrate_one(program)` | Four bypasses, filed on #188. |
| bench | `read("pmset")`, `rclone_copy(binary)`, `rclone_version(binary)` | Operator tool. Argued in the registry as never aimed at an estate source by the agent. |
| handoff | ten functions, eleven children (the gpg probe builds two). Two are Git: `signing_key` (`config --get`) and `ls_remote` | Credential probes. The two Git children are **not** hardened; filed on #188. |

- A site is named `crate::module[::inline module]::function(program)`.
- Each id is **counted**. Every id occurs once, and the gpg probe twice. A
  second child inside a registered function fails the test.
- Each agent child and each Git child has its **text pinned**, from
  `Command::new` to the spawn or the end of the builder function.
- The **callers** of the two probe builders are pinned, so a second script
  cannot ride `local_probe` or `ssh_command`.
- The ids must be a subset of a **frozen list of 17**. An entry cannot be
  swapped for a different child, and the list cannot grow.

**The sanctioned builder's contract, checked statically.** `git_carry::git`
uses `git_env::CLEARED`, `CONFIG` and `SET`, and adds `--no-optional-locks`
and `GIT_CEILING_DIRECTORIES`. `CONFIG` carries `core.hooksPath=/dev/null`,
`core.fsmonitor=false`, `gc.auto=0` and `maintenance.auto=false`. `SET`
carries `GIT_OPTIONAL_LOCKS=0`, `GIT_NO_LAZY_FETCH=1`,
`GIT_TERMINAL_PROMPT=0`, `GIT_CONFIG_NOSYSTEM=1`,
`GIT_CONFIG_GLOBAL=/dev/null` and `GIT_NO_REPLACE_OBJECTS=1`.

**What callers do with the builder, checked statically.**

- No agent code calls `env_clear` or `envs`.
- No literal outside `git_env` assigns a Git configuration key, except
  registered extras, and none of those touches a guarded key. The probe
  script must repeat the table's own values.
- The contract's variables, and the variables that inject configuration,
  are named only in `git_env`, the probe builder and one registered
  injector (`git_carry::nest_status`).
- Every Git subcommand in the `git_carry` tree is a registered read or one
  of 14 registered writers. Each writer is pinned to the exact number of
  uses the tree holds (44 in all at main `a80c63b`): one more fails, and so
  does one fewer, until the number is lowered (section 5). A literal that
  holds a shell line, such as the probe script, is read as shell, so a Git
  subcommand inside it is checked too.

**This is a census, not a proof.** A text scan cannot tell a source
repository from a capture's private one. That every writer is aimed at a
private, scratch, envelope or destination repository was established by
reading each call site, and by the dynamic leg for the verbs it runs. The
real fix is two builder types, which is production work filed on #188.

**The dynamic leg.** A `git` wrapper first on `PATH` records every Git
child of a real `git-export`, a real local `git-carry-estimate` and a real
`estate-capture`. One run recorded 345 children: 77 from the export, 36
from the estimate and 232 from the estate capture. Of those, 232 were aimed
at a protected repository. Every child carried the contract, none overrode
a guarded key with a later `-c`, none carried injected configuration except
`filter.*` keys, and every child aimed at a protected repository ran a
read.

### P77 SOURCE-LOCK-TRACE (`tests/source_lock_trace.rs`, Linux only)

The `io-trace` recorder cannot answer the lock question. It records only the
agent's own mutating `io::sys` calls. P77 watches the kernel instead, with
no `LD_PRELOAD` and no new crate, through four channels:

- **The lock table.** `/proc/locks`, sampled while the verb runs. It lists
  `flock`, POSIX and OFD locks for every process. **It does not list Git's
  locks.** Git locks by creating `*.lock` files.
- **This process's descriptors.** `/proc/self/fdinfo`, sampled in the same
  loop. It does not see a child's descriptors.
- **A write watch.** An inotify watch on every source directory. It is a
  queue, not a sample, and it reports every create, delete, rename, write,
  re-stamp and close of a write-mode descriptor, by any process. This is
  the channel that sees a Git child's `index.lock`.
- **A holder.** It keeps an exclusive `flock` and a whole-file OFD write
  lock on every source node. A blocking lock attempt then waits until the
  deadline. The copy legs also make the tree read-only.

An `lstat` census of the source (every field but atime) is compared before
and after each verb.

| Leg | What it requires |
|---|---|
| `copy`, free run | No lock line on a source inode, no write-mode source descriptor, census unchanged, every byte carried. |
| `copy`, holder run | The copy neither waits (120 s deadline) nor refuses, with the tree read-only. |
| `serve`, as its own process, holder run | The serving process takes and waits for no lock. |
| `git_carry::export_repository` (v1), holder run | No write event in the repository from any process, no lock line, no in-process write-mode open, census unchanged. |
| `estate::capture` of two repositories, holder run | The same, for both repositories. The estate's own lock and the plan's lie outside both. |
| local `git-carry-estimate`, source and destination both held | The same, for both repositories. |
| `provider_sqlite::snapshot`, idle WAL writer in another process | See below. |
| `provider_sqlite::snapshot`, committing WAL writer | The writer's commits all succeed: none finds the database busy, and the slowest is under 10 s. |

Both Git fixtures have a stale index. A self-test shows that an ordinary
`git status` rewrites that index and that the watch sees it, so a clean leg
means the children were hardened, not that the fixture was easy.

**For the Git legs, the lock evidence is the write watch and the census,
not the lock table.** The lock table and the holder add only that no
`flock` or `fcntl` lock is taken either.

**The SQLite legs run one of two ways, and print which.** As an ordinary
user they run the backup and hold it to the counted exception. As root they
assert the refusal (OI-1003-Q76). Neither run proves the other's half.
Section 5 has the root finding and both sets of numbers.

**As a user: the exception.** OI-1003-Q16 allows a shared read lock on the
database that is bounded and counted. OI-1003-Q36 allows the `-shm`
wal-index (`source_wal_index_touched`). OI-1003-Q72 allows an empty `-wal`
only where none existed (`source_wal_created`). Three legs:

- **Idle writer** (a `-wal` with frames exists). This process's locks are
  only READ on the database, READ on the `-shm` lock bytes, and a one-byte
  read-mark WRITE on the `-shm`. The database read lock is released on
  return and held under a 30 s test bound. No process waits. The listing,
  the `lstat` census without the `-shm` (the `-wal`'s row included), the
  bytes of the database, the `-wal` and a sibling, and the write watch all
  agree that nothing but the `-shm` was written. The counters move by
  `source_wal_index_touched=+1` and `source_wal_created=+0`. The exception
  is observed, not assumed: the leg repeats the snapshot (at most 40 times,
  every one held to every limit) until the sampler sees the database lock,
  the `-shm` locks and the `-shm` open in one snapshot.
- **Closed database** (no `-wal`, no `-shm`). The first snapshot's write
  set is the two counted sidecars and nothing else: `+1` and `+1`, a
  zero-byte `-wal`, events on the `-wal` of `create` and `close-write`
  only, and the database and sibling byte- and `lstat`-identical. A second
  snapshot finds the empty `-wal` in place: `source_wal_created=+0` and the
  `-wal` `lstat`-identical.
- **Committing writer.** The writer is never found busy and its slowest
  commit is under the bound.

**The residual: an existing `-wal` is held open `O_RDWR`.** Q72 says an
empty `-wal` is allowed "only where no `-wal` existed" and that "a `-wal`
that existed before the read stays byte-identical". Q16, Q36 and Q72 do not
name a write-mode descriptor on a `-wal` that already existed, and neither
counter sees one. Q72's condition for that file is on its bytes, and it
holds. So the open is neither granted nor a breach of a stated condition.
The test names it `RESIDUAL_WAL_OPEN_READ_WRITE` and tolerates exactly two
things for an existing `-wal`: this process's write-mode descriptor, and
that descriptor's one `close-write` event. It tolerates no `modify`, no
`attrib`, no create, delete or rename, and no lock. The criterion "no
write-mode open but the `-shm`" is still not met; closing it needs a
provider change or a ruling, on #157, which another lane owns.

## 2. Mutation evidence, leg by leg

Every mutant was applied to a **copy** of the tree under
`/srv/cache/jess/s2-proof-closure-mut`, never to the lane worktree. Static
mutants ran the 14 static registry tests. Controls before and after were
green.

### P76, static registry (16 mutants, all red)

| Mutant | Caught as |
|---|---|
| `use std::process::Command as Cmd; Cmd::new("git")` | "`Command as ..` renames the type" |
| A `#[cfg(test)]` enum variant placed before a production function that builds a Git child | the function's child is unregistered |
| An ungated `tests.rs` module that builds a Git child | the child is unregistered |
| `libc::system(..)` | a child started without `Command` |
| A second `ssh` in `main::pull_command` | built 2 times, registered as 1 |
| A second `bash` in `local_probe` | built 2 times, and the text changed |
| A `program` variable shadowed inside `hydrate_one` | built 2 times, and the text changed |
| A caller doing `env_clear`, a later `-c` on guarded keys, `update-index`, `reflog expire` and `fetch` | contract breaches |
| A caller adding only `-c gc.auto=1` | contract breach |
| A caller running only `reflog expire` | unregistered writer |
| One more `update-ref` call | over the writer's ceiling |
| A new caller of `local_probe` with its own script | callers changed |
| A new Git child in `bulkload-handoff` | unregistered |
| A new Git child in `bulkload-bench` | unregistered |
| `GIT_OPTIONAL_LOCKS=1` inside the probe script | contract breach |
| `g gc --auto` inside the probe script | contract breach |

### P77 and the dynamic leg (library mutants)

| Mutant | Edit | Result |
|---|---|---|
| G1 | Strip `--no-optional-locks` and `GIT_OPTIONAL_LOCKS=0` from the builder | P76 **red** (static contract and dynamic leg). P77 **green**: the v1 capture runs no index-refreshing subcommand on the source, so this mutant cannot change what Git does there. |
| G2 | Add a hardened `git status` to `export_pass` | P77 **green**. This is the control for G3. |
| G3 | G2 plus G1's strip | P77 Git leg **red**: the watch reports `.git/index.lock` created and renamed over `.git/index`. Estate leg **red**, but for a different reason: the capture refuses with `ContractSelfInconsistent` before the leg's own write assertion is reached. |
| X1 | Add an unhardened `git status` to the probe script | P77 estimate leg **red**: the watch reports the destination's `index.lock`. |
| X2 | The same status through the script's hardened `g` | P77 **green**. The control for X1. |
| S1 | Open the source read-write and checkpoint | Both SQLite legs **red**: WRITE locks on the `-shm` outside the exception, and a write-mode open of the database. |
| S2 | Create `state.db-journal` in the source directory | Both SQLite legs **red**, but only because the backup itself then fails (`SqliteBackupFailed`). This mutant does not exercise the listing check. S4 does. |
| S3 | Hold `BEGIN IMMEDIATE` on a second connection | Both SQLite legs **red**: a WRITE lock on the `-shm` write-lock byte, and a write-mode open of the database. |
| S4 | Create and delete a stray file in the source directory | Both SQLite legs **red**: the watch reports the create, the write and the delete. |
| C1 | Blocking `flock(LOCK_SH)` in `transfer::open_source` | Copy free run, copy holder run and the serve leg all **red**. |
| C2 | Non-blocking `F_SETLK` read lock, failure ignored | Copy free run **red** (POSIX READ lines). Copy holder run and serve leg **green**. This is the stated blind spot. |
| E1 | Take the estate lock inside the first source | Estate leg **red**: the watch reports `estate.lock` created in the source. |

### Recheck stage (independent mutants, on `c8c1783`)

The recheck stage wrote its own mutants, again on a copy under
`/srv/cache/jess/s2-proof-closure-mut`. 40 static mutants ran the 14 static
registry tests; controls before and after were green.

- **37 red.** A module alias (`use std::process as p; p::Command::new`), a
  function pointer (`Command::new` without a call), a grouped rename, seven
  `#[cfg(test)]` placements in front of a production child (a `const` with a
  struct literal, a grouped `use`, a `static` closure, stacked attributes, a
  trait method declaration, a match arm, a `cfg(all(test, ..))` statement),
  a new child in `main.rs`, in `bulkload-proto` and in `bulkload-handoff`,
  `libc::fork`, `CommandExt::exec`, one more use of **each** of the 14
  registered writers (every ceiling is tight), a `format!` on a guarded
  key, an `alias.*` assignment, a contract variable set and removed by a
  caller, a subcommand held in a `const`, a new `pub(crate)` builder, and
  two writers' argument lists in `estate.rs`.
- **3 green**, all deliberate spellings, now listed in section 3: a macro
  that takes the type as an argument, an ungated `#[path]` module that names
  a gated test file, and an `include!` of a file that is not `.rs`.

Two library mutants ran both test files:

| Mutant | Edit | Result |
|---|---|---|
| R1 | `update-ref refs/heads/sneak HEAD` on the source at the top of `export_pass` | P76 **red** three ways (the ceiling, the caller test, the dynamic leg). P77 Git leg and estate leg **red**. |
| R2 | A write-mode reopen of each source file in `transfer::open_source`, closed at once, failure ignored | Copy free run **red**: the watch reports `close-write` on every file. Copy holder run and serve leg **green**: the tree is read-only there, the open fails, and a failed open leaves no event. The same class as C2. |

## 3. What is not proved

- **#165, the S2 measured budget (OI-1003-Q34).** The proof-grade run on
  neo has not run. These property tests do not stand in for it.
- **The `-wal` `O_RDWR` open on an existing `-wal` (#157).** A named
  residual: no ruling grants it and none refuses it. See section 1.
- **A permitted SQLite read as root.** There is none to prove: the provider
  refuses (OI-1003-Q76). The root legs prove the refusal and an untouched
  source. They do not prove the Q16/Q36/Q72 exception, which only a non-root
  run exercises. PR CI runs as root, so **CI proves the refusal half only**;
  the exception half is proved by local runs as a user (section 5).
- **Real root.** The local root runs used `unshare -r`, a user-namespace
  root (euid 0, mapped to uid 1000). It reproduces the CI failure exactly
  (section 5), but real root on the CI runner is proved only by CI itself.
- **Lock lines on sidecars the snapshot creates.** In the closed-database
  leg the `-shm` and `-wal` have no inode when the sampler starts, so their
  lock lines are not sampled. The idle-writer leg accounts for the `-shm`.
- **A typed source/private Git builder (#188).** Until it exists, "no
  writer is aimed at a source" is a census plus a dynamic check of three
  verbs.
- **Verbs with no P77 leg.** `estate::apply`, `git-carry-estimate` over
  ssh, `export_repository_with_policy` and the prerequisite and drift
  paths, `hydrate-state`, and a real `pull` over ssh. S2's lock property
  stays open for each.
- **Verbs outside the P76 dynamic leg.** `estate-apply`, `restore` and the
  ssh estimate are covered by the static registry only.
- **`git fetch` grandchildren.** Four `fetch` calls read a bundle file into
  a private repository. Git starts its own helpers for a fetch, and the
  `PATH` wrapper does not record them.
- **`git_carry::nest_status`.** It injects `filter.*` configuration to
  switch a nested repository's filter drivers off. The dynamic fixture has
  no nested repository with a filter, so that path has not run under the
  oracle.
- **A non-blocking lock attempt whose failure is ignored.** Under the
  holder it fails silently, and in the free run it is seen only if a sample
  lands on it (mutant C2). The sampling period was 2 to 26 ms in these
  runs. A lock shorter than one period can be missed.
- **A read-mode open by a child**, and a lock on a file outside the watched
  trees.
- **What the scan cannot find.** A subcommand or configuration key
  assembled at run time, a macro that assembles a spawn name, and a child
  started by a dependency. Three deliberate spellings also pass (recheck
  mutants): a macro that takes the type as an argument (`mk!(Command)`
  expanding to `$t::new`), an ungated `#[path]` module that names a file
  another module gates behind `#[cfg(test)]`, and an `include!` of a file
  that is not `.rs`. The tree holds none of the three today: its two
  `#[path]` attributes name the platform `io::sys` files, which are scanned.
- **A write-mode open that fails and is ignored.** Under the holder the
  copy and serve trees are read-only, so the open fails and leaves no event
  (recheck mutant R2). The copy's free run sees it; `serve` has no free run.
- **The Git legs do not make the tree read-only**, and none runs while a
  user's own `index.lock` exists.
- **No-signals half of S2.** These tests do not scan for signal calls.
  Under R-N92 lanes keep process-control words out of their commands, so a
  text scan for them was not written.
- **Darwin.** P77 compiles to nothing there: no `/proc`, no inotify. neo
  needs another channel.
- **The load1-lag estimator ruling and FADV.** Neither was touched by this
  lane. Both bear on the budget, not on the lock and write properties.

## 4. Receipts

- Tests at `a7b7ccc` (main `2247ab8` merged): `source_command_registry`
  15 passed; `source_lock_trace` 11 passed, of which 10 are tests and one,
  `sqlite_writer_helper`, is a helper that does nothing without its
  environment variable.
- Tests at `959ac6c` (adds the estimate leg): `source_lock_trace` 12
  passed, 11 tests and the helper.
- `cargo clippy -p bulkload-agent --all-targets -- -D warnings` was clean
  at `959ac6c`, and at `a7b7ccc` with and without `--features io-trace`.
  `cargo fmt --check` was clean.
- `just check-fast` at `ad80d52` (main `2247ab8` merged) exited 0; the
  receipt is on #188. The first round's receipt was for `f3df6dc` on base
  `b6ecd50` and is superseded.
- Recheck stage, at `c8c1783` (main `48bd697` merged, which brings
  `git_carry/decide.rs` and a changed `git_carry/shared.rs`):
  `source_command_registry` 15 passed and `source_lock_trace` 12 passed,
  with no registry entry changed.
- Second fix round, on `fdc5fac` (main `a80c63b` merged) plus the round's
  test changes: `source_command_registry` 16 passed, `source_lock_trace` 13
  passed as uid 1000 and 13 passed under `unshare -r`, `prop_seed_guard` 6
  passed with `scanned=83 exempt_present=1 escapes=0`. `cargo fmt --all
  --check` clean; `cargo clippy --workspace --all-targets --locked -- -D
  warnings` clean, and again for `-p bulkload-agent --features io-trace`.
- `just check-fast` on the final head is reported in the pull request and
  the agent note's follow-up, not here: a commit cannot carry the receipt of
  its own check.

## 5. Second fix round, 2026-10-07: root, the empty `-wal`, exact tables

### The root finding

PR CI run 37540759845 failed on `b385f08`:
`a_sqlite_snapshot_locks_only_under_the_q16_q36_exception`, `write events
outside the -shm: [WriteEvent { path: .../state.db-wal, what: "attrib" }]`.
CI runs as root. Run as root, the bundled SQLite re-applies the database's
owner to the `-wal` and `-shm` it opens, which moves their ctime; as any
other user it skips that call. P77's write watch saw exactly that. It is a
source metadata write that no ruling admits and no counter sees. The
operator ruled OI-1003-Q76: the provider refuses to read a source as root
(`SQLITE_SOURCE_AS_ROOT`, landed in #196).

P77 now proves, per run, the half its uid allows.

| Run | What is asserted | What is not |
|---|---|---|
| uid 0 (PR CI) | All five provider verbs return `SQLITE_SOURCE_AS_ROOT`; no write event; no lock or write-mode open of this process; the listing, every node's `lstat` row (directory, database, `-wal`, `-shm`) and every file's bytes are identical; a closed database gets no sidecar; the output directory stays empty. Three sources: idle writer, closed database, committing writer (its own two sidecars excepted). | The Q16/Q36/Q72 exception: no backup runs. |
| any other uid | The exception as in section 1, with both counters. | The refusal. |

No privilege is dropped or gained in the test. Main's P75 tried to drop to
an unprivileged uid in a child and CI denied it, so P77 does not try.

**Local root run** (`unshare -r <test binary> --nocapture --test-threads=1`,
13 passed):

```
p77 sqlite leg [closed database]: ROOT (euid 0). Proving the typed refusal SQLITE_SOURCE_AS_ROOT and an untouched source (OI-1003-Q76). The Q16/Q36/Q72 exception leg is NOT run as root.
p77 sqlite root refusal (Closed, OI-1003-Q76): 5 verbs refused SQLITE_SOURCE_AS_ROOT; write events 0; our locks 0; write-mode opens 0; 3 nodes lstat-identical, 2 files byte-identical; 6 samples; writer None
p77 sqlite leg [live writer]: ROOT (euid 0). ...
p77 sqlite root refusal (LiveWriter, OI-1003-Q76): 5 verbs refused SQLITE_SOURCE_AS_ROOT; write events 0; our locks 0; write-mode opens 0; 3 nodes lstat-identical, 2 files byte-identical; 6 samples; writer Some(WriterReport { commits: 12, busy: 0, slowest: 277µs })
p77 sqlite leg [idle writer]: ROOT (euid 0). ...
p77 sqlite root refusal (IdleWriter, OI-1003-Q76): 5 verbs refused SQLITE_SOURCE_AS_ROOT; write events 0; our locks 0; write-mode opens 0; 5 nodes lstat-identical, 4 files byte-identical; 5 samples; writer Some(WriterReport { commits: 0, busy: 0, slowest: 0ns })
```

The refused calls return in microseconds, so the sampler takes few samples.
The complete channels for the refusal are the write watch (a queue), the
census, the bytes and a lock-table read after the calls return.

**Local user run** (uid 1000, same binary, 13 passed):

```
p77 sqlite empty -wal (Q72): first snapshot source_wal_index_touched=+1 source_wal_created=+1, -wal events {"close-write", "create"}, -wal 0 bytes, db-read-lock=0 shm-write-open=0 wal-write-open=0 over 7 samples; second snapshot source_wal_created=+0, existing -wal lstat-identical, its events {"close-write"} (close-write is the #157 residual)
p77 sqlite live writer: 1 attempt(s) ["Ok(()) during 13 commits, 6 samples with our lock so far"]; writer commits=16 busy=0 slowest commit 1.597ms (bound 10s); db-read-lock=6 shm-read-lock=11 shm-read-mark-write=0 over 13 samples, mean period 2942 us
p77 sqlite exception (sample counts, Q16/Q36/Q72): snapshot 2 of at most 40, 10 samples, mean period 2567 us, db-read-lock=4 shm-read-lock=7 shm-read-mark-write=0 shm-write-open=4 wal-write-open=4 (#157, residual), db-read-lock held 8.573565ms (bound 30s), source_wal_index_touched=+1 source_wal_created=+0, write events {"close-write"}
```

These ran with the scratch on tmpfs (`/dev/shm`), where a 2 MiB backup takes
a few milliseconds and can fall between two samples of `/proc/locks`. The
earlier numbers in this file (71 samples) were on xfs. That is why the
idle-writer leg now repeats until the exception is seen, and why the live
leg also waits for a sample that saw this process's lock. In this
round, before the change, one of the first two runs on tmpfs failed "the
sampler never saw the database READ lock".

### Mutants for this round

Applied to a copy under `/srv/cache/jess/s2-proof-closure-mut`; all red.

| # | Mutant | Result |
|---|---|---|
| S1 | `refuse_source_read_as_root()?` removed from `snapshot` | Under `unshare -r`, all three root legs **red**: `snapshot as root (OI-1003-Q76)`, got `None` or `SQLITE_STATE_CHANGED`, wanted `SQLITE_SOURCE_AS_ROOT`. |
| S2 | S1, and the test told to run the user leg whatever the uid | Under `unshare -r`, the idle-writer leg **red** with the CI failure itself: `write events outside the -shm: [WriteEvent { path: .../state.db-wal, what: "attrib" }]`. So the user-namespace root reproduces the finding. |
| S3 | `Footprint::settle` no longer bumps `source_wal_created` | Closed-database leg **red** as a user: counters `(1, 0)`, wanted `(1, 1)`. |
| S4 | A new `Command::new("git").arg("gc")` in `git_carry/shallow.rs` | P76 **red**: `agent::git_carry::shallow::sneak("git") ... not registered`, and `"gc" rewrites a repository's store`. |
| S5 | A second `git(..).args(["index-pack", "--stdin"])` in `shallow.rs` | P76 **red**: `"index-pack": 2 uses, ceiling 1`. At `b385f08` plus main this mutant **passed**: the registered number was still 2. |

### P76 against main `a80c63b`

The frozen id list is unchanged and exact: 17 ids, 18 children with the gpg
probe's two, plus the sanctioned builder. #196 and #198 added no child
process; #189 removed none of the 17 (carry_v2's children were built
through `git_carry::git` and the estimate's probe builder).

What #189 did leave behind is slack, and the test passed with it. The
tables were upper bounds, and one rule tolerated a missing caller "only
under carry_v2". After the merge that meant:

| Table | Stale at `b385f08` + main | Now |
|---|---|---|
| `local_probe` callers | 3 carry_v2 calls registered and gone, tolerated by name | removed; the tolerance is deleted, the caller list is exact |
| `BUILDERS` | 3 carry_v2 builders | removed; 8 builders, and a listed builder that is gone fails |
| `EXTRA_CONFIG` | 4 carry_v2 assignments | removed; 3 entries, and an entry no literal spells fails |
| `WRITERS` | `index-pack` 2 (holds 1), `update-ref` 10 (holds 9) | 1 and 9; 44 uses over 14 writers |
| `NOT_COMMANDS` | `commit` 6 (holds 4), `tag` 4 (holds 2), `refs` 1 (holds 0) | 4, 2, and `refs` removed; 18 literals |

`stale_entries` now fails the test when any table holds more than the
workspace bears out, and a self-test (`an_entry_that_outlives_its_code_is_refused`)
removes a module and requires each table to report it. No rule was
loosened: every change lowers a number, deletes an entry for deleted code,
or adds a check. `tests/prop_seed_guard.rs` reports `escapes=0` with these
files present (neither uses proptest).

```
p76 static: 17 bypass ids, 8 builders, 44 writer uses over 14 writers, 18 non-command literals, 3 extra config, stale=0
p76 dynamic: 345 Git children (77 git-export, 36 estimate, 232 estate-capture), 232 aimed at a protected root
```
