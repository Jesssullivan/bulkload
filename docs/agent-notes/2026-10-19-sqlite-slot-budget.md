# 2026-10-19: SQLite slot budget, busy retry, corrupt carry-beside (C3, C4)

Lane: `feat/sqlite-slot-budget-20261019`, from origin/main `01be8c9`. Edits
on sting; every build and test on honey (worktree
`~/git-build/bulkload-01be8c96efab/wt-sqlite-slot-budget`, own target dir,
`nice -n 19 nix develop .#default --command ...`). No link traffic, no live
estate state touched.
Rulings: R-N13; plan of record 2026-10-19 revision 2 (tummycrypt
`docs/agent-notes/2026-10-19-bulkload-reset-review.md`, Step C gaps C3 and
C4); operator 2026-10-18 A-115 "Chunked snapshot"; SPEC R1 (KEEP-02,
KEEP-14, C14/RUL-14 "carried as files and marked corrupt"), R2.1 (LIVE-08,
never halt the source), Q185/Q192 (nothing excluded by content).

Requirement served, verbatim (SPEC R1): "Everything is carried, regardless
of content ... SQLite stores (KEEP-02 ...)"; "the 35 corrupt SQLite rescue
copies and the two corrupt `logs_2` copies, carried as files and marked
corrupt in closure (C14; replaces CONF-19, CONF-20)". SPEC R2.1: "SQLite
stores that are being written live ... are captured consistently while
writers keep writing (LIVE-08 ...)". Acceptance 1: "every refusal is typed
and dispositioned ... `carried-as-files` counts as carried (... RUL-14 for
the corrupt copies, which closure also marks corrupt)".

## What the 2026-10-18 pull refused, and what this lane changes

`pull --sqlite=snapshot` of neo's Codex home (sting, 21:54Z): 12
`BUDGET_EXCEEDED`, 24 `SQLITE_INTEGRITY_CHECK_FAILED`, 11
`SQLITE_STATE_CHANGED`, 1 `FIELD_DOMAIN_VIOLATION`; the live
`thread_history_1.sqlite` is 8.8 GB, 11 backups are over 4 GiB.

| Gap | Before | After |
|---|---|---|
| C3 slot cap | `SLOT_BYTES` 4 GiB, a store over it refused `BUDGET_EXCEEDED` | `--sqlite-slot-bytes=BYTES` (default 4G); a store over the whole budget takes the one oversize slot, outside the budget, bounded by `--min-free-percent` and the manifest bound; its snapshot is cut into fixed chunks of whole pages (`chunked_size`: 8.8 GB -> 128 KiB chunks) |
| C3 wall | `CARRY_WALL` 30 s | `--sqlite-carry-wall=SECONDS` (default 30); an oversize store gets +1 s per 32 MiB, at most 30 min; `pull` forwards both flags to the remote `serve` |
| C3 busy step | refused `SQLITE_STATE_CHANGED` on the first lock | retried with backoff 1 ms doubling to 256 ms inside the wall, holding no lock of its own; refused only when the wall is spent (`source_sqlite_busy_retries`) |
| C3 sidecar fan-out | every sidecar of a refused database a second `SQLITE_STATE_CHANGED` | the sidecar follows its base's one refusal (`sqlite_sidecars_follow_refused`; closure `covered`, outcome `sidecar-follows-refused-base`) |
| C4 corrupt | `SQLITE_INTEGRITY_CHECK_FAILED`, default `abandon` (Q148) | carried beside its path as `<db>.corrupt-<YYYYMMDD>` (UTC date of the source file's mtime), never installed as a database; `carried-corrupt` in the transfer record, closure `carried`; the rerun finds it held and reads nothing; `abandon` withdrawn, no refusal has a default |

## Design decisions to review

- **The slot file is the whole snapshot.** The backup API writes a
  database: a restart rewrites earlier pages and the last step finishes
  the header, and `integrity_check` needs the whole file, so no byte can
  leave for the wire before the backup is complete. "Never holds more than
  the slot" is therefore met for memory (one chunk at a time) and for the
  slots beside the oversize one (the budget), not for the oversize slot's
  own disk, which the free-space floor bounds. The brief's alternative
  ("streamed to the destination slot as it fills") is not possible with
  the backup API and no wire change; this is the equivalent chosen.
- **What a corrupt store lands as.** A snapshot whose `integrity_check`
  fails lands as the backup's page copy (every page the pager could read,
  WAL frames merged, journal mode DELETE), not as the raw main file: the
  repo rule "never copy live WAL/SHM bytes" (AGENTS.md) holds, a live
  corrupt store is still one consistent state, and its sidecars are
  covered. For the 24 static rescue copies the pages are the file's; the
  header's change counter and version fields are SQLite's. A backup that
  itself meets corruption (`SQLITE_CORRUPT`/`SQLITE_NOTADB`) beside no
  `-wal` with frames lands as the raw bytes, exactly as they are.
- **The beside name is a function of the row** (path + mtime date), so an
  unchanged store lands once and the rerun skips it (R2.4). A corrupt store
  modified since lands under its own date beside the earlier copy; two
  states of one store on the same day refuse `DESTINATION_OCCUPIED` on the
  second (the first stays). Known limit, named below.
- **No wire change.** The corrupt carry is announced as a snapshot; the
  destination's own checks tell it apart and land it beside. An older
  destination would refuse it `SQLITE_INTEGRITY_CHECK_FAILED` as before,
  never install it. An older source's remembered integrity records are
  ignored in snapshot mode so the 24 stores are carried on the next run.

## Gaps left

- A backup that meets corruption beside a `-wal` with frames stays a typed
  refusal (`SQLITE_BACKUP_FAILED`, pending review): the raw main file could
  miss the `-wal`'s transactions, and no entry exists for the `-wal` on wire
  v6.
- A changed corrupt store landing at a beside name that exists is refused
  `DESTINATION_OCCUPIED` (no superseding exchange for beside files).
- `--sqlite-slot-bytes` and `--sqlite-carry-wall` reach `serve` as flags;
  an older remote agent does not know them (C14 of the plan, same build on
  both ends, is still open).
- The fault harness has no row for the oversize or chunked paths (unit and
  integration tests only).
