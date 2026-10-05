# blahaj under v1's ref table: carry, exact restore and S2, sting, 2026-10-05

Rulings: OI-1003-Q54 (v1 must carry refs-heavy repositories before
carry_v2 is deleted), OI-1003-Q55 (prove that precondition on the real
blahaj before L5 deletes carry_v2), OI-1003-Q35 (byte counters,
`census_walks`, pack bytes and the rusage CPU are admissible; wall time is
informational), R-N13.

**Label: informational, sting, ungated.** This is one sample. No power or
load gate applied (load1 was 10 to 21 around the passes, from other lanes).
Nothing here is an S1 sample.

## Verdict

**The Q54 precondition is MET, on the shallow-envelope path, from an
empty corpus.** The real blahaj (122,994 refs over 2,382 distinct objects)
carries under v1's new ref-table format (#182, main `40adca8`) and
restores exactly. The proof:

- **Scope.** blahaj is shallow. `shared::write_bundle` and
  `write_chained_capped` return `shallow::write_bundle` before
  `write_full`, so every capture here is a shallow envelope. The run
  exercised the envelope's manifest cap and the ref table's writer and
  reader. It did not reach #182's header-cap code for bundle headers.
  The non-shallow writers (`write_full`'s header sum and the thin-cap
  fallback in `write_excluding_tip_trees`) did not run. The `prerequisites`,
  `skip_header` and `chain::advertised` readers saw at most the 95 B outer
  header. The corpus started empty, so a retained old-format capture was
  not tested (Open 5).
- **It captures.** `estate-capture` exits 0 with outcome `captured` and
  no refusal. The new header equivalent (the envelope's manifest) is
  265,281 B, 1.58 % of the 16 MiB cap. **The old format's outcome on
  blahaj was not observed.** The 24.4 MB figure (Baseline) models the
  non-shallow bundle-header path, which blahaj never takes. From the code
  of main before #182 (`73952f9`; `shallow.rs` as at `3b634ba`),
  `shallow::write_bundle` put the whole `for-each-ref` inventory in the
  manifest with no cap, under a 1-ref outer header. Only the readers
  (`custody_manifest`, on the restore and attach side) capped the manifest
  at 16 MiB, and they refused `BUDGET_EXCEEDED`, not
  `GIT_INVENTORY_MALFORMED`. So the old binary most likely recorded
  `captured` and failed only when the manifest was read.
- **It restores exactly.** All 122,994 refs come back, with every name and
  oid exact: 0 missing, 0 extra, 0 oid differences. HEAD matches by oid
  and by its symbolic branch. Stashes match (none at the source, none
  carried). The 80 nested worktrees' custody matches, HEADs included. The
  shallow frontier matches.
- **The unchanged rerun is a reuse hit.** It wrote 0 bundle bytes and 0
  pack bytes, added 19 B of private state, and used 1.45 % of the first
  pass's CPU. Its 0 source bytes is the hit path's constant, not a
  measurement (see Capture).
- **S2 holds.** The lstat census of blahaj's `.git` is identical before,
  between and after every step: 2,884 entries, the same digest 7 times.

**One documented difference.** blahaj has 2 symbolic refs besides HEAD
(`refs/remotes/github/HEAD` and `refs/remotes/yoga/HEAD`). They come back
as plain refs at the identical resolved oid, as the v1-header plan's
"Restore exactness" section states. Their symbolic targets are not carried.

## Setup

- **Build.** A release `bulkload-agent` of main
  `40adca8043a1bc5075a59979743cdddd5dbadf8f` (PR #182 merged).
  - Built with `s3_estate.py build --rev 40adca8 --jobs 4 --lock
    .check-fast.lock`. The source was exported with `git archive` into
    private scratch, then built with `cargo build --release --locked -p
    bulkload-agent` in that tree's `nix develop`.
  - It ran at nice 19 with `CARGO_BUILD_JOBS=4`, under the lanes' shared
    flock, with its own `CARGO_TARGET_DIR`.
  - Queued 14:27:06Z; cargo finished in 3 min 31 s (14:30:45Z).
  - sha256 `74a6e0ce06e1f5614cd54c4955a3fb4142d4e4f8c796d5e437d71f1271493ad8`.
- **Host.** sting: 32 CPUs, Linux 6.12, git 2.52.0, python 3.12.14.
  Scratch is `/srv/scratch` (xfs over LVM), in a private 0700 directory.
- **Source.** `/srv/fast-local/jess/git/blahaj`, strictly read only.
  - Every git command the harness ran there is a read: `for-each-ref`,
    `rev-parse`, `symbolic-ref`, `worktree list` and `stash list`, with
    `GIT_OPTIONAL_LOCKS=0`.
  - The census only `lstat`s `.git`.
  - The agent's own reads are what S2 tests.
- **Verbs.** These are the ones `s3_estate.py` uses for the git half at
  `40adca8`:
  - `estate-add-batch` of a one-item plan: source blahaj, destination
    repository `<scratch>/dest/blahaj.git`, no workspace (`-`);
  - `estate-capture PLAN PRIVATE_STATE CORPUS 2`;
  - then `estate-apply PLAN CORPUS APPLY_STATE blahaj-q54 1`.

  Every child ran under s3_estate's `Runner`, which records
  RUSAGE_CHILDREN deltas and keeps raw output. Its environment is
  s3_estate's `git_env`: no inherited `GIT_*`, an empty global config and
  no system config. The agent ran at its default background class (every
  capture counters line says `priority=background priority_from=default`).
- **Restore.** Refs only, into a fresh `git init --bare` destination. A
  workspace restore would have written blahaj's carried working files
  (about 6.0 GB, the bytes pass 1 read) into shared scratch and was not
  run (see Open).
- **Harness.** [`2026-10-05-blahaj-v1-carry-harness.py`](2026-10-05-blahaj-v1-carry-harness.py),
  sha256 `e7597a76…` (`census t0` ran `576abfb7…`, which differs only by the
  per-step sha line). **Record:**
  [`2026-10-05-blahaj-v1-carry.json`](2026-10-05-blahaj-v1-carry.json). It
  holds every step, every counters line, the raw logs and the build record.

## Baseline (read-only, 14:31:10Z to 14:31:32Z)

| Item | Value |
|---|---|
| Refs (`for-each-ref`) | 122,994 (22,947,396 B listing, sha256 `ba95aa7f…`) |
| Canonical carry refs / native refs | 119,761 / 3,233 |
| Distinct target objects | 2,382 |
| Symbolic refs besides HEAD | 2 (`refs/remotes/github/HEAD`, `refs/remotes/yoga/HEAD`) |
| HEAD | `bb62d090…` on `refs/heads/backup/glorious-build-tenant-pre-rebase` |
| Stashes | 0 |
| Worktrees | 121: the main checkout, 80 nested inside it, 40 outside it |
| Object format, shallow | sha1; shallow, a 1-commit frontier |
| Old-format header, modelled (non-shallow path) | 24,439,443 B ref term + 17 B = 24,439,460 B, 7,662,244 B over 16 MiB |

The modelled old header matches L3's read-only model
([q42 probes](2026-10-04-q42-probes.md)) to the byte. It models a
non-shallow bundle header (`write_full`), which blahaj, being shallow,
never writes. It is a size reference for the ref term, not blahaj's
old-format outcome (Verdict). A second baseline
after both captures had the same ref-set digest, HEAD, stashes, symrefs and
worktree list, so the source did not move under the run.

## Capture

| Pass | Outcome | Source bytes read | New bundle bytes | Pack written | CPU s (user + sys) | Wall s | `census_walks` | Refusal |
|---|---|---:|---:|---:|---:|---:|---:|---|
| `pass1`, 14:31:41Z | `captured` | 6,008,771,154 | 229,677,508 | 236,378,653 B, 63,079 objects | 61.08 (50.68 + 10.40) | 57.53 | 4 | none |
| `pass2` (unchanged rerun), 14:32:57Z | `capture-reused-after-census` | 0 | 0 | 0 | 0.885 (0.480 + 0.405) | 0.81 | 1 | none |

- **S3's unchanged clause: the ratio is measured, the zero read is not.**
  The rerun's CPU is **1.45 %** of the first pass's (≤ 10 %); wall time is
  1.42 % (informational).
  - **Its 0 source bytes is a constant.** On the `Retained::Hit` path,
    `estate::capture_item` returns
    `Completion::clean("capture-reused-after-census")`, which sets
    `bytes_read` to 0. The receipt's `source_bytes_read=0` is that
    constant.
  - **The process counter is blind here.** `read_source_file_bytes`
    cannot see the git half's reads: it is 0 on pass 1 too, whose receipt
    says 6,008,771,154 B.
  - **Only CPU and wall time bound it.** No recorded instrument would have
    shown a nonzero content read during pass 2's key computation. The
    physical bound is CPU (0.885 s against pass 1's 61.08 s) and wall
    time (0.81 s). That rules out re-reading and re-hashing the 6.0 GB
    pass 1 read, but not a small read. A measured zero needs an
    independent read measure (Open 6).
  - **What pass 2 wrote.** 0 bundle bytes and 0 pack bytes: the corpus
    has no new or changed file and stays at 229,677,780 B. It added 19 B
    of private state (709,758,497 B to 709,758,516 B), with
    `flush_full_count=1` and `flush_dir_count=3`.
- **No chained link was written, by design.** An unchanged capture is a
  reuse hit, so there is nothing to chain. blahaj is also shallow: a
  shallow checkout is always written as a whole envelope, never as a thin
  link (`shared::write_bundle`). See Open.
- **pass1 read 6.0 GB for a 230 MB bundle.** Most of blahaj's working
  bytes are 911 files under `tofu/state`, ignored OpenTofu provider caches
  in `.<name>.terraform` directories. Those names are not in the
  rebuildable set (only `.terraform` is), so they are carried. They hold
  only 4 distinct large binaries, which deduplicate as blobs. pass1 also
  read 217,313,280 B back from the pack (`read_source_pack_readback_bytes`).
- **Where the source read is counted.** The 6.0 GB is in the item
  receipt's `source_bytes_read`, which is what `s3_estate.py` sums for the
  git half. The process counters line reports `read_source_file_bytes=0`
  for `estate-capture`.

### Header and table

blahaj is shallow, so its bundle is a shallow envelope. The ref inventory
lives in the envelope's manifest, which has the same 16 MiB cap
(`GIT_INVENTORY_OVER_CAP`).

| Measure | Bytes | Notes |
|---|---:|---|
| Outer bundle header | 95 | `# v2 git bundle`, 1 ref line (`refs/carry-export/shallow-custody-v1`), 0 prerequisites |
| Manifest (frontier + inner inventory + pack oid) | 265,281 | 1.58 % of 16,777,216 |
| Inner inventory | 265,195 | 2,393 lines: 2,382 `ref-tip-v1/<oid>`, 1 `refs/carry-ref-table/v1`, 10 metadata |
| Old-format header, modelled | 24,439,460 | non-shallow path, which blahaj never takes; over the cap there. blahaj's old-format outcome was not observed (Verdict) |
| Ref table, raw | 11,424,561 | 158 blobs (native + 157 snapshot namespaces) |
| Ref table, packed | 219,784 | blobs 200,212 + trees 19,466 + commit 106: about 1.8 B per carried ref |

The inner inventory matches the plan's prediction: about 266 KB, at
111 B per distinct object. It is **92 times smaller** than the modelled
per-ref term. Before #182, the shallow manifest carried one such line per
private ref with no cap: about 24.4 MB by the same model, not measured.
The private repository wrote 2,393 refs, not 122,994.

## Restore and exact comparison

`estate-apply` took 35.95 s wall and 38.39 s CPU (21.88 user, 16.51 sys),
with outcome `refs-imported` and no refusal. It staged and hashed the
229,677,508 B bundle once. The destination holds 63,083 objects in 2 packs
(456,859 KiB). An independent `git fsck --connectivity-only --no-dangling`
of the destination is clean; its only notice is the bare repository's
unborn HEAD.

The restored ref set was mapped back and compared with the `before`
baseline:

- a canonical ref keeps its name (`refs/carry/v1/<tail>`);
- a native ref `refs/X` arrives as `refs/carry/v1/blahaj-q54/<digest>/refs/X`;
- a stash arrives as `.../stashes/<oid>`.

The excluded refs are only the 10 documented capture metadata refs:
`configuration-v1`, `exclude`, `filesystem-v1`, `head`, `head-symbolic`,
`nested-worktrees-v1`, `rebuildable-omissions-v1`, `shallow-frontier-v1`,
`staged` and `worktree`.

| Check | Result |
|---|---|
| Restored refs | 123,004 = 119,761 canonical + 3,233 native + 10 metadata; one snapshot digest (`6bad6325…`) |
| Names | 0 missing, 0 extra |
| Oids | 0 differences over 122,994 refs |
| `ref-tip-v1` refs imported | 0; no ref outside `refs/carry/v1/` |
| HEAD | `head` = `bb62d090…` = source; `head-symbolic` = `refs/heads/backup/glorious-build-tenant-pre-rebase` = source |
| Stashes | source none, carried none |
| Nested worktrees (`nested-worktrees-v1`) | 80 carried = 80 in `worktree list` inside the checkout; same paths, same HEADs |
| Shallow frontier | destination `shallow` = source (`bf9acf31…`) |
| Symbolic refs besides HEAD | 2, restored as plain refs at the identical oid: `github/HEAD` = `github/main` = `f0e481de…`; `yoga/HEAD` = `yoga/main` = `bf9acf31…` |

**Exact restore: yes**, for names, oids, HEAD, stashes, nested-worktree
custody and the shallow frontier. The one difference is the two symrefs'
symbolic targets, which v1 does not carry (documented design). The 40
worktrees outside the checkout are their own estate items and were not in
this plan.

## S2: no write to blahaj

The lstat census covers blahaj's `.git` and everything under it: 2,884
entries, recording mode, size, mtime_ns, ctime_ns, ino and nlink. It was
taken 7 times and was identical every time (sha256 `136cb1bd…`; the
JSON holds the field-by-field diffs t0..t1, t1..after-pass1,
after-pass1..after-pass2, t0..final and t0..post-cleanup, all empty):

| Census | Taken | Identical to t0 |
|---|---|---|
| `t0` (before anything) | 14:31:10Z | n/a |
| `t1` (after the baseline reads) | 14:31:32Z | yes |
| `after-pass1` | 14:32:50Z | yes |
| `after-pass2` | 14:33:17Z | yes |
| `t-after-baseline` (after the second baseline) | 14:33:21Z | yes |
| `final` (after apply, compare and fsck) | 14:34:33Z | yes |
| `post-cleanup` (after the lane deleted its scratch) | 14:36:10Z | yes |

**S2 census identical: yes.** Any create, delete, rename, write or lock
file in `.git` changes some entry's mtime or ctime, or a directory's. The
census excludes atime, which reads update.

It covers `.git`, not the working tree. The pass-2 reuse hit means the
capture key, which hashes the working-tree census, did not change between
the passes, but no lstat census of the working tree was taken.

## Open

1. **A changed rerun of a shallow checkout is expensive (from the code;
   not measured).** For a shallow source, `offered_reuse` returns
   `reuse_unavailable=shallow`, and `shared::write_bundle` writes a whole
   envelope. A changed blahaj capture would therefore re-read every seat
   (about 6.0 GB here) and write about 230 MB per pass. That is an S3
   delta cost of shallow custody, not of the ref format; it does not bear
   on Q54. blahaj itself was not mutated (it is read only); a mutable copy
   would measure it.
2. **Non-HEAD symrefs.** Their targets are not carried (2 in blahaj). If
   the operator wants them restored as symrefs, that is a new metadata
   ref, and a ruling.
3. **No workspace restore.** Restoring the working tree (about 6.0 GB)
   was not run, for scratch-space reasons. The ref set, HEAD and custody
   comparison above does not depend on it.
4. **One sample, under load.** CPU figures are not load invariant (see the
   q42 probes' setup notes).
5. **Retained old-format blahaj captures, before L5 (from the code; not
   measured).** This run started from an empty corpus. Under the v1-header
   plan's compatibility rule, a capture written before #182 keeps its key,
   record and sidecars, so it stays a reuse hit. A pre-#182 blahaj capture
   most likely recorded `captured` with a manifest of about 24 MB (see
   Verdict). The hit path never reads the manifest, so an unchanged blahaj
   under the new binary would return `capture-reused-after-census` on that
   capture, and restore would then refuse it `GIT_INVENTORY_OVER_CAP`
   (`custody_manifest` maps the 16 MiB `BUDGET_EXCEEDED` to it). Before
   L5, find any retained old-format blahaj capture in any corpus.
   Alternatively, observe the old outcome with a one-off pre-#182 capture
   into scratch.
6. **Pass 2's zero read is not measured.** To measure it, record an
   independent read measure for the agent and its git children, such as
   `/proc/<pid>/io` `rchar`/`read_bytes`, or `ru_inblock` from a cold
   cache. The receipt's `source_bytes_read` and the process counter
   `read_source_file_bytes` cannot show it (Capture).
