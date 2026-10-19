# 2026-10-19: beside landings converge and adopt by proof (gap C12)

Lane: plan of record rev 2 (tummycrypt
`docs/agent-notes/2026-10-19-bulkload-reset-review.md`, gap table row C12);
operator ruling A-115, 2026-10-18: "Landings placed beside refuse every
later pass once neo changes" → "Yes, converge them". Branch
`feat/beside-adopt-converge-20261019` from `origin/main` `01be8c9`. Edited
on sting; every build and test ran on honey
(`~/git-build/beside-adopt-converge-20261019`, target
`~/git-build/target-beside-adopt-converge`,
`TMPDIR=/var/tmp/jess-bulkload-tests/beside-adopt-converge-20261019`,
removed after). Nothing ran against the live estate. R-N13
(lab R-HOOK-CONVERGENCE-20261004).

Requirement served (docs/SPEC.md R2.2, verbatim): "a diverged DEST copy is
set aside intact, with its unpushed branches imported create-only
(LIVE-32), or diffed and preserved before it moves forward (LIVE-33)", and
"a rerun replaces a DEST file only when it is bulkload's own output,
unchanged since it wrote it (LIVE-30)"; R2.3: "A live lane is caught up by
repeated passes" (ONCE-01, LIVE-04, LIVE-07).

## The failure this closes

`estate-move --label neo-20261016 --exclude-union` on sting (agents
`01be8c96efab`, MOVE_STATE `/srv/fast-local/jess/estate/inplace-stage/move/gf01`)
refused six GloriousFlywheel items `GIT_DESTINATION_OCCUPIED`:

- `GloriousFlywheel`, `GloriousFlywheel-arc-11ace`,
  `GloriousFlywheel-gf-rbe-contraction-20260817`, `glorious-build-infra`,
  `glorious.build`: the live checkout's adopt proof refused
  `GIT_DESTINATION_DIVERGED` (diff.jsonl `adopt=diverged`), so the item
  went beside, and `<live>.neo-20261016` already held a standalone
  checkout with a workspace ledger under the same SOURCE label
  (`neo-inplace-20261016`) naming **another item**: the in-place lane's
  10-17 hand plan had landed it with `estate-apply` as an item of its own
  (source → beside path), and estate-handoff had written its blocks there
  on 10-18 (`handoff.landed`). `beside_item` refused any ledger naming
  another item `GIT_DESTINATION_OCCUPIED` (diff.jsonl
  `beside=occupied:<path>`).
- `GloriousFlywheel-mms-dind-pin-20260831`: no beside involvement. Its
  live path `/srv/fast-local/jess/git/GloriousFlywheel-mms-dind-pin-20260831`
  is a symlink (made 2026-10-06 13:27) to
  `/srv/data/jess/archive/fast-local/git/GloriousFlywheel-mms-dind-pin-20260831`,
  whose checkout carries a `neo-inplace-20261016` workspace ledger naming
  item `2350cbc4…` (not the plan's `e1658e0c…`). The move plan decided
  `action=refuse reason=symlinked-path` (`estate/moving.rs` `action`, no
  `--side-root`), the apply ran anyway, and the A11b re-key refused the
  symlink spelling (`converge/adopt.rs` `foreign_shape`:
  `canonicalize(worktree) != worktree` → `symlinked-path`), which
  `adopt::rekey` returns as `GIT_DESTINATION_OCCUPIED` before any proof.
  diff.jsonl: `class=source-ahead baseline=ledger adopt=symlinked-path`.
  The beside path is absent because the beside step only follows a
  `GIT_DESTINATION_DIVERGED`. Not changed here (see "Open").

## What changed

`estate-apply --adopt --beside SUFFIX` (and so `estate-move`) now lands a
diverged item beside on every pass. `git_carry::beside::land` reads the
beside path before anything is written and takes it one way:

| Beside path holds | Before | Now |
| --- | --- | --- |
| nothing | first linked landing | same; one this item's `.beside` record says it landed and that is gone is never laid down again (`GIT_DESTINATION_MODIFIED`; diff `beside=retired`) |
| this item's landing, this capture | present | present |
| this item's landing, older capture | `GIT_DESTINATION_OCCUPIED` every pass | converged (diff `beside=converge`): proved against its ledger first, an edit there refuses `GIT_DESTINATION_MODIFIED` with nothing written; a linked one stays detached and never steps the live repository's refs (`Place::beside`) |
| another item's landing (the GF case) | `GIT_DESTINATION_OCCUPIED` | re-keyed by A11b after its proof, then converged (diff `beside=rekey`) |
| standalone checkout, no ledger | adopted if equal (HEAD, index, exclude, seats), else `GIT_DESTINATION_OCCUPIED` | adopted if equal, the stash compared too, a bulkload hand-off block at byte 0 of `AGENTS.md`/`CLAUDE.md`, sealed as estate-handoff renders it, the one difference allowed (`beside-handoff-block`); else `GIT_BESIDE_FOREIGN` |
| anything else | `GIT_DESTINATION_OCCUPIED` | `GIT_BESIDE_FOREIGN path="…"` (new code, carries the path), nothing there written |

- **Refs.** On a later pass a namespace ref this item created (its
  `.beside` or `.refs-in` record) that still holds the value it created is
  moved to the new capture's value by compare-and-swap
  (`beside-ref-moved`); one it created that is gone is not recreated
  (`beside-ref-retired`); any other differing ref is kept and refuses the
  item as before (B4). LIVE-30: only bulkload's own unchanged output moves.
- **Refs-in for a refused beside.** When the beside landing refuses (any
  code), `--refs-in` still imports the item's branches, tags and stash
  entries (`refs/<ns>-stash/<n>`) create-only. The item stays refused
  pending review; closure prints `refs_in` (`ns`, `created`, `kept`) on
  the refused row, and `refusal_detail` (the path) on a
  `GIT_BESIDE_FOREIGN` row.
- **Standalone stays standalone (the brief's choice).** A standalone beside
  checkout is adopted, re-keyed and converged in its own repository and
  never converted to a linked worktree. Converting would rewrite its
  `.git`, move its objects, branches and stash into the live repository
  and change what `git worktree list` shows there; the proof needs none of
  that, and the live repository gets the capture's provenance and the
  namespace refs either way.
- No wire frame change (refusal codes travel as strings); no new crate or
  dependency; closure stays `bulkload.closure.v5` with two added row keys.

Files: `crates/bulkload-proto/src/refusal.rs` (`GitBesideForeign`),
`crates/bulkload-agent/src/git_carry/beside.rs` (`land`, `plan_own`,
`write`, `stash_wanted`, `ledger_of`), `git_carry/converge.rs`
(`Place::beside`, the recovered-landing converge), `converge/adopt.rs` and
`converge/handoff.rs` (the beside adoption proof), `converge/diff.rs` and
`estate/diff.rs` (diff verdicts), `estate.rs` (`beside_item`, refs-in on a
refused beside, the ledger's `refs_in`), `closure.rs`, `main.rs` help,
`docs/design.md`.

## Tests

New rows (fixtures created and destroyed by the tests, R-N98):

- `tests/estate_adopt.rs` P115 (converge of an earlier beside landing;
  then an edit in the beside checkout refuses `GIT_DESTINATION_MODIFIED`
  and stays), P116 (adopt of an equal foreign standalone beside, stash
  included; the next pass converges it in its own repository), P117 (adopt
  with only a hand-off block difference; a block over edited bytes and a
  malformed block refuse `GIT_BESIDE_FOREIGN` with nothing written), P118
  (`GIT_BESIDE_FOREIGN` naming the path for a differing seat and for a
  stash of its own; closure `refusal_detail`), P119 (refs, the stash
  entry included, imported despite a refused beside; closure `refs_in`),
  P122 (the GloriousFlywheel shape: a hand-plan standalone landing with
  estate-handoff blocks, re-keyed and converged), P123 (a retired beside
  landing is not laid down again; diff `beside=retired`), P124 (a gitfile
  naming a gone or another repository's gitdir is `GIT_BESIDE_FOREIGN`),
  P125 (stash entries decided by commit).
- Fault harness: `estate_apply_beside_converge_after_{intent,seat,index,head}`
  (a crashed beside converge settles forward, no native ref of the live
  repository moved); `estate_apply_beside_rekey_after_{intent,seat,head}`
  (the same for a re-keyed standalone hand-plan landing);
  `estate_apply_beside_stale_capture` now converges.
- Changed rows: P111 and P112 expect `GIT_BESIDE_FOREIGN` (diff
  `beside=foreign`) where they expected `GIT_DESTINATION_OCCUPIED`;
  `tests/estate_move.rs`'s converge pass expects the beside item
  `applied-beside` (`beside-converged=1`) instead of refused.

Runs on honey before the commit (lane worktree, TMPDIR above): clippy
`-D warnings` (default and `fault-injection,io-trace`), `estate_adopt` 38
passed, the `beside` fault-harness rows 7 passed, `estate_move` 1 passed,
`estate_carried` 12 passed, `closure` 36, `beside` 8 and `handoff::render`
5 library tests passed, `bulkload-proto` 18 passed. The full
`cargo test -p bulkload-agent --locked` and `just rust-check` on the
pushed head are in the PR body with their host, head and TMPDIR.

One library test fails on honey under `TMPDIR=/var/tmp/...` and is not this
lane's: `materialize::tests::the_ext_magic_reads_its_mount_type_by_major_and_minor`
(`materialize.rs:3583`, "an ext4 mount of this device batches"). It fails
alone and single-threaded too, and the same way in the other 2026-10-19
lanes' honey logs (`single-file-final-6541ffd3cf4d`,
`sqlite-slot-budget-r3-nff`, `beside-live-home-final-faddd661`), while
some of their earlier runs pass: it depends on the host's mounts, not on
this diff (`materialize.rs` and `io/` are untouched here). The PR body
records it against `origin/main` as well.

## Review

One adversarial read-only review (a refuter subagent; nothing run). Its
findings and what was done:

| # | Finding | Disposition |
| --- | --- | --- |
| 1 | a refused beside's refs-in recreated namespace refs the operator removed with a retired beside | fixed: refs-in plans with `plan_own` over both records and never recreates (`refs-in-retired`); P123 `--refs-in` variant |
| 2 | the hand-off allowance accepted any well-formed block, an edit inside one included | fixed: the block must be sealed (`render::sealed`: its `id=` is the blake3 of the block with the id blanked); P117 `edit-inside`, `unsealed`; a later converge that replaces the seat drops the block, as it drops a recorded one |
| 3 | a re-keyed linked beside could settle another item's pending attach intent and move a live branch | fixed: a linked checkout with a pending workspace intent is never re-keyed (`converge::rekey_check`, `GIT_BESIDE_FOREIGN`) |
| 4 | a refs-in a crash cut short left its refs pending, so the beside pass read them as kept | fixed: settled in memory before the own set is built |
| 5 | `plan_own` matched any value a name ever held | fixed: one value per name (`own_refs`, the `.beside` record's over the `.refs-in` one's); refs-in upserts |
| 6 | a refused re-key took the lock and swept probes before refusing | fixed: `rekey_check` runs read-only in `land` first |
| 7 | diff and apply disagreed on a re-key the apply refuses | fixed: diff runs `rekey_check` (`foreign`); the linked-without-ledger-but-with-landing-intent case still reads `foreign` in the diff while the apply finishes it (open) |
| 8 | the refused beside's refs-in is not in the receipt | open: closure's `refs_in` names it; the refusal's receipt is the refusal |
| 9 | tests: `contains` matched `=10`; no edit-inside or retired-refs rows | fixed: exact word matching, P117 and P123 rows; a CAS conflict on a move and a third party moving a namespace ref between passes are not tested (open) |

## Second review (coordinator, PR #282 comment, 2026-10-19T06:07Z, at `20c8981`)

Verdict there: no data-loss findings; F1 must be fixed before the live run.

| # | Finding | Disposition |
| --- | --- | --- |
| F1 | a beside directory whose `.git` gitfile names a gitdir that is gone (the two `/Users/jess/...` dirs, any copy of a pruned worktree) ended as a bare `IO (errno 2)`: unaccounted, not dispositionable | fixed: `beside::linked_into` resolves the gitfile, the gitdir and its common directory read-only before anything else; a gone gitdir or another repository's common directory is `GIT_BESIDE_FOREIGN`; P124 (`gone`, `other`) checks nothing is written and closure has 0 unaccounted |
| F2 | stash refs are named by index but retired and moved by name, so a stash commit the operator deleted came back under another index | fixed: `beside::stash_wanted` decides stash entries by commit: a commit this item put under `refs/<ns>-stash/` that no ref there holds now is left out, the rest renumbered (beside landing) or new ones put at a free index (refs-in); `plan_own` never retires a stash entry by name; P125 (beside converge and refused-beside refs-in) |
| F3 | `plan_own` recreates an absent namespace ref this item never created (another item's, or the 10-17 hand refs-in's), even one the operator deleted | gap: needs every item's records for the repository (items of one repository share its refs); additive only, no ref moved or lost |
| F4 | `rekey_refusal` checks only the current plan, so two live plans naming the same `<X>.LABEL` could re-key it back and forth | gap: each re-key still needs its full ledger proof first, so no edit is lost; one plan per MOVE_STATE is the run discipline tonight |
| F5 | an `OCCUPIED`/`DIVERGED` raised inside `apply_workspace` after `landed::lock`, `settle_repository` and `sweep_probes` is mapped to foreign, so under a race a lock file and ledger directory get created inside a foreign `.git` | gap: only the post-lock race re-checks (another apply adopting the same checkout at that moment, or a change between the proof and the lock) reach it; every decision `land` can make read-only now runs before the lock (`linked_into`, `rekey_check`, the adoption proof); what the race leaves is bulkload's empty lock file and ledger directory, no user byte; removing them is unsafe while the other apply holds that lock |
| F6 | the new fault rows covered only the linked beside converge; tonight's shapes are standalone | fixed: `estate_apply_beside_rekey_after_{intent,seat,head}` (a hand plan's standalone landing at the beside path, re-keyed, its converge crashed; settled forward, still standalone, no live ref moved) |

## Open

- `GloriousFlywheel-mms-dind-pin-20260831` (and every symlinked live
  path): not a beside case and unchanged here. Resolving a symlinked live
  path to its real path (or `--side-root` seating it) is its own gap; the
  plan already says `refuse reason=symlinked-path`.
- C12's first half: a beside landing under `--exclude-union` whose live
  shared `info/exclude` lacks captured lines still refuses
  `GIT_IGNORE_POLICY_CONFLICT` (a beside never appends to the live
  exclude). The plan's suggested fix (a standalone beside checkout with
  the captured exclude) is not built; refs still go in under `--refs-in`.
- The 19 ledgerless standalone `*.neo-20261016` directories on sting and
  the two linked ones whose `.git` names a `/Users/jess/...` path: they
  hold older neo state than the current capture, so they will refuse
  `GIT_BESIDE_FOREIGN` (named) and wait for closure-dispose; their refs go
  in (before F1's fix the two linked ones ended as a bare `IO`). Those
  refused items get only their refs on this pass, not the source's newest
  tree bytes: the tree waits for the operator's review (a requirement gap
  the second review named, not a defect). The 56 hand-plan landings with
  hand-off records (the GloriousFlywheel shape) re-key and converge when
  their proof holds.
- An adopted beside checkout whose hand-off block bulkload did not record
  (the foreign-adoption allowance) is proved by its print on later passes;
  a later content check (racy print, another file system) reads the block
  as a difference and refuses `GIT_DESTINATION_MODIFIED`, never losing it.
- Refs-in itself stays create-only: a refs-in ref the source moved since
  is kept and counted (`refs-in-kept`), as before. A beside landing on a
  later pass moves this item's own refs-in refs, since they are its own
  unchanged output.
- `estate-move`'s per-item plan line still prints `beside=` only for
  carry-beside items; what the apply does at an existing beside path is in
  the plan's `estate-diff` preview (`beside=converge|rekey|existing|
  foreign|retired`). Left so to stay out of the `estate_move.rs` regions
  two other lanes own.
- The live run (sting, gf01) was not re-run from this lane (nothing runs
  against the live estate from a code lane); it needs this build deployed
  first.
