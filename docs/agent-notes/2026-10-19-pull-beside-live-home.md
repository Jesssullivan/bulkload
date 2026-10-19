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

## Open

- The Codex re-seat on a destination with no live `state_5.sqlite` (nothing
  to land beside, so the pull lands the source's snapshot at the live name)
  then reads that same file as the landed index. Reseat refused that
  destination before too (`RESEAT_INDEX_ABSENT`); the real estate's Codex
  index is live with sidecars, which is the case built and tested here.
- Re-seat outcomes that hold the gate (`reseat_held=`) are not yet
  dispositionable by `closure-dispose`; whether an operator review may close
  one (for example an unmapped neo scratch cwd) is a dated question for the
  operator, not decided here.
- Beside outputs never converge back into the live path (C12 for files,
  as for checkouts): a later pass keeps them beside, current.
