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

Produced: PR https://github.com/Jesssullivan/bulkload/pull/278, signed
commits `77fa2ce8` (the change), `4b1fe938` (this note), `a8421bc5`
(rustfmt), `5189c99d` (clippy), and the P76 registration commit that also
carries this paragraph: forwarding the two flags changed `pull_command`'s
registered `ssh` child, so its shape and argument in
`tests/source_command_registry.rs` are updated as `b7f46e46` did for
`--source-ledger-sync=full`. The receipts (`cargo test -p bulkload-agent
--locked`, `just rust-check`, host honey) are in the PR body, at the PR's
final head, with the `TMPDIR` they ran under. A parallel, uncommitted draft of chunked snapshots sits in the
local worktree `bulkload.worktrees/sqlite-chunked-20261018` (branch
`feat/sqlite-chunked-snapshot-20261018`, never pushed, last touched
2026-10-18 22:42Z); this lane does not build on it and left it as it was.

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

## Host-dependent test results on honey (not this change)

Two existing tests pass or fail by where `TMPDIR` lives, which this lane
found while taking receipts; neither touches the SQLite code:

- `tests/estate_adopt.rs` (31 tests) runs `estate-capture` with the default
  `--min-free-percent=25`. With `TMPDIR` on honey's `/home` (88 % used)
  every capture refuses `DESTINATION_SPACE_INSUFFICIENT`.
- `materialize::tests::the_ext_magic_reads_its_mount_type_by_major_and_minor`
  writes a synthetic mountinfo whose first line is the device `253:0` as
  xfs, then looks up `temp_dir()`'s device. honey's root filesystem (and
  `/tmp`, `/var/tmp`) is `253:0`, so the lookup hits the xfs line and the
  test fails; on any other device it passes. The real gate drops `TMPDIR`
  (`nix develop --ignore-environment`), so on honey it meets this too.
- Earlier honey lanes passed both with `TMPDIR` under `/dev/shm`, which the
  global rule R-C856 now forbids. Per the coordinator (2026-10-19), this
  lane ran its receipts under `/var/tmp/jess-bulkload-tests/<lane>` (root
  device, 49 % used), which clears the floor and meets the collision. A fix
  belongs to the owners of those tests (pass `--min-free-percent=0` as
  `estate_carried`/`estate_move` do; key the synthetic root line off a
  device that is not the handle's), not to this lane.

Process note: one honey probe in this session ran `pgrep -f` on the lane's
own scripts, which the repo's R-N104 ("never check whether a PID is
alive") rules out; every later wait was on files.

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

## 2026-10-19: merge of main and the adversarial review of #278

Merge: signed merge commit `68546a61` of origin/main `9854236` (#281, #282);
one doc-only conflict in `closure.rs`, both sentences kept. #281's
`638ef089` fixed the ext-magic fixture this note's host section names.

Review (PR comment, coordinator seat, at `42c9d99a`): source side safe;
destination and closure not yet. Fixed on this branch, each with a
regression test, in the commit after the merge:

| Item | Fix | Test |
|---|---|---|
| 1 (high) could not check recorded as corrupt | `verify_received` lets `SQLite` judge first: `Corrupt` only for `SQLITE_CORRUPT`/`SQLITE_NOTADB` or rows not `ok`; any other open, prepare or step failure is `SQLITE_INTEGRITY_CHECK_FAILED`; a sound file whose header is not a snapshot's is refused | `a_check_that_could_not_run_is_refused_not_corrupt`, `a_sound_store_whose_check_cannot_run_is_refused_not_carried_corrupt` (545-byte root) |
| 2 (med-high) sidecar follows a plain file's refusal | follows only a base proven a SQLite seat: announced by `SqliteSnapshot`, or refused with `SQLITE_BACKUP_FAILED`, `SQLITE_INTEGRITY_CHECK_FAILED`, `SQLITE_SOURCE_NOT_OWNER`, `SQLITE_SOURCE_AS_ROOT` or `SQLITE_STATE_CHANGED` | `a_file_named_like_a_sidecar_beside_a_refused_plain_file_keeps_its_own_refusal`; P83 split |
| 3 (med) changed corrupt store | outcomes of the beside file are the database's (`SqliteOutcomes::besides`); second name `<db>.corrupt-<YYYYMMDD>-<HHMMSS>.<ns>`; same bytes adopted by content; held check looks at both names | `a_corrupt_store_changed_on_the_same_day_lands_beside_its_first_carry`, `a_failed_corrupt_carry_is_one_refusal_under_its_database` |
| 4 (med) oversize queueing, restarts | oversize slots taken at once, several at a time, each bounded by the floor; slot wait its own; carry budget is the wall plus at most 4 restarts before half a pass; oversize wall base + 3 passes at 32 MiB/s; `sqlite-backup-restarts` on stderr | `oversize_slots_never_queue_behind_one_another` (review probe), `a_store_restarted_late_in_each_pass_is_given_more_passes` |
| 5 (low-med) wall unbounded | parse 1..=1800; cap applied after `.max(base)`; `checked_add` deadline | flags test, `a_slot_wait_never_overflows_the_clock` |
| 6 (low) | sidecar check after the sound/corrupt decision; raw path only when settled, identity checked after every read (and the reader bounded to the announced size); explicit defaults not forwarded; extended busy codes retried | `a_corrupt_snapshot_lands_beside_whatever_appeared_at_its_path`, `a_raw_corrupt_read_rewritten_under_it_is_refused`, flags test, `extended_busy_codes_are_waited_out` |

Gaps left by the review:

- Item 4, not fixable in code under OI-1003-Q16: a store committed to more
  often than one backup pass takes can never finish, since each commit
  restarts the backup from page 1 and Q16 releases the read lock between
  steps. It now stops after four early restarts with `BUDGET_EXCEEDED`
  and prints its restart count. Holding one read transaction across the
  steps (in WAL mode it does not block writers, only checkpoints past it)
  would finish it; that changes Q16's reading and is the operator's call.
  [2026-10-19, later: ruled, see below; this gap is closed for WAL-mode
  sources and stays as written for rollback-journal ones.]
- Item 2: a database refused before it was announced with a code any file
  may get (`BUDGET_EXCEEDED` at the source, `DESTINATION_OCCUPIED` at its
  decision) is not proven a database, so its sidecars stay refused by
  name (one row each, as before this lane). Closing that needs a typed
  code for a database's budget refusal.
- Item 3: a store that is changed, then only touched, lands its touched
  copy under a new timed name (reuse by content looks at the dated name
  and the row's own timed name only).

## 2026-10-19: operator ruling "Hold one read snapshot"

Ruling (operator interview 2026-10-19, relayed by the coordinator; it
replaces OI-1003-Q16's short-locks-only reading for this case): "Hold one
read snapshot". For a WAL-mode source the carry backup holds one read
transaction for the whole copy, so commits no longer restart it; the
source's writes are never blocked; its checkpoints cannot pass the
snapshot until the copy ends, so its `-wal` grows for the copy's duration
and then checkpoints normally. The carry wall (1 to 1800 s, oversize
longer) still bounds the hold: at the wall the snapshot is released and
the store refused `BUDGET_EXCEEDED`, never held past it. Rollback-journal
sources keep the per-step lock (a long read lock there would block
writers).

Built: `provider_sqlite::hold_read_snapshot` (journal mode read, `BEGIN`
plus one read to take the snapshot, busy start retried inside the wall),
used only by a carry backup (`StepBudget::Carry`); the provider verbs keep
per-step locks, so P77's lock traces of the `snapshot` verb are unchanged.
The wall runs from before the snapshot is taken; the snapshot is rolled
back when the copy ends, refused or not. Reported per store on stderr
(`sqlite-snapshot-held PATH held_ms= wal_before= wal_after= wall_s=
outcome=`) and counted (`source_sqlite_snapshot_holds`,
`source_sqlite_snapshot_held_ns`, `source_sqlite_held_wal_before_bytes`,
`source_sqlite_held_wal_after_bytes`).

Tests: `a_wal_store_written_continuously_is_carried_from_one_held_snapshot`
(writer committing every 1 ms with zero busy timeout through a slowed
copy: no restart, writer never busy and still committing, one committed
state, `-wal` grew, hold counted); `a_held_snapshot_is_released_at_its_wall`
(1 s wall: refused `BUDGET_EXCEEDED`, held 1 to 2 s, then the writer's
`wal_checkpoint(TRUNCATE)` empties the `-wal`, and the next run carries
it); `p80_a_commit_between_steps_restarts_a_rollback_backup_not_a_held_wal_one`
(rollback: restart, commit carried, no hold; WAL: no restart, the
snapshot's own state, one hold); `p80_a_busy_wal_store_...` now always
carried.

Residual: a held copy's `-wal` growth is bounded only by the wall and the
source's write rate; the counters and the stderr line make it visible.

## 2026-10-19: re-review of 86ca6396 and the ruling's caveats

Re-review (coordinator relay, honey probes): items 1, 3, 4, 5 fixed; 2
partial; 6 fixed except busy codes. Folded in with the ruling's caveats:

| Finding | Fix | Test |
|---|---|---|
| A (med) `SQLITE_STATE_CHANGED` taken as proof of a SQLite seat | dropped from `sqlite_seat_refusal`: the source gives it to any header-less file beside a `-wal` with frames | `a_user_file_named_like_a_wal_beside_a_plain_file_keeps_its_own_refusal`; P83 encrypted-store leg back to by-name |
| B (med-low) a fatal step rc is sticky on the backup object (`sqlite3.c` 3.46) | an extended busy code (`SQLITE_BUSY_RECOVERY`) is waited out on a new `Backup`; never stepped again on the old one | `an_extended_busy_step_is_waited_out_on_a_new_backup_object` (real SQLite: OFD locks on the `-shm` WRITE and RECOVER bytes give 261; the same object returns 261 after release; a new one goes on; the carry finishes) |
| C (low-med) one full copy per change | a new timed copy, once committed, retires this store's own earlier timed copies (identity recorded by this store, untouched), recorded `corrupt-copy-retired` | `a_changing_corrupt_store_keeps_one_timed_copy_and_never_touches_others` |
| caveat ii, `-wal` growth not counted by the floor | source floor re-checked every 250 ms while held; under it, released and `BUDGET_EXCEEDED` | `a_held_snapshot_is_released_when_the_source_floor_is_crossed` |
| caveat iii, release on any error | a held run returns at once on any failure (busy included); the caller rolls back | `a_held_snapshot_is_released_at_once_when_its_backup_fails` |

Caveat i, Codex's checkpoint modes, read at openai/codex `79a19cbb3008afe2882ccc2c5d93f02cde9a0823`:

- `codex-rs/state/src/runtime/logs_maintenance.rs` L44: `PRAGMA wal_checkpoint(PASSIVE)` (startup, logs DB).
- `codex-rs/state/src/runtime/reclamation.rs` L415-L423 `checkpoint_complete`: `PRAGMA wal_checkpoint(PASSIVE)`; its maintenance connection sets `busy_timeout(0)` and `wal_autocheckpoint=0` (L236-L241), and backs off as `Contended` when a checkpoint cannot complete (L318, L355).
- `codex-rs/state/src/sqlite.rs` L380-L384: the read-write pools set `synchronous(Normal)` and `busy_timeout`, no `wal_autocheckpoint` override (SQLite's automatic checkpoint, PASSIVE); L503 `journal_mode = WAL` on a new DB only.
- `codex-rs/state/src/runtime.rs` L599 `journal_mode=DELETE; BEGIN EXCLUSIVE` is inside the test `sqlite_integrity_check_can_be_interrupted_and_retried` (L549-L550), not product code.
- Codex pins `sqlx = "=0.9.0"` (`codex-rs/Cargo.toml` L489); sqlx-sqlite 0.9.0's sources contain no `wal_checkpoint`, and `optimize_on_close` defaults to disabled (`sqlx-sqlite/src/options/mod.rs` L215), which Codex never enables.

So Codex issues only PASSIVE checkpoints, which never wait for a reader:
the held snapshot defers them (and Codex's reclamation reads that as
contended and retries later) and blocks none of its writes. Caveats: this
is Codex's main branch, not necessarily the build installed on neo; and
a source that issues FULL, RESTART or TRUNCATE checkpoints is outside the
ruling.

## 2026-10-20: merge of #280 (b7b9d10)

#280 (live-home `--beside`, listing, owner-checked roots, A-106) merges
first; #278 merged its head with a signed merge (R-N13 receipt) and kept
both sides:

- `EntryOutcome`: `AppliedBeside` keeps #280's position, and this lane's
  three variants follow it, so a record decodes in main's order.
- A beside-path committer outcome is the source path's (#280). A corrupt
  carry's outcome is its database's (item 3).
- #280 split `entry` into `decide_*`. `decide_regular` still skips a
  corrupt carry held beside the path, with nothing read.
- A corrupt snapshot under `--beside` keeps the seat's own row and key. It
  lands at `<db>.corrupt-<YYYYMMDD>`, a name of its own that never touches
  the live file, and is awaited under that file. As auto-merged, it would
  have been named after the beside path and awaited under the wrong name.
- Not covered by a test: a corrupt snapshot pulled with `--beside`.
