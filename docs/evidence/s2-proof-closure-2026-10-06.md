# S2 proof closure, sting, 2026-10-06 (property tests, not the budget)

Rulings: OI-1003-Q5 and OI-1003-Q9 (S2: never interrupt the source; no
locks, no writes, no signals), OI-1003-Q16 (WP0(b) typed source access and
the bounded SQLite exception), OI-1003-Q36 (the `-shm` wal-index
exception), OI-1003-Q60 (completion-bar audit), R-N13.

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
see sections 2 and 4. The host was heavily
shared. Tests ran inside `nix develop` with
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
  of 14 registered writers. Each writer has a ceiling, and the ceilings sum
  to 46. A literal that holds a shell line, such as the probe script, is
  read as shell, so a Git subcommand inside it is checked too.

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

**The SQLite exception.** OI-1003-Q16 allows a shared read lock on the
database that is bounded and counted. OI-1003-Q36 allows the `-shm`
wal-index. The leg requires:

- this process's locks are only READ on the database, READ on the `-shm`
  lock bytes, and a one-byte read-mark WRITE on the `-shm`;
- the exception is observed, not assumed: the counts are greater than zero;
- the database read lock is released on return and is held under a 30 s
  test bound;
- no process waits on any lock;
- no other source write occurs: the directory listing, an `lstat` census
  without the `-shm`, and the write watch all agree.

One run gave `db-read-lock=68 shm-read-lock=102 shm-read-mark-write=0
shm-write-open=67 wal-write-open=67` over 71 samples, with the database
read lock seen held for 122 ms. Under a committing writer, two runs gave 51
commits with the slowest at 998 ms, and 11 commits with the slowest at
59 ms. Both had `busy=0`.

**The `-wal` criterion is NOT met.** The backup's read-only connection
holds the source `-wal` open `O_RDWR`. Q16 and Q36 do not name that. The
bytes do not change, and the test tolerates it under
`WAL_OPENED_READ_WRITE = true`. So the acceptance criterion "no write-mode
open but the `-shm`" fails today, and the test records the failure rather
than hiding it. It needs a provider fix or a Q36 amendment. Both belong to
#157, which another lane owns.

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
- **The `-wal` `O_RDWR` open (#157).** The criterion is not met. See above.
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
- `just check-fast` on the final head is reported in the pull request and
  the agent note's follow-up, not here: a commit cannot carry the receipt of
  its own check.
