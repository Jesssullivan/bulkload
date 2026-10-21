# 2026-10-20: SQLite row union, Codex stores consolidated (`sqlite-union`)

Lane: `feat/sqlite-row-union-20261020`, on top of #278 (`8945b1dd`).
Receipts cite R-N13.

## Requirement served

SPEC R2.2: "Where both sides changed, the run keeps both: DEST's refs,
dirt, sessions and rows stay, SOURCE's land beside them [...] Every run is a
lossless union into DEST, possibly from more than one SOURCE host: never a
mirror, and never clobbering DEST (C10; LIVE-15, LIVE-18, CONF-05,
KEEP-36)." Also R0.4 ("Codex state rows" made to name DEST paths) and R2.1
(SQLite stores captured consistently while writers keep writing).

Operator ruling 2026-10-20, verbatim from the coordinator's relay:
"neither "WIN". they must be consoldiated. this is a non trivial task.
this is what builload should be doing." and "no dupes, no duplciated
besides, no stage dupes".

## What was built

- **Row stream protocol.** `bulkload-proto::rows` is its own protocol:
  `ROWS_VERSION` 1 and a schema id, using the transfer wire's framing. The
  transfer wire stays v6.
- **Read side.** `sqlite_union::serve_rows` runs as `--sqlite-rows serve`.
  - It holds one WAL read snapshot for the pass, within the carry wall and
    the source floor.
  - An unchanged store (the `stat` of the db and a non-empty `-wal`) is not
    opened.
  - Rows are streamed past the destination's marks, in rowid order.
- **Write side.** `sqlite_union::consolidate`:
  - Merges parents first, in `BEGIN IMMEDIATE` batches of at most 256 rows
    or 20 ms.
  - Matches natural keys and resolves conflicts by version column (later
    wins). A replacement is an `UPDATE`.
  - Inserts surrogate keys anew, with pending-batch confirmation.
  - Respells `cwd`/`path`/`*_path` through `--map` (A-055).
  - Keeps a ledger at `STATE/consolidate.sqlite (tables row_union_*)`.
  - Runs `integrity_check` and `foreign_key_check` after the pass.
- **Verb.** `sqlite-union`, over `pull`'s registered ssh child
  (`--sqlite-rows` is a fixed literal from `serve_flags`; FROZEN is
  unchanged), or `--local`.
- **Refusals.** Four new codes: `SQLITE_SCHEMA_MISMATCH`,
  `SQLITE_UNION_TARGET_ABSENT`, `SQLITE_UNION_SOURCE_NOT_WAL` and
  `SQLITE_UNION_SCHEMA_UNSUPPORTED`.
- **Design.** docs/design.md has a new section, "SQLite row union".

Why rows and not a page stream into a temporary database: there is no
staged copy on either host ("no stage dupes"). A rerun also reads only the
changed rows (the coordinator's "reads only rows changed since the last
pass"), where a page copy would re-read the whole store.

The schemas were read from Codex `79a19cbb`'s migrations
(`codex-rs/state/{migrations,thread_history_migrations,logs_migrations,queue_migrations}`):

- Text UUID keys union by primary key.
- `logs.id` and `queued_thread_revisions.revision` are `AUTOINCREMENT`
  surrogates.
- `thread_attachments` has a secondary `UNIQUE`.
- `threads` has `*_ms` fill-in triggers, and `queued_items` has revision
  triggers.
- There are no virtual or FTS tables.

## Open

- The SQLite `consolidate` signature has been proposed to the coordinator,
  for the #297 dispatch. #297's `consolidate.rs` defines none yet.
  `pull --consolidate` does not yet route a home's stores here.
- `thread_items` has no time column. An in-place update of an older item
  reaches DEST only on `--full`.
- Per-table later-wins can mix versions across tables (projection state
  against items).
- darwin-check on pzm: owed when the PR is darwin-touched.
- Not run against the live estate.

## 2026-10-20: alignment with #297 (coordinator relay)

- **Signature.** The stream signature
  `sqlite_union::consolidate(input, output, &Options) -> Result<UnionStats>`
  is confirmed. `pull --consolidate` (#297) will skip the db and its
  sidecars in the file receive and run one serve session per store. SQLite
  never uses #297's file-based `Union`.
- **Ledger.** It borrows its connection: `Ledger::new(conn, ..)` with tables
  `row_union_*`, in `STATE/consolidate.sqlite` (#297's ledger file).
  Moving to #297's `Ledger::connection()` is one line.
- **Live store.** It must be this user's regular file (`PATH_ESCAPES_ROOT`
  otherwise). Its `(dev, ino)` is taken before the open, checked after it,
  and checked again before every write batch.
- **Outcomes.** `sqlite_union::action` maps a lossless union to
  `Consolidated`, and an unmergeable store to `Held(code)` with nothing
  written. Two items wait on #297's codes reaching main: `Created` (rows
  into an absent store; the union holds `SQLITE_UNION_TARGET_ABSENT` until
  then), and `HOMES_CONSOLIDATE_RACED` for a racing writer or a replaced
  store (`SQLITE_STATE_CHANGED` today).

## 2026-10-20: restacked on main, and the coordinator's four points

Branch `feat/sqlite-row-union-main-20261020` is on main `c2da575` and
supersedes #299. #278 is not needed. What #299 took from #278 is rehomed in
`sqlite_union::hold`:

- the held WAL read snapshot and the busy backoff;
- the source floor probe;
- the carry wall and `--sqlite-carry-wall`;
- `serve_flags` in pull's child, with the registry shape;
- four counters.

It takes no snapshot-file seat, slot budget, chunking or `--beside`
corrupt-copy landing.

1. **A corrupt or unreadable source** is `SQLITE_UNION_SOURCE_UNREADABLE`:
   held, nothing written.
2. **Per-thread winners** come from `threads.updated_at`, recorded per host
   in `row_union_threads` and followed by every thread-scoped table of the
   home's stores. The coordinator's SPEC rule 4 correction applies: an extra
   row of an older thread is inserted, and only key conflicts follow the
   winner. An extra that reuses an ordinal holds the thread (typed, listed,
   nothing more written).
3. **`thread_items`** and other tables without a time column are read by
   their thread's change (`--clock STATE_DB`). Time marks are taken at the
   scan's start.
4. **An absent store is Created**: schema only under a private name, linked
   in if the path is still free, rows by union.

The `pull --consolidate` order (state_5 first) has gone to #297's dispatch
lane.

## 2026-10-21: main 3d4fca5 (#297) merged; dispatch wired

Merge `32032d9` was signed with no rebase. The refusal codes of both sides
are kept.

- **Dispatch.** `copy`/`pull --consolidate` skips a store and its sidecars
  on the file path (`TransferStats::sqlite_routed`, nothing read).
  `sqlite_union::union_routed` then merges each store, one serve session
  per store (`pull`: its ssh child; `copy`: in process), `state_*.sqlite`
  first and as the clock of its directory's other stores.
  `sqlite_union::fold` writes each outcome into the transfer record before
  it is written.
- **Ledger.** `consolidate::Ledger::open(..).connection()`; this lane's own
  opener is gone.
- **Outcomes.** These are #297's `Action`: `Consolidated`, `Covered`
  (unchanged), `Created`, or `Held(code)`. A write lock busy past the batch
  wall, or a live store replaced mid-pass, is `HOMES_CONSOLIDATE_RACED`.
- **#297's test.** It now expects a store and its `-wal` to be routed
  rather than held.

Honey ran targeted tests only (rule of 2026-10-21): fmt, both clippy lanes,
and lib `sqlite_union consolidate` (38 passed).
