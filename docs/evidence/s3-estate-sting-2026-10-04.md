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
  `test_s3_estate.py` (12 tests, optional tier). `just bench-s3-estate`
  wraps it.
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

- **Inequality 1, file half.** Source bytes read ≤ the `walk` seats of the
  copied areas in `reads_allowed`, plus racy seats.
- **Inequality 1, git half.** Receipt `source_bytes_read` ≤ the round's
  `worktree` seats, plus racy seats.
- **Inequality 2, file half.** `bytes_received` ≤ the absent chunks. Per
  changed file, each changed range is widened by one maximum CDC chunk
  (256 KiB) before it and two after it, then capped at the file. A new file
  counts whole, and a deleted one counts 0. This widening is the harness's
  resynchronisation allowance, not a ruling.
- **Inequality 2, git half.** `write_source_pack_bytes` ≤ the sum over
  changed items of the round's new objects in the item's repository
  (uncompressed) plus the item's changed worktree bytes. The bound has no
  allowance for capture metadata, so any excess is reported per item.
- **The unchanged clause.** 0 content bytes read and received, 0 pack bytes,
  one census walk per censused item, and wall time ≤ 10 % of the first pass.
  The wall-time part is informational.

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
| Wall time ≤ 10 % of `first` (informational) | **pass** | 0.6 % to 1.7 % (estate), 3.7 % to 6.5 % (small) |

### Delta reruns (OI-1003-Q18)

| Pass | Half | Inequality | Measured | Bound | Verdict |
|---|---|---|---:|---:|---|
| estate mutate-1 | file | 1 | 176 | 0 | fail (sniff bytes only) |
| estate mutate-1 | file | 2 | 0 | 0 | pass |
| estate mutate-1 | git | 1 | 3,209 | 3,209 | pass |
| estate mutate-1 | git | 2 | 39,352 | 9,782 | fail (+29,570) |
| estate mutate-10 | file | 1 | 674,467 | 4,095,275 | pass |
| estate mutate-10 | file | 2 | 1,217 | 263,623 | pass |
| estate mutate-10 | git | 1 | 67,115,992 | 67,115,992 | pass |
| estate mutate-10 | git | 2 | 70,619,800 | 67,129,040 | fail (+3,490,760) |
| small mutate-1 | file | 1 / 2 | 64 / 0 | 0 / 0 | fail (sniff) / pass |
| small mutate-1 | git | 1 / 2 | 865 / 5,753 | 865 / 4,666 | pass / fail (+1,087) |
| small mutate-10 | file | 1 / 2 | 51,122 / 1,879 | 735,842 / 51,058 | pass / pass |
| small mutate-10 | git | 1 / 2 | 1,059,187 / 2,239,880 | 1,059,187 / 1,069,932 | pass / fail (+1,169,948) |

The SQLite half fails both inequalities at every delta pass, because snapshot
reads and writes whole databases. At estate `mutate-10` it read 180,589,368 B
against 3,420,984 B of changed SQLite seats, and wrote 180,518,912 B against
32,816 B of changed pages. Without `data/`, the verdicts are the same. Without
history-heavy, estate `mutate-1` passes both git inequalities trivially (0,
0), and `mutate-10` git inequality 2 is 70,580,320 against 67,118,480.

Git inequality 2, per item, from the bundles each capture added:

| Pass | Item | Bundle bytes | Bound | Why the bound is what it is |
|---|---|---:|---:|---|
| estate mutate-1 | `git/history-heavy` | 39,352 | 9,782 | a 5-object commit (6,573 B) and a 3,209 B file |
| estate mutate-10 | `git/history-heavy` | 39,480 | 10,560 | a 5-object commit and an edited file |
| estate mutate-10 | `git/r00-delta` | 68,702,196 | 67,108,864 | `large-edit` of the 64 MiB tracked `assets/model.bin`, uncommitted |
| estate mutate-10 | `git/r20-resume.worktrees/detached` | 1,834,076 | 3,545 | `head-move`: `checkout --detach` rewrote a few files |
| estate mutate-10 | `git/r05-bytes` | 46,395 | 6,071 | a 5-object commit on the unmerged `side` |
| small mutate-10 | `git/r00-delta.worktrees/detached` | 1,105,407 | 5,337 | `head-move` |
| small mutate-10 | `git/r00-delta` | 1,113,336 | 1,048,576 | `large-edit` of the 1 MiB `model.bin` |
| small mutate-10 | `git/r01-row`, `git/r01-row.worktrees/tree` | 10,563, 6,674 | 3,307 each | a commit on `side` |
| small mutate-10 | `git/history-heavy` | 7,424 | 9,405 | **within the bound** |

## Findings

1. **The git half meets the unchanged clause exactly.** Every unchanged
   rerun reused every censused item (`capture-reused-after-census`): 0 source
   bytes, 0 pack bytes, and one census walk per item. `census_walks` matched
   the sidecar's `census_walks_expected` at every pass: 216, 54, 57 and 66
   at estate. The census model in the corpus doc holds at estate scale.
2. **Git inequality 1 holds with equality.** The receipts'
   `source_bytes_read` equals the changed worktree seats to the byte (3,209
   B; 67,115,992 B). Nothing unchanged was re-read.
3. **Git inequality 2 fails in v1's snapshot layer, not in history.** Three
   shapes:
   - **A grouped item re-packs its checkout.** The examples are a linked
     worktree after a `head-move` (estate 1.83 MB against 3.5 KB; small
     1.1 MB against 5.3 KB) and a main checkout after an unrelated `side`
     commit. At small scale, the detached bundle held 70 objects (50 blobs,
     1,100,983 B). They include the unchanged committed 1 MiB `model.bin`
     (`c2574974`) and a tag. The worktree and staged snapshot commits in it
     are parentless.
   - **The cause is a hypothesis, not root-caused here.** An item in a
     multi-item group takes the plan's shared group base as its prerequisite.
     `ExportOptions::chain` is "ignored when `prerequisite` is set"
     (`git_carry.rs`, main 4a10bb8). The base's trees are then never edges of
     the parentless snapshot commits. history-heavy, a group of one, takes
     the chain and packed 19 objects.
   - **A constant per-capture overhead.** A changed capture adds its own
     metadata: snapshot commits and trees, carry refs and the bundle header.
     history-heavy carries 29,570 B of it at estate and 1,087 B at small.
     The estate figure scales with its ref and tree count.
4. **First-pass duplication across linked worktrees.** Every item bundle of
   `r00` and `r12` (4 items each) carries the 64 MiB blob, at 67.99 to
   68.70 MB per bundle. That is 546 MB of the first pass's 932 MB of v1
   bundles. The shared group bases total 155.9 MB, and history-heavy's
   bundle is 179.4 MB.
5. **A changed capture fetches its whole retained bundle for reuse.** At
   estate `mutate-1`, history-heavy's capture read 179,409,341 B of its own
   retained bundle (`read_source_capture_reuse_bytes`). That is private
   state, not the source. It spent 13.81 s of CPU on a 5-object change; the
   v2 estimate of the same change took 0.29 s. At `mutate-10`, the retained
   bundle was `mutate-1`'s 39,352 B one, and the capture took 0.45 s.
6. **File half: a 16-byte sniff per refused SQLite seat, every pass.** copy
   reads the magic of each database it then refuses (`SQLITE_STATE_CHANGED`):
   4 × 16 B at small and 11 × 16 B at estate. The `-wal` and `-shm` are
   refused by name, unread. This is the only thing that fails the file half's
   "0 content bytes" clause. It is the review's WP6 PR 2 item: memoise
   refused seats by stat identity, and count sniff bytes separately.
7. **File half deltas pass both inequalities.** At estate `mutate-10`, copy
   read 674,467 B against a bound of 4,095,275 B and received 1,217 B (the
   new file) against 263,623 B.
8. **No-clobber: `blocked by WP0(d)`.** A changed seat whose destination
   already holds an older output is read and then refused
   `GIT_DESTINATION_OCCUPIED`, and the destination keeps the stale bytes:
   - estate `mutate-10`: the appended 673,054 B rollout JSONL and the edited
     `cli.js`;
   - small `mutate-10`: the appended 49,179 B rollout.

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
    priority. It is why the unchanged reruns sit at 0.6 % to 1.7 % of the
    first pass.

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
v1's first-pass bundle files (932,163,797 B; the counter says 932,119,799 B)
split into:
- the shared group bases, 155,889,062 B;
- history-heavy's bundle, 179,409,341 B;
- the eight `r00` and `r12` item bundles, 546,715,099 B;
- the other 45 item bundles, 50,150,295 B. A repository without linked
  worktrees has no group base, so its history rides in these.

So for refs and history, v1 (335 MB, plus at most 50 MB inside the other
item bundles) and v2 (350 MB) are comparable. v2 carries no worktree,
index, untracked or ignored state, so the rest of v1's bytes have no v2
counterpart. The uncommitted 64 MiB edit at `mutate-10` alone is 67 MB that
v2 cannot carry.

Building the destination models took 26.8 s (m0) and 64.8 s (m1) at estate,
with 24 stash entries fetched each time.

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
- **The bounds are the harness's.** The file half's absent-chunk widening is
  an allowance, not a ruling. The git bound includes no capture metadata.
- **The v2 column is a projection.** It is history and refs only, against a
  mirror model of the destination. A real v2 sender also needs the snapshot
  layer.
- **Coverage gaps.** The top-level dotfiles are not carried, because copy
  runs per area. The bare mirrors are refused.
- **The corpus models a well-behaved estate.** See
  [estate-corpus-v1-2026-10-03.md](estate-corpus-v1-2026-10-03.md), "Not
  modelled".
