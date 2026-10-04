# Estate-shaped corpus v1 — 2026-10-03

Rulings: OI-1003-Q19 (WP0(e): an estate-shaped S1 corpus beside R23's),
OI-1003-Q23, OI-1003-Q35 (Sprint 2 S3 measurement on this corpus), R-N13.
Context: [docs/slo.md](../slo.md) WP0(e), S1 and S3, and
[the architecture review](../plans/2026-10-03-architecture-review.md) WP0(e)
and WP6 PR 4.

This is phase 1 only. It covers the generator, its seal, its self-test, and
one sealed archive copy per scale. The S3 measurement harness comes after
the WP2 counter PRs #144 and #146. The coordinator reports that #144 merged
at 04ea9cb87; that was not checked here. This doc claims no S1 or S3 number.

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
  matches `DEST/SEAL.json`, or, once sealed, its `VERIFY-RECEIPT.json`.
- **The S3 knob:** `estate_corpus.py mutate DEST N` (the `--mutate N` knob).
- **Seal:** `estate_corpus.py seal DEST` makes a kept copy read-only and
  writes its receipt (see [Seal and archive copies](#seal-and-archive-copies)).
- **umask:** every subcommand sets umask 022 first. Before this, `mutate`
  inherited the caller's umask, which could change the modes of the
  directories and objects git creates.
- **Self-test:** `just bench-estate-corpus-selftest`, run by
  `just check-optional` and not by `check-fast` (OI-1003-Q7). It generates
  scale `small` twice under `$TMPDIR`. It requires one identity, the
  recorded one when the toolchain matches, and byte-identical manifests. It
  then checks these:
  - the classes partition the manifest, and none is empty;
  - the history repository holds four packs plus loose objects;
  - `verify` accepts a fresh copy;
  - the host-path scan flags a planted host path and an absolute symlink;
  - a WAL-aware read leaves the seal intact;
  - one mutation of each kind gives the same identity on both copies, and
    the sidecar lists exactly the manifest difference and its byte bounds;
  - a tampered byte fails `verify`;
  - `seal` refuses a copy with a `-shm`, then seals it to 0444/0555;
  - `verify` accepts the sealed copy, and `mutate` and a second `seal`
    refuse it;
  - `verify` fails on the sealed copy after one added file, after one
    removed file, and after `mutate 1` once the seal is undone by hand.
  Last, it removes both copies. It passed in 17.7 to 19.2 s on sting. The
  final run used a caller umask of 077, so it also exercised the umask fix.

All bytes come from SHAKE-256 keyed by (seed, scale, label), as in
`r23_corpus.py`. Nothing uses `random`. The text alphabet is r23's
`TEXT_TABLE`.

## What the corpus contains

The corpus root models an agent's `$HOME`.

| Area | Contents |
|---|---|
| `git/rNN-*` | Non-bare repositories with history. Each has a `--no-ff` merge of a feature branch, an annotated tag `v0.1.0`, a lightweight tag `v0.2.0` and an unmerged `side` branch. Each also has a tracked symlink, an executable script, a binary asset, and ignored outputs (`target/`, `debug.log`, `.env.local`). |
| packing profile | Repository *i* is `mixed`, `loose` or `packed`, by *i* mod 3. A mixed repository has a pack and packed-refs written mid-history, then loose objects and loose refs. |
| `git/history-heavy` | The #48 repack shape: a long history much larger than its checkout. `assets/model.bin` gets a new incompressible revision every 4th commit, and `*.bin -delta` keeps it out of delta search. A few source files change in every commit. Incremental `repack -d` at 1/4, 2/4, 3/4 and 19/20 of the history leaves four packs, and the newest commits stay loose. Tags every 50 commits and an unmerged `side` branch are in packed-refs. Scale estate: 600 commits, 1,500 source files and 150 revisions of 9 MiB. Scale small: 40 commits, 60 source files and 10 revisions of 512 KiB. |
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
the sum of the manifest's size column. In that column, a worktree `.git` file
or `gitdir` counts at its root-normalised size.

| Count | small | estate |
|---|---:|---:|
| entries | 1,710 | 142,318 |
| regular files | 1,117 | 123,421 |
| directories | 575 | 18,306 |
| empty directories | 19 | 76 |
| symlinks | 18 | 591 |
| bytes | 21,532,490 | 5,368,891,605 |
| files ≤ 4 KiB | 953 | 101,783 |
| files ≥ 1 MiB | 12 | 320 |
| `.git` directories (non-bare repositories, nested included) | 5 | 28 |
| bare repositories | 1 | 4 |
| linked worktree administrations | 4 | 26 |
| worktree `.git` files | 4 | 26 |
| git indexes | 9 | 54 |
| loose objects | 190 | 18,803 |
| packs | 7 | 24 |
| JSONL / JSONL.zst | 7 / 1 | 801 / 24 |
| SQLite databases / WAL images | 4 / 1 | 11 / 1 |
| `host_scan_skipped` (files above 64 MiB) | 0 | 11 |

`host_scan_skipped` counts the files above the 64 MiB scan limit. At scale
estate there are 11: the five `data/` files, the four history packs (270 to
343 MiB each), and the `r00` and `r12` packs that hold a 64 MiB blob.

### Per class

`SEAL.json` `counts.classes` partitions the manifest's entries into 13 estate
classes. Rules apply in order, and the first match wins:

| Class | Entries |
|---|---|
| `git-history` | All of `git/history-heavy`, its `.git` included. |
| `git-admin` | Everything inside a git directory (`.git`, a bare `*.git`, `git/mirrors`), plus each linked worktree's `.git` file. |
| `git-worktree` | A linked worktree's checkout: `git/*.worktrees/**` and `.claude/worktrees/**` inside a repository. |
| `git-checkout` | The rest of `git/`: main checkouts, their untracked, ignored and nested-checkout files, and `git/` itself. |
| `sqlite` | Outside `git/`: any SQLite database, by its header, and any `-wal`. |
| `agents` | `.codex/`, `.claude/` and `.claude.json`. |
| `credentials` | `.ssh/` and `.config/agent/`. |
| `node-modules` | Any path with a `node_modules` component. |
| `projects` | The rest of `projects/`. |
| `cache` | `.cache/`. |
| `large-data` | `data/`. |
| `dotfiles` | Other dot entries at the top, and `dotfiles/`. |
| `edge` | Everything else: `names/`, `links/`, `deep/`, `Documents/`, `Downloads/`. |

Each cell is entries / files / dirs / symlinks / bytes.

| Class | small | estate |
|---|---|---|
| `agents` | 29 / 14 / 15 / 0 / 264,380 | 1,158 / 955 / 203 / 0 / 663,598,559 |
| `cache` | 147 / 64 / 83 / 0 / 256,644 | 19,014 / 15,000 / 4,014 / 0 / 62,718,063 |
| `credentials` | 5 / 3 / 2 / 0 / 708 | 5 / 3 / 2 / 0 / 708 |
| `dotfiles` | 30 / 12 / 17 / 1 / 13,361 | 31 / 12 / 18 / 1 / 11,676 |
| `edge` | 68 / 21 / 40 / 7 / 1,910 | 68 / 21 / 40 / 7 / 1,910 |
| `git-admin` | 546 / 303 / 243 / 0 / 1,392,837 | 23,646 / 19,378 / 4,268 / 0 / 190,134,620 |
| `git-checkout` | 218 / 169 / 46 / 3 / 1,737,834 | 31,228 / 29,628 / 1,576 / 24 / 217,542,747 |
| `git-history` | 134 / 104 / 30 / 0 / 6,090,472 | 2,005 / 1,788 / 217 / 0 / 1,435,739,524 |
| `git-worktree` | 224 / 182 / 38 / 4 / 3,625,244 | 31,711 / 30,141 / 1,544 / 26 / 484,390,779 |
| `large-data` | 7 / 3 / 4 / 0 / 6,291,476 | 10 / 5 / 5 / 0 / 2,113,933,397 |
| `node-modules` | 281 / 226 / 52 / 3 / 176,520 | 33,388 / 26,445 / 6,410 / 533 / 20,200,047 |
| `projects` | 16 / 11 / 5 / 0 / 13,328 | 42 / 33 / 9 / 0 / 38,447 |
| `sqlite` | 5 / 5 / 0 / 0 / 1,667,776 | 12 / 12 / 0 / 0 / 180,581,128 |

### Repositories

Small scale: three repositories plus the history repository.

| Repository | Packing | Source files at start | Commits | Other state |
|---|---|---:|---:|---|
| `r00-delta` | mixed | 35 | 11 | branch, detached and agent worktrees; 2 stashes; dirty index; nested `vendor/offset-lib`; commit-graph; 1 MiB blob |
| `r01-row` | loose | 35 | 12 | branch worktree |
| `r02-head` | packed | 34 | 10 | 2 stashes; bare mirror `git/mirrors/r02-head.git` |
| `history-heavy` | four packs plus loose | 60 | 40 | 10 `model.bin` revisions of 512 KiB; tag `v1.0.0`; `side` |

Estate scale: 24 repositories plus the history repository.

- **Packing:** 8 each of mixed, loose and packed.
- **Languages:** 6 each of rs, py, ts and go.
- **Generated source files** at the initial commit: 29,070 in total, 371 to
  2,337 per repository. Each repository also has 8 skeleton files, or 9 with
  the blob.
- **Commits** reachable from branches and tags: 721 in total, 17 to 43 per
  repository.
- **History repository:** 600 commits, 1,500 source files, 150 revisions
  of 9 MiB, 4 packs and 258 loose objects. It has 1,788 files and
  1,435,739,524 bytes.
- **Under `git/`:** 80,935 files and 2,327,807,670 bytes. Apart from the
  history repository, 19,352 files and 190,133,102 bytes sit inside git
  directories, the bare mirrors included. Without `git/history-heavy`,
  `git/` holds 79,147 files and 892,068,146 bytes, the same totals as the
  pre-history corpus.
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
| `git/` | 80,935 | 2,327,807,670 |
| `data/` | 5 | 2,113,933,397 |
| `.claude/` | 627 | 439,353,121 |
| `.codex/` | 328 | 251,367,628 |
| `.local/` | 11 | 146,031,390 |
| `.cache/` | 15,000 | 62,718,063 |
| `projects/` | 26,478 | 20,238,494 |
| `.config/` | 5 | 7,427,412 |
| top-level files | 7 | 8,572 |
| `dotfiles/` | 2 | 3,308 |
| `names/` | 18 | 1,305 |
| `.ssh/` | 2 | 640 |
| `Documents/` | 2 | 600 |
| `deep/` | 1 | 5 |

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

`SEAL.json` holds the identity, the generation root, the counts (per class
included), a per-repository inventory, the toolchain versions, the
generator's own SHAKE-256 and the mutation history.

### Host-path scan

Every manifest pass scans for the generating host's absolute paths. That
covers `generate`, `verify`, `mutate` and `seal`. It reads every regular
file up to 64 MiB and every symlink target. The markers are:
- the corpus root and DEST;
- `$HOME`, as given and resolved;
- the generator's checkout;
- the temp directory, as given and resolved;
- the git scratch prefix `estate-corpus-git-`.

Markers shorter than 8 bytes, such as a bare `/tmp`, are skipped. They would
match generated text by chance.

The one exception is the documented normalisation. A linked worktree's
`.git` file and its administration's `gitdir` are scanned after the root
becomes `@CORPUS_ROOT@`. So a leak anywhere else, including the root itself
in any other file, is a problem. It fails `generate` and `verify`, and
`mutate` and `seal` refuse.

The scan has three limits:
- Bytes are scanned as stored. Zlib git objects, packs and `.zst` frames are
  not decompressed.
- Files above 64 MiB are counted as `host_scan_skipped`, not scanned.
- The corpus is generated into two different DEST paths and the manifests
  are byte-identical. That proves no DEST-dependent byte enters the corpus
  anywhere, compressed bytes included.

Generated text names a modelled home, `/home/agent`. A host whose `$HOME` is
`/home/agent` would fail the scan.

### Measured identities (sting, x86_64-linux, devShell)

| Scale | Seed | Identity | How it was checked |
|---|---|---|---|
| small | `bulkload-estate-corpus-v1` | `931af5b130fe601f385a06fa68f85c1f2830f32458787912134b579f01d4572b` | Generated into two fresh scratch directories, with byte-identical manifests (`cmp`). Two self-test runs generated it four more times (two each), under the nix-shell `$TMPDIR`, and the archive copy once more, in place. Every run gave this identity. |
| estate | `bulkload-estate-corpus-v1` | `0cb96231438c8cd721460276146c8447100951b7ebaeff0eadfe7efa8a3b60bf` | Generated twice, in parallel, into two fresh scratch directories, with byte-identical manifests (`cmp`). The archive copy, generated in place afterwards, gave it a third time (`recorded=True`). |

Toolchain for both: git 2.54.0 built with zlib-ng 2.3.3, SQLite 3.53.1,
zstd 1.5.7 and Python 3.12.13. `RECORDED` in the script pins both identities
to the git, zlib, SQLite and zstd versions. With a matching toolchain,
`generate` prints `recorded=True`, or fails on a mismatch. With any other
toolchain it prints `recorded=none`.

Superseded: `b3645454…` (small) and `0bd1104e…` (estate). These identities
were recorded on this branch before the history repository was wired in, in
58b1c2e to 7b75b26. No archive copy was made of them.

## Bytes that are not a function of the seed

| Source | Where | Handling |
|---|---|---|
| Stat data | Each entry of `.git/index` and `.git/worktrees/*/index` (ctime, mtime, dev, ino, uid, gid, size, and the trailing checksum over them) | **Normalised** (`norm=git-index`). The digest is over the index with those words zeroed and the checksum dropped; mode, oid, flags, path and TREE stay. A stat-bearing extension (UNTR, FSMN, link) is a generation problem. Both index v2 and v3 occur: the intent-to-add entry sets an extended flag, so `r00`'s index is v3. The raw index differs between copies; the identity does not. |
| Absolute paths | A linked worktree's `.git` file and `.git/worktrees/*/gitdir` | **Normalised** (`norm=root`). The recorded root becomes `@CORPUS_ROOT@`. Bulkload resolves a worktree's back-pointer absolutely (`git_carry.rs`), so `--relative-paths` worktrees are not used. Any other file that contains the root, or any other host path, is a problem (see [Host-path scan](#host-path-scan)). |
| Wall-clock dates | Commits, tags, stashes and every reflog line, including the reflog from `checkout` and `worktree add` | **Made deterministic.** Every git call gets the next value of a logical clock in `GIT_AUTHOR_DATE` and `GIT_COMMITTER_DATE`. |
| Wall-clock expiry | Reflog and prune expiry (`gc`) | **Made deterministic.** Set to `never`, and no `git gc` runs; packing uses `repack -a -d`, `repack -d` and `pack-refs --all`. |
| Thread count | Pack delta search; IEOT and EOIE index extensions | **Made deterministic** with `pack.threads=1` and `index.threads=1`. |
| Caches | Untracked cache, fsmonitor, split index, bitmaps, auto gc and maintenance | **Off.** |
| Platform config | `.git/config` (`ignorecase` and `precomposeunicode` on macOS) | **Rewritten** to the Linux `git init` text. |
| Git version | Template hooks and `description` | **Made deterministic** with an explicit `--template`, which also means no sample hook varies by version. |
| Environment | Global and system git config, attributes, umask | **Isolated:** `GIT_CONFIG_GLOBAL` points at a private file, with `GIT_CONFIG_NOSYSTEM=1` and `GIT_ATTR_NOSYSTEM=1`. HOME is private and inherited `GIT_*` variables are dropped. Every subcommand sets umask 022. |
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
| `commit` | Adds a new commit on an unmerged `side` branch, through plumbing: `hash-object`, a temporary index, `commit-tree` and `update-ref`. The history repository is a candidate too. |
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
  `verify` passes after a mutation of an unsealed copy.
- **Sealed copies:** `mutate` refuses a sealed copy while it is read-only.
  If the seal is undone by hand and `mutate` runs, `verify` fails from then
  on (see below).
- **Failure:** a failed mutate leaves the corpus unsealed. Regenerate it.

Measured results:

| Run | Added | Removed | Modified | `stat_only` | `reads_allowed` | `reads_allowed_bytes` | `repos_changed` | Identity |
|---|---:|---:|---:|---:|---:|---:|---:|---|
| small, round 1, N=6 | 14 | 1 | 7 | 8 | 15 | 75,137 | 3 | `931af5b1…` → `cb825526…` |
| estate, round 1, N=12 | 16 | 6 | 58 | 43 | 71 | 990,411 | 8 (17 unchanged) | `0cb96231…` → `fbf36b5b…` |

- At scale small, both self-test copies reached the same identity.
- A head-move rewrites the files that differ between the two commits, so
  `removed` and `modified` include worktree files as well as the deleted
  ones.

## Sprint 2 S3 measurement support (OI-1003-Q35)

Under OI-1003-Q35, as relayed by the coordinator, the Q15 engine decision may
use S3 byte counters and the rusage CPU ratio measured on sting. The
admissible counters are `source_bytes_read`, content bytes, `census_walks`
and pack bytes; wall time is informational only. Running estate verbs on
this synthetic, sealed corpus under `/srv/scratch` is a test, not an R-N56
estate operation.

Sprint 2 measures at three builds: adb9c66; the counters build (#144, merged
to main at 04ea9cb87); and #146 once it merges from main. The corpus supports
that in four ways:

1. **Same input at every build.** Generation is deterministic, and
   `generate` prints `recorded=True` when it reproduces the identity in
   `RECORDED`: `931af5b1…` (small) or `0cb96231…` (estate). Regenerate in
   place per build; the corpus is not relocatable, and the sealed archive
   copy cannot serve a WAL-aware open (see below).
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
4. **CPU ratio.** At scale estate the corpus is large enough that the rusage
   CPU of both the first pass and the rerun can be measured. It has
   142,318 entries and 5.37 GB, with 32 git directories, 54 indexes,
   18,803 loose objects and 24 packs.

Keep bulkload's private state, ledger and destination outside `DEST/corpus`.
Then every pass's source side should leave `verify` ok, apart from
`shm_ignored`. Any other difference that `verify` lists is a write to the
source (S2).

## Seal and archive copies

`seal DEST` runs in place only, on a DEST that verifies and holds no SQLite
`-shm`. It then does four things:
- **Modes.** Every file becomes 0444, or 0555 if it has an execute bit.
  Every directory becomes 0555, DEST and `mutations/` included. The original
  modes (0600, 0640, 0700, 0755) stay in `MANIFEST.tsv`.
- **Read-only view.** It records `readonly_identity`: the identity of the
  read-only view of `MANIFEST.tsv`, which is the same rows with the mode
  column mapped as above.
- **Receipt.** It checks that the sealed tree's manifest equals that view,
  byte for byte. Only then does it write `VERIFY-RECEIPT.json`, which holds:
  - both identities;
  - the round count and the counts;
  - the toolchain;
  - the generator's and the sealer's SHAKE-256;
  - the host and the time.
- **README.** It rewrites `README.md` with a PROTECTED notice.

From then on, `verify` checks the receipt. It requires all of these:
- the read-only view identity;
- read-only modes everywhere, DEST included;
- the original identity in `MANIFEST.tsv`;
- no mutation round after the seal.

So a sealed copy cannot change without an explicit chmod, and any change
fails `verify`: one added file, one removed file, or a `mutate` round after
the seal is undone by hand. The self-test checks all three.

The archive copies were generated in place, not copied: linked worktrees
hold absolute paths. Then they were sealed and verified.

| Scale | DEST (in place) | Identity | `readonly_identity` | Receipt | `verify` |
|---|---|---|---|---|---|
| small | `/srv/data/jess/archive/bulkload-evidence/estate-corpus-931af5b130fe/` | `931af5b1…` | `8dfa9965d45bd8fb4e05d6648877470cd3a930ac71dc824f95a89ef8cd4ca883` | `VERIFY-RECEIPT.json`, sealed 2026-10-04T04:04:58Z | ok: `sealed=1 relocated=0 shm_ignored=0` |
| estate | `/srv/data/jess/archive/bulkload-evidence/estate-corpus-0cb96231438c/` | `0cb96231…` | `b7590ca05e19127dc618fa890938ec1b7daa2db6daca673d89cf2a7a7a44443d` | `VERIFY-RECEIPT.json`, sealed 2026-10-04T04:21:37Z | ok: `sealed=1 relocated=0 shm_ignored=0` |

Seal and verify times are in [Generation cost](#generation-cost-informational-not-a-gated-sample).
Each receipt is the DEST's `VERIFY-RECEIPT.json`, and it records the same
identities, `"rulings": "OI-1003-Q19, OI-1003-Q23, OI-1003-Q35, R-N13"`, and
`generator_shake256` `62d253b4fede`… (the committed script).

- **Not a measurement source.** The archive copies are references to verify
  against, not trees to measure in. A read-only, WAL-aware open of the
  sealed WAL image fails ("unable to open database file"), because SQLite
  cannot create `-shm` in the 0555 directory. `immutable=1` opens it but
  ignores the WAL frames: 257 rows instead of 377 at scale small. Generate
  a fresh copy in place on the measurement host and compare its identity
  with the receipt.
- **Privacy.** The seal makes the 0600 and 0700 entries world-readable.
  This corpus holds no real credentials, and `/srv/data/jess` is 0700.

## Carriability smoke (not an S1 sample)

This was run on the pre-history corpus, with identities `b3645454…` and
`0bd1104e…`, and it was not rerun on the current identities.
`bulkload-agent walk` is the read-only stat walk, built in this worktree from
`57030e1` (debug):

| Scale | Rows | Refusals | Capped subtrees | `bytes_seen` | Elapsed |
|---|---:|---:|---:|---:|---:|
| small | 1,576 | 0 | 0 | 15,442,290 | 0.07 s |
| estate | 140,313 | 0 | 0 | 3,933,153,589 | 8.2 s |

- The row counts equal the manifest's entry counts of that corpus.
- `bytes_seen` is the raw size. It exceeds the manifest's normalised `bytes`
  by the length of the generation root inside each worktree pointer.
- No walk refusal means the names, depth and seat kinds stay inside
  bulkload's walk limits. The history repository adds no new name, depth or
  seat kind.
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
  run git in a moved copy: its worktrees administer the original. The
  archive copies were generated in place for this reason.
- **WAL image and S2.** A WAL-aware open of the WAL image creates
  `storage.db-shm` beside it, even read-only. `provider_sqlite::snapshot`
  opens the source that way (`SQLITE_OPEN_READ_ONLY`, not immutable).
  An earlier probe, with SQLite 3.51.2 (host Python), ran on a copy of the
  small image:
  - the open recovered 377 rows, and `integrity_check` was ok;
  - `storage.db` and `storage.db-wal` stayed byte-identical;
  - `storage.db-shm` was created.
  The devShell's SQLite 3.53.1 created `-shm` as well. That is a write on
  the source side, for S2 and OI-1003-Q16 to rule on. The generator sets a
  `-shm` beside a SQLite database aside and never seals it: it is SQLite's
  wal-index, and bulkload never carries it. `verify` reports it as
  `shm_ignored=N`, with one note line per file, so an S2 harness still sees
  the write. A first measurement pass does not stop the next `mutate`.
- **APFS.** Names avoid case collisions and NFD, so the tree is legal on
  APFS. The rewritten `.git/config` says `ignorecase` is unset even on a
  case-insensitive volume. That is harmless for this corpus, which has no
  case-only renames.

## Generation cost (informational, not a gated sample)

All on sting, with no load gate:

| Step | small | estate |
|---|---|---|
| `generate`, scratch, twice | 4.0 s and 4.5 s | 7 min 43 s and 7 min 42 s, in parallel; peak RSS 404 MB |
| `generate`, archive, in place | 4.6 s | 9 min 42 s; peak RSS 281 MB |
| `seal` (two manifest passes) | 1.2 s | 6 min 48 s; peak RSS 476 MB |
| `verify` of the sealed copy | 0.5 s | 4 min 28 s; peak RSS 534 MB |
| `mutate`, round 1 | about 1 s, inside the self-test | 5 min 22 s for N=12; peak RSS 667 MB |
| self-test | 17.7 to 19.2 s | not run |

Other lanes kept load1 between 10 and 33 throughout, so the times are an
upper bound.
