# 2026-10-03 WP0(e) estate-shaped corpus

Rulings: OI-1003-Q19 (WP0(e): an estate-shaped S1 corpus beside R23's),
OI-1003-Q23, OI-1003-Q35 (Sprint 2 measures S3 byte counters and the rusage
CPU ratio on this corpus), R-N13.

- **Branch:** `feat/wp0e-estate-corpus-20261003`, from `origin/main` 57030e1.
- **Worktree:** `bulkload.worktrees/wp0e-estate-corpus-20261003`.
- **Push:** pushed to `origin`; no PR (a PR is not in this lane's scope).

## Incident: duplicate lane copies in one worktree

As the coordinator reported, several copies of this lane's agent ran at
the same time on 2026-10-03, all in this one worktree. The coordinator also
reports that all of them have ended. Verified from git and the file system:

- **They made the first four commits.** 58b1c2e (22:07:17 -0400), 8bead15
  (22:07:24), f01155d (22:21:09) and 7b75b26 (22:21:49) are all signed and
  were all pushed. They are not rewritten here.
- **The earlier note was wrong.** It said that "a concurrent agent" made
  the first three commits "from this lane's work". As the coordinator
  reports, every one of the four came from a copy of this same lane. The
  earlier note also said the branch was not pushed, which stopped being true
  once 7b75b26 was pushed.
- **Rulings lines differ.** f01155d ends `Rulings: OI-1003-Q35,
  OI-1003-Q18, OI-1003-Q19, R-N13`; the other three end `Rulings:
  OI-1003-Q19, OI-1003-Q23, R-N13`. They are left as made.
- **One copy left unfinished work.** It left about 160 uncommitted lines in
  `estate_corpus.py`:
  - `build_history`, `Scale.history` and `HISTORY_REPO`, not wired into
    `generate`;
  - receipt and host-path constants that nothing used;
  - a half-written host-path scan in `manifest()`, whose helper was never
    called.
- **A guard refusal under R-N101.** The earlier note recorded one: the
  process-control hook refused a copy's self-audit grep for process-control
  words. Under R-N101 that copy dropped the audit, did not reformulate it,
  and continued. The pre-commit process-safety audit stays the authoritative
  wording check (R-N92).
- **Superseded identities.** The copies recorded `b3645454…` (small) and
  `0bd1104e…` (estate), before the history repository was wired in. Both are
  superseded, and no archive copy of them exists.
- **Scratch from the copies is untouched.** `$TMPDIR/wp0e-check-fast-2.log`,
  `$TMPDIR/wp0e-msg/` and `$TMPDIR/wp0e-owner/` are left as they were.
- **One writer from now on.** The final session was the only writer here.
  It kept the sound parts of the leftover diff and finished them (below).

## Done

- **`crates/bulkload-bench/scripts/estate_corpus.py`** is a deterministic,
  sealed generator of an estate-shaped corpus. It uses stdlib Python, the git
  CLI, the `sqlite3` module and the `zstd` CLI from devShell stdenv. Its
  subcommands are `generate`, `verify`, `manifest`, `mutate DEST N` (the S3
  knob), `seal DEST` and `selftest`.
- **Final session** (on top of 7b75b26):
  - **#48 history repository.** `build_history` is wired into `generate`, so
    `git/history-heavy` is part of the corpus at both scales. At estate scale
    it has 600 commits, with a new 9 MiB `model.bin` revision every 4th
    commit, held in four packs plus loose objects.
  - **umask.** Every subcommand sets umask 022; `mutate` used to inherit it.
  - **Host-path scan.** Every manifest pass scans every file up to 64 MiB,
    and every symlink target, for the host's absolute paths. The only
    exception is the documented, normalised worktree `gitdir` fields. A hit
    fails `generate`, `verify`, `mutate` and `seal`.
  - **Per-class counts.** `SEAL.json` carries `counts.classes`, which
    partitions the entries into 13 estate classes.
  - **`seal DEST`.** It makes a copy read-only (0444/0555), records
    `readonly_identity` and writes `VERIFY-RECEIPT.json`. From then on,
    `verify` checks the receipt.
  - **Self-test.** It covers all of the above, including that `verify`
    fails on a sealed copy after `mutate 1`, one added file and one removed
    file. It runs in about 20 s.
  - **Identities re-recorded,** with the archive copies generated in place
    and sealed. Details are in the evidence doc.
- **just recipes:** `bench-estate-corpus dest [seed] [scale]` and
  `bench-estate-corpus-selftest`. The self-test is in `check-optional` only
  (OI-1003-Q7).
- **Evidence:**
  [docs/evidence/estate-corpus-v1-2026-10-03.md](../evidence/estate-corpus-v1-2026-10-03.md)
  holds the identities, per-class counts, archive paths, receipts,
  toolchain and caveats.

## Identities and archive copies

Toolchain: the devShell on sting, git 2.54.0 with zlib-ng 2.3.3, SQLite
3.53.1, zstd 1.5.7 and Python 3.12.13.

- **small:** `931af5b130fe601f385a06fa68f85c1f2830f32458787912134b579f01d4572b`.
  Generated twice into fresh directories, with byte-identical manifests;
  the self-test and the archive copy reproduced it.
- **estate:** `0cb96231438c8cd721460276146c8447100951b7ebaeff0eadfe7efa8a3b60bf`.
  Generated twice into fresh directories, with byte-identical manifests;
  the archive copy reproduced it.
- **Archive copies,** generated in place, sealed, with a passing `verify`:
  - `/srv/data/jess/archive/bulkload-evidence/estate-corpus-931af5b130fe/`
  - `/srv/data/jess/archive/bulkload-evidence/estate-corpus-0cb96231438c/`

  Each holds its `VERIFY-RECEIPT.json`.

## Findings for later lanes

- **S2 and the WAL image.** A read-only, WAL-aware open (what
  `provider_sqlite::snapshot` does) of a WAL database with no `-shm` creates
  `<db>-shm` beside it, in the source directory. That is a write on the
  source side. The S2 property tests and the OI-1003-Q16 SQLite exception
  should state whether it is in scope. On a sealed copy, the same open fails
  ("unable to open database file"), and `immutable=1` opens it but ignores
  the WAL frames. So measure on a fresh in-place generation, never on the
  sealed archive copy.
- **Not relocatable.** Linked worktrees hold absolute paths, which
  bulkload's carry needs. Generate in place; never copy the corpus, and
  never run git in a copy.
- **Hooks.** Git in the generator uses an isolated global config, so the
  operator's global `core.hooksPath` hooks do not run inside the synthetic
  repositories. Nothing sets `core.hooksPath` and nothing passes
  `--no-verify` (R-N98). This lane's commits ran the repository hooks
  normally.

## Validation

- **Self-test:** `estate_corpus.py selftest` gave 35 checks ok and 0
  failures, in 17.7 to 19.2 s. The final run used a caller umask of 077.
- **Lint:** `ruff check` and `ruff format --check` on the script are clean
  (ruff 0.15.22 from the devShell). The repository's `python-lint` covers
  `scripts/` and `tests/` only, not the bench scripts.
- **check-fast:** `flock …/.check-fast.lock nice -n 10 nix develop .#default
  --command just check-fast` passed (exit 0). It ran on the final script
  and docs, apart from small wording edits to this note made afterwards. It
  waited about 28 min for the shared lock, then ran for about 6.5 min. The
  gates passed: ruff, shellcheck, actionlint, gitleaks ("no leaks found"),
  cargo fmt, clippy and tests, the fault harness, the power-loss proofs, and
  the 22 contract tests.
- **Generations:** small seven times and estate three times, as listed
  above. No generation reported a host-path problem.
- **Mutate:** an estate mutate round of N=12 on a scratch copy ran clean
  (see the evidence doc).

## Commits

- 58b1c2e, 8bead15, f01155d and 7b75b26: made by the duplicate copies (see
  the incident above).
- The final session's two signed commits on top of 7b75b26: the generator
  changes, then the evidence doc and this note.

## Open

- The S3 measurement harness, after the WP2 counter PRs #144 and #146.
- S1 samples on this corpus (gated, R-N81/R-N91).
- A first generation on neo, to confirm the identity on aarch64-darwin.
- Linear: distilled facts are not on the owning Linear issue yet. This lane
  had no Linear write in scope.
- v2 candidates: prunable worktree administration, reftable, SHA-256
  repositories and submodules.
