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

## Item 2: the structured-config merge (this commit)

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

## Item 1: per-class dispatch (next)

The dispatch belongs in the transfer engine's receive side, as a mode
beside `--beside`, so the source's bytes are read once and nothing is
staged beyond an entry's own private temporary. Plan:

- `pull --consolidate` (the homes' default in `estate-move --sessions`,
  replacing `--beside .LABEL` and `set_beside_always`) decides each
  differing live file by class:
  - **Append logs** (`*.jsonl`):
    - a live prefix is extended in place (existing A-103 path);
    - a live file the source is a prefix of is covered;
    - a diverged one goes to the #294 lane's JSONL union.
  - **Structured config:** `config::consolidate` with the staged bytes.
  - **Codex SQLite:** the #278 lane's row union.
  - **Other files:** equal bytes are adopted; differing bytes are a
    `HOMES_CONSOLIDATE_UNSUPPORTED` hold, remembered under the settled
    identities so an unchanged rerun reads nothing, and listed.
- Until #294 and #278 land their unions, those two classes hold typed with
  that code. They are listed and never besided.
- The transfer record gets `consolidated` and `held` outcomes, which the
  closure accounts for. A rerun with an unchanged source is stat-only.

## Item 3: `--plan-only` for the homes (next)

Report-only: every file of both homes with its class, its action
(adopt, extend, covered, consolidate or hold, with the reason) and the
sizes on both sides. Nothing is written. The operator sees it before
anything lands.

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
