# 2026-10-24: bounded shallow estate consolidation

Authority: the operator's 2026-10-24 request to complete bulkload and move
the lab, blahaj and remaining resumable estates from neo to sting.
SPEC goal 1 and rules 1, 4, 7 and 8 apply. R-N13 requires this receipt.
Lexical: XL-ACC-004.

The source capture refused every common repository containing `shallow`.
That also refused every linked worktree using that common administration.
The streamed snapshot now carries the validated boundary as metadata.
Objects still travel through the existing Git pack stream directly into
the live destination store. No upstream fetch or source deepening occurs.

Supported destination cases are absent administrations, equal shallow
boundaries and full repositories that already prove all source boundary
ancestry. The last case remains full. Differing boundaries remain typed
`GIT_HAVES_UNPROVABLE` holds. No destination history is truncated.
An owned landing with the same boundary resumes. A changed frontier is
held until its non-destructive consolidation has its own tests.
A source that becomes full also holds while the destination retains its
old frontier. It cannot report success with that history still hidden.

Frontiers bypass ordinary seat transfer. A new frontier is published
create-only after object connectivity is proved. A concurrent frontier
change refuses before checkout and ref consolidation. Temporary frontier
metadata shares the destination administration's filesystem.

Snapshot records use an explicit version prefix. The old postcard layout
still decodes as a full repository. Empty-frontier snapshot and fetch
digests remain unchanged, preserving existing full-source resume marks.
Shallow fetch marks include the boundary. The source and destination
executables must be updated together for capture negotiation messages.

Validation is pending on honey. No build or test ran on sting or neo.
The new integration tests cover shallow landing, linked worktrees, dirty
and staged state, stash, unchanged reruns, equal boundaries, richer full
destinations and typed frontier holds. Unit tests cover old records and
digest compatibility. Darwin validation remains a separate receipt.

## Follow-up from the first Linux and Darwin runs

Both hosts passed eight shallow tests and failed two. One assertion wrongly
required source freshness after the destination already held the captured
full graph. Its replacement proves complete history, zero transferred
objects and an unchanged full destination.

The second failure exposed a real rerun conflict. Hand-off appended its
entry to the destination's `info/exclude`. The ordinary transfer then
refused that longer file. Exclude bytes now join the existing snapshot
metadata. The transfer still carries the file normally, including its
mode. It keeps a longer destination only when all source bytes remain a
prefix, the exact hand-off suffix remains and the recorded hand-off
proves the current local block. Both sets of patterns stay untouched.
An unrecorded marker, changed block or missing source bytes remains held.
The proof covers the captured source prefix, not future source writes.
A fresh capture that adds an absent source pattern remains held.

Native landing accepts the original plan identity for hand-off provenance.
Home mapping changes paths without changing that identity. Compatibility
callers retain the existing landing API. Exclude and local-block reads
use nonblocking, no-follow opens and require regular files before reading.
FIFO replacements remain untouched and return a typed policy conflict.

The final check of a skipped exclude now requires a real parent directory
and a regular file that still contains the captured source bytes. A longer
file must retain its proved hand-off provenance. If the transfer observes
a concurrent deletion, landing holds and leaves the deletion untouched.
The controlled-transfer regression removes the skipped file during landing.
Authority remains SPEC rules 4 and 7, the operator's 2026-10-24 direction
and R-N13. No local build or test accompanied this postcondition fix.

This follow-up has not been built or tested. The coordinator owns honey
and PZM validation. No completion or resumed-session claim follows.

| stream | owner | repo/branch | state/evidence | next |
| --- | --- | --- | --- | --- |
| shallow estates | Codex | bulkload/codex/streamed-shallow-20261024 | source and fixture tests, unvalidated | honey checks and independent review |
