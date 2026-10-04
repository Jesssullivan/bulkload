# Estate-shaped corpus v1 — 2026-10-03

Rulings: OI-1003-Q19 (WP0(e): an estate-shaped S1 corpus beside R23's),
OI-1003-Q23, OI-1003-Q35 (Sprint 2 S3 measurement on this corpus), R-N13.
Context: [docs/slo.md](../slo.md) WP0(e), S1 and S3, and
[the architecture review](../plans/2026-10-03-architecture-review.md) WP0(e)
and WP6 PR 4.

This is phase 1 only. It covers the generator, its seal, its self-test, one
sealed archive copy per scale, and a carriability smoke. The S3 measurement
harness is not built yet; it can now start, since the WP2 counter PRs #144
and #146 are merged (main 04ea9cb and 46587af). This doc claims no S1 or S3
number.

Revised on 2026-10-04 after a review of the first version. The fixes changed
the generated history repository, so both identities moved. The ones
recorded on 2026-10-03 are superseded (see
[Measured identities](#measured-identities-sting-x86_64-linux-devshell)).

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
- **Dependencies:** stdlib Python 3.12 or later, the git CLI, Python's
  `sqlite3` module, and the `zstd` CLI. The devShell provides zstd through
  stdenv; without it, the `.jsonl.zst` files are skipped and SEAL.json
  records a note. The `sqlite` mutation needs Python 3.12's
  `sqlite3.Connection.setconfig`.
- **Generate:** `just bench-estate-corpus DEST [SEED] [SCALE]`, or
  `estate_corpus.py generate DEST --seed S --scale small|estate`.
- **Check:** `estate_corpus.py verify DEST` exits 0 only if `DEST/corpus`
  matches `DEST/SEAL.json`, or, once sealed, its `VERIFY-RECEIPT.json`.
- **The S3 knob:** `estate_corpus.py mutate DEST N` (the `--mutate N` knob).
- **S2:** `estate_corpus.py s2-snapshot DEST OUT` and
  `estate_corpus.py s2-diff DEST SNAPSHOT` (see [S2](#s2-what-verify-cannot-see)).
- **Seal:** `estate_corpus.py seal DEST` makes a kept copy read-only and
  writes its receipt (see [Seal and archive copies](#seal-and-archive-copies)).
- **The caller does not change the identity.** The review showed that it
  did, in two ways:
  - **Locale.** Path names decoded with the locale's filesystem encoding,
    and `MANIFEST.tsv` used the locale's text encoding. Under an
    ISO-8859-1 locale (`LC_ALL=aa_DJ.iso88591`), `verify` of a correct
    sealed copy failed with another identity, and `generate` crashed on the
    Japanese file name. The script now re-runs itself in Python's UTF-8
    mode, and every `MANIFEST.tsv`, `SEAL.json` and receipt read or write
    is UTF-8. Under that locale, `generate` now gives the recorded small
    identity, and `verify` of the 2026-10-03 small archive copy is ok.
  - **Modes.** umask 022 alone does not fix modes. A setgid DEST parent
    gave 574 directories mode 2755, and a default ACL on the parent gave
    0777 directories and 0666 git files: two other identities. Now DEST and
    `corpus/` get explicit modes, which drop an inherited setgid bit, and
    `canonical_modes` clears setuid, setgid, sticky and group or other
    write below them, after the build and after each mutation round. No
    corpus entry carries those bits, so on a normal host this changes
    nothing. `generate` refuses a DEST parent that has a default POSIX ACL,
    since that also adds ACL entries the manifest does not cover.
- **Self-test:** `just bench-estate-corpus-selftest`, run by
  `just check-optional` and not by `check-fast` (OI-1003-Q7). It generates
  scale `small` twice under `$TMPDIR`, the second time under a setgid
  parent. It requires one identity, the recorded one when the toolchain
  matches, and byte-identical manifests. It then checks these:
  - the classes partition the manifest, and none is empty;
  - the history repository holds four packs plus loose objects, has no
    `.gitattributes`, and stores `model.bin` revisions as deltas;
  - `verify` accepts a fresh copy, and gives the same identity under an
    ISO-8859-1 locale when the host has one;
  - the host-path scan flags a planted host path and an absolute symlink;
  - a WAL-aware read leaves the seal intact; a grown wal-index, and a
    `-shm` beside a DELETE-mode store, fail `verify`;
  - a corrupt git index checksum fails `verify`;
  - after a `git status` that refreshes an index, `verify` is still ok and
    `s2-diff` lists the index;
  - one mutation of each of the eleven kinds, with `history`, `sqlite` and
    `large-edit` first, gives the same sidecar on both copies apart from
    `timing`; the sidecar lists exactly the manifest difference, keeps the
    pre-round manifest, splits its byte bounds by class, and names
    history-heavy among the changed items;
  - a tampered byte fails `verify`;
  - `seal` refuses a copy with a `-shm`, then seals it to 0444/0555;
  - `verify` accepts the sealed copy, and `mutate` and a second `seal`
    refuse it;
  - `verify` fails on the sealed copy after a junk `-shm`, a copied valid
    wal-index, one added file, one removed file, a same-size edit, and a
    writable `corpus/`.

  Last, it removes both copies. It ran 46 checks with 0 failures, in
  25.1 s on sting under load (21 to 76 s across this session's runs).

All bytes come from SHAKE-256 keyed by (seed, scale, label), as in
`r23_corpus.py`. Nothing uses `random`. The text alphabet is r23's
`TEXT_TABLE`.

## What the corpus contains

The corpus root models an agent's `$HOME`.

| Area | Contents |
|---|---|
| `git/rNN-*` | Non-bare repositories with history. Each has a `--no-ff` merge of a feature branch, an annotated tag `v0.1.0`, a lightweight tag `v0.2.0` and an unmerged `side` branch. Each also has a tracked symlink, an executable script, a binary asset, and ignored outputs (`target/`, `debug.log`, `.env.local`). |
| packing profile | Repository *i* is `mixed`, `loose` or `packed`, by *i* mod 3. A mixed repository has a pack and packed-refs written mid-history, then loose objects and loose refs. |
| `git/history-heavy` | The #48 repack shape: a long history much larger than its checkout. `assets/model.bin` gets a new revision every 4th commit. The first revision is incompressible, and each later one rewrites about one 4 KiB block in ten of the previous one, so pack-objects finds deltas and pays a real delta search. No attribute disables delta search (v1 of 2026-10-03 had `*.bin -delta` and fully random revisions, which left the history almost delta-free). A few source files change in every commit. Incremental `repack -d` at 1/4, 2/4, 3/4 and 19/20 of the history leaves four packs, and the newest commits stay loose. Tags every 50 commits and an unmerged `side` branch are in packed-refs. Scale estate: 600 commits, 1,500 source files and 150 revisions of 9 MiB, in four packs of 37.5 to 45.9 MB plus 258 loose objects (69.9 MB). Scale small: 40 commits, 60 source files and 10 revisions of 512 KiB. |
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

### Not modelled, and what that costs the measurements

The corpus models a well-behaved estate. Each state below is absent, so
the run that would exercise it is not measured on this corpus. The bulkload
behaviour cited is that of `git_carry.rs` and `transfer.rs` at main 46587af.

| Not modelled | What bulkload does with it | Effect on S1, S3 and S4 measured here |
|---|---|---|
| Shallow repository | Capture records the boundary and reports `reuse_unavailable=shallow` on a rerun. | S3 never sees the shallow-reuse loss. |
| Submodule or other gitlink | Recorded as `NestedRepository` custody; the gitlink stays in the staged tree. | S4 never sees gitlink custody. |
| Merge, cherry-pick, revert, rebase or bisect in progress; an unmerged index | An unmerged index (a stage flag) refuses; a nest with an operation in progress counts as dirty. | S4's refusal and disposition path is never exercised. |
| assume-unchanged, skip-worktree, sparse checkout | An index entry flag other than intent-to-add refuses `GIT_INVENTORY_MALFORMED`. In a nest, any such flag, a sparse checkout or a split index means status may miss a change, so the nest is not believed clean. | As above: no refusals. |
| Clean and smudge filters, LFS | A nest whose configuration sets a filter command refuses `GIT_INVENTORY_MALFORMED`; every filter driver is also neutralised for the nest's status. | Neither the refusal nor filtered content is measured. |
| Dirty nested repository | Refused (R-N73). | Only one clean nest per eight repositories is modelled: no nest refusal. |
| Locked or prunable worktree administration | Not checked here. | Neither is measured. |
| Repositories outside `git/` (a dotfiles repository, `~/.cache/nix` tarball-cache) | Carried by the same git path once planned. | Discovery and planning of scattered repositories is not measured. |
| `node_modules` or a large `target/` inside a checkout | The default `CapturePolicy` records `REBUILDABLE_DIRECTORIES` roots as custody, by name and size, and does not carry them. | Custody recording is under-represented: each checkout has a `target/` of 3 to 12 files of at most 32 KiB. |
| `projects/web-*/node_modules` outside every repository | Carried as ordinary walk seats: 26,445 files at scale estate. | S1 carries files a real estate often holds inside a checkout and omits as custody. Report S1 per class (`node-modules`). |
| Large ref counts (#48 measured 119,761 refs and carry refs) | Ref inventory is part of every capture key. | Ref-inventory cost is under-represented: tens of refs per repository. |
| Hardlinks, holes, xattrs, ACLs | Not modelled by the manifest. | None. |
| FIFOs, sockets, devices | Refused as non-regular seats. | None. |
| Names with control or format characters | Refused as `PATH_NOT_PORTABLE`. | None. |
| Case-colliding pairs, NFD names | The tree must stay legal on APFS. | None. |
| Reftable, SHA-256 repositories | Not checked here. | Neither is measured. |

A v1.1 hazard set (a shallow clone, a gitlink, a merge in progress, a
skip-worktree entry, a dirty nest, locked and prunable worktrees, a
top-level dotfiles repository, and `node_modules` inside a checkout) is the
candidate fix. It would sit behind a flag and stay out of the S1 timing set.
It is not built in this phase.

## Exact counts per scale

Counts are from the manifest, so they are the same for every copy. `bytes` is
the sum of the manifest's size column. In that column, a worktree `.git` file
or `gitdir` counts at its root-normalised size.

| Count | small | estate |
|---|---:|---:|
| entries | 1,707 | 142,321 |
| regular files | 1,116 | 123,420 |
| directories | 573 | 18,310 |
| empty directories | 19 | 76 |
| symlinks | 18 | 591 |
| bytes | 18,687,873 | 4,189,857,196 |
| files ≤ 4 KiB | 952 | 101,782 |
| files ≥ 1 MiB | 8 | 320 |
| `.git` directories (non-bare repositories, nested included) | 5 | 28 |
| bare repositories | 1 | 4 |
| linked worktree administrations | 4 | 26 |
| worktree `.git` files | 4 | 26 |
| git indexes | 9 | 54 |
| loose objects | 190 | 18,803 |
| packs | 7 | 24 |
| JSONL / JSONL.zst | 7 / 1 | 801 / 24 |
| SQLite databases / WAL images | 4 / 1 | 11 / 1 |
| `host_scan_skipped` (files above 64 MiB) | 0 | 7 |

`host_scan_skipped` counts the files above the 64 MiB scan limit. At scale
estate there are 7: the five `data/` files, and the `r00` and `r12` packs
that hold a 64 MiB blob. The history packs are now below the limit and are
scanned.

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
| `git-history` | 131 / 103 / 28 / 0 / 3,245,855 | 2,008 / 1,787 / 221 / 0 / 256,705,115 |
| `git-worktree` | 224 / 182 / 38 / 4 / 3,625,244 | 31,711 / 30,141 / 1,544 / 26 / 484,390,779 |
| `large-data` | 7 / 3 / 4 / 0 / 6,291,476 | 10 / 5 / 5 / 0 / 2,113,933,397 |
| `node-modules` | 281 / 226 / 52 / 3 / 176,520 | 33,388 / 26,445 / 6,410 / 533 / 20,200,047 |
| `projects` | 16 / 11 / 5 / 0 / 13,328 | 42 / 33 / 9 / 0 / 38,447 |
| `sqlite` | 5 / 5 / 0 / 0 / 1,667,776 | 12 / 12 / 0 / 0 / 180,581,128 |

**Report S1 and S3 per class.** At scale estate, `large-data` is 50% of
the bytes in 5 files, and the four history packs another 4% in 4 files. A
whole-tree S1 ratio is mostly sequential large-file IO, which R23 already
covers, and a per-entry regression in the 123,000 small files barely moves
it. So every S1 sample on this corpus also reports the subset without
`data/` and `git/history-heavy` (the `large-data` and `git-history`
classes): at scale estate, 140,303 entries and 1,819,218,684 bytes. A tool
is timed on that subset by excluding those two paths.

### Repositories

Small scale: three repositories plus the history repository.

| Repository | Packing | Source files at start | Commits | Other state |
|---|---|---:|---:|---|
| `r00-delta` | mixed | 35 | 11 | branch, detached and agent worktrees; 2 stashes; dirty index; nested `vendor/offset-lib`; commit-graph; 1 MiB blob |
| `r01-row` | loose | 35 | 12 | branch worktree |
| `r02-head` | packed | 34 | 10 | 2 stashes; bare mirror `git/mirrors/r02-head.git` |
| `history-heavy` | four packs plus loose | 60 | 40 | 10 `model.bin` revisions of 512 KiB, 6 of them stored as deltas; tag `v1.0.0`; `side` |

Estate scale: 24 repositories plus the history repository.

- **Packing:** 8 each of mixed, loose and packed.
- **Languages:** 6 each of rs, py, ts and go.
- **Generated source files** at the initial commit: 29,070 in total, 371 to
  2,337 per repository. Each repository also has 8 skeleton files, or 9 with
  the blob.
- **Commits** reachable from branches and tags: 721 in total, 17 to 43 per
  repository.
- **History repository:** 600 commits, 1,500 source files, 150 revisions
  of 9 MiB, 4 packs (173,312,424 bytes) and 258 loose objects (69,910,907
  bytes). It has 1,787 files and 256,705,115 bytes.
- **Under `git/`:** 80,934 files and 1,148,773,261 bytes. Without
  `git/history-heavy`, `git/` holds 79,147 files and 892,068,146 bytes, of
  which 19,352 files and 190,133,102 bytes sit inside git directories, the
  bare mirrors included. Those totals are unchanged from v1 of 2026-10-03.
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
| `git/` | 80,934 | 1,148,773,261 |
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

Every manifest pass scans for host paths. That covers `generate`, `verify`,
`mutate` and `seal`. It reads every regular file up to 64 MiB and every
symlink target. The markers come from the process that runs the pass, not
from the generating host, and SEAL.json does not record them:
- the corpus root and DEST;
- `$HOME`, as given and resolved;
- the generator's checkout;
- the temp directory, as given and resolved;
- the git scratch prefix `estate-corpus-git-`.

Markers shorter than 8 bytes, such as a bare `/tmp` or a `$HOME` of
`/root`, are skipped. They would match generated text by chance.

The one exception is the documented normalisation. A linked worktree's
`.git` file and its administration's `gitdir` are scanned after the root
becomes `@CORPUS_ROOT@`, and one that already holds that token is a
problem. So a leak anywhere else, including the root itself in any other
file, is a problem. It fails `generate` and `verify`, and `mutate` and
`seal` refuse.

The scan has these limits:
- Bytes are scanned as stored. Zlib git objects, packs and `.zst` frames are
  not decompressed.
- Files above 64 MiB are counted as `host_scan_skipped`, not scanned.
- Only the fixed markers are checked. A root-normalised file that holds an
  absolute path outside the corpus, such as `/nix/store/…`, is not flagged.
- The corpus is generated into two different DEST paths and the manifests
  are byte-identical. That proves no DEST-dependent byte enters the corpus
  anywhere, compressed bytes included. It says nothing about `$HOME` or the
  checkout, which were the same for both runs.

Generated text names a modelled home, `/home/agent`. A host whose `$HOME` is
`/home/agent` would fail the scan.

### Measured identities (sting, x86_64-linux, devShell)

| Scale | Seed | Identity | How it was checked |
|---|---|---|---|
| small | `bulkload-estate-corpus-v1` | `c767aa685c670abc2d406de3c300b4e1118db8581a941de42ceee7808f98aa69` | Generated into two fresh scratch directories by the self-test, the second under a setgid parent, with byte-identical manifests. Also generated under an ISO-8859-1 locale, for the carriability smoke, for the mutate measurement, and for the archive copy in place. Every run gave this identity. |
| estate | `bulkload-estate-corpus-v1` | `586de100483a317afda3b6013ddf0a35c3e3424f57de871a328535bd3d507a93` | Generated twice, in parallel, into two fresh scratch directories, with byte-identical manifests (`cmp`). The archive copy, generated in place afterwards, gave it a third time (`recorded=True`). |

Toolchain for both: git 2.54.0 built with zlib-ng 2.3.3, SQLite 3.53.1,
zstd 1.5.7 and Python 3.12.13. `RECORDED` in the script pins both identities
to the git, zlib, SQLite and zstd versions. With a matching toolchain,
`generate` prints `recorded=True`, or fails on a mismatch. With any other
toolchain it prints `recorded=none`. `verify` does not compare a copy with
`RECORDED`; compare the receipt's `base_identity` with this table.

Superseded:
- `931af5b1…` (small) and `0cb96231…` (estate), recorded on 2026-10-03
  with `*.bin -delta` and fully random `model.bin` revisions. Their archive
  copies remain sealed in place and still verify against their own
  receipts; they are not measurement references any more.
- `b3645454…` (small) and `0bd1104e…` (estate), recorded before the
  history repository was wired in (58b1c2e to 7b75b26). No archive copy
  was made of them.

## Bytes that are not a function of the seed

| Source | Where | Handling |
|---|---|---|
| Stat data | Each entry of `.git/index` and `.git/worktrees/*/index` (ctime, mtime, dev, ino, uid, gid, size, and the trailing checksum over them) | **Normalised** (`norm=git-index`). The trailing SHA-1 is checked first, since `index.skipHash` is pinned false: a corrupt one, which `git fsck` refuses, is a problem. Then the digest is over the index with the stat words zeroed and the checksum dropped; mode, oid, flags, path and TREE stay. A stat-bearing extension (UNTR, FSMN, link) is a generation problem. Both index v2 and v3 occur: the intent-to-add entry sets an extended flag, so `r00`'s index is v3. The raw index differs between copies; the identity does not. |
| Absolute paths | A linked worktree's `.git` file and `.git/worktrees/*/gitdir` | **Normalised** (`norm=root`). The recorded root becomes `@CORPUS_ROOT@`. Bulkload resolves a worktree's back-pointer absolutely (`git_carry.rs`), so `--relative-paths` worktrees are not used. Any other file that contains the root, or any other host path, is a problem (see [Host-path scan](#host-path-scan)). |
| Wall-clock dates | Commits, tags, stashes and every reflog line, including the reflog from `checkout` and `worktree add` | **Made deterministic.** Every git call gets the next value of a logical clock in `GIT_AUTHOR_DATE` and `GIT_COMMITTER_DATE`. |
| Wall-clock expiry | Reflog and prune expiry (`gc`) | **Made deterministic.** Set to `never`, and no `git gc` runs; packing uses `repack -a -d`, `repack -d` and `pack-refs --all`. |
| Thread count | Pack delta search; IEOT and EOIE index extensions | **Made deterministic** with `pack.threads=1` and `index.threads=1`. |
| Caches | Untracked cache, fsmonitor, split index, bitmaps, auto gc and maintenance | **Off.** |
| Platform config | `.git/config` (`ignorecase` and `precomposeunicode` on macOS) | **Rewritten** to the Linux `git init` text. |
| Git version | Template hooks and `description` | **Made deterministic** with an explicit `--template`, which also means no sample hook varies by version. |
| Environment | Global and system git config, attributes, umask, locale, DEST parent | **Isolated:** `GIT_CONFIG_GLOBAL` points at a private file, with `GIT_CONFIG_NOSYSTEM=1` and `GIT_ATTR_NOSYSTEM=1`. HOME is private and inherited `GIT_*` variables are dropped. UTF-8 mode, umask 022, explicit DEST modes, `canonical_modes`, and a refused default ACL (see [Generator](#generator)). |
| `sqlite3_randomness` | WAL salt-1 and salt-2, and every frame checksum | **Rewritten** to seed-derived salts with the checksums recomputed. The image is then proved: a copy opens with `integrity_check` ok and the expected row count. Without the rewrite, two runs give different `-wal` bytes; with it, they are identical. |
| SQLite version | DELETE-mode headers (offset 96) | **Toolchain caveat.** Byte-stable for one SQLite (3.53.1). |
| zstd version | `.jsonl.zst` frames | **Toolchain caveat.** Byte-stable for one zstd, with `-3 --single-thread`. |
| zlib-ng and git versions | Loose-object and pack bytes, pack names, `.idx`, `.rev`, commit-graph | **Toolchain caveat.** Byte-stable for one git and zlib-ng. |
| Filesystem | mtimes, ctimes, inodes, directory sizes | **Excluded** from the manifest. Mtimes are left as git set them, so no index is racily clean. |

## S2: what verify cannot see

The identity is blind to every write that leaves the manifest rows as they
were: an index refreshed under `index.lock` (the stat words are
normalised), a rewrite with the same bytes (a new inode), and an mtime or
ctime touch. So **`verify` ok does not mean that nothing wrote the
source.** The self-test shows it: after `git status` refreshes an index,
`verify` is still ok.

For S2, take a stat snapshot before each pass and diff it after:

```text
estate_corpus.py s2-snapshot DEST OUT        # OUT outside DEST/corpus
<the pass>
estate_corpus.py s2-diff DEST OUT            # exit 0 only if nothing moved
```

The snapshot records every entry's path, type, mode, inode, size, mtime and
ctime, the corpus root and any `-shm` included. `s2-diff` lists every entry
added (`+`), removed (`-`) or changed (`~`, with the fields that moved). A
read changes nothing it records; atime is not recorded.

Keep bulkload's private state, ledger and destination outside `DEST/corpus`.
The carriability smoke below found two source writes this way, neither
visible to `verify`.

## Mutation knob for the S3 harness

`mutate DEST N` applies N operations. The first three are always, in this
order, `history`, `sqlite` and `large-edit`; the rest cycle through the
other eight kinds in a seed-shuffled order. So every round of N ≥ 3 moves
history-heavy, changes SQLite and overwrites part of a large file, and
N ≥ 11 runs every kind. Targets are seed-chosen from the sorted manifest.

| Kind | Effect |
|---|---|
| `history` | A commit on `git/history-heavy`'s checked-out `main`: one source file edited, staged and committed (#48: the large history's authority moves every round). |
| `sqlite` | INSERT three rows and UPDATE one in a DELETE-mode store through the `sqlite3` API, then a WAL-mode commit of five rows appended to the WAL image: `wal_autocheckpoint=0` and `SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE`, so `storage.db` itself is untouched and the new frames continue the salts and checksums. A copy is then proved to recover every row. |
| `large-edit` | Overwrites 64 bytes at a seed-chosen, 64-aligned offset of a file of at least 64 MiB (1 MiB at scale small): a `data/` file or a tracked `model.bin`. |
| `edit` | Overwrites 64 bytes in place; size is unchanged. |
| `append` | Appends a JSONL line. |
| `new` | Creates a file. |
| `delete` | Removes a file of at most 1 MiB. |
| `commit` | Adds a new commit on an unmerged `side` branch, through plumbing: `hash-object`, a temporary index, `commit-tree` and `update-ref`. |
| `head-move` | Runs `checkout --detach` in a detached worktree. |
| `repack` | Runs `repack -a -d` in a repository other than history-heavy: the same objects, consolidated into a new pack. |
| `fetch` | A new upstream commit arrives as a new pack and moves `refs/remotes/upstream/main`. It is built in a private object directory and stored with `pack-objects` and `index-pack`, as `fetch` stores a pack, so no URL or host path enters `FETCH_HEAD` or a reflog. |

- **Target rules:** file operations never touch a git directory, a
  detached worktree, a nested repository's work tree, a `.gitignore` or
  `.gitattributes`, or a read-only file. Only `sqlite` touches a SQLite
  file or the `-wal`, and no operation touches a `.zst` file.
- **Refusals before any change:** `mutate` refuses a sealed DEST, a DEST
  that does not match its seal, and an N its pools cannot serve (each
  target is drawn at most once per round).
- **Determinism:** round *r* is deterministic from (seed, scale, *r*), and
  its git clock starts at 2026-10-03 + *r* days. Two runs of a round give
  the same sidecar, apart from `timing`.
- **Pre-round manifest:** each round keeps the manifest it started from as
  `DEST/mutations/round-RRRR.before.tsv`.
- **Sidecar:** each round writes `DEST/mutations/round-RRRR.json`, which
  holds:
  - `operations`: per file operation its `pre_size`, `post_size` and
    changed byte `ranges`; per ref move its `ref`, `old` and `new` tips;
    for `sqlite`, each store's changed page ranges;
  - `identity_before` and `identity_after`;
  - `changed`, the exact manifest difference: added, removed and modified;
  - `stat_only`, entries whose row is unchanged but whose inode, size, mtime
    or ctime moved, such as a directory whose listing changed or a rewritten
    file;
  - `reads_allowed`, the regular files among those, with
    `reads_allowed_sizes` and `reads_allowed_bytes`;
  - `reads_by_class` and `reads_allowed_class`: those files split into
    `walk` (outside every git item), `worktree` (an item's checkout seats,
    per item), `rebuildable` (below a `REBUILDABLE_DIRECTORIES` root of a
    checkout, custody under the default `CapturePolicy`), `git-objects` and
    `git-admin` (the rest of a git directory: refs, reflogs, HEAD, index);
  - `changed_range_bytes`, the bytes the operations changed, and
    `changed_content_bytes`, the whole sizes of the added and modified files;
    `cdc_bytes` holds bulkload's CDC minimum, average and maximum chunk
    sizes (16, 64 and 256 KiB);
  - `items`: bulkload's estate items, one per main checkout, linked
    worktree, nested repository and bare mirror (R-N114); the ones whose
    capture key changed, with the paths that changed them; and
    `census_walks_expected`;
  - `git_objects`, per repository: loose objects and packs added and
    removed, with their bytes, and each ref move's new objects (count,
    size and stored size);
  - `timing`: `mutated_at_ns` and `settle_ns` (2 s, bulkload's
    `RACY_GRANULARITY_NS`).
- **Seal:** `SEAL.json` and `MANIFEST.tsv` move to the new identity, so
  `verify` passes after a mutation of an unsealed copy. If a problem shows
  after the operations (a host path, say), they do not move, the corpus no
  longer verifies, and it must be regenerated.

### Which items change

`items` models bulkload's capture key (`git_carry.rs` `KeyParts` at main
46587af):
- a checkout seat changes its own item;
- a per-worktree file (`HEAD`, `index`, `logs/HEAD`, `ORIG_HEAD`) changes
  the item whose git directory holds it;
- the ref inventory (`refs/`, `logs/refs/`, `packed-refs`), `config`,
  `info/exclude` and `shallow` change every item of that repository;
- `objects/` changes no item, since the pack listing is drift evidence and
  not part of the key: a `repack` alone is a reuse hit;
- a nested worktree's or nested repository's `HEAD` also changes the
  checkout around it;
- a change below a rebuildable root changes no item under the default
  policy (`changed_include_rebuildable` lists the items it would change
  under `--include-rebuildable`).

`census_walks_expected` is 4 per changed item and 1 per unchanged one, as
`tests/git_capture_counters.rs` asserts (#144). A bare mirror counts for
neither, because `estate-capture` refuses a bare repository (see the
smoke). The carriability smoke checked this model against bulkload: after
small round 1, the seven items the sidecar named were re-captured, the
other two were reuse hits, and `census_walks` was 30, the sidecar's
expectation.

### How the S3 harness uses the sidecar

- **Inequality 1** (OI-1003-Q18): `source_bytes_read` ≤ the `walk` and
  `worktree` bytes of `reads_by_class`, plus `rebuildable` under
  `--include-rebuildable`, plus racy seats. `git-objects` and `git-admin`
  are not file seats: the git path reads them through git, so they are
  not part of this bound. A changed file may be read whole to find its
  changed chunks, so the bound uses whole sizes.
- **Inequality 2** (wire ≤ absent chunks): per file, the absent chunks are
  the CDC chunks that overlap its `ranges`, with the boundary shift CDC
  allows after an edit. The harness computes them from the ranges and
  `cdc_bytes`; the sidecar does not. Never use `changed_content_bytes` as
  a wire bound: it counts whole files, so a build that resends a whole
  JSONL file for a 200-byte append would pass.
- **Pack bytes:** with the prerequisite, a changed item's capture packs
  the round's new objects (`git_objects`), its new staged and worktree
  content, and the capture's own metadata, never the history again. An
  unchanged item, or a repository that was only repacked, packs nothing.
- **Census walks:** `census_walks_expected`.
- **Racy seats:** wait until `mutated_at_ns + settle_ns` before the pass.
  A seat changed less than 2 s before the pass starts is racy, and a racy
  capture is sent but never recorded as a reuse key (R-N76).

### Measured rounds

| Run | Added | Removed | Modified | `stat_only` | `reads_allowed` | `reads_allowed_bytes` | Items changed / unchanged | `census_walks_expected` | Identity |
|---|---:|---:|---:|---:|---:|---:|---|---:|---|
| small, round 1, N=11 | 35 | 36 | 18 | 21 | 39 | 3,824,784 | 7 / 3 | 30 | `c767aa68…` → `dfa242bc…` |
| estate, round 1, N=12 | 38 | 703 | 164 | 66 | 192 | 81,901,677 | 6 / 52 | 72 | `586de100…` → `fedc6ca6…` |

- At scale small, the self-test's two copies, the smoke copy and the
  measurement copy all reached the same identity after round 1. `stat_only`
  is 20 instead of 21 when a WAL-aware read has already left a `-shm`
  beside the WAL image: the round's own `-shm` then does not change that
  directory's listing.
- Small round 1, by class: `walk` 5 files and 3,731,471 bytes (the 3 MiB
  `disk.img` that `large-edit` touched dominates), `worktree` 4 files and
  7,933 bytes, `git-objects` 17 files and 58,116 bytes, and `git-admin` 13
  files and 27,264 bytes. `changed_range_bytes` is 47,813.
- Estate round 1 ran `history` twice (ops 0 and 11, as N=12 wraps the
  cycle). It changed six items: history-heavy, `r05` (the `side` commit),
  `r11` (the fetch), `r12` (`large-edit` on its tracked 64 MiB `model.bin`),
  `r23` (an edit, a new file and a delete) and `r16`'s detached worktree
  (the head-move). The `repack` of `r06` changed no item. Of 58 items, 54
  are censused (4 bare mirrors are not): 6 × 4 + 48 × 1 = 72.
- Estate round 1, by class: `walk` 3 files and 12,197,659 bytes, `worktree`
  154 files and 67,488,560 bytes (the 64 MiB `model.bin` and the
  head-move's rewritten files), `git-objects` 22 files and 1,752,743 bytes,
  and `git-admin` 13 files and 462,715 bytes. `changed_range_bytes` is
  37,145. The `sqlite` op changed 3 pages of an 11,259,904-byte store and
  appended 8,240 bytes of frames to the WAL image.
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

Sprint 2 measures at three builds: adb9c66, the counters build (#144, main
04ea9cb), and #146 (main 46587af). The corpus supports that in four ways:

1. **Same input at every build.** Generation is deterministic, and
   `generate` prints `recorded=True` when it reproduces the identity in
   `RECORDED`: `c767aa68…` (small) or `586de100…` (estate). Regenerate in
   place per build; the corpus is not relocatable, and the sealed archive
   copy cannot serve a WAL-aware open (see below).
2. **Unchanged rerun.** A first pass may create `storage.db-shm` beside the
   WAL image. When it is exactly that image's wal-index it is reported
   (`shm_ignored`) but never sealed, so `verify` still passes and the rerun
   starts from the sealed identity. The unchanged-estate clause expects 0
   content bytes against this state. The smoke's unchanged rerun gave 9
   reuse hits, `census_walks=9` and no pack bytes.
3. **Delta rerun.** `mutate DEST N` is deterministic per round. Every round
   moves history-heavy, so the #144 regression (a changed rerun without a
   prerequisite packs all of history again) re-packs history-heavy's whole
   object store in every round: 243 MB on disk at scale estate (173 MB in
   packs, 70 MB loose). A build with the prerequisite packs only the
   round's new objects (`git_objects`) and new content. Every
   round also changes a DELETE-mode store and the WAL image (the
   provider_sqlite delta) and overwrites 64 bytes of a large file (the
   "reads only the changed chunks" clause). The sidecar gives the bounds
   [above](#how-the-s3-harness-uses-the-sidecar).
4. **CPU ratio.** At scale estate the corpus is large enough that the rusage
   CPU of both the first pass and the rerun can be measured. It has
   142,321 entries and 4.19 GB, with 32 git directories, 54 indexes,
   18,803 loose objects and 24 packs. The history's `model.bin` revisions
   are deltas of each other, so a full pack of history-heavy pays a real
   delta search, as a v1 rerun without a prerequisite would.

## Seal and archive copies

`seal DEST` runs in place only, on a DEST that verifies and holds no SQLite
`-shm`. It then does four things:
- **Modes.** Every file becomes 0444, or 0555 if it has an execute bit.
  Every directory becomes 0555, DEST, `corpus/` and `mutations/` included.
  The original modes (0600, 0640, 0700, 0755) stay in `MANIFEST.tsv`.
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
- read-only modes everywhere: DEST, `corpus/`, every entry, and DEST's
  top-level files and sidecars;
- the original identity in `MANIFEST.tsv`;
- no mutation round after the seal.

A sealed copy exempts no `-shm`: no reader can create one in a 0555
directory, so any `-shm` is an added entry. `mutate` refuses a sealed copy
outright. So a sealed copy cannot change without an explicit chmod, and any
change found in `corpus/` fails `verify`. The self-test checks a junk
`-shm`, a copied valid wal-index, one added file, one removed file, a
same-size edit and a writable `corpus/`. The receipt, `SEAL.json`,
`README.md` and the sidecars are not signed or hashed: a writer who chmods
the copy and rewrites them consistently is not detected. Compare the
receipt's identities with this doc.

The archive copies were generated in place, not copied: linked worktrees
hold absolute paths. Then they were sealed and verified.

| Scale | DEST (in place) | Identity | `readonly_identity` | Receipt | `verify` |
|---|---|---|---|---|---|
| small | `/srv/data/jess/archive/bulkload-evidence/estate-corpus-c767aa685c67/` | `c767aa68…` | `3d4362ba1f9ce20b5bab3cbe026b9d3d71a2cbb9507f3cad14f1c57018d46443` | `VERIFY-RECEIPT.json`, sealed 2026-10-04T05:33:13Z | ok: `sealed=1 relocated=0 shm_ignored=0` |
| estate | `/srv/data/jess/archive/bulkload-evidence/estate-corpus-586de100483a/` | `586de100…` | `36b32e17ec90f2178f64ab47f071055f3c04fc7cd83cd6073caadcdeea8a1b46` | `VERIFY-RECEIPT.json`, sealed 2026-10-04T05:46:23Z | ok: `sealed=1 relocated=0 shm_ignored=0` |

Seal and verify times are in [Generation cost](#generation-cost-informational-not-a-gated-sample).
Each receipt is the DEST's `VERIFY-RECEIPT.json`, and it records the same
identities, `"rulings": "OI-1003-Q19, OI-1003-Q23, OI-1003-Q35, R-N13"`, and
`generator_shake256` `4d9d1350bd0b`… (the committed script).

The 2026-10-03 copies, `estate-corpus-931af5b130fe/` and
`estate-corpus-0cb96231438c/`, stay in place, sealed, as superseded
references. Both still verify against their own receipts with the new
script (`8dfa9965…` and `b7590ca0…`, ok). Removing them is left to the
operator.

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

Run on 2026-10-04 on a fresh in-place generation of `c767aa68…` (small) in
`/srv/scratch`, with a release `bulkload-agent` built from main 46587af.
There was no power or load gate (load average 27 to 58), so the times are
not samples.

**Transfer (`copy`).** It carried 1,111 files, 17,020,497 source bytes
read and 17,020,433 bytes materialised, and refused 5 seats, each typed
`SQLITE_STATE_CHANGED`: `.codex/state.sqlite`, `state.vscdb`, `kv.sqlite`,
`storage.db` and `storage.db-wal`. The transfer path refuses any seat named
`*-wal`, `*-shm` or `*-journal`, and any file that starts with the SQLite or
WAL magic. The verb then exits 1 with `CONTRACT_SELF_INCONSISTENT`, which
is how `copy` reports a run with refusals. `s2-diff` of the source was
clean. At scale estate the same rules refuse 12 seats: 11 databases and
`storage.db-wal`.

**SQLite (`snapshot`).** `provider_sqlite::snapshot` (the online backup API)
copied each of the four databases into a 0700 directory: all
`integrity_check` ok, with 508, 543, 760 and 377 rows. The 377 rows of
`storage.db` include its WAL frames. The WAL-aware open created
`storage.db-shm` in the source: `s2-diff` lists it and its directory, and
`verify` accepts it as the exact wal-index (written by the agent's bundled
SQLite 3.46.0). That is the known S2 write for OI-1003-Q16.

**Git (`estate-add-batch`, `estate-capture`).** All ten items were planned
with no workspace: three main checkouts, history-heavy, three linked
worktrees of `r00`, its agent worktree, its nest and the bare mirror.

| Pass | Captured | Reused | Refused | `census_walks` | `write_source_pack_bytes` | Source stat changes (`s2-diff`) |
|---|---:|---:|---:|---:|---:|---|
| first | 9 | 0 | 1 | 36 | 6,977,572 | 83 loose objects and 1 pack: mtime and ctime only |
| unchanged rerun | 0 | 9 | 1 | 9 | 0 | none |
| after small round 1 (N=11), settled | 7 | 2 | 1 | 30 | 46,309 | 7 loose objects and 1 pack: mtime and ctime only |

- **The bare mirror is refused** with `IO (errno 2)`, an untyped refusal,
  on every pass. So a bare repository cannot be an estate item at 46587af,
  and the sidecar's census expectation leaves mirrors out.
- **The census model holds.** The first pass is 4 walks for each of 9
  items, the rerun 1 each. After round 1, the seven items the sidecar named
  were re-captured and the nest and `r02-head` were reused: 7 × 4 + 2 × 1 =
  30, the sidecar's `census_walks_expected`.
- **The prerequisite holds.** After round 1, the capture packed 46,309
  bytes, while history-heavy's packs hold 2.5 MB at this scale.
- **A source write.** Each changed capture changed the mtime and ctime of
  existing loose objects and a pack in the source repositories (history-heavy,
  `r00`, its nest and `r01` on the first pass). Inode, size and bytes did
  not change, so `verify` stayed ok. This looks like git freshening objects
  it finds through the private repository's `alternates`. It is a write to
  the source for WP1 (S2) to rule on; `s2-diff` is how to see it.

**Seats compared in S1.** rclone copies every seat as bytes. Bulkload
carries the whole tree through the transfer path except the SQLite seats
(5 at scale small, 12 at scale estate), which go through `snapshot`. So an
S1 comparison is like for like only on the tree without those seats, timed
for both tools, with the SQLite seats reported separately: rclone's raw
copy against bulkload's `snapshot` of each database. The git path
(`estate-capture`) is a separate measurement, S3's, not S1's.

## Cross-host caveats

- **Toolchain.** The identity is a function of the seed and the toolchain:
  git 2.54.0 with zlib-ng 2.3.3, SQLite 3.53.1 and zstd 1.5.7, as the
  devShell pins them. SEAL.json records the versions. A host with another
  git, zlib-ng, SQLite or zstd produces a different identity from the same
  seed. Generation refuses git older than 2.45 (`--ref-format`).
- **Locale and DEST parent.** The identity does not depend on the locale,
  the umask or a setgid DEST parent. A DEST parent with a default POSIX ACL
  is refused.
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
  `storage.db-shm` beside it, even read-only, and the smoke saw
  `provider_sqlite::snapshot` do so. The generator exempts that file on an
  unsealed copy only when it is exactly the wal-index SQLite rebuilds from
  the `-wal` beside a WAL-mode database: its size, header, salts, frame
  count, and page and hash tables are all checked, and only the 40-byte
  reader marks are free. Any other `-shm` beside a database is an added
  entry and a problem; before this fix, 1,050,000 junk bytes in
  `.codex/state.sqlite-shm` passed `verify`, sealed copy included. `verify`
  reports an exempt file as `shm_ignored=N`, with one note line per file,
  and `s2-diff` lists it as the write it is.
- **APFS.** Names avoid case collisions and NFD, so the tree is legal on
  APFS. The rewritten `.git/config` says `ignorecase` is unset even on a
  case-insensitive volume. That is harmless for this corpus, which has no
  case-only renames.

## Generation cost (informational, not a gated sample)

All on sting, with no load gate; other lanes kept the load average between
27 and 58, so the times are an upper bound. Elapsed times are the script's
own; peak RSS includes `nix develop`.

| Step | small | estate |
|---|---|---|
| `generate`, scratch, twice | 4.4 s each, inside the self-test | 611.5 s and 614.3 s, in parallel; peak RSS 385 MB and 389 MB |
| `generate`, archive, in place | 5.2 s | 541.4 s; peak RSS 385 MB |
| `seal` (two manifest passes) | 0.8 s | 230.3 s; peak RSS 479 MB |
| `verify` of the sealed copy | 5.3 s with `nix develop` | 83.0 s with `nix develop`; peak RSS 538 MB |
| `mutate`, round 1 | 1.9 s for N=11 | 253 s for N=12, with `nix develop`; peak RSS 668 MB |
| self-test | 25.1 s; 21 to 76 s across runs | not run |
