# Bulkload product contract

## Completion

The destination must support daily work: authenticated agents with history,
SSH, Git commits and pushes, PR operations, editors and persistent terminal
sessions. Counts, manifests and documentation merges alone do not establish
completion.

Coverage includes both hosts' unique Git and agent state, credentials, dots,
SQLite and worktrees. Unsupported items remain outstanding with preserved
custody; nothing is silently excluded.

## Live union

Both hosts stay usable. Bulkload never signals sessions, never requires a
stillness pair and never requires writers to pause.

Git carry retains refs, objects, real stash commits including binaries and
untracked files, indexes and dirt, worktree administration and translated
paths. Import preserves divergence and leaves active HEADs, indexes and
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
  seats) is deferred to W6 git carry v2 (bulkload#48).
- A whole capture is reused (`capture-reused-after-census`) only when its key
  is unchanged and no seat is racy against its recorded pass start. A capture with a racy seat, or with no recorded pass start
  (records from before the start was recorded), takes the per-seat path
  instead (R-N76).
- An incremental pass reuses a retained blob only for a seat whose stat
  identity is unchanged and which was not racy. A seat stamped within one
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
| W6 M1 git carry | Sent bytes ≤ 1.1× `missing_thin_pack_bytes`, and sent objects ≤ the estimate's object count (the estimate is an upper bound). M1 sends every destination-held tip as a have in its first round, ordered ancestors first. On every fixture, the sent object set equals what `upload-pack` sends for exactly that have set. The estimate and the sender both pin `pack.useSparse=false` and `pack.useBitmaps=false`, and add `--shallow` with `--objects-edge-aggressive` when the destination is shallow. A shallow source with a full destination is refused (`GIT_HAVES_UNPROVABLE`, `source_shallow_destination_full`) by both, before anything is sent; a shallow file is never written into a full destination. Extra haves beyond the held tips can make a shallow pack larger, so M1 sends exactly the held tips. A crash in segment k re-sends only segments ≥ k. | R-N60, R-N74, R-N75, R-N97, R-N113, R-N116, R-N131 |
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
payload, sent with `writev`), tag 3 a Git pack piece (reserved).

- **Entries.** The source offers each walked seat as a numbered `Entry`, up to
  1024 undecided, and the destination answers each with `Decide`: `Skip` (a
  directory or symlink it made), `Reuse` (it holds this exact stat identity
  durably; the source reads nothing), `Refuse`, `Send` or `WantManifest`.
- **Send.** The source reads the file once, chunks it, hashes each chunk and
  streams it as a data frame (entry, chunk index, offset, size, digest), then
  sends `End` with the manifest root, chunk count and size. The destination
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
- **Held.** The destination answers every `End` with `Held`. A capture is
  committed to the ledger only when the destination holds its bytes
  durably: `Held{true}` is sent once the output's group commit has
  returned (file and directory sealed, then the store commit), for a
  written output or an existing one verified against the manifest. No
  flush is added for it. So a committed capture is never read from the
  source again (R25, OI-1001-Q15): a resume reuses or adopts the final
  name, or salvages a temporary. The sweep keeps every one of this
  store's orphaned file temporaries as a chunk source, under a name of the
  new session, and removes them when the session finishes, unless the
  destination refused an entry, in which case they are kept for the next.
- **Hints.** The destination records, per digest, every published output
  holding it, newest first; a hint is re-read and re-verified on use, and a
  miss falls through to the next.

Known limit: a fresh destination is streamed with no cross-file
deduplication, so a chunk repeated across files crosses the wire once per
file. Deduplication applies when the destination asks for a manifest.

### Reserved Git sub-stream (W6)

Control variants 12 to 19 and tag 3 are reserved for W6 git carry over the
same session and are refused today. A sub-stream carries negotiated thin
packs for one repository (R-N60):

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

## Reclaim

Unique content is preserved before redundant containers are deleted. Content
and database rows are compared as appropriate; names and sizes are not
redundancy proof. Hardlinks mean apparent tree size is not reclaimable space.
