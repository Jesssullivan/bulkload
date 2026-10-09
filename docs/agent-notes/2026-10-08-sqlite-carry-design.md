# 2026-10-08: SQLite carry design (#218)

Lane `sqlite-carry`, branch `feat/sqlite-carry-20261008`, worktree
`bulkload.worktrees/sqlite-carry-20261008`, from `origin/main` `bdb8994`
(#217). **Design only.** No code, no commit, no push, no PR.

Rulings cited: OI-1003-Q136 (the #218 finding), OI-1003-Q117 (the neo
snapshot run), OI-1003-Q16, Q36, Q72 and Q76 (S2's SQLite exceptions),
OI-1003-Q6, Q10 and Q18 (S3), OI-1003-Q1, Q74 and Q75 (S4), OI-1003-Q100,
Q101 and Q102 (the superseding publish, and model-first), R25 / R-N58,
R-N59 / R-N118 (hard wire cuts), R-N13.

Every decision below that changes a ratified rule is marked **needs a
ruling** and is listed again in [Open questions](#open-questions-for-the-operator).
Nothing here is ratified.

**Amended 2026-10-08 after the adversarial review** (section 0). Section 0
records each finding and how it was taken, and the conservative answer
this lane built for each open question pending the operator's ruling.
Where section 0 and a later section disagree, section 0 wins; the later
sections were edited to match it where noted "(amended)".

## 0. Review round (2026-10-08): findings and conservative decisions

### 0.1 Findings

| # | Finding | Taken | Resolution (built) |
|---|---|---|---|
| R1 | #169 adoption of unrowed outputs takes a stale WAL snapshot as current | **accepted** | A snapshot output's capture record has its own domain and is keyed on the walked row **and the settled `-wal` identity** (`unrowed::sqlite_record_key`); its size is the snapshot's, not the main file's. An unsettled capture writes no record (like a racy one). Adoption reads the record once and compares it against the file key and, in `snapshot` mode, the snapshot key. P81 row and the model mutation `sqlite_capture_record_main_only`. `unrowed.rs` is in the file plan. |
| R2 | Sidecar coverage decided by name and base presence: silent loss | **accepted** | A sidecar's outcome is bound to its base's **actual** outcome, resolved at the end of the session on the destination: covered only when the base was published from a snapshot or reused under a snapshot key; otherwise refused by name, `SQLITE_STATE_CHANGED`, as v5 does. A snapshot output's reuse key is always distinct from `row_key` (it carries a snapshot marker, `sqlite_key`), so the row itself says "snapshot seat". A non-snapshot file keeps `row_key`; its `-wal` sibling's identity only adds a second lookup, so no existing row changes key. SQLCipher-style (no magic) and plain-file pairs are P83 layouts. |
| R3 | "Writers are never blocked" is false while the read mark is held | **accepted** | The pinned WAL backup is **not built**: Q16 is not amended (decision D2). Both journal modes use the stepped backup, so no read mark outlives a step. Stated instead: a WAL-mode source connection holds `SHARED` on the main file from open to close (an exclusive-mode opener waits that long), and `source_sqlite_lock_ns` measures exactly that span. The wall-clock cap per backup is narrowed from 120 s to 30 s. |
| R4 | A non-owner read creates foreign-owned `-shm`/`-wal` | **accepted** | Per seat, before any `SQLite` open: a database whose `st_uid` differs from the effective uid is refused with a new code, `SQLITE_SOURCE_NOT_OWNER` (from the `fstat` of the descriptor already open for the sniff: 0 extra source bytes). The root refusal stays session-wide. P82 leg. |
| R5 | Remembered `DESTINATION_OCCUPIED` for a sidecar never clears | **accepted, changed** | The destination sidecar check runs at **Decide**, before `Send` or `WantManifest`, so no source snapshot is taken; it is destination-only, so it is **not remembered** (a record would save no source byte), and a removed sidecar converges on the next run. It runs again just before publish (a narrower window). P83 row: refuse, remove the sidecar, rerun converges. |
| R6 | Superseding under an idle rollback-mode connection can corrupt | **accepted; superseded by OI-1003-Q146** (section 0.4) | Built first as decision D7: **a snapshot output is never superseded in place**. A changed store whose path holds other bytes (this store's own output or not) is refused `DESTINATION_OCCUPIED` and kept; remembered under the snapshot key when settled. Residual of the alternative restated as possible corruption (howtocorrupt §2.4). |
| R7 | Space admission and size bounds use the main file's size | **accepted** | Admission at Decide reserves `row.size + wal.size`; the source announces the snapshot's exact size before any content (`SqliteSnapshot`), and the destination re-admits that size. The chunk-count bound and the retained-slot bound use the snapshot's size. |
| R8 | `refuse` mode not byte-for-byte v5; tag 1 cannot be skipped for a database seat | **accepted** | Tags: 1 legacy (database or WAL, written by `refuse` mode exactly as v5), 2 snapshot integrity, 3 WAL header, 4 snapshot `SQLITE_CORRUPT`, 5 snapshot `SQLITE_NOTADB`. `snapshot` mode ignores tag 1 (re-sniffs once), honours 3 under `row_key` and 2/4/5 under `sqlite_key`; `refuse` mode honours 1 and 3 and never computes `sqlite_key`. Keys differ, so the modes never overwrite each other. `refuse` mode restated: v5's decision for every seat; no raw `SQLite` byte crosses the wire; a snapshot output is not reused (its key differs) and is refused at the source by its sniff. |
| R9 | `SQLITE_OPEN_NOFOLLOW` fails under a symlinked root; residual understated | **accepted** | `serve` already canonicalises the root before opening its descriptor; `SQLite` opens `canonical_root/rel_path`, and the `(dev, ino)` of the walked seat is checked before and after. Residual restated: an intermediate directory swapped and swapped back during the backup can also make `SQLite` create its Q36/Q72 sidecars **outside the root**. A P80 case runs under a symlinked source path. |
| R10 | Remembered corruption must mask the extended result code | **accepted** | `code & 0xff` in {11, 26} (`provider_sqlite::corrupt_code`); a unit test covers 779, 523 and 267; P82 has an index-corruption case. |
| R11 | Model plan misreads `sqlBackups < 1` | **accepted** | Section 16 amended: a separate model (`SqliteCarry.tla`) with total backups ≥ 2 and an in-flight semaphore, main and `-wal` stat'ed in separate steps, soundness defined against the version at the walk, a checkpointer and an exclusive opener, and unrowed adoption with the main-only-key mutation. |
| R12 | The P80 oracle cannot catch the live-file mutant | **accepted** | `k` must be at least the commits acknowledged before the capture; a deterministic row commits from a test hook after the backup and before the chunking, and requires the published rows to exclude that commit; the snapshot's content bytes are counted apart (`read_source_snapshot_bytes`) and `read_source_file_bytes` adds 0 for a snapshot seat. |
| R13 | Counters undercount source reads and lock time | **accepted** | `source_sqlite_backup_bytes` = pages stepped × page size (restarts included) **plus** the `-wal`'s size at the pre-stat (an upper bound on the wal-index rebuild), so the bound is main + 2 × `-wal`; `source_sqlite_lock_ns` is measured from the source connection's open to its close. |
| R14 | Root refusal names only `serve` | **accepted** | `copy` runs `serve` in-process as its source half, so the one check at `serve`'s `Open` covers both; a `copy` leg is in P82 and the model's `SqliteRootRefusedUpFront`. |
| R15 | Wire fields with no consumer | **accepted** | No `SnapshotSeal`: `End` is unchanged. One new control, `SqliteSnapshot{entry, size, wal}`, sent before an entry's first content, carries the snapshot size (needed before content: R7) and the settled `-wal` identity; `End.racy` carries "not settled". `page_size`, `page_count` and `source_wal_mode` are gone. |

### 0.1b Review round 2 (2026-10-08, staged carry)

Eight findings on the built carry. Each real one was fixed with a test that
failed first; the rest are recorded with their evidence.

| # | Finding | Taken | Resolution (built) |
|---|---|---|---|
| 1 | An encrypted store (no magic) is published raw while its live `-wal` is refused | **accepted** | In `snapshot` mode a regular seat offered with a `-wal` that holds frames (`size > 0`) and no database magic is refused `SQLITE_STATE_CHANGED`, never carried raw, and remembered under `sqlite_key(row, wal)` as refused-seat tag 6 (`SqliteBesideWal`) when the main file did not move and was not racy. An empty `-wal` leaves it carried raw. `refuse` mode is v5's, unchanged. P83 `p83_a_sidecar_of_a_base_that_is_not_a_database_is_refused` flipped. |
| 2 | P80's live writer never lands between backup steps | **accepted (coverage)** | Test seam `provider_sqlite::set_carry_steps` (pages per step and a hook after each step). New pinned row `p80_a_commit_between_steps_restarts_the_backup_and_arrives_whole` (WAL and DELETE): a writer commit after step 2 restarts the backup (`source_sqlite_backup_restarts > 0`) and the store arrives with that commit. The P80 property now steps 2 pages. |
| 3 | Closing a sniff descriptor can drop another backup's POSIX locks (hard links) | **accepted** | A sniffed database's descriptor (`Opened`) is closed only under the backup lock: by `take_snapshot` once its connection is closed, or by its `Drop`, which takes the lock. A seat that is not a database releases its descriptor at once. Test `a_sniffed_database_is_closed_under_the_backup_lock` (test-only counter of closes outside the lock: 3 before the fix, 0 after). |
| 4 | A store failing only the destination's `integrity_check` is re-sent every run | **accepted** | The source runs `integrity_check` on the slot (`finish_carry`), so the refusal is the source's and remembered under the snapshot key. P82 `p82_an_index_out_of_step_with_its_table_is_refused_at_the_source_and_remembered` (an index entry left stale, which `quick_check` passes): rounds 2 and 3 read and send 0 bytes. The reviewer's "missing entry" variant is caught by `quick_check` in SQLite 3.46 (`wrong # of entries`); a stale entry is not. |
| 5 | A database seat on the manifest path takes the memory budget before its sniff | **accepted** | A manifest job with no room in the memory budget calls `send_capture` with `want_manifest`: a database seat is still offered as a manifest from its slot; a raw seat streams as before. Test `a_snapshot_manifest_is_not_bounded_by_the_memory_budget` (bytes received 126976 before, 0 after). |
| 6 | No test fails when the key drops the `-wal` or `wal_settled` always settles | **accepted (coverage)** | `p81_a_commit_only_in_the_wal_is_not_reused`, `p81_a_commit_after_the_last_step_leaves_the_capture_unsettled` and the table test `wal_settled_judges_every_pair`; the two scratch mutants were run against them (impl note). |
| 7 | Streamed slots are not bounded; no free-space check | **accepted** | `SlotBudget`: every slot is reserved before its backup and holds the reservation until removed (section 5.6, amended); free-space floor before each backup. Test `snapshot_slots_alive_at_once_stay_within_the_slot_budget` (peak 385024 bytes against a 144384 budget before, within it after). |
| 8 | R9: `SQLite` follows a directory swapped for a symlink | **rejected as stated; code mapped** | `SQLITE_OPEN_NOFOLLOW` refuses a symlink at **any** component (`unixFullPathname` counts symlinks over the whole path and `sqlite3PagerOpen` returns `SQLITE_CANTOPEN_SYMLINK`, SQLite 3.46.0), not only the leaf, so the stated swap is refused and nothing outside the root is read. That refusal now reports `PATH_ESCAPES_ROOT` (it was `SQLITE_BACKUP_FAILED`). Test `a_directory_swapped_for_a_symlink_before_the_backup_is_not_followed` (swap from a new before-backup hook). Residual restated: a swap between `SQLite`'s own path walk and its `open`, or before it opens the `-wal`/`-shm` by name. |

Found while rerunning the checks (not one of the eight): the P80 property
failed 2 of 18 parallel runs under load: a live DELETE-mode store's
`store.db-journal`, listed and deleted by a commit before the walk's stat,
was refused `IO`. The walk now yields such a name, beside a listed regular
base, as that base's sidecar in a `snapshot` walk (`Walker::sidecar_of`);
an orphan is still refused. Test `walk::tests::a_sidecar_gone_before_its_stat_is_its_bases_sidecar`
(failed first); P80 then passed 18 of 18 parallel runs.

### 0.2 Conservative decisions on the open questions (not rulings)

The lane was told to decide conservatively and record. Each is the
smallest change, and each is reversible by a ruling; none changes SLO
text (section 0.3 lists what the operator must rule).

| Q | Decision built |
|---|---|
| D1 (Q1) | Option (a) is built behind `--sqlite=refuse|snapshot`; the **default stays `refuse`** until P80–P83 and the neo→sting field confirmation pass. |
| D2 (Q2) | **Q16 is not amended.** No pinned read transaction: the stepped backup (one `step(128)`, the read lock released between steps) in both journal modes. A busy store may restart until its step budget ends `BUDGET_EXCEEDED` (not remembered; re-carried), counted in `source_sqlite_backup_restarts`. |
| D3 (Q3) | `integrity_check` on the snapshot at the source (amended by review round 2, finding 4: was `quick_check`, which passed stores the destination then refused on every run without a record) and again at the destination; foreign-key violations are carried and counted (`source_sqlite_fk_violations`), not refused. |
| D4 (Q4) | Destination journal mode DELETE, as the `snapshot` verb produces. |
| D5 (Q5) | As root: the whole `snapshot`-mode session is refused at `Open`, before any store or source open. |
| D6 (Q6) | Measure first: no IO-class change, the step stays 128 pages (as the ratified verb). |
| D7 (Q7) | ~~Never supersede a snapshot output in place (R6).~~ **Replaced by OI-1003-Q146** (section 0.4): supersede through the WP0(d) exchange when owned and untouched and no sidecar at Decide or at the exchange. |
| D8 (Q8) | v6 keeps the W6 reserved frames unchanged; WP3 cuts v7. |
| D9 (Q9) | P80–P83 allocated (next free on `origin/main` `bdb8994`). Engineering defaults: one backup at a time per `serve`; 4 GiB of snapshot slots alive at once, streamed or retained, each reserved before its backup with a wait of up to 30 s for room, and the slot filesystem kept above the `--min-free-percent` floor (review round 2, finding 7; a store that does not fit is refused `BUDGET_EXCEEDED`, not remembered); step budget `max(4, ceil(pages/128) × 4)`; **30 s** wall clock per backup (R3). |
| D10 (Q10) | Typed refusals only; no raw exception for corrupt rescue copies. |
| D11 (Q11) | Name-based matching stays for orphans; a sidecar with a base follows the base's outcome (R2), so `foo` + `foo-journal` with `foo` not a database keeps v5's refusal of `foo-journal`. |
| D12 (Q12) | S4 for transfer refusals depends on WP3 PR 4: accepted as a dependency, not a blocker. |

### 0.4 Operator rulings, 2026-10-09 (Linear TIN-4543)

Three of the open questions are ruled. Where a ruling and anything else in
this note disagree, the ruling wins; the sections it touches are marked
"(Q146)".

| Ruling | Text (summarised) | Built |
|---|---|---|
| **OI-1003-Q146** (replaces D7; answers question 7) | A changed SQLite store **supersedes** its landed snapshot output through the existing WP0(d) exchange path (`prepare_supersede`, intent row, `RENAME_EXCHANGE`, displaced check), only when bulkload landed that output and its ownership proof holds (untouched since), **and** no `-wal` or `-journal` sidecar exists beside it at the destination at decide time, re-checked immediately before the exchange. Otherwise a typed refusal, reusing codes: `DESTINATION_OCCUPIED` for not-owned; the sidecar-present refusal as today (`DESTINATION_OCCUPIED`). | The `if snapshot` short-circuits (`Plan::Occupied`, and `supersede = None` on the streamed path) are gone, so a snapshot output takes the files' path (`plan_file`, `owned_output`). A verified snapshot's staged file is marked (`StagedFile::mark_sqlite`); its exchange looks for a `-wal`, `-journal` or `-shm` beside the leaf (`sqlite_sidecar_appeared`) after the last identity look and the test hook, immediately before `io::exchange`, and refuses `DESTINATION_OCCUPIED` with nothing exchanged and the old rows given back (`Exchanged::Undone{restore}`). The fresh rename makes the same look. `-shm` is checked too (stricter than the ruling; as the Decide check always did). Counters `dest_sqlite_superseded`, `dest_sqlite_sidecar_refused`. |
| **OI-1003-Q147** (answers question 1) | The CLI default stays `--sqlite=refuse`; `snapshot` is opt-in. The full neo→sting run passes `--sqlite=snapshot`. | Already the build; USAGE and design.md say so. |
| **OI-1003-Q148** (answers question 10 for integrity failures) | A store failing `integrity_check` is refused `SQLITE_INTEGRITY_CHECK_FAILED`; its default disposition in the S4 closure/disposition ledger is `abandon`. | Transfer refusals are not closure-ledger rows yet (the ledger holds planned items only; WP3 PR 4). So, as ruled for that case, the ruling is recorded in docs and in the refusal's report text: the transfer prints `default-disposition <path>: abandon (SQLITE_INTEGRITY_CHECK_FAILED, OI-1003-Q148)` beside the refusal (`transfer::default_disposition`). No ledger integration is built. **Backup-detected corruption** (review of the Q146 change, round 4, 2026-10-09): a store whose corruption the backup step reports (`SQLITE_CORRUPT` 11, `SQLITE_NOTADB` 26, extended codes masked) never reaches `integrity_check`; it keeps its code `SQLITE_BACKUP_FAILED` (remembered as `SqliteSnapshotCorrupt` or `SqliteSnapshotNotADatabase`) and takes the same `abandon` default line, as an integrity-failed store under Q148. The source sends `SQLite`'s code beside the refusal (wire v6 `Refused.sqlite_code`), `TransferStats::refusal_sqlite_codes` keeps it, and `transfer::default_disposition(code, sqlite_code)` decides. Any other backup failure (busy, I/O, a hot journal) has no default. |

Residuals that Q146 accepts, stated (corrected 2026-10-09, review of the
Q146 change): (1) a `-journal` or `-wal` created between the last `lstat`
and the exchange; (2) an application whose SQLite predates 3.8.3, or that
uses a VFS whose files do not track their inode (on Linux `unix-none`,
`unix-dotlock`, `unix-flock`, a custom VFS: `fileHasMoved` needs the
`unixInodeInfo` only the default posix-lock style keeps), whose next write
after the exchange can create its `-journal` beside the new file. The
mechanism, read in the bundled SQLite 3.46.0: `pager_open_journal` calls
`databaseIsUnmoved` before it opens a rollback journal, which asks
`SQLITE_FCNTL_HAS_MOVED`; the unix VFS `stat`s the path and compares the
inode it opened, and a moved file fails the write
`SQLITE_READONLY_DBMOVED`. R6's scenario as first written (an idle rollback-mode connection on
the old output journaling beside the new file on its next write) does not
hold for a modern SQLite on the unix VFS: since 3.8.3 such a connection's
next write fails `SQLITE_READONLY_DBMOVED` and creates no `-journal`.
Checked with SQLite 3.51.2, the old file replaced by rename under an open
connection: an idle connection, and one in `BEGIN IMMEDIATE` that had not
journaled, both failed "attempt to write a readonly database"
(`SQLITE_READONLY_DBMOVED`), no `-journal` appeared, and the new file
stayed `integrity_check` ok with its own rows. A connection whose
`-journal` already exists is exposed only through the `lstat` window (1);
outside it the look sees the `-journal` and refuses.

### 0.3 For the operator (not decided here)

(Amended 2026-10-09: D1's default and D7 are ruled, section 0.4.)

- D1's default flip, D2's pinned mode (with the narrowed claim of R3), D7
  (supersede with a lock-byte probe instead of never) and D9's numbers
  need rulings; this lane changed no SLO text.
- `SQLITE_SOURCE_NOT_OWNER` (R4) adds one code to the S4 closed set.
- The rollback-mode factor in S3 inequality 1 (section 13) and the
  main + 2 × `-wal` bound (R13) need a line in the S3 amendment when
  `snapshot` becomes a default.

## 1. The problem

`pull` refuses every seat whose first 16 bytes are a SQLite database or WAL
magic, and every `-wal`, `-shm` or `-journal` by name, with
`SQLITE_STATE_CHANGED` (`transfer.rs` `capture_file`; design.md "Refused
seats", #186). That is right for live provider state: raw bytes of a live
database can be torn. But it refuses quiescent backup-API snapshots too, so no
bulkload verb carries SQLite between hosts. `snapshot`, `compose`,
`compose-state`, `hydrate-state` and `apply-state-candidate` are all local.
The product bar ("snapshot, then compose, then apply") is missing its middle
step.

What happened on 2026-10-08 (files in `/srv/fast-local/jess/bulkload/neo-20261008/`):

| Step | Script | Result |
|---|---|---|
| `snapshot` each store on neo, one process per store | `neo-snapshot.sh`, `neo-snapshot2.sh` | 2,724 stores listed; 2,689 snapshots; 35 refused (left as `.err` files) |
| `pull` the snapshot set | `sting-pull-sqlite.sh` | `refusals=2724`, `completed=35`: every snapshot refused `SQLITE_STATE_CHANGED`. The only files carried were the 35 `.err` texts (69,280 bytes) |
| zstd each snapshot, sha256 manifest | `neo-zst-wrap.sh` | 2,689 wrapped, plus the manifest |
| `pull` the wrapped set | `sting-sqlite-unwrap.sh` | `completed=2690`, 6,636,537,148 bytes, 438.8 s elapsed; source `cdc_hash_ns` 1,193 s summed over 4 workers |
| unwrap, `sha256sum -c`, `PRAGMA integrity_check` | `sting-sqlite-unwrap.sh` | 0 sha failures, 0 integrity failures |

The workaround is sound for what it is, but it is not product:

- it is four tools and two hosts' scripts, with nothing tying a wrapped file
  to the store it came from;
- it has no S3: every rerun re-snapshots, re-wraps and re-reads every store;
- it defeats the sniff by renaming the bytes; nothing stops a raw live
  database from being wrapped the same way;
- its refusals (the 35) are `.err` files, not typed outcomes.

## 2. What the native path must guarantee

| Id | Requirement | From |
|---|---|---|
| G1 | A live SQLite store on the source arrives on the destination as **one consistent snapshot**: the database as of one committed transaction, never torn raw bytes. | #218, AGENTS.md "Capture SQLite through its backup API" |
| G2 | The live `-wal`, `-shm` and `-journal` are **never carried raw**, in any mode. | AGENTS.md, design.md "Live union" |
| G3 | The destination **verifies** what it publishes: chunk digests and manifest root (as for every file), then `PRAGMA integrity_check`. A file that fails is never published. | #218 workaround step 3 |
| G4 | **S2.** The source access is the backup API's bounded shared read (Q16), with Q36's `-shm` and Q72's empty `-wal` as the only writes, counted. No new lock or write class. Background priority. **Never as root** (Q76). | slo.md S2 |
| G5 | **S3.** An unchanged store is not read again: 0 content bytes, 0 sniff bytes, 0 backups, 0 source locks on a warm rerun. A changed store is read once; the wire carries only absent chunks. | slo.md S3, R25 |
| G6 | **S4.** A corrupt or unreadable store is a typed refusal from a closed set, never `IO` or `FRAME_CODEC`, and is remembered so an unchanged corrupt store is not re-read. | slo.md S4, #186 |
| G7 | `SQLITE_STATE_CHANGED` stays for raw bytes. Nothing in this design lets a raw SQLite or WAL byte cross the wire. | #218 "Live SQLite ... stays refused" |

## 3. Options weighed

### (a) `pull` gains a SQLite seat kind (chosen)

The serve side takes a backup-API snapshot of the live store into its own
private state, and streams **the snapshot** as the seat's content, marked on
the wire as a snapshot. The destination verifies it (digests, root,
`integrity_check`) and publishes it at the seat's path.

- One verb. The source never needs a separate snapshot set, so nothing
  persistent doubles the source's disk.
- S3 is native: the seat's identity decides `Reuse` before the source opens
  anything (section 5).
- The "sealed" property the issue asks for comes from provenance, not a
  manifest file: the bytes streamed come from a file the serve process
  itself wrote through the backup API inside its private state, which the
  existing `SNAPSHOT_ROOTS_OVERLAP` check keeps disjoint from the source
  root. A raw file can never be presented as a snapshot.
- It reuses the transfer's whole machinery: CDC chunks, chunk hints (so a
  changed store's new snapshot crosses the wire as a delta against the old
  one), group commit, `Held`, the superseding publish, remembered refusals.
- Already-quiescent snapshot files (the issue's case, and neo's existing
  snapshot set) are simply SQLite files: they are carried the same way.

Cost: a wire cut (section 9), and a source-side write of each changed
store's snapshot into private state (it is read back from page cache to
chunk it).

### (b) An estate item kind for SQLite stores

Rejected. Estate items are reviewed plan entries (2,724 of them here), with
their own capture, apply and closure path. An estate capture still lands in a
local corpus that `pull` must carry, so (b) still needs (a) or (c) to cross
hosts. It adds a planning burden and no transport.

### (c) `pull` carries provider snapshot objects explicitly

The issue's sketch: `snapshot` writes a sealed manifest beside its output,
and `pull` carries SQLite-magic seats only under a root the manifest marks as
a snapshot set, verifying each against the manifest.

Rejected as the primary path, for three reasons:

- **S3 fails at the first step.** `snapshot` has no change detection, so
  every rerun re-snapshots every store (G5). Adding a ledger to `snapshot`
  rebuilds (a)'s key in a second place.
- **Trust in a marker.** The root marker is a source-side file. A raw copy
  of a live database dropped into a marked root is caught only if its sha
  disagrees with the manifest, and the manifest is written by the same
  operator flow that might have made the mistake.
- **Two persistent copies on the source** and two verbs to orchestrate per
  run, which is the workaround's shape with the zstd step removed.

### Variants considered and rejected

- **Productise the zstd wrap.** Still two steps, still no S3, and it makes
  "defeat the sniff" a supported operation.
- **Serialize to memory** (`sqlite3_serialize` of a `:memory:` backup).
  Bounded only by the largest store; kept as a later optimisation for small
  stores (below, section 7) so they skip the private-state write.
- **`VACUUM INTO`.** One read transaction like the WAL path below, but in
  rollback mode it holds `SHARED` for the whole copy and blocks writers
  (S2). It also rewrites the database (no page identity with the source), so
  the delta between two snapshots is worse.
- **Checkpoint then copy raw.** A checkpoint writes the source. Refused by
  S2.

## 4. Decision

Build (a): a **SQLite snapshot seat** in the file transfer (`pull` and
`copy`), behind a session mode the destination chooses:
`--sqlite=refuse|snapshot`. `refuse` is exactly today's behaviour. The
default stays `refuse` until the operator rules it otherwise (open question
1).

## 5. Data model

### 5.1 Seat classes on the source, in `snapshot` mode

| Seat | How it is recognised | What happens |
|---|---|---|
| SQLite database | Its first 16 bytes are `SQLite format 3\0` (the sniff, read only when the destination asked for content) | Snapshot seat (5.3) |
| SQLite sidecar | A regular file named `<base>-wal`, `<base>-shm` or `<base>-journal` whose `<base>` is a regular file in the same directory listing | Never carried. Reported as `SqliteSidecar{rel_path, database}`, not an `Entry`; its outcome is its base's actual one (amended, R2; section 8) |
| Orphan sidecar | Such a name with no `<base>` | Offered as an `Entry` and refused by name, `SQLITE_STATE_CHANGED`, as today |
| WAL-magic file not named `-wal` | Sniff sees `0x377f0682` or `0x377f0683` | Refused `SQLITE_STATE_CHANGED` and remembered (amended, R8: `RefusedSeat::SqliteWalHeader`, tag 3): a stray WAL is not a database |
| Anything else | | Unchanged |

In `refuse` mode every row keeps today's behaviour, including the remembered
`SqliteHeader` refusals of #186.

### 5.2 The store's identity (the S3 / R25 key)

A WAL-mode commit appends to `-wal` and leaves the main file untouched until a
checkpoint, so the main file's stat identity alone cannot vouch for a store.
The key is the **pair**:

```
StoreIdentity = (main: StatIdentity, wal: Option<SidecarId>)
SidecarId     = (dev, ino, size, mtime_ns, ctime_ns) of <base>-wal, or None when absent
```

Why not the counters the task names:

- **`PRAGMA data_version`** is per connection. Two values from different
  connections (or runs) say nothing about each other.
- **The file change counter** (header offset 24) is not maintained per
  transaction in WAL mode (SQLite file-format doc: "the change counter might
  not be incremented on each transaction in WAL mode"). It is also a source
  read: checking it on every warm run costs sniff bytes on every unchanged
  store, which S3's 0-sniff clause forbids.

The pair is metadata only (the walk already stats `<base>-wal` as its own
seat; it attaches that identity to `<base>`'s `Entry`). Why it is sound:

- every WAL commit writes `-wal` (size or mtime moves), and every checkpoint
  writes the main file;
- a WAL reset after a checkpoint rewrites `-wal` from its start (mtime and
  ctime move even where the size does not);
- every rollback-mode commit writes the main file;
- `-shm` is **not** in the key: every reader, ours included, opens it
  read-write (Q36), so it would never be stable;
- the racy rule covers same-tick rewrites (5.4).

Limits, stated:

- A writer built with `SQLITE_MMAP_READWRITE` writes the main file through a
  shared mapping, where mtime moves only at writeback. The default build
  writes with `write(2)`. Out of scope, as for any file writer that maps.
- (Amended, R2.) A regular non-SQLite file that happens to have a
  `<name>-wal` sibling keeps its row key: the sibling's identity only adds a
  second lookup under the snapshot key, so its row never changes key and it
  is not read again when the sibling changes.
- Network-mounted sources keep the clock limit design.md already states.

### 5.3 Destination rows

The destination's reuse row key for a snapshot seat:

```
sqlite_key = postcard(authority, row, "bulkload sqlite snapshot seat v1", wal: Option<SidecarId>)
```

- (Amended, R2 and R8.) The key is **never** a `row_key`: it has bytes
  after the row, so a reuse row under it says the output is a snapshot, a
  raw file's row never answers for a snapshot or the reverse, and the two
  modes' records never overwrite each other. A file that is not a database
  keeps its `row_key`, so no existing destination row changes key at the
  upgrade (no #125-style re-read of the estate).
- `path_prefix` and `owner_key` are unchanged: both read only the first two
  postcard fields, so ownership rows, the superseding publish and the #187
  remembered refusals work as they do for files.
- The row is recorded under the **settled** identity (5.4), which the `End`
  carries, not the walk's.

### 5.4 Settled identity and the racy rule

The snapshot may run long after the walk, and a WAL-aware open of a closed
WAL database creates an empty `-wal` (Q72). So the source:

1. stats the main file and `-wal` immediately before the backup (`pre`);
2. runs the backup;
3. stats both again after the source connection is closed (`post`).

The capture is **settled** when `post.main == pre.main == row`, and
`post.wal == pre.wal` or `pre.wal` was absent and `post.wal` is a 0-byte
regular file (Q72's case), and neither identity is racy under the existing
2 s allowance (`git_carry::racy`, #86). A settled capture's `End` carries
`post.wal`, and the destination records a reuse row under
`sqlite_key(row, post.wal)`. The next walk sees that `-wal`, so the next run
answers `Reuse`. Without this, the Q72 file would make the second run of a
closed WAL store re-snapshot it once.

A capture that is **not settled** is still consistent (the backup API
guarantees that, not the stat identity), so it is sent and published with an
ownership row and no reuse row, exactly as a racy capture is (#86,
OI-1003-Q101). The next run snapshots it again. A store under constant
writes therefore re-snapshots every run. S3 allows that: it is a changed seat.

This replaces today's `SOURCE_CHANGED_AFTER_SNAPSHOT` for SQLite seats. A
live store whose identity moves between walk and capture is the normal case,
and refusing it would leave every busy store uncarried (S5).

### 5.5 Source-side records

- **No capture row** in the source ledger for a snapshot seat. A ledger
  manifest would let a later run answer `WantManifest` without reading, but
  the chunks it names lived in a snapshot that is gone, and the live file at
  those offsets holds different bytes. Every content request for a snapshot
  seat takes a new snapshot. R25 is carried by the destination's rows, as
  OI-1003-Q40 reads it.
- **Remembered refusals** (amended, R8 and R10). Tags: 1 `SqliteHeader`
  (database or WAL magic, written by `refuse` mode exactly as v5), 2
  `SqliteSnapshotIntegrity`, 3 `SqliteWalHeader`, 4 `SqliteSnapshotCorrupt`
  (primary code 11), 5 `SqliteSnapshotNotADatabase` (primary code 26). 2, 4
  and 5 are written under the snapshot key for a settled capture only, from
  `RefusedSeat::of_snapshot_refusal`, which masks the extended code. An
  unchanged corrupt store is then refused from the record, with 0 bytes and
  no open. Busy, budget, root, owner, IO and hot-journal refusals are never
  remembered.
- `snapshot` mode ignores tag 1 (it does not say which magic was seen: the
  seat is sniffed once more) and honours 3 under the row key and 2, 4, 5
  under the snapshot key; `refuse` mode honours 1 and 3 and never computes
  a snapshot key. No schema change: `kind` is already an integer column.

### 5.6 The snapshot slot

(Amended.) Snapshots are written to `<SOURCE_STATE>/sqlite-snapshots/`,
mode 0700, each under a name of the process and a serial. A slot lives until
the end of its entry's content: its streamed `End`, or the chunk requests
after its manifest (a file-backed `Retained`), or a refusal. It never waits
for `Held`, so no capture thread ever waits on the destination. `serve`
removes every leftover file in the directory at start: no record depends on
one.

Bounds: at most **one backup at a time** per `serve` (a mutex), and at most
4 GiB of slots alive at once, streamed or retained (the salvage bound's
figure; unruled engineering default). (Amended, review round 2, finding 7.)
Every slot reserves its main file's and `-wal`'s size before its backup,
outside the backup lock, waiting up to 30 s for another slot to go; the
reservation is trued up to the snapshot's real size after the backup and
given back when the slot is removed. Before the backup the slot
directory's filesystem must keep the `--min-free-percent` floor with the
slot written. A store whose main file and `-wal` together exceed the bound,
or that finds no room in time, or would breach the floor, is refused
`BUDGET_EXCEEDED` (not remembered). A database seat's manifest is never
bounded by the 512 MiB memory budget: its slot keeps its chunks (finding
5).

## 6. Per-seat flow (amended)

```
source (serve, snapshot mode)                         destination (pull/copy)
--------------------------------------                ------------------------------------
walk: Entry{row, wal: Some(id)|None};       ───────▶  reuse row under sqlite_key(row, wal) or row_key?
      SqliteSidecar{path, base}             ───────▶    yes: Decide Reuse (0 bytes, no open, no lock)
                                                      record (#169) under either key?  adopt, Reuse
                                                      <path>-wal/-journal/-shm here?  Refuse OCCUPIED
                                                        (nothing read on the source; not remembered)
                                            ◀───────  Send | WantManifest, admitted at size + wal.size
capture job:
  remembered under the row key (tag 3) or the snapshot key (2, 4, 5)?  → Refused (0 bytes)
  open, sniff 16 bytes (source_sniff_bytes)
    not database magic → today's path (a moved seat refused as before)
    database magic:
      st_uid != euid → SQLITE_SOURCE_NOT_OWNER (nothing opened by SQLite)
      one backup at a time; pre-stat main, -wal beneath the seat's directory
      stepped backup into the slot (provider_sqlite::snapshot_for_carry),
        Footprint counts Q36/Q72, journal_mode=DELETE, quick_check, FK count
      post-stat; walked inode still at the path?; settled? (5.4)
SqliteSnapshot{entry, size, post.wal}       ───────▶  key := sqlite_key(row, wal); size re-admitted
Data frames | Manifest+NeedChunks (the slot) ───────▶  verify every chunk digest, coverage, root
End{root, chunks, size, racy = !settled}    ───────▶  sidecars still absent?  integrity_check (immutable=1)
                                                      publish no-replace, or supersede own untouched
                                                        output by exchange (Q146; sidecar look first), record
                                                        and row under sqlite_key unless racy
                                            ◀───────  Held{held}
slot removed at the end of the entry's content
session end: each sidecar covered iff its base is held as a snapshot, else refused by name
```

`End` is unchanged: its `size` is the snapshot's, which `SqliteSnapshot`
announced before any content, so every size check (admission, chunk bound,
coverage) uses it; the walked row's size stays in the key.

## 7. S2: locks, writes, priority, root

### 7.1 Lock discipline (amended: D2, Q16 not amended)

Both journal modes use the stepped backup the `snapshot` verb uses: one
`step(128)` at a time, the read lock (rollback `SHARED`, or a WAL read mark)
taken and released inside each step, a zero busy timeout, a busy step
refused `SQLITE_STATE_CHANGED` at once. No read transaction is pinned across
steps; the pinned WAL mode this section first proposed is not built, and
its claim ("writers are never blocked") is withdrawn as stated (R3). What
the stepped backup does to the source's other users:

- a WAL-mode writer is never blocked (P80 asserts `busy=0`);
- a rollback-mode writer waits for at most one step's `SHARED`;
- a FULL, RESTART or TRUNCATE checkpoint waits for at most one step's read
  mark;
- a WAL-mode source connection holds `SHARED` on the main file from its open
  to its close, so an `EXCLUSIVE`-locking-mode opener is delayed by up to the
  connection's whole life, bounded by the step budget and the 30 s wall
  cap, and measured as `source_sqlite_lock_ns`;
- a writer that opens during the wal-index rebuild of a closed store may see
  `SQLITE_BUSY_RECOVERY` once.

A store whose writer commits between most steps restarts until its step
budget ends `BUDGET_EXCEEDED` (not remembered: the next run tries again),
counted in `source_sqlite_backup_restarts`. That is the S5 cost of D2; the
pinned mode, with R3's narrowed claim and a cap far below 120 s, is the
operator's ruling to make.

Priority inversion (D6): measured first; no IO-class change is built.

### 7.2 Bounds and counters

- Step budget: `max(4, ceil(page_count / 128) × 4)` steps (restarts
  included), and a wall-clock cap of 30 s per backup (amended from 120 s by
  R3; unruled engineering defaults). Past either: `BUDGET_EXCEEDED`, not
  remembered.
- One backup at a time per `serve` (5.6), so at most one source lock is
  held by bulkload at any moment.
- New counters, on every counters line:
  - `source_sqlite_snapshots`: backups completed;
  - `source_sqlite_backup_bytes` (amended, R13): pages stepped × page size,
    restarts included, plus the `-wal`'s size at the pre-stat (an upper bound
    on the wal-index rebuild), counted for a refused backup too. This **is**
    source content read and is added to `source_bytes_read`;
  - `source_sqlite_backup_restarts`, counted as each restart happens;
  - `source_sqlite_lock_ns` (amended, R13): the source connection's life,
    from open to close, for the transfer and the `snapshot` verb alike;
  - `source_sqlite_unsettled`: captures sent as racy;
  - `read_source_snapshot_bytes`: the private-state read of the snapshot to
    chunk it, and later to serve `NeedChunks`. Not a source read;
  - `sqlite_sidecars_covered`;
  - `source_sqlite_fk_violations` (informational, D3);
  - destination: `dest_sqlite_verify_bytes`, `dest_sqlite_verified`.
- Q36's `source_wal_index_touched` and Q72's `source_wal_created` count
  exactly as for the verb: the same `Footprint` wraps the same open.

### 7.3 No new source write

The source writes are Q36's `-shm` and Q72's empty `-wal`, unchanged. The
snapshot slot is private state, not source. P75's assertions (main file
byte- and metadata-identical, an existing `-wal` byte-identical) apply to the
transfer path unchanged. P77's named residual (`RESIDUAL_WAL_OPEN_READ_WRITE`,
#157) applies unchanged too.

### 7.4 Never as root (Q76), never as another user (R4) (amended)

A `snapshot`-mode session with an effective uid of 0 is refused
`SQLITE_SOURCE_AS_ROOT` at `Open` (`transfer::read_open`), before the walk
and before any store is created: nothing on the source is opened. `copy`
runs `serve` as its source half, so the one check covers both verbs (R14).
`refuse` mode as root is unchanged.

A database seat whose owner is not the reader is refused
`SQLITE_SOURCE_NOT_OWNER` (a new code, R4) from the `fstat` of the
descriptor the sniff holds, before `SQLite` opens it: `SQLite` would create
the database's `-shm` (and, under Q72, an empty `-wal`) owned by the reader,
which the owner's own writer may then fail to open read-write. Not
remembered.

### 7.5 Opening beneath the root (amended, R9)

`serve` canonicalises the root before it opens the root descriptor, so the
path `SQLite` opens, `canonical_root/rel_path`, has no symlink above the
walked components, and `SQLITE_OPEN_NOFOLLOW` refuses a symlink at the leaf.
The source checks, beneath the root descriptor, that the walked inode is at
the path after the backup; a mismatch discards the snapshot and refuses
`PATH_ESCAPES_ROOT`.

Residual, restated: an intermediate directory swapped for a symlink and
swapped back within the backup is not seen. Its effect is to read a
different database the same user can read **and to let `SQLite` create its
Q36 `-shm` and Q72 `-wal` beside that database, outside the root**. That
needs a hostile local writer in the source tree. Stated for the adversarial
review.

## 8. Sidecars (amended, R2 and R5)

### 8.1 On the source

In `snapshot` mode the walk classifies a regular `-wal`, `-shm` or
`-journal` whose base is a regular file in the same directory listing
(`Walker::with_sqlite`; the base sorts before its sidecars and is stat'ed
again for its kind). Such a sidecar is sent as `SqliteSidecar{rel_path,
database}`: never carried, never an entry, costing no read. Its outcome is
bound to its base's actual outcome, resolved when the session ends
(`transfer::resolve_sidecars`): covered when the base was published from a
snapshot, or reused or adopted as one (`sqlite_sidecars_covered`), and
refused by name, `SQLITE_STATE_CHANGED`, otherwise. So the `-wal` of a store
without the magic (SQLCipher) carried raw, and a user's `notes-journal`
beside a plain `notes`, stay visible refusals, as in v5. An orphan sidecar
is offered and refused by name, as today.

The live `-wal`'s content reaches the destination inside the snapshot: the
backup reads committed WAL frames through the WAL-aware connection. A
`-journal` is either a writer's in-flight transaction (the snapshot sees the
committed state under its lock) or hot (the read-only open cannot roll it
back and fails `SQLITE_BACKUP_FAILED`, not remembered).

### 8.2 On the destination

A snapshot is published with **no** sidecar beside it. The destination
`lstat`s `<path>-wal`, `<path>-journal` and `<path>-shm` at **Decide**,
before `Send` or `WantManifest`, for every regular entry in `snapshot` mode
(R5): any present refuses the entry `DESTINATION_OCCUPIED` with nothing read
or snapshotted on the source. The check reads only the destination, so it is
**not remembered**: a record would save no source byte, and removing the
sidecar converges on the next run. It runs again just before the publish;
a sidecar appearing after that is the stated residual. (It also refuses a
non-database file whose path has such a neighbour here: a visible
over-refusal.)

## 9. Placement and verification on the destination

- **Where:** at the seat's path under DEST, as a regular file with the
  source's mode bits, exactly where a file seat would land. DEST is a carry
  corpus (as in every lab run); installing into a live provider directory
  stays the job of `compose-state` and `apply-state-candidate`.
- **Form:** journal mode DELETE, as the `snapshot` verb already produces
  ("A WAL source must produce one portable database, without relying on
  destination sidecars"). Restoring the source's WAL header bit is open
  question 4.
- **Verification**, before seal, on the staged temporary, opened
  `SQLITE_OPEN_READ_ONLY` with `immutable=1` (the destination's own private
  file, so no `-shm` or `-wal` is created, and Q76's `fchown` concern does
  not arise):
  1. every chunk digest, coverage and `manifest_root` (unchanged);
  2. the header magic, and `page_size × page_count == End.size`;
  3. the journal mode is not WAL;
  4. `PRAGMA integrity_check` returns exactly `ok`.

  A failure refuses `SQLITE_INTEGRITY_CHECK_FAILED` (refusal site
  `transfer::verify_sqlite`), removes the temporary, publishes nothing and
  records no row. The source runs the same `integrity_check` first (review
  round 2, finding 4), so a store that fails it is refused and remembered
  at the source; the destination's check is defence in depth, and catches a
  peer that sends a non-database as a snapshot.
- **Supersede (Q146, 2026-10-09; replaces the D7 text below).** A changed
  store's snapshot supersedes its own landed, untouched output through
  WP0(d)'s exchange when no `-wal`, `-journal` or `-shm` sits beside it at
  Decide and at the last look immediately before the exchange; a touched
  or foreign output, or a sidecar, refuses `DESTINATION_OCCUPIED` and
  leaves the old output as it was (section 0.4). The D7 text, kept for the
  record: **Never supersede (amended, R6, D7).** A snapshot output is never
  replaced in place: a changed store whose path holds other bytes, this
  store's own output or not, is refused `DESTINATION_OCCUPIED` and kept
  (`Plan::Occupied`), remembered under the snapshot key when settled. The
  reason: an application holding an idle rollback-mode connection on the
  old file does not show in its identity, and SQLite names its journal by
  path, so that connection's next write transaction would create
  `<path>-journal` beside the new file with the old inode's pages; a crash
  then rolls them into the new snapshot (howtocorrupt §2.4): corruption,
  not a stale file. A changed store is carried again once its old output is
  moved aside. Ruling the alternative (supersede after an `F_OFD_GETLK`
  probe of SQLite's lock bytes, with the idle-connection hole stated) is the
  operator's. (Correction, 2026-10-09: the idle-connection hole holds only
  for SQLite before 3.8.3 or a VFS without the moved-file check; a modern
  unix-VFS connection gets `SQLITE_READONLY_DBMOVED` instead, section 0.4.)
- **Foreign-key violations** are carried, not refused (open question 3).
  The `snapshot` verb refuses them because composition relies on them; a
  transport must carry the application's state as it is. The source records
  the count (`source_sqlite_fk_violations`, informational).

## 10. Wire v6 (amended, R15, D8)

A hard cut, as every wire change is (R-N59, R-N118): `PROTO_VERSION = 6`, a
new `WIRE_SCHEMA` text and so a new `wire_id`
(`a6e9842a...0203a5d0` since round 4 of the Q146 review, which added
`Refused.sqlite_code`; `75c2c429...9ef94b4f` before). A v5 peer is refused `FRAME_CODEC` at `Open`, before
anything else, on either side (a v5 `Open` has no `sqlite` field and does
not decode). Both hosts' binaries move together; `pull`'s
`REMOTE_EXECUTABLE` argument already names the remote binary per run, and
`pull` passes no flag: the mode travels in `Open`.

Changes against the v5 schema text:

```
-bulkload wire v5 (2026-10-02)
+bulkload wire v6 (2026-10-08)
-control 0 Open{proto u16, wire_id [u8;32], root bytes, state bytes}
+control 0 Open{proto u16, wire_id [u8;32], root bytes, state bytes, sqlite SqliteMode}
-control 2 Entry{entry u64, row RowSchema}
+control 2 Entry{entry u64, row RowSchema, wal Option<SidecarId>}
-control 3 Refused{entry Option<u64>, rel_path bytes, code string}
+control 3 Refused{entry Option<u64>, rel_path bytes, code string, sqlite_code Option<i32>}
+control 21 SqliteSidecar{rel_path bytes, database bytes}
+control 22 SqliteSnapshot{entry u64, size u64, wal Option<SidecarId>}
+sqlitemode 0 Refuse, 1 Snapshot
+sidecarid {dev u64, ino u64, size u64, mtime_ns i128, ctime_ns i128}
```

- `End` is **not** changed (R15): no seal, no page size or count (the
  destination reads the header), no `source_wal_mode` (D4 ruled DELETE).
  `SqliteSnapshot` precedes an entry's first content and carries the size
  every check needs before content arrives (R7) and the settled `-wal`;
  `End.racy` carries "not settled".
- `RowSchema` is not changed: it is the postcard input of every row key.
- `Refused.sqlite_code` (added 2026-10-09, review round 4): `SQLite`'s
  extended result code of a `SQLITE_BACKUP_FAILED` refusal, `None` for
  every other code (a peer that attaches one to another code is refused
  `PROTOCOL_STATE_VIOLATION`). The receiver reads backup-detected
  corruption from it (primary 11 or 26) to print Q148's `abandon` default.
- `Entry.wal` is `Some` only in `snapshot` mode; `SqliteSidecar` and
  `SqliteSnapshot` only in `snapshot` mode. A destination that receives any
  of them in `refuse` mode refuses `PROTOCOL_STATE_VIOLATION`.
- `Decision`, `Held`, data frames, credit and the W6 reserved frames are
  unchanged (D8: WP3 cuts v7).
- **Stores.** No schema change: `refused_seats.kind` gains tags 2 to 5, and
  snapshot keys are longer `outputs` keys. An older binary that opens a
  newer store reads an unknown tag as a miss and sniffs again.

## 11. Refusal codes

One new code (amended, R4): `SQLITE_SOURCE_NOT_OWNER`. The closed set a
snapshot seat can end in:

| Code | When | Remembered | Suggested disposition |
|---|---|---|---|
| `SQLITE_INTEGRITY_CHECK_FAILED` | Source or destination `integrity_check` fails | yes (source side, settled capture) | **abandon** by default (OI-1003-Q148; printed as `default-disposition`) |
| `SQLITE_BACKUP_FAILED(11 or 26)` | Backup open or step reports corrupt or not-a-database | yes | as above |
| `SQLITE_BACKUP_FAILED(other)` | Other SQLite errors, including `SQLITE_READONLY_ROLLBACK` (hot journal) | no | re-carry |
| `SQLITE_STATE_CHANGED` | A rollback-mode step met a writer's lock (busy timeout 0); an orphan sidecar; a stray WAL-magic file; every database in `refuse` mode | busy: no; header: as today | re-carry, or accept for orphans |
| `BUDGET_EXCEEDED` | Step budget, wall-clock cap, or the slot bound | no | re-carry |
| `SQLITE_SOURCE_AS_ROOT` | `serve` (or `copy`) in `snapshot` mode as uid 0: the session | no | re-run as the owner |
| `SQLITE_SOURCE_NOT_OWNER` (new, R4) | A database owned by another user than the reader, before `SQLite` opens it | no | re-run as the owner, or accept |
| `PATH_ESCAPES_ROOT` | The resolved database is not the walked inode (7.5) | no | re-carry |
| `DESTINATION_OCCUPIED` | A foreign file, or this store's older snapshot touched since it landed (Q146; an untouched one is superseded): remembered (#187 record) when settled. A sidecar beside the path: at Decide or at the exchange's last look, not remembered (R5, Q146) | file: yes; sidecar: no | move the file or the sidecar aside |

`SQLITE_STATE_CHANGED`'s doc text ("Shared rows diverged between planning and
apply") is already wider than its uses; the PR updates it to name the three.

S4 note: transfer refusals are not yet in the closure ledger; that is WP3
PR 4 (slo.md, S4 proof status). Until then a snapshot seat's refusals are
typed on the refusal lines and counted; the disposition ledger covers them
once WP3 PR 4 lands. Dependency, not a blocker for the code.

## 12. Interaction with `SQLITE_STATE_CHANGED` and #186

- `refuse` mode (amended, R8): v5's decision for every seat. #186's sniff,
  its remembered tag-1 record (written exactly as v5 writes it) and the
  name-based sidecar refusal all stay; no raw `SQLite` byte crosses the
  wire. An output a `snapshot`-mode run published is not reused in `refuse`
  mode (its key is not a row key): it is refused at the source by its sniff,
  and from the record on later runs. A `refuse`-mode run also honours a
  tag-3 record. P21 and P23 with refused seats stay green unchanged.
- `snapshot` mode: database magic stops being a refusal and becomes the
  trigger for the snapshot path. WAL magic, orphan sidecars and rollback
  busy stay `SQLITE_STATE_CHANGED`. Raw `-wal`, `-shm` and `-journal` are
  never carried in either mode (G2).
- The sniff is still read only after the destination asked for content, so
  a warm rerun sniffs nothing.

## 13. S3 accounting

- **Unchanged store:** `Reuse` from the destination's row. 0 content bytes,
  0 sniff bytes, 0 backups, no open, no lock, and so `source_wal_index_touched`
  and `source_wal_created` add 0. This is better for S2 than the verb, which
  touches the `-shm` on every run.
- **Changed store, WAL mode** (amended, R13): one backup, so
  `source_sqlite_backup_bytes` is `page_count × page_size` of the committed
  database plus the `-wal`'s size (the wal-index rebuild's bound): at most
  `size(main) + 2 × size(-wal)`. Inequality 1 holds with that size; it
  needs a line in the S3 amendment.
- **Changed store, rollback mode:** `(restarts + 1) × size`, restarts bounded
  by the step budget. Inequality 1 holds only with that factor. Stated, not
  hidden: it needs a line in the S3 amendment.
- **Wire:** `WantManifest` against chunk hints, so only chunks absent at
  the destination cross (inequality 2). Under Q146 a changed store's own
  older output is superseded, and its chunks fill the new snapshot as for
  any superseded file.
- **neo's corpus:** the first native run reads every store once (about the
  workaround's cost, without the wrap). A warm rerun reads only stores whose
  identity moved: the live Codex and Claude stores, not the static
  `db-backups` copies (their share of the 2,724 was not counted here). The workaround's corpus on sting has no rows, so the
  native run targets a fresh DEST (or its paths are refused
  `DESTINATION_OCCUPIED`).

## 14. File and function plan (amended: as built, uncommitted)

| File | Change |
|---|---|
| `crates/bulkload-proto/src/frame.rs` | `PROTO_VERSION = 6`; `WIRE_SCHEMA` v6; `SqliteMode`, `SidecarId`; `Open.sqlite`, `Entry.wal`, `Control::SqliteSidecar` (21), `Control::SqliteSnapshot` (22) |
| `crates/bulkload-proto/src/frame/tests.rs` | Round trips of the new fields and variants (trailing-byte refusal for each, by the existing loop); `wire_id` pin moved; a v5 `Open` refused; `sqlitemode` indices |
| `crates/bulkload-proto/src/refusal.rs` | `SQLITE_SOURCE_NOT_OWNER` (R4); `SqliteStateChanged`'s doc text names its uses |
| `crates/bulkload-agent/src/provider_sqlite.rs` | `copy_source` on a shared stepped `backup_into`/`step_all` (restarts counted as they happen, `BackupStats` returned refused or not, `source_sqlite_lock_ns` for the verb too); `snapshot_for_carry` (`NOFOLLOW`, `Footprint`, DELETE, `quick_check`, FK count, `CARRY_WALL` 30 s); `verify_received` (header, size, not WAL, `integrity_check`, `immutable=1`) |
| `crates/bulkload-agent/src/walk.rs` | `Walker::with_sqlite`: `WalkItem::RowWithWal` and `WalkItem::SqliteSidecar`; a level keeps its whole sorted listing |
| `crates/bulkload-agent/src/transfer.rs` | `copy_with`, `receive_with`, `--sqlite` default; `read_open` (root refusal), `open_slots`; `SourceWork` (mode, euid, backup mutex, slots, slot budget); `capture_file` returns `Captured`; `snapshot_seat`, `take_snapshot`, `chunk_slot`, `slot_chunk`, `WalStat`, `wal_settled`; mode-aware `remembered_refusal`; destination `Seat`, `sqlite_snapshot`, Decide-time sidecar check, `verify_snapshot`, `Plan::Occupied`, `resolve_sidecars`; `TransferStats.sqlite_*` |
| `crates/bulkload-agent/src/transfer/unrowed.rs` | `sqlite_record_key` (R1); `prove` compares a record against both keys; a snapshot's size is its record's |
| `crates/bulkload-agent/src/transfer_store.rs` | `sqlite_key`; `RefusedSeat` tags 2 to 5 and `of_snapshot_refusal` (R8, R10) |
| `crates/bulkload-agent/src/materialize.rs` | `Destination::sqlite_sidecars_present`, `Destination::staged_path`; (Q146) `sqlite_sidecar_beside`, `StagedFile::mark_sqlite` and `sqlite_sidecar_appeared`, the look in `StagedFile::publish` and `StagedFile::exchange`, `dest_sqlite_superseded` |
| `crates/bulkload-agent/tests/fault_harness.rs`, `tests/fault_harness/sqlite_supersede.rs` | (Q146) eight `sqlite_supersede*`/`sqlite_superseding*` crash rows over the exchange's points; the crash child copies in `snapshot` mode when `BULKLOAD_W7_CHILD_SQLITE` is set |
| `crates/bulkload-agent/src/counters.rs` | `source_sqlite_snapshots`, `_backup_bytes`, `_backup_restarts`, `_lock_ns`, `_unsettled`, `_fk_violations`, `read_source_snapshot_bytes`, `sqlite_sidecars_covered`, `dest_sqlite_verified`, `dest_sqlite_verify_bytes` |
| `crates/bulkload-agent/src/main.rs` | Global `--sqlite=refuse|snapshot`; `sqlite_mode=`, `sqlite_snapshots=`, `sqlite_sidecars_covered=` on the transfer line; USAGE |
| `crates/bulkload-agent/src/fault.rs` | **Not built**: no new fault point (the harness's fixtures would need a `SQLite` fixture first; section 15) |
| `docs/design.md` | "Wire v6"; "Refused seats", "SQLite snapshot seats"; the S2 lock now counted; the transfer's root and owner refusals |
| `docs/slo.md` | **Not changed** (section 0.3 lists what needs a ruling) |
| `docs/plans/2026-10-03-property-test-plan.md` | Rows P80 to P83 |
| `docs/formal/` (Q146: moved from `docs/formal/sqlite/`) | `SqliteCarry.tla`; `catalogue/SqliteCarry.dhall` renders `configs_sq.tsv` and 25 `MC_sq_*.cfg`; README section "SqliteCarry" (section 16) |

## 15. Test plan

All properties go through `test_support::prop_config` (fixed seed, PR-gate
counts, `BULKLOAD_PROPTEST_DEEP=1` for twenty times). No fuzzing
(OI-1003-Q7). The live writer is a child process (the test binary re-run, as
P75 does), so its locks are its own. P-numbers P80 to P83 are the next free
ones on `origin/main` `bdb8994` and in the two open PRs (#210, #221); they
need allocating when the PR lands (open question 9).

### P80 SQLITE-CARRY-CONSISTENT (G1, G3; S2)

For all generated stores (journal mode WAL or DELETE; page size 1024 or
4096; 1 to 40 committed rows), all P75 shapes, and a writer that commits a
transaction every 2 to 20 ms during the carry (each transaction inserts row
`k` and moves an amount between two accounts, so the sum is invariant), a
`copy --sqlite=snapshot` and a `serve`/`receive` pair over pipes (how `pull`
runs, without ssh) publish a file where:

- `integrity_check` is `ok`, the journal mode is DELETE, and there is no
  `-wal`, `-shm` or `-journal` beside it;
- its rows are exactly `1..=k` for some `k` the writer committed, and the
  sum is invariant: one transaction's state, never torn;
- on the source, the main file keeps its bytes and its whole identity; an
  existing `-wal` keeps its bytes; only P75's footprint occurs, and both
  counters rise as P75's oracle says;
- the writer's commits all succeeded with a busy timeout of 0 in WAL mode
  (never held up). In DELETE mode the commits are counted, not asserted.

Pinned rows: a 2,000-page WAL store under a writer every 2 ms (the restart
case today's stepped backup would lose: `source_sqlite_backup_restarts = 0`
and pages copied equals `page_count`, the PR 2 check of 7.1); a closed WAL
store with no `-wal`; a DELETE store with a hot journal (refused
`SQLITE_BACKUP_FAILED`, not remembered).

### P81 SQLITE-CARRY-RESUME (G5; S3, R25)

For all generated stores and change scripts:

- a rerun with nothing changed reads 0 content bytes, 0 sniff bytes, takes
  0 backups, holds the lock 0 ns, and both Q36/Q72 counters add 0;
- a closed WAL store's second run reuses it (the Q72 settled identity), not
  only its third;
- after `n` commits to one store of many, the rerun snapshots exactly that
  store, and wire content bytes are at most the chunks absent at the
  destination;
- a racy or unsettled capture publishes with an ownership row and no reuse
  row; the next run snapshots it again and supersedes it;
- `refuse` mode across the same scripts behaves as v5 (#186's rows).

### P82 SQLITE-CARRY-REFUSALS (G6; S4, Q76)

- For all corruption kinds (a truncated file, a garbled interior page, a
  garbled header past the magic, a freelist loop): the outcome is one of
  `SQLITE_INTEGRITY_CHECK_FAILED` or `SQLITE_BACKUP_FAILED(11|26)`, never
  `IO` or `FRAME_CODEC`; nothing is published; the slot is removed; an
  unchanged rerun refuses from the record with 0 sniff bytes and no open.
- A remembered `SqliteHeader` from a `refuse`-mode run does not stop a
  `snapshot`-mode run, and is honoured again by a later `refuse`-mode run.
- **As root:** `serve` in `snapshot` mode refuses the session
  `SQLITE_SOURCE_AS_ROOT`; the source directory is byte- and
  metadata-identical (no `-shm`, no `-wal`, no ctime moved), both counters
  0; then the whole property runs again in a child dropped to uid and gid
  65534 (P75's pattern), and says so on stderr when the drop cannot be made.
- A destination-side check: a fault-injection `serve` that sends a raw,
  non-database file with a snapshot seal is refused
  `SQLITE_INTEGRITY_CHECK_FAILED` at the destination.

### P83 SQLITE-CARRY-SIDECARS (G2)

Exhaustive over source layouts {base present, absent} × {`-wal`, `-shm`,
`-journal`, none} and destination layouts {nothing, a stale `-wal`, a stale
hot `-journal`, a `-shm`, an existing own output, a foreign file}:

- no sidecar is ever an `Entry` with content, and no raw sidecar byte is
  ever on the wire (asserted on the frame trace);
- a sidecar with a base is reported `SqliteSidecar`, an orphan is refused
  by name;
- a snapshot is published only where the destination had no sidecar;
  otherwise `DESTINATION_OCCUPIED`, the destination byte-identical, and a
  rerun refuses from the record with 0 source bytes.

### End to end (#218's acceptance)

`tests/sqlite_carry.rs`: a 50 MB WAL store with a writer committing every
5 ms throughout; `serve`/`receive` over pipes in `snapshot` mode. Check P80's
consistency on the result; stop the writer; rerun and check the new state
arrived; rerun again and check 0 bytes, 0 backups, 0 locks. A second case
runs the CLI `copy --sqlite=snapshot` and reads the counters line. A field
confirmation (not CI): `pull --sqlite=snapshot` neo→sting of the 2,724 stores
into a fresh DEST, then a warm rerun, both recorded in `docs/evidence/`.

### Existing suites extended

- **P75** gains a transfer leg: the same shapes through `capture_sqlite`, the
  same oracle.
- **P77** gains a `serve --sqlite=snapshot` leg (Linux): only the
  Q16/Q36/Q72 locks; in WAL mode the read mark is held for the whole backup
  and a committing writer is never held (`busy=0`); as root, the session
  refusal with no lock and no write-mode open.
- **Fault harness:** rows at the three new points; every rerun converges,
  the slot directory is empty after the next `serve` start, no row names
  bytes that are not durable.
- **`refusal_taxonomy`:** no new code; the new constructor sites are listed.
- **S2 gated run (Q34):** one window with `--sqlite=snapshot` and the
  reference workload's own SQLite store inside the carried root, so the
  budget is measured while bulkload holds a read mark on the store the
  workload writes.

### Mutants that must each fail

Every mutant fails a named property: key on the main file alone (P81, a WAL
commit without a checkpoint is reused stale); record a reuse row for an
unsettled capture (P81); key the capture record on the main file alone
(P81's R1 row); honour `SqliteHeader` in `snapshot` mode (P82); skip the
destination sidecar check (P83); send the live main file instead of the slot
file (P80's R12 row: the hook's commit would be carried); drop the root
check (P82's root leg); drop the owner check (P82's R4 leg); carry a sidecar
(P83's marker scan); cover a sidecar by name (P83's raw-base rows).

### As built (amended)

Built: P80 to P83 as `transfer/tests/sqlite_carry.rs` (20 tests, fixed
seeds, the writer a child process), the end-to-end CLI test
`tests/sqlite_carry.rs` (with its root and dropped legs), the frame and
store unit tests above. Not built, and open: the fault-harness rows (a
`SQLite` fixture in `tests/fault_harness.rs`); P75's transfer leg as its own
row (the CLI test asserts P75's footprint on the transfer path); P77's
`serve --sqlite=snapshot` lock-trace leg; the S2 gated window (Q34); the
neo→sting field confirmation. The mutants above were reasoned against the
tests, not run as scratch mutants.

## 16. What the formal model must add (amended: built as `docs/formal/sqlite/SqliteCarry.tla`)

**Moved and folded in (Q146, 2026-10-09).** The module is now
`docs/formal/SqliteCarry.tla`, a third module of the Dhall catalogue
(`catalogue/SqliteCarry.dhall`, `configs_sq.tsv`, `MC_sq_*.cfg`), run by
`just tla-check`; D7's `SqliteNeverSupersede` is replaced by
`SqliteNoClobber`, `SqliteOldOrNewWhole` and the exchange's sidecar look
under `SqliteNoForeignSidecar` (docs/formal/README.md, "SqliteCarry").

**As built (R11).** A separate module, `docs/formal/sqlite/SqliteCarry.tla`,
with its README and 21 hand-written configs, all of whose outcomes match
(6 pass, 2 reach, 13 mutations each failing its named property). It walks
the main file and the `-wal` in two steps, defines `SqliteReuseSound`
against `lv` at the walk, bounds backups by runs (at least 2) with one
connection at a time, has a checkpointer and the unrowed adoption with the
main-only record mutation, and the root, owner, sidecar, supersede and
header rules. It is **not yet in the Dhall catalogue** (`just tla-check`
does not run it); folding it in, as a third `Module`, is open before PR 3
merges. The text below is the plan it was built from.


`BulkloadTransfer.tla` models the SQLite backup today only as an estate read
(`EstateReads`, `BackupBegin` to `BackupEnd`, one backup, a lock held inside
one step). Per OI-1003-Q102's precedent (model first, then merge), PR 1 adds:

- **State.** A seat kind (`file` or `sqlite`). For a `sqlite` seat: a
  logical version `lv`, the main and `-wal` identities, the journal mode,
  and a history of committed versions. The destination output of a sqlite
  seat holds a version, or `torn` (a value only a raw read can produce).
- **Environment.** `SqlCommitWal` (lv and wal identity move),
  `SqlCheckpoint` (main identity moves, lv does not), `SqlWalReset`,
  `SqlCommitRollback` (lv and main identity move; disabled while a rollback
  step holds `SHARED`).
- **Actions.** `SnapBegin`, `SnapStep`, `SnapRestart` (rollback mode, a
  commit between steps), `SnapEnd` (settled or not), carried through the
  existing `RecvEnd`, `Publish`, `Commit` and `Held`; the destination's
  `VerifySqlite`; `SidecarAppear` at the destination (a third party).
- **Invariants.**
  - `SqliteNeverTorn`: a published sqlite output's version is in the
    seat's history.
  - `SqliteNeverRaw`: no content read of a sqlite seat or a sidecar beyond
    the sniff.
  - `SqliteReuseSound`: `Reuse` of a sqlite seat only when the output's
    version is the seat's current `lv` (the key's soundness, with racy and
    settled).
  - `WalWriterNeverBlocked`: in WAL mode no source lock disables
    `SqlCommitWal`.
  - `S2_BackupLockBounded`, amended: shared only, at most one backup, held
    for one step in rollback mode or one pinned backup in WAL mode, steps
    bounded.
  - `SqliteRootRefusedUpFront`: as root in `snapshot` mode, no source
    operation at all.
  - `SqliteNoForeignSidecar`: a sqlite output is published only where the
    destination had no sidecar.
  - `S3_UnchangedSqliteZero`: an unchanged, held sqlite seat takes 0
    backups.
  - `R25_NoDurableReread` and `R25_StrictNoDurableReread` with sqlite seats
    present, and every #187 invariant (`NoClobber`, `SupersedeAtomic`,
    `OwnershipNeverReuse`, `RememberedRefusalSound`).
- **Mutations**, each failing its one named property: `sqlite_raw_send`
  (`SqliteNeverTorn`), `sqlite_key_main_only` and `sqlite_record_unsettled`
  (`SqliteReuseSound`), `wal_lock_exclusive` (`WalWriterNeverBlocked`),
  `sqlite_unbounded_pinned` (`S2_BackupLockBounded`), `sqlite_as_root`
  (`SqliteRootRefusedUpFront`), `sqlite_publish_beside_sidecar`
  (`SqliteNoForeignSidecar`), `sqlite_header_refusal_in_snapshot_mode`
  (`ClosureAccounted`: the store never closes as applied).
- **Bounds.** One sqlite seat and one file seat, two commits, one
  checkpoint, `MaxBackupSteps = 2`, the existing crash actions.
- **Explorer.** The Haskell explorer's domain does not include these, as for
  #187's records: pinned outside it, and its counts of record do not move.
- **Not modelled**, stated in the README: SQLite's own internals (that the
  backup of a pinned transaction is one version is an axiom, checked by P80,
  not proved); the 7.5 path race; clocks.

## 17. Order of work

(Amended.) All of PRs 1 to 3 is built in this worktree, uncommitted, as one
change set; it can still be split along these lines for review. PR 2 has no
`backup_pinned` (D2). PR 4 is open.

1. **PR 1, model.** Section 16 in `BulkloadTransfer.tla`, the catalogue and
   the README. No code. Merged before PR 3 (Q102's precedent).
2. **PR 2, provider.** `backup_pinned` and `backup_stepped`,
   `snapshot_for_carry`, `NOFOLLOW` and the inode check, the lock counter
   for the verb too. P75 extended; the pinned-restart property. No wire
   change, so it can land on v5.
3. **PR 3, the seat.** Wire v6, walk classification, `capture_sqlite`, the
   slot, destination verification and sidecar check, the CLI flag (default
   `refuse`), P80 to P83, the end-to-end test, fault rows, P77's leg, docs.
4. **PR 4, rulings and field.** The slo.md amendment once ruled; the S2
   gated window; the neo→sting field confirmation and its evidence; a
   comment on #218.

## 18. Open questions for the operator

Each has a conservative answer built (section 0.2); each still needs the
operator's ruling.

1. **(Default ruled: OI-1003-Q147, `refuse` stays.)** **Ratify (a)**, and the default: `refuse` until P80 to P83 and the field
   confirmation pass, then `snapshot`? Or `snapshot` from the start?
2. **Amend Q16 for WAL mode:** the backup holds one read transaction (a WAL
   read mark) for the whole bounded backup, not one step. Writers are never
   blocked; checkpoints wait. Rollback mode keeps per-step locking.
3. **Integrity bar for carry:** `quick_check` at the source and
   `integrity_check` at the destination; carry foreign-key violations (the
   verb refuses them) and count them?
4. **Destination journal mode:** DELETE, as the verb produces, or restore
   the source's WAL header bit so the file opens in the mode it had?
5. **As root:** refuse the whole `snapshot`-mode session at `Open`, or
   refuse each database and carry everything else?
6. **Rollback-mode priority inversion:** raise the IO class to best-effort
   for each step's span, or cut the step to 16 pages, or measure first?
7. **(Ruled: OI-1003-Q146, section 0.4.)** **Superseding a SQLite output** in the carry corpus: allowed under the
   ownership rule plus the sidecar check (residual: an idle rollback-mode
   connection on the destination), or never, so a changed store gets a new
   name?
8. **v6 and WP3:** take WP3's removal of the W6 reserved frames in the same
   cut, or keep them and let WP3 cut v7?
9. **P-numbers** P80 to P83, and engineering defaults: one backup at a time,
   4 GiB of retained snapshots, step budget `ceil(pages/128) × 4`, 120 s
   per backup.
10. **(Integrity failures ruled: OI-1003-Q148, default `abandon`.)** **The 35 corrupt rescue copies:** typed refusals with an `abandon` or
    `accept` disposition, or does the operator want a ruled exception that
    carries a quiescent corrupt copy raw as evidence? This design builds
    none.
11. **`-journal` naming:** `<base>-journal` is matched by name, as today;
    a non-SQLite pair such as `foo` and `foo-journal` keeps today's
    refusal of the second. Keep?

## 19. Session record (R-N13)

- **Done:** read AGENTS.md, slo.md (Q16, Q34, Q36, Q72, Q76 and the S3/S4
  amendments), design.md ("Live union", "Wire v5", "Refused seats"),
  `provider_sqlite.rs`, `transfer.rs` (`capture_file`, `run_job`,
  `manifest_capture`, `serve_chunks`), `transfer_store.rs` (`row_key`,
  `owner_key`, `RefusedSeat`), `frame.rs`, `refusal.rs`, the formal README's
  abstraction map and properties, P75 and P77, #218, and the neo run's
  scripts and counters. Wrote this design.
- **Produced:** this file only. No commit, no push, no PR, no Linear or
  GitHub comment (the lane was told not to). Nothing built or run.
- **Open:** every item in section 18; then PRs 1 to 4 of section 17. The
  distilled decision belongs on #218 and the Linear SSOT ledger once the
  operator rules.

Workstream line (design session): `sqlite-carry | this lane (design) |
bulkload feat/sqlite-carry-20261008 @ bdb8994, uncommitted | design
written, nothing ruled | needs the section 18 rulings | next: PR 1 (model)
after ruling`.

The implementation session's record is
[`2026-10-08-sqlite-carry-impl.md`](2026-10-08-sqlite-carry-impl.md).
