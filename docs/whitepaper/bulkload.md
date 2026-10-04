# bulkload: moving a live agent estate between machines

**Whitepaper, draft of 2026-10-03.** Proof package item one (OI-1003-Q7).
Code base: `origin/main` at `4a10bb8`. That includes WP1 (#145, merged as
`adb9c66`), WP2 PR 1 (#144, `04ea9cb`), WP10 PR 1 (#152, `6268175`),
WP2 PR 2 (#146, `46587af`), bounded salvage with legacy-row invalidation
(#154, `4a7b86b`) and WP10 PR 2 (#153, `4a10bb8`).
References use keys in square brackets (for example
[Rsync96]); each one resolves in the [bibliography](bibliography.md), with
a note on how it was verified.

This paper explains and argues. It is not a source of truth. Where it and a
normative document disagree, the normative document wins:

- [`docs/design.md`](../design.md) is the product contract;
- [`docs/slo.md`](../slo.md) defines S1–S5 and the completion bar;
- [`docs/evidence/`](../evidence/) holds every measured result this paper
  quotes.

Statements about the code cite a file under `crates/`. Statements about
results cite an evidence file and its date. Work that is ratified but not on
`main` at `4a10bb8` is marked as such. The paper quotes no SLO number; it
links [`docs/slo.md`](../slo.md) for them, so a dated amendment there cannot
leave this paper silently out of date.

## Abstract

bulkload moves a developer's working estate from one machine to another
while the source machine stays in use. The estate is Git repositories with
linked worktrees, stashes, staged and unstaged work; agent transcripts and
SQLite stores; dotfiles and credentials. The target user runs coding agents
that keep committing, writing transcripts and updating databases during the
move. bulkload's goal is that the move is not an event: it is a sync that
can be rerun at any time. The aim is that each rerun reads only what
changed, never reads a byte the destination already holds durably, and ends
with every planned item accounted for. Sections 4 and 5 say how much of
that is proven today.

The design has four parts:

- a streaming wire protocol (v5) in which the destination decides, per file,
  whether it needs anything;
- a digest-only source ledger that commits a capture only after the
  destination reports the bytes durable;
- a destination that stages, seals and publishes without replacement, and
  commits its records in groups;
- Git-aware and SQLite-aware capture, with typed refusals and drift custody
  in place of silent skips.

The claims are stated as SLOs (S1–S5 in [`docs/slo.md`](../slo.md)). Each is
to be backed by a property test, a formal model, a crash or power-loss
proof, or a gated benchmark; most of that is still being built. The
evidence so far is mixed, and this paper says so plainly. The resume
invariant (R25) has held in every recorded bench run. Every such run
measured the file mover, before wire v5; the Git carry path has not been
measured for it. **S1 is not met.** Gate (a) has no passing verdict. The
last completed R23 sample (2026-09-18, before wire v5) failed on the initial
copy: native median 3015.294 ms against rclone's 601.010 ms. It won the 1 %
delta, 58.842 ms against 127.064 ms, but both comparisons are mandatory
([evidence](../evidence/r23-2026-09-18.md)). The samples of 2026-09-23 were
ungated or informational. The wire v5 engine has no gated sample.

## 1. Problem

### 1.1 The estate

The estate is everything a developer and their agents need to resume work on
another machine ([`docs/design.md`](../design.md), "Completion"). It
includes:

- Git repositories, with refs, objects, stash commits, the index, dirty
  working bytes, ignored files, and linked worktrees with their
  administration directories;
- agent state: transcripts, configuration, and SQLite databases, often in
  WAL mode and open by a live process;
- dotfiles and account credentials, which must be carried privately and
  never printed.

Two terms recur. A **seat** is one path in a tree that holds a file, a
directory or a symlink. A **capture** is bulkload's record of one seat's (or
one repository's) content at one moment.

### 1.2 The source is live

The source machine keeps working during the move. Agents commit, rewrite the
index, append transcripts and write to SQLite. Two SLOs turn this into
requirements. Paraphrased here, they are stated exactly, with their
budgets, in [`docs/slo.md`](../slo.md):

- **S2:** bulkload must not interrupt the source: no locks, writes or
  signals on the source, background priority, and a measured latency and
  load budget.
- **S5:** bulkload must tolerate a moving source. What moved is recorded as
  drift custody, not refused wholesale and not silently absorbed. The next
  pass catches up.

There is no freeze window and no "stillness" ceremony. The ceremony was
retired along with the Python engine (`AGENTS.md`, "Repository").

### 1.3 Why rsync, rclone and tar are not enough

Each general-purpose mover is excellent at its own job, and bulkload borrows
from all of them (section 6). They fall short on this estate for five
reasons.

1. **Git state is a set of files that must agree.** A repository's index,
   refs, packs and loose objects are written by Git at different moments. A
   file-level copy taken while Git writes can pair an index with a ref set
   from another instant. Worktree administration also holds absolute paths,
   which a byte copy carries unchanged to a host where they are wrong. After
   the first neo → sting migration, ruling R36 (2026-09-02) had "mechanical
   residue" fixed before reclaim could proceed: 62 gitdir pointers, 72
   failed fetches and 6 HEAD syncs
   ([rulings ledger](../evidence/rulings-ledger-r1-r43.md)). The ledger
   records that residue, not its causes; this paper does not attribute it
   to byte-level copying.
2. **A live SQLite database is not its main file.** In WAL mode, committed
   transactions can live only in the `-wal` file until a checkpoint. A copy
   that separates the database from its WAL can lose committed transactions
   or corrupt the database [SQLiteWAL]. A safe copy needs SQLite's own
   backup interface [SQLiteBackup].
3. **No durable record of what the destination holds.** rsync decides by
   default with a quick check of size and modification time, so a rerun
   over an unchanged tree reads only metadata. With `--checksum`, the sender
   reads every file in the transfer and the receiver reads every file whose
   size matches the sender's [RsyncMan]. For a changed file, its delta
   algorithm reads the destination's old file to build block signatures and
   the source file to find matches [Rsync96]. rclone moves whole files and
   does not do partial-file deltas [Rclone]. Neither keeps a durable record
   of what the destination already holds. Each run re-derives the answer
   from size and mtime, or from checksums, and size and mtime cannot see the
   same-tick rewrite of reason 4. The gap is a durable record and same-tick
   correctness, not the cost of rerunning over an unchanged tree.
4. **Stat metadata can lie for one clock tick.** A same-size rewrite within
   the timestamp granularity keeps size and mtime. A quick check of size and
   mtime cannot see it. Git solves this for its own index with "racy" entry
   handling [RacyGit]. bulkload adapts the idea (section 2.6).
5. **"Done" must be checkable.** A file mover reports per-file errors. An
   estate migration needs a closure account: every planned item ends
   `applied`, `referenced-only` or a typed refusal, and nothing is
   unaccounted (S4).

`tar` adds a sixth gap: a one-shot stream has no resume, so an interrupted
run starts again from the first byte.

### 1.4 The goal: a routine rerun, not an event

OI-1003-Q13 sets the completion bar as confidence in the code and the
product. The full neo → sting migration is then meant to be one more run of
a trusted tool. It can start whenever convenient and be repeated as neo's
work lanes finish. Once S3 and S5 hold, each rerun converges without
re-reading anything it has already carried. That requires three properties
together:

- **S3:** an unchanged estate costs a metadata walk and no content reads;
- **S5:** a moving source produces custody, not failure;
- **S4:** every run ends with an exact account of every item.

The engine's speed (S1) matters, but the bar is that a rerun is cheap,
correct and boring.

## 2. Design

### 2.1 Shape

bulkload is a Rust workspace of four crates (`AGENTS.md`, "Repository"):

- `crates/bulkload-proto`: the wire codec and schema text
  (`src/frame.rs`), the row schema (`src/row.rs`) and the refusal taxonomy
  (`src/refusal.rs`);
- `crates/bulkload-agent`: the walk, the transfer engine, the destination
  store, Git estate carry, SQLite capture, closure and the crash tooling;
- `crates/bulkload-bench`: the R23 benchmark against rclone;
- `crates/bulkload-handoff`: the credential-class handoff probes. WP10 PR 1
  (#152) moved them out of the agent binary. A source scan now checks that
  no agent source names a credential or agent tool as a program to run
  (`crates/bulkload-agent/tests/no_credential_tools.rs`).

The agent may not depend on tokio, opendal, reqwest, ring or tonic. A test
enforces this (`crates/bulkload-agent/tests/dep_graph.rs`). R-N54 makes raw
syscalls, zero-copy and reused buffers the default design choice. The engine
does use raw syscalls (`src/io/sys_*.rs`), but buffer reuse is not wired in
yet. The slab pool and fused chunker (`src/io/buf.rs`, `src/io/chunker.rs`)
are compiled under a non-test `allow(dead_code)`. Since WP10 PR 2 (#153),
its stated reason defers wiring or deleting them to the gate (a) evidence
(#88, WP10 PR 3) (`src/io/mod.rs`). The transfer allocates a new buffer per
chunk (`send_capture` in `src/transfer.rs`), so no S1 number in this paper
includes buffer reuse. #153 also deleted most of the other unwired W4 io
items. Refusals are values, never panics, under a lint wall in each crate
(R33).

### 2.2 Wire v5

The transfer wire is protocol 5, a hard cut with no dual stack (R-N59,
R-N118). The schema text `WIRE_SCHEMA` and its BLAKE3, `wire_id`, live in
`crates/bulkload-proto/src/frame.rs`. A peer whose `Open` names another
protocol or another `wire_id` is refused before any other frame. Each frame
is a 4-byte big-endian length, a tag byte and a body:

- tag 1 is one postcard control message, with nothing after it;
- tag 2 is a content chunk: a fixed 64-byte header (entry, chunk index,
  size, offset, digest), then the payload;
- tag 3 is reserved for the Git sub-stream and refused today.

A session runs as follows ([`docs/design.md`](../design.md), "Wire v5";
`crates/bulkload-agent/src/transfer.rs`):

```text
source                                   destination
  Open{proto, wire_id, root, state}  -->
  Entry{n, row}  (streamed, <=1024 undecided)
                                     <--  Decide{n, Skip|Reuse|Send|WantManifest|Refuse}
  Send:          data frames, End{n, root, chunks, size, racy}
  WantManifest:  Manifest{n, ...}  -->
                                     <--  NeedChunks{n, indices}
                 data frames for those indices, End{n, ...}
                                     <--  Credit{bytes}   (16 MiB window)
                                     <--  Held{n, true|false}
  SourceDone{entries, source_bytes_read}   (after the ledger's last commit)
```

- **Entry and Decide.** The source offers each walked seat as a numbered
  `Entry`, with at most 1024 undecided (`ENTRY_WINDOW`). The destination
  answers with `Skip` (a directory or symlink it made), `Reuse` (it holds
  this exact stat identity durably, so the source reads nothing), `Send`,
  `WantManifest` or `Refuse`.
- **Streaming walk.** The walk offers each seat as it finds it, parents
  before children, names in byte order within a directory, with no global
  sort. It runs at most 4096 items ahead of the wire (`WALK_AHEAD`). Every
  seat is opened beneath one descriptor of the source root, one component at
  a time, with `O_NOFOLLOW` at each step (`openat_beneath` in
  `crates/bulkload-agent/src/io/sys_posix.rs`; `src/walk.rs`). A directory
  swapped for a symlink is refused, never followed. Depth and path-length
  caps (256 components, 4095 bytes) refuse a subtree as a typed value and
  carry its siblings (#110, #129). Content is read with `pread`, never
  mapped.
- **Send.** The source reads the file once, cuts it into content-defined
  chunks, hashes each chunk and streams it. The destination verifies every
  chunk against its digest, writes it at its offset, and checks coverage
  and the manifest root before the output joins a group commit.
- **WantManifest and NeedChunks.** The destination chooses this for a
  regular file it cannot `Reuse` when any of three things holds
  (`Inbound::entry` in `transfer.rs`): the output path already exists; the
  store records any chunk hint at all (`Store::has_output_hints`, checked
  once per session, not per file); or the start-of-session sweep salvaged
  any temporary. So in any session after one that published an output,
  every new or changed file costs a manifest round trip, whether or not
  the destination holds any of that file's chunks.
  [`docs/design.md`](../design.md) ("WantManifest") states the rule more
  narrowly: an existing output, or published outputs that hold chunks. The
  formal model's README at `3dbfbdb` already records the salvage condition
  as a disagreement between the code and `docs/design.md`.

  The source sends its manifest, from its ledger without reading when the
  stat identity is recorded. The destination fills what it can from
  verified local chunks, then asks only for the missing indices. A fresh
  manifest is built from one read kept in memory (512 MiB budget,
  `RETAIN_BYTES`); a larger file is streamed as for `Send`, so no seat is
  read twice in a session.
- **Credit.** The destination grants 16 MiB of payload credit
  (`CREDIT_WINDOW`) and returns it as it writes. The source never has more
  than the grant in flight, nor more than 16 entries' content.
- **Held.** The destination answers every `End` with `Held`. `Held{true}`
  means the output's group commit has returned: file and directory sealed,
  then the store commit. Only then does the source commit the capture to its
  ledger (section 2.4).

### 2.3 Chunks, digests and the manifest root

Chunk boundaries come from FastCDC through the `fastcdc` crate's 2020
variant, with 16 / 64 / 256 KiB minimum / average / maximum
(`crates/bulkload-agent/src/hash.rs`) [FastCDC16] [FastCDC20]. Each chunk is
hashed with BLAKE3 [BLAKE3]. A file's identity on the wire is its
`manifest_root`: BLAKE3 in derive-key mode, under a dated context string,
over each chunk's digest and size (`frame.rs`, `MANIFEST_ROOT_CONTEXT`). This
replaced the whole-file hash, so neither side hashes a file twice.

The manifest is built over content-defined chunks, not BLAKE3's own internal
tree. BLAKE3's tree splits input into fixed 1 KiB chunks [BLAKE3], so an
insertion near the start of a file shifts every later leaf. Content-defined
boundaries move only near the edit, which is what lets the destination reuse
the chunks it already holds.

### 2.4 The digest-only source ledger

The source keeps a ledger keyed by each seat's row key and stat identity
(`row_key` in `crates/bulkload-agent/src/transfer_store.rs`). Production
commits go through the source's group committer: `LedgerSink::publish`, then
`StorePublisher::commit_captures`, one transaction per group, in the same
file. (`Store::record_capture` commits one capture outside that path; only
tests call it, and WP8 moves it under `cfg(test)`.) A row holds the manifest
(chunk digests, sizes and `manifest_root`), never content bytes (R-N58).
Earlier engines kept a source-side byte pack for resume. On 2026-09-23, M0
counted one copy writing that pack (226,762,914 B) and reading it back (the
same count). W3 removed the read-back, and W4 removed the pack
([m0](../evidence/m0-2026-09-23.md), [w3](../evidence/w3-2026-09-23.md)).
The power-loss test now asserts that a `copy` writes no source pack
(`crates/bulkload-agent/tests/power_loss.rs`).

A row is written only when three conditions all hold:

- the capture's final stat check passed, so a seat rewritten during its
  read leaves no row (R-N86);
- the capture was not racy (section 2.6);
- the destination answered `Held{true}`.

The third condition is what makes R25 safe. A committed capture means the
destination holds the bytes durably, so a resume reuses or adopts the
output and never reads the seat again (OI-1001-Q15).

### 2.5 Destination: stage, seal, publish, group commit

The destination never overwrites an existing file; publication is
no-replace (`crates/bulkload-agent/src/materialize.rs`,
`src/io/durable.rs`). Each output is written under a tagged temporary name,
`.bulkload-<tag>-<pid>-<n>`, where the tag derives from the destination
store's random authority. Its group commit then:

1. seals each file: `F_BARRIERFSYNC` on Darwin, `fsync` elsewhere
   (`seal_file`);
2. publishes it under its final name without replacement, using
   `renameat2(RENAME_NOREPLACE)` on Linux or `renameatx_np(RENAME_EXCL)` on
   Darwin, or a link and unlink where the filesystem has neither (R-N119);
3. seals each touched directory once (`seal_dir`);
4. fully flushes each touched device other than the store's own;
5. commits the group's records in one SQLite WAL transaction with
   `synchronous=FULL` and `fullfsync=ON`. On Darwin that commit's
   `F_FULLFSYNC` drains the store's device, so a group whose files share
   that device needs no other device-cache flush.

A group closes at 64 files, 256 MiB or 20 ms idle (`GROUP_FILES`,
`GROUP_BYTES`, `GROUP_IDLE`). A directory the engine creates gets a tagged
temporary name, its record is bound to the new inode, and it is renamed into
place without replacement (R-N102). The engine never adopts a directory it
did not create (R-N78). `--durability=strict` fully flushes every file
instead, for comparison.

Space is checked before writing. An entry decided `Send` or `WantManifest`
reserves its size until its `Held`, and a write that would leave less than
`--min-free-percent` free (default 25 %) is refused
`DESTINATION_SPACE_INSUFFICIENT` as a value. The session continues
([`docs/design.md`](../design.md), "Durability").

### 2.6 Crash resume and racy captures

A rerun after an interruption, or over a store an older engine wrote, meets
five kinds of leftover state:

- **Committed captures and output records.** A seat whose stat identity is
  unchanged is answered `Reuse`, and the source reads nothing.
- **Final names without records.** An existing output is adopted only after
  it is verified against the source's manifest (`WantManifest`).
- **Temporaries.** The sweep acts on a temporary only when it carries this
  store's tag, matches the name grammar exactly, and has the expected kind
  and owner (R-N79). It keeps each such orphaned file temporary that has no
  other link as a chunk source for the new session, under a session name
  (`sweep` in `materialize.rs`). These save wire bytes only: no recorded
  capture's bytes live only in a temporary.
  - Since #154 (`4a7b86b`), the end of the session removes them all,
    except those that an entry refused in that session had staged chunks
    from. Such a refusal touched bytes: the entry's staged file was
    filled, then failed to verify, to publish or to commit with its group.
    Those temporaries are kept for the retry and reported as left (#124,
    OI-1002-Q33).
  - An entry refused before it staged anything keeps nothing, so a path
    refused on every run never accumulates temporaries (#97).
  - Kept salvage is bounded per session at 1024 temporaries and 4 GiB
    (`SALVAGE_KEEP_FILES` and `SALVAGE_KEEP_BYTES` in `transfer.rs`),
    filled greedily in salvage order (`bound_salvage`). A temporary past
    the bound is removed and refused as a value, `SALVAGE_BOUND_EXCEEDED`
    under its current name, and its chunks are sent again by the next run.
  - The values were ratified as OI-1003-Q24 (Linear TIN-4543), as the
    comment at the constants says. Their `docs/slo.md` text is on
    `docs/coordinator-20261003` (`9784467`), not on `main`, and
    `docs/design.md` on `main` still calls them unruled engineering
    defaults.
- **Rows from before the racy guard** (#125, OI-1003-Q26). Since #154, a
  store this engine creates carries a `racy_guard` marker from its first
  commit. A store without the marker was written by an earlier engine, some
  of them before the racy rule below existed (#86), so none of its ledger or
  output rows is trusted to show that its seat was not racy.
  - The first writable open deletes every `captures` and `outputs` row in
    the transaction that adds the marker, and counts them
    (`transfer_legacy_rows_invalidated`). A read-only handle on an unmarked
    store trusts none of them (`Store::open` and `Store::open_reader` in
    `transfer_store.rs`).
  - Each such seat is read once more, which R25 allows, and is then
    recorded under the guard. Chunk hints are kept, so content the
    destination still holds is adopted after verification and does not
    cross the wire again.
- **A failed group.** A group whose seal or store commit fails (a full disk,
  for example) answers each entry `Held{false}`. Neither store records
  them, and the next run reads each one once (#100).

**Racy captures.** A seat whose mtime or ctime falls within a 2 s allowance
of the clock read before it was opened, or later than the clock read after
its final stat check, is racy (`RACY_GRANULARITY_NS` in
`crates/bulkload-agent/src/git_carry.rs` and `src/transfer.rs`). This adapts
Git's racy-index idea [RacyGit] to a mover (section 6 says how it differs).
A same-size rewrite in that window can keep its stat identity. A racy
capture is sent and published, but neither side records it, so the next run
reads it again (#86, R-N76). Correctness comes before the zero-reread claim.

### 2.7 Git estate carry

Git state is carried as Git objects, not as files under `.git`
(`crates/bulkload-agent/src/git_carry.rs`, `src/estate.rs`,
`src/git_carry/shared.rs`). The production engine (v1) captures each
repository into a private repository that borrows the source's objects
through `objects/info/alternates`, and writes the history as a bundle
[GitBundle]. A capture carries:

- refs, objects and real stash commits, including binaries and untracked
  files;
- the index bytes, `info/exclude`, configuration, the symbolic HEAD and the
  shallow frontier;
- working-tree dirt, ignored files and worktree administration, with paths
  translated for the destination.

Import preserves divergence. It never changes an active checkout's HEAD,
index or working bytes. Restore verbs check a bundle's in-band drift marker
from its headers before they write anything.

Every Git child of the v1 carry and the estimate is built by `git()` in
`git_carry.rs` from one hardening table, `git_carry::git_env` (WP1;
[`docs/design.md`](../design.md), "Live union", "Source safety"):

- it removes eleven inherited variables that would point Git at another
  repository, object store, ceiling or configuration (`GIT_DIR`,
  `GIT_INDEX_FILE`, `GIT_CONFIG_PARAMETERS` and the like). Other `GIT_*`
  variables, such as `GIT_EXEC_PATH`, `GIT_SSH` or `GIT_TRACE`, pass
  through;
- it sets no prompt, no system or global configuration, no replace objects,
  `GIT_NO_LAZY_FETCH`, no optional locks and the C locale;
- it passes `--no-optional-locks` and `-c` overrides that turn off hooks
  (`core.hooksPath=/dev/null`), fsmonitor, automatic gc and maintenance, and
  bound pack resources;
- it sets `GIT_CEILING_DIRECTORIES` at the given path's parent.

The estimate's remote probe script is tested entry for entry against the
same table. A partial-clone source refuses `GIT_SOURCE_PARTIAL_CLONE` in
`git-export` and `estate-capture` before any other read. `GIT_NO_LAZY_FETCH`
on every child is the second guard against faulting in a lazy fetch.

**The cost problem, and the ruling.** Before WP2 PR 2, a v1 capture with
no prerequisite base ran `bundle create --all` (`write_bundle` in
`shared.rs`), so a changed rerun re-packed the repository's whole history.
The cohort-1 estimate (run on 2026-09-24 UTC) measured 9.759 MB of thin
pack actually missing, against 1,475.90 MB of history on disk
([estimate](../evidence/git-carry-estimate-2026-09-23.md)). A second engine,
`carry_v2`, implements negotiated thin packs in the style of Git's pack
protocol [GitPackProto] [GitPackObjects]. On fixtures, its sent object set
equalled what `upload-pack` sends for the same have list
([sender evidence](../evidence/w6-m1-sender-2026-09-24.md),
[ingest evidence](../evidence/w6-m1-ingest-2026-09-25.md)). WP0(a)
(OI-1003-Q15) chose to measure first:

- count v1's pack reads and census walks;
- derive v1's bundle prerequisite from the previous retained capture's tips;
- measure S3 on the estate-shaped corpus;
- then choose v1, a hybrid, or v2 on the numbers.

The first two steps are on `main`:

- **Counters.** WP2 PR 1 (#144) counts the storage reads of the Git
  children that pack a capture, and every metadata census (`census_walks`;
  `tests/git_capture_counters.rs` pins it at 4 for a changed item and 1 for
  a reuse hit).
- **Auto-prerequisite chains.** WP2 PR 2 (#146, merged as `46587af`;
  `git_carry/chain.rs`; [`docs/design.md`](../design.md), "Drift").
  - With no plan base, a capture that follows a retained capture of the
    same checkout declares that capture's tips as its prerequisites.
    Only tips whose commits the source object store holds count. It then
    packs only what is new (`write_chained` in `shared.rs`).
  - A `.prior` sidecar, durable before the record, names each link.
  - Depth is capped at 8 (`CHAIN_DEPTH_LIMIT`). The capture after a
    depth-8 bundle re-bases to a self-contained bundle, which re-packs the
    whole history again.
  - Restore checks every link and flattens the chain into one bundle
    before it applies. A broken chain is never a reuse hit, and the next
    capture re-bases.
  - Some side-door verbs still take a single bundle (#148).

The evidence for the chains is from fixtures only:

- #146's counters test shows that a rerun after one new commit writes
  less than an eighth of the 256 KiB history it used to re-pack;
- a property test (P-CHAIN, `estate::wp2_chain`) checks that random reruns
  chain without re-packing that history, and that apply restores HEAD and
  the bytes.

No estate run has measured them yet. Measuring S3 on the estate-shaped
corpus is the next step.

Until then the ruling keeps `carry_v2` frozen behind a feature. The code
does not have that gate yet. `carry_v2` is declared unconditionally
(`pub mod carry_v2;` in `git_carry.rs`), and the agent's only cargo features
are `fault-injection`, `io-trace` and `m1-spike`, so every build compiles
it. It is frozen by ruling and by having no product caller: no verb reaches
it. WP2 PR 3 removes it, or gates it, once the measured choice is made.

### 2.8 SQLite through the backup API

Live provider databases are captured with SQLite's online backup API, never
by copying `-wal` or `-shm` bytes (`snapshot` in
`crates/bulkload-agent/src/provider_sqlite.rs`; `AGENTS.md`, "Estate
rules"). The capture:

- opens the source read-only through normal WAL-aware access, never
  `immutable` mode, with a zero busy timeout;
- copies 128 pages per step, within a bounded step budget;
- on lock contention, refuses `SQLITE_STATE_CHANGED` at once instead of
  waiting on the live writer;
- converts the snapshot to `journal_mode=DELETE`, so it is one portable file;
- runs `quick_check` and `foreign_key_check` before the snapshot may be used.

The backup API holds a read lock on the source only while a step reads, and
restarts when another connection writes mid-backup [SQLiteBackup]. The step
budget turns an endlessly restarting backup into a typed refusal
(`BUDGET_EXCEEDED`). That shared read lock is the one stated exception to
S2's "no locks" (WP0(b), OI-1003-Q16). The ruling requires it to be
shared-read only, bounded and counted. The code bounds it by step count
(`max_steps` steps of at most 128 pages, with no wait on contention), not by
duration. No counter records backup steps or lock holds yet.

A probe reported on 2026-10-03 found that this kind of open also creates a
`<db>-shm` file, SQLite's wal-index, beside a WAL source database that has
none (section 8). That is a source-side write. OI-1003-Q36 (recorded on
Linear TIN-4543) admits it by extending the Q16 exception, on conditions:

- a backup-API read of a WAL-mode source may create or touch `<db>-shm`.
  It is SQLite's coordination file, it carries no user data, and a live
  writer on the source would create it in any case;
- the effect is counted and recorded in S2 evidence;
- the main database file and its `-wal` must stay byte-identical;
- a property test asserts that the read makes no other source write.

The ruling's `docs/slo.md` text is on `docs/coordinator-20261003`
(`8dd4546`), not on `main`. Its implementation is open as #157: on `main`
no counter records the `-shm` effect and no property test checks the
conditions.

Composition preserves unique rows on both sides and keeps conflicting
snapshots for explicit resolution. A snapshot is never installed over a
live database.

### 2.9 Typed refusals and closure accounting (S4)

Refusals are values of one taxonomy, grouped by family
(`crates/bulkload-proto/src/refusal.rs`). A refusal crosses the wire and
lands in receipts by its stable code.

Not every refusal is typed enough to act on yet (architecture review,
section 1, problem 3). That review counted, at `727493a`:

- 104 non-test `Io(None)` sites, 35 of them in the SQLite provider code;
- 149 occurrences of `GIT_INVENTORY_MALFORMED` in `git_carry.rs`, because
  `output()` maps any failed Git child to that one code and drops its
  stderr.

On `4a10bb8`, a failed SQLite backup step still maps to `Io(None)`
(`snapshot` in `src/provider_sqlite.rs`). `closure.rs` still reads a refusal
code back out of the receipt's Display text (`split_whitespace`), instead of
matching a typed value. Such refusals name no cause that a disposition could
act on, so they are a known S4 gap. WP3 PRs 1–3 address them.

`closure-report` is bulkload's own completion gate
(`crates/bulkload-agent/src/closure.rs`;
[`docs/design.md`](../design.md), "Completion"). It reads the plan, the
corpus's capture records and the apply ledger, and ends every planned item
as one of:

- `applied`;
- `referenced-only`;
- `refused` with a typed code;
- `unaccounted`.

A bare `IO` or `FRAME_CODEC` names no cause, so it never closes an item. The
native `verdict` passes only when `unaccounted` is 0. An attestation ledger
can close natively unaccounted items in a separate block. It must be bound
to the plan, the source label and each item's current capture digest, and it
never overrides a native record (#133).

S4 is not fully met by this. The charter also requires an operator-reviewed
disposition (accept, re-carry or abandon) for every typed refusal before a
run counts as complete. That disposition ledger, and closure over file and
SQLite items as well as Git items, are WP3 work (section 7).

### 2.10 Drift custody (S5)

A capture pass tolerates refs and worktree seats moving under it. What moved
is recorded, never absorbed ([`docs/design.md`](../design.md), "Drift";
R-N30, R-N72):

- The pass window runs from the pre-pass key to the post-pass key. A capture
  is clean only when the pre-pass key, the export's own before and after ref
  inventories, and the post-pass key all agree.
- **Export drift**, which moved under the export itself, leaves the
  drifted seats' bytes out of the bundle and marks it in band. Every restore
  and import verb refuses a marked bundle (`CAPTURE_DRIFTED`) before it
  writes anything.
- **Key drift**, which moved only outside the export's window, leaves a
  coherent snapshot that applies.
- A drifted capture records a poisoned key that no census hashes to. It is
  never a reuse hit, even if its sidecar is lost.
- The next pass extends it (`capture-extended-from-drift`).
- **An object-store rewrite is drift too** (WP1). The export reads the
  source's pack listing with its authority. Suppose a Git child of the pass
  fails and that listing has changed: a `gc`, `repack` or `prune` raced the
  pass through the private repository's `alternates`. Then the item is
  `deferred-with-drift`, with one `ObjectStoreRewritten` row and no capture
  record, and the next pass captures the rewritten store. The same failure
  under an unchanged listing still refuses
  (`an_object_store_rewrite_under_the_pass_is_drift_custody` in
  `src/estate.rs`).

On the file path, a seat rewritten during its read is refused
`SOURCE_CHANGED_AFTER_SNAPSHOT` for that seat alone. Its bystanders are
carried with correct ledger rows, and the victim gets no row (`live_writer`
scenarios in `crates/bulkload-agent/tests/fault_harness.rs`). That covers
in-place overwrite, truncation, rename-replace, and a same-size rewrite with
its mtime restored.

**What still refuses on `main`, and what covers it.** Landing #38 alone
does not complete S5:

- HEAD and index movement inside a captured worktree refuse
  `GIT_AUTHORITY_CHANGED`. OI-1003-Q11 (#38) makes them drift classes
  (`captured-with-drift`); that is WP5 PR 1. Whether a nest's HEAD counts as
  "inside a captured worktree" is open decision D2 of the property-test
  plan.
- Configuration, the shallow frontier, nested worktree custody and the
  omitted rebuildable roots moving under a pass also refuse
  `GIT_AUTHORITY_CHANGED` (R-N30; [`docs/design.md`](../design.md),
  "Drift"). No ruling or work package yet makes them drift classes. An agent
  that rewrites `.git/config` while a pass reads that repository still makes
  the item refuse.
- Directory-shape drift refuses fail-closed: a directory removed, a
  directory replaced by a file, or a file replaced by a symlink. No work
  package yet names it.
- On the file path, a seat that vanishes before it is opened surfaces as a
  bare `IO` refusal, not as drift. WP5 PR 2 adds typed `Vanished` and
  `ChangedDuringRead` outcomes (architecture review).

## 3. Invariants and why each matters

### 3.1 R25: never re-read durable bytes or unchanged seats

**Statement** (R-N58; [`docs/design.md`](../design.md), "Performance"):

- a byte the destination already holds durably is never read again;
- a seat whose stat identity is unchanged, and was not racy when recorded,
  is never read again.

The stat identity is the source authority, device, inode, size, and mtime
and ctime at nanosecond precision.

**Why it matters.** R25 is what makes a migration a rerun instead of an
event. If a resume re-reads what it already carried, its cost grows with the
estate, not with the change. An operator then has a reason to schedule the
run, and to fear interrupting it.

**How it is held.** Three mechanisms hold it:

- `Held` gates every ledger commit (section 2.4);
- `Reuse` answers unchanged identities (section 2.2);
- `WantManifest` lets the destination fill from its own verified chunks.

The racy rule (section 2.6) is the deliberate exception: it trades one
re-read for correctness.

**Limits.** A stat identity is a reuse key, not a content digest. Changed
files, replaced inodes, journal gaps and lost source authority invalidate
reuse ([`docs/design.md`](../design.md)). The racy reference is the
capturing host's wall clock, not the filesystem's. A source on NFS or SMB
whose server clock runs more than 2 s behind the capturing host can defeat
the guard; one that runs ahead only makes more captures racy.
`docs/design.md` now states this as a known limit ("Known limit: clocks",
added by #154). It advises keeping such sources NTP-synchronised with the
capturing host, or carrying them from a host where they are local. The
counters
that make R25 measurable on the file path (`source_bytes_read`,
`transferred_content_bytes`) do not see reads made by Git child processes.
Since WP2 PR 1, the Git children that pack a capture are counted from their
own resource usage, as a lower bound that misses page-cache hits. Other Git
children remain uncounted (WP6 PR 1; section 5.4).

### 3.2 Durability ordering

**Statement.** A record is never committed before the bytes it describes are
durable on the destination ([`docs/design.md`](../design.md),
"Durability").

**Why it matters.** Durability ordering and R25 are coupled. Under R25 a
committed record forbids a later read. So a record that outlived its bytes
after a power loss would turn a recoverable crash into permanent, silent
loss: the resume would trust the record and never fetch the bytes again.

**How it is held.** The group commit seals data before records
(section 2.5). Directory creation is bound to its inode before it is named
(R-N102). On Darwin the protocol relies on barrier ordering for files and
one draining commit per group. Barriers are modelled device-wide by default
(R-N103), with a stricter per-object model available. How it is proven is
in section 4.

### 3.3 S2: source safety

**Statement** (paraphrased; the normative text and the budget are S2 and
WP0(b) in [`docs/slo.md`](../slo.md)). S2 requires that bulkload take no
lock on a source repository, write nothing to the source, signal no
process, and run at background priority (OI-1003-Q5, Q9). Both stated
exceptions are SQLite's: the backup's shared read lock (OI-1003-Q16), and
the backup read's creation or touch of the `<db>-shm` wal-index
(OI-1003-Q36, which extends Q16 on conditions; section 2.8). Q36 is
recorded on Linear TIN-4543, and its `docs/slo.md` text is on
`docs/coordinator-20261003` (`8dd4546`), not yet on `main`. This section
says how much of that holds today.

**Why it matters.** The source is a live workstation. A mover that takes
`index.lock`, triggers `gc`, runs a hook, fetches from a promisor remote, or
competes for I/O at normal priority interrupts the agents it was meant to
leave alone. Any of these makes the migration an event again.

**What held before WP1.**

- The file walk reads through descriptors beneath the root, with
  `O_NOFOLLOW` at every component, and uses `pread`.
- A power-loss trace test checks that every write a `copy` makes lands in a
  file the trace itself created, and that no source chunk pack exists
  (`the_source_writes_no_content_bytes` in
  `crates/bulkload-agent/tests/power_loss.rs`). This is the R-N58
  no-source-pack check, not a full S2 proof: it does not trace the SQLite
  stores, and it would not see a node newly created under the source root.
- The estimate's hardening is tested
  (`probe_and_source_git_calls_are_hardened` and
  `partial_clone_source_never_fetches_and_upstream_tip_is_not_a_have` in
  `src/git_carry/estimate.rs`).
- After the cohort-1 estimate run (2026-09-24, 03:45Z to 03:48Z), a `find`
  of every repository's Git directory showed no writes on sting and, on neo,
  only another session's commit
  ([estimate](../evidence/git-carry-estimate-2026-09-23.md)).

**What WP1 delivered, on `main` since `adb9c66`** (PR #145;
[`docs/design.md`](../design.md), "Live union", "Source safety (S2, WP1)";
[agent note](../agent-notes/2026-10-03-wp1-s2-source-safety.md)). The
architecture review had found S2 held partly by convention
([review](../plans/2026-10-03-architecture-review.md), section 1,
problem 1). WP1 closed these parts of that finding:

- **One hardening table** feeds every Git child of the v1 carry and the
  estimate (section 2.7). It adds `GIT_NO_LAZY_FETCH`, no optional locks,
  `maintenance.auto=false`, the C locale and a discovery ceiling to v1,
  which lacked them before WP1.
- **Partial-clone sources** refuse `GIT_SOURCE_PARTIAL_CLONE` before any
  other read, in `git-export` and `estate-capture`. Tests:
  `a_partial_clone_source_is_refused_before_any_read` (`git_carry.rs`) and
  `a_partial_clone_item_refuses_before_its_key_reads_it` (`estate.rs`).
- **Background priority by default.** It covers `serve`, `estate-capture`,
  `snapshot`, `git-carry-estimate`, `git-export` and `copy`, entered before
  any thread or child exists. On Linux that is nice 19 and the idle I/O
  class. On Darwin it is `IOPOL_THROTTLE`, QoS background and nice 19.
  `--priority=normal` is the explicit, recorded opt-out. Every counters line
  and the bench header record the class. An io test checks that threads and
  children inherit it
  (`background_priority_is_inherited_by_threads_and_children` in
  `src/io/tests.rs`). When WP1 merged, its Darwin code had not been
  compiled for Darwin or run on neo (agent note; #104).
- **Overlap first.** `serve` and `copy` refuse a state root that overlaps
  the source (`SNAPSHOT_ROOTS_OVERLAP`) before any store is created.
  Previously `serve` opened its store first.
- **A source-census property.** P-S2
  (`a_run_leaves_the_source_lstat_census_unchanged` in
  `src/transfer/tests.rs`) runs over generated trees. A copy, its warm rerun,
  and a copy refused for a state root inside the source must leave the
  source unchanged. Every source node keeps its lstat identity: mode, size,
  mtime, ctime, inode, link count and owner. The node set stays the same,
  and the refused run creates nothing. This is the S2 evidence for the file
  path today. It does not cover the Git carry or the SQLite capture.

WP1's priority list is wider than WP0(f)'s list in `docs/slo.md`. A pending
amendment (OI-1003-Q25, recorded on Linear TIN-4543; its text is on
`docs/coordinator-20261003` at `9784467`), not yet in `docs/slo.md` on
`main`, widens WP0(f) to match.

**What is still open:**

- **Typed source access** (OI-1003-Q16). The table is one, but `git()`
  still takes an untyped path and serves source, private and destination
  repositories alike. Typed source, private and destination repositories,
  with a closed allowlist of source read commands, are WP7.
- **The S2 budget sampler** (WP6) is not built, so the latency and load
  budget has never been measured (section 5.3).
- **P34 and P35** in the property-test plan's form are pending. P34 covers
  source inertness over traced copies, carries and estimates, including lock
  events. P35 is a per-thread priority probe.
- **The formal model** has TLC results committed on its branch
  (`3dbfbdb`, not on `main`; section 4). It models the transfer protocol,
  and it cannot prove S2 at the level of the code. It represents Git and
  SQLite source access only as abstract typed reads, and it does not model
  the Git carry itself. Its `S2_TypedSourceAccess` has no `-shm` write: the
  README at `3dbfbdb` leaves the backup's possible touch of `-shm` to P34.
- **The SQLite wal-index exception** (OI-1003-Q36, #157). The WP0(e)
  estate-corpus work reported a probe on 2026-10-03: a read-only, WAL-aware
  open, made the way `snapshot` opens a source, created `<db>-shm` beside a
  WAL database (section 8). Q36 admits that write inside the Q16 exception
  on its conditions (section 2.8). None of them is built yet. #157 is to
  add the counter and record it in S2 evidence, and a P34-family property
  test. Over generated WAL sources, with and without an existing `-shm`,
  that test is to show that the main file and `-wal` stay byte-identical
  and that no other source write occurs.
- **A reported Git-carry source write** (no ruling yet). The WP0(e) lane's
  reviewed evidence (#159 at `378b634`, not on `main`) records a smoke run
  of `estate-capture`, built from `46587af`, over the small corpus. Each
  pass that captured any item moved the mtime and ctime of existing loose
  objects and a pack in source repositories. Their inode, size and bytes
  did not change, so the corpus's own `verify` still passed; a stat diff of
  the source showed it. The lane reads this as Git freshening objects that
  it finds through the private repository's alternates, and leaves it to
  S2 (WP1) for a ruling. TIN-4543 records none yet. P-S2 does not cover the
  Git carry, and the formal model has Git only as an abstract read, so
  nothing on `main` would catch it.

The idle I/O class only helps where the kernel's I/O scheduler honours
priority classes [IoprioSet]. This is why S2's budget is measured, not
inferred (section 4).

## 4. SLOs and how each is proven

The definitions of S1–S5, with every number they set, live in
[`docs/slo.md`](../slo.md). This paper paraphrases them where it must and
copies none of their numbers. This section says which instrument carries
each claim. Five instruments exist or are being built.

- **Property tests** ([plan](../plans/2026-10-03-property-test-plan.md)).
  Example tests are decomposed into stated properties. CI runs them on a
  fixed seed and a bounded corpus; a deep local tier runs random seeds at 20
  times the cases. There is no fuzzing (OI-1003-Q7). The method follows
  QuickCheck [QuickCheck00] through `proptest` [Proptest]. A test is retired
  only when its subsuming property catches the specific mutant the old test
  was written for. On `main` at `4a10bb8`, `proptest!` appears in seven
  places:
  - the slab pool and chunker models (`src/io/buf/tests.rs`,
    `src/io/chunker/tests.rs`);
  - the `carry_v2` random-DAG test (`tests/git_carry_v2.rs`);
  - WP1's P-S2 source-census property (`src/transfer/tests.rs`,
    section 3.3);
  - WP10 PR 1's two properties of the shared child-drain helper,
    `drain_bounded` (`src/child.rs`, #152);
  - WP2 PR 2's P-CHAIN property of auto-prerequisite chains
    (`estate::wp2_chain` in `src/estate.rs`, #146);
  - #154's salvage-bound property,
    `the_salvage_bound_keeps_greedily_within_both_limits`, a second block
    in `src/transfer/tests.rs` (#124, section 2.6).

  WP1 also added `test_support::prop_config`, the plan's fixed-seed helper,
  in minimal form; P-S2, `drain_bounded`'s properties, P-CHAIN and the
  salvage-bound property run through it.
  The plan counted three places before WP1
  ([plan](../plans/2026-10-03-property-test-plan.md), section 0). The
  catalogue is mostly planned work.
- **Formal model.** `docs/slo.md` ("Proof package") asks for a TLA+ (or
  equivalent) specification of wire v5, `Held`, ledger commit and resume. It
  is to be model-checked for R25, durability ordering, and S2's no-write
  and no-lock properties. OI-1003-Q32 (Linear TIN-4543) makes the model a
  hybrid:
  - TLA+ with the TLC model checker [TLA94] [Specifying02] [TLC99] is the
    checker of record;
  - planned for sprint 2: a Dhall typed catalogue will render the TLC
    configurations (`configs.tsv` and the `MC_*.cfg` files), replacing
    today's generator script;
  - planned for sprint 2: an independent breadth-first explorer written in
    Haskell will re-check one core configuration, as an N-version
    cross-check of TLC. It must match TLC's distinct-state count and TLC's
    verdicts on three mutations.

  The TLA+ model is on the branch `docs/tla-model-20261003`, which is not
  on `main`. This section describes `3dbfbdb`, the commit whose results
  this paper was written against. After its review the branch moved on. It
  is open as #160, at `27581be` when this paper last checked it
  (2026-10-04). Its commits `8bc6672` and `7738b6e` change the
  specification, the configurations and the README. This section has not
  yet been re-checked against them; two of their changes bear on the
  results below and are noted where they apply. At `3dbfbdb`,
  `docs/formal/BulkloadTransfer.tla` models:
  - wire v5 per entry, from `Entry` and `Decide` through `Held` and
    `SourceDone`;
  - destination staging, file seal, no-replace publish, directory seal and
    the store's group commit;
  - the digest-only source ledger, committed only on `Held{true}`;
  - crashes of either host or both, and the rerun after them;
  - racy captures, source edits, third-party writes at the destination,
    failed group commits and the space refusal;
  - source access as typed reads, with the SQLite backup's shared read lock
    as the one bounded exception. The model has no `-shm` write, so it does
    not represent OI-1003-Q36's wal-index exception; its README leaves the
    backup's possible touch of `-shm` to P34;
  - variants for WP0(g), the relaxed source ledger, and two candidate
    designs for WP0(d), superseding publish.

  At `3dbfbdb` its safety invariants are `TypeOK`,
  `R25_NoDurableReread`, `R25_NoCommittedCaptureReread`, `ReadOnce`,
  `S3_ReadsOnlyChanged`, `S3_UnchangedReadsZero`, `S3_ClosedPassIsHeld`,
  `RecordImpliesBytes`, `HeldAfterCommit`, `LedgerAfterHeld`,
  `DoneAfterLedger`, `ReuseSound`, `LedgerSound`, `NoClobber`,
  `S2_TypedSourceAccess`, `S2_BackupLockBounded` and `ClosureAccounted`.
  `RunsClose` and `AllRunsFinish` are its liveness properties.
  `WithinBudget` is a wall-clock budget on a TLC run, not a property of the
  protocol. The branch's README freezes these names. Each negative
  configuration breaks exactly one rule and must produce a counterexample.

  **TLC results** (committed at `3dbfbdb`, in `docs/formal/README.md`). The
  run of record was one full `just tla-check` on sting on 2026-10-04, with
  TLC 2.19. It ran over the specification and configurations of
  `3760263`; the specification is unchanged at `3dbfbdb`. All 35
  configurations gave their expected outcome:
  - 9 positive configurations finished with no property violated.
    `MC_main` (two seats, two runs, one crash, one source edit, with
    symmetry over seats) explored 869,296 distinct states. The N-version
    core, `MC_nv_core`, has 15,834.
  - 24 negative configurations each violated exactly the one property
    they name, so every mutation was caught.
  - `MC_live` satisfies `RunsClose` and `AllRunsFinish` under weak
    fairness of the protocol, with no crash and a source that stops
    changing. Without that fairness, `RunsClose` fails.
  - `MC_main_sim` is a random simulation with larger constants. It is
    evidence, not a model-checking result.
  - The budget self-test is expected to be inconclusive. It shows that the
    wall-clock budget can end a run.

  These results hold within small bounds: two seats with two runs, or one
  seat with three runs and two crashes. TLC found no violation within
  them. That is not a proof for every estate size.

  The README at `27581be` supersedes this run of record. It reports a later
  full run, over `8bc6672`'s specification, in which all 43 configurations
  met their expectation. It gives the reason: the earlier `MC_wp0g`
  configuration used `MC_main`'s bound, under which the source never
  consulted its ledger (WP0(g) below).

  The results carry two findings on open rulings:
  - **WP0(g).** With relaxed ledger rows, `MC_wp0g` and `MC_wp0g_deep`
    pass. `MC_wp0g_authority` also relaxes the commit that creates the
    source store's authority, and it fails `R25_NoDurableReread`. The
    README concludes that WP0(g) holds for the ledger's row commits only
    if that creation commit stays durable, and it lists further
    conditions. Both stores still run at full durability in the code, so
    WP0(g) is not implemented. The review after `3dbfbdb` found that
    `MC_wp0g`'s pass there was no evidence, because at that bound the
    relaxed ledger was never read. At `27581be` the configuration has a
    new bound, a reachability configuration shows that the relaxed-only
    path is explored, and the condition is wider: the source store's whole
    creation, its state root's directory entry included, must be durable
    before the model's `Start`. That README also notes that the model
    assumes a ledger commit never fails, which the code does not yet
    honour.
  - **WP0(d).** The exchange design satisfies `NoClobber`; the
    check-then-rename design violates it. Superseding publish has no code
    yet.

  What the model cannot show:
  - It is a model of the protocol. It cannot prove S2 at the level of the
    code. `S2_TypedSourceAccess` holds for the model's abstraction of
    source access, not for the binary.
  - It does not cover the Git carry. Git carry v1 and v2, the ingest
    journal and estate apply are outside it (the specification's header
    says so). Git appears only as one abstract typed read. S3's Git half is
    not modelled.
  - Background priority is not a property of the model. The specification
    leaves it to P35 and the S2 budget.
  - `docs/slo.md` makes WP0(g) conditional on the model. The model's
    verdict is itself conditional, and the branch is not yet reviewed onto
    `main`. TIN-4543 records no ruling yet on adopting WP0(g) under those
    conditions.
- **Crash and power-loss proofs.**
  - The W7 fault harness crashes a real `copy` with `_exit` at each fault
    point. After each crash it checks four invariants:
    - **I1:** a record implies its bytes;
    - **I2:** no partial leaf exists under a final name;
    - **I3:** a committed file costs 0 source reads on resume;
    - **I4:** no temporary survives.

    Source: `crates/bulkload-agent/tests/fault_harness.rs`.
  - A process crash leaves the page cache intact, so that harness cannot see
    a missing flush. The R-N88 power-loss checker
    (`crates/bulkload-agent/src/io/crash_check.rs`) closes that gap. Like
    ALICE [ALICE14], it records the syscall trace of a real `copy` and
    reasons over the crash states an abstract persistence model allows.
    Unlike ALICE, which constructs selected states (section 6), it
    enumerates up to a bound. At each crash point with at most 12 optional
    mutations (`exhaustive_limit`), it tries every subset of them and
    discards the illegal ones. Above that bound it tries only a bounded
    family: the prefix state, the required set, every drop-one and every
    keep-one state ("Enumeration bound" in `crash_check.rs`). A bounded
    point fails the check unless the caller accepts it. The real-copy test
    accepts bounded points, because a real copy's trace is long
    (`accept_bounded` in `tests/power_loss.rs`). Each bounded point is
    listed in the report. So the real-copy proof is exhaustive up to the
    bound and covers only that bounded family above it (section 8). Each
    state checked must satisfy "committed implies durable", "captured
    implies held", "no torn final names" and "returned implies complete"
    (`tests/power_loss.rs`). The Darwin rules model
    `F_FULLFSYNC` as a drain of the whole device queue, and
    `F_BARRIERFSYNC` as ordering without durability, as Apple's `fcntl(2)`
    describes them [AppleFcntl]. Apple's guidance points apps that need a
    write barrier to `F_BARRIERFSYNC`, and reserves `F_FULLFSYNC` for apps
    that need a strong expectation of persistence [AppleDiskWrites].
  - Two directory resume proofs (#74) cover adoption of a directory
    created on the R-N119 fallback path, where no no-replace rename exists,
    and of a directory bound to its record
    (`materialize::adoption_power_loss`, run by `just resume-power-loss`).
    They also accept bounded crash points (`accept_bounded` in
    `src/materialize/adoption_power_loss.rs`).
  - All of these are in the mandatory `check-fast` tier and never move to
    the optional tier (`AGENTS.md`, "Validation").
- **The R23 gate (a) harness.** `r23_ab.py` (`just bench-r23-ab`) builds the
  candidate B and the baseline A and runs the order B/A/B/A/B. Each rep is a
  full R23 bench against rclone. B passes only if every B rep passes; A is
  informational (OI-1002-Q30). A sample is gated only on AC power with
  1-minute load under 2.5, and every row records both (R-N81). Other lanes
  are held quiet (R-N91). The harness aborts a sample on any precondition
  failure or corpus mismatch
  ([harness note](../agent-notes/2026-10-02-r23-harness.md)). It runs on the
  deterministic R23 corpus v1
  ([corpus evidence](../evidence/r23-corpus-v1-2026-10-02.md)).
- **The estate-shaped corpus.** WP0(e) (OI-1003-Q19) adds a deterministic,
  sealed generator of Git-heavy, many-small-file trees. S1 is then measured
  on it in addition to R23's 23-file corpus. It is open as #159
  (`feat/wp0e-estate-corpus-20261003`), not on `main`.

| SLO | Instrument | State at `4a10bb8` |
|---|---|---|
| S1 gate (a) | R23 A/B harness, R-N81 gating, corpus v1 and the estate corpus | **Not met.** The last completed sample (2026-09-18, pre-wire-v5) failed on the initial copy. Wire v5 has no gated sample. |
| S1 gate (b) | A remote pull-vs-rclone-over-sftp arm (WP6) | Not built. Pending gate. |
| S2 properties | P6, P-S2, P34 and P35 property tests; formal model (protocol level only); existing trace and hardening tests | Partial: WP1 on `main` (hardening table, partial-clone refusal, background priority, overlap-first), with P-S2 for the file path (section 3.3). Typed source access (WP7), P34 and P35 pending. In the model, `S2_TypedSourceAccess` and `S2_BackupLockBounded` hold in `MC_s2` at `3dbfbdb`, at the protocol level only, on a branch not on `main`; the model has no `-shm` write. The `-shm` wal-index write is ruled admissible on conditions (OI-1003-Q36, TIN-4543; `docs/slo.md` text on the coordinator branch, not on `main`); its counter and property test are pending (#157). A Git-carry source write (object freshening through alternates) is reported on #159's branch, with no ruling yet (section 3.3). |
| S2 budget | An S2 sampler of a reference workload's p95 latency and load1 (WP6) | Not built. Pending gate. |
| S3 zero reads | Counters per run; P21, P23 and P32; fault-harness I3; formal model (file transfer only; S3's Git half is not modelled) | Partial. Holds on the file path in every recorded (pre-wire-v5) bench run (section 5.4). Git carry unmeasured in any run. Since WP2 PR 1 its pack children are counted, as a lower bound; other Git children are not (WP6). WP2 PR 2's chains are on `main`, measured on fixtures only. Since #154 a store from before the racy guard has its rows invalidated once, counted (#125). In the model, the R25 and S3 invariants hold within its bounds at `3dbfbdb`. |
| S3 rerun ratio and delta inequalities | Bench `s3_ratio` verdict and P-S3-delta (WP6) | Not recorded. Pending gate. |
| S4 | Native closure report and attestation; disposition ledger (WP3); P8 and P61 | Not yet provable. Closure gate on `main`; disposition ledger pending. |
| S5 | Drift custody; P29 and P39 | Ref, seat and object-store drift on `main`. HEAD and index pending (#38, WP5). Configuration, shallow frontier, nest custody, rebuildable roots and directory shape still refuse, with no work package yet. File-path vanish is bare `IO` (WP5 PR 2). See section 2.10. |
| Durability ordering | Fault harness I1–I4, R-N88 checker, R-N119 proofs; formal model (`RecordImpliesBytes`, `HeldAfterCommit`, `LedgerAfterHeld`) | In `check-fast` and PR CI; #154's and #153's source, build, test and fault-harness checks passed before they merged as `4a7b86b` and `4a10bb8`. #153 retargeted the recorded crash-check tests at the production no-replace publish, on both its rename and its link-and-unlink path (R-N119). The real-copy and adoption power-loss proofs accept bounded crash points. In the model, these invariants hold within its bounds and their mutants are caught (`3dbfbdb`, not on `main`). |

## 5. Results

This section reports only what [`docs/evidence/`](../evidence/) records,
with dates. No number below is rounded, extrapolated or combined from two
files. Numbers recorded before 2026-10-02 used the R23 corpus of record,
which was lost that day. Their absolute times cannot be compared with any
sample on corpus v1 ([corpus evidence](../evidence/r23-corpus-v1-2026-10-02.md),
"Baseline break"). Every timed result also predates the agent's move to
wire v5 (W4 PR 2, 2026-10-01,
[note](../agent-notes/2026-10-01-w4-pr2-v5-migration.md)).

### 5.1 S1 gate (a): not met

- **2026-09-18, R23 fail**
  ([r23-2026-09-18](../evidence/r23-2026-09-18.md)). Native initial-copy
  median 3015.294 ms against rclone 601.010 ms: **fail**. Native 1 %-delta
  median 58.842 ms against rclone 127.064 ms: pass. Both comparisons are
  mandatory, so the gate failed. This run used the predecessor bench binary
  at revision `c6cead96f325+worktree-d7f9f8d33baf0e99`, with rclone v1.75.0,
  3 reps and an unflushed cache. The file records no power or load fields.
- **2026-09-23, M0 measurement on TinylandState**
  ([m0-2026-09-23](../evidence/m0-2026-09-23.md)). This was not a gate
  sample: n = 3, run-order drift, other lanes compiling (load1 3.8 to
  22.2), power not logged. Medians:
  - single-file, single-flush floor: 1005.481 ms;
  - rclone as shipped: 1404.463 ms;
  - rclone `--local-no-clone`: 1487.520 ms;
  - native at `b320e4b`: 8558.612 ms.

  The audit found that rclone as shipped clones every file on that APFS
  volume and spends its time on MD5 verification. It writes no data bytes.
  The corpus-shaped durable floor was left pending. M0's in-bench run of the
  `b320e4b` binary printed `verdict status=fail`: it lost the initial copy
  and won the 1 % delta.
- **Wire v5.** No gated sample of the wire v5 engine exists in
  `docs/evidence/`. Section 7 lists the 2026-10-03 attempts, none of which
  produced a verdict.
- **2026-09-23, W3 engine improvements (informational; native still slower
  than rclone)** ([w3-2026-09-23](../evidence/w3-2026-09-23.md)). Every
  sample was on battery at load1 3.2 to 21, so none was gated (R-N81).
  - Destination BLAKE3 fell from 4.00 × P to 1.00 × P.
  - File reads fell from 4.00 × P to 1.07 × P.
  - Full flushes were 10 for 8 groups.
  - The native initial median improved on the `c3f0b02` baseline:
    2,133.031 ms (run 1) against 8,391.591 ms, under the same host
    conditions.
  - Native still lost the initial copy to rclone in every W3 run. Initial
    medians, native against rclone:
    - run 1, group mode: 2,133.031 ms against 1,764.784 ms;
    - run 2, group mode: 2,719.378 ms against 2,683.015 ms;
    - strict mode: 2,298.498 ms against 1,629.643 ms.
  - The `c3f0b02` baseline's own run printed `verdict status=fail`. Its
    initial medians were 8,391.591 ms native against 2,573.792 ms rclone.
  - The < 1.5 s target was not met.

### 5.2 S1 gate (b)

No cross-host gate sample exists. M0 measured the neo → sting tailnet link
on 2026-09-23 at a 28.34 MB/s single-stream median in that session, with
four streams adding nothing (26.36 MB/s aggregate). M0 also notes an
earlier session recorded 6.7 MB/s. The link rate varies between sessions,
so it must be measured beside every cross-host sample
([m0-2026-09-23](../evidence/m0-2026-09-23.md), "Link calibration").
**Pending gate.**

### 5.3 S2 budget

No run has measured S2's p95-latency and load1 budget
([`docs/slo.md`](../slo.md), S2). **Pending gate.**

### 5.4 S3 and R25

- **2026-09-18:** every unchanged native warm sample and the injected
  interrupted resume reported `source_bytes_read=0` and
  `transferred_content_bytes=0`. The 1 % delta, against a workload of
  2,426,057 changed bytes, read 3,280,458 source bytes and transferred
  2,444,912 bytes ([r23-2026-09-18](../evidence/r23-2026-09-18.md)).
- **2026-09-23:** warm and interrupted resumes read and transferred 0 bytes
  in every M0 and W3 sample
  ([m0](../evidence/m0-2026-09-23.md), [w3](../evidence/w3-2026-09-23.md)).
  After W3 removed the destination byte pack, the delta transfer equalled
  the source read (3,280,458 B). The bench deletes the mutated destination
  files before the delta phase, so their unchanged chunks are sent again.
  W3 records this as consistent with R-N58, because the destination no
  longer held those bytes.
- **The rerun ratio** (S3's bound on an unchanged-estate rerun's wall-clock
  as a share of the first pass's; [`docs/slo.md`](../slo.md), S3) has not
  been recorded as a verdict by any run. **Pending gate.**
- **Scope.** All of the above measured the file mover, and every sample
  predates wire v5. The 2026-09-18 sample came from the predecessor bench,
  `tcfs-bulkload-bench`. No run has measured S3 on the Git carry path:
  - since WP2 PR 2 (#146), a changed rerun packs only what is new since
    the previous retained capture, up to a chain depth of 8. That is
    measured on fixtures only (section 2.7);
  - WP2 PR 1 (#144) now counts the storage reads of the Git children that
    pack a capture, from their own resource usage, and the metadata
    censuses (`census_walks`). The pack-read counter is a lower bound: it
    misses page-cache hits (`src/counters.rs`). Reads by other Git
    children are still uncounted (WP6 PR 1);
  - before WP2 PR 1, four declared S3 counters were never incremented
    (architecture review, section 0). #144 wired them or deleted them, and
    a source scan now refuses a counter that nothing increments
    (`every_counter_is_incremented_somewhere`).

  No evidence file yet records these counters on an estate run.

### 5.5 Estate operations

- **2026-09-22, cohort 3**
  ([cohort3](../evidence/cohort3-index-repair-20260922.md)). Of 73 plan
  items, 66 were captured and 7 refused `GIT_INVENTORY_MALFORMED`. The pull
  moved 152 corpus files (5,864,514,759 bytes) in 7m55s, sha256-identical to
  neo. The repair step:
  - repaired 63 items;
  - refused 3, where the destination lacked bundle prerequisites;
  - skipped 7 that had no bundle.

  In 36 non-cohort worktrees of the same repositories, the index identity
  and HEAD bytes were identical before and after. This predates wire v5 and
  the current closure gate.
- **2026-09-23/24, cohort-1 Git estimate**
  ([estimate](../evidence/git-carry-estimate-2026-09-23.md)). Across 26
  repositories, the set to carry from neo to sting was 5,415 objects. Its
  thin pack was 9,758,779 bytes, against 1,475.90 MB of history on disk
  (informational). This is the measured gap behind the WP0(a) ruling.
- **2026-09-24/25, carry_v2 on fixtures**
  ([sender](../evidence/w6-m1-sender-2026-09-24.md),
  [ingest](../evidence/w6-m1-ingest-2026-09-25.md)).
  - At the default segment cap, the sent set equalled `upload-pack`'s on
    every fixture and on 12 random DAGs, at a ratio of 1.0000.
  - A crash in segment k re-sent only segments ≥ k, at seven crash points.
  - Nothing ran on an estate repository. The engine is now frozen
    (section 2.7).

## 6. Related work

Each entry says what the system does, then what bulkload borrows or rejects,
and why.

**rsync** [Rsync96] [RsyncMan]. The receiver sends weak rolling checksums
and strong checksums for fixed-size blocks of its old file. The sender
slides the weak checksum over its own file to find matching blocks, and
sends only literal data and block references. By default, files are selected
by a quick check of size and modification time. *Borrowed:* the side that
already holds bytes should describe them before content moves.
`WantManifest` and `NeedChunks` invert rsync's roles: the destination fills
from verified local chunks and asks for the rest. *Rejected:*
- per-run re-derivation: rsync keeps no durable record of what the
  destination holds, so its skip decision is not tied to bytes the
  destination holds durably, as R25 requires;
- fixed block grids;
- the size-and-mtime quick check, which cannot see a same-size rewrite in
  the same tick. bulkload's identity also includes ctime, so a rewrite that
  restores its mtime still changes it (the `live_writer` scenario with the
  mtime restored). The racy rule covers the same-tick case.

**rclone** [Rclone]. A multi-backend file mover. It transfers whole files,
because its object-store backends cannot patch part of an object. It skips
files by size and modification time or by checksum. On APFS, local-to-local
copies clone by default unless `--local-no-clone` is given. *Borrowed:*
rclone is the performance bar (R23), with its exact flag set recorded in
every sample. *Rejected:* whole-file granularity, and no Git or SQLite
awareness. Note for readers of gate (a): on the APFS gate volume, rclone as
shipped clones and verifies rather than writing data (M0, section 5.1). The
gate still compares against it, as R-N62 asks.

**Git's pack protocol and bundles** [GitPackProto] [GitPackObjects]
[GitBundle]. In the pack protocol, the client lists objects it wants and
objects it has, and the server builds a pack of only what is missing. A thin
pack also omits delta bases the receiver holds; `index-pack --fix-thin`
completes it. A bundle carries refs and objects for offline transfer. A
bundle made from a revision range names prerequisites the receiver must
already hold. *Borrowed:* Git's own object model and tools do all packing,
and bulkload never re-implements them. The estimate and `carry_v2` pin
`pack.useSparse=false` and `pack.useBitmaps=false`, so their object counts
are exact. `upload-pack` is the exactness oracle. *Deferred:* negotiated
thin packs over the bulkload wire (`carry_v2`) are frozen by WP0(a). The
cheaper fix is v1 bundles whose prerequisite is the previous retained
capture's tips. It is on `main` (#146), and it is to be measured on the
estate corpus first.

**Git's racy index** [RacyGit]. Git treats an index entry as racily clean
when its cached `st_mtime` is the same as, or newer than, the index file's
own timestamp. It then compares the entry's content with the recorded
object, so a same-tick rewrite is not mistaken for clean. *Adapted*, not
borrowed directly, as the racy-capture rule (section 2.6). bulkload has no
index file to compare with. It compares a seat's mtime and ctime with
wall-clock reads taken before the open and after the final stat, with a
2 s allowance. Instead of re-checking content, it records nothing for a
racy capture, so the next run reads the seat again.

**casync and desync** [Casync17] [Desync]. casync cuts a serialized tree or
block image into content-defined chunks with a buzhash rolling hash, stores
them in a chunk store named by digest (SHA-512/256 by default, SHA-256
optional, per the repository README), and describes the image with an index
file. desync implements the same formats in Go, with parallel chunking and
more store backends. *Borrowed:* a chunk index as the manifest, and clients
fetching only the chunks they lack. *Rejected:*
- a chunk store as an intermediate: bulkload publishes usable files
  directly into a home directory;
- whole-image serialization, which assumes a quiescent source tree. bulkload
  works seat by seat on a live tree.

**restic and borg** [Restic] [Borg]. Content-defined-chunking backup tools
that store content-addressed chunks in a repository. restic uses
Rabin fingerprints with chunk sizes between 512 KiB and 8 MiB, and SHA-256
ids. borg uses buzhash, and identifies each chunk by a cryptographic hash
or MAC. borg keeps a files cache (inode, size, a timestamp and chunk ids) so
it can skip re-reading unchanged files. By default it compares ctime, size
and inode (`--files-cache`, default `ctime,size,inode`). When it saves the
cache, it does not persist entries whose timestamp is the newest it saw in
that run (`LocalCache.commit` in borg's 1.4 maintenance branch) [Borg]. That
is a racy-entry guard of its own.
*Borrowed:* borg's files cache is the same idea as R25's stat-identity
reuse, and its newest-timestamp guard is close kin to bulkload's racy rule.
What bulkload adds:

- mtime and ctime together in one key;
- the device and the source authority in the key;
- a racy window measured against the capturing host's wall clock (2 s);
- a row committed only after the destination answers `Held{true}`.

*Rejected:* a backup repository as the destination. The
destination must be a working home directory (completion means daily work on
sting), and a separate restore step would write everything twice.

**bup** [Bup]. A backup tool that writes Git packfiles directly, splits large
files with a rolling checksum, and keeps a separate index of file metadata,
so it can tell which files changed without reading them. *Borrowed:* Git's
storage is the right container for Git-like data, and change detection
belongs in a metadata index kept apart from content. *Rejected:* storing the
estate in a backup repository, for the same reason as restic and borg.

**Unison** [Unison04] [FileSync98] [UnisonRepo]. A bidirectional file
synchronizer. It keeps an archive of each replica's last synchronized state,
propagates non-conflicting changes, and surfaces conflicts instead of
resolving them silently. Its behaviour has a formal specification. *Borrowed:*
- conflicts are kept for explicit resolution: SQLite composition keeps
  conflicting snapshots, and Git union preserves divergence;
- a formal specification is part of the product, not an afterthought.

*Rejected:* replica reconciliation. Each bulkload run moves data one way,
and both hosts' unique state is preserved by union at the item level.

**ZFS send and receive** [ZfsSend]. Block-level streams of a snapshot,
incremental between two snapshots, and resumable from a token after
interruption. *Borrowed:* resumable transfer from a durable token is the
model for bulkload's ledger and journals. *Rejected:* it requires the same
filesystem and snapshots on both ends. The lab's source is APFS (neo) and
its destination XFS (sting) ([m0](../evidence/m0-2026-09-23.md),
[corpus v1](../evidence/r23-corpus-v1-2026-10-02.md)). bulkload also takes
no source snapshot, by design.

**LBFS** [LBFS01]. A network file system that cuts files at content-defined
boundaries (Rabin fingerprints) and indexes the chunks of files it already
holds, so it avoids sending data the other side has. *Borrowed:* the
destination's chunk hints. For each digest, the destination records every
published output holding it, newest first. A hint is re-read and re-verified
before use ([`docs/design.md`](../design.md), "Hints").

**FastCDC** [FastCDC16] [FastCDC20]. Gear-hash content-defined chunking with
normalized chunk sizes, much faster than Rabin-based chunking at similar
deduplication. *Borrowed directly*, through the `fastcdc` crate's 2020
variant. M0 measured FastCDC slice cutting at about 1.4–1.5 GB/s per thread
on neo on 2026-09-23, under contention
([m0](../evidence/m0-2026-09-23.md), "CPU micro-benchmarks").

**BLAKE3** [BLAKE3]. A cryptographic hash built as a Merkle tree over 1 KiB
chunks, parallel across SIMD lanes and threads. It has hash, keyed-hash and
derive-key modes. *Borrowed:* chunk digests, the `wire_id` schema hash, and
`manifest_root` in derive-key mode with a dated context. *Not used:*
BLAKE3's internal tree as the file manifest (section 2.3).

**Crash-consistency research.**
- *ALICE* [ALICE14] showed that applications often depend on file-system
  behaviour that POSIX does not promise. It records an application's
  syscall trace and, under an abstract persistence model, constructs
  selected crash states (section 3.3 of the paper):
  - each prefix of the system calls;
  - intermediate states of a single call;
  - for each pair of calls A before B, the state with every call up to B
    applied except A.

  Its authors call part of that exploration not exhaustive (section 3.3),
  and the tool not complete (section 3.6). bulkload borrows the
  trace-plus-persistence-model approach (section 4). Its checker differs in
  two ways. First, it enumerates every subset of the optional mutations at
  a crash point up to a bound, and falls back above the bound to an
  ALICE-like bounded family: prefix, drop-one and keep-one. Second, it adds
  Darwin's drain and barrier rules and checks bulkload's own invariants.
- *CrashMonkey and B3* [B3-18] [CrashMonkey19] test file systems by
  simulating power loss under bounded, exhaustively generated workloads.
  bulkload borrows bounding with exhaustive enumeration: the checker's
  exhaustive limit, and the local `crash-sweep` in the property-test plan.
  It does not trace at the block layer, because it tests its own protocol,
  not the file system. Their finding that mature file systems have crash
  bugs is a threat to validity (section 8).
- *Optimistic crash consistency* [OptFS13] separates write ordering from
  durability. bulkload's group mode makes the same split in user space on
  Darwin: a barrier per file for ordering, and one draining commit per group
  for durability.

**TLA+** [TLA94] [Specifying02] [TLC99]. Lamport's temporal logic of actions
and specification language, with the TLC model checker. TLA+ with TLC is
the checker of record for the formal model (OI-1003-Q32; section 4).

## 7. Future work

The architecture review
([`docs/plans/2026-10-03-architecture-review.md`](../plans/2026-10-03-architecture-review.md))
ranks the work. This paper only points at it:

- S2 source safety: WP1 is on `main` (section 3.3). Still to come: WP7 for
  typed source access in the types, WP6's S2 sampler, P34 and P35 from the
  property-test plan, review and merge of the formal model, and #157, the
  counter and property test that OI-1003-Q36's wal-index exception
  requires;
- bounded salvage (#124, OI-1002-Q33) and legacy-row invalidation (#125,
  OI-1003-Q26) are on `main` (#154, `4a7b86b`). The ratified bound values
  (OI-1003-Q24) still need their `docs/slo.md` text to reach `main`, and
  `docs/design.md`'s "unruled engineering defaults" wording to be updated;
- one Git engine with counted, incremental v1: WP2. PR 1 (pack-read and
  census counters) and PR 2 (auto-prerequisite chains, #146) are on
  `main`. The S3 measurement on the estate corpus, and then the choice of
  engine, come next;
- typed refusals and S4 closure over every item kind: WP3 (PRs 1 and 2,
  #150 and #151, open);
- one `sync` verb in place of today's choreography of verbs: WP4;
- drift custody for HEAD and index (#38), and superseding publish under
  WP0(d): WP5;
- S1–S3 measurement (S3 ratio, S2 sampler, remote arm, estate corpus):
  WP6, with the property-test plan. The S2 budget is to be measured against
  a v0 synthetic reference workload (OI-1003-Q34, Linear TIN-4543);
- one durability façade: WP8;
- destination throughput for estate-shaped trees: WP9;
- sprawl removal: WP10. PR 1 (#152) and PR 2 (#153) are on `main`; PR 3
  wires or deletes the slab pool and fused chunker on the gate (a)
  evidence (section 2.1).

Two proof-package items are in progress elsewhere. The formal model is on
`docs/tla-model-20261003`, open as #160 (`27581be` when last checked). This
paper describes its `3dbfbdb` results; the review fixes after that commit
are not yet folded in (section 4). It is not yet on `main`.
Its Dhall catalogue and Haskell explorer are sprint 2 work (OI-1003-Q32).
The estate-shaped corpus is on `feat/wp0e-estate-corpus-20261003`, open as
#159 (`633ff72` when last checked), not on `main`. A gated
gate (a) run on corpus v1, and a first gate (b) run, are still owed.
Linear TIN-4543 records three gate (a) attempts on 2026-10-03, none with a
verdict:

- 01:32Z failed to build, because local builds were disabled on neo;
- 02:01Z was refused at load1 37;
- 19:30Z aborted when the bench's per-arm R-N81 preflight saw load1 2.51,
  over the 2.5 limit, before any timed arm ran.

No evidence file for them exists in `docs/evidence/`, so section 5 does not
count them as results. By OI-1003-Q33 (TIN-4543), the next gate (a) attempt
waits for the full post-train `main`.

## 8. Limits and threats to validity

- **The speed claim is unproven.** No gated S1 sample passes. Every timed
  result predates wire v5, and the corpus they used is gone. The R23 corpus
  has 23 files, so it does not exercise per-entry costs; the estate-shaped
  corpus exists to fix that. The bench deletes delta targets before the
  delta phase (its header records
  `delta_target_preconditioning=remove-mutated-private-targets-outside-timing`),
  so the update path is not timed.
- **The comparator differs by volume.** On APFS, rclone as shipped clones.
  A durable writer is then compared against metadata and hashing.
- **Media durability is assumed, not shown.** M0 notes that `F_FULLFSYNC`
  reaching stable media on the USB enclosure is unproven. Apple's own
  guidance, written for iOS, calls `F_FULLFSYNC` best effort against sudden
  power loss [AppleDiskWrites]. The R-N88 checker tests bulkload's crash
  states under a persistence model. It assumes the file system and device
  honour their documented semantics, and CrashMonkey's results show mature
  file systems sometimes do not [CrashMonkey19]. The device-wide barrier scope
  (R-N103) is a further assumption, with a stricter per-object model
  available.
  Apple's `fcntl(2)` says `F_BARRIERFSYNC` needs hardware support, and
  that Apple SSDs are guaranteed to provide it [AppleFcntl]. The gate volume
  is a USB enclosure (M0), for which no such guarantee is stated.
- **The power-loss proof of a real copy is bounded.** The checker is
  exhaustive only up to 12 optional mutations per crash point
  (`exhaustive_limit`). Above that it tries the prefix, the required set,
  and the drop-one and keep-one states. The real-copy test and the adoption
  proofs pass with bounded points (`accept_bounded`). The report lists
  them, but no evidence file records which points were bounded on a given
  run. A missing ordering that shows only in an unvisited subset would not
  be caught.
- **A process crash is not a power loss.** The `_exit` fault harness cannot
  see a missing flush. That is why the R-N88 checker exists, and it models
  only the destination image plus store commit events: the databases
  themselves are not modelled.
- **Counters are lower bounds.** Flush counts exclude SQLite's and Git's own
  syncs (M0). The file-path byte counters miss reads by Git child processes.
  The pack-child read counter added by WP2 PR 1 misses page-cache hits, and
  on Darwin it is a lower bound even for storage reads. Reads by other Git
  children are not counted at all (section 5.4).
- **S2 is proven only in part** (section 3.3). WP1 made more of it
  structural, but source access is not yet typed, the budget is unmeasured,
  the P-S2 property covers the file path only, and the model checks S2 at
  the protocol level only. One more edge: a read-only WAL connection may
  need to create `-shm` or `-wal` files when they are absent [SQLiteWAL].
  The architecture review listed this as plausible, not demonstrated. The
  WP0(e) estate-corpus work then probed it on 2026-10-03 with the open
  flags `snapshot` uses, but outside bulkload: SQLite 3.51.2 on a copy of
  the corpus's WAL database, and 3.53.1 on an equivalent image. Both
  created `<db>-shm` beside the database. The 3.51.2 probe also showed the
  main file and `-wal` staying byte-identical. The record is in
  `docs/evidence/estate-corpus-v1-2026-10-03.md` on
  `feat/wp0e-estate-corpus-20261003` (`7b75b26` and `ec142cf`), which is
  not on `main`, and on Linear TIN-4543. After its review (#159,
  `378b634`) that file no longer gives those probes. It records instead a
  smoke run on 2026-10-04, with a release agent built from `46587af`, in
  which bulkload's own `snapshot`, through the agent's bundled SQLite
  3.46.0, created `storage.db-shm` beside the source's WAL image. The same
  smoke reports a second, unruled source write by the Git carry (section
  3.3). OI-1003-Q36 admits the `-shm` write on conditions (section 2.8), but
  until #157 lands, no repository test on `main` runs bulkload's own open
  in that state, and nothing counts the write or checks that the main file
  and `-wal` stay byte-identical. The model does not represent the write
  either (section 4).
- **The stat-identity trust model is cooperative.** Reuse trusts stat
  identity and the capturing host's clock. A writer able to forge ctime, or
  a filesystem whose clock lags by more than 2 s, defeats it. Capture
  records and their sidecars are not authenticated; corpus integrity rests
  on 0700 custody ([`docs/design.md`](../design.md), "Drift").
- **Property tests are bounded.** A fixed-seed CI corpus is a regression
  net, not a proof. A formal model proves the model, and TLC checks it
  only within small bounds (section 4). The gap between model and code is
  closed only by keeping both reviewed together.
- **The sample is narrow.** Two hosts, one operator and one estate. The
  `carry_v2` results come from fixtures only.
