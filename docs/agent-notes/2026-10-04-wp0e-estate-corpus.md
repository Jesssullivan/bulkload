# 2026-10-04 WP0(e) estate corpus: review fixes

Rulings: OI-1003-Q19 (WP0(e): an estate-shaped S1 corpus beside R23's),
OI-1003-Q23, OI-1003-Q35 (Sprint 2 measures S3 byte counters and the rusage
CPU ratio on this corpus), R-N13.

- **Branch:** `feat/wp0e-estate-corpus-20261003`, on top of ec142cf.
- **Worktree:** `bulkload.worktrees/wp0e-estate-corpus-20261003`. This
  session was its only writer. The earlier duplicate copies had all ended.
- **Push:** pushed to `origin`; no PR (a PR is not in this lane's scope).
- **Facts:** the evidence doc,
  [estate-corpus-v1-2026-10-03.md](../evidence/estate-corpus-v1-2026-10-03.md),
  holds the measurements. This note records what was done and what is open.

## Done

A review of the first version raised 12 medium or high findings. Each is
fixed in `crates/bulkload-bench/scripts/estate_corpus.py`, in the evidence
doc, or in both:

1. **`-shm` hiding place.** `verify` accepted any `<db>-shm` beside a
   database, sealed copy included. Now an unsealed copy exempts only the
   exact wal-index of a WAL-mode database: size, header, salts, frame count,
   and page and hash tables are rebuilt from the `-wal` and compared, and
   only the 40-byte reader marks are free. A sealed copy exempts nothing.
   The model matched the `-shm` that SQLite 3.53.1 (Python) and 3.46.0 (the
   agent's bundled SQLite) wrote, before and after a writer appended frames.
2. **Git index checksum.** It is checked before normalisation; a corrupt one
   is a problem.
3. **Locale.** The script re-runs itself in Python's UTF-8 mode, and every
   text read and write is UTF-8. Under `aa_DJ.iso88591`, `generate` gives
   the recorded identity, and `verify` of the 2026-10-03 small archive copy
   is ok.
4. **Modes.** Explicit DEST and `corpus/` modes, `canonical_modes` after the
   build and each mutation round, and a refused default ACL on DEST's
   parent. The self-test's second generation runs under a setgid parent.
5. **The #48 case.** New kinds `history` (a commit on history-heavy's
   checked-out `main`, the first operation of every round), `repack` and
   `fetch`. The sidecar records per-repository new objects and tips.
6. **Easy side of S3.** New kinds `sqlite` (a DELETE-mode INSERT and UPDATE,
   plus a WAL commit with no checkpoint) and `large-edit` (64 bytes in a
   file of at least 64 MiB). With `history`, they run first in every round.
7. **Items.** The sidecar's `items` models bulkload's capture key per item
   and gives `census_walks_expected`. The smoke confirmed it: 30 expected
   and 30 measured after small round 1.
8. **Byte bounds.** Per-op sizes and ranges, `reads_by_class`,
   `changed_range_bytes`, `cdc_bytes`, the kept pre-round manifest, and
   `timing` (`mutated_at_ns`, `settle_ns`).
9. **S2.** New `s2-snapshot` and `s2-diff` subcommands. The doc now says
   that `verify` ok does not mean that nothing wrote.
10. **Hazards not modelled.** Documented, each with what bulkload does and
    its effect on S1, S3 and S4. A flagged v1.1 hazard set is the candidate
    fix; it is not built.
11. **Large objects.** `model.bin` revisions now rewrite a tenth of the
    previous one, and `*.bin -delta` is gone, so the history packs as
    deltas. The doc requires S1 and S3 per class, and S1 also on the tree
    without `data/` and `git/history-heavy`.
12. **Carriability.** A smoke of `copy`, `snapshot`, `estate-add-batch` and
    `estate-capture` on a fresh small generation, with a release build of
    main 46587af, is in the doc.

Low findings fixed in passing: `verify` checks the mode of `corpus/` on a
sealed copy; a root-normalised file that already holds `@CORPUS_ROOT@` is a
problem; `mutate` refuses a sealed DEST outright and an N its pools cannot
serve; a problem after the operations no longer advances the seal; and
mutations no longer touch nested work trees, `.gitignore` or
`.gitattributes`. The host-path scan wording now says the markers come from
the running process. Not changed: the receipt is still unsigned (now
documented), and `verify` still does not compare with `RECORDED`.

## Identities and archive copies

Fix 11 changed the history repository, so both identities moved. Same
toolchain as before: git 2.54.0 with zlib-ng 2.3.3, SQLite 3.53.1, zstd
1.5.7 and Python 3.12.13.

- **small:** `c767aa685c670abc2d406de3c300b4e1118db8581a941de42ceee7808f98aa69`.
- **estate:** `586de100483a317afda3b6013ddf0a35c3e3424f57de871a328535bd3d507a93`.
  Generated twice in parallel into fresh directories, with byte-identical
  manifests, then a third time in place for the archive.
- **Archive copies,** generated in place, sealed, with a passing `verify`:
  - `/srv/data/jess/archive/bulkload-evidence/estate-corpus-c767aa685c67/`
  - `/srv/data/jess/archive/bulkload-evidence/estate-corpus-586de100483a/`
- **Superseded:** `931af5b1…` and `0cb96231…`. Their 2026-10-03 archive
  copies stay in place, sealed. Whether to remove them is the operator's
  call.

## Findings for other lanes

- **S2: v1 capture freshens source objects.** Each changed `estate-capture`
  pass changed the mtime and ctime of existing loose objects and a pack in
  the source repositories: 83 loose objects and a pack on the first pass,
  7 and a pack after round 1, and none on a reuse pass. Bytes, sizes and
  inodes did not change, so `verify` stayed ok; `s2-diff` saw it. This
  looks like git freshening objects found through the private repository's
  `alternates`. It is for WP1 (S2) to rule on.
- **A bare mirror is not an estate item.** `estate-capture` refuses a bare
  repository with `IO (errno 2)` on every pass. That refusal is untyped, so
  closure-report would not count it as a typed refusal.
- **Transfer refusals.** `copy` refuses every SQLite seat
  (`SQLITE_STATE_CHANGED`): 5 at scale small and 12 at scale estate. The
  run then exits `CONTRACT_SELF_INCONSISTENT`. `snapshot` carries each of
  them. An S1 comparison is like for like only without those seats.
- **The provider_sqlite `-shm`.** `snapshot` of the WAL image creates
  `storage.db-shm` in the source, as the 2026-10-03 note predicted. That is
  the S2 write for OI-1003-Q16.

## Validation

- **Self-test:** 46 checks ok, 0 failures, in 25.1 s under load, on the
  committed script.
- **Lint:** `ruff check` and `ruff format --check` on the script are clean
  (ruff from the devShell).
- **check-fast:** `flock …/.check-fast.lock nice -n 10 nix develop .#default
  --command just check-fast` passed (exit 0) on `fbd980e` and these docs,
  before the last number edits to the docs. It waited 4.5 min for the
  shared lock, then ran for 8.4 min. The gates passed: ruff, shellcheck,
  actionlint, gitleaks ("no leaks found"), cargo fmt, clippy and tests,
  the fault harness, the power-loss proofs, and the 22 contract tests.
- **Generations:** small twelve times (eight in four self-test runs, then
  under ISO-8859-1, for the smoke, for the measured round and for the
  archive) and estate three times. None reported a host-path problem.

## Commits

- `fbd980e`: the script (signed; the hooks ran, and the process-safety
  audit passed). Its SHAKE-256, `4d9d1350bd0b`…, is the
  `generator_shake256` of both new receipts.
- The commit after it: the evidence doc and the agent notes (signed).

## Open

- The S3 measurement harness (#144 and #146 are merged).
- S1 samples on this corpus (gated, R-N81/R-N91), per class and on the
  subset without `data/` and `git/history-heavy`.
- WP1: the object freshening above, and the provider_sqlite `-shm`
  (OI-1003-Q16).
- A typed refusal, or support, for a bare mirror as an estate item.
- The v1.1 hazard set, behind a flag.
- A first generation on neo, to confirm the identities on aarch64-darwin.
- Linear: distilled facts are not on the owning Linear issue yet. This lane
  had no Linear write in scope.
