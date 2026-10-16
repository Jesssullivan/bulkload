# 2026-10-16: `estate-reseat` review fixes (TIN-6587, R0)

> Superseded in part (2026-10-16, later): rollouts with no snapshot row now
> get a row from their header (OI-1003-Q198 D, not `RESEAT_INDEX_ROW_ABSENT`
> and `row-absent` as below), the ledger is version 3, and the re-reviews
> of `8a89f9a` are closed in
> `2026-10-16-estate-reseat-rereview-fixes.md`.

Lane: estate-reseat review fixes, worktree
`bulkload.worktrees/estate-reseat-20261016`, branch
`feat/estate-reseat-20261016`, on top of the build `550396c` and the
investigation `35408b0` (off `origin/main` `0d96dbd`). Linear TIN-6587
(Urgent). Spec: R0 of the draft `docs/SPEC.md` (PR #236). Rulings cited:
Q185 and Q192 (nothing dropped but truly recreatable content), R25 (live
host, never re-read the source), never overwrite user data, R-N11 (never
signal another session), R-N13 (this receipt).

Two adversarial reviews refuted `550396c` as safe for the live run (16
findings: 7 from review 0, real binaries; 9 from review 1, placement and
crash rules). Every finding was reproduced first: each has a row in
`crates/bulkload-agent/src/reseat/tests/findings.rs` (or the CLI and canary
rows in `tests/estate_reseat.rs`), and all 16 rows were run against the
unchanged `550396c` code and failed for the reason the review gave, before
any fix (output kept at the time in `/dev/shm/reseat-fix-failfirst.txt`;
the reasons are summarised per finding below). Then the fixes; then every
row passes.

## Per finding

Review 0 (real binaries):

1. **Paginated `.jsonl.zst` threads do not resume (high). Fixed.** Every
   landed `.jsonl.zst` rollout is placed **decompressed as plain `.jsonl`**
   and its row names that path; identity is the decompressed stream. Fail
   first: the placed file was `.zst`, the row named it. Real binaries:
   sting thread `019f901f` (paginated, `.zst` only) re-seated into a
   throwaway home from the reviewer's landing: `codex resume` (0.163.0-
   alpha.11, `bwrap --unshare-net`) opened it and appended 1,620 bytes, the
   placed bytes an exact prefix; `--check` then `pass` (`continued`). The
   control (same thread placed as `.zst`) fails with "failed to scan
   paginated model context lineage". A new canary
   (`canary_real_codex_resumes_a_reseated_paginated_thread`) does the same
   with a synthetic paginated thread in an index the installed Codex makes
   itself; the same synthetic thread placed as `.zst` fails the same way.
   Decompressing every `.zst` (not only paginated) also fixes finding 5 and
   costs destination space: neo's 6,118 + 1,776 `.zst` (4.6 GB) are about
   5–7× larger decompressed (two real samples: 4.8× and 7.1×).
2. **`--check` passes vacuously (high). Fixed.** A landed `.claude` or
   `.codex` that is not a directory (absent, or a symlink to a source-only
   volume) refuses `RESEAT_LANDING_ABSENT` for the run and the check unless
   `--no-claude` / `--no-codex`; each prints a `tool` line; a check that
   checked nothing fails. Fail first: `Ok` and `r0:pass`.
3. **Companions of an occupied transcript are written (high). Fixed.**
   Companions are placed only when the transcript outcome is placed,
   adopted, extended or unchanged. Fail first: sidecar, file-history and
   plan were written. On a mirror of live sting's tummycrypt key (its
   symlinks as symlinks) with the 2026-09-30 neo landing: 3 sessions
   `occupied`, **0 companions written** (`write_reseat_dest_bytes=0`), the
   memory notes 18 adopted / 9 kept diverged, `--check` fails both lanes
   (`not-regular`, and `dest-cwd-absent` for the m1 worktree). The build's
   "tummycrypt lane pass" used an empty `projects/` and did not reflect live
   sting.
4. **No destination index locks threads out (medium). Fixed.** A landing
   with Codex threads and a destination with no `state_5.sqlite` refuses
   `RESEAT_INDEX_ABSENT` before anything (Claude's half too) is written.
   Fail first: rollouts placed, `index-absent` rows.
5. **Source-side compression duplicates the rollout (medium). Fixed** by
   (1): one destination form; the landed form change is the same stream (the
   ledger row follows the landed path, the decompressed stream is proven
   equal). Also: a destination row this ledger did not register is never
   given a second rollout beside it. Fail first: two rollouts, a conflict.
6. **R0 ignores unmapped, unresolved and by-ID sessions (medium). Fixed.**
   Each is an `unlaned` line and fails R0 unless `--waive ID` /
   `--waive-from FILE` names it. Fail first: `r0:pass`.
7. **`--check` fails after any destination resume (low). Fixed.** A
   destination that holds the landed whole lines and more (a resume) is
   `continued` and passes; so is a rollout Codex compressed at the
   destination (`rollout_path` the `.zst`). A copy missing landed bytes still
   fails. Fail first: `size-differs`, `RESEAT_R0_FAILED`.

Review 1 (placement and crash rules):

1. **Concurrent destination append spliced mid-line (high). Fixed.**
   Extensions write whole lines only and check the destination's size before
   every write; a writer beside them stops the pass (`RESEAT_EXTEND_RACED`,
   the destination's from then on, mtime not reset). Fail first (the
   reviewer's race, 24 MiB tail): a line `...ppp{"type":"user","from":"sting-resume"}`.
   After: every destination line is a landed line or the writer's, 3/3.
2. **Supersede crash then in-place edit overwritten (high). Fixed.** The
   intent stores the staged file's whole identity; recovery accepts the
   exchange only when the path still holds exactly it, removes only a
   displaced file that is this ledger's placement, and hands an edited file
   over. Fail first: the user's edit was replaced by the landing.
3. **Partial last line merged with a resume's first append (medium).
   Fixed.** `.jsonl` files are placed and extended only up to the landed
   last newline (the rest held back until whole); `--check` flags a
   destination transcript ending mid-line (`ends-mid-line`). Fail first:
   the destination ended mid-record.
4. **Two source keys on one destination key ping-pong memory (medium).
   Fixed.** A placement row names its landed file; another landed file for
   the same destination path is `RESEAT_LANE_COLLISION` (a `file` line),
   never superseded, read 0 bytes on reruns. Fail first: superseded every
   pass, 47 bytes read each time.
5. **By-ID failure passes R0 (medium). Fixed** with review 0 finding 6.
6. **Rollouts with no snapshot row never reported (medium). Fixed.** The
   landed rollout trees are walked; a rollout no snapshot row names is placed
   (resumable by ID), its lane from its `session_meta` `cwd` (cached), and
   reported `RESEAT_INDEX_ROW_ABSENT`; `--check` fails it `row-absent`.
   Another form of a named thread is an `extra-form` line. No row is made up
   (operator decision below). Fail first: no line at all.
7. **Retention point-in-time, user settings only (medium). Fixed.** The
   smallest `cleanupPeriodDays` of managed settings (and `.d` drop-ins),
   user `settings.json` / `settings.local.json` and every lane's
   `.claude/settings{,.local}.json` must keep every file the pass would place
   (transcripts, sidecars, per-session files, plans of a known slug) past
   now plus `--retention-margin-days` (default 365). Fail first: a 29-day
   transcript under the default 30 was placed. Claude Code 2.1.299 reads
   managed settings at `/etc/claude-code/managed-settings.json` (+ `.d/`) and
   merges every source (`Ub`: `(er()||{}).cleanupPeriodDays`).
8. **Window proof makes hybrids (low). Fixed.** The row keeps the BLAKE3 of
   every placed byte; growth re-proves all of them before appending (a
   change anywhere is `RESEAT_SOURCE_REWRITTEN`). Cost: a grown transcript
   is read once from the landing (the destination's own copy), not two
   windows. Fail first: `extended`, a hybrid.
9. **Snapshot reads not counted (low). Fixed.** The snapshot's threads are
   cached in the ledger with its identity; an unchanged snapshot is not
   opened, a changed one counts its size. Fail first: the rerun opened the
   (unreadable) snapshot and refused.

Also: the ledger now carries `user_version` 2; the first build's ledger is
refused `SCHEMA_MISMATCH` (no live ledger exists). New refusal codes:
`RESEAT_LANDING_ABSENT`, `RESEAT_INDEX_ABSENT`, `RESEAT_INDEX_ROW_ABSENT`,
`RESEAT_LANE_COLLISION`, `RESEAT_EXTEND_RACED`. New dependency: `zstd`
0.13 (decoder only, no default features) for the `.zst` rollouts. Two new
fault-harness rows (a `.jsonl.zst` rollout crashed after its temporary and
after its publish).

## Validation

In `nix develop .#default`, `nice -n 10`, the lane's own target directory,
`just check-fast` exit 0: repository contract checks, gitleaks (no leaks),
`cargo fmt --check`, the three clippy passes (`-D warnings`), `rust-test`
(all 34 test executables in 5 groups; agent lib 783 passed, 5 ignored, with
`reseat::` 34 rows; `estate_reseat` 3 incl. both real-binary canaries, not
skipped), `fault-harness` (`fault_harness` 123 rows incl. 11 `reseat_*`,
`power_loss` 18, the `io::`, P5, resume power-loss, hand-off-off and
delta-lever arms), `contract-test` (24 OK). `just tla-render --check`: all
257 files equal.

## Operator decisions (none taken; defaults conservative)

1. Destination cwd per lane: `~/git/<name>` by default.
2. gftb / GloriousFlywheel sessions (D4b).
3. **Reframed:** neo's own sessions blocked at their destination key by
   sting's symlinked transcripts (on live sting, neo's two tummycrypt
   sessions `3025961c`, `bb510cc3` and the m1 worktree's `752101b2` are
   `occupied`; the investigation counts 10 such under `-srv-fast-local-*`),
   besides sting's own 388 symlinked transcripts. R0 cannot pass for those
   lanes until the symlinks are replaced by regular files (a ruling; TIN-5268's
   hold).
4. Existing Codex rows: insert only (sting holds 11,433 of neo's IDs; those
   threads are `RESEAT_INDEX_CONFLICT` and fail R0 until ruled).
5. Home, `/private/tmp` and other unmapped cwds: they fail R0 unless waived
   (`--waive-from`), or mapped (`--map`), or kept by ID.
6. **New:** rollouts with no snapshot row (253 on neo): placed and reported;
   synthesise a row from `session_meta` (as Codex's backfill does), or waive?
7. **New:** the destination space for decompressed `.zst` rollouts (about
   25–30 GB for neo's 4.6 GB).
8. **New:** the `zstd` crate in the agent's dependency set.

## Mutation receipt (R-N13)

- This worktree only: code, tests, docs, this note; one commit, signed, on
  `feat/estate-reseat-20261016`, pushed, PR opened (not merged).
- Throwaway state only under `/dev/shm/reseat-lane/fix-*` and the tests'
  temporary directories. The live `~/.claude` and `~/.codex` of sting were
  only read (the tummycrypt key's entries, to mirror its symlinks); neo was
  not touched. The 2026-09-30 neo landing and the reviewers' `/dev/shm`
  landings were only read.
- Processes: this lane's own sandboxed `codex` and `claude` children (left
  with keystrokes). One sandbox Codex app-server daemon this lane started
  (HOME `/dev/shm/reseat-lane/fix-c1/home`) was stopped with that home's own
  `codex app-server daemon stop`; later runs used `--no-daemon`. No other
  process was signalled.
- Nothing posted to Linear by this lane (the orchestrating workflow owns
  the TIN-6587 comment).
