# 2026-10-22 — homes pass: sessions-pull refused:IO, closure FIELD_DOMAIN_VIOLATION

Pass `homes-20261021` with build aa81fbc, run on sting. Receipts are in
`/srv/fast-local/jess/bulkload/receipts/homes-pass-20261021.{cmd,out,err}`.
The coordinator's ruling: "Accepted typed refusals and a socket never fail
sessions-pull. A bare IO gets a typed code. Reseat runs after the pulls with
refusals listed."

## What the output shows

### Pull outcomes

- `phase=sessions-pull outcome=refused:IO`.
- `phase=reseat outcome=skipped:sessions-pull-refused`.
- `phase=state ok`.
- There is no closure line. The process exited with
  `bulkload-agent: refused: FIELD_DOMAIN_VIOLATION`.
- The pull records for `.claude`, `.codex` and `.claude.json` all completed.
  Their path refusals are typed rows, which return ITEMS_REFUSED, and that
  counts as completed.
- The 377 codex file links completed. Each has one SOURCE_VANISHED entry.
- The 1004 claude stage directory links returned SOURCE_ROOT_MISSING, which is
  accepted. Each one still left a refused session record (TRANSPORT_LOST at
  `transfer::pull`), so each would be a closure row with no use.
- `ipc/ipc.sock` was refused FIELD_DOMAIN_VIOLATION. That is an entry row
  inside the completed `.codex` session, not a step failure.

### Where the IO came from

No IO line was printed. The only step that can fail without printing is the
key-map-unused note: `keymap::unused` calls `claude::first_cwd` on the first
listed transcript of each source-keyed directory.

Listed transcripts can be missing at their landed path, because the pull
refused them or a fold moved them on. For those, `open_nofollow` returns
`Io(Some(ENOENT))`.

Two facts support this:

- No `key-map-unused` lines were printed.
- Both listings were written.

The notes are a report only. The re-seat never reads `maps.unused`.

### Why closure refused FIELD_DOMAIN_VIOLATION

`session_pulls` joined every directory under `MOVE_STATE/pull` except
`plan-*`. That included `plan`, the pull of the source's ledgers, and
`corpus`, left by a retired pass. Both record the same transfer
(`eb8f60f1…`), so `joined_once` refused.

## Fix (branch fix/sessions-pull-typed-20261022)

- `keymap::unused` skips a listed transcript it gets ENOENT, ELOOP or ENOTDIR
  for, and tries the next transcript of that key. A key with no transcript
  left gets no note.
- Every sessions-pull step that refuses prints
  `estate-move sessions-pull step=<name> outcome=refused:<CODE> errno=<n>`.
- The key-map-unused step no longer holds the re-seat.
- A link pull that returns SOURCE_ROOT_MISSING deletes its pull directory and
  prints `source=absent (nothing to merge)`, as a file link already did.
- `session_pulls` joins only `claude`, `claude-json`, `codex`, `*-link-*` and
  `aside-*`.

This carries the vanished file-link change (2dd4ad8).
