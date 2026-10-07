# Bulkload product contract

## Completion

The destination must support daily work: authenticated agents with history,
SSH, Git commits and pushes, PR operations, editors and persistent terminal
sessions. Counts, manifests and documentation merges alone do not establish
completion.

Coverage includes both hosts' unique Git and agent state, credentials, dots,
SQLite and worktrees. Unsupported items remain outstanding with preserved
custody; nothing is silently excluded.

Closure is bulkload's own gate, never a metric defined outside it
(OI-1001-Q2). `closure-report PLAN CORPUS SOURCE PRIVATE_STATE...` reads the
plan, the corpus's capture records and the apply ledger (outcome records and
`.done` journals). It ends every planned item as one of:

- `applied`: the item plans a workspace, and the exact journal for its
  current capture and SOURCE says `workspace-restored`.
- `refused-pending-review` (S4, 2026-10-06): a typed refusal code that no
  operator review disposes yet. The item is accounted for, but the run is
  not complete.
- `refused`: a typed refusal code that a review disposes (accept, re-carry
  or abandon; before 2026-10-06 this meant any typed refusal). A bare `IO`
  or `FRAME_CODEC` names no cause and does not close an item, and neither
  does a code that has since left the taxonomy (`refusal-code-retired`).

  Refusal codes stay typed at the source (WP3, 2026-10-03). Every taxonomy
  code is raised by product code, never kept as unused vocabulary. A Git
  child that exits non-zero refuses `GIT_CHILD_FAILED` with a
  `stderr_class=` from a closed set; its stderr is classified, never echoed
  or kept (R-N121). No blanket conversion turns an OS or codec error into a
  refusal: each site names itself with `.refuse_at(site)`. The sites that
  still raise a bare `IO` with no errno are an allowlist that only shrinks.
- `referenced-only`: the item plans no workspace, and the exact
  current-capture journal says `refs-imported`. `git-repair-missing-index`
  given `PLAN CORPUS PRIVATE_STATE` binds its bundle to the planned item
  whose current capture it is (by digest and repository) and writes the same
  journal, then the outcome `index-repaired` (or `refused` with its code), so
  a repaired item reads as referenced-only natively (#95). An item that plans
  a workspace never binds: the repair refuses `RECEIPT_BINDING_INVALID`
  before writing anything, and estate-apply restores the workspace (#132).

Anything else is `unaccounted`: a stale journal from an earlier capture, a
record naming another source, or refs only for an item that plans a
workspace. Stale and foreign journals are listed. The native `verdict`
passes only when `unaccounted` is 0. The report's `gate` is the exit
status, and a failing gate exits nonzero with `CLOSURE_UNACCOUNTED`. Since
S4 (WP3 PR 3, 2026-10-06) `gate` no longer equals `verdict` without an
attestation ledger: it also fails while any typed refusal is
`refused-pending-review`, with or without `--attest`. `CLOSURE_UNACCOUNTED`
therefore also covers refusals pending review, and can be raised while
`verdict` is `pass` and `unaccounted` is 0 (`totals.pending_review` says how
many). A pass is necessary for completion, not sufficient: the daily-work
bar above still applies.

`closure-report --attest LEDGER.json PLAN ...` (#95; the option is
recognised only before PLAN, #133) joins a `bulkload.closure-ledger.v1`
attestation ledger for items closed by audit rather than by a verb. The
ledger must name this plan (`plan`) and SOURCE label (`source_label`). A row
closes a planned item only when the native ledger leaves it unaccounted, and
only with a matching source, the item's current capture digest (`capture`,
as the native item row prints it; `null` only when the corpus holds no
capture record), a known disposition (`applied`, `refused` with
a typed code, `referenced-only`, `present`, `source-absent`), a basis other
than `native-closure-report`, and non-empty evidence. Rows are reported in a
separate `attested` block with their evidence; native totals are unchanged
and a native record is never overridden (rows for natively closed items are
counted as superseded, and disagreements listed). The top-level `verdict`
stays native; `gate` passes only when every item is native-accounted or
attested (#133). A ledger missing or naming another plan or SOURCE label,
or listing an item twice, refuses.

S4 (WP3 PR 3, 2026-10-06): outcome records are typed (`Outcome`,
`Refusal{code, site, errno}`, postcard with no bytes left over; legacy string
records are mapped), and a typed refusal is `refused-pending-review` until
`closure-dispose` records an accept, re-carry or abandon review for it;
`closure-report --dispositions LEDGER` joins those reviews, and `gate` then
also requires every typed refusal, native or attested, to be reviewed. A
bare `IO` can be neither reviewed nor attested away: whether an attestation
may close an item is decided from the item's decoded record, so a bare `IO`
recorded under another source (`record-source-mismatch`) is still refused
(`native-refusal-untyped`).

- **Item reviews are bound to the refusal instance.** `closure-dispose
  [--attest LEDGER.json] LEDGER PLAN CORPUS SOURCE ITEM CODE DECISION
  REVIEWER DATE PRIVATE_STATE...` reads the same inputs as `closure-report`
  and refuses (`RECEIPT_BINDING_INVALID`) unless ITEM holds a typed refusal
  with CODE now, so no review is written ahead of its refusal. The row
  records that refusal's `instance`: a digest of the item's current capture
  and its outcome record (source, code, site, errno, reason), printed on
  every item row. A later refusal of the same code for the same item against
  another capture or with another record is another instance; the old row
  does not dispose it and is listed under `reviewed.unmatched` as
  `refusal-instance-stale`. The record carries no time, so two refusals with
  the same capture and a byte-identical record are one instance.
- **Standing policies are open-ended.** `closure-dispose LEDGER PLAN CORPUS
  SOURCE --policy CODE ...` disposes every refusal with CODE under the bound
  plan and SOURCE label, now or later, for any item. It names no instance;
  the plan digest and the label are its only bounds.
- **The ledger is bound to the plan's bytes.** A disposition ledger records
  its plan's path and a blake3 digest of the plan file. Reading or appending
  refuses (`RECEIPT_BINDING_INVALID`) when the plan at that path now has
  other bytes, and `closure-dispose` reads PLAN for every row, so a ledger is
  never created for a path that holds no plan. Appending an item to the plan
  (`estate-add`) therefore starts a new ledger.
- **Retired codes fail closed per row.** Writers record only a code the
  taxonomy holds now. Readers accept any well-formed code token: an outcome
  record naming a code that has since left the taxonomy reads as
  `unaccounted` with `refusal-code-retired` (never as unreadable), no review
  or attestation closes it, and only a verb recording a current outcome
  does. A review row naming such a code disposes nothing and is listed under
  `reviewed.unmatched`; the rest of the ledger keeps working.

The report schema is `bulkload.closure.v2` from this change (it was
`bulkload.closure.v1`). What changed for a v1 reader:

- `disposition` can be `refused-pending-review`; `refused` means reviewed.
- `totals.refused` counts reviewed refusals only (0 without
  `--dispositions`); `totals.refused_pending_review` and
  `totals.pending_review` (native plus attested) are new. The `refusals`
  map still counts every typed refusal by code, reviewed or not.
- Each item row gains `instance`, and `review` when a review disposes it.
- `refusal` (with `site` and `errno`) is still printed for typed refusals
  only. A recorded refusal that is not typed for this item (a bare `IO` or
  `FRAME_CODEC`, a retired code, a record naming another source) is printed
  as `recorded_refusal` with the same `site` and `errno`.
- `outcome` is still `"refused"` for a legacy refusal record that names no
  code (`refusal-untyped`).
- The `reviewed` block (`schema`, `ledger`, `plan_digest`, `totals`,
  `decisions`, `policies`, `unmatched` with a `reason` per row) is new, and
  the `attested` block gains `totals.pending_review`, a `pending_review`
  list and `review` on reviewed rows.

## Live union

Both hosts stay usable. Bulkload never signals sessions, never requires a
stillness pair and never requires writers to pause.

Source safety (S2, WP1):

- Every Git child is built from one hardening table (`git_carry::git_env`):
  no hooks, fsmonitor, automatic gc or maintenance, optional locks, lazy
  fetch, system or global config, or replace objects; the C locale; and a
  discovery ceiling at the given path's parent. The estimate's remote probe
  is tested against the same table.
- A partial-clone source refuses `GIT_SOURCE_PARTIAL_CLONE` before any other
  read, in `git-export` and `estate-capture`.
- The source's object store is read-only to a capture (#162).
  - Git freshens (re-stamps the mtime of) any existing copy of an object it
    writes, alternates included.
  - So the private repository's object writers (`hash-object -w`, `mktree`,
    `write-tree` and its archival commits) run against a write store that
    borrows nothing (`git_env::WRITE_STORE`).
  - `fast-import` never explodes its pack into loose objects
    (`fastimport.unpackLimit=0`).
  - Readers see the write store, then the source, through `alternates`.
  - The P34 property holds every `lstat` field of the source fixed across a
    capture and its chained reruns.
- Source-side verbs (`serve`, `estate-capture`, `snapshot`,
  `git-carry-estimate`, `git-export`, `copy`) enter background CPU and IO
  priority before anything else (WP0(f)), inherited by every thread and
  child. `--priority=normal` is the explicit, recorded opt-out; every
  counters line and the bench header record the class.
- `serve` and `copy` refuse a private state root that overlaps the source
  (`SNAPSHOT_ROOTS_OVERLAP`) before any store is created.

Git carry retains refs, objects, real stash commits including binaries and
untracked files, indexes and dirt, worktree administration and translated
paths. A bare repository (a mirror) is carried as ref custody: its refs, HEAD
and administration, with empty staged and worktree trees, since it has
neither an index nor a worktree (S4, #162).

- The capture marks itself bare in-band (`bare-repository-v1`, lifted into a
  shallow envelope's headers). Every verb that lays down a workspace, an
  index or a payload attachment refuses such a capture
  `GIT_BARE_CAPTURE_WORKSPACE` before writing anything; it applies only as
  `refs-imported`, planned without a workspace.
- A bare repository is a root only at its own git dir. Reached through a
  `.git` gitfile (the bare-plus-worktrees layout), it refuses
  `GIT_REPOSITORY_NOT_AT_PATH`, as the estimate probe does.
- A repository's own administration below its root, reached through a
  gitfile (`--separate-git-dir`), is never a seat.
- A non-bare repository with no index file (a `--no-checkout` clone or
  worktree) refuses `GIT_INVENTORY_INDEX_ABSENT`. Git reads the absent file
  as an unborn index that `git checkout` populates, and an empty index file
  does not, so no carried index restores it.
- A capture's header does not list every carried ref (OI-1003-Q54, #178;
  `docs/plans/2026-10-05-v1-header.md`). The refs travel in a
  content-addressed ref table commit (`refs/carry-ref-table/v1`), with one
  tip ref per distinct object they name, so the header grows with the
  distinct objects, not the refs: 111 B each self-contained and 165 B each
  in a thin bundle (SHA-1; 159 B and 237 B on SHA-256). Import expands the
  table into exactly the refs the old format carried; a capture without a
  table (every capture written before) imports as it always did. Import is
  linear in the bundle: `bundle unbundle` and one connectivity walk, no
  ref-name matching.
- **No mixed-version apply.** A capture with a ref table is applied only by
  a build that reads one. The table ref sits outside `refs/carry-export/`
  on purpose: every earlier build refuses such a capture
  `GIT_INVENTORY_MALFORMED` before it writes a ref, never imports it as a
  wrong ref set.
- A bundle header, a ref table or a shallow envelope's manifest over its
  size bound refuses `GIT_INVENTORY_OVER_CAP`, never
  `GIT_INVENTORY_MALFORMED`. Writers measure the header before they write
  it, so no capture is recorded that a later reader refuses for size. A
  thin bundle (a chained link or a plan base's item) whose header would be
  over the cap is written self-contained instead of refused.

Import preserves divergence and leaves active HEADs, indexes and
working bytes untouched. Account credentials carry privately; platform stores
may require a format transcode. Destination machine keys and Home Manager
links are preserved. Credential contents are never printed.

SQLite is captured with backup-API snapshots, never raw live WAL/SHM copies.
Composition preserves unique rows on both sides; schema or row conflicts keep
their snapshots for explicit resolution.

## Drift

A capture pass tolerates refs and worktree seats moving under it (R25), so
writers never pause for a capture. What moved is recorded, never absorbed:

- Only ref and seat drift is tolerated. HEAD, the index, configuration, the
  shallow frontier, nested worktree custody or the omitted rebuildable roots
  moving under a pass still refuse `GIT_AUTHORITY_CHANGED` (R-N30).
  Directory-shape drift (a directory removed, a directory replaced by a file, a
  file replaced by a symlink) refuses fail-closed.
- An object-store rewrite is drift too (WP1, S5). The export reads the
  source's pack listing with its authority; when a Git child of the pass
  fails and that listing has changed (a `gc`, `repack` or `prune` racing
  the pass through the private repository's `alternates`), the item is
  `deferred-with-drift` with one `ObjectStoreRewritten` row, no capture
  record is written, and the next pass captures the rewritten store. The
  child's failure is the one a failed v1 Git child raises,
  `GIT_CHILD_FAILED` with its stderr class (WP3), or
  `GIT_INVENTORY_MALFORMED` for output that did not parse; the same failure
  under an unchanged listing still refuses.
- The pass window runs from the pre-pass key to the post-pass key, not only
  across the export's own snapshot. A capture is clean only when the
  pre-pass key parts, the export's own before and after ref inventories, and
  the post-pass key parts all agree; a ref that vanishes before the export's
  snapshot and returns after its last ref read is drift (R-N72).
- Git authority is read once per export and carried exactly (the index
  checks validate a private copy of the carried bytes, never the live index):
  HEAD, the symbolic HEAD, the index bytes, `info/exclude`, the stash reflog, the
  configuration files and the shallow frontier. The export re-reads all of
  it at the end of its pass, and the capture refuses `GIT_AUTHORITY_CHANGED`
  unless both key parts name exactly what the export carried, so authority
  that moves away and back outside the export's window can never leave a
  stale bundle behind an equal key.
- Index entry flags (#106): an intent-to-add entry (`git add -N`) is
  carried as index custody, `refs/carry-export/intent-to-add-v1` (path,
  mode, empty-blob flag), recorded only when there is one; its seat's bytes
  ride in the worktree tree. `git-restore`, `git-restore-linked`, estate
  apply and `git-repair-missing-index` re-mark the paths after reading the
  staged tree and check them back, so `git status --porcelain=v2` matches.
  An intent-to-add entry whose seat is gone, one whose seat's mode is not
  the entry's (`git add -N f; chmod +x f`: restore re-marks the path from
  the seat and would read 100755, #131), or one in a nest (whose index is
  not carried), refuses `GIT_INVENTORY_INTENT_TO_ADD` at capture; the
  restore verbs check the custody against the worktree tree again before
  any worktree byte is laid down. The `git-attach-*` verbs refuse a capture
  carrying any. Any other entry flag (assume-unchanged, skip-worktree)
  still refuses `GIT_INVENTORY_MALFORMED`. Git's fsmonitor validity bit
  (`CE_FSMONITOR_VALID`) is cache state, not index state: it is masked
  before an entry is classified, a source's fsmonitor never runs (every
  carry pins `core.fsmonitor=false`), and a restored index carries no
  fsmonitor extension (#131).
  A bundle whose prerequisite commits the receiving repository lacks
  refuses `GIT_INVENTORY_MISSING_PREREQUISITE`; fetch them as objects and
  retry.
- There are two drift classes. Export drift moved under the export itself:
  the bundle omits the drifted seats' bytes and carries the in-band
  `refs/carry-export/capture-drift-v1` marker. Every restore and import verb
  (estate-apply, `git-restore`, `git-restore-linked`, `git-import`,
  `git-repair-missing-index`, both `git-attach-*` verbs and
  `git-restore-registered-payload`) refuses a marked bundle with
  `CAPTURE_DRIFTED` before it writes anything, whether or not any corpus
  sidecar exists. A shallow envelope lifts the marker into its own headers
  (`shallow-drift-v1`), so the check reads bundle headers only and never
  fetches a pack. Each verb first stages the bundle into a private copy and
  reads only that copy, so the checked bytes are the imported bytes. The
  stage sits next to the bundle (the corpus, for an estate apply), falling
  back to TMPDIR. It is a full copy, never a clone, hashed while it is
  written, so apply's digest check costs no second read. A shallow envelope
  whose inner inventory carries the marker but whose headers do not still
  refuses when it is unpacked. Re-applying an item that is already done
  reads its journal first and stages nothing. Key drift moved only outside the export's window: the
  bundle is a coherent snapshot of the export's own view and applies.
- A drifted capture of either class records a poisoned key that no census
  hashes to, so it is never a reuse hit, even if its `{bundle}.drift` sidecar
  is lost or two state directories sharing one corpus interleave their
  records. The sidecar is the receipt's statement of what moved, not the
  guard. The next capture pass extends it (`capture-extended-from-drift`).
  R-N29 (apply proceeds on an occupied destination, recording uncaptured
  seats) is deferred (bulkload#48); carry v2, the W6 engine it was deferred
  to, is deleted (OI-1003-Q44, OI-1003-Q56).
- Auto-prerequisite chains (WP2, OI-1003-Q15, 2026-10-03). Without a shared
  plan base, a capture that follows a retained capture of the same checkout
  declares that capture's tips as its bundle prerequisites, so it packs only
  what is new since then instead of re-packing all history. Only tips whose
  commits the source object store holds qualify; the capture's own metadata
  commits never do, so every link stays fetchable wherever the source is an
  alternate, and a pruned tip only means a larger pack. The `{bundle}.prior`
  sidecar (durable before the record) names the predecessor by name, digest,
  stat identity and chain depth. Depth is bounded by
  `git_carry::chain::CHAIN_DEPTH_LIMIT` (8): the capture after a depth-8
  bundle re-bases to a self-contained one. Estate apply verifies the chain
  (every link retained under its recorded name and digest, depths falling by
  one to a self-contained root, every prerequisite satisfied) and restores
  from one flattened bundle whose refs must equal the head bundle's. Apply
  never binds a link by stat identity: a corpus pulled to the apply host
  gives every file a new device, inode and ctime, exactly as the shared
  `.base` import binds by digest only (R-N72). Capture-side reuse and
  chaining do require the recorded identity, so a link replaced in place is
  never extended. A missing link refuses `SEALED_OBJECT_MISSING`, a link
  with other bytes `DIGEST_MISMATCH`, inconsistent depths
  `RECEIPT_BINDING_INVALID`, and a broken chain is never a reuse hit: the
  next capture re-bases. The tip query runs through the estimate's hardened
  source wrapper, and every stdin-fed git child drains its answer while its
  requests are written, so a prior with thousands of refs cannot hang a
  capture on a full pipe. The capture counters
  (`write_source_pack_bytes`, `write_source_pack_objects`,
  `read_source_pack_readback_bytes`, `census_walks`) measure it. A chained
  link or a grouped item bundle is thin (OI-1003-Q42): its deltas name
  bases its prerequisites hold, which packing reads but never writes, so
  `write_source_pack_bytes` no longer bounds the source reads of a thin
  capture; `read_source_pack_readback_bytes` sees them, as a lower bound.
  The next pass's blob-reuse fetch of a thin retained capture completes it
  with `index-pack --fix-thin`, reading every base from the source object
  store: `read_source_capture_reuse_bytes` counts those bases beside the
  bundle, and the fetch's storage reads join the readback counter.
  Since Q42 L6a (OI-1003-Q43) these choices are one pure, total function,
  `git_carry::decide::decide`, which P67 pins to the Haskell reference's rows.
- A whole capture is reused (`capture-reused-after-census`) only when its key
  is unchanged and no seat is racy against its recorded pass start. A capture with a racy seat, or with no recorded pass start
  (records from before the start was recorded), takes the per-seat path
  instead (R-N76).
- An incremental pass reuses a retained blob only for a seat whose stat
  identity is unchanged and which was not racy. The transfer applies the
  same rule to its ledger and output rows (see Wire v5, racy captures). A seat stamped within one
  timestamp tick (a 2 s allowance) of the retained pass start can be
  rewritten at the same size without its identity moving, as in Git's racy
  index, so it is read again. A pass that reuses none of the blobs it was
  offered says why: `reuse_unavailable=shallow`, `retained-unreadable`,
  `pass-start-unrecorded` or `future-stamp` (a seat stamped later than the
  pass's clock blocks every whole-capture reuse until the clock passes it). A retained capture that cannot be read degrades
  to a full read, and its transient refs never reach the new bundle.
- A seat stamped later than the current pass's own clock reading is racy too
  (fail-closed on timestamps from the future). Known limit: the racy
  reference is the capturing host's wall clock, not the filesystem's. A
  filesystem whose clock runs behind the host by more than the 2 s allowance
  (an NFS or SMB server with NTP skew) can stamp a write made after the pass
  start earlier than the window, and the guard cannot see it. Bulkload never
  writes into a source to read the filesystem's clock.
- Known limit: capture records and their sidecars (`.capture`, `.parts`,
  `.drift`, `.base`) are not authenticated. Anyone who can write the corpus
  can forge a record into a whole-capture reuse. The in-band drift marker
  still makes every restore verb refuse a drifted bundle, and apply still
  checks the bundle digest, but corpus integrity rests on its 0700 custody.

## Performance

Performance is the product bar (R23, R25). Two gates, each a 3-rep A/B/A/B/A
median (R-N57):

- **(a) Loopback:** the native engine, running its full protocol over a local
  socket with no clone fast path, beats `rclone copy` as shipped on the same
  corpus and volume.
- **(b) Cross-host:** a pull between two hosts beats rclone over sftp on the
  same corpus.

Resume (R25, R-N58): a byte the destination already holds durably is never
read again, and a seat whose stat identity (source authority, device, inode,
size, mtime and ctime at nanosecond precision) is unchanged, and was not racy
when it was recorded, is never read again. Stat identities are reuse keys, not content digests; changed files,
replaced inodes, journal gaps and lost source authority invalidate reuse, and
correctness takes precedence over a zero-reread claim. Every run reports
bytes read, bytes sent, flushes, memory and wall time.

Since #169 an output whose bytes are durable but whose row never committed
(a crash between publish and row commit, or a failed group) is adopted on
resume from its capture record, by hashing the destination's own bytes: 0
source bytes read.

- The record is an extended attribute on the output. It names the walked
  row (the seat's path and stat identity, with no store authority), the
  capture's manifest root and its size.
- A staged file gets it before its seal. An existing output adopted against
  a manifest gets it, or has a stale one replaced, once its bytes are
  verified.
- Only a non-racy capture gets one. It needs extended attributes, and on an
  adopted output a mode its owner can write. A record that could not be
  written is counted (`transfer_capture_records_unset`).
- An output with a matching row is never adopted: its `Reuse` comes from
  the row. A clean rerun adopts nothing.
- A source store that lost its authority re-keys every row, but its
  outputs' records still match, so those outputs are adopted, not re-read.
- An existing output with no matching row that no record proves is read
  again and counted (`transfer_unrowed_unproven`): no record, bytes
  rewritten in place, or a changed mode. A record that names another row
  (the seat moved since that capture) is read again and not counted.

## M2 gates (current)

The acceptance criteria for each M2 workstream as they stand today. Rulings
are recorded on TIN-3692. When a ruling changes a gate, this table is
updated.

| Workstream | Acceptance | Rulings |
|---|---|---|
| W2 measure | Byte, hash, flush and commit counters in every verb. Flush counts are a lower bound, because SQLite's and git's own syncs are not counted. The corpus-shaped durable floor is measured interleaved with rclone, ≥ 7 reps, with every gated sample taken under the conditions below. | R-N62, R-N81, R-N87 |
| W3 v3 engine wins | Destination BLAKE3 ≤ 1× payload. Full flushes ≤ groups + 1. The bench median improves. The < 1.5 s target moved to W4. | R-N77, R-N95 |
| W4 single pass (wire v5) | Gate (a): the loopback native engine beats rclone as shipped, 3-rep medians. Median < 1.5 s on the TinylandState corpus. File I/O ≤ 2.05× payload and hash ≤ 2× payload. Warm resume reads 0 bytes. A refused live-writer capture leaves no source ledger row. | R-N57, R-N58, R-N86, R-N95, R-N118 |
| W5 parallel and wire | Gate (b): a neo→sting pull beats rclone over sftp, 3 reps. Each result reports its percentage of the calibrated link ceiling. | R-N57, R-N64 |
| W6 M1 git carry | **Retired 2026-10-05 (OI-1003-Q44, OI-1003-Q56).** carry_v2's sender and ingest are deleted; v1 is the Git engine, and tag `carry-v2-final` holds the deleted code and this row's earlier text. The estimate half stands: `git-carry-estimate`'s thin pack equals what `upload-pack` sends for exactly the destination's held tips, offered ancestors first (P66). It pins `pack.useSparse=false` and `pack.useBitmaps=false`, adds `--shallow` with `--objects-edge-aggressive` when the destination is shallow, and refuses a shallow source with a full destination (`GIT_HAVES_UNPROVABLE`, `source_shallow_destination_full`). The segment-resume rule and the network-filesystem ingest refusal went with the ingest. | R-N60, R-N74, R-N75, R-N97, R-N113, R-N116, R-N131, OI-1003-Q44, OI-1003-Q56 |
| W6 M2 git carry | `census_walks == 1`. `bytes_read` equals the total size of the changed seats. The live-writer test passes. | R-N58 |
| W7 proofs | I1 (a record implies its bytes), I2 (no partial leaves) and I3 (a committed file means 0 source reads on resume) hold at every process-crash fault point. Power-loss ordering is proven by a syscall-trace crash-state checker. Darwin barriers are modelled device-wide, with the per-fd model available as a strict option. | R-N86, R-N88, R-N103 |

Gated benchmark samples require AC power and a 1-minute load below 2.5,
and each sample row records both. The coordinator keeps the other lanes
quiet while gated samples run (R-N81, R-N91). The clone fast path never
counts toward a gate (R-N57, R-N63).

## Wire v5

The transfer wire is protocol 5, a hard cut with no dual stack (R-N59,
R-N118). `bulkload-proto` holds the codec and its schema text, `WIRE_SCHEMA`;
a peer whose `Open` names another protocol or another BLAKE3 of the schema
(`wire_id`) is refused before anything else. Every frame is a 4-byte
big-endian length, a tag byte and a body: tag 1 is a postcard control
message, tag 2 a content chunk (a fixed 64-byte little-endian header, then the
payload, sent with `writev`), tag 3 a Git pack piece (reserved). A control
body is exactly one postcard message: bytes after it are refused
`FRAME_CODEC`, as are bytes after a record in either store, which read as a
miss (#87).

- **Entries.** The source offers each walked seat as a numbered `Entry`, up to
  1024 undecided, and the destination answers each with `Decide`: `Skip` (a
  directory or symlink it made), `Reuse` (it holds this exact stat identity
  durably; the source reads nothing), `Refuse`, `Send` or `WantManifest`.
- **Walk.** The source walk is a stream: each seat is offered as it is
  found, a directory before anything beneath it, names in byte order within
  a directory and no global sort, so the first `Entry` leaves before the
  walk ends. The walk runs at most 4096 items ahead of the wire. Every seat
  is walked and read beneath one descriptor of the source root, opened
  component by component with `O_NOFOLLOW` at each one, so a directory
  swapped for a symlink is refused, never followed out of the root. A seat
  more than 256 components below the root, or with a relative path over 4095
  bytes, is refused as a value (`PATH_DEPTH_EXCEEDED`, `PATH_TOO_LONG`) with
  its subtree, and its siblings are carried; the walk so holds at most 256
  directory descriptors, plus one while it lists a directory at the cap
  (#110). Both caps are `WalkLimits`, configurable only below these
  defaults. A directory at the depth cap is listed and refuses its contents
  only when it holds something the walk would carry; the length cap applies
  after the stat. Engine temporaries are matched before either cap, and a
  seat on another device in a same-device walk is skipped before either, so
  neither is a false refusal (#129). A capped subtree is a refusal like any
  other, by path and code: it is never counted as carried, it keeps a
  strict-completeness run from finishing its directories, and the transfer
  receipt counts it as `capped_subtrees`. Content is read with `pread`,
  never mapped.
- **Send.** The source reads the file once, chunks it, hashes each chunk and
  streams it as a data frame (entry, chunk index, offset, size, digest), then
  sends `End` with the manifest root, chunk count, size and whether the
  capture was racy. The destination
  verifies every chunk against its digest, writes it at its offset, and checks
  coverage and the root before the output is queued for its group commit.
- **WantManifest.** Chosen only when the destination could fill chunks
  itself: the output path already exists (adopt or refuse), or published
  outputs hold chunks (resume, incremental). The source sends `Manifest`, from
  its ledger without reading when the stat identity is recorded; the
  destination fills what it can from verified local chunks and asks for the
  rest by index (`NeedChunks`); the source sends only those, then `End`. A
  fresh manifest is built from one read whose chunks are all kept in memory
  (512 MiB budget); a file past the budget is streamed as for `Send`
  instead, so no seat is read twice in a session.
- **Credit.** The destination grants 16 MiB of data payload and returns
  credit as it writes; the source never has more than the granted bytes in
  flight, and at most 16 entries' content. Available credit never exceeds
  the window; a grant past it is refused.
- **Ledger.** The source ledger is digest-only (R-N58): a capture records its
  row key and manifest (digests, sizes and `manifest_root`), never bytes, and
  only after its final stat check, so a refused live-writer capture leaves no
  row (R-N86). `manifest_root` (BLAKE3 in derive-key mode over each chunk's
  digest and size) replaces the whole-file hash. `SourceDone` follows the
  ledger's last commit.
- **Racy captures.** A capture whose seat's mtime or ctime falls within the
  2 s allowance of the clock read before the file was opened, or later than
  the clock read after its final stat check, is racy, exactly as in the Git
  carry (R-N76): a same-size rewrite in that tick can keep its stat
  identity. It is sent and published, but never recorded: the source keeps
  no ledger row, and `End{racy}` tells the destination to keep no output row
  under its key (its chunk hints are kept; they are re-verified on use). The
  next run reads the seat again (#86).
- **Rows from before the racy guard.** A store created by this engine
  carries a `racy_guard` marker from its first commit. A store without it was
  written before #86, so none of its ledger or output rows is proven
  non-racy. The first writable open deletes every `captures` and `outputs`
  row in the transaction that adds the marker, and counts them in
  `transfer_legacy_rows_invalidated`; a read-only handle on an unmarked store
  serves none of them. Each such seat is read from the source once more,
  which R25 allows (its row cannot show it was not racy), and recorded under
  the guard; chunk hints are kept, so content the destination still holds is
  adopted after verification and never crosses the wire again (#125). The
  single migration run (OI-1002-Q24) may therefore start from existing
  pre-migration state.
- **Known limit: clocks.** The racy predicate compares the source
  filesystem's mtime and ctime with the capturing host's wall clock, as in
  the Git carry. Destinations are local filesystems (OI-1001-Q17), but a
  source may not be: a source on NFS or SMB whose server clock runs behind
  the capturing host by more than the 2 s allowance can stamp a rewrite made
  during a capture earlier than the window, and the guard cannot see it; a
  server clock running ahead only makes more captures racy (fail-closed).
  Bulkload never writes into a source to read its filesystem's clock. Keep
  network-mounted sources NTP-synchronised with the capturing host, or carry
  them from a host where they are local.
- **Held.** The destination answers every `End` with `Held`. A capture is
  committed to the ledger only when the destination holds its bytes
  durably: `Held{true}` is sent once the output's group commit has
  returned (file and directory sealed, then the store commit), for a
  written output or an existing one verified against the manifest. No
  flush is added for it. So a committed capture is never read from the
  source again (R25, OI-1001-Q15): a resume reuses or adopts the final
  name. A group whose seal or store commit fails (a full disk refuses
  `DESTINATION_SPACE_INSUFFICIENT`) answers each of its entries
  `Held{false}`: neither store records them, the session finishes with the
  refusals, and the next run reads them once (#100). The sweep keeps every
  one of this store's orphaned file temporaries as a chunk source, under a
  name of the new session. They only save wire bytes: no recorded capture's
  bytes live only in a temporary. When the session finishes it removes them,
  except those an entry refused in that session had staged chunks from (a
  byte-touching refusal: verification, publication or the group commit
  failed after the entry was filled), which are kept for the retry and
  reported as left (#124, OI-1002-Q33). An entry refused before it staged
  anything keeps nothing, so a path refused on every run never keeps
  temporaries (#97). What is kept is bounded, 1024 temporaries and 4 GiB per
  session (unruled engineering defaults: OI-1002-Q33 ruled that salvage is
  bounded, not these numbers; a ruling may change them); a temporary past the bound is removed and refused as a value,
  `SALVAGE_BOUND_EXCEEDED` under its current name, and its chunks are sent
  again.
- **Hints.** The destination records, per digest, every published output
  holding it, newest first; a hint is re-read and re-verified on use, and a
  miss falls through to the next.

Known limit: a fresh destination is streamed with no cross-file
deduplication, so a chunk repeated across files crosses the wire once per
file. Deduplication applies when the destination asks for a manifest.

### Reserved Git sub-stream (W6)

Control variants 12 to 19 and tag 3 are reserved for W6 git carry over the
same session and are refused today. A sub-stream carries negotiated thin
packs for one repository (R-N60). Since carry_v2's deletion (2026-10-05,
OI-1003-Q44, OI-1003-Q56) no code builds or consumes them. They stay in wire
v5 unchanged (`WIRE_SCHEMA` and `wire_id` do not move) and go with WP3's v6
cut.

| Frame | Direction | Meaning |
|---|---|---|
| `SubOpen{sub, GitPack{repo, refs_digest}}` | source → destination | Opens sub-stream `sub` for a repository and the ref inventory it will carry. |
| `GitHaves{sub, haves, done}` | destination → source | Object ids the destination holds, in rounds, ancestors first; `done` closes negotiation. |
| `GitSegmentStart{sub, segment, objects, bytes}` | source → destination | A thin-pack segment begins, with its object and byte counts. |
| tag 3 `PackData{sub, segment, offset, size}` | source → destination | Raw pack bytes of a segment at an offset, under the same credit as data frames. |
| `SegmentEnd{sub, segment, pack_digest, objects, bytes}` | source → destination | A segment's bytes are all sent, with their BLAKE3. |
| `SegmentDurable{sub, segment}` | destination → source | The segment is ingested durably in quarantine. |
| `GitRefs{sub, updates}` | source → destination | The ref transaction to apply after every segment: name, expected old value, new value. |
| `GitCommitted{sub, transaction_digest}` | destination → source | The ref transaction committed. |
| `GitResume{sub, durable_segments}` | destination → source | On a resumed sub-stream, the segments already durable, so only the others are re-sent (W6 M1: a crash in segment k re-sends only segments ≥ k). |

**Caller retry contract (#89, OI-1001-Q17): retired 2026-10-05.** It
specified how a caller retries carry_v2's ingest locks
(`Ingest::finish_retrying`, `FenceRetry`). Those were deleted with carry_v2
(OI-1003-Q44, OI-1003-Q56), along with the `JOURNAL_OWNERSHIP_CONFLICT` and
`GIT_DESTINATION_FILESYSTEM_UNSUPPORTED` refusals that only they raised. Tag
`carry-v2-final` holds the code and the contract's text. A future receiver
for these frames needs a contract of its own.

## Durability

A record is never committed before the bytes it describes are durable on the
destination. Existing destination files are never overwritten; publication is
no-replace. A directory the engine creates is made under a tagged temporary
name, its record is bound to the new inode, and it is then renamed into place
with no-replace. The engine never adopts a directory it did not create
(R-N78, R-N102).

Records commit in groups. Each file is sealed (`F_BARRIERFSYNC` on Darwin,
`fsync` elsewhere, so its mode is durable too) and renamed into place without
replacement. Each touched directory is sealed once. Each touched device other
than the state store's is fully flushed. Then one `SQLite` WAL commit
(`synchronous=FULL`, `fullfsync=ON`) makes the whole group durable; its full
flush drains the store's own device. A group whose files share the store's
device therefore needs no other device-cache flush. `--durability=strict`
fully flushes every file instead, for comparison.

A store's state root and its database entry are sealed (the root fully
flushed) before `Store::open` returns, so before Start and any commit, and a
store without the `root_sealed` setting is sealed again on open (#161, R25).
The setting commits only after the seal returns. A sealed store reopens
without opening the root's parent; a root that still needs its seal under a
parent the agent cannot read refuses with `IO` (`EACCES`). Other private
state directories seal their own entry before records go in.

Space preflight (OI-1001-Q2): a write that would leave its destination
filesystem (`statvfs`; `statfs` on Darwin) with less than
`--min-free-percent` free (default 25%; 0 still refuses a write larger than
the space available) refuses `DESTINATION_SPACE_INSUFFICIENT` before it
starts.

- `copy` and `pull` decide it per entry on wire v5. An entry decided
  `Send` or `WantManifest` reserves its size until its `Held`. Each
  admission checks every reserved byte against a probe that is refreshed on
  each group commit and every 256 MiB admitted. An entry that does not fit
  is answered `Decision::Refuse` with that code: no new frame, the session
  continues, and a later run resumes it without re-reading durable bytes
  (R25).
- `estate-apply` checks before any item runs. Each pending item's bundle
  size is charged to its repository's filesystem and, for a linked worktree,
  to its workspace's. The corpus's filesystem is charged twice the `jobs`
  largest bundles, for bundle and base staging. Bundle size is a lower
  bound on a checkout. An item the plan cannot read is skipped and refuses
  on its own.
- `estate-capture` checks per item (#101, OI-1002-Q11). Before an item's
  export writes, its estimated bundle (the larger of its census's
  regular-file bytes and the retained bundle it extends) plus every byte
  still reserved by in-flight items is checked against a fresh probe of
  CORPUS. An item that does not fit refuses
  `DESTINATION_SPACE_INSUFFICIENT` as its own receipt; the other items run
  and the next pass retries it. A reuse hit writes nothing and reserves
  nothing. Shared-base preparation is not charged.

## Reclaim

Unique content is preserved before redundant containers are deleted. Content
and database rows are compared as appropriate; names and sizes are not
redundancy proof. Hardlinks mean apparent tree size is not reclaimable space.
