# 2026-10-16: `estate-reseat` re-review fixes and OI-1003-Q198 (TIN-6587, R0)

Lane: estate-reseat, worktree `bulkload.worktrees/estate-reseat-20261016`,
branch `feat/estate-reseat-20261016`, PR #242, on top of `8a89f9a`.
Rulings cited: TIN-6587 (R0), OI-1003-Q198 (the operator's rulings,
Linear TIN-6587 comment `fbccc49b`), Q173, Q185 (nothing dropped), R25,
R-N11 (no process signalled; canaries leave on keystrokes), R-N13 (this
receipt). Builds: `CARGO_TARGET_DIR=/srv/fast-local/jess/cache/cargo-target/reseat`,
`TMPDIR=/dev/shm/reseat-lane`, `nix develop .#default`, `nice -n 10`. Every
test ran in throwaway homes; the real binaries ran only under
`bwrap --unshare-net`; the live `~/.claude` and `~/.codex` were not touched.

Two re-reviews of `8a89f9a`: data safety (FIX-FIRST: one MAJOR, three
MEDIUM, three LOW) and user outcome (MERGEABLE: four MINOR, one INFO).

## Failing first

Each finding has a row in
`crates/bulkload-agent/src/reseat/tests/findings/rereview.rs` (and the CLI
space row `cli_space_floor_refuses_before_writing` in
`tests/estate_reseat.rs`), written against the API `8a89f9a` had. With the
source at `8a89f9a` (the new code stashed), all ten failed for the
reviewers' reasons: `ds_1` placed the rowless rollout beside the
destination's `.zst` and row; `ds_2` was not refused (no lock); `ds_3`
returned `Err(SourceChangedAfterSnapshot)`; `ds_5` extended the archive
copy through its hardlink; `ds_6` passed R0; `uo_1` reread 125,204 landed
bytes; `uo_2` named `landing-absent`; `uo_3` had no row; `uo_4` carried no
`drafts/`; the CLI row placed at `--min-free-percent=100`. Then the fixes;
then every row passes.

## Per item

Data safety:

- **MAJOR, rowless rollout beside a destination row. Fixed.** The
  snapshot path's rule now applies to rowless rollouts too: a destination
  row for the thread (unless it names this placement) or another form of
  its rollout at the destination (`.jsonl.zst`) is `RESEAT_INDEX_CONFLICT`
  and nothing is placed (`ds_1`: scenario C, scenario B, and a form with no
  row). The "another form" check applies to snapshot threads as well.
- **MEDIUM, no lock, shared temporary. Fixed.** A pass `flock`s each
  destination agent root it writes (the directories themselves) and
  `STATE/reseat.lock`; a second run refuses `RESEAT_LOCKED` (new code)
  before writing. Temporaries carry the ledger's own token
  (`.<name>.<token>.bulkload-reseat`); a migrated version 2 ledger keeps the
  shared name for what it may have left in flight (`ds_2`).
- **MEDIUM, a truncated `.zst` aborted every pass. Fixed.** Any read error
  on a compressed landing, the decoder's `UnexpectedEof` included, is that
  file's `RESEAT_UNREADABLE_FORM`; a failed stage removes its temporary
  whatever the refusal; an extension a broken stream cuts short keeps its
  whole, hashed lines as this ledger's; an unreadable landed file is
  ledgered (`unreadable`) and not read again while unchanged (`ds_3`).
- **MEDIUM, no free-space check. Fixed.** The engine's rule
  (`--min-free-percent`, `DESTINATION_SPACE_INSUFFICIENT`) runs before
  anything is written, per destination file system, over an upper bound of
  the bytes a pass would write (a `.zst` at its frame's declared size,
  else counted; an owned placement's tail only; every symlinked
  transcript's target). Sting's state file system is under the default
  25% floor, so the live run must name its floor explicitly
  (`cli_space_floor_refuses_before_writing`).
- **LOW, hardlinked destination files. Fixed.** A destination file with
  more than one link is `occupied`, never adopted or extended, also after
  it was placed (`ds_5`: an archive hardlink and a landing hardlink).
- **LOW, `--check` passed an unplaced longer extra form. Fixed.** Every
  other landed form of a snapshot thread must be held by the destination
  rollout; a longer one fails the thread `extra-form-unplaced` (`ds_6`).
- **LOW, extend fstat/write race. Documented.** No conditional append
  exists; the window (two system calls per 1 MiB chunk) is detected after
  the write, the file handed over, `--check` fails it, nothing lost
  (`place.rs` header, design table row).

User outcome:

- **Minor 1, S3 rereads of refused sessions. Fixed.** Refused Claude
  sessions and Codex rollouts are ledgered with their recorded `cwd` (or
  none, then by landed size and mtime); retention reads no refused
  session's head; an unchanged rerun reads 0 (`uo_1`).
- **Minor 2, rollout outside the source root. Fixed.** `--check` names it
  `RESEAT_LANE_UNMAPPED:rollout-root` (the fix is `--codex-source-root`),
  never `landing-absent` (`uo_2`).
- **Minor 3, by-ID resume writes the source cwd. Fixed by Q198 D,
  documented.** Rowless rollouts get a row, so the lane picker lists them
  and resume asks nothing (real Codex canary); a row carrying the source
  `cwd` is named `row-cwd-is-source`; design.md says to choose "Use
  current directory" for a thread still row-less (`uo_3`).
- **Minor 4, other directories under a key. Fixed.** Everything under a
  project key that is no session's (`drafts/`, `vercel-plugin/`, orphaned
  sidecars) goes to the destination key with its memory (`Kind::Extra`); a
  key with no lane is named by a `key` line (`uo_4`).
- **INFO** (picker-hidden threads hidden at the source too, a subagent
  whose parent did not land): unchanged, recorded here.

## OI-1003-Q198

- **(1) cwd-only re-spelling. Implemented.** A destination row whose `cwd`
  resolves to the lane's canonical path but is spelled otherwise gets only
  that column updated in a `BEGIN IMMEDIATE` transaction, compare-and-set
  on the old value, with the old value ledgered first (`cwd_update`,
  `intent` then `applied`/`skipped`); a `row-update` line names both
  values. `--undo` restores the old value with the same compare-and-set;
  a row changed since is `undo-kept`; an undone row is not re-spelled
  again; `--no-respell` turns it off (`q198_1_*`; real Codex canary: the
  row was hidden from the lane picker before and listed after; undo puts
  the spelling back).
- **(2) regularization. Implemented.** `projects/<key>/*.jsonl` symlinks in
  real key directories become regular-file byte copies of their targets
  (mode and mtime kept), exchanged in atomically and settled by the
  target's and copy's identities, the archive kept, ledgered
  (`regularized`). No reflink: P84 SOURCE-NO-CLONE (OI-1003-Q165) forbids
  every remap call in non-test code, so "reflink when possible" is never
  possible here. `--undo` puts the link back over an unchanged copy and
  keeps one written since; `--no-regularize` turns it off (`q198_2_*`;
  real Claude Code canary: not listed as a symlink, listed once
  regularized).
- **A, B.** The default lane and `realpath` already give `~/git/<name>` and
  sting's gftb/GloriousFlywheel estate links (`q198_ab_*`).
- **C.** The source home itself and `/private/tmp`, `/tmp` are by-ID by
  default (`--by-id DIR`, `--no-default-by-id`) and in no lane
  (`q198_c_*`).
- **D.** Rows built from the rollout header (`session_meta`, first
  `turn_context`, first prompt), only when the destination has no row or
  form for the thread (`uo_3`, `r1_6` updated; real Codex canary: listed
  in the lane picker, resumed by ID with no working-directory question,
  appended).
- **E.** Unchanged (`.zst` placed decompressed, zstd decoder).

Ledger: version 3 (`meta`, `cwd_update`, `regularized`, session
`l_size`/`l_mtime`, placement state `unreadable`); version 2 is migrated in
place and readable read-only before migration (`q198_a_version_2_*`).

Fault rows: five new points (`reseat.after_synth_row_insert`,
`after_cwd_update`, `after_regularize_exchange`, `after_undo_cwd`,
`after_undo_exchange`), five new rows (`Reseat::Q198`, `Reseat::Undo`):
each crash settles forward on the rerun, then S3 holds.

Also fixed on the way: the canaries' screen reader ended an OSC only at
BEL, so Codex's ST-terminated colour queries swallowed its whole screen
and the existing Codex canary's "failed to resume" check could never
fire; it now ends at BEL or ST, and the pty is sized (Codex draws nothing
on 0x0).

## Validation

`just check-fast` exit 0 on the final tree; reseat unit rows 51 (34 before),
fault-harness reseat rows 16 (11 before), `tests/estate_reseat.rs` 6 (3
before: the CLI rows and four real-binary canaries, all run, none skipped:
Claude Code 2.1.299, codex-cli 0.163.0-alpha.11).

## Re-review of fix round 2: two MEDIUM (finishing the PR)

A re-review called round 2 mergeable with two MEDIUM findings; both fixed,
each with rows in `reseat/tests/findings/q198.rs` (`q198_2_rr_*`).

- **MEDIUM 1, regularize race. Fixed.** Regularizing could orphan or
  delete a copy a live session appended to during it. Now: the copy is
  read back from its name before the swap and must be the bytes read
  (length and BLAKE3); the target's identity (inode, size, mtime, by
  descriptor and path), the link's and the copy's are re-checked just
  before the exchange and the target again just after it. Nothing holding
  a session's bytes is removed or replaced unless its bytes are held
  elsewhere: on a mismatch the transcript is `RESEAT_REGULARIZE_RACED`
  and both copies stay (the staged copy kept aside as
  `.<name>.<token>.kept-<inode>.bulkload-reseat`, named by the line's
  `kept` field, never removed by a pass; a copy written under the swap
  stays at the path). A test-only hook (`regularize::set_hook`) acts as
  the live session: an append between the read and the swap
  (`q198_2_rr_append_between_read_and_swap_keeps_both_copies`), and
  appends under the swap, target only and target plus copy
  (`q198_2_rr_append_under_the_swap_keeps_both_copies`).
- **MEDIUM 2, undo safety. Fixed.** `--undo` verifies, before it puts a
  link back or removes the regular file, that the link text still resolves
  to the recorded target and that it is a regular file holding the
  recorded bytes (the ledger now keeps the BLAKE3 of the copy: column
  `regularized.digest`, added in place to an existing version 3 ledger),
  unchanged up to the swap. Otherwise the row is `undo-refused`,
  `RESEAT_UNDO_TARGET_CHANGED` (new code), and nothing changes for it
  (`q198_2_rr_undo_verifies_the_archive_target`: a target rewritten in
  place at the same length, and one moved away, are refused with the
  regular file and the `done` row kept; moved back, it is undone).

Residual window, documented in design.md: a session that opened the link
before the swap and writes after the last check appends to the archive.

## Open

- The live run: sting's floor under `--min-free-percent` 25 needs an
  explicit operator floor; `--check` will still fail threads whose
  destination row names sting's own rollout (conflicts by design).
- Merge is the operator's (not merged here).
