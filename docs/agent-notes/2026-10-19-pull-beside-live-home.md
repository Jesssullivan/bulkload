# 2026-10-19: `pull --beside`, and `estate-move --sessions` into the live home

Lane: plan of record 2026-10-19 (tummycrypt
`docs/agent-notes/2026-10-19-bulkload-reset-review.md`, revision 2), Step C
gaps C1, C2, C9, C10, C11. Operator rulings served: "no intermidiary stage
patterns" (2026-10-19), "no work should ever be clobbered on sting from neo"
(2026-09-21), "refs in, tree beside" (2026-10-16). Branch
`feat/pull-beside-live-home-20261019` from `origin/main` `01be8c9`. Edited on
sting; every build and test ran on honey in this lane's own directories
(`~/git-build/beside-live-home-20261019`, target
`~/git-build/target-beside-live-home`; test scratch
`TMPDIR=/var/tmp/jess-bulkload-tests/pull-beside-live-home-20261019` on
honey's root xfs, removed after the run; never `/dev/shm`, R-C856). R-N13.

Host fact: honey `/home` is 88% full, below the engine's 25% free-space
floor, so `tests/estate_reseat.rs::cli_check_fails_first_then_passes_and_rerun_reads_nothing`
refuses `DESTINATION_SPACE_INSUFFICIENT` with its scratch there (a
pre-existing dependence of that test on its host, not this change). With the
scratch on `/var/tmp` it passes.

## The requirement served

SPEC R2.2, quoted: "DEST is live too, and is never clobbered. [...] A run
never overwrites, resets or deletes DEST work that SOURCE does not hold
(LIVE-15, LIVE-18, KEEP-08) [...] Where both sides changed, the run keeps
both: DEST's refs, dirt, sessions and rows stay, SOURCE's land beside them
[...] Every run is a lossless union into DEST [...] never a mirror, and never
clobbering DEST (C10; LIVE-15, LIVE-18, CONF-05, KEEP-36)." SPEC R4, quoted:
"On DEST each tree lands once, at its live path (FOOT-18, PUR-20). There are
no duplicate landing archives, stale run directories or rollback copies left
behind without an operator ruling." SPEC "Lane mapping", quoted: "Sessions
are re-seated under DEST's project key for that cwd, in DEST's live
agent-state directory, not only in a landing archive (RES-10, RES-12,
RES-19, L1)." Acceptance 1, quoted: "The closure report shows 0 unaccounted
and every refusal is typed and dispositioned (SEAT-12, `slo.md` S4)."

## What was built

- `pull [--beside SUFFIX]` and `copy [--beside SUFFIX]` (C1, C2). Before:
  an existing destination file bulkload did not write whose bytes are not
  the seat's was refused `DESTINATION_OCCUPIED`, pending review, and never
  placed; a database with a `-wal`, `-shm` or `-journal` beside it was
  refused at Decide. Now, under the flag, the seat's bytes are published
  at `<path><SUFFIX>` with the live file untouched (byte for byte and by
  inode), a differing symlink is created beside, and a database with a live
  sidecar lands its snapshot at `<db><SUFFIX>` (the sidecar check moves to
  the beside path; the snapshot is still verified `integrity_check ok` in
  journal mode DELETE). A byte-identical file is still adopted; a verified
  byte prefix of an append-only file is still extended in place (M2r2
  A-103); a file the extension proved not to be a prefix after all (its
  straddling chunk differs) lands whole beside, from the proven prefix,
  re-hashed as it is copied, and the staged tail. A beside path that holds
  something of its own is refused `DESTINATION_OCCUPIED` (a foreign file
  that happens to hold the seat's bytes is adopted); only this store's own
  earlier beside output is superseded, by the WP0(d) exchange. Each beside
  placement is recorded under the seat's beside row (`row_key` of the beside
  path; the keys of a path share its prefix, so the snapshot key follows),
  so an unchanged rerun reuses it with 0 source bytes (S3) and keeps
  accounting it. The transfer record gains an `applied-beside` entry per
  seat, naming the beside path; `closure-report --transfer` classifies it
  `applied-beside` (closure v5's disposition, counted in `applied_beside`),
  accounted and never pending review; the row prints `beside`. Without the
  flag every answer is as before. The flag leads the verb's positional
  arguments (`copy --beside .neo-20261019 SOURCE DEST ...`); the suffix is
  one name component. New counter `dest_applied_beside`; new stats line
  field `applied_beside=` and `applied-beside PATH at=BESIDE` lines.
- `estate-move --sessions` (C9, C10, C11). Before: both homes were pulled
  into `MOVE_STATE/landing`, an intermediary stage, and `estate-reseat`
  moved sessions only, so settings, `history.jsonl` and plugins never
  reached the live home; a pull that refused any path skipped the re-seat;
  the gate ignored the session pulls and the re-seat. Now phase 6 pulls
  `SOURCE_HOME/.claude` and the Codex root with `--beside .LABEL` into the
  live `DEST_HOME/.claude` and `DEST_HOME/.codex` (created 0700 when
  absent, else taken as they are), records in `MOVE_STATE/pull/{claude,codex}`;
  the re-seat runs whenever both sessions completed, `ITEMS_REFUSED`
  included (refused paths stay typed closure rows); its landing is the live
  home itself, and the landed Codex index is `state_5.sqlite.LABEL` when the
  live index was kept (`estate-reseat --landing-codex-index FILE`, new).
  Phase 7 joins the two transfer records into the closure report
  (`join_transfers`: `applied-beside` accounted, refused paths pending
  review, a transfer that never ran unaccounted) and holds the gate on any
  re-seat outcome that leaves a session not resumable and not kept beside
  (`reseat::word_accounted`; `Summary::unaccounted`). The closure line adds
  `sessions=`, `transfer_rows=`, `reseat_unaccounted=` and `reseat_held=`.
- `estate-reseat` on a live-home landing (the landed tree is the live
  tree, so every file there is listed). Claude: a transcript this ledger
  placed on an earlier pass is an output and is skipped unread; one already
  under the key Claude Code lists it by in its lane (its own key, or its
  lane's destination key) is `in-place`, ledgered, nothing written, no
  `RESEAT_LANE_UNMAPPED`/`UNRESOLVED`. Codex: a rollout the destination
  index already names, or one whose recorded `cwd` is not under the source
  home and has no lane, is the destination's own and `in-place`. Only
  sessions and rollouts landed from the source with a lane elsewhere are
  placed and registered, as before. Found by the rerun of the estate-move
  test: before this rule, pass 2 re-read the transcript pass 1 placed as a
  landed session and refused it `RESEAT_LANE_UNRESOLVED`, and every
  sting-own rollout would have held the gate as `RESEAT_LANE_UNMAPPED`.

## Tests

- `transfer::tests::beside`: a differing file lands beside and an identical
  one is adopted (and the rerun reuses, and a changed seat supersedes the
  beside output); without the flag the differing file is still refused; a
  differing symlink is created beside; a database with live sidecars lands
  its snapshot beside (and is refused without the flag, nothing read); a
  grown prefix is still extended and a false prefix lands whole beside; a
  foreign file at the beside path is refused or adopted, never replaced; the
  transfer record names the entries and round-trips; the suffix rule.
- `closure::transfer_tests::an_applied_beside_entry_is_accounted_and_gate_relevant`.
- `tests/estate_move.rs::sessions_land_in_the_live_home_beside_and_the_gate_reads_them`:
  the live-home landing end to end through the binary, with the destination's
  own session, a live Codex index with a sidecar, the destination's own
  indexed and rowless rollouts, a refused path (C10) and a passing rerun.

## Receipt

Host `honey`, head `cf1166bde850cbad676449048ebec400b3db4e76` (the pushed
branch, checked out detached in `~/git-build/beside-live-home-20261019`),
`CARGO_TARGET_DIR=~/git-build/target-beside-live-home`,
`TMPDIR=/var/tmp/jess-bulkload-tests/pull-beside-live-home-20261019`, every
command under `nice -n 19 nix develop --no-write-lock-file
--ignore-environment .#default`. Logs on honey:
`~/git-build/logs/beside-live-home-testall-cf1166b.log`,
`~/git-build/logs/beside-live-home-rustcheck-cf1166b.log`.

- `cargo test -p bulkload-agent --locked`: exit 0; 39 test binaries,
  1233 passed, 0 failed, 8 ignored (lib: `test result: ok. 924 passed; 0
  failed; 6 ignored`).
- `just rust-check`: `cargo fmt --all -- --check` ok; `cargo clippy
  --workspace --all-targets --locked -- -D warnings` ok; `cargo clippy -p
  bulkload-agent --all-targets --locked --features io-trace -- -D warnings`
  ok; `rust-test`: every group (1-unit, 2-integration-a-e,
  3-integration-f-g, 4-integration-rest, 5-doc) ended with status 0, "all
  42 test executables ran, in 5 groups", 1277 passed, 0 failed, 9 ignored;
  `just rust-check` exit 0.
- `just darwin-touched`: yes (counters.rs, materialize.rs, reseat.rs carry
  platform cfgs already; this diff adds none). `just darwin-check` on pzm is
  owed before merge (OI-1003-Q199), not run in this lane.
- The commit that adds this receipt changes only this Markdown file over
  `cf1166b`.

Earlier run at `faddd661` (the first commit) on the same host: 922 passed,
2 failed in the lib. `materialize::tests::the_ext_magic_reads_its_mount_type_by_major_and_minor`
collided with the TMPDIR device (honey `/var/tmp` is 253:0, the fixture's
hardcoded root), fixed by `cf1166b` (test only).
`reseat::tests::findings::space::a_regularization_keeps_the_placements_bytes_reserved_then_retries`
failed once: it reserves all but half a 64 MiB link against a live probe of
free space, and honey's root filesystem moved more than that between probe
and check under load. It passed in the targeted run before and in the
`cf1166b` run, and the code it drives (`regularize`) is untouched here: a
host-load flake, recorded, not fixed in this lane.

## Review of #280 (2026-10-19) and the fixes

The coordinator's adversarial review of `0e08334` (comment on #280, probes
on honey) found the defects below. They are fixed on this branch in new
commits (no force-push); the receipt above is for `cf1166b` and is
superseded by the one added with the fixes.

1. **Blocking: a live path was replaced under `--beside`.** An "owned" live
   file (adopted identical, or published, on an earlier pass) whose seat
   changed was exchanged into place: the destination's bytes were lost, and
   a published rollout held open by a destination writer was replaced, so
   the writer's next line went to the unlinked inode. Fixed: under
   `--beside` ownership of a live path licenses nothing. The file is judged
   as any other: equal is adopted, a grown append-only file is extended
   through the `O_APPEND` prefix path, anything else lands beside. Both
   reproductions are tests
   (`an_adopted_live_file_is_never_replaced_when_its_seat_changes`,
   `a_published_append_only_file_held_open_by_a_writer_is_extended_in_place`).
   Without the flag, WP0(d)'s supersede of the store's own unchanged output
   is as before.
2. **Blocking: the PR body was another lane's** (it would have closed
   #232, #276, #277, #279). The shared scratchpad body file had been
   overwritten by another lane before `gh pr create`. Rewritten from this
   note and the commits, from a lane-unique file; it closes no issue.
3. **Unchanged reruns re-read the source.** A beside landing refused for
   a foreign file at the beside path is now remembered under the seat's
   beside key (`RefusedOutput::BesideOccupied`, new tag 4) with that file's
   settled identity, beside the live path's own record; an unchanged rerun
   refuses it with 0 source bytes (test
   `a_refused_beside_landing_is_remembered_and_reads_nothing_again`).
4. **Beside-name collision aborted the session.** Each suffixed name a
   session writes is claimed once (a seat's own live path or another seat's
   beside path); a second claim is that seat's `DESTINATION_OCCUPIED`,
   typed, and the session completes. Beside rows carry a key mark, so only
   this seat's own earlier beside output is ever superseded at a beside
   path; the source's own `x<suffix>` output is never replaced with `x`'s
   bytes (test `a_beside_name_the_source_also_holds_is_a_typed_refusal_of_one_seat`).
5. **Kind mismatch was bare `IO`.** A destination open that meets a symlink
   or a non-directory (`ELOOP`, `ENOTDIR`) is `DESTINATION_OCCUPIED`,
   typed. Under `--beside` a regular seat over a symlink (sting's Home
   Manager `~/.codex/AGENTS.md`) lands beside, the link untouched; a
   directory over a symlink is refused typed with its children (landing a
   whole subtree beside is a gap, below). Test
   `a_seat_over_a_destination_symlink_lands_beside_or_refuses_typed`.
6. **The Codex re-seat could read the live index as the snapshot.** Now:
   `estate-move` makes the Codex index seat always land beside
   (`state_5.sqlite.LABEL`, live path absent included), so a pull never
   writes the live index; the re-seat gets `--landing-codex-index` only for
   the snapshot this pass's completed Codex transfer record names
   (`applied-beside`, started this pass), else `--no-landing-codex-index`
   (new): a stale snapshot is never reused. `estate-reseat` refuses
   `FIELD_DOMAIN_VIOLATION` before writing when the landed index resolves
   to the live one. A destination with no index gets one seeded from this
   pass's snapshot (`VACUUM INTO` a temporary, its thread rows removed,
   published without replacing anything), then each lane's thread is
   registered under the destination `cwd` and rollout path; every other
   table, `backfill_state` included, is kept, so Codex never backfills the
   landed rollouts under their source `cwd`. Seeding needs a snapshot whose
   `backfill_state` holds a row (its Codex completed the backfill it runs
   once); without one the run still refuses `RESEAT_INDEX_ABSENT` before
   writing, as review 0 finding 4 ruled (`r0_4_...` unchanged). Test
   `sessions_into_a_destination_with_no_codex_index_seed_it`.
7. **Destination-own sessions held the gate.** On a live-home landing, a
   transcript under a key that is not under the source home's key is the
   destination's own and `in-place`, with or without a `cwd` its key
   agrees with (same test as 6). When both homes key alike (one home path
   on both hosts) nothing is taken for the destination's by its key alone.
8. **`append_tail` could split a record under a racing appender.** Every
   piece of a prefix extension now ends at its last newline (a record
   longer than a piece is written whole), one `O_APPEND` write each, so a
   destination append between two pieces never lands inside a record of
   ours; the size checks before and after each piece are unchanged.
9. **Neo-key project directories stay inside the live `~/.claude`.** Not
   fixed in this PR (gap below).

## Second review of #280 (2026-10-19, at `2290e26`) and the fixes

Verdict of the second review: no path overwrites or loses live bytes, but
it could not run on sting's real homes. `origin/main` (`9854236`, #281 and
#282) is merged with a merge commit first. Each item below has a
regression test built on sting's shape.

- **N1: sting's `~/.claude` and `~/.codex` are symlinks** to
  `/srv/fast-local/jess/state/{claude,codex}`. `estate-move` resolves a
  symlinked live home once (`canonicalize`), requires a real directory this
  user owns, and runs the transfer under the real path, following no further
  link. An `estate-move sessions-home name=… live=… real=…` line names it,
  and the transfer record's destination is the real path. Test:
  `sessions_into_a_destination_with_no_codex_index_seed_it`, now with both
  homes symlinked.
- **N2: the destination's own archive links** (366 transcripts and 14,992
  rollouts on sting link into `/srv/data/jess/archive`). On a live-home
  landing they are never read or written through.
  - `estate-reseat --beside-suffix SUFFIX` (new; estate-move passes
    `.LABEL`) tells the re-seat how the pull named the source's copy landed
    beside a link.
  - Claude: a symlinked transcript with the source's copy beside it
    (`<id>.jsonl.LABEL`, or under `<key>.LABEL/`) leaves that copy as the
    landed session. The copy is placed at the lane's live name, and when
    that name is a link into a `--replace-archived-dest` root it replaces
    the link (A-106, existing placer rule; the archived fork stays).
    Without the root it is `occupied` and holds the gate. A link with no
    copy beside it is the destination's own and is `in-place`.
  - Codex: the same rule through `Options::landed_codex`. A link with no
    copy beside it is `landing-absent` for the snapshot's thread, and
    `in-place` when unindexed.
  - `estate-move --replace-archived-dest ROOT` (new) forwards the root.
    Symlinked key directories on a live landing are the destination's own
    and are not entered.
  - Test: `archive_links_follow_a106_and_destination_links_stay`.
- **N3: a racy capture's beside output** recorded only the shared
  ownership row, so after the source changed it was refused as a stranger's.
  Its row now also carries a beside mark (`BESIDE_RACY_MARK`, in `outputs`,
  under a key no reuse lookup asks for). It is never reused, and a later
  pass supersedes it. Test: `a_racy_beside_output_is_still_this_seats_own`.
- **N4: an unmapped source session passed as in place.** Restored
  `RESEAT_LANE_UNMAPPED` (typed, reviewable) for a source-keyed session
  with no lane; only the destination's own sessions are in place. Test:
  the archive test asserts `reseat_held=claude/RESEAT_LANE_UNMAPPED:1`.
- **N5 (the Codex half of finding 7):** on a live landing, the
  destination's own `.jsonl.gz`, its oddly named files and other forms are
  `in-place`. So is a snapshot thread whose landed form is `.gz`: the bytes
  are carried at the live name, and Codex reads no `.gz` on either host
  (TIN-5268). Test: the archive test.
- **Finding 5, directories:** under `--beside`, a directory whose live path
  holds a symlink or another kind (sting's 275
  `~/.claude/file-history/<uuid>` links into a repair tree) lands beside as
  `<dir><SUFFIX>/`, accounted `applied-beside`. Every entry under it is
  decided there, and the link is never followed. Live-mode re-seat skips
  companions already at their own name (the per-ID trees), so it never
  walks a linked `file-history/<id>` for a write. Test:
  `a_seat_over_a_destination_symlink_lands_beside_or_refuses_typed`.
- **Finding 9, no duplicate bits: not in this PR; the next lane's spec.**
  The pull *can* map neo project keys to destination keys at pull time, so
  that nothing lands under a neo key. It is a receive-side change to how
  each entry's destination path is keyed and recorded, so it gets its own
  PR and review:
  1. `pull`/`copy --map-prefix SRC_REL=DST_REL` (repeatable; receive side,
     wire unchanged). An entry at `SRC_REL` or under `SRC_REL/` is decided,
     keyed (`row_key` over the mapped row), recorded and published at
     `DST_REL` plus the rest. Matching is by whole path components, and the
     longest prefix wins. Two maps whose destinations nest or coincide
     refuse `FIELD_DOMAIN_VIOLATION` before the session.
  2. The transfer record names the source path and the mapped path (a
     `mapped` entry outcome, accounted like `applied`). A mapped seat whose
     destination holds something else follows `--beside` as any seat does;
     two seats mapped onto one name in a session follow the finding-4 claim
     rule (the second is typed `DESTINATION_OCCUPIED`).
  3. `estate-move --sessions` builds the maps from the plan's lanes:
     `projects/<key(S)>` becomes `projects/<key(D)>` for each lane `S → D`,
     only when no other source working directory under SOURCE_HOME shares
     `key(S)`. Keys are lossy, so the source's `projects/` names are
     checked against the lanes, and an ambiguous key stays unmapped and is
     re-keyed by the re-seat as today. The per-ID global trees need no map.
  4. The re-seat then finds each mapped session `in-place` (lane key =
     directory key) and copies nothing. Transcript bytes and `cwd` are
     unchanged (C8).
  5. Tests: a neo key mapped to the lane's destination key with no
     `-Users-jess-*` directory created on the destination; byte-identical
     transcripts; a rerun that reads 0; an ambiguous key that stays and is
     re-seated.
  6. The 80 neo-key directories already on sting are outside it: removing
     a transcript is forbidden (C8, C9). They stay, reported as residue.

## Receipt after both reviews

Host `honey`, head `7d986f59f5be28340d0c099140dfa56fc2a429c0` (the pushed
branch: the review fixes on the merge of `origin/main` `9854236`),
checked out detached in `~/git-build/beside-live-home-20261019`.
- Environment: `nice -n 19 nix develop --no-write-lock-file
  --ignore-environment .#default`, with `CARGO_BUILD_JOBS=4`,
  `RUST_TEST_THREADS=4` and debuginfo off (honey load 70–110, another
  session's production build sharing it).
- `CARGO_TARGET_DIR=~/git-build/target-beside-live-home`.
- `TMPDIR=/var/tmp/jess-bulkload-tests/pull-beside-live-home-20261019/tmp`,
  removed after the run.

- `cargo clippy -p bulkload-agent --all-targets --locked -- -D warnings`:
  ok.
- `cargo test -p bulkload-agent --locked`: exit 0. 39 test binaries, 1251
  passed, 0 failed, 8 ignored (lib: 931 passed, 6 ignored). Log
  `~/git-build/logs/beside-live-home-testall-7d986f5.log`.
- `just rust-check`, second run: exit 0. fmt check ok; workspace clippy
  `-D warnings` ok; `io-trace` clippy ok; rust-test groups 1 to 5 status
  0, "all 42 test executables ran, in 5 groups", 1295 passed, 0 failed, 9
  ignored. Log `~/git-build/logs/beside-live-home-rustcheck-7d986f5.log`.
- The first `just rust-check` at the same head failed one test in group 2.
  `estate_adopt::p90_the_gftb_shapes_adopt_and_converge_with_bulkload_verbs_only`
  asserts the bytes re-verified for seats written in their ledger's mtime
  tick, which moves with load (24 against 57). The same test passed in the
  `cargo test` run above and in the second `rust-check`, and it exercises
  git adopt, which this diff does not touch.
- #282's estate-move converge test failed once, at `96cdde0`, the same
  way (a first capture racy or not by timing), and passed at `7d986f5`.
- `just darwin-touched`: yes (platform cfgs already in the files touched);
  `just darwin-check` on pzm is owed before merge. The merge gate
  (`just local-gate-receipt --pr 280 --post`) has not been run.

## Open

- **Finding 9 (no duplicate bits, R4):** the source's transcripts land in
  the live `~/.claude/projects/<neo key>/` and the re-seat copies each
  re-keyed one to `<destination key>/`, so the neo-key copy stays as a
  second copy. The right shape is a pull that re-keys at the destination:
  a receive-side prefix map (`projects/<neo key>/` →
  `projects/<destination key>/`) that `estate-move` builds from the plan's
  lanes for keys only one source `cwd` maps to (the key is lossy; reseat
  keeps resolving the rest by each transcript's `cwd`). It touches row
  keys, ownership, beside paths, parent creation and the reseat's ledger,
  so it is its own PR; not done here. Removing or linking the neo-key copy
  instead is ruled out (C8/C9: no transcript removed, none symlinked; the
  placer refuses hardlinks).
- Re-keyed transcripts leave the neo-key copy in the live `~/.claude`
  (finding 9, spec above), and an A-106 replacement keeps the source's
  `<id>.jsonl.LABEL` copy beside the placed one: both are duplicate bytes
  until the `--map-prefix` lane lands.
- The second review notes that A-106's open-descriptor guard may not see
  an open-append-close writer: a running destination session's next lines
  can land in the placed copy (not lost, but no longer byte-identical).
- Re-seat outcomes that hold the gate (`reseat_held=`) are not yet
  dispositionable by `closure-dispose`; whether an operator review may close
  one is a dated question for the operator, not decided here.
- Beside outputs never converge back into the live path (C12 for files,
  as for checkouts): a later pass keeps them beside, current.
- `just darwin-check` on pzm is owed before merge.
