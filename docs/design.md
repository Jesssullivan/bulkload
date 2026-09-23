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
size, mtime and ctime at nanosecond precision) is unchanged is never read
again. Stat identities are reuse keys, not content digests; changed files,
replaced inodes, journal gaps and lost source authority invalidate reuse, and
correctness takes precedence over a zero-reread claim. Every run reports
bytes read, bytes sent, flushes, memory and wall time.

## Durability

A record is never committed before the bytes it describes are durable on the
destination. Existing destination files are never overwritten; publication is
no-replace.

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
