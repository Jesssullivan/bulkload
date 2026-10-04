# S3 on the estate corpus, sting, 2026-10-04 (informational, ungated)

Rulings: OI-1003-Q35 (the admissible S3 evidence is `source_bytes_read`,
content bytes, `census_walks`, pack bytes and the rusage CPU ratio; wall time
is informational; estate verbs on this synthetic corpus under scratch are a
test, not an R-N56 estate operation), OI-1003-Q18 (the S3 delta
inequalities), OI-1003-Q36 (count SQLite `-shm` creation), OI-1003-Q15 (the
git engine is chosen on these numbers; see the
[decision packet](../plans/2026-10-04-git-engine-decision.md)), OI-1003-Q38
(cited by the Sprint 2 dispatch; its text is not in the repo), R-N13.

**Label: informational, ungated.** No power or load gate was applied
(load1 16 to 47 during the passes; other lanes were busy). One run per
scale. Nothing here is an S1 sample.

**Measured on main `4a10bb8`, before #151.** Every number below comes from
a release build of main `4a10bb8`. That build has neither #150 nor #151
(WP3 typed refusals): both merged after the 08:49Z build. The branch that
carries this doc was later merged with main `dfb9604` (#159, #160, #150,
#151). The numbers were **not re-measured** on `dfb9604`.

- **What may differ on `dfb9604`.** #151 removed the blanket I/O refusal
  conversions. It also routes v1 git children through the estimate
  classifier (`GIT_CHILD_FAILED`), so refusal text may read differently
  there, such as finding 11's `IO (errno 2)`. Its commit message says
  errno values and transfer refusal codes are unchanged.
- **What was checked.** The `ExportOptions::chain` doc comment that
  finding 3 cites ("Ignored when `prerequisite` is set") is the same at
  `dfb9604`.

**Re-evaluated after review (same day).** The verdicts below come from
`s3_estate.py evaluate`, which re-runs the evaluation of the recorded JSON
(the same two files, sha256 below) with the revised harness. No pass was
re-run, and no counter changed. What changed:

- Each changed seat counts in one half. File inequality 1 no longer counts
  SQLite seats, so it fails by the sniff bytes at every delta pass.
- File inequality 2 is `n/a: blocked by WP0(d)` when a changed seat was
  refused and not carried.
- Git inequality 1 covers worktree seats only. Object-store reads are
  reported beside it.
- Git inequality 2 counts each repository's new objects once and is
  reported at chunk and object granularity.
- The unchanged clause is judged on the CPU ratio. It fails at small
  scale.
- Findings 3 and 4 are re-attributed to the group-base exclusion, using
  the code at `4a10bb8` and scratch probes of the measured binary (see
  "Probes after the run").

## Setup

- **Build:** release `bulkload-agent` of main
  `4a10bb8278b189064ce0b32ae0a43ed6127cf218` (it carries #144 counters, #145
  WP1 and #146 auto-prerequisite chains; PRs #150 and #151 were still open,
  so neither is in). Exported with `git archive` into scratch and built with
  `cargo build --release --locked -p bulkload-agent` in that tree's
  `nix develop`, at nice 19, `CARGO_BUILD_JOBS=4`, under the lanes' shared
  flock, with its own `CARGO_TARGET_DIR`. Default features: carry_v2 is
  compiled in, as at main, and no verb reaches it; `m1-spike` is off. Binary
  sha256 `2da7ec114bb1cdd193e1a1db1949a8f67860bbc590b0de58eb91cad6a58a98ee`.
  Queued 08:49Z, compiled in 4 min 44 s, done 09:05:50Z.
- **Harness:** `crates/bulkload-bench/scripts/s3_estate.py` (sha256
  `76239763ae7e`…, the committed file), run as
  `nice -n 10 nix develop .#default --command python3 s3_estate.py run
  --agent BIN --work WORK --scale small|estate`. Its tests are
  `test_s3_estate.py` (12 tests then, optional tier). `just bench-s3-estate`
  wraps it. The verdicts were then re-evaluated with the revised harness of
  commit `9581384` (`s3_estate.py evaluate RUN.json --out NEW.json`, 15
  tests).
- **Host:** sting, x86_64 Linux, 32 CPUs, 54.7 GiB RAM. MemAvailable was
  15.3 GiB before the estate run (the 6 GiB precondition held) and 13.3 to
  25.2 GiB at the starts of its passes.
- **Corpus:** generated in place under a private scratch WORK, never copied
  (it is not relocatable). Both generations printed `recorded=True`:
  - small `c767aa685c670abc2d406de3c300b4e1118db8581a941de42ceee7808f98aa69`
    (6.5 s);
  - estate `586de100483a317afda3b6013ddf0a35c3e3424f57de871a328535bd3d507a93`
    (789 s).

  After both mutation rounds, `estate_corpus.py verify` was ok with
  `shm_ignored=1` (small `c7eac73b…`, estate `64125df6…`).
- **Raw output:** `WORK/s3-estate.json` and `WORK/logs/`, in scratch:
  `run-small-2` (sha256 `ce1601940a1b`…) and `run-estate-1` (sha256
  `1b0ed6f10e25`…). They are not durable; the numbers below are the record.
  `run-small-1` was an earlier small run of the same build, with the harness
  before per-item bundle accounting was added. Its counters agree with
  `run-small-2`, with two exceptions. `read_source_pack_readback_bytes`
  differs (see the caveats). Pack bytes differ by a few bytes per bundle,
  through the capture commits' timestamps.

## What a run does

Six passes: `first`, then `rerun-1` to `rerun-3` with nothing changed, then
`mutate-1` and `mutate-10`. Each mutation pass runs
`estate_corpus.py mutate WORK/estate N` first and then waits until the
sidecar's `mutated_at_ns + settle_ns` plus 1 s, so no seat is racy. Racy
seats were checked at every pass start and there were none. Every pass
drives three halves, one child at a time:

- **File half.** `bulkload-agent copy` once per top-level directory other
  than `git/`. That is 14 areas, each with its own destination and state
  kept across passes. copy has no exclude, and the walk has no `.git`
  partition, so the git half cannot be cut out of a single copy of the root.
  The 7 top-level dotfiles (`.bashrc` and so on) lie outside every area and
  are not carried. No mutation touched them.
- **SQLite half.** `bulkload-agent snapshot` of each SQLite database outside
  `git/` (4 at small, 11 at estate) into a new private directory per pass.
  copy refuses those seats and their `-wal`/`-shm` with `SQLITE_STATE_CHANGED`.
  snapshot has no ledger, so it reads the whole database every pass. Its
  bytes are derived (database plus `-wal` size); no counter covers them.
- **Git half.** `estate-capture PLAN STATE CORPUS 2` over two plans, so each
  plan's counters can be read alone. Plan `rest` holds every item but
  `git/history-heavy`: main checkouts, linked worktrees, the nest and the
  bare mirrors (9 items at small, 57 at estate). Plan `history` holds
  `git/history-heavy`.
- **v2 projection, after the v1 pass.** `git-carry-estimate` of every
  repository against a model of what a v2 destination would hold:
  - for `first`, an empty bare repository;
  - for the later passes, a `clone --mirror --no-local` of the source taken
    after the pass before, plus every stash entry, fetched by object id.

  carry_v2 was neither enabled nor run.

Around the v1 pass and again around the v2 projection, an S2 stat snapshot
of the whole corpus (`estate_corpus.s2_rows`) is diffed. Per child, the run
records exit status, the parsed `key=value` lines, refusals, wall time and
rusage user plus system CPU (`RUSAGE_CHILDREN` deltas).

**Subsets.** Each pass is evaluated four ways: all; without `data/` (the
`large-data` class, dropping the `data` copy); without `git/history-heavy`
(the `git-history` class, dropping plan `history` and its estimate); and
without both. Each copy and each plan is its own process, so a subset is an
exact sum of process-scope counters.

**Bounds (OI-1003-Q18), from each round's sidecar.**

Each changed seat counts in one half only. SQLite databases and their
`-wal`, `-shm` and `-journal` companions belong to the SQLite half: copy
refuses them (`SQLITE_STATE_CHANGED`), so they are in no file-half bound.

- **Inequality 1, file half.** Source bytes read ≤ the non-SQLite `walk`
  seats of the copied areas in `reads_allowed`, plus racy seats.
- **Inequality 1, git half.** Receipt `source_bytes_read` ≤ the round's
  `worktree` seats, plus racy seats. At `4a10bb8` that counter covers
  worktree streaming only (`raw_tree.rs`: "Metadata censuses, symlink reads
  and Git repacking are separate"), so both sides count worktree seats.
  The git children's reads of the source object store are outside it, though
  `counters.rs` says every object in a capture pack "was read from an object
  store to be written". They are reported beside the inequality:
  - `write_source_pack_bytes`, the logical measure (it includes objects the
    capture wrote into its private repository);
  - `read_source_pack_readback_bytes`, a lower bound;
  - the round's changed `git-objects` seats.

  Whether object-store reads count as "source content bytes" under
  OI-1003-Q18 needs a ruling.
- **Inequality 2, file half.** `bytes_received` ≤ the absent chunks of the
  seats copy carried. Per changed file, each changed range is widened by one
  maximum CDC chunk (256 KiB) before it and two after it, then capped at the
  file. A new file counts whole, and a deleted one counts 0. This widening
  is the harness's resynchronisation allowance, not a ruling. A changed seat
  refused `GIT_DESTINATION_OCCUPIED` was not carried, so the delta did not
  converge. The verdict is then `n/a: blocked by WP0(d)`, and the bound with
  the blocked seats is shown beside it.
- **Inequality 2, git half.** `write_source_pack_bytes` ≤ the sum over
  changed repositories of the round's new objects (uncompressed, counted
  once per repository however many of its items changed) plus its changed
  worktree seats. The seats are counted at two granularities:
  - **chunk:** the file half's absent-chunk bound on each changed seat;
  - **object:** each changed seat whole, as a new blob. Git packs whole
    blobs, so this is the finest bound a Git pack can meet.

  Which granularity OI-1003-Q18 means for the git half needs a ruling. The
  bound has no allowance for capture metadata, so any excess is reported per
  repository.
- **The unchanged clause.** 0 content bytes read and received, 0 pack
  bytes, one census walk per censused item, and the rusage CPU ratio ≤ 10 %
  of the first pass (OI-1003-Q35). The wall ratio is reported beside it and
  is informational.

## Results, scale estate (142,321 entries, 4.19 GB)

Subset **all**. "Pack" is `write_source_pack_bytes`. "Readback" is
`read_source_pack_readback_bytes`, a lower bound, since page-cache hits are
invisible. "SQLite read" is the derived whole-database bytes. "v2 thin" is
the summed `missing_thin_pack_bytes`. CPU is v1's three halves, user plus
system. The ratios are against `first`. Every source-side child ran
`priority=background priority_from=default`.

| Pass | File read | File received | Git read | Pack | Readback | Census (expected) | SQLite read | v2 thin | v1 CPU s | CPU util | v1 wall s | Wall ratio | CPU ratio | load1 before / after |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|
| first | 2,860,494,411 | 2,860,494,235 | 711,653,958 | 932,119,799 | 506,966,016 | 216 (216) | 180,581,128 | 350,103,767 | 178.96 | 0.088 | 2,029.7 | 1 | 1 | 18.33 / 20.5 |
| rerun-1 | 176 | 0 | 0 | 0 | 0 | 54 (54) | 180,581,128 | 0 | 13.16 | 0.497 | 26.5 | 0.013 | 0.074 | 22.79 / 23.14 |
| rerun-2 | 176 | 0 | 0 | 0 | 0 | 54 (54) | 180,581,128 | 0 | 12.33 | 1.078 | 11.4 | 0.006 | 0.069 | 23.14 / 29.29 |
| rerun-3 | 176 | 0 | 0 | 0 | 0 | 54 (54) | 180,581,128 | 0 | 14.07 | 0.399 | 35.2 | 0.017 | 0.079 | 29.29 / 46.73 |
| mutate-1 | 176 | 0 | 3,209 | 39,352 | 32,768 | 57 (57) | 180,581,128 | 499 | 26.57 | 1.311 | 20.3 | 0.010 | 0.148 | 31.22 / 25.88 |
| mutate-10 | 674,467 | 1,217 | 67,115,992 | 70,619,800 | 16,859,136 | 66 (66) | 180,589,368 | 1,065 | 21.07 | 1.112 | 19.0 | 0.009 | 0.118 | 16.58 / 17.86 |

The other subsets differ from **all** only in the counters below. The
unchanged reruns and `mutate-1` without history-heavy are the same as
**all**, less history-heavy's 1 census walk and its `mutate-1` capture.

| Pass | Subset | File read | Git read | Pack | Census (expected) | v2 thin | v1 CPU s | CPU ratio |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| first | without `data/` | 746,561,014 | 711,653,958 | 932,119,799 | 216 (216) | 350,103,767 | 170.87 | 1 |
| first | without history-heavy | 2,860,494,411 | 698,747,686 | 752,710,458 | 212 (212) | 170,742,243 | 169.50 | 1 |
| first | without both | 746,561,014 | 698,747,686 | 752,710,458 | 212 (212) | 170,742,243 | 161.41 | 1 |
| mutate-1 | without both | 176 | 0 | 0 | 53 (53) | 0 | 12.75 | 0.079 |
| mutate-10 | without both | 674,467 | 67,112,409 | 70,580,320 | 62 (62) | 503 | 20.62 | 0.128 |

CPU per half, in seconds:

| Pass | File (copy) | SQLite (snapshot) | Git (`estate-capture`) | v2 projection (estimate) |
|---|---:|---:|---:|---:|
| first | 59.39 | 0.61 | 118.96 | 20.65 |
| rerun-1 | 7.50 | 0.59 | 5.08 | 5.15 |
| rerun-2 | 6.87 | 0.80 | 4.65 | 6.07 |
| rerun-3 | 8.20 | 0.68 | 5.19 | 7.18 |
| mutate-1 | 7.12 | 0.76 | 18.70 | 6.65 |
| mutate-10 | 5.51 | 0.55 | 15.02 | 5.19 |

## Results, scale small (1,707 entries, 18.7 MB)

Subset **all** (`run-small-2`):

| Pass | File read | File received | Git read | Pack | Census (expected) | SQLite read | v2 thin | v1 CPU s | Wall ratio | CPU ratio | load1 before / after |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|
| first | 7,009,439 | 7,009,375 | 5,725,568 | 6,977,621 | 36 (36) | 1,667,776 | 2,448,913 | 6.75 | 1 | 1 | 41.37 / 41.79 |
| rerun-1 | 64 | 0 | 0 | 0 | 9 (9) | 1,667,776 | 0 | 1.06 | 0.065 | 0.157 | 41.79 / 42.77 |
| rerun-2 | 64 | 0 | 0 | 0 | 9 (9) | 1,667,776 | 0 | 1.03 | 0.037 | 0.152 | 42.77 / 41.75 |
| rerun-3 | 64 | 0 | 0 | 0 | 9 (9) | 1,667,776 | 0 | 1.07 | 0.040 | 0.159 | 41.75 / 41.75 |
| mutate-1 | 64 | 0 | 865 | 5,753 | 12 (12) | 1,667,776 | 422 | 1.50 | 0.050 | 0.223 | 41.31 / 41.31 |
| mutate-10 | 51,122 | 1,879 | 1,059,187 | 2,239,880 | 24 (24) | 1,680,112 | 947 | 3.54 | 0.095 | 0.525 | 40.25 / 40.63 |

Without both `data/` and history-heavy, the small first pass reads 717,963
file bytes and writes a 5,778,585-byte pack (32 census walks). `mutate-1`
then writes no pack, and `mutate-10` writes a 2,232,456-byte pack.

## Verdicts

### Unchanged reruns (all three, both scales, every subset)

| Clause | Verdict | Measured |
|---|---|---|
| Git: 0 source bytes, 0 pack bytes, one census walk per item | **pass** | 0, 0, and 54 of 54 (estate) or 9 of 9 (small) |
| File: 0 content bytes received | **pass** | 0 |
| File: 0 content bytes read | **fail** | 176 B (estate) or 64 B (small): 16 B per refused SQLite database, every pass |
| SQLite: 0 content bytes read | **fail** | snapshot re-reads every database: 180,581,128 B (estate), 1,667,776 B (small) |
| CPU ratio ≤ 10 % of `first` (OI-1003-Q35) | estate **pass**, small **fail** | see below |

The CPU ratio, rusage user plus system of v1's three halves, per scale and
subset (range over `rerun-1` to `rerun-3`), with the wall ratio beside it
(informational):

| Scale | Subset | CPU ratio | Verdict | Wall ratio (informational) |
|---|---|---:|---|---:|
| estate | all | 6.9 % to 7.9 % | pass | 0.6 % to 1.7 % |
| estate | without `data/` | 7.2 % to 8.2 % | pass | 0.6 % to 1.7 % |
| estate | without history-heavy | 7.2 % to 8.2 % | pass | 0.6 % to 1.7 % |
| estate | without both | 7.6 % to 8.7 % | pass | 0.6 % to 1.7 % |
| small | all | 15.2 % to 15.9 % | fail | 3.7 % to 6.5 % |
| small | without `data/` | 15.1 % to 15.8 % | fail | 3.8 % to 6.5 % |
| small | without history-heavy | 15.5 % to 16.2 % | fail | 3.5 % to 5.4 % |
| small | without both | 15.4 % to 16.1 % | fail | 3.5 % to 5.4 % |

The small-scale failure comes from a small denominator. The first pass
there is 6.75 s of CPU, 5.88 s of it the git capture of 9 censused items.
A rerun's
census costs about 0.09 s per item at both scales: 0.78 s to 0.81 s for 9
items at small, and 4.65 s to 5.19 s for 54 items at estate. Against a
first-pass capture of 118.96 s at estate, that is under 10 %. Against
5.88 s at small, it is not. The file half adds 0.22 s to 0.23 s per small
rerun over 14 copy processes.

### Delta reruns (OI-1003-Q18)

"Sniff only" means the excess equals the 16-byte magic copy reads from each
refused SQLite database (finding 6). Git inequality 2 gives the chunk bound
first, then the object bound.

| Pass | Half | Inequality | Measured | Bound | Verdict |
|---|---|---|---:|---:|---|
| estate mutate-1 | file | 1 | 176 | 0 | fail (sniff only, +176) |
| estate mutate-1 | file | 2 | 0 | 0 | pass |
| estate mutate-1 | git | 1, worktree seats | 3,209 | 3,209 | pass |
| estate mutate-1 | git | 2, chunk / object | 39,352 | 9,782 / 9,782 | fail (+29,570 / +29,570) |
| estate mutate-10 | file | 1 | 674,467 | 674,291 | fail (sniff only, +176) |
| estate mutate-10 | file | 2 | 1,217 | 1,217 carried (263,623 with the 2 blocked seats) | n/a: blocked by WP0(d) |
| estate mutate-10 | git | 1, worktree seats | 67,115,992 | 67,115,992 | pass |
| estate mutate-10 | git | 2, chunk / object | 70,619,800 | 806,672 / 67,129,040 | fail (+69,813,128 / +3,490,760) |
| small mutate-1 | file | 1 / 2 | 64 / 0 | 0 / 0 | fail (sniff only, +64) / pass |
| small mutate-1 | git | 1 / 2 (chunk = object) | 865 / 5,753 | 865 / 4,666 | pass / fail (+1,087) |
| small mutate-10 | file | 1 | 51,122 | 51,058 | fail (sniff only, +64) |
| small mutate-10 | file | 2 | 1,879 | 1,879 carried (51,058 with the blocked seat) | n/a: blocked by WP0(d) |
| small mutate-10 | git | 1, worktree seats | 1,059,187 | 1,059,187 | pass |
| small mutate-10 | git | 2, chunk / object | 2,239,880 | 291,969 / 1,066,625 | fail (+1,947,911 / +1,173,255) |

Object-store reads beside git inequality 1 are not judged; their status
needs a ruling (see Bounds):

| Pass | Readback (lower bound) | Pack bytes (logical) | Changed object-store seats | If they counted |
|---|---:|---:|---:|---|
| estate mutate-1 | 32,768 | 39,352 | 4,386 | fail |
| estate mutate-10 | 16,859,136 | 70,619,800 | 447,490 | fail |
| small mutate-1 | 0 | 5,753 | 2,753 | unknown (page cache) |
| small mutate-10 | 0 | 2,239,880 | 1,118,125 | unknown (page cache) |

The SQLite half fails both inequalities at every delta pass, because snapshot
reads and writes whole databases. At estate `mutate-10` it read 180,589,368 B
against 3,420,984 B of changed SQLite seats, and wrote 180,518,912 B against
32,816 B of changed pages. Those 3,420,984 B (`kv.sqlite` 3,284,992 B and
`storage.db-wal` 135,992 B) are in the SQLite half's bound only. Without
`data/`, the verdicts are the same. Without history-heavy, estate `mutate-1`
passes both git inequalities trivially (0, 0). Without history-heavy, estate
`mutate-10` git inequality 2 is 70,580,320 against 796,112 (chunk) or
67,118,480 (object), and its readback is 16,384 B.

Git inequality 2, per repository, from the bundles its items' captures
added. New objects count once per repository:

| Pass | Repository (items recaptured) | Bundle bytes | New objects | Bound chunk / object | Why the bound is what it is |
|---|---|---:|---:|---:|---|
| estate mutate-1 | `git/history-heavy` | 39,352 | 6,573 | 9,782 / 9,782 | a 5-object commit and a 3,209 B file |
| estate mutate-10 | `git/history-heavy` | 39,480 | 6,977 | 10,560 / 10,560 | a 5-object commit and an edited file |
| estate mutate-10 | `git/r00-delta` (main) | 68,702,196 | 0 | 786,496 / 67,108,864 | `large-edit` of 64 B in the 64 MiB tracked `assets/model.bin`, uncommitted |
| estate mutate-10 | `git/r20-resume` (`worktrees/detached`) | 1,834,076 | 0 | 3,545 / 3,545 | `head-move`: `checkout --detach` rewrote a few files |
| estate mutate-10 | `git/r05-bytes` (main; a group of one, chained) | 46,395 | 6,071 | 6,071 / 6,071 | a 5-object commit on the unmerged `side` |
| small mutate-10 | `git/r00-delta` (main, `worktrees/detached`) | 1,113,336 + 1,105,407 | 0 | 279,257 / 1,053,913 | `large-edit` of the 1 MiB `model.bin`, and a `head-move` |
| small mutate-10 | `git/r01-row` (main, `worktrees/tree`) | 10,563 + 6,674 | 3,307 | 3,307 / 3,307 | a commit on `side` |
| small mutate-10 | `git/history-heavy` | 7,424 | 4,131 | 9,405 / 9,405 | **within the bound** |

## Findings

1. **The git half meets the unchanged clause exactly.** Every unchanged
   rerun reused every censused item (`capture-reused-after-census`): 0 source
   bytes, 0 pack bytes, and one census walk per item. `census_walks` matched
   the sidecar's `census_walks_expected` at every pass: 216, 54, 57 and 66
   at estate. The census model in the corpus doc holds at estate scale.
2. **Git inequality 1, worktree seats: no unchanged worktree seat was
   re-read.** The receipts' `source_bytes_read` equals the changed worktree
   seats to the byte (3,209 B; 67,115,992 B). That equality holds by
   construction, since both sides count worktree seats only (see Bounds).
   Unchanged objects *were* re-read from object stores, outside the
   counter:
   - At estate `mutate-10`, readback was 16,859,136 B, a lower bound. Of
     that, 16,842,752 B was history-heavy's packing child, for a 39,480 B
     bundle with a 5-object change. At `mutate-1` it was 32,768 B.
   - At small `mutate-10`, the detached worktree's bundle re-packed the
     unchanged committed 1 MiB `model.bin` (finding 3). Packing it means
     reading it from an object store, yet readback shows 0 there because of
     page-cache hits.

   If object-store reads count as source content bytes, git inequality 1
   fails at both estate delta passes. That needs a ruling.
3. **Git inequality 2 fails on v1 mechanisms, not on the round's
   history.** Three shapes:
   - **A grouped item re-packs the blobs its group base holds.** The
     examples are a linked worktree after a `head-move` (estate 1.83 MB
     against 3.5 KB; small 1.1 MB against 5.3 KB) and a main checkout after
     an unrelated `side` commit (small `r01-row`, 17,237 B against 3,307 B).
     At small scale, the detached bundle held 70 objects (50 blobs,
     1,100,983 B). They include the unchanged committed 1 MiB `model.bin`
     (`c2574974`) and a tag. The worktree and staged snapshot commits in it
     are parentless.
   - **The cause is the group-base exclusion.** It was root-caused from
     the code at `4a10bb8` and a scratch probe, and not re-measured on the
     corpus. An item in a multi-item group takes the plan's shared group
     base as its prerequisite. `write_with_prerequisites` (`shared.rs`)
     writes the bundle with plain `git bundle create --all --stdin` and
     `^tip` lines. That marks only the trees of edge commits uninteresting,
     so the parentless snapshot commits' trees re-pack every tracked blob
     they share with HEAD, including blobs the base already holds. The
     chained path, `write_excluding_tip_trees`, already avoids this with
     `rev-list --objects-edge-aggressive`, and its own comment describes
     the effect. In the probe, a parentless snapshot over a 4 MiB tracked
     blob with a one-line change gave a 4,300,866 B bundle through
     `bundle create ^base --all`, and 3,507 B through the tip-tree
     exclusion. history-heavy, a group of one, takes the chained path and
     packed 19 objects.
   - **Capture metadata, which grows with refs and trees.** A changed
     capture adds its own snapshot commits and trees, carry refs and a
     bundle header. The header lists the whole private ref inventory
     (`refs(private)` in `write_excluding_tip_trees_pending`). The residuals
     on chained items are:
     - history-heavy: 29,570 B (estate `mutate-1`), 28,920 B (estate
       `mutate-10`) and 1,087 B (small `mutate-1`);
     - `r05-bytes`: 40,324 B (estate `mutate-10`).

     The cost is per ref and per changed tree, not constant.
4. **First-pass duplication across linked worktrees, from the same
   exclusion.** Every item bundle of `r00` and `r12` (4 items each) carries
   the committed 64 MiB blob, at 67.99 to 68.70 MB per bundle. That is
   546.7 MB of the first pass's 932 MB of v1 bundles.
   - The two 68 MB shared group bases already hold those blobs:
     `f34c244b…` in one and `c8fbd764…` in the other, checked by scanning
     the packs. Each of the eight item bundles carries one of them again,
     with 876 to 1,530 other blobs.
   - These are history bytes that the missing tip-tree exclusion leaves in.
     They are not snapshot layer.
   - With the exclusion, the eight copies, about 537 MB, would go (derived,
     not measured). That would leave v1's first pass near 395 MB.
   - The shared group bases total 155.9 MB, and history-heavy's bundle is
     179.4 MB.
5. **A changed capture fetches its whole retained bundle for reuse.** At
   estate `mutate-1`, history-heavy's capture read 179,409,341 B of its own
   retained bundle (`read_source_capture_reuse_bytes`). That is private
   state, not the source. It spent 13.81 s of CPU on a 5-object change; the
   v2 estimate of the same change took 0.29 s. At `mutate-10`, the retained
   bundle was `mutate-1`'s 39,352 B one, and the capture took 0.45 s.
6. **File half: a 16-byte sniff per refused SQLite seat, every pass.** copy
   reads the magic of each database it then refuses (`SQLITE_STATE_CHANGED`):
   4 × 16 B at small and 11 × 16 B at estate. The `-wal` and `-shm` are
   refused by name, unread. This sniff alone fails the file half's
   "0 content bytes" clause. It also fails file inequality 1 at every delta
   pass at both scales, by exactly 176 B or 64 B. It is the review's WP6
   PR 2 item: memoise refused seats by stat identity, and count sniff bytes
   separately.
7. **File half deltas: inequality 1 fails by the sniff bytes only, and
   inequality 2 is n/a at `mutate-10`.**
   - **Inequality 1, estate `mutate-10`.** copy read 674,467 B against
     674,291 B of changed non-SQLite seats: 673,054 B in `.codex` (the
     rollout, read whole) and 1,237 B in `projects`, plus the 176 B sniff.
     The first evaluation's bound, 4,095,275 B, also counted `kv.sqlite`
     (3,284,992 B) and `storage.db-wal` (135,992 B). copy refuses those
     seats and only sniffs them (`.local` read 144 B), and the SQLite half
     owns them. So that "pass" was an artifact of the bound.
   - **Inequality 1, small `mutate-10`.** 51,122 B against 51,058 B, over
     by 64 B.
   - **Inequality 2.** At estate `mutate-10`, copy received 1,217 B, all of
     it the new file. The two other changed non-SQLite files were refused
     `GIT_DESTINATION_OCCUPIED` and never received (finding 8): the 673,054 B
     rollout append and the edited `cli.js`. At small `mutate-10`, the
     modified rollout was refused and only the new file was carried. Over
     the carried seats the inequality holds (1,217 ≤ 1,217; 1,879 ≤ 1,879).
     The delta did not converge, though, so that is not S3 evidence.
8. **No-clobber: `blocked by WP0(d)`.** A changed seat whose destination
   already holds an older output is read and then refused
   `GIT_DESTINATION_OCCUPIED`, and the destination keeps the stale bytes:
   - estate `mutate-10`: the appended 673,054 B rollout JSONL and the edited
     `cli.js`;
   - small `mutate-10`: the appended 49,179 B rollout.

   This is why file inequality 2 is `n/a: blocked by WP0(d)` at both
   `mutate-10` passes.

   Until OI-1003-Q18 (d) superseding publish lands, a live estate's edited
   files never converge. Each such seat is probably re-read on every rerun,
   since a refused seat records no capture; that was not measured here.
9. **The SQLite half has no S3 path.** snapshot re-reads every database in
   full on every pass: 180.6 MB at estate. It fails the unchanged clause and
   both delta inequalities. **OI-1003-Q36:** the first pass created one
   `-shm` (`.local/share/opencode/storage.db-shm`, beside the WAL image),
   which also moved its directory's size and mtime. Every later pass touched
   it. The main database and its `-wal` were byte-identical (BLAKE2b) across
   every pass.
10. **S2: v1 capture freshens source objects; the estimate does not.**
    `estate-capture` changed the mtime and ctime of existing entries in the
    source object stores (loose objects and packs):
    - estate: 31 on the first pass (a pack among them), 4 at `mutate-1`,
      and 38 at `mutate-10`;
    - small: 89, 4 and 2 (a pack among the last 2).

    Inode, size and bytes did not change. `git-carry-estimate` moved nothing
    on any pass. This confirms the WP0(e) smoke's finding for WP1.
11. **Bare mirrors are refused untyped.** Each bare mirror is refused with
    `IO (errno 2)` on every pass (4 at estate, 1 at small). `estate-capture`
    of plan `rest` then exits `CONTRACT_SELF_INCONSISTENT`.
12. **Per-entry cost dominates the first pass (S1, informational).** At
    estate, copy spent:
    - 664 s on `.cache` (15,000 files) and 1,020 s on `projects` (26,478
      files), against 9.6 s on `data/` (5 files, 2.1 GB);
    - for `.cache` alone, 293 s in 15,000 file barriers (about 19.5 ms each),
      195 s in 16,606 directory barriers, and 231 s in 9,414 SQLite commits.

    That is WP9's serial per-entry chain on a loaded volume at idle IO
    priority. It is why the unchanged reruns' wall time (informational) sits
    at 0.6 % to 1.7 % of the first pass, while their CPU sits at 6.9 % to
    8.7 %.

## v2 projection (for the decision packet)

`missing_thin_pack_bytes` summed over every repository:

| Pass | v2 thin pack (estate) | v1 pack (estate) | v2 thin pack (small) | v1 pack (small) |
|---|---:|---:|---:|---:|
| first | 350,103,767 | 932,119,799 | 2,448,913 | 6,977,621 |
| rerun-1 to rerun-3 | 0 | 0 | 0 | 0 |
| mutate-1 | 499 | 39,352 | 422 | 5,753 |
| mutate-10 | 1,065 | 70,619,800 | 947 | 2,239,880 |

At estate `first`, the v2 thin pack splits into 170,742,243 B for the
repositories without history-heavy and 179,361,524 B for history-heavy.
v1 and v2 do not cover the same repositories. v1 refused all 4 bare mirrors
on every pass (finding 11), and the projection includes them:

| Pass | v2 thin pack, all (estate) | of which bare mirrors | v2 without mirrors (estate) | v2 without mirrors (small) |
|---|---:|---:|---:|---:|
| first | 350,103,767 | 6,427,129 | 343,676,638 | 2,407,663 |
| rerun-1 to rerun-3 | 0 | 0 | 0 | 0 |
| mutate-1 | 499 | 0 | 499 | 422 |
| mutate-10 | 1,065 | 0 | 1,065 | 947 |

The mirrors at estate `first` are `r02` 1,558,041 B, `r08` 565,541 B, `r14`
2,420,149 B and `r20` 1,883,398 B. At small, the one mirror is 41,250 B.

v1's first-pass bundle files (932,163,797 B; the counter says 932,119,799 B)
split into:
- the shared group bases, 155,889,062 B;
- history-heavy's bundle, 179,409,341 B;
- the eight `r00` and `r12` item bundles, 546,715,099 B. About 537 MB of
  that is the 64 MiB base blob, re-packed eight times (finding 4);
- the other 45 item bundles, 50,150,295 B. A repository without linked
  worktrees has no group base, so its history rides in these.

So v1's first pass holds 335 MB of bases and history-heavy, and at most
50 MB more history inside the other item bundles. On top of that come
about 537 MB of base blobs re-packed by the group-base exclusion, which are
history bytes, not snapshot layer. Over the same repositories, v2's thin
packs come to 343.7 MB. v2 carries no worktree, index, untracked or ignored
state, so the snapshot share of v1's item bundles has no v2 counterpart. In
unfixed v1 that share cannot be separated from the re-packed base blobs.
The uncommitted 64 MiB edit at `mutate-10` alone is 67 MB that v2 cannot
carry.

Building the destination models took 26.8 s (m0) and 64.8 s (m1) at estate,
with 24 stash entries fetched each time.

## Probes after the run (review round)

The review ran three scratch probes with the measured binary (sha256
`2da7ec11…`) under an empty git config. Their outputs were re-read for this
section; the scratch is not durable. Each probe is small, and none is an
estate-corpus measurement. They show two v1 behaviours that the six passes
could not reach, because the runs made at most 2 changed captures per item:

- **Exclusion.** A parentless snapshot commit over a 4 MiB tracked blob
  with a one-line change, excluding a base that holds the blob:
  - `git bundle create ^base --all`, the group-base path: 4,300,866 B;
  - `rev-list --objects-edge-aggressive | pack-objects`, the chained path:
    3,507 B.
- **Chained item re-base** (`CHAIN_DEPTH_LIMIT` = 8, `chain.rs`). One
  ungrouped repository with a 4 MiB tracked blob takes ten 512 KiB commits,
  with one `estate-capture` after each:

  | Capture | 0 | 1 to 8 | 9 | 10 |
  |---|---:|---:|---:|---:|
  | Bundle bytes | 4,197,421 | 526,413 to 526,727 | 8,919,284 | 526,818 |
  | Depth | self-contained | chained | self-contained (re-base) | chained |

  Every ninth changed capture re-packs the item's whole history, by design.
- **Grouped items never chain.** `chain_offer` drops the link whenever a
  plan base exists (`link.filter(|_| unbased)`), and `prepare_base` reuses
  `shared-{group}.base` for good. The probe used one repository plus one
  linked worktree (a 2-item group, so a shared base), with the same ten
  commits. Each commit changes the ref inventory in both items' keys, so
  both recapture every pass. Each pass's bundle grows by about one
  commit:

  | Pass | 0 | 1 | 2 | 3 | … | 10 |
  |---|---:|---:|---:|---:|---|---:|
  | Item bundle bytes (each of the 2) | 4.25 M | 526,696 to 526,926 | 1,051,382 to 1,051,626 | 1,576,092 to 1,576,350 | … | 5,249,800 to 5,250,161 |

  At pass 0, the two item bundles (4,245,323 B and 4,245,531 B) sit beside
  a 4,243,658 B shared base. The base's 4 MiB blob is re-packed into both,
  which is finding 4's shape.

  By pass 10, a 512 KiB commit costs about 10.5 MB across the two items. A
  grouped item re-packs all history added since the first pass on every
  changed capture, once per item in the group. In the corpus, `r00` and
  `r12` are 4-item groups. history-heavy, the only history-delta case the
  runs measured, is a group of one.

## Caveats

- **Ungated.** No power or load gate was applied, and load1 ran 16 to 47.
  Wall times and CPU utilisation are informational. The rusage CPU is less
  load-sensitive, but it still includes contention effects. Each scale ran
  once.
- **Pack readback is a lower bound.** Page-cache hits are invisible to
  `read_source_pack_readback_bytes`: two small first passes gave 2,347,008 B
  and 61,440 B.
- **The SQLite bytes are derived.** They are the database plus `-wal` sizes
  the backup API reads, not a counter.
- **The bounds are the harness's.**
  - The file half's absent-chunk widening is an allowance, not a ruling.
  - The git bound includes no capture metadata. It is reported at chunk
    and object granularity, pending a ruling on which applies.
  - Git inequality 1 covers worktree seats only, pending a ruling on
    object-store reads.
- **The v2 column is a projection.** It is history and refs only, against a
  mirror model of the destination. A real v2 sender also needs the snapshot
  layer.
- **Coverage gaps.** The top-level dotfiles are not carried, because copy
  runs per area. The bare mirrors are refused.
- **The corpus models a well-behaved estate.** See
  [estate-corpus-v1-2026-10-03.md](estate-corpus-v1-2026-10-03.md), "Not
  modelled".
