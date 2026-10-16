# bulkload: moving a live agent estate between machines

**Whitepaper, refreshed 2026-10-16** (OI-1003-Q155, publishable quality;
first drafted 2026-10-03, revised 2026-10-04 and 2026-10-07). Proof
package item one (OI-1003-Q7). References use keys in square brackets (for
example [Rsync96]); each one resolves in the [bibliography](bibliography.md),
with a note on how it was verified. [Section 8](#8-what-changed-since-2026-10-07)
lists what this refresh changed, and [section 9](#9-open-before-publication)
what still stands between this text and publication.

**Code base.** This revision describes the tree of branch
`feat/s1-stack-20261015` at `f9a0c0e` with `origin/main` at `a8d837a`
merged into it (the merge is resolved and staged, not yet committed). That
tree is everything on `main` through #227, plus the S1 stack that is not on
`main`: #210 (the batched group seal, held for review), the S1 throughput
work (`8a24161`), the S1 levers (`59e5e60`), the clone study and its
exclusion (`9806d36`, `f9a0c0e`) and the directory-finish fold (staged
2026-10-16). OI-1003-Q173 rules that the stack merges on an independent
adversarial review
([merge note](../agent-notes/2026-10-15-s1-stack-merge-main.md)). Where a
statement holds only for the stack and not for `main`, it says so.
`main` has moved on since: #228 (`5beb031`, OI-1003-Q168 and Q169) merged
after `a8d837a` and is **not** in this tree; section 3.3 and 2.11 cite it
by pull request where they mention it.

This paper explains and argues. It is not a source of truth. Where it and a
normative document disagree, the normative document wins:

- [`docs/design.md`](../design.md) is the product contract;
- [`docs/slo.md`](../slo.md) defines S1–S5 and the completion bar, with
  every dated amendment;
- [`docs/formal/README.md`](../formal/README.md) holds every model-checking
  result;
- [`docs/evidence/`](../evidence/) holds every measured result of record.

Four rules keep the paper honest:

- **Every statement about a result cites a file in this tree**: an
  evidence file, `docs/slo.md`, `docs/formal/`, a test file or a dated
  agent note. The exceptions are named where they occur. There are four
  kinds:
  - rig logs on mbp-13 that no file in the tree copies, cited through the
    agent note that records them;
  - the GFTB field receipts, which live outside the repository and are
    cited by path and sha256 (section 5.4);
  - #228, on `main` but not in this tree;
  - CI and issue states read from GitHub on 2026-10-16.
- **Every result carries one of four labels.**
  - *Gated*: enforced on every pull request by `just check-fast` and the two
    CI gates, or a benchmark sample taken under the R-N81 and R-N91 gate
    conditions.
  - *Informational*: measured or checked on record, but enforced by no
    gate. Every TLC run is in this class, because no tier starts TLC. A
    benchmark sample taken with the load gate set aside is
    **informational, under load**, and never of record (OI-1003-Q39, Q50).
  - *Field*: a real run over a real estate, outside any gate. It shows the
    tool working on live data; it decides no SLO (OI-1003-Q97).
  - *Pending*: not built, not run, or not on `main`.
- **The paper quotes no SLO number as its own.** It links
  [`docs/slo.md`](../slo.md) for them, so a dated amendment there cannot
  leave this paper silently out of date.
- **No number is rounded, extrapolated or combined from two files.**

## Abstract

bulkload moves a developer's working estate from one machine to another
while the source machine stays in use. The estate is Git repositories with
linked worktrees, stashes, staged and unstaged work; agent transcripts and
SQLite stores; dotfiles and credentials. The target user runs coding agents
that keep committing, writing transcripts and updating databases during the
move. bulkload's goal is that the move is not an event: it is a sync that
can be rerun at any time. Each rerun should read only what changed, never
read a byte the destination already holds durably, end with every planned
item accounted for, and bring an already landed checkout up to the source
without touching work done on the destination since.

The design has five parts:

- a streaming wire protocol (v6) in which the destination decides, per
  file, whether it needs anything;
- a digest-only source ledger that commits a capture only after the
  destination reports the bytes durable;
- a destination that stages, seals and publishes without replacement,
  commits its records in groups, and supersedes only what it can prove it
  wrote;
- Git-aware and SQLite-aware capture, with typed refusals and drift custody
  in place of silent skips;
- estate convergence: a rerun that updates a checkout bulkload landed only
  when its ledger proves nobody changed it since.

The claims are stated as SLOs (S1–S5 in [`docs/slo.md`](../slo.md)). Their
state, in one paragraph each:

- **Proven and gated.** The crash and power-loss proofs, R25 in both its
  readings on the code, S3 on the file transfer with refused and changed
  seats, the Git pack laws and decision core, S2's source census, command
  registry, lock trace and clone ban, the SQLite source exceptions, and S4
  over a whole run (estate items and transfers) all run in `check-fast` on
  fixed seeds (sections 4 and 5.1).
- **Checked on demand.** Five TLA+ modules are model-checked with TLC within
  small bounds: the transfer, Git carry custody, the SQLite snapshot seat,
  estate convergence and directory records. A Dhall catalogue renders their
  configurations (section 4.2). No tier or CI gate runs TLC.
- **S1 is not met.** The one gated sample of record (2026-10-07) is a FAIL.
  Since then the throughput work and the levers have cut native's initial
  copy on the rig from about 856 ms to about 400 ms, measured only under
  load and never of record, against plain rclone's about 357 ms. The
  ruled admission margin for another gated sample, 0.90x rclone, is not
  reached (section 5.3). A durable copy on that rig cannot take less than
  about 230 to 240 ms, so the margin is narrow (section 5.3). The clone
  fast path is excluded: it writes the source (section 3.3).
- **S2's budget has never been measured.** The instrument judges an
  observation alongside a live run against the source's own load
  (OI-1003-Q160), and #228 on `main` adds a pass schedule fixed in advance
  (OI-1003-Q168). No observation has been run on neo.
- **Field evidence.** A neo → sting carry of 137 GFTB items (2026-10-15)
  closed with 0 unaccounted, and its rerun on the unchanged source read 0
  source bytes and received 0 content bytes (section 5.4). It is one run,
  outside any gate.

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
index, push and pop stashes, append transcripts and write to SQLite. Two
SLOs turn this into requirements. They are paraphrased here and stated
exactly, with their budgets, in [`docs/slo.md`](../slo.md):

- **S2:** bulkload must not interrupt the source: no locks, writes or
  signals on the source, background priority, and a measured latency and
  load budget. The stated exceptions are all SQLite's (section 3.3).
- **S5:** bulkload must tolerate a moving source. What moved is recorded as
  drift custody, not refused wholesale and not silently absorbed. The next
  pass catches up.

There is no freeze window and no "stillness" ceremony. The ceremony was
retired along with the Python engine (`AGENTS.md`, "Repository").
OI-1003-Q152 makes the absence of a freeze window explicit: the full run
closes by converging deltas while the source stays live
([`docs/slo.md`](../slo.md), "Amendment 2026-10-15 (later)").

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
   `applied`, `referenced-only` or a typed, reviewed refusal, and nothing is
   unaccounted (S4).

`tar` adds a sixth gap: a one-shot stream has no resume, so an interrupted
run starts again from the first byte. And none of them can bring a checkout
that already landed up to a newer source without overwriting what someone
did on the destination since; that is section 2.8.

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
  store, Git estate carry and convergence, SQLite capture, closure and the
  crash tooling;
- `crates/bulkload-bench`: the R23 benchmark against rclone, and the
  measurement harnesses under `scripts/`;
- `crates/bulkload-handoff`: the credential-class handoff probes, kept out
  of the agent binary (#152). A source scan checks that no agent source
  names a credential or agent tool as a program to run
  (`crates/bulkload-agent/tests/no_credential_tools.rs`).

Three rules shape the code:

- **The dependency wall.** The agent may not depend on tokio, opendal,
  reqwest, ring or tonic. A test enforces it
  (`crates/bulkload-agent/tests/dep_graph.rs`).
- **Refusals are values**, never panics, under a lint wall in each crate
  (R33).
- **Unsafe-first** (R-N54): raw syscalls, zero-copy and reused buffers are
  the default design choice. The engine uses raw syscalls
  (`src/io/sys_*.rs`). Zero-copy reaches one place: in a local `copy` the
  source hands each chunk's buffer to the destination in memory (section
  2.2). The slab pool and fused chunker (`src/io/buf.rs`,
  `src/io/chunker.rs`) are still compiled under a non-test
  `allow(dead_code)` whose reason defers wiring or deleting them to the
  gate (a) evidence (`src/io/mod.rs`).

The agent's cargo features are `fault-injection` and `io-trace` only
(`crates/bulkload-agent/Cargo.toml`).

### 2.2 Wire v6 and the loopback hand-off

The transfer wire is protocol 6, a hard cut with no dual stack (R-N59,
R-N118; `PROTO_VERSION` in `crates/bulkload-proto/src/frame.rs`). v6
(#218, 2026-10-08) is v5 plus the SQLite snapshot seat (section 2.9); every
other frame is v5's ([`docs/design.md`](../design.md), "Wire v6"). The
schema text `WIRE_SCHEMA` and its BLAKE3, `wire_id`, live in `frame.rs`. A
peer whose `Open` names another protocol or another `wire_id` is refused
before any other frame. Each frame is a 4-byte big-endian length, a tag byte
and a body:

- tag 1 is one postcard control message, with nothing after it;
- tag 2 is a content chunk: a fixed 64-byte header (entry, chunk index,
  size, offset, digest), then the payload;
- tag 3 and control variants 12 to 19 are reserved for a Git sub-stream and
  refused. No code builds or consumes them since `carry_v2`'s deletion; they
  stay so that `wire_id` does not move ([`docs/design.md`](../design.md),
  "Reserved Git sub-stream").

A session runs as follows ([`docs/design.md`](../design.md), "Wire v6";
`crates/bulkload-agent/src/transfer.rs`):

```text
source                                   destination
  Open{proto, wire_id, root, state, sqlite_mode}  -->
  Entry{n, row, wal?}  (streamed, <=1024 undecided)
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
  `Entry`, with at most 1024 undecided. The destination answers `Skip` (a
  directory or symlink it made), `Reuse` (it holds this exact stat identity
  durably, so the source reads nothing), `Send`, `WantManifest` or
  `Refuse`.
- **Streaming walk.** The walk offers each seat as it finds it, parents
  before children, names in byte order within a directory, with no global
  sort, at most 4096 items ahead of the wire. Every seat is opened beneath
  one descriptor of the source root, one component at a time, with
  `O_NOFOLLOW` at each step (`openat_beneath` in
  `crates/bulkload-agent/src/io/sys_posix.rs`). A directory swapped for a
  symlink is refused, never followed. Depth and path-length caps refuse a
  subtree as a typed value and carry its siblings (#110, #129). Content is
  read with `pread`, never mapped.
- **Send.** The source reads the file once, cuts it into content-defined
  chunks, hashes each chunk and streams it. The destination verifies every
  chunk against its digest, writes it at its offset, and checks coverage
  and the manifest root before the output joins a group commit.
- **WantManifest and NeedChunks.** The destination chooses this for a
  regular file it cannot `Reuse` when the output path already exists or
  published outputs hold chunks. The source sends its manifest, from its
  ledger without reading when the stat identity is recorded. The
  destination fills what it can from verified local chunks, then asks only
  for the missing indices. A fresh manifest is built from one read kept in
  memory (512 MiB budget); a larger file is streamed as for `Send`, so no
  seat is read twice in a session.
- **Credit.** The destination grants 16 MiB of payload credit and returns
  it as it writes. The source never has more than the grant in flight, nor
  more than 16 entries' content.
- **Held.** The destination answers every `End` with `Held`. `Held{true}`
  means the output's group commit has returned. Only then does the source
  commit the capture to its ledger (section 2.4).

**The loopback hand-off (local `copy` only; OI-1003-Q154 levers 1 and 2;
on the S1 stack, not on `main`).** `copy` runs both halves in one process,
from one build, over a socket pair it makes. Since the levers it joins them
with a hand-off queue that only `copy` can make
(`crates/bulkload-agent/src/transfer/handoff.rs`; `serve`, `receive` and
`pull` take none). The source moves each chunk's allocation into the queue
and writes only its 64-byte header on the socket, under a tag outside v6's
(0xB1), so the frame keeps a data frame's place among the control frames.
A chunk can be made only by hashing its bytes, and its bytes sit behind an
immutable `Arc`, so the destination writes a handed chunk whose digest is
its frame's without hashing it again: the bytes are verified once, by the
source. Bytes that arrive over any transport are always hashed. Credit,
the windows and every durable step are unchanged, and crash adoption and
local reuse still hash the destination's own bytes. The proofs
([levers note](../agent-notes/2026-10-15-s1-levers.md), section 1):

- a cross-host session (`serve` and `receive` over a socket pair, as `pull`
  runs them) with one data frame corrupted refuses that file
  `DIGEST_MISMATCH`, and under a test-only mutation that trusts transport
  bytes, the same run publishes the corrupt file and the same check fails
  (`a_cross_host_session_verifies_every_byte_and_its_mutation_is_caught`
  in `src/transfer/tests/levers.rs`); a second test does the same for a
  delta's `NeedChunks` answers;
- a real `bulkload-agent serve` child, read by `transfer::receive`, hashes
  every received byte, and a handed frame is no v6 frame: the proto codec
  refuses it `FRAME_CODEC` (`tests/loopback_handoff.rs`);
- the hand-off and the plain protocol carry the same trees with the same
  counters, first copy and delta
  (`the_hand_off_and_the_plain_protocol_carry_the_same_tree`);
- `BULKLOAD_LOOPBACK_HANDOFF=0` turns it off, and the fault harness runs
  its copy-based suites a second time with it off (`just handoff-off`), so
  the transport path `pull` takes keeps its crash coverage (`AGENTS.md`,
  "Validation").

The residual it accepts: a fault that changes memory between the source's
hash and the destination's write inside one process goes unseen, where
before the destination's own hash would have caught it. An open wording
question follows: R-N57 says gate (a) runs "its full protocol over a local
socket", and with the hand-off the payload no longer crosses the socket.
OI-1003-Q154 authorized the change; the R-N57 text has not been amended
([`docs/design.md`](../design.md), "Performance";
[merge note](../agent-notes/2026-10-15-s1-stack-merge-main.md), finding 8).

A known limit: a fresh destination is streamed with no cross-file
deduplication, so a chunk repeated across files crosses the wire once per
file ([`docs/design.md`](../design.md), "Wire v6").

### 2.3 Chunks, digests and the manifest root

Chunk boundaries come from FastCDC through the `fastcdc` crate's 2020
variant, with 16 / 64 / 256 KiB minimum / average / maximum
(`crates/bulkload-agent/src/hash.rs`) [FastCDC16] [FastCDC20]. Each chunk is
hashed with BLAKE3 [BLAKE3]. A file's identity on the wire is its
`manifest_root`: BLAKE3 in derive-key mode, under a dated context string,
over each chunk's digest and size (`frame.rs`, `MANIFEST_ROOT_CONTEXT`).

The manifest is built over content-defined chunks, not BLAKE3's own internal
tree. BLAKE3's tree splits input into fixed 1 KiB chunks [BLAKE3], so an
insertion near the start of a file shifts every later leaf. Content-defined
boundaries move only near the edit, which is what lets the destination reuse
the chunks it already holds.

### 2.4 The digest-only source ledger

The source keeps a ledger keyed by each seat's row key and stat identity
(`crates/bulkload-agent/src/transfer_store.rs`). A row holds the manifest
(chunk digests, sizes and `manifest_root`), never content bytes (R-N58).
A power-loss test asserts that a `copy` writes no source pack
(`the_source_writes_no_content_bytes` in
`crates/bulkload-agent/tests/power_loss.rs`).

A row is written only when three conditions all hold:

- the capture's final stat check passed, so a seat rewritten during its
  read leaves no row (R-N86);
- the capture was not racy (section 2.6);
- the destination answered `Held{true}`.

The third condition is what makes R25 safe. A committed capture means the
destination holds the bytes durably, so a resume reuses or adopts the
output and never reads the seat again (OI-1001-Q15).

**The ledger's rows are relaxed (WP0(g), landed).** The model showed that
across a crash between the destination's commit and the source's row, only
the destination's `Reuse` keeps the seat from being read again
([formal README](../formal/README.md), "WP0(g) verdict"). So OI-1003-Q20
and Q37 let the source ledger's *row* commits run with
`synchronous=NORMAL` and `fullfsync=OFF`, and OI-1003-Q104 had it built
([`docs/slo.md`](../slo.md), "Amendment 2026-10-07 (sixth)"):

- the commit that creates a store, with its authority, stays FULL on both
  sides, after the state root is sealed (#161, #166);
- a failed row commit is counted and the transfer goes on (#163, closed);
- a ledger read that fails is a miss: "a corrupt or absent source ledger is
  treated as empty";
- property P79 RELAXED-LEDGER-LOSS and a power-loss proof over every WAL
  prefix of a real copy show that a seat the destination holds reads 0
  bytes whatever the ledger lost.

### 2.5 Destination: stage, seal, publish, group commit

The destination never overwrites a file it cannot prove it wrote;
publication is no-replace (`crates/bulkload-agent/src/materialize.rs`,
`src/io/durable.rs`). Each output is written under a tagged temporary name,
where the tag derives from the destination store's random authority. Its
group commit then:

1. seals each file: `F_BARRIERFSYNC` on Darwin, `fsync` elsewhere;
2. publishes it under its final name without replacement, using
   `renameat2(RENAME_NOREPLACE)` on Linux [Rename2] or
   `renameatx_np(RENAME_EXCL)` on Darwin, or a link and unlink where the
   filesystem has neither (R-N119);
3. seals each touched directory once;
4. fully flushes each touched device other than the store's own;
5. commits the group's records in one SQLite WAL transaction with
   `synchronous=FULL` and `fullfsync=ON`.

**The batched seal (#210, OI-1003-Q107; on the S1 stack).** On Linux a
group of two or more outputs replaces steps 1, 3 and 4 with two device-wide
seals: one `syncfs` per device before any rename, which makes every
temporary's data durable, and one per touched device after the renames,
which makes the entries durable [Syncfs]. Step 1's order is kept: no output
is renamed before its data is durable. `syncfs` reports write-back errors
only since Linux 5.8 [Syncfs], and until 5.17 the kernel dropped the
filesystem's own sync result (an XFS log force's, for example), so below
5.17 the code keeps per-file seals (OI-1003-Q113 set 5.8; OI-1003-Q141
moved the floor to 5.17; `SYNCFS_REPORTS_ERRORS_SINCE` in
`src/io/durable.rs`, which names the two kernel commits). The stack's review found one defect and fixed it: a
superblock error raised once was reported again for every later group whose
seal ran on an old descriptor; the seal now runs `syncfs` on a descriptor it
opens for the purpose
([merge note](../agent-notes/2026-10-15-s1-stack-merge-main.md), finding 1).
A new power-loss proof explores every crash state of a small batched copy:
42 crash points, 13,111 states, exhaustive, 0 violations (same note,
finding 4).

**What the S1 stack changed about when things become durable, never the
order** ([S1 throughput note](../agent-notes/2026-10-08-s1-throughput.md);
[levers note](../agent-notes/2026-10-15-s1-levers.md);
[fold note](../agent-notes/2026-10-16-s1-fold.md)):

- **Write-back while data streams** (OI-1003-Q143 item 1):
  `sync_file_range(SYNC_FILE_RANGE_WRITE)` every 8 MiB as data lands. It is a
  hint only: it starts write-back, waits for nothing and syncs no metadata
  [SyncFileRange]. Every seal stays.
- **Receive workers and parallel store opening** (Q143 items 1 and 3).
- **Directory batching** (Q143 item 2): the directories of one level are
  created and recorded in one commit, under R-N102's order (section 3.2).
- **Larger destination groups** (Q154 lever 3b): a group waits 250 ms for
  the next output and closes at most 1 s after its first; the stream's drain
  still closes the last group at once, and the file and byte bounds are
  unchanged. A larger group holds `Held` back longer and can leave more
  outputs durable without a row after a crash, still at most one group;
  #169's adoption takes those on resume with 0 source bytes read. No commit
  is relaxed.
- **Stores created directly in WAL mode** (lever 3a): `Store::open` writes
  SQLite's own first page of an empty WAL-mode database into the file it
  just created and seals it, so no rollback journal is made. Two of three
  syncs per store go on Linux. The page's seal is the database file's own
  I/O, outside the R-N88 trace, like the SQLite syncs it replaces; on
  Darwin its `F_BARRIERFSYNC` ordering is argued, not proven on APFS
  ([merge note](../agent-notes/2026-10-15-s1-stack-merge-main.md),
  finding 3).
- **The directory finish folded into the last output group** (2026-10-16,
  Q154). The final modes of the directories a copy created are set and
  sealed by the last group's seal, and their completion records commit in
  that group's commit, instead of four directory seals and one FULL commit
  after it. The directory's *pending* record cannot fold: it must be
  durable before the directory's name, which must be durable before
  anything inside it is decided, so it is the session's first durable step.
  The model came first (section 4.2, `MC_dir_fold`). A fold needs every
  final mode to keep owner read, write and search (`0o700`); a review found
  the first rule (`0o300`) too weak and the fix has failing-first tests
  ([fold note](../agent-notes/2026-10-16-s1-fold.md), section 8).
  `BULKLOAD_DIRECTORY_FOLD=0` turns it off.

Every knob above exists to measure its lever on the rig with the B arm's
environment (`r23_ab.py --b-env`), and each only ever adds work when off.

Since #198 a staged file also carries a **capture record** before its seal:
an extended attribute that names the walked row, the manifest root and the
size (`src/transfer/unrowed.rs`). Section 3.1 says what it is for.

**The superseding publish (WP0(d); #187; OI-1003-Q18, Q100–Q102).** A rerun
may replace an output only when its identity equals a row this store
committed: bulkload wrote it, and nothing touched it since. The new file is
staged, filled from the old output's own chunks, and traded with the old
one by `RENAME_EXCHANGE` [Rename2] under a durable intent, with the
displaced file checked afterwards. A crash leaves the old output with its
old rows or the new output with a row of its own. A file system without an
atomic exchange refuses such a seat up front,
`DESTINATION_EXCHANGE_UNSUPPORTED`, with no fallback, by ruling (Q100). An
output published from a racy capture gets an ownership row that lets a
later change supersede it, and never a reuse row (Q101). The model held
this before it merged (Q102; [`docs/slo.md`](../slo.md), "Amendment
2026-10-07 (fifth)").

Space is checked before writing. An entry decided `Send` or `WantManifest`
reserves its size until its `Held`, and a write that would leave less than
`--min-free-percent` free is refused `DESTINATION_SPACE_INSUFFICIENT` as a
value. With larger groups, reservations of written but uncommitted entries
accumulate until their group commits, so a nearly full destination can be
refused earlier, by at most a group's bytes
([levers note](../agent-notes/2026-10-15-s1-levers.md), lever 3b).

### 2.6 Crash resume and racy captures

A rerun after an interruption, or over a store an older engine wrote, meets
six kinds of leftover state:

- **Committed captures and output records.** A seat whose stat identity is
  unchanged is answered `Reuse`, and the source reads nothing.
- **Durable outputs without a row** (#169, #198). A crash between an
  output's publish and its row commit, or a failed group, leaves bytes at
  the final name with no row. The destination adopts such an output from
  its capture record by hashing its own bytes, and the source reads nothing
  ([`docs/design.md`](../design.md), "Performance").
- **Temporaries.** The sweep acts on a temporary only when it carries this
  store's tag, matches the name grammar exactly, and has the expected kind
  and owner (R-N79). It keeps orphaned file temporaries as a chunk source.
  These save wire bytes only: no recorded capture's bytes live only in a
  temporary. Kept salvage is bounded per session, and a temporary past the
  bound is refused `SALVAGE_BOUND_EXCEEDED` (OI-1003-Q24). `docs/design.md`
  still calls the bounds unruled defaults, which is stale.
- **Rows from before the racy guard** (#125, OI-1003-Q26). A store without
  the `racy_guard` marker has its `captures` and `outputs` rows invalidated
  on its first writable open, and each such seat is read once more.
- **A failed group.** A group whose seal or store commit fails answers each
  entry `Held{false}`; neither store records them (#100), and the next run
  adopts those of its outputs that were published and carry a record.
- **A lost source store.** A recreated source store has a new authority, so
  every row key changes. The capture record holds no authority, so outputs
  that carry one are still adopted without a read
  (`MC_r25_strict_unsealed`).

**Racy captures.** A seat whose mtime or ctime falls within a 2 s allowance
of the clock read before it was opened, or later than the clock read after
its final stat check, is racy (`RACY_GRANULARITY_NS` in
`crates/bulkload-agent/src/git_carry.rs` and `src/transfer.rs`). This adapts
Git's racy-index idea [RacyGit] to a mover (section 6 says how it differs).
A racy capture is sent and published, but neither side records it as a
reuse source and it gets no capture record, so the next run reads it again
(#86, R-N76). Correctness comes before the zero-reread claim.

### 2.7 Git estate carry: one engine

Git state is carried as Git objects, not as files under `.git`
(`crates/bulkload-agent/src/git_carry.rs`, `src/estate.rs`,
`src/git_carry/`). **v1 is the engine** (OI-1003-Q15, Q44); `carry_v2` was
deleted by #189, and tag `carry-v2-final` holds it
([`docs/slo.md`](../slo.md), "Amendment 2026-10-05").

**How a capture works.** v1 captures each repository into a private
repository that reads the source's objects through
`objects/info/alternates`, and writes the history as a bundle [GitBundle].
A capture carries refs, objects and real stash commits, the index bytes,
`info/exclude`, configuration, the symbolic HEAD, the shallow frontier,
working-tree dirt, ignored files and worktree administration, with paths
translated for the destination. Restore verbs check a bundle's in-band
drift marker before they write anything.

**Hardening.** Every Git child of the carry and the estimate is built from
one table, `git_carry::git_env` ([`docs/design.md`](../design.md), "Live
union"): no hooks, fsmonitor, automatic gc or maintenance, optional locks,
lazy fetch, system or global configuration, or replace objects; the C
locale; and a discovery ceiling. A partial-clone source refuses before any
other read. Since #197 a registry names every non-test child-process
builder in the workspace and pins each one (section 3.3, P76).

**What v1 has, each on `main`:**

- **Auto-prerequisite chains** (#146): a capture declares the previous
  retained capture's tips as its bundle prerequisites and packs only what
  is new, up to a depth limit.
- **A read-only source object store** (#172): the private repository's
  writers run against a write store that borrows nothing, because Git
  re-stamps any existing copy of an object it is asked to write.
- **Thin group bases** (#177, lane L1), held by two laws as fixed rows
  (`crates/bulkload-agent/tests/git_group_minimality.rs`): P64
  PACK-MINIMALITY and P65 THIN-DELTA.
- **The ref-table capture format** (#182): refs travel in a
  content-addressed ref table commit, so the header grows with distinct
  objects, not refs. It was proven on a real repository of 122,994 refs
  (section 5.3).
- **A pure decision core** (#191, lane L6a): `git_carry::decide::decide`,
  checked by P67 against 363 rows a Haskell reference renders
  (`docs/formal/hs/GitCarryCore.hs`). The callers run only the v1 policy.
- **Grouped chaining under a plan base** (#199, lane L6b, P68). It also
  fixed #181 in the code: a missing plan base now refuses
  `SEALED_OBJECT_MISSING`, not a bare `IO`
  ([formal README](../formal/README.md), "Findings" under GitCarry). The
  GitHub issue is still open.
- **Manifest reuse** (#205, lane L7, P69 REUSE-COST and P70
  SIDECAR-ORDER): reuse reads a `{bundle}.reuse` manifest, not the bundle,
  and the manifest is published before the record.
- **Native refs on a first landing** (#215): a standalone landing restores
  every carried ref natively, the stash in the source's reflog order
  [GitStash], with provenance under `refs/carry/v1/`.
- **Inert origins** (#216, fixed by #221; OI-1003-Q129, Q135): a
  non-HTTPS `origin` is preserved in the receipt, not activated, and a
  standalone restore is built in a private stage and published by one
  no-replace rename, so a refused or killed restore leaves no destination
  ([note](../agent-notes/2026-10-08-fix-216-inert-origin.md)).

**Not built.** The Q46 re-root policy with CORPUS garbage collection (lane
L8, P71). GitCarry's grounding lists its symbols as the module's four
pending ones ([formal README](../formal/README.md), "Results (GitCarry)").

### 2.8 Estate convergence (OI-1003-Q144, Q149)

A rerun of `estate-apply` over a checkout bulkload already landed used to
refuse `GIT_DESTINATION_OCCUPIED`. Since #224 it converges that checkout to
the newer capture, under one rule: **bulkload may update a checkout it
landed only when its ledger proves, by content, that nobody changed it
since landing**; any local change refuses `GIT_DESTINATION_MODIFIED` and is
never overwritten ([`docs/slo.md`](../slo.md), "Amendment 2026-10-15:
estate convergence ruled"; [`docs/design.md`](../design.md), "Live union").

- **Ledgers.** A ref ledger in the common directory and a workspace ledger
  per worktree record what bulkload landed: the capture, HEAD, an index
  entries digest and the stat print of every seat. Each is written
  atomically after the Git writes it names, which run with
  `core.fsync=reference,index,objects`.
- **Proof.** A seat is proved by print, and by content whenever its print
  differs or is racy. A failed proof writes nothing.
- **Refs.** A ref follows the source only while it still holds the value
  bulkload set (a compare-and-swap in a Git ref transaction
  [GitUpdateRef]). A ref changed or deleted on the destination, any
  worktree's checked-out branch, and a branch a rebase or bisect holds are
  left alone and reported. A remote-tracking ref the source pruned is
  deleted; a local branch or tag is kept.
- **Writes.** Only the delta is written, under a durable intent: each seat
  by `RENAME_EXCHANGE` with the displaced file checked and put back if it
  is not the landed one, the index under `index.lock` by compare-and-swap,
  HEAD's branch in its own transaction under every worktree's `HEAD.lock`,
  then HEAD. A crash leaves every component old or new, and the next apply
  settles the intent forward before anything else. Configuration is
  applied at the first landing and only reported after.
- **Three stated limits** (Q149), none of which loses work: a HEAD detached
  for a branch-name conflict re-attaches, and a ref the operator put back
  to its landed value re-converges, only at the next changed capture,
  because an unchanged rerun reads nothing (S3); and a concurrent
  `git switch` can race the HEAD locks, because Git commits the index
  before it takes `HEAD.lock`, so no lock closes that window.
- **Not converged.** Landings made before #224 carry no ledger. `--adopt`,
  which would make such a landing bulkload's by proof, is not built
  (OI-1003-Q145); #226 tracks it, and its design is on an unmerged branch
  (section 5.4).

The design came with its own TLA+ module, `EstateConverge.tla`, and the
module's properties gated the merge (section 4.2).

### 2.9 SQLite through the backup API

Live provider databases are captured with SQLite's online backup API, never
by copying `-wal` or `-shm` bytes (`snapshot` in
`crates/bulkload-agent/src/provider_sqlite.rs`; `AGENTS.md`, "Estate
rules"). The capture opens the source read-only through normal WAL-aware
access, with a zero busy timeout; copies pages in bounded steps within a
step budget; refuses `SQLITE_STATE_CHANGED` at once on lock contention;
converts the snapshot to `journal_mode=DELETE`; and checks it before use.
The backup API holds a read lock on the source only while a step reads, and
restarts when another connection writes mid-backup [SQLiteBackup]. The step
budget turns an endlessly restarting backup into a typed refusal.

**The snapshot seat in the transfer (wire v6, #218, merged by #223;
OI-1003-Q146–Q148).** The first draft's limit, that `snapshot` had no
ledger and read every database on every pass, is closed for the transfer
([`docs/design.md`](../design.md), "SQLite snapshot seats"):

- with `--sqlite=snapshot`, a database seat is captured by the stepped
  backup into a private slot, checked with `integrity_check`, and streamed
  as the slot's chunks, never the live file's;
- its reuse row is keyed on its row and its `-wal`'s identity after the
  backup, so an unchanged store is `Reuse`d with nothing opened on the
  source (P81 SQLITE-CARRY-RESUME);
- a published store is one committed state, under a child writer committing
  every 2 to 20 ms (P80); no sidecar byte reaches the destination, and a
  sidecar is covered only by its base's snapshot (P83);
- a changed store's snapshot replaces the old output only through the
  superseding publish, and only when no `-wal`, `-journal` or `-shm` sits
  beside it at Decide and at the exchange's last look (Q146). The window
  between that look and the exchange is a stated residual;
- a corruption the database's bytes decide carries the default S4
  disposition `abandon` (Q148; P82);
- the default stays `--sqlite=refuse` (Q147): a run that must carry
  databases passes `snapshot` explicitly.

The capture's S2 exceptions (one lock and two counted writes) and the
refusal to read as root are section 3.3's. `SqliteCarry.tla` modelled the
seat before it was built (section 4.2).

### 2.10 Typed refusals, outcomes and closure (S4)

Refusals are values of one taxonomy (`crates/bulkload-proto/src/refusal.rs`).
A refusal crosses the wire and lands in receipts by its stable code.

- Every taxonomy code is raised by product code; a source scan requires it
  (`crates/bulkload-agent/tests/refusal_taxonomy.rs`).
- No blanket conversion turns an OS or codec error into a refusal. Each
  site names itself with `.refuse_at(site)` (`src/refuse.rs`).
- A Git child that exits non-zero refuses `GIT_CHILD_FAILED` with a
  `stderr_class=` from a closed set. Its stderr is classified, never echoed
  or kept (R-N121).
- **The S4 refusal sweep** (#222; [`docs/slo.md`](../slo.md), "Amendment
  2026-10-08") added `CAPTURE_ABSENT`, `GIT_SOURCE_ALTERNATES` and
  `SPACE_EXHAUSTED [path=]`, so a disposition reviews the real cause.
- **56 non-test sites in 14 files still raise `IO` with no errno**, 28 of
  them in the SQLite provider code. A test holds each file's count to an
  allowlist that may only shrink (`IO_NONE_ALLOWLIST` in
  `tests/refusal_taxonomy.rs`).

**Typed outcomes and the disposition ledger** (#195). Outcome records are a
typed `Outcome`, persisted with postcard and decoded strictly
(`src/outcome.rs`). `closure-report` ends every planned item as `applied`,
`referenced-only`, `refused` (a typed refusal that a review disposes),
`refused-pending-review` or `unaccounted` (`src/closure.rs`).
`closure-dispose` records an operator review (accept, re-carry or abandon)
in a ledger bound to the plan's bytes and the SOURCE label, and to the
refusal instance it reviews (`src/disposition.rs`). The `gate` passes only
when every planned item is accounted and every typed refusal is disposed. A
bare `IO` or `FRAME_CODEC` names no cause and never counts; a code that has
left the taxonomy fails closed (OI-1003-Q74, Q75). #200 (a busy ledger lock
surfacing as a bare `IO`) is fixed and closed (#207).

**S4 over a whole run (WP3 PR 4, #225).** Every `copy` and `pull` session
records its outcome in its destination state, one record per transfer, and
`closure-report --transfer` joins it to the same report, ledger and gate
([`docs/slo.md`](../slo.md), "Amendment 2026-10-15 (later)"):

- a session row is `applied` when it ended, and `unaccounted` when it never
  ended, left no record, or refused with a bare `IO`;
- one row per refused path, typed and pending review;
- one `covered` row per live SQLite sidecar a snapshot covers;
- an integrity failure carries Q148's default `abandon`, which a review
  overrides;
- a transfer manifest (`transfer-expect`) binds the run's expected
  transfers to its plan, so a listed transfer with no record, or whose
  session started before the manifest, is unaccounted, and no transfer row
  counts twice.

P86 TRANSFER-CLOSURE carries it (`closure/transfer_tests.rs`). Still
outside: the offline provider verbs, which belong to no run. The manifest
is optional as built; whether the gate should require it is an open
operator question ([finish-line note](../agent-notes/2026-10-15-finish-line.md),
"Open"). The charter's daily-work verdict on the destination (#40) is still
owed.

### 2.11 Drift custody (S5)

A capture pass tolerates refs and worktree seats moving under it. What moved
is recorded, never absorbed ([`docs/design.md`](../design.md), "Drift";
R-N30, R-N72):

- The pass window runs from the pre-pass key to the post-pass key. A capture
  is clean only when the pre-pass key, the export's own before and after
  inventories, and the post-pass key all agree.
- **Export drift**, a move under the export itself, marks the bundle in
  band. Every restore and import verb refuses a marked bundle
  (`CAPTURE_DRIFTED`) before it writes anything: it is never laid down.
- **Key drift**, a move only outside the export's window, leaves a coherent
  snapshot of the state the export read. It applies, and the closure counts
  it applied, as it would a clean pass the source moved past a moment later.
- A drifted capture records a poisoned key that no census hashes to. It is
  never a reuse hit, even when the source moves away and back. The next
  pass carries the move.
- **An object-store rewrite is drift too.** A `gc`, `repack` or `prune`
  that races the pass leaves the item `deferred-with-drift`, and the next
  pass captures the rewritten store; since #222 so does a lender's repack
  under a source that borrows from it.

**HEAD, index and stash moves are custody now** (#38, closed; OI-1003-Q11
under Q152, and Q161). An agent committing, switching, staging or stashing
inside a captured checkout, or committing in a worktree nested in it, no
longer refuses `GIT_AUTHORITY_CHANGED`. The pass records `HeadMoved`,
`IndexRewritten`, `NestedWorktreeMoved` or `StashMoved`, and the next quiet
pass converges the landing to the source. P87 drives this end to end: pass 1
raced by a commit and a `git add`, pass 2 lands, the agent commits, switches
and stages, pass 3 raced by a `git switch`, pass 4 converges; and a second
row has an agent push, pop and drop stashes under passes, each converged
([`docs/slo.md`](../slo.md), the two amendments of 2026-10-15). #228 on
`main` (not in this tree) adds stash-order-v2 (OI-1003-Q169): a stash entry
is its commit and its reflog message, so a converged stash list is the
source's `git stash list` exactly.

**What still refuses**: the exclude file, configuration, the shallow
frontier, foreign nests and gitlinks, and the omitted roots moving under a
pass; directory-shape drift refuses fail-closed. On the file path a seat
rewritten during its read is refused `SOURCE_CHANGED_AFTER_SNAPSHOT` for that
seat alone, and its bystanders are carried (`live_writer` scenarios in
`crates/bulkload-agent/tests/fault_harness.rs`).

One mutation survived: recording a drifted capture's key unpoisoned is not
caught by any Rust row alone, because the `.drift` sidecar is a second
guard; the model's `drift_key_reused` abstracts both as one
([finish-line note](../agent-notes/2026-10-15-finish-line.md), S5-M9).

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
estate, not with the change.

**How it is held.** `Held` gates every ledger commit (section 2.4). `Reuse`
answers unchanged identities. `WantManifest` lets the destination fill from
its own verified chunks. The racy rule is the deliberate exception: it
trades one re-read for correctness.

**Two readings, and which one is the SLO** (OI-1003-Q40;
[`docs/slo.md`](../slo.md), "Amendments 2026-10-04"):

- **The committed-row reading is the SLO's obligation.** A byte is held when
  a committed destination row proves it durable. In the model this is
  `R25_NoDurableReread`.
- **The strict reading** also counts bytes durable at the final path with
  no row. It is not part of the SLO, and since #198 it holds in the model
  (`R25_StrictNoDurableReread`) through the capture record's adoption,
  within stated limits: only a non-racy capture gets a record; the record
  is an extended attribute, so a filesystem without them keeps none
  (counted as `transfer_capture_records_unset`); the model does not model
  the record's loss; and an output published before #198 has none.

On the code, P74 and two power-loss proofs check the strict reading, all
gated. On the S1 stack, `MC_r25_strict_three` checks it with groups of one,
two and three seats and a crash, because lever 3b grows groups
([levers note](../agent-notes/2026-10-15-s1-levers.md), section 9).
Verify-once removes one CPU step and no durable step, so it leaves R25's
argument unchanged.

**Limits.** A stat identity is a reuse key, not a content digest. The racy
reference is the capturing host's wall clock: a source on NFS or SMB whose
server clock runs more than 2 s behind can defeat the guard
([`docs/design.md`](../design.md), "Known limit: clocks"). The file path's
byte counters do not see reads made by Git child processes; the packers are
counted from their own resource usage, as a lower bound.

### 3.2 Durability ordering

**Statement.** A record is never committed before the bytes it describes are
durable on the destination ([`docs/design.md`](../design.md),
"Durability").

**Why it matters.** Durability ordering and R25 are coupled. Under R25 a
committed record forbids a later read. So a record that outlived its bytes
after a power loss would turn a recoverable crash into permanent, silent
loss.

**How it is held.** The group commit seals data before records (section
2.5); on the stack the batched seal keeps that order with two `syncfs`
calls. Directory creation is bound to its inode before it is named
(R-N102): a directory's pending record is durable before its name, and its
name before anything inside it is decided. Its completion follows its
children; with the fold, in the same commit as the last outputs, which
`CompletionAfterChildren` states (section 4.2). On Darwin the protocol relies
on barrier ordering for files and one draining commit per group, modelled
device-wide by default (R-N103). The store's state root is sealed before
`Store::open` returns (#161, fixed by #166).

### 3.3 S2: source safety

**Statement** (paraphrased; the normative text and the budget are S2 and
its amendments in [`docs/slo.md`](../slo.md)). bulkload takes no lock on a
source repository, writes nothing to the source, signals no process, and
runs at background priority (OI-1003-Q5, Q9).

**Why it matters.** The source is a live workstation. A mover that takes
`index.lock`, triggers `gc`, runs a hook, fetches from a promisor remote, or
competes for I/O at normal priority interrupts the agents it was meant to
leave alone.

**The counted exceptions.** All are SQLite's
([`docs/design.md`](../design.md), "Live union"):

| Exception | Ruling | Counted? |
|---|---|---|
| The backup API's shared read lock on the source database | OI-1003-Q16 | As the connection's whole life, `source_sqlite_lock_ns`, on the snapshot seat ([`docs/design.md`](../design.md), "SQLite snapshot seats", *S2*). |
| Creating or touching the source's `<db>-shm` wal-index | OI-1003-Q36 | Yes: `source_wal_index_touched` |
| Creating an empty `-wal` where none existed | OI-1003-Q72 | Yes: `source_wal_created` |

With an effective uid of 0 every provider verb that opens a database
refuses `SQLITE_SOURCE_AS_ROOT` before it opens anything (OI-1003-Q76):
run as root, SQLite re-applies the database's owner to the `-wal` and
`-shm` it opens, which moved a `-wal`'s ctime on sting.

**The clone is a source write, so it is excluded (OI-1003-Q164–Q166).** The
clone lever study ([note](../agent-notes/2026-10-15-s1-clone-lever.md))
asked whether a same-filesystem copy could clone instead of copying. It
found that the clone is a different operation, not a faster copy, and that
it writes the source while every stat field stays the same:

- On XFS a first clone sets `XFS_DIFLAG2_REFLINK` on the **source** inode
  in a logged transaction [XfsReflink] and adds references to its extents.
  Only `FIEMAP`'s shared flag shows it outside the kernel [Fiemap]; the
  stat rows, the bench's source check and P34's `lstat` census do not.
- Writers to the source are blocked for the whole remap. Measured once on
  the rig: a writer stalled 141 ms while a dirty 128 MiB source was cloned,
  against 0.02 ms at most at baseline. Every later write to a shared block
  is copy-on-write [Ficlone]; the study measured its cost on the console
  only, with no log on the rig, and the paper does not quote it.
- A clone needs only `O_RDONLY` on the source, and a same-filesystem
  `copy_file_range` is a clone wherever the filesystem offers reflinks
  [CopyFileRange]; so are Rust's `std::fs::copy` on Linux and, from Python
  3.14, `shutil`'s fast copy [CPythonShutil]. APFS records the same thing
  as `INODE_WAS_EVER_CLONED` [APFSRef].
- A clone across hosts or filesystems is impossible (`EXDEV`), so gate (b)
  and the neo → sting migration could never use one.

The operator ruled the clone excluded and the invisible write a write under
S2 ([`docs/slo.md`](../slo.md), "Amendment 2026-10-15: S2 forbids a clone of
a source file"):

- **(a) No clone call on a source file.** P84 SOURCE-NO-CLONE's static half
  (`tests/source_command_registry.rs`) scans every crate's non-test code:
  no remap call or clone ioctl number is named, in any radix; no `use`
  hides a copy; every raw `syscall` is registered; and every `fs::copy` and
  `io::copy` is a registered site whose input is argued not to be a source
  file. Its kernel half (`tests/source_lock_trace.rs`) proves that `FIEMAP`
  sees a clone the census misses, then that a real `copy`, a `serve`, a v1
  capture and a SQLite snapshot leave every source extent unshared. The
  kernel half needs a reflink filesystem and returns early without one, so
  `just p84-reflink` is the proof that it ran; it is on demand, not a PR
  gate. A `0` from `FIEMAP` means "shares nothing now", not "was never
  cloned"; the static half is what rules the call out.
- **(b) Rig corpus copies are explicit read/write copies, verified unshared
  by `FIEMAP`**, and a shared extent refuses the sample. The study found
  that the rig's sealed corpus already shared all 23 files' extents with a
  copy made by a cloning `cp` [Coreutils90]; OI-1003-Q166 re-sealed it as
  an unshared copy, so the stat-based fixture identity starts a new series
  (section 4.4).

**What is proven, and how.**

- **The file path.** P-S2 (`a_run_leaves_the_source_lstat_census_unchanged`
  in `src/transfer/tests.rs`): every source node keeps its lstat identity
  across a copy, its warm rerun and a refused copy. Gated.
- **The Git carry's object store.** P34's Git leg
  (`src/git_carry/source_inert_tests.rs`) holds every `lstat` field of every
  source node fixed across a capture and two incremental passes. Gated.
- **The command registry** (P76, #197). Every non-test child-process
  builder in the workspace is the source-safe Git builder or an argued
  bypass, pinned by text. Issue #188, which named four agent bypasses, is
  still open on GitHub: the registry pins them as argued bypasses rather
  than removing them.
- **The lock trace** (P77, #197; Linux). During a `copy`, a `serve`, a v1
  export, an estate capture and an estimate, no process takes or waits for
  a lock on a source inode (`/proc/locks`, sampled).
- **The clone ban** (P84), above.
- **The SQLite exceptions.** P75 (`tests/sqlite_wal_index.rs`) asserts
  exactly the counted writes and nothing else, with an oracle independent
  of the implementation. Gated, with a CI coverage gap: CI runs as root, so
  there it proves the refusal only (section 7).
- **Background priority.** Source-side verbs enter background CPU and I/O
  priority before any thread or child exists; a test checks that threads
  and children inherit it. The idle I/O class helps only where the kernel's
  scheduler honours it [IoprioSet], which is why the budget is measured,
  not inferred. On Darwin the inheritance test fails on `main` as it is
  ([merge note](../agent-notes/2026-10-15-s1-stack-merge-main.md),
  finding 3); it has no issue yet.
- **The model.** `S2_TypedSourceAccess` and `S2_BackupLockBounded` hold at
  the protocol level only; the model cannot show that the binary issues no
  other syscall.

**The measured budget: an instrument, no measurement.** The budget (a
latency and load bound on a reference workload) has never been measured
(#165, open). Since no freeze window is allowed (Q152), the instrument now
observes a real run instead of starting its own:

- `s2_budget.py observe` samples the reference workload and load1 for the
  length of a real run, starting nothing and listing no process (R-N11,
  R-N104), and `alongside` joins it with the run's intervals into OFF and
  ON windows (#225);
- **OI-1003-Q160** (#227) judges such an observation against the source's
  own live load: the OFF windows between passes are the baseline, with no
  absolute load1 cap, the budget unchanged, and each pass compared with the
  OFF windows on both sides of it, interpolated to its time, gate by gate.
  That removes linear drift only: load that rises and falls with the passes
  themselves is not separable without listing processes, which the
  instrument never does;
- **OI-1003-Q168** (#228, on `main`, not in this tree) answers that by
  protocol: during a measured run, pass start times come from a schedule
  sealed before any of its slots, never from host load, and evidence is
  refused unless the schedule was followed.

Every verdict rule is proven on synthetic traces, with mutation tables in
the [finish-line note](../agent-notes/2026-10-15-finish-line.md). No
alongside observation has run on neo.

## 4. SLOs and the proof method

The definitions of S1–S5 live in [`docs/slo.md`](../slo.md). This section
says which instrument carries each claim: property tests, the formal model,
crash and power-loss traces, and benchmark gates. Section 4.5 says what CI
runs, and section 4.6 gives the state per SLO.

### 4.1 Property tests: fixed seeds, no fuzzing

[`docs/slo.md`](../slo.md) ("Proof package") asks for property tests, not
fuzzing, and a bounded tested corpus. The method follows QuickCheck
[QuickCheck00] through `proptest` [Proptest]. The conventions
([plan](../plans/2026-10-03-property-test-plan.md); `AGENTS.md`,
"Validation"):

- **One fixed seed in CI.** Every property takes its configuration from one
  helper, `test_support::prop_config`, so each CI run replays the same
  bounded corpus.
- **A deep local tier** (OI-1003-Q78, Q81). `just props-deep` draws twenty
  times the cases **from the same fixed seed** and runs the heavy fixed
  rows that skip themselves in the PR gate; `just crash-sweep` crashes one
  copy at every hit of every fault point. Both fail unless their rows
  really ran (R-N122). Neither is a PR gate. (The 2026-10-07 revision said
  the deep tier drew random seeds; that is no longer so.)
- **A guard test** (#194, `tests/prop_seed_guard.rs`) fails on any proptest
  configuration other than the helper call. One file is exempt,
  `tests/refusal_taxonomy.rs`, which keeps its own fixed seed.
- **Mutation evidence, not coverage.** A new property records the mutants
  that turn it red, and each lane's note tables them.

In this tree, `proptest!` or a `TestRunner` appears at 27 sites in 19
files, the guard aside. Property rows added to the plan since the
2026-10-07 revision, each with code in this tree: P68 GROUPED-CHAIN, P69
REUSE-COST, P70 SIDECAR-ORDER, P76 SOURCE-COMMAND-REGISTRY, P77
SOURCE-LOCK-TRACE, P78 SUPERSEDE-OWN, P79 RELAXED-LEDGER-LOSS, P80 to P83
(the SQLite seat), P84 SOURCE-NO-CLONE, P86 TRANSFER-CLOSURE and P87
HEAD-INDEX-DRIFT-CUSTODY. #228 on `main` adds P85 (the stash list). P71 (the
re-root policy) has no code. This revision does not recount the plan's
total of built properties; that count is owed (section 9).

**S3's ignored properties are green now.** The 2026-10-07 revision listed
three S3 properties ignored against #186 and #187. Both are fixed on
`main` (#203; [`docs/slo.md`](../slo.md), "Amendment 2026-10-07 (fourth)"
and "(fifth)"): a refused SQLite seat is sniffed once per stat identity and
its refusal remembered, so the unchanged-estate clause reads 0 content and
0 sniff bytes; and a changed seat whose output this store wrote is
superseded, filled from the old output's own chunks. P21 and P23 with
refused seats and P21 over in-place changes are green and unweakened. One
test stays ignored and unruled: the "twins" reading of S3's second
inequality (`p21_pinned_twin_seats_cross_their_shared_chunks_once`). Issue
#187 is still open on GitHub although its fix is on `main`.

### 4.2 The formal model: TLA+, a Dhall catalogue, a Haskell cross-check

OI-1003-Q32 makes the model a hybrid of three tools, each with one job
([formal README](../formal/README.md), the source for everything below):

| Role | Tool | Job |
|---|---|---|
| Checker of record | TLA+ with TLC [TLA94] [Specifying02] [TLC99] | Every verdict, count and counterexample cited as a result is TLC's. |
| Typed catalogue | Dhall [Dhall] (`docs/formal/catalogue/`) | Holds every configuration's constants and expectation, every mutation's verdict and every property's traceability row, for all five modules. It renders every `configs*.tsv` and `MC_*.cfg`: 243 files in this tree. |
| N-version cross-check | Haskell (`docs/formal/hs/`) | A second encoding of BulkloadTransfer and GitCarry that must reproduce TLC's state counts and mutation verdicts, and the reference decision core. It does not cover the three newer modules. |

**None of this is gated.** `just tla-check`, `just tla-render` and
`just formal-nv` are standalone recipes; every result here is
*informational*. Two checks run before any TLC run: *staleness* (the
committed configurations equal the catalogue's rendering) and *grounding*
(every operator, constant, mutation and cited code symbol exists, the
symbols in the non-test Rust sources). Each negative configuration breaks
one rule and must fail on the one property it names.

**What N-version checking does and does not show.** In N-version
programming, several versions are prepared independently and their outputs
compared, on the assumption that they fail independently; Knight and
Leveson report that tests in which more than one version failed were
substantially more frequent than independence predicts [KnightLeveson86]
[NVersion85]. The explorers are "independent code, shared design",
transliterated by hand from the TLA+ text, so parity shows that TLC
evaluates the specification as its text reads, not that the specification
reads the Rust code right. The pinned decision rows are the one place the
Haskell side checks the code (P67).

A caveat on every module: a model holds what its author abstracted, at
small constants. The paper reports each module's scope from its README and
cites no module as proof of more.

#### `BulkloadTransfer.tla`: the transfer protocol

It models the wire per entry; staging, seal, no-replace publish, directory
seal and the group commit; `Held` and the digest-only ledger; the source
store's authority; crashes of either host or both; racy captures, source
edits, third-party writes, failed group commits and the space refusal;
typed source access; WP0(g)'s relaxed ledger; and #187's superseding
publish with its intent, ownership row and remembered refusal. Seventeen
frozen safety invariants include `R25_NoDurableReread`, `RecordImpliesBytes`,
`HeldAfterCommit`, `NoClobber`, `S2_TypedSourceAccess` and
`ClosureAccounted`; the superseding publish adds `SupersedeAtomic`,
`OwnershipNeverReuse`, `RememberedRefusalSound` and
`ExchangeRefusedUpFront`.

**Run of record** (README, "Results"): one `just tla-check` on sting,
2026-10-07, over the #187 extension: all 70 rows matched (22 PASS, 8
REACHED, 38 FAIL, 1 SIMULATION, 1 INCONCLUSIVE budget self-test), and a
second run after the merge of `8006085` matched again. The largest pass
row of the superseding publish, `MC_supersede_main`, explored 1,664,264
distinct states.

**What the S1 stack added** ([levers note](../agent-notes/2026-10-15-s1-levers.md),
sections 3 and 9). The spec's `Commit` already lets any nonempty set of
ready seats commit as one group, so lever 3b's larger groups are behaviours
every configuration already explores; the rows lever 3b rests on were
rerun unchanged (`MC_main` 963,314 distinct states, `MC_wp0g` 945,031,
`MC_r25_strict_main` 963,928, and their negative rows). The review added
`MC_r25_strict_three` (three seats, groups of one to three, a crash and the
strict reading): PASS at 636,976 distinct states. The spec abstracts a
digest check as equality of versions and has no separate wire-verify step,
so verify-once needs no row.

**What it does not prove** (README, "Not proven here"): code-level S2;
chunking, credits and cross-file deduplication; directories (that is
`DirectoryRecords.tla`); storage below the store; larger bounds.

#### `GitCarry.tla`: chain and base custody

This module (OI-1003-Q43, Q46) models what a v1 capture depends on: the
plan base a group shares, chain links and depth, crashes between a bundle,
its sidecars and its record, third-party damage to the corpus, and Q46's
re-root and garbage collection, which have no code. **Run of record**:
2026-10-05, all 23 rows matched; L6b's and L7's checks (2026-10-07) added
rows and grounded their symbols, leaving 4 pending, all lane L8's
(README, "Results (GitCarry)"). **One finding stands open**: a bundle
rewritten in place blocks its item while the source holds still
(`MC_gc_live_rewritten`); every refusal is typed, but recovery does not
happen. The other finding, a missing plan base as a bare `IO`, was fixed
by L6b (section 2.7).

#### `SqliteCarry.tla`: the SQLite snapshot seat (#218, OI-1003-Q146)

Model first, per Q102's precedent. One live store, one destination path,
at most three runs and one crash: the store's logical version and the stat
identities of its main file and `-wal`; the walk's two separate stats; the
stepped backup with the lock released between steps; the superseding
publish with its last look for sidecars; third parties adding a sidecar or
writing the output. Twelve properties, among them `SqliteNeverTorn`,
`SqliteReuseSound`, `WalWriterNeverBlocked`, `S2_BackupLockBounded` and
`S3_UnchangedSqliteZero`. **Run of record** (README, "Results
(SqliteCarry)"): 2026-10-09, every row as expected: 6 PASS, 3 REACHED, 15
FAIL on their named properties, and the budget self-test. It does not model
SQLite's internals (that a backup whose steps saw one version copies it is
an axiom, checked by P80), clocks, or the window between the last sidecar
look and the exchange.

#### `EstateConverge.tla`: destination custody of convergence (OI-1003-Q144)

This module models estate convergence and gated its merge. One repository
with a main checkout and a linked worktree, refs of every class and a
stash; a source that commits, renames, switches, stashes and edits; captures
and applies with their intents, ledgers and settles; crashes at every step,
power loss that undoes Git renames no directory sync made durable, and a
crash inside a Git ref transaction; an operator editing seats, refs, the
stash and HEAD between or during applies. Properties: `NoLocalWorkLost`,
`NoSourceWorkLost`, `TypedRefusal`, `NoReRead`, `CrashAtomicity`,
`OneCheckout`, `NoDriftedLanding`, and the liveness properties
`ConvergesWhenQuiet` and `NoWedge`.

Its method is the paper's strongest example of model and code reviewed
together. Four review rounds found defects in the code; where a reviewer
named the rule, the model holds the rule and the code's behaviour before
the fix is a mutation whose row must fail. **Run of record** (README,
"Results (EstateConverge)"): 2026-10-09, all 85 rows matched (33 PASS, 1
seeded simulation, 3 REACHED, 2 documented-limit finding rows, the unfair
negative, 43 mutation rows and one `also` row, and the self-test); 53 cited
code symbols, none pending. #38 and Q161 added the raced-capture rows; every
`MC_ec_` row was run again on 2026-10-15 (89 rows and the self-test: 34
PASS, 49 FAIL, 5 REACHED, 1 SIMULATION), and every pass row but
`MC_ec_capture_raced` explored its recorded count. The two documented-limit
rows are Q149's limits 1 and 2, which fail `ConvergesWhenQuiet` by ruling.

What it does not prove (README, "What EstateConverge does not prove"):
bytes, modes, symlinks, gitlinks, the exclude and configuration; the
timestamp-granularity window inside one seat operation; `git switch` as
more than one step; two applies at once (a lock serialises them).

#### `DirectoryRecords.tla`: directory records (R-N102, Q143; Q154 fold)

A module of its own, since composing per-directory state into the
transfer model would multiply a state space already at its budget. It
models directories created as the code creates them (a tagged `mkdirat`,
its parent sealed, its record bound to the inode, an exclusive rename, its
parent sealed again), batching per level, adoption after a crash, per-name
power loss, reused inode numbers and a third party's directory. Properties
include `NoStrandedDirectory`, `CompleteImpliesFinal`, `NeverAdoptForeign`,
`NoDirectoryUnderTemporary` and, for the fold, `CompletionAfterChildren`.
**Run** (README, "Directory records"): 2026-10-16, every `MC_dir_` row as
expected, including `MC_dir_fold` (PASS, 1,140,105 distinct states),
`MC_dir_fold_crashes` (PASS), and the fold's two mutations
(`MC_dir_neg_fold_leaves_owed` failing `CompletionAfterChildren`,
`MC_dir_neg_fold_unsealed` failing `CompleteImpliesFinal`). The exhaustive
pass rows without the fold explored the same states as before it. It
models one device and no liveness.

### 4.3 Crash and power-loss traces

- **The W7 fault harness** crashes a real `copy` (and, since L7, an estate
  capture's publication) with `_exit` at each fault point and checks: a
  record implies its bytes (I1); no partial leaf exists under a final name
  (I2); a committed file costs 0 source reads on resume (I3); no temporary
  survives (I4). Source: `crates/bulkload-agent/tests/fault_harness.rs`. On
  the stack it passes 105 tests, and its copy-based suites run again with
  the loopback hand-off off ([fold note](../agent-notes/2026-10-16-s1-fold.md),
  section 4).
- **The R-N88 power-loss checker.** A process crash leaves the page cache
  intact, so the harness cannot see a missing flush. The checker
  (`crates/bulkload-agent/src/io/crash_check.rs`) records the syscall trace
  of a real `copy` and reasons over the crash states an abstract
  persistence model allows, like ALICE [ALICE14]. Unlike ALICE, it
  enumerates up to a bound: at each crash point with at most 12 optional
  mutations it tries every subset. Above the bound it tries a bounded
  family. Darwin's rules model `F_FULLFSYNC` as a drain of the whole device
  queue and `F_BARRIERFSYNC` as ordering without durability [AppleFcntl]
  [AppleDiskWrites].
- **New proofs on the stack**, each exhaustive with 0 bounded points: a
  small batched copy (42 crash points, 13,111 states), and a folded finish
  in group and strict mode (46 and 43 crash points, 74,829 and 81,849
  states). Each has teeth: a dropped seal or a mode moved after its seal
  turns it red ([merge note](../agent-notes/2026-10-15-s1-stack-merge-main.md);
  [fold note](../agent-notes/2026-10-16-s1-fold.md), section 3). The
  power-loss copy also passes at every destination group size of its tree,
  1 to 7 files ([levers note](../agent-notes/2026-10-15-s1-levers.md),
  section 9).
- **Resume proofs** (`materialize::adoption_power_loss`): directory
  adoption, and an unrowed output adopted without source reads at every
  power-loss state of a crashed publish.

All of these are *gated*: they are in the mandatory `check-fast` tier
(`AGENTS.md`, "Validation").

### 4.4 Benchmark gates

- **Gate (a), local.** `r23_ab.py` builds the candidate B and the baseline
  A and runs B/A/B/A/B; B passes only if every B rep passes, and A is
  informational (OI-1002-Q30). A sample is gated only on AC power with the
  1-minute load under the R-N81 limit, with other lanes held quiet (R-N91).
  `--under-load` lifts only the load gate and never produces a verdict
  (OI-1003-Q39, Q50).
- **The hermetic rig is the gate of record** (OI-1003-Q96, Q97): mbp-13, a
  2015 MacBook Pro (2 cores, 4 threads) on Linux with an XFS work root,
  with a pinned A control (`3931471`, OI-1003-Q103) and the flake-pinned
  rclone v1.74.4 (OI-1003-Q105). A neo → sting run is a field
  confirmation, never the verdict. Two questions on how to read the rig are
  still open in [`docs/slo.md`](../slo.md) ("Amendment 2026-10-07
  (later)"): whether the load bound should scale to the rig's 2 cores, and
  whether gate (a) on Linux is meant to compare a flushed native copy with
  an unflushed rclone copy.
- **An equal-durability arm, informational** (#209): the bench also times
  one `syncfs` of rclone's destination, outside the timed window, and
  reports "rclone+syncfs". It never enters the verdict: the gate is
  against rclone as shipped (OI-1003-Q154 keeps S1 gated against plain
  rclone).
- **The admission margin** (OI-1003-Q143): a coordinator-quiet window for a
  gated sample is spent only when, over an informational series, B's native
  initial and delta medians are both at or under 0.90x rclone's
  ([fixed-cost plan](../agent-notes/2026-10-08-s1-fixed-cost-plan.md),
  step 2). Below that margin a gated sample would only record another FAIL.
- **The corpus is unshared** (OI-1003-Q165, Q166; section 3.3): the rig's
  default corpus is the re-sealed copy, every working copy is a read/write
  copy checked with `FIEMAP` before the sample and after every rep, and
  the bench checks its fixture after every arm. The re-seal starts a new
  series: samples before and after it are not one series.
- **Per-lever arms**: `--b-env KEY=VALUE` reaches the B reps only, is
  recorded in the report and the evidence, and is refused in a gated
  sample; the harness refuses any inherited `BULKLOAD_*` variable.
- **Gate (b), cross-host.** The harness is on `main`
  (`crates/bulkload-bench/scripts/gate_b.py`). It drives today's
  single-stream `pull`; the native multi-stream arm does not exist, and no
  run happens until gate (a) passes (OI-1003-Q66).
- **The estate-shaped corpus** (#159, OI-1003-Q19), **the S3 estate
  harness** (`s3_estate.py`) and **the S2 budget instrument**
  (`s2_budget.py`) are on `main`. None has produced a gated run.

### 4.5 What CI runs

The pull-request workflow has **two gates**, `source` and `fault-harness`,
each capped at 25 minutes (`.github/workflows/ci.yml`; #190, OI-1003-Q65,
Q71). `source` runs formatting, clippy with warnings denied, the workspace
tests, the secret scans and the contract tests; `fault-harness` runs the
fault harness, the power-loss and resume proofs, and the hand-off-off arm.
TLC, the deep property tier, `p84-reflink`, `darwin-check` and every
benchmark are outside both.

Read from GitHub on 2026-10-16: every pull request merged since the last
revision (#209 to #228) carries a green run of both gates. No push run on
`main` is recorded after 2026-10-07 (`148cf75`), so "CI green on `main`"
is an inference from the merged pull requests, not an observed run.

### 4.6 State per SLO

| SLO | Instrument | State |
|---|---|---|
| S1 gate (a) | R23 A/B harness on the rig | **Not met.** The one gated sample of record (2026-10-07) is a FAIL, 0 of 3 B reps (section 5.2). Informational, under load: the stack is at about 1.13x plain rclone on the initial copy (section 5.3), short of Q143's 0.90x admission margin. |
| S1 gate (b) | `gate_b.py` | Pending: no run, and the native multi-stream arm is not built. |
| S2 properties | P-S2, P34, P75, P76, P77, P84, the priority test; `MC_s2`, `SqliteCarry` | Gated for the file path, the Git object store, the command registry, the lock trace, the clone ban (static half) and the SQLite exceptions. The clone ban's kernel half is on demand. #188 is open. |
| S2 budget | `s2_budget.py` (`run`, `observe`, `alongside`; Q168's schedule on `main`) | **Pending. Never measured** (#165). |
| S3 file path | P18, P19, P21, P23, P78, P79, fault-harness I3; the model's S3 invariants | Gated, with refused and changed seats (#186 and #187 fixed). One twins property ignored and unruled. |
| S3 Git path | P64, P65, P67, P68, P69, counters tests | Laws gated. One estate measurement, informational and taken before four fixes (section 5.3). |
| S3 SQLite | P81; `S3_UnchangedSqliteZero` | Gated in the snapshot seat: an unchanged store is reused with nothing opened. The default mode refuses databases (Q147). |
| S3 rerun ratio | `s3_estate.py` | Informational, one run (2026-10-04). `docs/slo.md` words the bound on wall-clock; the evidence judged the CPU ratio (OI-1003-Q35), which `docs/slo.md` does not carry. |
| R25 | P74, I3, power-loss proofs; `R25_NoDurableReread`, `R25_StrictNoDurableReread`, `MC_r25_strict_three` | Gated on the code; holds in the model within its bounds. |
| Durability order | fault harness, R-N88 checker, resume proofs; transfer and directory-record invariants | Gated. The real-copy proof is bounded; the new batched and fold proofs are exhaustive. |
| S4 | closure gate, disposition ledger; P72, P73, P86; `ClosureAccounted` | Gated **over a whole run**: estate items and transfers. 56 bare `IO` sites remain. The manifest is optional. |
| S5 | P87, drift tests; `EstateConverge` raced rows | Gated: ref, seat, object-store, HEAD, index, nested-worktree and stash drift are custody. The exclude, configuration, shallow frontier, foreign nests and omitted roots still refuse. |
| Convergence (Q144) | `estate_converge` tests, fault-harness rows; `EstateConverge.tla` | Gated on the code; model run informational. Three ruled limits (Q149). `--adopt` not built. |

## 5. Results

### 5.1 Gated (on the code)

A gated result is one that `check-fast` and the two CI gates enforce on
every pull request. These hold in this tree:

- **Crash and power-loss ordering**: fault-harness invariants I1–I4, the
  real-copy power-loss proof (bounded), the batched-copy and folded-finish
  proofs (exhaustive), and the resume proofs.
- **S3 on the file path**, with refused and changed seats: a rerun reads at
  most the changed seats; a resume after any cut reads only the unapplied
  files and a further rerun reads 0; changed seats cross as absent chunks.
- **R25's strict reading**: P74 over fixed seeds.
- **Git**: P64 and P65's pack laws, the ref table at 131,072 refs, P67 over
  363 reference rows, P68 to P70.
- **S2**: the source census on the file path and the Git object store, P76,
  P77, P84's static half, P75's counted SQLite exceptions.
- **SQLite seat**: P80 to P83.
- **S4**: P72, P73 and P86, over estate items and transfers.
- **S5 and convergence**: P87, the drift tests, and the `estate_converge`
  suite.

On the S1 stack (not on `main`): the levers' proofs (section 2.2), the WAL
template and fold tests, and the new power-loss proofs. The stack's last
`just check-fast` passed on sting with 728 library tests and 5 ignored
([fold note](../agent-notes/2026-10-16-s1-fold.md), section 8). On Darwin
the stack builds and passes everything but one test that fails on `main`
as it is (section 3.3); the last Darwin run stalled in the kernel and has
no result for the fold review's four new tests (same note).

### 5.2 Gated benchmark sample: S1's, and it fails

The only gated benchmark sample of record is S1's, on the rig, 2026-10-07
14:10Z (B = `adfdf57`, A = the pinned `3931471`, rclone v1.74.4;
[evidence](../evidence/r23-2026-10-07-1410Z-mbp-13-record.md)). It is a
**FAIL**, 0 of 3 B reps: native initial copy 856.695 ms against rclone
360.705 ms (medians over all samples), and on the 1 % delta too. Warm and
interrupted resume read zero in every rep, and peak memory stayed under its
bound. No S2-budget or S3-ratio sample is gated.

It predates every S1 change in section 5.3 and the corpus re-seal.

### 5.3 Informational

Each item below is on record but enforced by nothing. Every S1 series here
ran in `--under-load` mode: **informational, under load, never of record**,
and none is an S1 result in either direction.

**S1: where the gap was (2026-10-07 and 08).** Measured after the FAIL of
record, on the rig:

- **The file syncs were not the critical path.** A sample of #210's head
  batched 20 of 23 files and cut flush time by about 140 ms, and wall time
  did not move ([evidence](../evidence/r23-2026-10-07-2146Z-mbp-13-pr210.md)).
- **WP0(g) changed nothing measurable**: native initial copy 880.3 ms
  before and 880.0 ms after; the source row commits were 4 % of the copy
  and off its critical path
  ([seal breakdown](../evidence/s1-seal-breakdown-2026-10-07.md)).
- **CPU: two evidence files read it differently.** The #210 sample's file
  calls the copy CPU-bound on FastCDC and BLAKE3, from a worker-summed
  `cdc_hash_ns` near twice the wall time. The later profile
  ([profile](../evidence/s1-profile-2026-10-07.md)) notes that
  `cdc_hash_ns` is elapsed, not CPU, time, and finds no native thread
  saturated: about 1.1 user CPU-seconds, with about 1.5 of 4 CPUs busy.
  This paper follows the profile, which measured CPU directly. Both agree
  that hashing was where native and rclone differed most in work.
- **Fixed per-copy work was the gap**: the steady stream accounted for
  about 350 ms of the receiving thread, the other 450 to 500 ms of each copy
  lay outside it
  ([hand-off timers](../evidence/r23-2026-10-08-0239Z-mbp-13-handoff.md)).

**S1 throughput, 2026-10-09** (B = `8a24161`, the Q143 work on #210; A =
`main` `91d4294`; 5 rounds, 15 B reps;
[evidence](../evidence/r23-2026-10-09-mbp-13-s1tp-underload.md)):

- against plain rclone, B was slower in every rep: initial 555.110 ms
  against 358.509 ms (median ratio 1.548), delta 120.186 against 104.443 ms
  (1.151);
- against A in the same window, B's initial copy took 0.615x A's time
  (555.110 against 902.544 ms), and the spreads did not overlap;
- at equal durability, informational only, B's initial median was 0.940x
  rclone+syncfs.

**S1 levers, 2026-10-15** (B = `59e5e60`, A = `8a24161`, 5 rounds of
`r23-s1lev-20261015-underload-{1..5}.md` on mbp-13;
[evidence](../evidence/r23-2026-10-15-mbp-13-s1lev-underload.md)): B's
round medians for the initial copy were 360.8, 419.9, 402.9, 402.4 and
406.3 ms against rclone at 347.4 to 362.7 ms; the median of the round
medians is 402.9 against 356.8 ms, **1.13x**. The series read the old
sealed corpus (whose extents were shared with another copy); `r23_ab.py`'s
working copy of it did not share extents (clone note, section 1). The
spread between rounds, about 59 ms, is larger than anything the fold could
save.

**The fold, 2026-10-16** (sting, not the rig; not of record; [fold
note](../agent-notes/2026-10-16-s1-fold.md), section 4). Every rep of a
census removed four directory seals and one FULL commit per copy, as
designed. Timers showed the tail shrinking by about 7 to 9.5 ms and the
drain growing by about 6 to 8 ms, because the drain now sets the final
modes; the wall difference was within noise in either direction.

**The fold on the rig, 2026-10-16** (B = `51d1d4c`, A = `8a24161`, on the
unshared corpus; 5 rounds, each a fold-on run and then a
`BULKLOAD_DIRECTORY_FOLD=0` run;
[evidence](../evidence/r23-2026-10-16-mbp-13-s1fold-underload.md)):

- the counters moved exactly as the fold note predicted: 4
  `directories_folded` per initial copy with the fold on and 0 with it
  off, directory barriers 4 against 8, SQLite commits 5 against 6;
- B's initial median was lower with the fold on in all five paired rounds,
  by 9.3 to 79.9 ms; the medians of the round medians were 372.0 against
  437.3 ms, 65.3 ms apart. A, the same binary in both arms, was also
  slower in every fold-off run, by 12 to 50 ms, and the fold-off run always
  went second, so part of the gap is order and drift. The receive tail
  accounts for about 28 ms (28.4 to 0.7 ms), and on the rig the drain did
  not grow to absorb it;
- against plain rclone the fold-on round medians ran 0.948x to 1.151x
  (median 1.038x): not at Q143's 0.90x;
- the 1 % delta was slower than rclone in 9 of the 10 round medians, in
  both arms; the fold does not touch it.

**Where S1 stands, and why the margin is narrow: the SOTA study**
(2026-10-15, rig micro-benchmarks, not R23 samples;
[note](../agent-notes/2026-10-15-s1-sota-study.md)):

- **A fully durable copy of corpus v1 on the rig cannot take less than
  about 230 to 240 ms**: the time to put 240 MB onto that SSD behind one
  barrier group. A buffered copy with no sync takes 48 ms. Plain rclone
  takes about 330 ms and syncs nothing (its `strace` shows no `fsync`,
  `fdatasync`, `syncfs` or `sync_file_range`), so its time is its own
  overhead, never the device's.
- **The largest lever was how many barrier groups lie on the critical
  path**: the same bytes cost 238 ms as one group and 539 ms as 23, about
  14 to 27 ms per extra group. The 2026-10-15 series shows the stack now
  lands corpus v1 behind one destination group (`durable_groups=2`, the
  source ledger's relaxed group and one destination group, in every B rep;
  fold note, section 1).
- **Substrate changes do not beat the floor on this rig**: more placement
  threads, stripes within a file, `O_DIRECT` and preallocation all landed
  within about 10 ms of the kicked floor; io_uring was not measured, and at
  low I/O load published measurements find the Linux I/O APIs competitive
  with each other [RenTrivedi23].
- **The implication**: rclone's 330 ms over a 235 ms floor leaves native
  about 60 ms at the 0.90x margin, and about 90 ms at 1.0x, for setup, the
  walk's ramp, store creation, commits, barriers and the tail.
- **The clone would beat the floor**, 15 to 18 ms durable as a lower bound,
  because it writes no data; it is excluded (section 3.3) and was never
  admissible to a gate (R-N57, R-N63).
- **Gate (b) topology** is analysed and nothing is built: a WAN probe
  between the rig and sting was link-bound (one stream as fast as four);
  zstd level 1 would remove 18.6 % of corpus v1's wire bytes but needs a
  wire version; QUIC is rejected, since over fast networks it has measured
  data-rate losses of up to 45.2 % against TCP [ZhangQUIC24].

**Sting censuses of the levers** (2026-10-15, not of record;
[levers note](../agent-notes/2026-10-15-s1-levers.md), section 5.2):
with the hand-off on, 194,988 to 194,997 bytes crossed the socket against
240,014,816 to 240,014,843 with it off, and the destination hashed none of
the 239,819,837 payload bytes; lever 3b cut the destination's FULL group
commits from 3 to 5 to 1 per copy.

**S3 on the estate corpus, sting, 2026-10-04**
([evidence](../evidence/s3-estate-sting-2026-10-04.md)). One run per scale,
ungated, at a 1-minute load of 16 to 47, measuring `main` at `4a10bb8`:
before typed refusals (#151), the read-only object store (#172), thin bases
(#177), the ref table (#182), #186's and #187's fixes and the SQLite seat.
**It has not been re-measured.** At estate scale (142,321 entries,
4.19 GB): unchanged reruns read 0 Git source and pack bytes; the file half
read 176 sniff bytes (#186, since fixed); the SQLite provider re-read every
database, 180,581,128 bytes per pass (the seat's reuse, since built, is not
measured at estate scale); and Git inequality 2 failed at ten changes, the
failure thin bases address, held since by P64 and P65 rather than by a new
measurement. The rerun ratio was inside the bound at estate scale on both
the CPU and the wall-clock reading; at small scale it failed the CPU
reading the evidence used and passed the wall-clock wording of
`docs/slo.md`.

**The 123k-ref repository, sting, 2026-10-05**
([evidence](../evidence/2026-10-05-blahaj-v1-carry.md)): a real repository
of 122,994 refs over 2,382 distinct objects captured with no refusal, its
manifest 1.58 % of the cap, and restored exactly (0 missing, 0 extra, 0
object-id differences), with the source's `.git` census identical before,
between and after every step. It was shallow, its corpus started empty, and
the restore was refs only.

**Model checking** (section 4.2): every row of every module matched its
expectation on its latest run.

### 5.4 Field evidence: the GFTB carry, neo → sting, 2026-10-15

This is the first carry of a real working estate by the current engine, and
the evidence that the design's promise, an unchanged rerun that costs
nothing, holds outside a test. It is **field evidence**: one run, outside
any gate, deciding no SLO.

**Provenance.** The results come from the lane's receipt,
`/srv/fast-local/jess/bulkload/gftb-resume-20261015/carry.receipt` on
sting (sha256
`d2d4852f71c51148e47d6e73373de40408286fe7b99831468bf6c628ff6a5713`), under
OI-1003-Q163, and are now recorded in the
[field evidence](../evidence/field-2026-10-15-gftb-carry.md). The agent
was `main` at `f4988060` (#224), built on both hosts, **not** the S1
stack. The receipt records that the live sting repositories, and
neo's GFTB repositories (read-only, with neo's probe identical before the
first capture and after the rerun capture), were left untouched.

**The plan.** 137 items, equal to the inventory's neo rows: 13 main
checkouts, 123 linked worktrees (36 nested in one repository's
`.worktrees`), and 1 bare repository carried as refs only.

| Step | First pass | Rerun on the unchanged source |
|---|---|---|
| neo `estate-capture` | exit 0, 287 s, 137 captured, 0 refusals; per-item `source_bytes_read` summed to 1,582,312,069 | exit 0, 19 s, 137 reused after census; summed `source_bytes_read` 0, no nonzero read counter |
| sting `pull` | exit 0, 32 s, 688 completed, 294,639,856 bytes received, 0 refusals | exit 0, 0 completed, 688 reused, 0 bytes received, `source_bytes_read` 0 (122,762 wire bytes, control frames only) |
| sting `estate-apply` into a fresh root | exit 0, 864 s at load 23 to 48: 136 workspaces restored, 1 refs-only import, 0 refusals, `source_bytes_read` 0 | exit 0, 2 s, `source_bytes_read` 0; landing probe identical before and after |
| `closure-report` | verdict pass, gate pass: 137 planned, 136 applied, 1 referenced-only, 0 refused, 0 pending review, **0 unaccounted** | pass, same totals |

A verification script compared neo's pre-capture probe with the landing:
137 of 137 matched on HEAD branch and commit, status count and digest,
untracked count, stash list (order and entries), and the heads and stash of
all 14 main and bare repositories, with no `carry/*` branch and no prunable
worktree. Configuration differences were reported, not applied: one
repository's non-HTTPS `origin` was absent in the landing, as #216's inert
origins rule requires.

**What it found, and what it does not show.**

- **A bare repository has no native landing into a fresh root.** The
  refs-only bare item received provenance refs only; its three native refs
  needed a hand step. The receipt names this as an agent finding.
- **The landing could not be made live by the tool.** Making the live
  sting paths point at the ledgered landing took a hand-written reconcile
  script (`reconcile.receipt`, sha256
  `637867b736b02c37774982aa0bd58e43f2941e6b7b455cadcbc5a42ac73a03c2`: 26
  occupied names swapped aside, 165 destination refs imported create-only,
  `verify_rc=0`, under OI-1003-Q163 and Q167). Issue #226 asks for this
  natively; its design (`estate-apply --adopt` into live paths) is staged on
  an unmerged branch and is not described here as a result.
- It ran on an unchanged source for the rerun, so it shows S3's unchanged
  clause, not S5's convergence under a moving source; P87 and the
  `estate_converge` suite carry that.
- No S2 observation ran alongside it, and SQLite stores were not part of
  the plan.

### 5.5 Pending

| Result owed | Blocker or state |
|---|---|
| An S1 PASS | The FAIL of record stands. First, an informational series at or under Q143's 0.90x on both initial and delta. |
| The fold's rig series, separated from drift | The 2026-10-16 series ran under load with the fold-on run always first ([evidence](../evidence/r23-2026-10-16-mbp-13-s1fold-underload.md)); its A control moved 12 to 50 ms between arms. |
| An S1 gate (b) sample | Waits on gate (a) (OI-1003-Q66). The multi-stream arm is not built. |
| The S2 measured budget | #165. No alongside observation on neo; Q168's schedule is on `main`, not in this tree. |
| S3 on the estate corpus after #172, #177, #182, #186, #187 and the SQLite seat | Not re-measured. |
| A gated S3 rerun-ratio verdict | None recorded. |
| `--adopt` and protect (#226) | Designed and staged on an unmerged branch; OI-1003-Q145 had held it unbuilt. |
| The re-root policy and CORPUS GC, P71 | Lane L8; no code. |
| The daily-work verdict on the destination | Issue #40. |

## 6. Prior art

Each entry says what the system does, then what bulkload borrows or rejects,
and why.

**rsync** [Rsync96] [RsyncMan]. The receiver sends weak rolling checksums
and strong checksums for fixed-size blocks of its old file. The sender
slides the weak checksum over its own file to find matching blocks, and
sends only literal data and block references. By default, files are selected
by a quick check of size and modification time. *Borrowed:* the side that
already holds bytes should describe them before content moves.
`WantManifest` and `NeedChunks` invert rsync's roles. *Rejected:* per-run
re-derivation with no durable record of what the destination holds; fixed
block grids; and the size-and-mtime quick check, which cannot see a
same-size rewrite in the same tick.

**rclone** [Rclone]. A multi-backend file mover. It transfers whole files
and skips them by size and modification time or by checksum. Its local
backend clones only on APFS on macOS [Rclone]; on the Linux rig, `strace`
of a copy shows no clone and no sync of any kind (section 5.3).
*Borrowed:* rclone as shipped is the performance bar (R23), with its exact
flags and pinned build recorded in every sample. *Rejected:* whole-file
granularity, and no Git or SQLite awareness. A note for readers of gate
(a): rclone as shipped syncs nothing, so on Linux the gate compares a
durable native copy with a page-cache copy, and on an APFS volume it
compares native's copy with rclone's clone ([m0](../evidence/m0-2026-09-23.md));
both are open questions for the operator (section 4.4).

**GNU cp and the reflink default** [Coreutils90]. Since coreutils 9.0, `cp`
uses `--reflink=auto` by default and `copy_file_range` where available. It
is the like-for-like comparator for a clone, not rclone. *Rejected* for
bulkload: a clone writes the source (section 3.3). It also explains a rig
finding: a plain `cp` of the sealed corpus shared its extents.

**Git's pack protocol, bundles and ref transactions** [GitPackProto]
[GitPackObjects] [GitBundle] [GitUpdateRef]. In the pack protocol the
client lists objects it wants and has, and the server builds a pack of only
what is missing; a thin pack omits delta bases the receiver holds. A bundle
carries refs and objects for offline transfer and can name prerequisites.
`git update-ref --stdin` applies several ref updates together and updates a
ref only after verifying its current value. *Borrowed:* Git's own object
model and tools do all packing; v1's chained bundles are thin packs with
prerequisites; `upload-pack` is the estimate's oracle (P66); and
convergence moves refs only by compare-and-swap in Git's own transactions.
*Rejected:* negotiating haves over bulkload's own wire, which was
`carry_v2` (OI-1003-Q44).

**Git's racy index** [RacyGit]. Git treats an index entry as racily clean
when its cached `st_mtime` is the same as or newer than the index file's
own timestamp, and then compares content. *Adapted* as the racy-capture
rule (section 2.6): bulkload compares a seat's mtime and ctime with
wall-clock reads before the open and after the final stat, and records
nothing for a racy capture.

**casync and desync** [Casync17] [Desync]. casync cuts a serialized tree
into content-defined chunks, stores them in a chunk store named by digest,
and describes the image with an index; desync implements the formats in Go.
*Borrowed:* a chunk index as the manifest, and fetching only missing
chunks. *Rejected:* a chunk store as an intermediate, and whole-image
serialization, which assumes a quiescent tree.

**restic and borg** [Restic] [Borg]. Content-defined-chunking backup tools
with content-addressed repositories. borg's files cache skips unchanged
files by ctime, size and inode, and does not persist entries whose
timestamp is the newest it saw in that run, a racy guard of its own.
*Borrowed:* the files cache is the same idea as R25's stat-identity reuse.
bulkload adds mtime and ctime together, the device and source authority, a
window against the capturing host's clock, and a row committed only after
`Held{true}`. *Rejected:* a backup repository as the destination.

**bup** [Bup]. Writes Git packfiles directly and keeps a separate metadata
index. *Borrowed:* Git's storage for Git-like data, and change detection
in an index apart from content.

**Unison** [Unison04] [FileSync98] [UnisonRepo]. A bidirectional
synchronizer with an archive of each replica's last synchronized state, a
formal specification, and conflicts surfaced rather than resolved silently.
*Borrowed:* conflicts kept for explicit resolution, and a formal model as
part of the product. Estate convergence (section 2.8) is the closest
bulkload comes to Unison's problem: its ledger plays the archive's part, and
a destination change since the last landing is a conflict that refuses,
never a merge. *Rejected:* replica reconciliation; each run moves data one
way.

**ZFS send and receive** [ZfsSend]. Snapshot streams, incremental and
resumable from a token. *Borrowed:* resumable transfer from a durable token.
*Rejected:* the same filesystem and snapshots on both ends; bulkload takes
no source snapshot by design.

**LBFS** [LBFS01]. Content-defined chunks indexed so that data the other
side holds is not sent. *Borrowed:* the destination's chunk hints, re-read
and re-verified before use.

**FastCDC** [FastCDC16] [FastCDC20] and **BLAKE3** [BLAKE3]. *Borrowed
directly* for chunk boundaries and digests; BLAKE3's own tree is not used
as the manifest (section 2.3).

**SQLite's durability settings** [SQLitePragma] [SQLiteWAL]. In WAL mode
`synchronous=FULL` adds a sync of the WAL after every commit, and
`synchronous=NORMAL` stays consistent but may roll back the newest commits
after a power loss. *Borrowed* exactly that split: destination records stay
FULL (about 8 ms per commit on the rig), and only the source ledger's rows,
whose loss costs at most a re-read, run NORMAL (WP0(g)).

**Crash-consistency research.**

- *ALICE* [ALICE14] records an application's syscall trace and constructs
  crash states under an abstract persistence model, not exhaustively.
  bulkload borrows the approach (section 4.3), enumerates every subset up
  to a bound, and adds Darwin's drain and barrier rules.
- *CrashMonkey and B3* [B3-18] [CrashMonkey19] test file systems under
  bounded, exhaustively generated workloads. bulkload borrows bounding with
  exhaustive enumeration; their finding that mature file systems have crash
  bugs is a threat to validity (section 7).
- *Optimistic crash consistency* [OptFS13] separates write ordering from
  durability. Darwin's group mode makes that split in user space (a barrier
  per file, one draining commit per group), and Linux's batched seal makes
  a version of it with two `syncfs` calls per group (section 2.5).

**Linux durability primitives** [Syncfs] [SyncFileRange] [Rename2].
`syncfs` gives the guarantees of `fsync` on every file of one filesystem
and reports write-back errors only since Linux 5.8 (bulkload's floor is
5.17, section 2.5); `sync_file_range`
starts write-back without syncing metadata and is no durability primitive;
`renameat2` offers `RENAME_NOREPLACE` and `RENAME_EXCHANGE`. bulkload uses
the first as a seal, the second only as a hint, and the third for
no-replace publication and the superseding exchange.

**Formal methods and N-version checking.** TLA+ [TLA94] [Specifying02] with
TLC [TLC99] is the checker of record; Dhall [Dhall] types the catalogue, so
a forgotten mutation verdict is a type error; the Haskell explorers apply
the N-version approach [NVersion85] to the model, with the caveat of
[KnightLeveson86] (section 4.2).

**Property testing.** QuickCheck [QuickCheck00] introduced random testing
against stated properties. bulkload uses `proptest` [Proptest] and departs
from the usual practice in one way: every tier replays one fixed seed, so
the tested corpus is bounded and identical on every run (section 4.1).

## 7. Limits and open problems

**Performance is unproven.**

- **S1 fails on the rig of record**, and the stack has not reached the
  admission margin for another gated sample (sections 5.2, 5.3). Every
  later number is under load and never of record.
- **The stack is not on `main`.** #210 and everything stacked on it wait on
  the independent review OI-1003-Q173 requires.
- **The hand-off and R-N57.** Whether gate (a)'s "full protocol over a
  local socket" admits the loopback hand-off is an operator wording
  decision, open before any gated S1 sample.
- **The rig is narrow.** A 2-core 2015 laptop on Linux with one SATA SSD.
  Two questions on reading it are open (section 4.4), and macOS local-copy
  performance is covered only by a field confirmation.
- Gate (b)'s native multi-stream arm is not built; buffer reuse is not
  wired in.

**S2 is proven in part.**

- **The budget has never been measured** (#165); the alongside method is
  proven on synthetic traces only.
- **#188 is open**: four argued bypasses of the source-safe Git builder are
  pinned, not removed.
- **The CI root-user gap for P75.** CI runs as root, where the provider
  refuses to read a source, so in CI P75 proves the refusal only; the
  counted exceptions are exercised by local runs as an ordinary user (a CI
  log of 2026-10-07, run 37582012173, cited as a dated observation).
- **P84's kernel half is not in CI**: it needs a reflink filesystem, and
  `just p84-reflink` is on demand. Darwin has no `FIEMAP`, so APFS clone
  detection is unmeasured.
- **A third destination-side copy** (convergence's `publish_index`) may
  share extents with the live index on a reflink filesystem; whether Q165's
  answer covers it is not ruled.

**S3 has known holes.**

- The twins reading of inequality 2 is ignored and unruled.
- A changed seat on a filesystem without an atomic exchange does not
  converge, by ruling (Q100).
- Unchanged untracked payload is re-packed on every recapture (#174).
- The estate measurement predates every fix since 2026-10-04.

**S4.**

- 56 bare `IO` sites remain, half in the SQLite provider.
- #201: a finished `copy` intermittently returns a bare `IO(EPIPE)`.
- #183: one `GIT_INVENTORY_MALFORMED` misattribution (`chain::flatten`) may
  remain; #222 fixed the other.
- The transfer manifest is optional; without it the report covers only
  the transfers named.
- The offline SQLite provider verbs belong to no run, and the daily-work
  verdict (#40) is owed.

**S5 and convergence.**

- The exclude, configuration, shallow frontier, foreign nests and gitlinks,
  and omitted roots still refuse under a moving source.
- Nested worktree custody is the nested HEAD commit only: a switch between
  two branches at one commit is no custody change.
- Q149's three limits, and convergence only for landings with ledgers;
  `--adopt` is not built, and a bare repository has no native landing into
  a fresh root (the GFTB finding).

**Restore cost is undercounted (#147).** `estate-apply`'s space preflight
undercounts a chained apply, and no evidence file measures a full workspace
restore's time. The GFTB apply (864 s for 137 items) is the only field
number.

**The models are bounded and partial.** TLC checks small constants; the
explorers share the specification's reading of the code and cover two of
five modules; no model result is gated, so a code change that broke a
modelled rule is caught only if someone reruns `just tla-check` with an
updated specification. The GitCarry finding `MC_gc_live_rewritten` is an
open defect.

**The power-loss proof of a real copy is bounded.** Above 12 optional
mutations per crash point the checker tries a bounded family only. The WAL
template's seal is outside the trace on both platforms.

**Media durability is assumed, not shown.** `F_FULLFSYNC` is best effort
against sudden power loss [AppleDiskWrites], and `F_BARRIERFSYNC` needs
hardware support Apple guarantees only for its own SSDs [AppleFcntl]. The
rig's SSD has a volatile write cache. Mature file systems sometimes break
their documented semantics [CrashMonkey19].

**Counters are lower bounds.** Flush counts exclude SQLite's and Git's own
syncs; the pack-read counter misses page-cache hits.

**The trust model is cooperative.** Reuse trusts stat identity and the
capturing host's clock. Capture records and sidecars are not
authenticated; the capture record's hash check against the output's own
bytes is what guards it.

**Documentation drift in this tree.**

- `docs/design.md` still says the converge half is "held for an operator
  ruling" (section "Live union"), although `docs/slo.md` records
  OI-1003-Q144 as ruled; it still calls the salvage bounds unruled.
- `docs/slo.md` carries no amendment text for OI-1003-Q143 or Q154; their
  text is in the agent notes and on Linear TIN-4543 only. It still words
  S3's unchanged-estate bound on wall-clock and has no text for OI-1003-Q35.
- `docs/formal/README.md`'s opening still pins the transfer model to
  `adb9c66` and calls itself a model of "wire v5".
- GitHub issues #181 and #187 are open although their fixes are on `main`.

**The sample is narrow.** Two hosts, one operator, one estate, one field
carry.

## 8. What changed since 2026-10-07

The previous revision was pinned to `main` at `a80c63b`. This one describes
`main` at `a8d837a` plus the S1 stack. In order of weight:

1. **Convergence.** A rerun now brings a landed checkout up to the source
   when its ledger proves nobody changed it (#224, OI-1003-Q144), with its
   own TLA+ module that gated the merge, and three ruled limits (Q149).
2. **S4 covers a whole run** (#225): transfers join the closure report and
   gate, with Q148's `abandon` default for integrity failures (P86).
3. **S5 covers HEAD, index and stash moves** (#38, Q161; P87), so a pass
   raced by a working agent is custody, not a refusal.
4. **The SQLite snapshot seat** (#223, wire v6, Q146–Q148): databases cross
   the transfer as backups, reused when unchanged, superseded only when
   provably bulkload's, with `SqliteCarry.tla` modelled first.
5. **S1 moved and is still not met.** The throughput work and the levers
   (on the stack) took the rig's informational initial copy from about
   856 ms to about 400 ms against rclone's about 357 ms; the SOTA study
   bounds what is left; the fold's rig series (under load) puts the
   initial copy at about 1.04x rclone with the fold on.
6. **The clone is excluded and S2 forbids it** (Q164–Q166; P84); the rig's
   corpus was re-sealed unshared.
7. **S2's budget method**: alongside a live run against its own baseline
   (Q160), and, on `main` after this tree, a pass schedule fixed in advance
   (Q168). Still never measured.
8. **S3's ignored properties are green** (#186, #187 fixed by #203), and
   WP0(g) has landed (Q104).
9. **Git carry**: grouped chaining (P68), manifest reuse (P69, P70), native
   refs, inert origins; #181 fixed in the code.
10. **S2 proof closure**: the command registry (P76) and lock trace (P77).
11. **The formal model grew from two modules to five**, and the catalogue
    renders all of them.
12. **Field evidence**: the GFTB carry of 137 items, with a 0-byte rerun
    (section 5.4).
13. **The deep property tier is fixed-seed** and gated on its rows having
    run (OI-1003-Q78, Q81, R-N122); the 2026-10-07 text was wrong about
    random seeds.
14. **The bibliography** gained fifteen entries for the new mechanisms and
    measurements, each verified on 2026-10-16; [Rclone] was re-verified for
    the version now pinned and its Linux clone behaviour.

## 9. Open before publication

What stands between this text and a publishable paper (OI-1003-Q155):

1. **Commit the field and rig evidence into the tree.** Done on
   2026-10-16: the GFTB carry and reconcile receipts
   ([field evidence](../evidence/field-2026-10-15-gftb-carry.md)), the
   2026-10-15 levers series
   ([evidence](../evidence/r23-2026-10-15-mbp-13-s1lev-underload.md)) and
   the 2026-10-16 fold series
   ([evidence](../evidence/r23-2026-10-16-mbp-13-s1fold-underload.md)).
   Section 5's S1 series and field results are now recorded in the tree.
2. **The stack must land on `main`** (OI-1003-Q173), or this paper's
   S1-stack statements must be re-scoped to a branch at publication.
3. **Re-pin to current `main`** to take in #228 (Q168, Q169) and whatever
   follows.
4. **An S1 verdict to report.** Today the paper can only report a FAIL of
   record and an under-load trend. The fold's rig series has run, under
   load (section 5.3): about 1.04x plain rclone on the initial copy with
   the fold on, and the 1 % delta still slower than rclone. An S1 PASS
   needs Q143's margin first.
5. **One S2 budget measurement** alongside a real run (#165).
6. **A re-measurement of S3 on the estate corpus.**
7. **A recount of the property plan** against the code (section 4.1).
8. **Rulings the text depends on**: R-N57's wording for the hand-off; the
   rig's load bound and flushed-against-unflushed comparison; whether the
   transfer manifest is required; Q165's reach to the index copy.
9. **Documentation drift** (section 7) fixed at its source, so the paper
   need not report it.
10. **An external read.** Every reviewer so far has been an agent lane or
    the operator; a publication needs a reader outside the project.
