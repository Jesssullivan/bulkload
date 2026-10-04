# 2026-10-03 WP0(e) estate-shaped corpus, phase 1

Rulings: OI-1003-Q19 (WP0(e): an estate-shaped S1 corpus beside R23's),
OI-1003-Q23, R-N13.

- **Branch:** `feat/wp0e-estate-corpus-20261003`, from `origin/main` 57030e1.
- **Worktree:** `bulkload.worktrees/wp0e-estate-corpus-20261003`.
- **Push:** not pushed; no PR yet.

## Done

- **`crates/bulkload-bench/scripts/estate_corpus.py`** is a deterministic,
  sealed generator of an estate-shaped corpus. It uses stdlib Python, the git
  CLI, the `sqlite3` module and the `zstd` CLI from devShell stdenv. It has
  five subcommands: `generate`, `verify`, `manifest`, `mutate DEST N` (the S3
  knob) and `selftest`. Scales are `small` (seconds) and `estate` (24
  repositories, 121,633 files, 3.93 GB).
- **The seal** is the SHAKE-256/256 of a canonical manifest over path, type,
  mode, size, content digest and symlink target. Two normalisations apply:
  - git index stat words and the checksum are zeroed;
  - the corpus root inside worktree `.git` and `gitdir` files is replaced by
    a token.
  Every other nondeterministic byte is made deterministic at generation:
  - a logical git clock;
  - pinned pack, index and gc config;
  - an explicit template and a rewritten `.git/config`;
  - WAL salts rewritten with checksums recomputed.
  The full table is in
  [docs/evidence/estate-corpus-v1-2026-10-03.md](../evidence/estate-corpus-v1-2026-10-03.md).
- **just recipes:** `bench-estate-corpus dest [seed] [scale]` and
  `bench-estate-corpus-selftest`. The self-test is in `check-optional` only
  (OI-1003-Q7). No contract test inventories bench recipes, so none changed.
- **Evidence:** `docs/evidence/estate-corpus-v1-2026-10-03.md`.

## Measured

- **Small identity:** `b36454548957e9bac0b4aefce6e54cf90335df0323d152ececad139aa09a29be`.
  All twelve small generations on sting gave it, into nine distinct paths.
  Six of them came from three self-test runs, and one from the just recipe.
  `generate` now fails if a matching toolchain gives a different identity
  (`RECORDED`).
- **Estate identity:** `0bd1104ecb67e160689dba4719f34dacf768420016180b38329f3064170c4baf`. Two generations into different paths gave byte-identical manifests, and both copies verify.
- **Toolchain** (devShell on sting): git 2.54.0 with zlib-ng 2.3.3, SQLite
  3.53.1, zstd 1.5.7 and Python 3.12.13.
- **Walk smoke:** `bulkload-agent walk` (debug build, 57030e1) reported 0
  refusals at both scales. Its rows equal the manifest entries: 1,576 small
  and 140,313 estate. This is not an S1 sample.

## Findings for later lanes

- **S2 and the WAL image.** A read-only, WAL-aware open (what
  `provider_sqlite::snapshot` does) of a WAL database with no `-shm` creates
  `<db>-shm` in the source directory. SQLite 3.53.1 and 3.51.2 both did.
  The 3.51.2 probe also showed that the main file and `-wal` stay
  byte-identical. The S2 property tests and the OI-1003-Q16 SQLite exception
  should state whether creating `-shm` is in scope.
- **Not relocatable.** The corpus cannot be moved: linked worktrees
  administer absolute paths, and bulkload's carry needs that. The S1/S3
  harness should generate the corpus in place, not copy it the way
  `r23_ab.py` copies R23. Never run git in a copy.
- **Hooks.** Git in the generator uses an isolated global config, as the task
  specified (HOME isolated, `GIT_CONFIG_NOSYSTEM=1`). So the operator's global
  `core.hooksPath` hooks do not run inside the synthetic corpus repositories,
  which live under `$TMPDIR` and hold only `.sample` hooks. Nothing sets
  `core.hooksPath`, and nothing passes `--no-verify` (R-N98). This lane's own
  commits ran the repository hooks normally.

- **Guard refusal (R-N101).** The process-control hook refused a
  self-audit grep for process-control words over the changed files; the
  refusal is quoted verbatim in the lane's result. Under R-N101 the audit was
  dropped, not reformulated, and the lane continued. The pre-commit
  process-safety audit stays the authoritative wording check (R-N92).

## Validation

- `just bench-estate-corpus-selftest`: ok, 0 failures, 9 to 15 s, recorded identity matched.
- `nix develop .#default --command just check-fast`: in progress at this checkpoint commit; the final result is in the next commit.

## Open

- **Not done yet:**
  - the S3 measurement harness, after the WP2 counter PRs #144 and #146
    merge;
  - S1 samples on this corpus (gated, R-N81/R-N91);
  - a first generation on neo, to confirm the identity on aarch64-darwin.
- **Linear:** distilled facts are not yet on the owning Linear issue. This
  lane had no Linear write in scope.
- **v2 candidates:** prunable worktree administration, reftable, SHA-256
  repositories and submodules.
