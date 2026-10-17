# 2026-10-16: M3 "bulkload proven", the real-link proof plan

Lane: M3 (operator-approved, runs beside the live neo→sting in-place move).
Rulings cited: axiom 7 (bulkload beats rclone on real many-file,
cross-network estates, by measurement); OI-1003-Q194 C2 (S1's headline is
cross-host gate (b) on an estate-shaped many-file corpus; reopens Q97) and C3
(the 1 % delta is a real in-place change to about 1 % of files); OI-1002-Q30
(B/A/B/A/B, every B rep must pass); OI-1003-Q3 (no wall-clock SLA);
OI-1003-Q82/Q89 (gate (b) protocol); OI-1003-Q150 (S1 stays gated against
plain rclone); OI-1003-Q105 (flake-pinned rclone); OI-1003-Q165 (no clone of
a source file); C5 (the 2026-10-03 S3 reading); SPEC R3 ("samples run on
live hosts"; a sample under load is recorded as such); R-N81; R-N13.

**State: prepared, not run.** No byte was sent between neo and sting by this
lane. The harness was proven on a sting loopback only. The field run waits
for the coordinator's word that the move's transfers have finished: the link
(13–36 MB/s) is the move's bottleneck.

## 1. Which S1 threshold applies

- **The gate:** S1 gate (b), now the headline (C2): a cross-host pull against
  rclone over sftp on an estate-shaped many-file corpus.
- **The rep rule** (OI-1002-Q30 with the Q82/Q89 protocol). A rep passes only
  if all of the following hold:
  - the native median beats the rclone median for the initial copy;
  - the native median beats the rclone median for the delta;
  - the unchanged rerun receives 0 bytes and reads 0 content bytes;
  - every native process (`pull`, and `serve` through the wrapper) stays
    under 2 GiB RSS.

  B passes only if **all 3 B reps** pass. There is no wall-clock SLA (Q3).
- **The comparator** is plain rclone (Q150): gate (a)'s flags,
  `--transfers 4 --checkers 4`.
  - Tuned rclone is a stronger, informational comparator.
  - So is rclone followed by `sync -f`, which gives equal durability with
    pull's sealed outputs.
  - rsync is a reference only (SPEC R3: bulkload must not merely "dress up
    rsync").
- **S3 (C5):** an unchanged rerun reads 0 content bytes, its walk is metadata
  only, and its wall time is at most 10 % of the first pass.
  - The delta is bounded by WP0(c) inequality 1: source content bytes read ≤
    the sizes of the changed seats.
  - The harness gates the first clause and the bound. It records the 10 %
    ratio.
- **Delta (C3):** an in-place change to about 1 % of files, timed as such.
  Nothing is removed at the destination first. This replaces gate_b's 1 %-of-
  bytes XOR, which C3 supersedes.
- **What a neo→sting result is: unruled.** C2 reopens Q97 but "does not say
  which hosts carry gate (b)'s gated samples" (SPEC R3). Until a dated ruling
  names them:
  - the evidence is labelled informational;
  - each sample records R-N81 (AC power, load1 < 2.5 on both hosts) as
    `gated`, or as under load (SPEC R3);
  - claims are earned only by gated samples (SPEC R3, Q143). An informational
    run is never cited as a win.

  **Operator question:** may neo→sting carry gate (b)'s gated samples?

## 2. Corpora

### 2.1 Census of the real estate (read-only, lstat only, committed)

`m3_link.py census` walks a tree with `lstat` only:
- no file content is read and nothing is written;
- it runs at nice 19 with the idle IO class (Darwin: IOPOL_THROTTLE);
- it emits counts, bytes, and histograms of size, class, fan-out, depth and
  age;
- for Git trees it adds the per-repository shape;
- the names of a state root's direct children are kept only when they are
  generic product names; everything else is `(other)`.

The output is committed in `docs/evidence/m3-census-2026-10-16/`.

| Tree (sting, 2026-10-16) | Files | Bytes | Dirs | Symlinks | ≤ 4 KiB | Mean | Walk |
|---|---:|---:|---:|---:|---:|---:|---:|
| stage `.claude` | 294,447 | 27.27 GB (25.4 GiB) | 100,969 | 5,424 | 40 % | 90.5 KiB | ~1 min |
| stage `.codex` | 25,892 | 44.46 GB (41.4 GiB) | 6,627 | 48 | 26 % | 1.64 MiB | <1 min |
| `~/git` | 7,045,525 | 340.2 GB (316.9 GiB) | 1,016,868 | 82,286 | 59 % | 47.2 KiB | 11 min |

Size histogram (files per bucket; the bucket is the upper bound):

| Tree | 0 | ≤512 | ≤4K | ≤16K | ≤64K | ≤256K | ≤1M | ≤4M | ≤16M | ≤64M | ≤256M | ≤1G | >1G |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `.claude` | 437 | 37,267 | 79,302 | 91,143 | 53,380 | 14,324 | 15,257 | 3,092 | 153 | 66 | 21 | 5 | 0 |
| `.codex` | 20 | 1,613 | 5,156 | 4,451 | 1,807 | 5,102 | 5,263 | 1,369 | 636 | 391 | 70 | 11 | 3 |
| `~/git` | 51,725 | 1,538,599 | 2,573,231 | 1,740,347 | 801,373 | 276,064 | 47,381 | 10,599 | 3,034 | 2,506 | 614 | 44 | 8 |

Bytes by class:
- **`.claude`:**
  - jsonl: 23,762 files, 15.6 GB;
  - other: 59,075 files, 7.9 GB;
  - text: 112,847 files, 2.3 GB;
  - source: 46,746 files, 0.9 GB;
  - json: 48,546 files, 0.4 GB.

  Top level:
  - `projects`: 58,023 files, 15.7 GiB;
  - `jobs`: 234,885 files, 4.1 GiB (the many-small-file part);
  - a rescue tree: 1,241 files, 5.6 GiB.
- **`.codex`:**
  - jsonl: 1,200 files, 19.2 GB;
  - compressed (`.zst` rollouts): 11,435 files, 15.3 GB;
  - SQLite: 533 files, 6.6 GB;
  - other: 1,185 files, 3.1 GB.

  Top level:
  - `sessions`: 10,838 files, 31.8 GiB;
  - `db-backups`: 591 files, 6.9 GiB.
- **`~/git`:**
  - 1,021 checkouts, 840 linked worktrees, 9 submodules, 12 bare repositories
    and 41 nested repositories (see the note below on the committed profile);
  - packs: 4,192 files, 74.5 GB;
  - loose objects: 396,009 files, 2.5 GB;
  - checkout files: 6.09 M files, 241 GB;
  - per checkout, files: p50 1,325, p90 8,551, p99 36,827, max 93,293;
  - per checkout, bytes: p50 28.5 MB, p90 484 MB, p99 1.49 GB.

Caveats, all of them reasons to take the census again after the move:
- **The stage was being filled.** The `.claude` stage held 25.4 GiB, against
  the roughly 64 GB reported for neo. Its mtimes are the landing time (every
  file is younger than a day), so the age histogram says nothing about neo.
- **The committed `git.json` counts 849 linked worktrees.** Its census read
  every `.git` file as a worktree. A second walk, after the review, tells
  submodules apart: 840 linked worktrees and 9 submodules, with every other
  number within 0.03 %. The profile is kept as committed because the corpus
  identity below is generated from it, and the generator does not read the
  repository counts.
- **`~/git` on sting is a proxy.** It is sting's own tree, into which the move
  is landing. Its age histogram:
  - under 1 day: 709 k files;
  - under 7 days: 2.90 M;
  - under 30 days: 2.06 M;
  - under 1 year: 1.16 M;
  - older: 0.21 M.
- **Once the transfers finish, take the census on neo itself**, still
  read-only:

  ```
  just bench-m3-link census /Users/jess/.claude --label .claude --ssh-host neo \
    --ssh-config /srv/fast-local/jess/bulkload/ssh-config-nomux --out docs/evidence/m3-census-<date>/neo-claude.json
  ```

  Do the same for `.codex` (`--kind state`) and `~/git` (`--kind git`). Neo's
  `/usr/bin/python3` is enough: the helper is Python 3.8+ and the standard
  library only.

### 2.2 The sealed synthetic corpus (M3 corpus v1)

`m3_link.py generate` follows the r23_corpus / estate_corpus pattern:
- the content is SHAKE-256 in counter mode, so it is the same on every host
  and every Python;
- mtimes are fixed;
- the identity is the SHA-256 of the sorted path/kind/size/sha256 manifest;
- the tree is sealed 0444/0555 and `SEAL.json` records it.

How it scales:
- Each census profile becomes an area: `.claude`, `.codex` and `git`.
- Every class and size bucket keeps its census mean size.
- The class's file count is scaled by `bytes / census bytes`, with a bounded
  largest-remainder rule. Counts land within a few files of the target and
  bytes stay close. A rare >1 GiB bucket is kept only when the scale has room
  for it; when it has none, its files are dropped and the count falls short.
- Directories follow the census directory-depth shape, with a heavy-tailed
  fan-out.
- Symlinks follow the census rate.
- SQLite seats carry the magic header. They are excluded from every file arm,
  as gate_b does.

The proof corpus is three profiles at 2 GiB, seed `bulkload-m3-corpus-v1`. It
was generated twice on sting, with identical identity:

- identity: `e728c90c0117bafb112e2f5425424e6ec62ac96fb96622081d9d650b418ad4b3`;
- totals: 38,397 files, 2,141,428,240 bytes, 5,867 directories and 457
  symlinks;
- 58 % of files are ≤ 4 KiB (the `~/git` census has 59 %); the mean is
  54.5 KiB;
- areas:

  | Area | Files | Bytes | Symlinks |
  |---|---:|---:|---:|
  | `.claude` | 1,535 | 127 MB | 28 |
  | `.codex` | 136 | 186 MB | 0 |
  | `git` | 36,726 | 1.83 GB | 429 |

The areas keep the estate's real byte proportions. A per-area run is a
`--profile` subset.

Generate it on neo with `nix shell --inputs-from . nixpkgs#python3 --command
python3 crates/bulkload-bench/scripts/m3_link.py generate <DEST> --profile …
--bytes 2147483648`. Check that the printed identity equals the one above.

The git area holds plain files shaped like `.git` (packs, loose objects,
admin) plus checkouts. It is not real Git, so the Git arm runs on 2.3 and
2.4.

### 2.3 Real subset (actual neo directories, read-only)

`run --source-subset LABEL=PATH` (repeatable) copies each real directory
into the harness's private working copy on neo.
- **How it copies.** The copy uses `os.read`/`os.write`, never a clone, and
  FIEMAP checks that nothing is shared (Q165). Sockets, FIFOs and files that
  vanish mid-copy are skipped and counted.
- **Linked worktrees.** Their absolute `.git`/`gitdir` links are rewritten
  inside the copy only, matched against every source by its given and its
  real path. A link that still names a path outside the copy refuses the
  subset, for example a worktree whose main repository was not selected.
  Submodules (`gitdir` under `.git/modules/`) belong to their outer
  checkout's item.
- **Refusals before any copy.** The subset refuses a work root that overlaps
  a source, sources that overlap each other, duplicate labels, and a source
  volume without room for the copy and the Git arm's captures plus 1 GiB.
- **What it never does.** It never writes the originals.
- **What evidence records.** Labels and a digest of the path list, never the
  paths.

Selection rule (about 3 GiB in all, chosen from the post-move neo census):

| Label | What |
|---|---|
| `claude` | One `~/.claude/projects/<p>` near the p90 size, plus `~/.claude/jobs` (the many-small-file shape). |
| `codex` | One month of `~/.codex/sessions/YYYY/MM` (jsonl and `.zst`). |
| `git` | Four checkouts with their `.worktrees/`: one each near p50 and p90 of `checkout_files`, the one with the most linked worktrees, and one pack-heavy. |

The coordinator names the paths.

### 2.4 Estate corpus v1 (`estate_corpus.py --scale estate`, sealed)

This keeps comparability with the rig, and it is the only sealed corpus that
holds real Git with worktrees, stashes and nested repositories for the G arm.

## 3. Arms

| Letter | Arm | Role |
|---|---|---|
| B | `bulkload-agent --min-free-percent=N pull neo SRC DEST SSTATE DSTATE WRAPPER CONFIG` (one `ssh -T`, serve at WP0(f) background priority) | candidate |
| P | `rclone copy` sftp, gate flags `--transfers 4 --checkers 4`, flake-pinned v1.74.4 | **gate comparator** (Q150) |
| R | the same rclone tuned: the fastest of `--rclone-tune 4,8,16,32` (checkers = 2 × transfers), measured once before the sample | informational |
| S | R's rclone, then `sync -f DEST` inside the timing | equal durability, informational |
| Y | `rsync -a -e "ssh -F CONFIG -T"` | reference only, never a transport |
| G | `estate-capture` on neo, then `pull` of the capture, then `estate-apply` on sting; the sum of the three, against R on the same checkouts (order G/R/G) | Git estate |

All arms share one OpenSSH client and one config. rclone uses `external`
ssh, so its cipher is the same as pull's. rclone, iperf3 and rsync come from
the flake-pinned nixpkgs (`nix shell` in the recipe). Neither host gets a
system package (rclone is not installed on sting).

## 4. Runs and order

- **Phases.** Each rep runs three phases over the same slots:
  1. the initial copy, into a new destination per slot;
  2. an unchanged rerun into the same destinations (S3);
  3. the C3 delta: about 1 % of files changed in place on neo's working copy,
     with a 2 s settle (R-N76), then run into the same destinations.

  The working copy is then restored by a second XOR and must hash back to its
  initial manifest.
- **Order.** The default per phase is `BPBPBRBRBSY`:
  - B/P/B/P/B is exactly the gate's B/A/B/A/B. Each comparator's ratio uses
    only the B slots beside that comparator's slots: B against P uses
    sequences 0, 2 and 4;
  - the alternation then continues against tuned rclone;
  - one S slot and one Y slot close the phase.
- **Git stage.** G/R/G, initial and rerun. A 2 s settle sits outside the
  timing between capture and pull: a pull inside the capture's racy window
  re-reads the whole carry by design (dry-run: 133,845 bytes read with the
  settle omitted, 0 with it).
- **Reps.** 3, the gate's count.
- **Budget at 13–36 MB/s on the 2 GiB corpus:**
  - each initial slot takes about 60–165 s, so a phase of 11 slots takes about
    11–30 min;
  - reruns are walk-bound;
  - a delta changes about 384 files and at most 24 MB;
  - about 25–50 min per rep, about 1.5–2.5 h for 3 reps, plus the tuning
    sweep (about 5–10 min) and the Git stage.

  A `--reps 1` smoke goes first.
- **Destination.** About 11 × 2.1 GB is held at once; each rep's destinations
  are released after it, and a failed release aborts the sample.
  - The harness refuses DEST_SPACE before copying anything on either path
    (corpus or subset). The budget counts every slot, five copies for the Git
    stage, and every rep with `--keep-destinations`. That also guards the
    rclone and rsync arms, which have no floor of their own.
  - On neo, the harness refuses SOURCE_SPACE when the working copy and the
    captures would not fit. The Git arm's captures on neo are released after
    each rep verifies, and only under `--source-work`.
  - A field run refuses a corpus that is not sealed, or whose working copy's
    identity, rebuilt from its manifest, is not the seal's.
  - sting's fast-local is 80 % used, so the agent's default 25 % floor would
    refuse. The run passes `--min-free-percent 10`, as the live move does, and
    the evidence records it.
  - The work root must sit outside the move's XFS project (Q170). That is the
    coordinator's call.

## 5. Method

- **Link baseline per rep.** iperf3 in both directions, each on its own port:
  - the server on neo is `-s -1 --idle-timeout 30`, so it ends itself and the
    harness never signals it;
  - the client retries a refused connection until the server listens,
    bounded by `--iperf3-ready-seconds` (default 120). That covers a slow
    `nix shell` start on neo;
  - the HostName comes from `ssh -G` and is used, never recorded.

  The ssh ceiling is also measured each rep: `dd` over one and over four
  streams, as in gate_b. Every arm reports its share of that ceiling.
- **Load.** Both hosts' load1 and power are taken before and after every arm.
  `gated` (R-N81) is computed per sample, and the rep reports `under_load`.
  Live hosts are the norm (SPEC R3).
- **Recorded per sample:**
  - argv (redacted) and the exit code;
  - wall time;
  - the max RSS of `pull` and of `serve`;
  - bytes received and `source_bytes_read`;
  - the reused count, and the timing lines;
  - destination verification: every regular file and directory by sha256,
    with symlinks and modes as fidelity counts.
- **What is left out.** SQLite seats are out of every file arm: pull refuses
  them by design (`--sqlite=refuse`), and rclone and rsync get the same
  anchored exclude list. **Open:** an arm with `pull --sqlite=snapshot` and
  rclone carrying raw database files. They are not byte-comparable, so this
  needs a ruling on how to verify it.

## 6. Harness

| Piece | Path |
|---|---|
| Script | `crates/bulkload-bench/scripts/m3_link.py`; it reuses gate_b's transport, helper, rclone config, redaction and verification |
| Recipes | `just bench-m3-link …` (positional arguments, inside `nix shell` with rclone, iperf3 and rsync); `just bench-m3-dry-run AGENT [WORK]` |
| Stub tests | `test_m3_link.py`, in `check-optional` |
| Output | `WORK/m3-link.json` and `WORK/m3-link.md`. A field run also writes `docs/evidence/m3-link-<stamp>-neo-sting.{md,json}`. A dry run never writes under `docs/evidence` |

### 6.1 Dry run on sting

Final run 2026-10-16 on the reviewed head, agent `b7787bbf0028`, `/dev/shm`,
sting load1 15–43, NOT A SAMPLE. The loopback ssh shim runs no real ssh and no crypto, so these numbers
say nothing about the link.

| Run | Phase | B | P | R | S | Y |
|---|---|---:|---:|---:|---:|---:|
| 135 MB, 2,405 files | initial | 544 | 1,155 | 1,193 | 1,210 | 364 |
| | rerun | 237 | 412 | 803 | 794 | 81 |
| | delta (24 files) | 344 | 744 | 1,104 | 1,197 | 158 |
| Git fixture (G/R) | initial | G 1,155 | – | R 2,509 | – | – |
| | rerun | G 265 | – | R 1,798 | – | – |

All times are medians in ms.

What the 135 MB run showed:
- Every arm verified.
- The native rerun received 0 and read 0, and the delta reads stayed under
  the changed-seat bound.
- The rerun/initial ratio was 0.44, above the 0.10 bound. Fixed per-session
  costs dominate at this size; the 2 GiB corpus will show whether it holds.

What it means for the field run:
- **rsync beat B on the rerun and the delta**, 81 ms against 237 ms and 158 ms
  against 344 ms. That is the risk to watch on the real link. bulkload's
  rerun cost on 38 k files is the question S3's 10 % clause will answer.
- **Tuned rclone (16/32) was slower than 4/8 on loopback.** The sweep picks
  per link, so the tuned R may differ on the WAN.

### 6.2 The command for the field run

Run it once the coordinator says the move's transfers are done. All of it is
read-only on neo's real trees.

**Prerequisites:**
- A release `bulkload-agent` at the merge sha on both hosts:
  - sting: `~/.local/bin/bulkload-agent-<sha>`;
  - neo: `/Users/jess/.local/bin/bulkload-agent-<sha>`.
- The bulkload checkout at main on neo.
- About 6 GB free on the neo volume chosen for `--source-work`.

```
# 1. Re-take the census on neo (read-only), commit it, then generate the corpus on neo:
ssh -F /srv/fast-local/jess/bulkload/ssh-config-nomux neo \
  'cd ~/git/bulkload && nix shell --inputs-from . nixpkgs#python3 --command python3 \
   crates/bulkload-bench/scripts/m3_link.py generate /Volumes/TinylandSSD/bulkload-m3/corpus-2g \
   --profile docs/evidence/m3-census-2026-10-16/claude.json \
   --profile docs/evidence/m3-census-2026-10-16/codex.json \
   --profile docs/evidence/m3-census-2026-10-16/git.json --bytes 2147483648'
#    expect identity e728c90c…ad4b3

# 2. The sample, on sting (a --reps 1 smoke first):
T=$(date -u +%Y%m%dT%H%MZ)
just bench-m3-link run \
  --work-root /srv/fast-local/jess/bulkload/m3-proof/run-$T \
  --agent ~/.local/bin/bulkload-agent-<sha> \
  --source-host neo --ssh-config /srv/fast-local/jess/bulkload/ssh-config-nomux \
  --source-corpus /Volumes/TinylandSSD/bulkload-m3/corpus-2g \
  --source-work /Volumes/TinylandSSD/bulkload-m3/work-$T \
  --remote-agent /Users/jess/.local/bin/bulkload-agent-<sha> \
  --remote-iperf3 'nix shell nixpkgs#iperf3 --command iperf3' \
  --min-free-percent 10 --rclone-tune 4,8,16,32 --reps 3

# 3. Real subset plus the Git arm (the paths are named by the coordinator from the census):
just bench-m3-link run --work-root /srv/fast-local/jess/bulkload/m3-proof/subset-$T \
  --agent … --source-host neo --ssh-config … --remote-agent … \
  --source-work /Volumes/TinylandSSD/bulkload-m3/subset-$T \
  --source-subset claude=/Users/jess/.claude/jobs --source-subset codex=/Users/jess/.codex/sessions/2026/10 \
  --source-subset git=<dir holding the 4 chosen checkouts and their .worktrees> \
  --git --git-subtree git --min-free-percent 10 --reps 3
```

## 7. Code-mass audit: what does not serve moving data or proving it

This is a proposal only; nothing was deleted. Counts are `wc -l` at
`b7787bb`. Every row needs its own PR, and the rows marked "ruling" need one
first.

| # | Path(s) | Lines | Why it does not serve moving or proving data | Recommendation |
|---:|---|---:|---|---|
| 1 | Q42 probe campaign: `docs/evidence/2026-10-04-q42-probes-review.json` (24,094), `-probes.json` (1,532), `-probes.md` (1,117), `-blahaj-header.json` (67); `scripts/q42_probes.py` (2,462) and its test (455) | 29,727 | A one-off carry_v2 versus v1 decision, closed by OI-1003-Q44/Q56. No recipe runs the script. | Keep the `.md` summary. Move the raw JSON to a Linear attachment or a release asset. Delete the script and its test. |
| 2 | Raw R23 JSON beside the Markdown evidence: `r23-2026-10-07-0652Z-no-a-control.json` (4,210, B/B/B superseded by Q103), `-1410Z-mbp-13-record.json` (6,646), `-2146Z-mbp-13-pr210.json` (6,766), `r23-2026-10-08-0239Z-mbp-13-handoff.json` (7,261) | 24,883 | Every verdict and number cited lives in the `.md`. The JSON is a full dump. | Delete the 0652Z pair, whose mode is retired. Move the other dumps out of the tree, keeping a sha256 line in each `.md`. |
| 3 | Rendered formal configs: 269 `docs/formal/MC_*.cfg` and the `configs*.tsv` | 8,472 | Generated from the Dhall catalogue ("edit the catalogue, never the outputs"). Committing them duplicates the source. | Render in `just tla-check`/`tla-render` and stop committing the outputs, with a CI drift check instead. No semantic loss. |
| 4 | `git-carry-estimate`: `src/git_carry/estimate.rs` (3,494), `estimate/stderr_store.rs` (567), `tests/git_estimate_shapes.rs` (1,354), `tests/git_estimate_dag.rs` (1,103), plus `s3_estate.py` `v2_half` and `git_wire` (~117) | 6,635 | It projects a negotiated thin-pack carry (v2) that was deleted (OI-1003-Q56). Nothing moves data through it. | **Ruling:** P66 keeps the verb. Retire it with its tests and the v2 half of s3_estate. |
| 5 | Agent notes for deleted or closed work (W6 M1 spike and sender, ingest journal, the Q42 lanes, carry_v2): 19 files | 4,519 | They describe code that no longer exists. R-N55 says to delete entries that are no longer true. | Distil the facts that still stand onto TIN-4543 or TIN-3692, then delete. The same sweep applies to the other pre-10-08 notes: 74 files, 14,521 lines. |
| 6 | `gate_b.py` (2,422) and `test_gate_b.py` (1,098) | 3,520 | Its run path calls `ab.resolve_rclone`, which no longer exists in `r23_ab.py`, so gate (b) cannot run past rclone resolution. No test covers that call. Its delta is C3-superseded. `m3_link.py run` is a superset of its arms. Its serve wrapper needs Python 3.9 (`os.waitstatus_to_exitcode`); m3 writes its own. | Move the transport, helper, rclone config, redaction and verify pieces (about 900 lines) into a shared module. Delete gate_b's main, verdict and evidence code (about 1,500) and their tests. Until then, fix the one call. |
| 7 | Evidence for deleted code: `docs/evidence/w6-m1-*.md`, `2026-10-05-blahaj-v1-carry{.json,-harness.py,.md}`, `pr59-review/` | 3,251 | Proofs of the carry_v2 / W6 path, deleted under Q56. | Archive them out of the tree under tag `carry-v2-final`, which already holds that code. |
| 8 | The Haskell N-version explorer, `docs/formal/hs/` (`Explorer.hs` 1,564, `GitCarryCore.hs` 1,536) | 3,100 | A second encoding of models TLC already checks: a duplicated proof. `GitCarryCore.hs` also pins `decide_rows.tsv`. | **Ruling:** keep `GitCarryCore`'s decide reference only if the Rust `decide` tests need a second oracle. Retire `Explorer.hs` and `just formal-nv`. |
| 9 | `m0_gate_a.py` (383) and its test (525) | 908 | The M0 gate of 2026-09-23, superseded by `r23_ab.py` and the rig. It runs only in `check-optional`. | Delete them and their `check-optional` line. |
| 10 | Bazel residue: `BUILD.bazel`, `MODULE.bazel`, its `.lock`, `justfile.flywheel`, `tools/bazel` (410), plus about 360 contract-test lines that pin them | ~770 | OI-1003-Q65 dropped the Bazel gates. The graph builds a docs filegroup and runs two Python tests that `contract-test` already runs directly. | **Ruling** (it touches GloriousFlywheel enrollment): drop Bazel, or keep it as one recipe with no pins. |
| – | Wire v5's reserved W6 frames (`bulkload-proto`, 53 lines reference them: 9 control frames and `TAG_PACK_DATA`) | ~150–250 est. | They are never sent. Held by ruling until WP3's v6 cut. | Cut at v6, as already ruled. |

Total for rows 1–10: about 86 k lines. Most of it is evidence and generated
mass, and about 11 k lines are code (rows 4, 6, 8 and 9). None of it moves
bytes, and none of it is cited by a current S1–S5 proof obligation, except
the formal outputs (row 3), which only need to stop being committed.

Kept on purpose:
- `bench src/micro.rs` (1,238): still cited by the 2026-10-15 S1 lever
  studies.
- `bulkload-handoff`: estate credential carry, in scope.
- `test_ci_contract.py`: the enforcement surface.

## Findings beside the plan

- `gate_b.py:2361` calls `ab.resolve_rclone(...)`, and no function of that
  name exists in `r23_ab.py` (it has `flake_rclone` and `choose_rclone`). The
  gate (b) harness therefore stops with an AttributeError before its first
  arm. The M3 harness calls `ab.flake_rclone` directly.
- **A pull within 2 s of an `estate-capture` re-reads the whole carry.** The
  capture's files are racy (R-N76), so the pull reads them again; this is by
  design. The live move's capture-then-pull scripts should leave that gap.

## Adversarial review (2026-10-16, before the gate)

An independent review of the first head (`26faed2`) found 15 defects: 2
blockers, 7 should-fix and 6 nits. All 15 are fixed in the second commit:

- **Disk budget.** The budget was skipped for the subset path and inverted
  under `--keep-destinations`.
- **Git links.** Links could escape the copy: symlinked sources, worktrees
  whose main repository was outside the subset, and submodules read as
  linked worktrees.
- **Subset into itself.** A work root inside a source made the subset copy
  into itself.
- **iperf3.** The server start raced the client.
- **Medians.** B-against-P medians mixed in the B slots beside tuned R.
- **Restore.** The restore bookkeeping could report a partly XORed copy as
  restored.
- **Release.** A failed release of destinations was reported as success.
- **Identity.** A field run never checked the corpus identity.
- **Source space.** Space on neo went unchecked, and captures piled up there.
- **Git R excludes.** The Git R slot's excludes were anchored at the wrong
  root.
- **Wrapper.** The serve wrapper needed Python 3.9.
- **Evidence file.** Its name could carry an FQDN, and an existing file could
  be overwritten.
- **Profile labels.** Duplicate labels overwrote each other.
- **Unreadable entries.** Unreadable files and directories were handled
  unevenly.
- **Census and plan wording.** The census and this plan's wording were
  inaccurate.

`test_m3_link.py` grew to 13 tests, which cover the subset guards, relinking,
identity, labels and adjacent medians.

## Done in this session

- Census of the stage `.claude` and `.codex` and of sting's `~/git`,
  read-only.
- `m3_link.py` (census, generate, verify, fixture, run) and
  `test_m3_link.py` (8 tests).
- The `bench-m3-link` and `bench-m3-dry-run` recipes, with `test_m3_link.py`
  in `check-optional`.
- Three loopback dry runs, the last through the recipe.
- Two 2 GiB generations, with identical identity.
- The PR, its state and the merge sha are recorded on TIN-4543 when it lands.

Open:
1. The field run, after the move's transfers end. Census neo first.
2. The operator question in section 1.
3. Rulings for audit rows 4, 8 and 10.
4. The `--sqlite=snapshot` arm.
