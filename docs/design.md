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
- Git authority is read once per export and carried exactly: HEAD, the
  symbolic HEAD, the index bytes, `info/exclude`, the stash reflog, the
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
  reads only that copy, so the checked bytes are the imported bytes (a clone
  on APFS, btrfs and XFS; a full copy elsewhere). Key drift moved only outside the export's window: the
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

## Durability

A record is never committed before the bytes it describes are durable on the
destination. Existing destination files are never overwritten; publication is
no-replace.

## Reclaim

Unique content is preserved before redundant containers are deleted. Content
and database rows are compared as appropriate; names and sizes are not
redundancy proof. Hardlinks mean apparent tree size is not reclaimable space.
