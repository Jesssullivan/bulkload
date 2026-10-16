# 2026-10-16: `estate-reseat`, the R0 verb (TIN-6587)

Lane: estate-reseat build, worktree `bulkload.worktrees/estate-reseat-20261016`,
branch `feat/estate-reseat-20261016` off `origin/main` `0d96dbd`, on top of
the investigation note `35408b0`
(`docs/agent-notes/2026-10-16-reseat-investigation.md`). Linear TIN-6587
(Urgent). Spec: R0 of the draft `docs/SPEC.md` (PR #236). Rulings cited:
Q185 and Q192 (nothing dropped but truly recreatable content), R25 (live
host, never re-read), never overwrite user data, R-N11 (never signal another
session), R-N13 (this receipt).

## What was built

`bulkload-agent estate-reseat`: the contract is `docs/design.md`, "Agent
session re-seat (R0, TIN-6587)"; the SLO amendment is `docs/slo.md`,
"Amendment 2026-10-16: R0".

- `crates/bulkload-agent/src/reseat.rs` (options, lanes, summary) and
  `reseat/`: `key.rs` (Claude Code 2.1.299's project key, the destination
  real path), `json.rs` (a streaming top-level field scanner: `cwd`,
  `slug`), `ledger.rs` (`STATE/reseat.sqlite`), `place.rs` (the
  never-overwrite rules, intents, the seven crash points), `claude.rs`,
  `codex.rs`, `check.rs` (`--check`, the R0 acceptance check).
- Nine typed refusals (`RESEAT_*`, `crates/bulkload-proto`), three counters
  (`read_reseat_landing_bytes`, `read_reseat_dest_bytes`,
  `write_reseat_dest_bytes`), seven fault points (`reseat.*`).
- `estate-diff`'s summary now carries each planned workspace's class, which
  the check reads as the lane's Git state.

Defaults are the conservative answers to the five open decisions
(investigation note section 6); none is taken:

1. Destination cwd: the same path under the home's `git` (resolved to its
   real path); `--map` or `--lanes-from-plan` choose otherwise.
2. gftb / GloriousFlywheel: re-seated like any lane under `~/git`; on sting
   their `~/git` entries are symlinks into `estate/gftb-20261015`, so they
   key to that real path. D4b decides; `--map` can redirect.
3. sting's own 388 symlinked transcripts and ~9,242 Codex rows: untouched
   (a symlink at a placement's path is `DESTINATION_OCCUPIED`).
4. Existing Codex rows: insert only; a different row is
   `RESEAT_INDEX_CONFLICT`.
5. Home and `/private/tmp` sessions: `RESEAT_LANE_UNMAPPED` by default;
   `--unmapped=by-id` keeps them resumable by ID.

## Tests

Honest order: the tests were written after the module compiled, not run
red against a stub first. Failing-first is shown inside the rows instead:
each R0 row first asserts that the check fails on the bare landing (the
state the investigation measured on sting), then that it passes after a
pass; the CLI row does the same through the binary.

- `reseat::tests` (16 rows, synthetic transcripts and Codex stores in
  throwaway homes; the destination `git` is a symlink as on sting): R0 fails
  then holds; S3 rerun reads 0; a grown transcript extended by its tail
  only (bytes read bounded by tail + two windows); existing copies adopted,
  prefix-extended or kept; a destination resume hands the session over
  (`dest-continued`, then `both-continued`, never appended again); symlinks
  occupied; retention refused before anything is written; lanes (`--map`,
  unmapped, unresolved, a >200-unit hashed key, `--unmapped=by-id`); plans
  superseded through the exchange and a collision kept; verdicts need Git
  and the hand-off; Codex rows registered with the real destination cwd and
  path, links cleared, tools copied, a conflicting row never updated, a
  `.gz` kept, a required destination column refused; the key function's
  six investigation cases and its hash; the scanner.
- `tests/estate_reseat.rs`: the CLI (`--check` fails first, a pass, `--check`
  passes, rerun `read_reseat_landing_bytes=0 write_reseat_dest_bytes=0`,
  no-plan check `incomplete`, typed argument refusals), and the **real
  Claude Code canary**: the installed `claude --resume` in the test's
  destination home under `bwrap --unshare-net` on a `script` pty lists the
  re-seated session (cwd entered through the `git` symlink), does not list
  a symlinked transcript beside it, leaves the transcript unchanged, and
  exits on Escape. It skips (and says so) where `claude`, `bwrap` or
  `script` is missing.
- `tests/fault_harness/reseat.rs`: nine rows over the seven points (first
  pass at its first and last temporary, its first and third publish, its
  row commit; a grown transcript at the intent, mid-tail and after the
  tail; a changed plan after the exchange). Each: the crash left a prefix
  or the old/new plan, the rerun settles forward with every destination
  file equal to its landed one, no temporary, one Codex row with the real
  cwd, `--check` passing, and the next rerun reads 0 landed bytes.

## Validation (in `nix develop .#default`, `nice -n 10`, lane target dir)

- `cargo fmt --all -- --check`: clean.
- clippy, three passes (`--workspace --all-targets`; `-p bulkload-agent
  --features io-trace`; `--workspace --features
  bulkload-agent/fault-injection,bulkload-agent/io-trace`), `-D warnings`:
  clean.
- `cargo test -p bulkload-agent --locked`: 975 passed, 0 failed, 7 ignored
  (the canary ran the installed Claude Code 2.1.299, not skipped).
- `just fault-harness` (clippy, `fault_harness` 121 rows incl. the nine
  `reseat_*`, `power_loss` 18, `io::`, P5 partial write, resume power
  loss, the hand-off-off and delta-lever arms): pass.
  `every_fault_point_has_a_scenario` covers all 50 points.
- `just contract-test`: 24 tests OK. `just tla-render --check`: all 257
  committed files equal the catalogue's rendering.
- Fixed on the way: `--note-marker` was renamed `--note-marker`, because
  `no_credential_tools::the_agent_has_no_handoff_verb` holds USAGE free of
  the word.

## Real-data dry run (read-only on every live tree)

Landing: the 2026-09-30 neo landing
`/srv/data/jess/neo-transfer-20260930T164001Z/neo-fs/Users/jess` (3
transcripts, 1,680 files, 567 MB), read only. Destination: a throwaway
home `/dev/shm/reseat-lane/home-real` whose `git` is a symlink to
`/srv/fast-local/jess/git`. Ledger in `/dev/shm/reseat-lane/state-real`.

- Without `settings.json`: `RESEAT_RETENTION_UNSAFE`, nothing written, no
  ledger (the transcripts are from 2026-09-02..25).
- `--check --sessions-only` before: both lanes `fail` (`absent`).
- Pass 1: 3 sessions `placed`, 1,680 files, `read_reseat_landing_bytes=566888373`.
- Pass 2: all `unchanged`, `read_reseat_landing_bytes=0`,
  `write_reseat_dest_bytes=0`.
- `--check --sessions-only` after: `~/git/tummycrypt` lane `pass`; the
  `tummycrypt.worktrees/tcfs-bulkload-m1-20260902` lane `fail`
  (`dest-cwd-absent`: that worktree does not exist on sting).
- The installed `claude --resume` in `home-real/git/tummycrypt`, under
  `bwrap --unshare-net` with `/srv/fast-local/jess/git`,
  `/srv/fast-local/jess/state` and `/srv/data` bound read-only, listed both
  neo sessions (`bulkload-slo-engine-perf`, 31.8 MB;
  `tcfs-nix-post-reboot-fg-delta`, 47.9 MB); both transcripts' sha256 were
  unchanged after the picker.

## Mutation receipt (R-N13)

- This worktree: the code, tests, docs and this note.
- Writes only under `/dev/shm/reseat-lane/` (throwaway homes, ledgers,
  logs) and the tests' temporary directories. The live `~/.claude` and
  `~/.codex` of sting were only read (schema and format); neo was not
  touched by this session.
- Processes: only this session's own sandboxed `claude` children, each of
  which left on an Escape keystroke. No signal was sent to any process.
- Nothing pushed, nothing posted to Linear (the orchestrating workflow
  owns both).

## Suggested SPEC R0 amendment (for PR #236, not applied here)

Under "Acceptance", item 2: "The R0 check is `bulkload-agent estate-reseat
--check` (TIN-6587). It reads the landing, never the ledger, and never opens
a session in the live destination. A lane without its Git input is
`incomplete`, never a pass; the hand-off note is required once TIN-6569's
marker exists." Under "Lane mapping": "Re-seat inserts Codex thread rows
only; a different existing row is reported (`RESEAT_INDEX_CONFLICT`), never
updated."

## Open

- The five operator decisions above (and D4b).
- Not built: an additive merge of Claude `history.jsonl` and Codex
  `session_index.jsonl` / `history.jsonl` (neither picker reads them; they
  stay in the landing); a Codex picker canary (the investigation verified
  the row rule against the real binary; the check mirrors it); closure
  integration (re-seat rows are report rows, not closure items); a TLA
  model of the placement (`tla-render --check` unchanged).
- neo has two Codex rollout roots (`~/.codex` and
  `/Volumes/TinylandState/tinyland-state/codex`): pass both with
  `--codex-source-root` (the flag replaces the default). 3,519 `.jsonl.gz`
  rollouts are not re-seatable (`RESEAT_UNREADABLE_FORM`).
- A session also present under an old `-Users-jess*` key shows twice in
  the picker's all-projects view (Ctrl+A); `--resume <id>` prefers the
  current key.
- The live apply waits for the neo-full landing of `home/` (empty today)
  and for decision 1.
