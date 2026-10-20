# 2026-10-20: no-staging guards on the move verbs (PR A)

Lane: the operator's 2026-10-20 reset, relayed by the coordinator. On neo,
every `estate-move-source` item's private state held two full copies of
its live repository: a `capture.bundle` (about 2 GB) and a
`repository.git` (about 2 GB). Beside them was `carry/corpus`. That was
about 140 GB per batch set and about 400 GB across runs, all deleted the
same day with a receipt. The operator: "this pattern can never happen
again." The reset SPEC rule 2 (its SPEC PR is separate): "Neither machine
ever gets a staging copy: no capture bundle, repository copy, corpus, side
root, landing area or overflow directory, on any drive. bulkload's private
state holds ledgers and resume marks only, never payload." R-N13.

Branch `feat/streamed-estate-capture-20261020`, from main `b8fb5ab`.
Edited on sting. Every build and test ran on honey, `nice -n 19`, jobs 4,
inside the gate's devShell, in this lane's own worktree, target and
`TMPDIR`.

## Why two PRs

Today's capture is built on bundles.

- Every working-tree seat (dirty, untracked, ignored) is hash-object'ed
  into the private `repository.git` (`objects-written`). The synthesized
  staged and worktree trees and the metadata blobs go with them, all
  written as `capture.bundle` and published to `carry/corpus`.
- The destination pulls that corpus over the file engine. It imports the
  corpus under `refs/carry/v1/...` and checks out the raw trees.

Replacing that (objects streamed from the live repository into the live
store, segment marks, haves negotiation) is a rewrite of capture, apply
and the wire. It is PR B. PR A (this note) stops the pattern now, and the
coordinator accepted the split: the git estate passes wait for B, and the
homes pass does not use capture.

## What PR A does

- `state_payload` is the guard.
  - `retired` names each retired staging format in a state directory. It
    names the format itself and does not descend into it. The formats:
    `carry/corpus`, `landing`, a capture attempt directory under
    `private/`, any `repository.git` and any `*.bundle`.
  - `measure` classifies every file. A file is a ledger or a resume mark
    only when the module's table names it:
    - the move's own ledgers;
    - the plan and pass marks;
    - the estate item records in `private/`;
    - the transfer and re-seat stores under `reseat/`, `carry-files/`,
      `pull/` and `serve/`;
    - `*.pending-N` publications.

    Anything else is payload. That includes symlinks and every file inside
    a retired format. Payload is never let through by size.
  - `refuse_staging` refuses a target under a state directory.
  - `resume_mark_bound(items)` is 8 MiB plus 64 KiB per item: the bound
    the tests hold a pass's ledgers to.
- Two typed refusals:
  - `STATE_PAYLOAD_LEFT`, naming a path: a step would stage payload in a
    state directory, or a pass left some there.
  - `STATE_FORMAT_RETIRED`, naming a path: refused at startup, before
    anything is read or written.

  `BulkloadRefusal::code` would have passed 100 lines, so the `RESEAT_*`
  arms moved to `reseat_code` (the same split as #293's), and the
  code-table test refuses an empty code.
- `estate-move-source`:
  - It checks for retired formats first.
  - It then inventories and plans, as before.
  - It refuses its capture `STATE_PAYLOAD_LEFT` before writing, for any
    non-empty plan. An empty plan completes.
  - Every pass ends with `estate-move-source phase=state outcome=...
    state_payload_bytes= state_payload_files= state_ledger_bytes=`. Each
    payload file is named on its own line, and any payload left refuses
    `STATE_PAYLOAD_LEFT`, which outranks the pass's own result.
- `estate-move`:
  - It checks its own state for retired formats, and also the source
    half's when `HOST` is `-`.
  - It refuses its corpus pull before any transport. The ssh stand-in
    test records that no ssh ran.
  - The session landing is refused the same way.
  - The same state line ends every pass, and the closure line carries the
    same words.

## Tests (honey, gate devShell, clippy 0.1.96)

- `cargo fmt --all -- --check` is clean. All three gate clippy passes are
  clean with `-D warnings`: the workspace, agent `io-trace`, and the
  workspace with `fault-injection,io-trace`.
- `bulkload-proto`: 18 passed.
- Agent lib: `estate::payload::`, `estate::moving::`, `outcome::` and
  `closure::` passed, 60 in all. The 5 payload unit tests cover:
  - the ledger table both ways;
  - retired formats named and counted as payload;
  - a ledger-only state passes, and one stray file or symlink is refused;
  - staging under a state is refused, and a live path passes;
  - a missing state holds nothing.
- `tests/estate_move.rs` (4), each through the binary:
  - the source half plans and refuses to stage, and its state holds no
    payload within the bound, twice;
  - the destination half refuses its pull, with and without `--plan-only`,
    and no live path changes;
  - retired formats are refused at startup by both halves, each named,
    and nothing is written (not even the lock or the plan);
  - a stray payload file is named and refused, and once it is removed by
    literal path the next pass completes.
- `tests/estate_move_source_roots.rs`: 5 passed.
- `refusal_taxonomy`: 5 passed. `source_command_registry`: 22 passed.
  The scan read a `&["pull", ...]` list as Git arguments, so the
  store-homes table is no longer led by `pull`.
- Full `cargo test --workspace --locked`: see the PR.

## What waits for PR B

These proofs wait for the streamed capture, which brings them back over
live-store transfers:

- P-MOVE-1..3 (the landing, adopt, beside and carry proofs this lane
  first deflaked in #290);
- the #276 serve-state proofs over ssh.

The lower-level verbs (`estate-capture`, `estate-apply` from a corpus,
`git-export`, `estate-carry-files`, `pull` into a directory the caller
names) are not guarded here. They still write bundles where they are told
to. PR B replaces their payload path.

## Rulings

- SPEC rule 2, reset 2026-10-20, operator-requested. It supersedes the
  older readings that kept the bundle path:
  - OI-1003-Q15/Q44: v1 bundles are the Git engine.
  - OI-1003-Q56: carry_v2 retired, and the W6 Git sub-stream with it.
  - Q193 as narrowed by C4: drop a batch's payload after a verified
    apply.
- R-N13: this note.
