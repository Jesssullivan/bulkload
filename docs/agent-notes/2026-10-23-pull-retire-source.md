# 2026-10-23: `pull-retire-source`

## Ruling

Operator ruling, 2026-10-23. It was relayed verbatim by the lane
coordinator; I did not hear it from the operator directly:

> "once migrated, we should clera the duplicates (sting can / shoudl be the
> only copy for thes codex objects)"

It follows the rollout rulings (#326, 05f5a7b). bulkload had no verb for
this.

## Verb

`pull-retire-source DEST_STATE [--ledger DIR] [--dry-run] [--remote-executable PATH] [--ssh-config FILE]`

### Destination side (`retire::plan`)

Everything here is read only.

- **Lock.** It takes `DEST_STATE/writer.lock` without waiting, and only
  while it reads. If a pass holds the state it refuses `MOVE_STATE_BUSY`.
- **Inputs.** It reads the state's closure records, which name `HOST:SOURCE`
  and the destination. It reads the store's `consolidated` and `outputs`
  rows. A row key is postcard `(authority, RowSchema)`, so each key gives
  the source identity its outcome was recorded against. It also reads the
  consolidation ledger's `rollout_link_outcome`.
- **Candidates.** A regular, non-SQLite seat is a candidate when its
  outcome proves the destination holds its content:
  - `adopt` (or an `outputs` row), `extend`, `covered`, or rulings'
    `extended`. The proof is the destination's BLAKE3 as of now.
  - Rollout `covered` by events (#325), or by ids (ruling 2). The proof is
    the ledger's outcome, and the live link must still resolve to the same
    linked file identity.
- **Skips.** Any other outcome, or a proof that no longer holds, is
  skipped here, with a typed reason.

### Source side (`retire::answer`)

This is the remote `retire-source` over pull's ssh builder, or it runs
locally for a `-` host. For each candidate:

- The path is opened beneath the root, following no link.
- It must be a regular file, and its identity must be one that was
  recorded.
- No process may hold it open. Linux checks `/proc`; Darwin runs
  `/usr/sbin/lsof -F i` and matches by inode. Exit 1 counts as "none
  open" only with nothing on stderr. If that cannot be told, the file is skipped.
  (Superseded 2026-10-28 by #339: Darwin reads libproc in the agent's own
  process and matches by device and inode. No lsof child remains.)
- When the proof is BLAKE3, the source file's BLAKE3 must equal the
  destination's, and its identity must be unchanged across the read.
- The identity is checked once more, then `unlinkat` removes that one name
  and the directory is synced.

Anything else is skipped, typed, and left as it is. It never removes a
directory or a SQLite store, and never touches the destination or an
archive.

### Output

- One receipt line per file is appended to
  `DEST_STATE/retire-source.receipts`, made durable:
  `at_ns`, verdict, path, `dev:ino:size:mtime:ctime`, proof, reason.
- stdout lists each file, then a summary, then one line per skip reason.
- `--dry-run` runs every check and removes nothing.
- Reruns are idempotent: a file already removed is skipped as absent.

## Tests (`retire::tests`)

- Only the provably held, unchanged files go: one landed equal, one
  covered by its linked archive file.
- Each of these is skipped and left:
  - a source changed after its outcome was recorded;
  - a destination whose bytes differ;
  - a link that now names another file;
  - a merged config;
  - a SQLite store.
- A dry run lists exactly the same files and removes nothing.
- The destination and the archive are unchanged (bytes, link text, mtime).
- A rerun reports the removed files absent.
- The receipts carry every line.
- A file another process holds open is skipped (Linux).
- A state a pass holds is refused `MOVE_STATE_BUSY`.

## Not proven here

(Superseded 2026-10-28 by #339: the lsof path is gone, and the libproc
path passed `darwin-check` on pzm.) (Correction 2026-10-28, review of
#339: `darwin-check` exited 1 on pzm at #339's head and on main. What
passed there is the retire holder test, which is no longer ignored on
Darwin, and the registry test of the holder check. #339 added no new
Darwin failure.) The Darwin `lsof` path has not been built or run: pzm is on hold and neo
is never a build host. The coordinator pair-builds the verb and runs it
with `--dry-run` first.
