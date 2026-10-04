# Estate-shaped corpus v1 — 2026-10-03

Rulings: OI-1003-Q19 (WP0(e): an estate-shaped S1 corpus beside R23's),
OI-1003-Q23, OI-1003-Q35 (Sprint 2 S3 measurement on this corpus), R-N13.
Context: [docs/slo.md](../slo.md) WP0(e), S1 and S3, and
[the architecture review](../plans/2026-10-03-architecture-review.md) WP0(e)
and WP6 PR 4.

This is phase 1 only: the generator, its seal and its self-test. The S3
measurement harness comes after the WP2 counter PRs #144 and #146. #144 is
merged at 04ea9cb87; that is reported by the coordinator and was not checked
here. No S1 or S3 number is claimed here.

## Why

R23 corpus v1 ([r23-corpus-v1-2026-10-02.md](r23-corpus-v1-2026-10-02.md))
is 23 regular files of 1 B to 88 MiB. It never exercises per-entry costs,
which the review names as the reason S1 is not yet measured where it matters
(section 1, finding 5). It also has no git, worktrees or SQLite.

A real agent estate is mostly small files, git administration and agent
state. Bulkload must carry all of them (AGENTS.md "Estate rules"). This
corpus models that estate, deterministically, so that S1 and S3 can be
measured on it in addition to R23.

## Generator

- **Script:** `crates/bulkload-bench/scripts/estate_corpus.py`.
- **Dependencies:** stdlib Python, the git CLI, Python's `sqlite3` module, and
  the `zstd` CLI. The devShell provides zstd through stdenv; without it, the
  `.jsonl.zst` files are skipped and SEAL.json records a note.
- **Generate:** `just bench-estate-corpus DEST [SEED] [SCALE]`, or
  `estate_corpus.py generate DEST --seed S --scale small|estate`.
- **Check:** `estate_corpus.py verify DEST` exits 0 only if `DEST/corpus`
  matches `DEST/SEAL.json`.
- **The S3 knob:** `estate_corpus.py mutate DEST N` (the `--mutate N` knob).
- **Self-test:** `just bench-estate-corpus-selftest`, run by
  `just check-optional` and not by `check-fast` (OI-1003-Q7). It generates
  scale `small` twice under `$TMPDIR` and requires one identity and
  byte-identical manifests. It compares that identity with the recorded one
  when the toolchain matches. It then checks four things:
  - `verify` accepts both copies;
  - one mutation of each kind on both copies gives the same identity;
  - the sidecar lists exactly the manifest difference;
  - a tampered byte fails `verify`.
  Last, it removes both copies.

All bytes come from SHAKE-256 keyed by (seed, scale, label), as in
`r23_corpus.py`. Nothing uses `random`. The text alphabet is r23's
`TEXT_TABLE`.

## What the corpus contains

The corpus root models an agent's `$HOME`.

| Area | Contents |
|---|---|
| `git/rNN-*` | Non-bare repositories with history. Each has a `--no-ff` merge of a feature branch, an annotated tag `v0.1.0`, a lightweight tag `v0.2.0` and an unmerged `side` branch. Each also has a tracked symlink, an executable script, a binary asset, and ignored outputs (`target/`, `debug.log`, `.env.local`). |
| packing profile | Repository *i* is `mixed`, `loose` or `packed`, by *i* mod 3. A mixed repository has a pack and packed-refs written mid-history, then loose objects and loose refs. |
| `git/rNN-*.worktrees/<word>` | A linked worktree on branch `wt/<word>`. It holds one commit, an unstaged edit and an untracked `NOTES.md`. |
| `git/rNN-*.worktrees/detached` | A detached-HEAD linked worktree at `HEAD~1`. It holds only an untracked file, so `mutate` can move its HEAD. |
| `.claude/worktrees/agent-*` | An in-repository linked worktree, ignored by `.gitignore`, with an unstaged edit. |
| `git/mirrors/*.git` | Bare repositories, filled by `git push` over a relative URL. |
| main-checkout state | Two real stashes, one of them `-u` (a three-parent stash commit). A dirty index: a staged edit with an unstaged edit on top, a staged new file and an intent-to-add entry. Also unstaged edits, untracked files and a nested repository under `vendor/`. |
| `projects/web-*/node_modules` | Scoped packages, nested `node_modules`, byte-identical `LICENSE` files (duplicate content) and relative `.bin` symlinks. |
| `.cache/agent-cache/` | A hex fan-out of 64 B to 8 KiB files. |
| `data/` | Large files: random, text (about 4 bits per byte) and a sparse pattern (alternating 64 KiB zero and random runs, written in full, without holes). |
| `.codex/` | Codex-like `sessions/YYYY/MM/DD/rollout-*.jsonl`, `history.jsonl`, `config.toml`, a TUI log, `state.sqlite` and `sessions/archive/*.jsonl.zst`. |
| `.claude/`, `.claude.json` | Claude-like `projects/-home-agent-git-*/<uuid>.jsonl` with a `parentUuid` chain, plus todos, settings and a shell snapshot. |
| SQLite | `journal_mode=DELETE` stores, each with deleted rows, so it has free pages. Also one WAL crash image, `.local/share/opencode/storage.db` plus its `-wal`. |
| dotfiles | Shell rc files, `.gitconfig`, a 0700 `.ssh` holding a 0600 `config`, a 0600 `auth.json`, a 0444 and a 0640 file, and a relative directory symlink `.config/nvim -> ../dotfiles/nvim`. |
| `links/` | Symlinks: relative to a file, relative to a directory, dangling, absolute (`/dev/null`), a two-link loop, and a link to a link. |
| `names/`, `deep/` | Unusual but legal names: spaces, a leading dash, a trailing space, NFC `é`, CJK, an emoji, quotes, a backslash, `:`, `*?`, brackets, `%`, `#&;`, a 255-byte ASCII name and a 254-byte UTF-8 name. Also a 32-deep directory chain. |
| empty dirs | `Downloads`, `.local/state/agent`, `.cache/empty`, `projects/scratch/tmp`, `names/empty dir`, plus git's own empty directories. |

Repository features come from fixed rules on the index *i*, so repository 0
carries every feature:

| Feature | Rule |
|---|---|
| branch worktree | *i* mod 3 ≠ 2 |
| detached worktree, commit-graph | *i* mod 4 = 0 |
| agent worktree | *i* mod 6 = 0 |
| stashes | *i* mod 2 = 0 |
| dirty index | *i* mod 3 = 0 |
| nested repository | *i* mod 8 = 0 |
| bare mirror | *i* mod 6 = 2 |
| large blob | *i* mod 12 = 0 |

### Not modelled, on purpose

| Not modelled | Why |
|---|---|
| Hardlinks, holes, xattrs, ACLs | The manifest does not cover link counts, holes or extended attributes, so the corpus does not plant them. |
| FIFOs, sockets, devices | Bulkload refuses non-regular seats. |
| Names with control or format characters | Bulkload refuses them as `PATH_NOT_PORTABLE`. |
| Case-colliding pairs, NFD names | The tree must stay legal on APFS. |
| Prunable worktree administration, reftable, SHA-256 repositories, submodules | Candidates for a v2. |

## Exact counts per scale

Counts are from the manifest, so they are the same for every copy. `bytes` is
the sum of the manifest's size column, in which a worktree `.git` file or
`gitdir` counts at its root-normalised size.

| Count | small | estate |
|---|---:|---:|
| entries | 1,576 | 140,313 |
| regular files | 1,013 | 121,633 |
| directories (empty) | 545 (17) | 18,089 (74) |
| symlinks | 18 | 591 |
| bytes | 15,442,018 | 3,933,152,081 |
| files ≤ 4 KiB / ≥ 1 MiB | 862 / 8 | 100,085 / 308 |
| non-bare repositories, of them nested | 4, 1 | 27, 3 |
| bare repositories | 1 | 4 |
| linked worktrees (branch, detached, agent) | 4 (2, 1, 1) | 26 (16, 6, 4) |
| git indexes | 8 | 53 |
| loose objects / packs | 176 / 3 | 18,545 / 20 |
| stashes | 4 | 24 |
| JSONL / JSONL.zst | 7 / 1 | 801 / 24 |
| SQLite databases / WAL images | 4 / 1 | 11 / 1 |

Small scale: three repositories.

| Repository | Packing | Source files at start | Commits | Other state |
|---|---|---:|---:|---|
| `r00-delta` | mixed | 35 | 11 | branch, detached and agent worktrees; 2 stashes; dirty index; nested `vendor/offset-lib`; commit-graph; 1 MiB blob |
| `r01-row` | loose | 35 | 12 | branch worktree |
| `r02-head` | packed | 34 | 10 | 2 stashes; bare mirror `git/mirrors/r02-head.git` |

Estate scale: 24 repositories.

- **Packing:** 8 each of mixed, loose and packed.
- **Languages:** 6 each of rs, py, ts and go.
- **Generated source files** at the initial commit: 29,070 in total, 371 to
  2,337 per repository. Each repository also has 8 skeleton files, or 9 with
  the blob.
- **Commits** reachable from branches and tags: 721 in total, 17 to 43 per
  repository.
- **Under `git/`:** 79,147 files and 892 MB, of which 19,300 files and 183 MB
  sit inside git directories.
- **Other areas:**
  - `node_modules` fan-out in 3 projects;
  - 15,000 cache files;
  - 800 transcripts, with 300 Codex-like sessions and 500 Claude-like ones;
  - 10 DELETE-mode databases;
  - five large files under `data/`: 1 GiB sparse pattern, 512 MiB random,
    256 MiB text, 128 MiB random and 96 MiB text;
  - a 64 MiB blob in `r00` and `r12`.

Estate scale by top-level area (regular files):

| Area | Files | Bytes |
|---|---:|---:|
| `data/` | 5 | 2,113,933,397 |
| `git/` | 79,147 | 892,068,146 |
| `.claude/` | 627 | 439,353,121 |
| `.codex/` | 328 | 251,367,628 |
| `.local/` (SQLite stores, WAL image) | 11 | 146,031,390 |
| `.cache/` | 15,000 | 62,718,063 |
| `projects/` (`node_modules`) | 26,478 | 20,238,494 |
| `.config/` | 5 | 7,427,412 |
| everything else | 32 | 14,430 |

## Identity

`MANIFEST.tsv` has a version header, then one line per entry under
`corpus/`, sorted by path bytes. Its columns are:

```text
path  type(d|f|l)  mode(perm bits, '-' for a symlink)  size  shake256/256  target  norm
```

- The identity is the SHAKE-256/256 of `MANIFEST.tsv`.
- **Why SHAKE-256:** it is r23's generator primitive, and stdlib has it.
  r23's own identity is BLAKE3 through `b3sum`, which is not in the
  devShell.
- **Excluded:** mtimes, ctimes, inodes, devices, link counts, owners and
  directory sizes.
- **Symlink mode:** it is `-`, because Linux reports 0777 for a symlink while
  macOS applies the umask.

`SEAL.json` holds the identity, the generation root, the counts, a
per-repository inventory, the toolchain versions, the generator's own
SHAKE-256 and the mutation history.

### Measured identities (sting, x86_64-linux, devShell)

| Scale | Seed | Identity | How it was checked |
|---|---|---|---|
| small | `bulkload-estate-corpus-v1` | `b36454548957e9bac0b4aefce6e54cf90335df0323d152ececad139aa09a29be` | Twelve generations into nine distinct paths gave this identity. Six of them came from three self-test runs, and one from `just bench-estate-corpus`. The self-test also requires byte-identical manifests and the recorded identity; it passes in 9 to 15 s. With seed `other-seed`, the identity is `60d88b23…` (1,538 entries). |
| estate | `bulkload-estate-corpus-v1` | `0bd1104ecb67e160689dba4719f34dacf768420016180b38329f3064170c4baf` | Generated twice, into `wp0e-estate-a` and `wp0e-estate-bb`. The two MANIFEST.tsv files are byte-identical (`cmp`), and both copies `verify` ok. |

Toolchain for both: git 2.54.0 built with zlib-ng 2.3.3, SQLite 3.53.1,
zstd 1.5.7 and Python 3.12.13. `RECORDED` in the script pins both
identities to this toolchain. With a matching toolchain, `generate` prints
`recorded=True`, or fails on a mismatch. With any other toolchain it prints
`recorded=none`.

## Bytes that are not a function of the seed

| Source | Where | Handling |
|---|---|---|
| Stat data | Each entry of `.git/index` and `.git/worktrees/*/index` (ctime, mtime, dev, ino, uid, gid, size, and the trailing checksum over them) | **Normalised** (`norm=git-index`). The digest is over the index with those words zeroed and the checksum dropped; mode, oid, flags, path and TREE stay. A stat-bearing extension (UNTR, FSMN, link) is a generation problem. Both index v2 and v3 occur: the intent-to-add entry sets an extended flag, so `r00`'s index is v3. The raw index differs between copies; the identity does not. |
| Absolute paths | A linked worktree's `.git` file and `.git/worktrees/*/gitdir` | **Normalised** (`norm=root`). The recorded root becomes `@CORPUS_ROOT@`. Bulkload resolves a worktree's back-pointer absolutely (`git_carry.rs`), so `--relative-paths` worktrees are not used. Any other git file that contains the root is a generation problem. |
| Wall-clock dates | Commits, tags, stashes and every reflog line, including the reflog from `checkout` and `worktree add` | **Made deterministic.** Every git call gets the next value of a logical clock in `GIT_AUTHOR_DATE` and `GIT_COMMITTER_DATE`. |
| Wall-clock expiry | Reflog and prune expiry (`gc`) | **Made deterministic.** Set to `never`, and no `git gc` runs; packing uses `repack -a -d` and `pack-refs --all`. |
| Thread count | Pack delta search; IEOT and EOIE index extensions | **Made deterministic** with `pack.threads=1` and `index.threads=1`. |
| Caches | Untracked cache, fsmonitor, split index, bitmaps, auto gc and maintenance | **Off.** |
| Platform config | `.git/config` (`ignorecase` and `precomposeunicode` on macOS) | **Rewritten** to the Linux `git init` text. |
| Git version | Template hooks and `description` | **Made deterministic** with an explicit `--template`, which also means no sample hook varies by version. |
| Environment | Global and system git config, attributes | **Isolated:** `GIT_CONFIG_GLOBAL` points at a private file, with `GIT_CONFIG_NOSYSTEM=1` and `GIT_ATTR_NOSYSTEM=1`. HOME is private and inherited `GIT_*` variables are dropped. |
| `sqlite3_randomness` | WAL salt-1 and salt-2, and every frame checksum | **Rewritten** to seed-derived salts with the checksums recomputed. The image is then proved: a copy opens with `integrity_check` ok and the expected row count. Without the rewrite, two runs give different `-wal` bytes; with it, they are identical. |
| SQLite version | DELETE-mode headers (offset 96) | **Toolchain caveat.** Byte-stable for one SQLite (3.53.1). |
| zstd version | `.jsonl.zst` frames | **Toolchain caveat.** Byte-stable for one zstd, with `-3 --single-thread`. |
| zlib-ng and git versions | Loose-object and pack bytes, pack names, `.idx`, `.rev`, commit-graph | **Toolchain caveat.** Byte-stable for one git and zlib-ng. |
| Filesystem | mtimes, ctimes, inodes, directory sizes | **Excluded** from the manifest. Mtimes are left as git set them, so no index is racily clean. |

## Mutation knob for the S3 harness

`mutate DEST N` applies N operations in a seed-shuffled order of the six
kinds below. Targets are seed-chosen from the sorted manifest.

| Kind | Effect |
|---|---|
| `edit` | Overwrites 64 bytes in place; size is unchanged. |
| `append` | Appends a JSONL line. |
| `new` | Creates a file. |
| `delete` | Removes a file of at most 1 MiB. |
| `commit` | Adds a new commit on an unmerged `side` branch, through plumbing: `hash-object`, a temporary index, `commit-tree` and `update-ref`. |
| `head-move` | Runs `checkout --detach` in a detached worktree. |

- **Target rules:** file operations never touch a git directory, a detached
  worktree, SQLite files, `-wal` files, `.zst` files or read-only files.
- **Determinism:** round *r* is deterministic from (seed, scale, *r*), and
  its git clock starts at 2026-10-03 + *r* days.
- **Sidecar:** each round writes `DEST/mutations/round-RRRR.json`, which
  holds:
  - `operations`, each with its primary path;
  - `identity_before` and `identity_after`;
  - `changed`, the exact manifest difference: added, removed and modified;
  - `stat_only`, entries whose row is unchanged but whose inode, size, mtime
    or ctime moved, such as a directory whose listing changed or a rewritten
    file;
  - `reads_allowed`, the regular files among those. The S3 harness asserts
    that a rerun reads only these;
  - `reads_allowed_sizes` and `reads_allowed_bytes`, their sizes and total;
  - `changed_content_bytes`, the total size of added and modified files;
  - `repos_changed`, each touched repository (with its worktrees and bare
    mirror), the count of its touched paths and how many of those sit in git
    directories;
  - `repos_unchanged`, the count of the rest.
- **Seal:** `SEAL.json` and `MANIFEST.tsv` move to the new identity, so
  `verify` passes after a mutation.
- **Failure:** a failed mutate leaves the corpus unsealed. Regenerate it.

Measured results:

| Run | Added | Removed | Modified | `stat_only` | `reads_allowed` | Identity |
|---|---:|---:|---:|---:|---:|---|
| small, round 1, N=6 | 13 | 1 | 7 | 9 | 15 | `b3645454…` → `58fbcdc4…` |
| estate, round 1, N=12 | 14 | 6 | 58 | 44 | 71 | `0bd1104e…` → `62d0385b…` |

- At scale small, the second copy reached the same identity. The sidecar
  gives `reads_allowed_bytes` = 73,994, and `repos_changed` lists 2
  repositories (1 unchanged).
- The estate round took 395 s with a peak RSS of 685 MB. That covers two
  full manifest passes, one before and one after.
- In the estate round, each head-move rewrote the files that differ between
  the two commits, so `removed` includes worktree files as well as the
  deleted ones.

## Sprint 2 S3 measurement support (OI-1003-Q35)

Under OI-1003-Q35, as relayed by the coordinator, the Q15 engine decision may
use S3 byte counters and the rusage CPU ratio measured on sting. The admissible
counters are `source_bytes_read`, content bytes, `census_walks` and pack
bytes; wall time is informational only. Running estate verbs on this
synthetic, sealed corpus under `/srv/scratch` is a test, not an R-N56 estate
operation.

Sprint 2 measures at three builds: adb9c66; the counters build (#144, merged
to main at 04ea9cb87); and #146 once it merges from main. The corpus supports
that in four ways:

1. **Same input at every build.** Generation is deterministic, and
   `generate` prints `recorded=True` when it reproduces the identity in
   `RECORDED`: `b3645454…` (small) or `0bd1104e…` (estate). Regenerate in
   place per build; the corpus is not relocatable.
2. **Unchanged rerun.** A first pass may create `storage.db-shm` beside the
   WAL image. That file is reported (`shm_ignored`) but never sealed, so
   `verify` still passes and the rerun starts from the sealed identity. The
   unchanged-estate clause expects 0 content bytes against this state.
3. **Delta rerun.** `mutate DEST N` is deterministic per round. Round *r*
   gives the same `identity_after` and the same sidecar on every build. The
   sidecar gives the S3 delta bounds:
   - `reads_allowed_bytes`, with per-file `reads_allowed_sizes`, bounds
     `source_bytes_read` (OI-1003-Q18 inequality 1, plus racy seats, which
     the harness adds);
   - each `edit` records its offset and its 64-byte length, for the
     chunk-level bound on content bytes;
   - `repos_changed` and `repos_unchanged` give the expected spread of
     `census_walks` and pack bytes. An unchanged repository should not be
     re-censused or re-packed.
4. **CPU ratio.** The corpus is large enough at scale estate that the rusage
   CPU of the first pass and of the rerun are both measurable: 140,313
   entries and 3.93 GB, of which 27 git directories, 53 indexes, 18,545 loose
   objects and 20 packs.

Keep bulkload's private state, ledger and destination outside `DEST/corpus`.
Every pass's source side should then leave `verify` ok, apart from
`shm_ignored`; any other difference that `verify` lists is a write to the
source (S2).

## Carriability smoke (not an S1 sample)

`bulkload-agent walk` is the read-only stat walk, built in this worktree from
`57030e1` (debug). It was run on both scales:

| Scale | Rows | Refusals | Capped subtrees | `bytes_seen` | Elapsed |
|---|---:|---:|---:|---:|---:|
| small | 1,576 | 0 | 0 | 15,442,290 | 0.07 s |
| estate | 140,313 | 0 | 0 | 3,933,153,589 | 8.2 s |

- The row counts equal the manifest's entry counts.
- `bytes_seen` is the raw size. It exceeds the manifest's normalised `bytes`
  by the length of the generation root inside each worktree pointer.
- No walk refusal means the names, depth and seat kinds stay inside
  bulkload's walk limits.
- This exercises only the walk, not estate-capture or a copy. It is not an
  S1 sample: the host was loaded, there was no power or load gate, and the
  build was debug.

## Cross-host caveats

- **Toolchain.** The identity is a function of the seed and the toolchain:
  git 2.54.0 with zlib-ng 2.3.3, SQLite 3.53.1 and zstd 1.5.7, as the
  devShell pins them. SEAL.json records the versions. A host with another
  git, zlib-ng, SQLite or zstd produces a different identity from the same
  seed. Generation refuses git older than 2.45 (`--ref-format`).
- **Not yet checked on neo.** The corpus has not been generated on neo
  (aarch64-darwin). The same flake should pin the same versions there; that
  is unverified until the first neo generation is recorded here.
- **Not relocatable.** Linked worktrees point at the absolute path the corpus
  was generated in. Generate in place where it is measured (generation is
  deterministic) instead of copying it. `verify` normalises with the
  recorded root, so it accepts a moved copy and reports `relocated=1`. Never
  run git in a moved copy: its worktrees administer the original.
- **WAL image and S2.** A WAL-aware open of the WAL image creates
  `storage.db-shm` beside it, even read-only. `provider_sqlite::snapshot`
  opens the source that way (`SQLITE_OPEN_READ_ONLY`, not immutable). This
  was probed with SQLite 3.51.2 (host Python) on a copy of the small image:
  - the open recovered 377 rows, and `integrity_check` was ok;
  - `storage.db` and `storage.db-wal` stayed byte-identical;
  - `storage.db-shm` was created.
  An earlier probe with the devShell's SQLite 3.53.1, on an equivalent image,
  also created `-shm`.
  The generator therefore sets a `-shm` beside a SQLite database aside, and
  never seals it: it is SQLite's wal-index, and bulkload never carries it.
  `verify` reports it as `shm_ignored=N`, with one note line per file, so an
  S2 harness still sees the write. A first measurement pass does not stop
  the next `mutate`.
- **APFS.** Names avoid case collisions and NFD, so the tree is legal on
  APFS. The rewritten `.git/config` says `ignorecase` is unset even on a
  case-insensitive volume. That is harmless for this corpus, which has no
  case-only renames.

## Generation cost (informational, not a gated sample)

The small scale takes 3 to 7 s per generation, and the self-test 9 to 15 s,
on sting. The estate figures were measured on a heavily loaded sting (load1
above 100 from other lanes), so they are an upper bound. Generation took 30 min 55 s at load1 above 100, and 6 min 28 s once load fell to about 60. `verify` took 4 min 47 s and 3 min 17 s. Peak RSS was about 260 MB.
One 12-operation `mutate` round took 6 min 35 s, with peak RSS 685 MB; that
covers two full manifest passes.
