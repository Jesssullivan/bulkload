# bulkload: moving a live agent estate between machines

**Whitepaper, refreshed 2026-10-07** (OI-1003-Q51; first drafted 2026-10-03,
revised 2026-10-04). Proof package item one (OI-1003-Q7). Code base:
`origin/main` at `a80c63b` (2026-10-07), the merge of #189, which deleted
`carry_v2`. References use keys in square brackets (for example [Rsync96]);
each one resolves in the [bibliography](bibliography.md), with a note on how
it was verified. [Section 8](#8-what-changed-since-2026-10-04) lists what
this refresh changed.

This paper explains and argues. It is not a source of truth. Where it and a
normative document disagree, the normative document wins:

- [`docs/design.md`](../design.md) is the product contract;
- [`docs/slo.md`](../slo.md) defines S1–S5 and the completion bar, with
  every dated amendment;
- [`docs/formal/README.md`](../formal/README.md) holds every model-checking
  result;
- [`docs/evidence/`](../evidence/) holds every measured result.

Three rules keep the paper honest:

- **Every statement about a result cites a file on `main`**: an evidence
  file, `docs/slo.md`, `docs/formal/`, a test file or a dated agent note.
- **Every result carries one of three labels.**
  - *Gated*: enforced on every pull request by `just check-fast` and the two
    CI gates, or a benchmark sample taken under the R-N81 and R-N91 gate
    conditions.
  - *Informational*: measured or checked once and on record, but enforced by
    no gate. Every TLC and Haskell run is in this class, because no tier
    starts them.
  - *Pending*: not built, not run, or in review and not on `main`.
- **The paper quotes no SLO number.** It links [`docs/slo.md`](../slo.md)
  for them, so a dated amendment there cannot leave this paper silently out
  of date.

## Abstract

bulkload moves a developer's working estate from one machine to another
while the source machine stays in use. The estate is Git repositories with
linked worktrees, stashes, staged and unstaged work; agent transcripts and
SQLite stores; dotfiles and credentials. The target user runs coding agents
that keep committing, writing transcripts and updating databases during the
move. bulkload's goal is that the move is not an event: it is a sync that
can be rerun at any time. Each rerun should read only what changed, never
read a byte the destination already holds durably, and end with every
planned item accounted for.

The design has four parts:

- a streaming wire protocol (v5) in which the destination decides, per file,
  whether it needs anything;
- a digest-only source ledger that commits a capture only after the
  destination reports the bytes durable;
- a destination that stages, seals and publishes without replacement, and
  commits its records in groups;
- Git-aware and SQLite-aware capture, with typed refusals and drift custody
  in place of silent skips.

The claims are stated as SLOs (S1–S5 in [`docs/slo.md`](../slo.md)). The
state of the proof on `main` at `a80c63b`, in one paragraph each:

- **Proven and gated.** The crash and power-loss proofs, the S3 headline
  properties on the file path, the Git pack laws, the Git decision core
  against its reference rows, the S4 disposition ledger for Git items, the
  SQLite source exceptions and R25's strict reading all run in `check-fast`
  on fixed seeds (section 4).
- **Checked on demand.** Two TLA+ modules are model-checked with TLC, within
  small bounds. A Dhall catalogue renders their configurations, and a
  Haskell explorer reproduces TLC's state counts (section 4.2). No tier or
  CI gate runs them.
- **Not met.** **S1 has no gated sample.** No benchmark of the wire v5
  engine has been taken under the gate conditions. The only S1 numbers since
  wire v5 are informational, taken under load on 2026-10-04 (section 5.2).
  The S2 budget has never been measured. S4 does not yet cover transfer and
  SQLite outcomes, and S5 still refuses HEAD and index movement.

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
requirements. They are paraphrased here and stated exactly, with their
budgets, in [`docs/slo.md`](../slo.md):

- **S2:** bulkload must not interrupt the source: no locks, writes or
  signals on the source, background priority, and a measured latency and
  load budget. The stated exceptions are all SQLite's (section 3.3).
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
   `applied`, `referenced-only` or a typed, reviewed refusal, and nothing is
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
  the default design choice. The engine does use raw syscalls
  (`src/io/sys_*.rs`). Buffer reuse is not wired in: the slab pool and the
  fused chunker (`src/io/buf.rs`, `src/io/chunker.rs`) are compiled under a
  non-test `allow(dead_code)` whose stated reason defers wiring or deleting
  them to the gate (a) evidence (`src/io/mod.rs`). So no S1 number in this
  paper includes buffer reuse.

The agent's cargo features are `fault-injection` and `io-trace` only
(`crates/bulkload-agent/Cargo.toml`). The `m1-spike` feature went with
`carry_v2` (section 2.7).

### 2.2 Wire v5

The transfer wire is protocol 5, a hard cut with no dual stack (R-N59,
R-N118). The schema text `WIRE_SCHEMA` and its BLAKE3, `wire_id`, live in
`crates/bulkload-proto/src/frame.rs`. A peer whose `Open` names another
protocol or another `wire_id` is refused before any other frame. Each frame
is a 4-byte big-endian length, a tag byte and a body:

- tag 1 is one postcard control message, with nothing after it;
- tag 2 is a content chunk: a fixed 64-byte header (entry, chunk index,
  size, offset, digest), then the payload;
- tag 3 and control variants 12 to 19 are reserved for a Git sub-stream and
  refused. Since `carry_v2`'s deletion no code builds or consumes them. They
  stay in the schema so that `wire_id` does not move
  ([`docs/design.md`](../design.md), "Reserved Git sub-stream").

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
  `crates/bulkload-agent/src/io/sys_posix.rs`). A directory swapped for a
  symlink is refused, never followed. Depth and path-length caps refuse a
  subtree as a typed value and carry its siblings (#110, #129). Content is
  read with `pread`, never mapped.
- **Send.** The source reads the file once, cuts it into content-defined
  chunks, hashes each chunk and streams it. The destination verifies every
  chunk against its digest, writes it at its offset, and checks coverage
  and the manifest root before the output joins a group commit.
- **WantManifest and NeedChunks.** The destination chooses this for a
  regular file it cannot `Reuse` when the output path already exists, when
  published outputs hold chunks, or when the start-of-session sweep salvaged
  any temporary. The source sends its manifest, from its ledger without
  reading when the stat identity is recorded. The destination fills what it
  can from verified local chunks, then asks only for the missing indices. A
  fresh manifest is built from one read kept in memory (`RETAIN_BYTES`); a
  larger file is streamed as for `Send`, so no seat is read twice in a
  session. `docs/design.md` omits the salvage condition; the formal model's
  [README](../formal/README.md) ("Code and design disagreements") records
  that difference.
- **Credit.** The destination grants 16 MiB of payload credit
  (`CREDIT_WINDOW`) and returns it as it writes. The source never has more
  than the grant in flight, nor more than 16 entries' content.
- **Held.** The destination answers every `End` with `Held`. `Held{true}`
  means the output's group commit has returned: file and directory sealed,
  then the store commit. Only then does the source commit the capture to its
  ledger (section 2.4).

A known limit: a fresh destination is streamed with no cross-file
deduplication, so a chunk repeated across files crosses the wire once per
file ([`docs/design.md`](../design.md), "Wire v5").

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
(`crates/bulkload-agent/src/transfer_store.rs`). A row holds the manifest
(chunk digests, sizes and `manifest_root`), never content bytes (R-N58).
Earlier engines kept a source-side byte pack for resume; M0 counted one copy
writing that pack and reading it all back, W3 removed the read-back and W4
removed the pack ([m0](../evidence/m0-2026-09-23.md),
[w3](../evidence/w3-2026-09-23.md)). A power-loss test asserts that a
`copy` writes no source pack (`the_source_writes_no_content_bytes` in
`crates/bulkload-agent/tests/power_loss.rs`).

A row is written only when three conditions all hold:

- the capture's final stat check passed, so a seat rewritten during its
  read leaves no row (R-N86);
- the capture was not racy (section 2.6);
- the destination answered `Held{true}`.

The third condition is what makes R25 safe. A committed capture means the
destination holds the bytes durably, so a resume reuses or adopts the
output and never reads the seat again (OI-1001-Q15).

The model checking found that this ledger does not carry R25 on its own.
Across a crash between the destination's commit and the source's row, only
the destination's `Reuse` decision keeps the seat from being read again
([formal README](../formal/README.md), "Mutations",
`src_ledger_carries_r25`). That is why WP0(g) may relax the source ledger's
durability (section 3.2).

### 2.5 Destination: stage, seal, publish, group commit

The destination never overwrites an existing file; publication is
no-replace (`crates/bulkload-agent/src/materialize.rs`,
`src/io/durable.rs`). Each output is written under a tagged temporary name,
where the tag derives from the destination store's random authority. Its
group commit then:

1. seals each file: `F_BARRIERFSYNC` on Darwin, `fsync` elsewhere
   (`seal_file`);
2. publishes it under its final name without replacement, using
   `renameat2(RENAME_NOREPLACE)` on Linux or `renameatx_np(RENAME_EXCL)` on
   Darwin, or a link and unlink where the filesystem has neither (R-N119;
   `publish_noreplace` in `src/io/mod.rs`);
3. seals each touched directory once (`seal_dir`);
4. fully flushes each touched device other than the store's own;
5. commits the group's records in one SQLite WAL transaction with
   `synchronous=FULL` and `fullfsync=ON` (`src/io/durable.rs`). On Darwin
   that commit's `F_FULLFSYNC` drains the store's device, so a group whose
   files share that device needs no other device-cache flush.

A group closes on a file count, a byte count or a short idle time
(`GROUP_FILES`, `GROUP_BYTES`, `GROUP_IDLE`). A directory the engine creates
gets a tagged temporary name, its record is bound to the new inode, and it
is renamed into place without replacement (R-N102). The engine never adopts
a directory it did not create (R-N78). `--durability=strict` fully flushes
every file instead, for comparison.

Since #198 a staged file also carries a **capture record** before its seal:
an extended attribute that names the walked row, the manifest root and the
size (`src/transfer/unrowed.rs`). Section 3.1 says what it is for.

Space is checked before writing. An entry decided `Send` or `WantManifest`
reserves its size until its `Held`, and a write that would leave less than
`--min-free-percent` free is refused `DESTINATION_SPACE_INSUFFICIENT` as a
value. The session continues ([`docs/design.md`](../design.md),
"Durability").

### 2.6 Crash resume and racy captures

A rerun after an interruption, or over a store an older engine wrote, meets
six kinds of leftover state:

- **Committed captures and output records.** A seat whose stat identity is
  unchanged is answered `Reuse`, and the source reads nothing.
- **Durable outputs without a row** (#169, #198). A crash between an
  output's publish and its row commit, or a failed group, leaves bytes at
  the final name with no row. Since #198 the destination adopts such an
  output from its capture record by hashing its own bytes, and the source
  reads nothing ([`docs/design.md`](../design.md), "Performance"). An output
  with no record, or whose bytes no longer match it, is verified against the
  source's manifest or read again.
- **Temporaries.** The sweep acts on a temporary only when it carries this
  store's tag, matches the name grammar exactly, and has the expected kind
  and owner (R-N79). It keeps orphaned file temporaries as a chunk source
  for the new session. These save wire bytes only: no recorded capture's
  bytes live only in a temporary.
  - The end of the session removes them all, except those that an entry
    refused in that session had staged chunks from. Those are kept for the
    retry and reported as left (#124, #154, OI-1002-Q33).
  - Kept salvage is bounded per session (`SALVAGE_KEEP_FILES` and
    `SALVAGE_KEEP_BYTES` in `transfer.rs`). A temporary past the bound is
    removed and refused as a value, `SALVAGE_BOUND_EXCEEDED`. The values are
    ratified in [`docs/slo.md`](../slo.md) (OI-1003-Q24). `docs/design.md`
    still calls them unruled defaults, which is stale.
- **Rows from before the racy guard** (#125, OI-1003-Q26). A store this
  engine creates carries a `racy_guard` marker from its first commit. A
  store without it has its `captures` and `outputs` rows invalidated in one
  transaction on its first writable open, counted in
  `transfer_legacy_rows_invalidated`. Each such seat is read once more,
  which R25 allows. Chunk hints are kept, so content the destination still
  holds does not cross the wire again.
- **A failed group.** A group whose seal or store commit fails (a full disk,
  for example) answers each entry `Held{false}`. Neither store records
  them (#100). Since #198 the next run adopts those of its outputs that were
  already published and carry a record.
- **A lost source store.** A source store that was lost and recreated has a
  new authority, so every row key changes. The capture record holds no
  authority, so outputs that carry one are still adopted without a read
  ([formal README](../formal/README.md), `MC_r25_strict_unsealed`).

**Racy captures.** A seat whose mtime or ctime falls within a 2 s allowance
of the clock read before it was opened, or later than the clock read after
its final stat check, is racy (`RACY_GRANULARITY_NS` in
`crates/bulkload-agent/src/git_carry.rs` and `src/transfer.rs`). This adapts
Git's racy-index idea [RacyGit] to a mover (section 6 says how it differs).
A same-size rewrite in that window can keep its stat identity. A racy
capture is sent and published, but neither side records it and it gets no
capture record, so the next run reads it again (#86, R-N76). Correctness
comes before the zero-reread claim.

### 2.7 Git estate carry: one engine

Git state is carried as Git objects, not as files under `.git`
(`crates/bulkload-agent/src/git_carry.rs`, `src/estate.rs`,
`src/git_carry/`). **v1 is the engine** (OI-1003-Q15, Q44). The second
engine, `carry_v2`, with its ingest, its journal, its fault points and the
W6 M1 spike, was deleted by #189 on 2026-10-07 (`a80c63b`). Tag
`carry-v2-final` holds the last `main` that had them
([`docs/slo.md`](../slo.md), "Amendment 2026-10-05"). The read-only
`git-carry-estimate` verb stays.

**How a capture works.** v1 captures each repository into a private
repository that reads the source's objects through
`objects/info/alternates` (`prepare_private` in `git_carry.rs`), and writes
the history as a bundle [GitBundle]. A capture carries:

- refs, objects and real stash commits, including binaries and untracked
  files;
- the index bytes, `info/exclude`, configuration, the symbolic HEAD and the
  shallow frontier;
- working-tree dirt, ignored files and worktree administration, with paths
  translated for the destination.

A bare repository is carried as ref custody (#172). Import preserves
divergence. It never changes an active checkout's HEAD, index or working
bytes. Restore verbs check a bundle's in-band drift marker from its headers
before they write anything.

**Hardening.** Every Git child of the carry and the estimate is built from
one table, `git_carry::git_env` ([`docs/design.md`](../design.md), "Live
union"): no hooks, fsmonitor, automatic gc or maintenance, optional locks,
lazy fetch, system or global configuration, or replace objects; the C
locale; and a discovery ceiling. A partial-clone source refuses
`GIT_SOURCE_PARTIAL_CLONE` before any other read. Section 3.3 lists what
this does and does not prove, including issue #188.

**What v1 gained since the first draft.** Each item below is on `main`.

- **Auto-prerequisite chains** (#146). With no plan base, a capture that
  follows a retained capture of the same checkout declares that capture's
  tips as its bundle prerequisites, and packs only what is new. A `.prior`
  sidecar, durable before the record, names each link. Depth is capped
  (`CHAIN_DEPTH_LIMIT` in `git_carry/chain.rs`); the capture after the
  deepest link re-bases to a self-contained bundle, which re-packs the whole
  history. Restore checks every link and flattens the chain into one bundle
  before it applies.
- **A read-only source object store** (#172). Git re-stamps the timestamps
  of any existing copy of an object it is asked to write, alternates
  included. So the private repository's writers now run against a write
  store that borrows nothing (`git_env::WRITE_STORE`), and readers see that
  store, then the source ([`docs/design.md`](../design.md), "Source
  safety").
- **Thin group bases** (#177, Q42 lane L1). Grouped items (those that share
  a plan base) and chained items now share one writer, which keeps the
  walk's edge lines as preferred delta bases. A small edit to a large
  tracked blob packs as a delta against the copy the prerequisites hold.
  Two laws state this, as fixed table rows
  (`crates/bulkload-agent/tests/git_group_minimality.rs`):
  - **P64 PACK-MINIMALITY**: a bundle packs nothing its prerequisites'
    marked trees already hold;
  - **P65 THIN-DELTA**: an edited large blob packs as a delta, and every
    restore completes the thin pack byte for byte.

  The same file pins what P64 does not bound: content held only deeper in
  history, and unchanged untracked payload, which is packed again on every
  pass that recaptures its item (#174).
- **The ref-table capture format** (#182, OI-1003-Q54, Q55). A capture's
  header no longer lists every carried ref. The refs travel in a
  content-addressed ref table commit, with one tip ref per distinct object,
  so the header grows with distinct objects, not refs
  (`src/git_carry/ref_table.rs`; [plan](../plans/2026-10-05-v1-header.md)).
  A header over its size bound refuses `GIT_INVENTORY_OVER_CAP`, and writers
  measure before they write. An older build refuses a ref-table capture
  before it writes a ref; there is no mixed-version apply. Section 5.2
  reports the real 123k-ref repository this was proven on.
- **A pure decision core** (#191, OI-1003-Q43, lane L6a). What a capture
  decides before it exports is one pure, total function,
  `git_carry::decide::decide` (`src/git_carry/decide.rs`): a reuse hit, a
  typed refusal, or an export with a basis, a depth, a rebase and a reuse
  offer. It replaced logic spread over three places. A Haskell reference
  copy (`docs/formal/hs/GitCarryCore.hs`) renders 363 pinned rows
  (`crates/bulkload-agent/tests/data/decide_rows.tsv`), and property P67
  checks the Rust function against every row
  (`src/git_carry/decide_tests.rs`). The rows cover three policies: 79 for
  v1 as it runs, 85 for chaining under a plan base, and 199 for the Q46
  re-root policy. **The callers run only the v1 policy** (`Policy::V1`) and
  refuse a plan v1 cannot carry out, so what the code does with the other
  two policies' decisions is unchecked until their lanes land
  ([formal README](../formal/README.md), "What GitCarry does not prove").

**In review or not built.**

- **Grouped chaining under a plan base** (P68, lane L6b) is PR #199. It is
  open and **not on `main`**.
- Manifest reuse and sidecar order (P69, P70, lane L7) and the re-root
  policy with CORPUS garbage collection (P71, lane L8, OI-1003-Q46) have no
  code. The formal README lists their symbols as pending.

### 2.8 SQLite through the backup API

Live provider databases are captured with SQLite's online backup API, never
by copying `-wal` or `-shm` bytes (`snapshot` in
`crates/bulkload-agent/src/provider_sqlite.rs`; `AGENTS.md`, "Estate
rules"). The capture:

- opens the source read-only through normal WAL-aware access, never
  `immutable` mode, with a zero busy timeout;
- copies pages in bounded steps, within a step budget;
- on lock contention, refuses `SQLITE_STATE_CHANGED` at once instead of
  waiting on the live writer;
- converts the snapshot to `journal_mode=DELETE`, so it is one portable file;
- runs `quick_check` and `foreign_key_check` before the snapshot may be used.

The backup API holds a read lock on the source only while a step reads, and
restarts when another connection writes mid-backup [SQLiteBackup]. The step
budget turns an endlessly restarting backup into a typed refusal.

This read is where S2's stated exceptions live: one lock and two writes,
plus one refusal. Section 3.3 states them with their rulings. Composition
preserves unique rows on both sides and keeps conflicting snapshots for
explicit resolution. A snapshot is never installed over a live database.

A limit that the S3 measurement made visible: `snapshot` has no ledger, so
it reads the whole database on every pass
([S3 evidence](../evidence/s3-estate-sting-2026-10-04.md), "Verdicts").
SQLite has no incremental path yet.

### 2.9 Typed refusals, outcomes and the disposition ledger (S4)

Refusals are values of one taxonomy (`crates/bulkload-proto/src/refusal.rs`).
A refusal crosses the wire and lands in receipts by its stable code.

**Typed at the source** (#150, #151; [`docs/design.md`](../design.md),
"Completion"):

- every taxonomy code is raised by product code; a source scan requires it
  (`crates/bulkload-agent/tests/refusal_taxonomy.rs`);
- no blanket conversion turns an OS or codec error into a refusal. Each
  site names itself with `.refuse_at(site)` (`src/refuse.rs`);
- a Git child that exits non-zero refuses `GIT_CHILD_FAILED` with a
  `stderr_class=` from a closed set. Its stderr is classified, never echoed
  or kept (R-N121);
- 57 non-test sites in 15 files still raise `IO` with no errno, 28 of them
  in the SQLite provider code. A test holds each file's count to an
  allowlist that may only shrink (`IO_NONE_ALLOWLIST` and
  `bare_io_none_sites_only_shrink` in `tests/refusal_taxonomy.rs`).

**Typed outcomes and the disposition ledger** (#195, WP3 PR 3). This is new
since the first draft, which listed it as future work.

- Outcome records are a typed `Outcome` with `Refusal{code, site, errno}`,
  persisted with postcard and decoded with no bytes left over
  (`src/outcome.rs`). A reader maps legacy string records. Closure matches
  the enum; it no longer parses display text.
- `closure-report` ends every planned item as `applied`, `referenced-only`,
  `refused` (a typed refusal that a review disposes),
  `refused-pending-review`, or `unaccounted`
  (`crates/bulkload-agent/src/closure.rs`).
- `closure-dispose` records an operator review (accept, re-carry or abandon,
  with reviewer and date) in a disposition ledger (`src/disposition.rs`).
  The ledger is bound to the plan's bytes and the SOURCE label. An item
  review is bound to the refusal instance it reviews: the item's current
  capture and its outcome record.
- The report's `gate` passes only when every planned item is accounted and
  every typed refusal carries a disposition. A bare `IO` or `FRAME_CODEC`
  names no cause, so no disposition can name it and no attestation can
  close it. A refusal whose code has left the taxonomy fails closed
  (OI-1003-Q74, Q75).

**What this does not cover yet.** Closure covers the estate ledger, which
is Git items. Transfer and SQLite provider outcomes are not in the ledger;
they join in WP3 PR 4. Until then S4 is proven for estate items only
([`docs/slo.md`](../slo.md), "Amendment 2026-10-06: S4 proof status"). The
charter's daily-work verdict on the destination (#40) is also still owed.

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
  never a reuse hit, even if its sidecar is lost. The next pass extends it.
- **An object-store rewrite is drift too.** When a Git child of the pass
  fails and the source's pack listing has changed (a `gc`, `repack` or
  `prune` raced the pass), the item is `deferred-with-drift` with no capture
  record, and the next pass captures the rewritten store
  (`an_object_store_rewrite_under_the_pass_is_drift_custody` in
  `src/estate.rs`).

On the file path, a seat rewritten during its read is refused
`SOURCE_CHANGED_AFTER_SNAPSHOT` for that seat alone. Its bystanders are
carried with correct ledger rows, and the victim gets no row (`live_writer`
scenarios in `crates/bulkload-agent/tests/fault_harness.rs`).

**What still refuses on `main`.** This is unchanged since the first draft
([`docs/design.md`](../design.md), "Drift"):

- HEAD and index movement inside a captured worktree refuse
  `GIT_AUTHORITY_CHANGED`. OI-1003-Q11 rules them drift classes; issue #38
  is open and no code implements it.
- Configuration, the shallow frontier, nested worktree custody and the
  omitted rebuildable roots moving under a pass also refuse. No ruling makes
  them drift classes.
- Directory-shape drift refuses fail-closed.

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

**How it is held.** `Held` gates every ledger commit (section 2.4). `Reuse`
answers unchanged identities (section 2.2). `WantManifest` lets the
destination fill from its own verified chunks. The racy rule (section 2.6)
is the deliberate exception: it trades one re-read for correctness.

**Two readings, and which one is the SLO.** "Holds durably" can be read two
ways, and OI-1003-Q40 ([`docs/slo.md`](../slo.md), "Amendments
2026-10-04") settles which is the obligation:

- **The committed-row reading is the SLO's obligation.** A byte is held when
  a committed destination row proves it durable. In the model this is
  `R25_NoDurableReread`, and it holds within the model's bounds.
- **The strict reading** also counts bytes that are durable at the final
  path with no row. It is not part of the SLO. Since #198 it also holds in
  the model (`R25_StrictNoDurableReread`), through the capture record's
  adoption, **within stated limits**:
  - only a non-racy capture gets a record;
  - the record is an extended attribute. A filesystem without them, or an
    adopted output whose mode gives its owner no write permission, keeps no
    record, and its unrowed bytes are read again. Both are counted
    (`transfer_capture_records_unset`, `transfer_unrowed_unproven`);
  - the model assumes the record can be written. It does not model its loss;
  - an output published before #198 has no record.

On the code, three tests check the strict reading: property P74
(`p74_unrowed_outputs_are_adopted_without_source_reads` in
`src/transfer/tests.rs`), the power-loss proof
`an_unrowed_output_is_adopted_without_source_reads`
(`src/materialize/adoption_power_loss.rs`), and
`an_output_adopted_against_a_manifest_carries_its_capture_record`. All three
are gated.

**Limits.** A stat identity is a reuse key, not a content digest. The racy
reference is the capturing host's wall clock, not the filesystem's: a source
on NFS or SMB whose server clock runs more than 2 s behind the capturing
host can defeat the guard ([`docs/design.md`](../design.md), "Known limit:
clocks"). The file path's byte counters do not see reads made by Git child
processes. The Git children that pack a capture are counted from their own
resource usage, as a lower bound that misses page-cache hits.

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
(R-N103), with a stricter per-object model available.

**The store's own directory (#161): fixed.** The first draft reported an
open defect: a fresh state root was never sealed, so a power loss could lose
a whole store, mint a new authority and re-read everything. #166 fixed it.
A store's state root and its database entry are now sealed before
`Store::open` returns, and a store without the `root_sealed` setting is
sealed again on open ([`docs/design.md`](../design.md), "Durability";
[`docs/slo.md`](../slo.md), OI-1003-Q37). The model row
`MC_store_root_unsealed` still fails by design: it documents the old
behaviour as a finding.

**WP0(g): ratified, not implemented.** OI-1003-Q37 lets source-ledger *row*
commits run with relaxed durability, on conditions, because the model shows
that losing a source row costs at most a re-read
([`docs/slo.md`](../slo.md), "Amendments 2026-10-04"). The code has not
taken the relaxation: both stores still run `synchronous=FULL` and
`fullfsync=ON` (`configure_sqlite` in `src/io/durable.rs`). One gap stands
between the model and the code here. The model's ledger commit never fails;
in the code a failed ledger commit fails the whole session (#163, open).

### 3.3 S2: source safety

**Statement** (paraphrased; the normative text and the budget are S2 and
its amendments in [`docs/slo.md`](../slo.md)). bulkload takes no lock on a
source repository, writes nothing to the source, signals no process, and
runs at background priority (OI-1003-Q5, Q9).

**Why it matters.** The source is a live workstation. A mover that takes
`index.lock`, triggers `gc`, runs a hook, fetches from a promisor remote, or
competes for I/O at normal priority interrupts the agents it was meant to
leave alone.

**The counted exceptions.** All of them are SQLite's, and all are bounded
([`docs/design.md`](../design.md), "Live union"):

| Exception | Ruling | Counted? |
|---|---|---|
| The backup API's shared read lock on the source database | OI-1003-Q16 | **Not yet.** The ruling says "counted"; no lock count or lock time exists. `docs/design.md` records this as an open gap. |
| Creating or touching the source's `<db>-shm` wal-index | OI-1003-Q36 | Yes: `source_wal_index_touched` |
| Creating an empty `-wal` where none existed | OI-1003-Q72 | Yes: `source_wal_created` |

The main database file must stay byte-identical with its timestamps
unchanged, and a `-wal` that existed before the read must stay
byte-identical.

**The refusal to read as root** (OI-1003-Q76). With an effective uid of 0,
every provider verb that opens a database to read it refuses
`SQLITE_SOURCE_AS_ROOT` before it opens anything
(`refuse_source_read_as_root` in `src/provider_sqlite.rs`). The mechanism:
run as root, SQLite re-applies the database's owner to the `-wal` and
`-shm` it opens (`fchown`), and skips that call as any other user. Measured
on sting, the same read-only open and backup left an existing `-wal`'s
ctime unchanged as uid 1000 and moved it as uid 0. That is a source
metadata write that no ruling admits and neither counter sees
([`docs/slo.md`](../slo.md), "Amendment 2026-10-06: no SQLite source read
as root").

**What is proven on `main`, and how.**

- **The file path.** P-S2 (`a_run_leaves_the_source_lstat_census_unchanged`
  in `src/transfer/tests.rs`) runs a copy, its warm rerun and a refused copy
  over generated trees. Every source node must keep its lstat identity, and
  the node set must stay the same. Gated.
- **The Git carry's object store.** The first draft reported that
  `estate-capture` moved the timestamps of existing source objects (#162).
  #172 fixed it (section 2.7). P34's Git leg
  (`p34_a_capture_leaves_the_source_lstat_census_unchanged` in
  `src/git_carry/source_inert_tests.rs`) holds every `lstat` field of every
  source node fixed across a capture and two incremental passes. Gated.
- **The SQLite exceptions.** P75 SQLITE-SHM-EXCEPTION
  (`crates/bulkload-agent/tests/sqlite_wal_index.rs`) runs generated
  WAL-mode sources in five shapes and asserts exactly the two counted
  writes and nothing else, with counter values fixed by an oracle
  independent of the implementation. As root it asserts the refusal, then
  tries to re-run the whole property in a child that has dropped
  privileges. Gated, **with a coverage gap in CI** (section 7).
- **Background priority.** Source-side verbs enter background CPU and I/O
  priority before any thread or child exists; `--priority=normal` is the
  recorded opt-out. An io test checks that threads and children inherit it
  (`background_priority_is_inherited_by_threads_and_children` in
  `src/io/tests.rs`). Gated. The idle I/O class only helps where the
  kernel's scheduler honours priority classes [IoprioSet], which is why the
  budget is measured, not inferred.
- **The model.** `S2_TypedSourceAccess` and `S2_BackupLockBounded` hold in
  `MC_s2`, at the protocol level only. The model's typed access holds by
  construction; it cannot show that the binary issues no other syscall
  ([formal README](../formal/README.md), "Not proven here").

**What is open.**

- **Issue #188: four child-process sites bypass the hardened path.** Four
  builders of child processes do not go through `git_carry::git`, so the
  single hardening table does not cover them. A registry allowlist (P76) and
  a traced lock sibling of P34 are in PR #197, which is open and not on
  `main`. Until it lands, "every Git child is built from one table" is a
  convention for those four sites, not a tested property.
- **The measured budget has never been run** (#165). The instrument is on
  `main` (`crates/bulkload-bench/scripts/s2_budget.py`, #168, OI-1003-Q34).
  No gated run exists.
- **The Q16 lock is not counted** (the table above).
- P34's traced form (no mutating syscall and no lock event beneath the
  source root, over copies, carries and estimates) and P35's per-thread
  probe are planned, not built
  ([property-test plan](../plans/2026-10-03-property-test-plan.md)).

## 4. SLOs and the proof method

The definitions of S1–S5, with every number they set, live in
[`docs/slo.md`](../slo.md). This section says which instrument carries each
claim. There are four kinds of instrument: property tests, the formal
model, crash and power-loss traces, and benchmark gates. Section 4.5 says
what CI runs, and section 4.6 gives the state per SLO.

### 4.1 Property tests: fixed seeds, no fuzzing

[`docs/slo.md`](../slo.md) ("Proof package") asks for property tests, not
fuzzing, and a bounded tested corpus. The method follows QuickCheck
[QuickCheck00] through `proptest` [Proptest]. The
[plan](../plans/2026-10-03-property-test-plan.md) sets the conventions:

- **One fixed seed in CI.** Every property takes its configuration from one
  helper, `test_support::prop_config`, so each CI run replays the same
  bounded corpus. No persistence file exists.
- **A deep local tier.** `BULKLOAD_PROPTEST_DEEP=1` switches the helper to
  random seeds at 20 times the cases. A failing deep run prints its seed,
  and the developer pins that case by hand.
- **Finite matrices are enumerated**, not sampled.
- **A guard test** (#194). `crates/bulkload-agent/tests/prop_seed_guard.rs`
  scans every workspace Rust file and fails on any proptest configuration
  other than the helper call. It is a text check, not a parse, and it errs
  towards refusing valid code. Its exemption list only shrinks; one file is
  left on it, `tests/refusal_taxonomy.rs`, which sets its own fixed seed.
- **Mutation evidence, not coverage.** A test is retired only when its
  subsuming property catches the specific mutant the old test was written
  for. New properties record the scratch mutants that turn them red.

On `main` at `a80c63b`, `proptest!` or a `TestRunner` appears at 25 sites in
16 files, the guard aside. The first draft counted nine.

**The honest count against the plan.** The plan's catalogue numbers
properties P1 to P63, with P38 folded into P34, and six rows added later
(P66, P67, P72, P73, P74, P75). Four were retired whole with `carry_v2`
(P48, P49, P50, P59). That leaves 64 planned properties.

- **12 of the 64 are on `main`**, seven whole and five in part:

  | Property | Claim | State on `main` | Test file |
  |---|---|---|---|
  | P18 HINTS | wire bytes are the size less verified survivors | end-to-end half only; store half open | `tests/s3_transfer_resume.rs` |
  | P19 RACY | a racy capture is sent and never a reuse | file half only; tick-boundary triples and the Git predicate open | `tests/s3_transfer_resume.rs` |
  | P21 WALK-RESUME | a rerun reads only changed seats (S3 headline) | landed; some clauses ignored, see below | `tests/s3_walk_resume.rs`, `tests/s3_transfer_resume.rs` |
  | P23 RESUME + READ-ONCE | a resume after any cut reads only unapplied files | landed; one dimension ignored, see below | `tests/s3_transfer_resume.rs` |
  | P34 SOURCE-INERT | no source mutation (S2) | in part: the lstat census legs (file path and Git object store); the traced form is not built | `src/transfer/tests.rs`, `src/git_carry/source_inert_tests.rs` |
  | P35 BACKGROUND-PRIORITY | workers run at background priority | in part: inheritance is tested; no per-thread probe | `src/io/tests.rs` |
  | P66 ESTIMATE-DAG | the estimate equals the `upload-pack` oracle | landed | `tests/git_estimate_dag.rs` |
  | P67 DECISION-CORE | `decide` equals the reference on 363 rows, and is total | landed | `src/git_carry/decide_tests.rs` |
  | P72 OUTCOME-ROUNDTRIP | outcome records round-trip strictly | landed | `src/outcome.rs` |
  | P73 CLOSURE-DISPOSITION | green iff every refusal is dispositioned | landed | `src/closure.rs` |
  | P74 R25-STRICT-ADOPT | unrowed outputs are adopted without source reads | landed | `src/transfer/tests.rs` |
  | P75 SQLITE-SHM-EXCEPTION | a snapshot writes only its counted sidecars | landed | `tests/sqlite_wal_index.rs` |

- **52 are planned and not built in the plan's form.** Two of them, P1 and
  P2, exist only as the older pool and chunker model properties that the
  plan extends.
- **Outside the plan's catalogue**, on `main`: P64 and P65 (section 2.7),
  the REFS-SCALE law of the ref table
  (`src/git_carry/refs_scale_tests.rs`), and the unnumbered properties from
  earlier work packages (the source census P-S2, auto-prerequisite chains,
  the salvage bound, the child-drain helper, `.refuse_at`'s errno, and
  never-echoed stderr).
- **Named but not on `main`:** P68 (PR #199, in review), P69, P70 and P71
  (no code), and P76 (PR #197, in review).

**Three S3 properties are ignored against open issues.** They are marked
`#[ignore]` in `tests/s3_transfer_resume.rs` because they are red on
`main`:

- `p21_changed_seats_cross_as_absent_chunks_and_converge`, against **#187**:
  a changed source file is refused `GIT_DESTINATION_OCCUPIED` on rerun
  instead of superseded. This is blocked by WP0(d), superseding publish,
  which has no code;
- `p21_with_refused_seats_an_unchanged_rerun_reads_nothing` and
  `p23_with_refused_seats_a_further_rerun_reads_nothing`, against **#186**:
  a refused SQLite seat is re-read for its 16 sniff bytes on every run.

What does hold with refused seats is stated as separate green "residue"
properties, to be deleted when #186 is fixed. A fourth test, a pinned shape
for twin added seats, is ignored as unstable and has no issue yet; it needs
a ruling on how to read S3's second inequality.

### 4.2 The formal model: TLA+, a Dhall catalogue, a Haskell cross-check

[`docs/slo.md`](../slo.md) asks for a formal model of wire v5, `Held`,
ledger commit and resume. OI-1003-Q32 makes it a hybrid of three tools, each
with one job. All of it is in [`docs/formal/`](../formal/README.md), whose
README is the source for everything below.

| Role | Tool | Job |
|---|---|---|
| Checker of record | TLA+ with TLC [TLA94] [Specifying02] [TLC99] | Every verdict, count and counterexample cited as a result is TLC's. |
| Typed catalogue | Dhall [Dhall] (`docs/formal/catalogue/`) | Holds every configuration's constants and expectation, every mutation's verdict and every property's traceability row. It renders `configs.tsv` and every `MC_*.cfg`. |
| N-version cross-check | Haskell (`docs/formal/hs/`) | A second encoding of each specification that must reproduce TLC's state counts and mutation verdicts. For Git carry it also holds the reference decision core. |

**None of this is gated.** `just tla-check`, `just tla-render` and
`just formal-nv` are standalone recipes. `check-fast` and CI never start TLC
or GHC, to keep CI slim. Every result in this section is therefore
*informational*: a dated run of record.

**The catalogue's guarantees.** Two checks must pass before any TLC run:

- *staleness*: the committed configuration files equal the catalogue's
  rendering byte for byte;
- *grounding*: every operator the catalogue names is defined in the
  specification, its constants and mutations match the specification's in
  both directions, and every code symbol it cites is found in the non-test
  Rust sources.

The catalogue's types also force every mutation to have a verdict and a
primary row, and every safety invariant except `TypeOK` to have at least one
row that fails it. Each negative configuration breaks exactly one rule and
must produce a counterexample on the one property it names.

**What N-version checking does and does not show.** N-version programming
runs independently written versions of one specification and compares their
results [NVersion85]. Its known weakness is that independently written
versions can still fail on the same inputs [KnightLeveson86]. The README is
explicit that the explorers are "independent code, shared design": each was
transliterated by hand from the TLA+ text. So parity shows that TLC
evaluates the specification as its text reads. It cannot catch a misreading
of the Rust code that the specification itself makes. The pinned decision
rows are the one place where the Haskell side checks the code (P67).

#### `BulkloadTransfer.tla`: the transfer protocol

It models wire v5 per entry; staging, seal, no-replace publish, directory
seal and the group commit; `Held` and the digest-only ledger; the source
store's authority; crashes of either host or both, and the rerun; racy
captures, source edits, third-party writes, failed group commits and the
space refusal; typed source access; and the WP0(d) and WP0(g) rulings.

**Invariants checked.** Seventeen frozen safety invariants:

- sanity: `TypeOK`;
- R25 and S3: `R25_NoDurableReread`, `R25_NoCommittedCaptureReread`,
  `ReadOnce`, `S3_ReadsOnlyChanged`, `S3_UnchangedReadsZero`,
  `S3_ClosedPassIsHeld`;
- durability order: `RecordImpliesBytes`, `HeldAfterCommit`,
  `LedgerAfterHeld`, `DoneAfterLedger`;
- soundness of reuse: `ReuseSound`, `LedgerSound`;
- destination safety: `NoClobber`;
- S2: `S2_TypedSourceAccess`, `S2_BackupLockBounded`;
- S4: `ClosureAccounted`.

Two more are checked and not frozen: `R25_StrictNoDurableReread` and
`AdoptOnlyUnrowed` (both from #198). The liveness properties are
`RunsClose` and `AllRunsFinish`, under weak fairness of the protocol, with
no crash and a source that has stopped changing. `WithinBudget` is a
wall-clock bound on a TLC run, not a property of the protocol.

`R25_NoDurableReread` is the operative R25 check, and this paper cites it
as the R25 result. `docs/slo.md` words the model obligation as "no committed
capture is re-read", which is `R25_NoCommittedCaptureReread`. The README
shows that wording is vacuous in the code's shape and asks for a ruling on
it; none is recorded.

**Run of record** (informational; README, "Results"). One `just tla-check`
over every row, on sting, 2026-10-06, TLC 2.19, over commit `8714c61`. All
54 rows matched their expectation:

- 17 PASS, each with its never-enabled actions equal to the set its row
  names, so coverage is enforced;
- 3 REACHED: reachability witnesses for the ledger branches and WP0(g)'s
  lost-row path;
- 32 FAIL, each on exactly the one property it names: 27 mutation rows
  covering 23 mutations, one row that drops fairness, and four finding rows
  that document old or rejected behaviour (`MC_store_root_unsealed`,
  `MC_wp0g_authority`, `MC_r25_unrowed_no_adopt`, `MC_wp0d_check_rename`);
- 1 SIMULATION, which is evidence and not a model-checking result;
- 1 INCONCLUSIVE, the budget self-test, as expected.

The largest pass rows explored 963,314 distinct states (`MC_main`: two
seats, two runs, one crash, one edit) and 963,928 (`MC_r25_strict_main`).

**Strict R25 in the model.** `R25_StrictNoDurableReread` passes in
`MC_r25_unrowed_bytes`, `MC_r25_strict_deep`, `MC_r25_strict_main`, and,
across a lost source authority, `MC_r25_strict_unsealed` and
`MC_r25_strict_authority`. The transfer before #198 fails it
(`MC_r25_unrowed_no_adopt`). Section 3.1 lists the limits. Seven rows are
kept on the transfer before #198 on purpose, and their verdicts say nothing
about the code since.

**Explorer parity** (informational; README, "The explorer"). One
`just formal-nv` run over `8714c61` on 2026-10-06. The explorer reached
TLC's distinct and generated counts and depths on all four presets: 15,834
and 142,450 states for the frozen core rows, 17,027 and 185,852 for the same
bounds with #198's adoption. All 39 mutation runs matched: 21 on the named
property and 18 with every invariant checked, where TLC and the explorer
must stop at the same first invariant after the same number of states. The
explorer's domain has 27 of the specification's 37 actions. Six mutation
rows are outside it.

**Code symbols.** The catalogue cites 19 distinct Rust symbols for this
module, and grounding finds every one. Nothing is listed as pending. Three
parts of the model have no code behind them at all: WP0(d)'s four actions
(superseding publish, section 4.6), and the relaxed stores of WP0(g).

**What it does not prove** (README, "Not proven here"):

- code-level S2, which is P34's job;
- chunking, credits, the entry window and cross-file deduplication, so S3's
  second inequality is not proven here;
- the tree: directories and their records, symlinks, walk caps;
- storage below the store;
- larger bounds. Constants are deliberately small: two seats with two or
  three runs, or one seat with three runs and two crashes;
- a failing source-ledger commit (#163);
- #154's salvage bound and legacy-row invalidation, which the README still
  lists as not modelled.

#### `GitCarry.tla`: chain and base custody

This second module (#179, OI-1003-Q43, Q46) models what a v1 capture
depends on: the plan base a group shares, chain links and their depth,
crashes between a bundle, its sidecars and its record, third-party damage
to the corpus, and what `estate-apply` and the next capture would do with
every record. It also models two designs with no code yet: Q46's re-root
policy with garbage collection, and chaining under a plan base.

**Invariants checked.** Seven safety invariants and one liveness property:

| Property | Statement, in short | Code symbols still pending (lane) |
|---|---|---|
| `ChainDepthBounded` | every chain is at most the limit deep | the re-root window (L8) |
| `PrereqsSatisfiedByEarlierLinks` | a restore whose digests check never fails bundle verification | flatten's base import (L6b); a re-root's header prerequisites and `.prior` from one chain path (L8) |
| `BrokenLinkNeverReuseHit` | a capture never reuses a record whose custody is broken | none |
| `BaseNotReplacedWhileDepended` | the plan base never moves while a record depends on it | the never-replace assertion (L6b) |
| `GCNeverDeletesDepended` | GC never removes a bundle something depends on | STATE and CORPUS GC (L8); GC's CORPUS-level exclusive lock (L8) |
| `SidecarsBeforeRecord` | a record names a published bundle whose sidecars exist | the `.reuse` sidecar (L7) |
| `RestoreOrRecapture` | every record restores, or is recaptured, or refuses by name | GC (L8) |
| `ChainRecovery` (liveness) | once the environment stops, every item gets a record that restores | the re-root window (L8) |

The right-hand column is the README's "pending" list: symbols the
properties are about that no code has yet. At lane L6a's check (2026-10-05)
grounding found 27 GitCarry code symbols as item definitions and printed 7
pending ones. So the model's Q46 and chain-under-base rows check a design,
not the product.

**Run of record** (informational; README, "Results (GitCarry)"). On sting,
2026-10-05, over `ba6b85c`. All 23 rows matched: 8 PASS, 3 REACHED, 11 FAIL
and the INCONCLUSIVE self-test. The 11 failing rows are 8 mutation rows
covering 7 mutations, one row that drops fairness, and two findings. The
explorer matched TLC's counts on seven presets. GitCarry's rows were not
part of the 2026-10-06 run above.

**Findings the model produced** (README, "Findings"). Both are open
defects on `main`:

- **A bundle rewritten in place blocks its item while the source holds
  still** (`MC_gc_live_rewritten`). The recapture produces the same content
  name, finds it taken by other bytes, and refuses `DIGEST_MISMATCH` on
  every pass until the source moves. Every refusal is typed; recovery does
  not happen. No fix is on `main`.
- **A missing plan base restores as a bare `IO`**
  (`MC_gc_base_missing_untyped`), which never counts under S4. Issue #181
  tracks it. The positive grouped rows assume the typed refusal.

**What it does not prove** (README, "What GitCarry does not prove"): pack
contents (P64 and P65's job), two writers on one corpus, drift, racy seats
and shallow sources (which the pinned rows vary and the model holds fixed),
and anything the code does beyond the v1 policy.

### 4.3 Crash and power-loss traces

- **The W7 fault harness** crashes a real `copy` with `_exit` at each fault
  point and then checks four invariants: a record implies its bytes (I1);
  no partial leaf exists under a final name (I2); a committed file costs 0
  source reads on resume (I3); no temporary survives (I4). Source:
  `crates/bulkload-agent/tests/fault_harness.rs`. Its CI table has 29 rows;
  the 10 Git-ingest rows went with `carry_v2`
  ([plan](../plans/2026-10-03-property-test-plan.md), "Retired with
  carry_v2").
- **The R-N88 power-loss checker.** A process crash leaves the page cache
  intact, so the harness cannot see a missing flush. The checker
  (`crates/bulkload-agent/src/io/crash_check.rs`) closes that gap. Like
  ALICE [ALICE14], it records the syscall trace of a real `copy` and reasons
  over the crash states an abstract persistence model allows. Unlike ALICE,
  it enumerates up to a bound: at each crash point with at most 12 optional
  mutations (`exhaustive_limit`), it tries every subset and discards the
  illegal ones. Above the bound it tries only a bounded family. The
  real-copy test accepts bounded points (`accept_bounded` in
  `tests/power_loss.rs`), so the proof is exhaustive up to the bound and
  covers only that family above it. The Darwin rules model `F_FULLFSYNC` as
  a drain of the whole device queue and `F_BARRIERFSYNC` as ordering without
  durability, as Apple's `fcntl(2)` describes them [AppleFcntl]
  [AppleDiskWrites].
- **Resume proofs** (`materialize::adoption_power_loss`, run by
  `just resume-power-loss`): two directory adoption proofs (#74), and since
  #198 the proof that an unrowed output is adopted without source reads at
  every power-loss state of a crashed publish.
- **The store root** (#166): power-loss traces now cover the state root's
  seal (`tests/power_loss.rs`).

All of these are *gated*: they are in the mandatory `check-fast` tier and
never move to the optional tier (`AGENTS.md`, "Validation").

### 4.4 Benchmark gates

- **Gate (a), local.** `r23_ab.py` (`just bench-r23-ab`) builds the
  candidate B and the baseline A and runs the order B/A/B/A/B. B passes only
  if every B rep passes; A is informational (OI-1002-Q30). A sample is gated
  only on AC power with the 1-minute load under the R-N81 limit, and other
  lanes are held quiet (R-N91). Since #176 the harness also has an
  `--under-load` mode, which lifts only the load gate and can never produce
  a verdict (OI-1003-Q39, Q50;
  [note](../agent-notes/2026-10-04-r23-under-load.md)).
- **Gate (b), cross-host.** The harness is on `main` since #193
  (`crates/bulkload-bench/scripts/gate_b.py`, `just bench-gate-b`;
  [protocol](../plans/2026-10-06-s1-gate-b-protocol.md)). It is a harness
  only:
  - it drives today's single-stream `pull`, one `ssh` session to one
    `serve`;
  - **the native multi-stream arm does not exist.** W5 owns it. The protocol
    plan lists what the agent lacks: parallel streams, wire compression,
    `Ref` dedup frames and byte-range requests. Wire v5 has none of those
    frames, so they need a new wire version. The harness refuses a request
    for more than one native stream (`NATIVE_REMOTE_ARM_MISSING`);
  - its protocol choices are drafts until the operator ratifies them, and
    no neo run happens until gate (a) passes
    ([`docs/slo.md`](../slo.md), "Amendment 2026-10-06: S1 gate (b)
    harness").
- **The hermetic rig: ruled, in progress, not on `main`.** Operator rulings
  OI-1003-Q96 and OI-1003-Q97 (2026-10-07) make a hermetic rig the S1 gate
  of record: two quiet Linux hosts plus a macOS source, on a sealed corpus.
  A neo → sting run then serves as a field confirmation, not as the gate.
  The rig's enablement is in progress. At `a80c63b`, `docs/slo.md` carries
  no amendment for these rulings and no rig code or evidence is on `main`;
  this paper cites the rulings as this refresh's dispatch gave them. Until
  the amendment lands, the gate text in `docs/slo.md` is the one above.
- **The estate-shaped corpus** (#159, OI-1003-Q19): a deterministic, sealed
  generator of Git-heavy, many-small-file trees
  ([evidence](../evidence/estate-corpus-v1-2026-10-03.md)).
- **The S3 estate harness** (`s3_estate.py`, #173) and **the S2 budget
  instrument** (`s2_budget.py`, #168) are on `main`. Neither has produced a
  gated run.

### 4.5 What CI runs

The pull-request workflow has **two gates**, `source` and `fault-harness`,
each capped at 25 minutes (`.github/workflows/ci.yml`; #190, OI-1003-Q65,
Q71). The two Bazel gates were dropped: neither compiled Rust
([note](../agent-notes/2026-10-06-ci-drop-bazel.md)).

- `source` runs formatting, clippy with warnings denied, the workspace
  tests (every property at its CI case count on the fixed seed, the
  dependency wall, the seed guard), the secret scans and the contract
  tests.
- `fault-harness` runs the fault harness, the power-loss proofs and the
  resume power-loss proofs.

`just check-fast` runs the same mandatory tier locally. TLC, GHC, the deep
property tier and every benchmark are outside both.

### 4.6 State per SLO at `a80c63b`

| SLO | Instrument | State |
|---|---|---|
| S1 gate (a) | R23 A/B harness under R-N81 | **Not met. No gated sample of the wire v5 engine exists.** Informational numbers only (section 5.2). |
| S1 gate (b) | `gate_b.py` | Harness on `main`. Pending: no run, and the native multi-stream arm is not built. |
| S1 gate of record | hermetic rig (OI-1003-Q96, Q97) | Ruled 2026-10-07; in progress; not on `main`. |
| S2 properties | P-S2, P34, P75, the priority test; `MC_s2` | Gated for the file path, the Git object store and the SQLite exceptions. Open: #188, the Q16 lock counter, P34's traced form. |
| S2 budget | `s2_budget.py` | Pending. Never measured (#165). |
| S3 file path | P21, P23, P18, P19, fault-harness I3; the model's S3 invariants | Gated, with three properties ignored against #186 and #187. |
| S3 Git path | P64, P65, P67, counters tests | Laws gated. One estate measurement, informational, taken before thin bases (section 5.2). |
| S3 SQLite | none | Not met: `snapshot` re-reads every database on every pass. |
| S3 rerun ratio | `s3_estate.py` | Informational, one run (section 5.2). No gated verdict. |
| R25 | P74, I3, the power-loss proofs; `R25_NoDurableReread` | Gated on the code; holds in the model within its bounds. Strict reading holds within stated limits (section 3.1). |
| Durability order | fault harness I1–I4, R-N88 checker, resume proofs; four model invariants | Gated. #161 is fixed. The real-copy proof is bounded. |
| S4 | closure gate, disposition ledger; P72, P73; `ClosureAccounted` | Gated **for Git estate items only**. Transfer and SQLite outcomes are not in the ledger. 57 bare `IO` sites remain. |
| S5 | drift custody tests | Ref, seat and object-store drift on `main`. HEAD and index still refuse (#38). |

## 5. Results

This section reports only what files on `main` record, with dates. No
number is rounded, extrapolated or combined from two files.

### 5.1 Gated

A gated result is one that `check-fast` and the two CI gates enforce on
every pull request. These hold on `main` at `a80c63b`:

- **Crash and power-loss ordering**: fault-harness invariants I1–I4 at 29
  crash rows, the real-copy power-loss proof (bounded, section 4.3), and the
  resume proofs (`tests/fault_harness.rs`, `tests/power_loss.rs`,
  `src/materialize/adoption_power_loss.rs`).
- **S3 on the file path**: a rerun reads at most the changed seats; a
  resume after any cut reads only the unapplied files and a further rerun
  reads and receives 0; added seats cross as absent chunks
  (`tests/s3_walk_resume.rs`, `tests/s3_transfer_resume.rs`). The three
  ignored properties of section 4.1 are *not* part of this result.
- **R25's strict reading on the code**: P74 over 12 fixed seeds
  (`src/transfer/tests.rs`).
- **Git pack laws**: P64 and P65 over 18 fixed rows
  (`tests/git_group_minimality.rs`), and the ref table at 131,072 refs
  (`src/git_carry/refs_scale_tests.rs`).
- **The Git decision core**: P67 over all 363 reference rows, with totality
  (`src/git_carry/decide_tests.rs`).
- **S2**: the source lstat census on the file path and the Git object
  store, and P75's counted SQLite exceptions (section 3.3). In CI, P75
  proves the root refusal only (section 7).
- **S4 for Git items**: P72 and P73 (`src/outcome.rs`, `src/closure.rs`).

**No benchmark sample is gated.** No S1, S2-budget or S3-ratio sample has
been taken under R-N81 and R-N91 on the wire v5 engine.

### 5.2 Informational

Each item below is on record but enforced by nothing.

**S1. There is still no gated sample.** The only S1 numbers since wire v5
are these.

- **2026-10-04, under load, on neo** (B = `cd4ffad`;
  [coordinator note](../agent-notes/2026-10-05-coordinator.md), "Measured";
  [under-load note](../agent-notes/2026-10-04-r23-under-load.md), "Runs").
  The load gate was set aside by OI-1003-Q39. The coordinator note records
  native initial copies of 9.3, 38.8 and 18.1 s against rclone's 112 and
  221 s, a mixed 1 % delta with one 10-minute native outlier, swap nearly
  exhausted, and rep 0 completing at a 1-minute load of 71.4. The sample
  then aborted when the A baseline failed with `EPIPE`. Three cautions:
  - it is **not a gate verdict**, and the note says so in bold;
  - it is one aborted sample on a host under memory pressure, so the ratio
    it suggests is not evidence of S1;
  - **no file under `docs/evidence/` records it.** The harness now writes
    under-load evidence files, but the rerun has not happened.
- **Before wire v5** (historical). The last completed gate sample,
  2026-09-18, **failed**: it lost the initial copy to rclone and won the
  1 % delta, and both are mandatory
  ([r23-2026-09-18](../evidence/r23-2026-09-18.md)). The M0 and W3
  measurements of 2026-09-23 were ungated, and native lost the initial copy
  in every one ([m0](../evidence/m0-2026-09-23.md),
  [w3](../evidence/w3-2026-09-23.md)). The corpus they used was lost on
  2026-10-02, so their times compare with nothing newer
  ([corpus v1](../evidence/r23-corpus-v1-2026-10-02.md)).

**S3 on the estate corpus, sting, 2026-10-04**
([evidence](../evidence/s3-estate-sting-2026-10-04.md)). One run per scale,
ungated, at a 1-minute load of 16 to 47. It measured `main` at `4a10bb8`:
before typed refusals (#151), the read-only object store (#172), thin bases
(#177) and the ref table (#182). **It has not been re-measured since.** At
estate scale (142,321 entries, 4.19 GB):

- **Unchanged reruns, Git half: pass.** 0 source bytes, 0 pack bytes, and
  one census walk per item (54 of 54), on all three reruns.
- **Unchanged reruns, file half: 0 bytes received, 176 bytes read.** The
  176 bytes are 16 per refused SQLite database, on every pass. That is
  issue #186.
- **Unchanged reruns, SQLite half: fail.** `snapshot` re-read every
  database: 180,581,128 bytes per pass.
- **CPU ratio** against the first pass: 6.9 % to 7.9 %, inside S3's bound
  at estate scale. At small scale it was 15.2 % to 15.9 %, outside it; the
  evidence attributes that to a small denominator. The wall-clock ratio was
  0.6 % to 1.7 % and is informational by ruling (OI-1003-Q35).
- **Delta reruns.** File inequality 1 failed by the sniff bytes only. File
  inequality 2 passed after one change and was `n/a: blocked by WP0(d)`
  after ten, because changed seats were refused, not superseded (#187). Git
  inequality 1 passed on worktree seats. **Git inequality 2 failed**: at
  ten changes the pack was 70,619,800 bytes against a bound of 806,672
  (chunk granularity) or 67,129,040 (object granularity).

The Git inequality 2 failure is what thin bases (#177) address. On `main`
that fix is held by the gated P64 and P65 rows, not by a new estate
measurement. The lane's own probe measured a 64-byte edit to a 64 MiB blob
packing as 2,514 bytes instead of 67,129,535
([note](../agent-notes/2026-10-04-q42-l1-thin-base.md), "Probes"); its
scripts were not kept.

**The 123k-ref repository, sting, 2026-10-05**
([evidence](../evidence/2026-10-05-blahaj-v1-carry.md); one sample,
ungated). This was the precondition for deleting `carry_v2` (OI-1003-Q54,
Q56).

- Before the ref table, v1 refused such repositories: at 119,761
  carry-shaped refs the capture header was 20,745,391 bytes, over the
  16 MiB cap ([probes](../evidence/2026-10-04-q42-probes.md), finding 1).
- With the ref table (`main` at `40adca8`), the real repository of 122,994
  refs over 2,382 distinct objects captured with no refusal. Its manifest
  was 265,281 bytes, 1.58 % of the cap.
- **It restored exactly**: all 122,994 refs, with 0 missing, 0 extra and 0
  object-id differences. HEAD, the shallow frontier and the custody of 80
  nested worktrees matched.
- The source's `.git` lstat census was identical before, between and after
  every step: 2,884 entries, the same digest seven times.
- **Scope limits the evidence states.** The repository is shallow, so only
  the shallow-envelope path ran; the non-shallow writers' header code did
  not. The corpus started empty. The restore was refs only; no workspace
  restore was run. Two symbolic refs besides HEAD came back as plain refs
  at the same object.

**Model checking** (section 4.2): 54 of 54 `BulkloadTransfer` rows on
2026-10-06 and 23 of 23 `GitCarry` rows on 2026-10-05 matched their
expectations, and the Haskell explorers matched TLC's counts
([formal README](../formal/README.md)).

**Earlier estate operations** (historical, before wire v5 and the current
closure gate): the cohort 3 repair of 2026-09-22
([cohort3](../evidence/cohort3-index-repair-20260922.md)) and the cohort-1
estimate, which measured a 9,758,779-byte thin pack against 1,475.90 MB of
history on disk and so motivated incremental Git carry
([estimate](../evidence/git-carry-estimate-2026-09-23.md)).

### 5.3 Pending

| Result owed | Blocker or state |
|---|---|
| A gated S1 gate (a) sample on wire v5 | No quiet gated window has been obtained; the hermetic rig (OI-1003-Q96, Q97) is in progress. |
| An S1 gate (b) sample | Waits on gate (a) (OI-1003-Q66). The multi-stream arm is not built. |
| A re-run of the under-load sample with the fixed harness | Open in the [under-load note](../agent-notes/2026-10-04-r23-under-load.md). |
| The S2 measured budget | Issue #165. Never run. |
| S3 on the estate corpus after #172, #177 and #182 | Not re-measured. |
| A gated S3 rerun-ratio verdict | None recorded. |
| Grouped chaining, P68 | PR #199, in review. |
| The Git command registry, P76 | PR #197, in review; issue #188. |
| Transfer and SQLite outcomes in the disposition ledger | WP3 PR 4, not started on `main`. |
| The daily-work verdict on the destination | Issue #40. |
| The neo read-only estimate pass | Deferred ([probes](../evidence/2026-10-04-q42-probes.md), "Pending"). |

## 6. Prior art

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
  the same tick. bulkload's identity also includes ctime, and the racy rule
  covers the same-tick case.

**rclone** [Rclone]. A multi-backend file mover. It transfers whole files,
because its object-store backends cannot patch part of an object. It skips
files by size and modification time or by checksum. On APFS, local-to-local
copies clone by default unless `--local-no-clone` is given. *Borrowed:*
rclone is the performance bar (R23), with its exact flag set recorded in
every sample. *Rejected:* whole-file granularity, and no Git or SQLite
awareness. A note for readers of gate (a): on an APFS gate volume, rclone as
shipped clones and verifies rather than writing data
([m0](../evidence/m0-2026-09-23.md)). The gate still compares against it.

**Git's pack protocol and bundles** [GitPackProto] [GitPackObjects]
[GitBundle]. In the pack protocol, the client lists objects it wants and
objects it has, and the server builds a pack of only what is missing. A thin
pack also omits delta bases the receiver holds; `index-pack --fix-thin`
completes it. A bundle carries refs and objects for offline transfer, and a
bundle made from a revision range names prerequisites the receiver must
already hold. *Borrowed:* Git's own object model and tools do all packing,
and bulkload never re-implements them. v1's chained and grouped bundles are
thin packs with prerequisites, completed on restore by `--fix-thin`.
`upload-pack` is the exactness oracle for the estimate (P66). *Rejected:*
negotiating haves over bulkload's own wire. That was `carry_v2`. It matched
`upload-pack` on fixtures, but the operator chose one engine, and bundles
whose prerequisites are the previous capture's tips reach the same thin
packs without a second protocol (OI-1003-Q44). `carry_v2` is deleted; tag
`carry-v2-final` keeps it.

**Git's racy index** [RacyGit]. Git treats an index entry as racily clean
when its cached `st_mtime` is the same as, or newer than, the index file's
own timestamp. It then compares the entry's content with the recorded
object. *Adapted*, not borrowed directly, as the racy-capture rule
(section 2.6). bulkload has no index file to compare with. It compares a
seat's mtime and ctime with wall-clock reads taken before the open and
after the final stat. Instead of re-checking content, it records nothing
for a racy capture, so the next run reads the seat again.

**casync and desync** [Casync17] [Desync]. casync cuts a serialized tree or
block image into content-defined chunks with a buzhash rolling hash, stores
them in a chunk store named by digest (SHA-512/256 by default, per the
repository README), and describes the image with an index file. desync
implements the same formats in Go, with parallel chunking and more store
backends. *Borrowed:* a chunk index as the manifest, and clients fetching
only the chunks they lack. *Rejected:* a chunk store as an intermediate,
because bulkload publishes usable files directly into a home directory; and
whole-image serialization, which assumes a quiescent source tree.

**restic and borg** [Restic] [Borg]. Content-defined-chunking backup tools
that store content-addressed chunks in a repository. restic uses Rabin
fingerprints with chunk sizes between 512 KiB and 8 MiB, and SHA-256 ids.
borg uses buzhash. borg keeps a files cache so it can skip re-reading
unchanged files; by default it compares ctime, size and inode. When it
saves the cache, it does not persist entries whose timestamp is the newest
it saw in that run (`LocalCache.commit` on borg's 1.4 maintenance branch)
[Borg]. That is a racy-entry guard of its own. *Borrowed:* borg's files
cache is the same idea as R25's stat-identity reuse, and its
newest-timestamp guard is close kin to bulkload's racy rule. bulkload adds
mtime and ctime together in one key, the device and the source authority,
a window measured against the capturing host's clock, and a row committed
only after the destination answers `Held{true}`. *Rejected:* a backup
repository as the destination. The destination must be a working home
directory, and a separate restore step would write everything twice.

**bup** [Bup]. A backup tool that writes Git packfiles directly, splits large
files with a rolling checksum, and keeps a separate index of file metadata.
*Borrowed:* Git's storage is the right container for Git-like data, and
change detection belongs in a metadata index kept apart from content.
*Rejected:* storing the estate in a backup repository.

**Unison** [Unison04] [FileSync98] [UnisonRepo]. A bidirectional file
synchronizer. It keeps an archive of each replica's last synchronized state,
propagates non-conflicting changes, and surfaces conflicts instead of
resolving them silently. Its behaviour has a formal specification.
*Borrowed:* conflicts are kept for explicit resolution, and a formal
specification is part of the product, not an afterthought. *Rejected:*
replica reconciliation. Each bulkload run moves data one way.

**ZFS send and receive** [ZfsSend]. Block-level streams of a snapshot,
incremental between two snapshots, and resumable from a token after
interruption. *Borrowed:* resumable transfer from a durable token is the
model for bulkload's ledger. *Rejected:* it requires the same filesystem
and snapshots on both ends. The lab's source is APFS and its destination
XFS ([m0](../evidence/m0-2026-09-23.md)). bulkload also takes no source
snapshot, by design.

**LBFS** [LBFS01]. A network file system that cuts files at content-defined
boundaries and indexes the chunks of files it already holds, so it avoids
sending data the other side has. *Borrowed:* the destination's chunk hints.
For each digest, the destination records every published output holding it;
a hint is re-read and re-verified before use.

**FastCDC** [FastCDC16] [FastCDC20]. Gear-hash content-defined chunking with
normalized chunk sizes. *Borrowed directly*, through the `fastcdc` crate's
2020 variant.

**BLAKE3** [BLAKE3]. A cryptographic hash built as a Merkle tree over 1 KiB
chunks, with hash, keyed-hash and derive-key modes. *Borrowed:* chunk
digests, the `wire_id` schema hash, and `manifest_root` in derive-key mode.
*Not used:* BLAKE3's internal tree as the file manifest (section 2.3).

**Crash-consistency research.**

- *ALICE* [ALICE14] showed that applications often depend on file-system
  behaviour that POSIX does not promise. It records an application's
  syscall trace and, under an abstract persistence model, constructs
  selected crash states. Its authors call part of that exploration not
  exhaustive. bulkload borrows the trace-plus-persistence-model approach
  (section 4.3). Its checker enumerates every subset of the optional
  mutations at a crash point up to a bound, falls back above it to an
  ALICE-like bounded family, and adds Darwin's drain and barrier rules.
- *CrashMonkey and B3* [B3-18] [CrashMonkey19] test file systems by
  simulating power loss under bounded, exhaustively generated workloads.
  bulkload borrows bounding with exhaustive enumeration. It does not trace
  at the block layer, because it tests its own protocol, not the file
  system. Their finding that mature file systems have crash bugs is a
  threat to validity (section 7).
- *Optimistic crash consistency* [OptFS13] separates write ordering from
  durability. bulkload's group mode makes the same split in user space on
  Darwin: a barrier per file for ordering, and one draining commit per group
  for durability.

**Formal methods and N-version checking.** TLA+ [TLA94] [Specifying02] with
the TLC model checker [TLC99] is the checker of record. Dhall [Dhall] is a
typed configuration language. The catalogue merges a handler over each
union type, and that is what turns a forgotten mutation verdict into a type
error ([formal README](../formal/README.md), "The catalogue"). The Haskell
explorers
apply N-version programming [NVersion85] to the model, and the README's
"independent code, shared design" caveat is the lesson of [KnightLeveson86]
stated for this project (section 4.2).

**Property testing.** QuickCheck [QuickCheck00] introduced random testing
against stated properties. bulkload uses `proptest` [Proptest] and departs
from the usual practice in one way: CI replays one fixed seed, so the
tested corpus is bounded and identical on every run (section 4.1).

## 7. Limits and open problems

**Performance is unproven.**

- **No gated S1 sample exists for the wire v5 engine.** Every gated or
  completed timing predates it, and the one sample since is informational,
  aborted, and not in `docs/evidence/` (section 5.2).
- The hermetic rig that is to be the gate of record is ruled and not built
  into `main` (OI-1003-Q96, Q97).
- Gate (b)'s native multi-stream arm is not built, so gate (b) can only
  measure the single-stream engine today.
- Buffer reuse is not wired in (section 2.1).
- On APFS, rclone as shipped clones. A durable writer is then compared
  against metadata and hashing.

**S2 is proven in part.**

- **Issue #188.** Four child-process builders bypass `git_carry::git`, so
  the hardening table does not cover them. The registry guard is in review
  (PR #197).
- The measured budget has never been run (#165).
- The Q16 backup lock is not counted.
- **The CI root-user coverage gap for P75.** CI runs as root. As root the
  provider refuses to read a source, so P75's full property can run only in
  a child that drops to an unprivileged uid. On the CI run of `main` at
  `95f43dc` (run 37582012173, 2026-10-07), both P75 tests printed
  `unprivileged leg SKIPPED (dropping to uid 65534: Permission denied)` and
  "this run proved the refusal only". So **in CI the two counted exceptions
  are not exercised**; they are proven only by local `check-fast` runs as an
  ordinary user. This is a CI log, not a file on `main`, and it is cited
  here as a dated observation.

**S3 has known holes.**

- A refused SQLite seat is re-read for 16 bytes on every run (#186).
- A changed source file is refused on rerun instead of superseded (#187).
  Superseding publish (WP0(d)) is ratified, modelled, and has no code.
- SQLite has no incremental path: every pass reads every database.
- Unchanged untracked payload is re-packed on every recapture (#174).
- The estate measurement predates four merged fixes and has not been
  repeated.

**S4 is proven for Git items only.**

- Transfer and SQLite outcomes are not in the disposition ledger.
- 57 bare `IO` sites remain.
- **Issue #200.** A briefly busy ledger lock in `disposition::record`
  surfaces as a bare `IO(EAGAIN)`, and its test is flaky under parallel
  load. A bare `IO` in the component that decides completeness is the
  defect S4 exists to exclude.
- Issue #201: a finished `transfer::copy` intermittently returns a bare
  `IO(EPIPE)`, and P-S2 flakes in the deep tier.
- Issues #181 and #183: a missing plan base refused as a bare `IO`, and two
  misattributed `GIT_INVENTORY_MALFORMED` refusals.

**S5 drift is incomplete (#38).** HEAD moves and index rewrites inside a
captured worktree still refuse the item, although OI-1003-Q11 rules them
drift classes. An agent that commits while a pass reads its repository
makes that item refuse. Configuration, shallow-frontier, nest and
directory-shape movement refuse too, with no ruling.

**Restore cost is undercounted (#147).** `estate-apply`'s space preflight
undercounts a chained apply, and `chain::flatten` stages inside the corpus.
A restore of a chained capture stages up to the depth limit plus one
bundles and writes a flattened copy. The paper's S3 argument is about
capture cost; the restore side has no SLO, no measurement and this open
defect. The same is true of time: no evidence file measures a full
workspace restore.

**The Git carry beyond v1 is a checked design, not a product.** The Q46
re-root policy, garbage collection and chaining under a plan base are
modelled and their decisions are pinned, but the callers run only the v1
policy. At the depth limit v1 still re-packs an item's whole history. A
bundle rewritten in place still blocks its item (section 4.2).

**The model is bounded and partial.**

- TLC checks small constants. That is not a proof for every estate size.
- The explorers share the specification's reading of the code
  (section 4.2).
- The transfer model assumes a source-ledger commit never fails (#163) and
  does not model #154's salvage bound or legacy-row invalidation.
- No model result is gated. A change to the code that broke a modelled rule
  would be caught only if someone reran `just tla-check`, and only if the
  specification was updated to match.

**The power-loss proof of a real copy is bounded.** Above 12 optional
mutations per crash point the checker tries a bounded family only, and no
evidence file records which points were bounded on a given run. The `_exit`
harness cannot see a missing flush at all.

**Media durability is assumed, not shown.** M0 notes that `F_FULLFSYNC`
reaching stable media on the USB enclosure is unproven. Apple's guidance
calls `F_FULLFSYNC` best effort against sudden power loss
[AppleDiskWrites], and `F_BARRIERFSYNC` needs hardware support that Apple
guarantees only for its own SSDs [AppleFcntl]. CrashMonkey's results show
mature file systems sometimes break their documented semantics
[CrashMonkey19].

**Counters are lower bounds.** Flush counts exclude SQLite's and Git's own
syncs. The pack-read counter misses page-cache hits. Reads by Git children
other than the packers are not counted.

**The trust model is cooperative.** Reuse trusts stat identity and the
capturing host's clock. Capture records and their sidecars are not
authenticated; corpus integrity rests on directory permissions
([`docs/design.md`](../design.md), "Drift"). The #198 capture record is an
extended attribute that anyone who can write the output can also write; the
hash check against the output's own bytes is what guards it.

**Documentation drift on `main`.** `docs/design.md` still calls the salvage
bounds unruled, and `docs/formal/README.md` still describes the state root
as unsealed in two places, although `docs/slo.md` records both as settled.
`docs/slo.md` has no text yet for OI-1003-Q96 and Q97.

**CI is not uniformly green.** The CI run of `main` at `3931471`
(2026-10-07) failed both gates, and issues #200 and #201 record flaky
tests. A gated result in section 5.1 means the gate enforces it, not that
every run of the gate has passed.

**The sample is narrow.** Two hosts, one operator and one estate.

## 8. What changed since 2026-10-04

The previous revision was pinned to `main` at `dfb9604`. This one is pinned
to `a80c63b`. In order of weight:

1. **One Git engine.** `carry_v2` and the M1 spike are deleted (#189, tag
   `carry-v2-final`). The earlier text that called `carry_v2` "frozen" and
   the engine choice "to be measured" is gone. v1 gained thin group bases
   (#177), the ref table (#182, proven on a real 123k-ref repository, #184)
   and a pure decision core with 363 reference rows (#191). Grouped
   chaining (P68) is in review, not on `main`.
2. **The formal model grew from one module to two, with a catalogue and a
   cross-check.** The Dhall catalogue and Haskell explorer were "planned
   for sprint 2"; both are on `main` (#170, #179). The transfer model went
   from 43 rows to 54, and `GitCarry.tla` adds 23.
3. **Strict R25 now holds in the model** (#198). The earlier revision
   reported `MC_r25_unrowed_bytes` as failing and the reading as unruled.
   OI-1003-Q40 ruled the reading, and the capture record's adoption makes
   the strict one pass, within stated limits.
4. **Three defects the earlier revision reported as open are fixed**: the
   unsealed state root (#161, by #166), the Git carry's source timestamp
   write (#162, by #172), and the uncounted SQLite `-shm` write (#157, by
   #196). The last one also brought the empty-`-wal` exception and the
   refusal to read as root. Issues #162 and #157 are still open on GitHub
   for their follow-ups; #161 is closed.
5. **S4 has its disposition ledger** (#195), for Git items.
6. **Properties are fixed-seed by rule, with a guard** (#194). Property
   sites went from 9 to 25, and 12 of the plan's 64 properties are on
   `main`.
7. **S3 has evidence on the estate corpus**, informational and taken before
   four later fixes.
8. **S1 is unchanged in the way that matters: there is still no gated
   sample.** The gate (a) run this paper's earlier revision said had
   "started" produced no verdict. New since then: an under-load mode and
   one informational sample, a gate (b) harness, and rulings that make a
   hermetic rig the gate of record.
9. **CI went from four gates to two**, capped at 25 minutes (#190).
10. **Rulings that were "not yet on `main`" now are**: OI-1003-Q24, Q25,
    Q26, Q34, Q36, Q37 and Q40 are all in `docs/slo.md`.
11. **The bibliography was re-verified entry by entry on 2026-10-07**, and
    three references were added for the formal method: [Dhall],
    [NVersion85] and [KnightLeveson86].
