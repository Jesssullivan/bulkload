# 2026-10-20: consolidate the agent homes (no beside)

Lane `feat/homes-consolidate-20261020`, stacked on #280
(`feat/pull-beside-live-home-20261019`, head `fc0cb4e`).

## The ruling served

Operator ruling 2026-10-20, as relayed by the coordinator (quoted as given):

- "neither "WIN". they must be consoldiated. this is a non trivial task.
  this is what builload should be doing. [...]" (not a glorified rclone
  wrapper)
- "no dupes, no duplciated besides, no stage dupes"
- "a dupllicate is inherinatly not path for patyh / byte for byte"

The coordinator's reading: #280's `--beside` into the live home is
superseded for the agent homes. They are **consolidated**, never landed
beside (SPEC RUL-10: "Every run is a lossless union: never a mirror, never
clobbering DEST"; C10, LIVE-15, LIVE-18, CONF-05, KEEP-36).

#280 keeps its safety parts:
- the owner-checked real homes;
- the source listing and sticky provenance;
- A-106's kept-aside copy;
- the `ELOOP` kind typing.

This lane owns three items:
1. the per-class dispatch of `estate-move --sessions`;
2. the structured-config merge;
3. a report-only `--plan-only` for the homes.

## Item 2: the structured-config merge (`80c505f`)

Module `crates/bulkload-agent/src/consolidate/` (no new crate or
dependency; R34 allows no JSON or TOML parser, so both are in-crate).

- **Class.** `*.json` and `*.toml` of the homes: `settings.json`,
  `settings.local.json`, `~/.claude.json`, `.mcp.json`, `plugins/*.json`,
  Codex `config.toml`.
- **Union** (`value::union`):
  - Objects are merged key by key: the live file's key order is kept and
    the source's new keys are appended.
  - Arrays are unioned by value: the live elements first, then each source
    element no element equals.
  - Equal scalars are kept. Equality ignores spelling: an object's key
    order, a number's radix or `_` separators, a TOML string's quote style,
    a date-time's `T`/`t`/space.
- **Conflicts.** A key both sides hold with different values (or kinds)
  takes the value of the side modified last (file mtime). The other value
  goes to the ledger `STATE/consolidate.sqlite` table `value_conflict`:
  file, RFC 6901 key path, both values on one line, which side each came
  from, first and last seen. A rerun upserts and never duplicates.
- **Translation.** Before comparing, the source's strings and object keys
  have each `--map` source prefix written as the destination's, only where
  it stands as a whole path (`/Users/jess` in `/Users/jessie` is left). So
  neo's `Read(/Users/jess/git/**)` is sting's rule, not a second one, and
  `~/.claude.json`'s `projects["/Users/jess/git/lab"]` merges into sting's
  `projects["/home/jess/git/lab"]`. Two source keys that translate to one
  key hold the file.
- **Formatting.**
  - JSON is written in the live file's style: its indent (spaces or a
    tab), its colon and comma spacing, CRLF, and its trailing newline.
    The writer writes what `JSON.stringify(v, null, indent)` writes, so an
    unchanged Claude-written file round-trips byte for byte (test).
  - TOML is edited in place, so comments, order and spelling stay:
    - a new key goes on a line at the end of its table's section;
    - a new table goes in a header block at the end of the file;
    - new array elements go before the closing bracket, in the array's
      single- or multi-line form;
    - a value the source wins replaces only its span, so the comment
      after it stays.
  - Every write is parsed back and must equal the union, or the file is
    held.
- **Held** (`HOMES_CONSOLIDATE_UNSUPPORTED`; nothing written, listed):
  - non-UTF-8, a byte-order mark;
  - JSON with a duplicate key, a comment, a trailing comma, or a lone
    surrogate; nesting past the depth limit;
  - TOML that is not well formed (duplicate key or table, a malformed
    value);
  - a TOML table defined by `[headers]` whose kind the union would change;
  - a file over 64 MiB.
- **The write** (`config::consolidate`):
  1. The live file is opened `O_NOFOLLOW` in the owner-checked directory.
     It must be this user's own regular file: a link is
     `DESTINATION_KIND_MISMATCH`, another user's file `PATH_ESCAPES_ROOT`.
  2. The union is written under a private name `.<name>.bulkload-consolidate.<pid>.<n>`
     with the live mode, and `fsync`ed.
  3. The live file's identity (inode, size, mtime, ctime) is checked again,
     and the two names are traded with one `renameat2(RENAME_EXCHANGE)`.
  4. The file traded out must be the one read; else a writer replaced or
     wrote it in between, and the trade is put back.
  5. A writer seen at either check means the file is read and merged
     again, up to 4 times. Past that the merge refuses
     `HOMES_CONSOLIDATE_RACED` (new, retryable), with nothing changed or
     recorded.
  6. The ledger is written only after the directory `fsync`.
  7. With no exchange the merge refuses `DESTINATION_EXCHANGE_UNSUPPORTED`.
  8. With no live file, the source's file is landed create-only.
- **Known gap**, the same class as #280's A-106 residual: a writer holding
  the live file open from before the exchange, which writes after the
  post-exchange check and before the private name is unlinked, writes into
  the unlinked old inode. The window is two syscalls long. Closing it needs
  the writer kept out across the exchange.
- New refusal codes `HOMES_CONSOLIDATE_UNSUPPORTED` and
  `HOMES_CONSOLIDATE_RACED`. The codes are strings in the refusal frame, so
  the wire frame version is unchanged.

## The class interface (`7db6b11`)

The #294 lane (append-log union) and the #278 lane (Codex SQLite row union)
implement `consolidate::Union`:

    pub type Union = fn(&Live<'_>, &Source<'_>, &Ledger) -> crate::Result<Outcome>;

The coordinator relayed it. `union_of(Class::AppendLog | Class::Sqlite)`
is `None` until they land: such files are held typed, listed, never
besided. Per the coordinator's adjustment (a), the SQLite union is a row
stream (`sqlite_union::consolidate`, fed by the source's `--sqlite-rows`
session). So a Codex store and its `-wal`, `-shm` and `-journal` never
take the file path. They are held at the decision, before any byte is
read, whether or not the destination has one. Their conflicts go to this
`Ledger` through `Ledger::connection()`.

## Item 1: `pull --consolidate`, the `--sessions` default

`copy`/`pull --consolidate [--consolidate-map SRC=DST]... [--plan-only]`.
`--beside` and `--consolidate` exclude each other. A live destination file
that differs from the seat is decided by its class
(`transfer::plan_consolidating`):

| Live file | Action |
|---|---|
| absent | landed, as any new path |
| a link or another kind | held (`HOMES_CONSOLIDATE_UNSUPPORTED`) |
| the seat's bytes | adopted |
| append log, holds the seat's bytes as its first bytes | `covered`, nothing written |
| append log, a proven prefix of the seat's | extended in place (A-103) |
| append log, diverged | held until the #294 union lands |
| JSON or TOML | received **in memory** (`memfd`), merged by `config::union`, dropped |
| SQLite store or sidecar | held at the decision (adjustment a) |
| anything else | held |

- **No payload copy (SPEC rule 2, adjustment b).** A config's received
  bytes are never on disk: `Destination::stage_in_memory` fills a
  `memfd_create` file. The only temp beside the live file is the union's
  atomic-rename temp of the merged result, which the exchange swaps in.
  The tests check both:
  - no payload byte in MOVE_STATE or in the destination state;
  - no private name beside a live path.
- **Never twice.**
  - A consolidated or covered seat is recorded in the store's new
    `consolidated` table (seat keys only), so an unchanged source is
    reused with 0 source bytes even if the destination edited the merged
    file since. The destination's later edits stand and are never undone
    by a re-merge.
  - A held seat is remembered (`RefusedOutput::ConsolidateHeld`, tag 5)
    under the live file's settled identity: an unchanged rerun refuses it
    again and reads nothing.
- **Accounting.**
  - A consolidated seat is a transfer-record row `consolidated` (new
    `EntryOutcome`, appended so old records decode). The closure accounts
    it as `covered`, with `"consolidation":"consolidate|covered"`, so the
    closure schema is unchanged.
  - Held seats are typed refusal rows, pending review, and hold the gate.
- **`estate-move --sessions`.**
  - Pulls both homes with `--consolidate`. The maps are the move's `--map`
    set plus SOURCE_HOME → DEST_HOME, and the ledger is
    `MOVE_STATE/consolidate/consolidate.sqlite`.
  - Merges `~/.claude.json` by a `--file` pull. A source with none leaves
    no record.
  - There is no beside suffix and no always-beside Codex index.
  - The re-seat gets no `--beside-suffix`.
- **A-106 is a union now, never neo-wins.**
  - `--replace-archived-dest` is no longer forwarded (`estate-move
    a106=union-pending`).
  - A source transcript or rollout whose live path is an archive link is
    held at the pull.
  - At the lane's key the re-seat keeps the destination's fork: its
    archive link is regularized in place (OI-1003-Q198) and the archive is
    untouched. Neo's copy is kept at its own key, `diverged`, so both are
    kept until the append-log union merges them.

## Item 3: `--plan-only` for the homes

`estate-move --plan-only --sessions` runs the homes' pulls as a dry
receive (`Consolidation::plan_only`). Every seat is decided and printed as
`planned PATH class= action= source_size= live_size= [reason=]`; actions
are create, adopt, extend, covered, consolidate or hold.

What it never does:
- stage, ask for chunks, create a directory or symlink, or write the store;
- record a transfer (a plan never replaces a pass's record).

Directories are answered `Skip`, and a planned file `Held{held:false}`, so
the source records nothing either. A live home that does not exist yet is
named, not made.

## Known limits

- An entry the source streams in place of its manifest (#77 F1, a source
  that lost its manifest chunks) is staged as a stream is, on disk, before
  it is judged. That temp is removed after the merge or hold. It is the one
  path where rule 2's "no payload copy" holds only transiently.
- On Darwin there is no `memfd`: a differing config is held typed there.
  The homes destination is Linux.
- With the Codex store held and no destination index, the re-seat refuses
  `RESEAT_INDEX_ABSENT` until the row union lands. Where the destination
  has an index, the source's rollouts are registered from their headers.
- A writer that opened the live config before the union's exchange and
  writes after its post-exchange check writes into the old inode. The
  window is two syscalls long, the same class as A-106's residual.

## Receipts

Ruling R-N13 (receipts cite it); builds and tests on honey only
(OI-1003-Q196). Lane env:
- `nice -n 19`, `CARGO_BUILD_JOBS=4`, `RUST_TEST_THREADS=4`;
- `CARGO_TARGET_DIR=~/git-build/target-homes-consolidate`, debuginfo off;
- `TMPDIR=/var/tmp/jess-bulkload-tests/pull-homes-consolidate-20261020/tmp`.

Item 2, at its commit:

| Check | Result |
|---|---|
| `cargo fmt --all -- --check` | ok |
| `cargo clippy -p bulkload-agent --all-targets --locked -- -D warnings` | ok |
| `cargo test -p bulkload-agent --lib -- consolidate::` | 17 passed |
| `cargo test -p bulkload-proto` | 18 passed |
| `--test refusal_taxonomy` | 5 passed |
